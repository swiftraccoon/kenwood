//! Narrow PM1 name updates with complete-page and lifecycle evidence.

use std::num::NonZeroU64;

use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::{
    Pm1NameUpdate, Pm1NameUpdateError, Pm1NameUpdateEvent, Pm1NameUpdateSession,
    Pm1NameUpdateStatus,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, Page, RadioModel};
use kenwood_transport::Transport;

/// The step at which a bounded PM1 name-update session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm1NameUpdateSessionStage {
    /// Obtain the engine-selected session before any I/O.
    Preparation,
    /// Obtain and compare a complete, new CAT identity before MCP entry.
    Identity,
    /// Enter MCP and require the exact reply.
    Entry,
    /// Obtain one complete, acknowledged format fragment or PM1 page.
    Read {
        /// Exact fragment or page requested.
        page: Page,
    },
    /// Compare the format byte and complete page with the required before-image.
    FreshComparison,
    /// Synchronize the caller's intent before permitting the only W command.
    DurableIntent,
    /// Dispatch the single complete page and require its acknowledgment.
    Write,
    /// Obtain and compare every byte of the immediate post-write page.
    ImmediateReadback,
    /// Acknowledge MCP exit without subsequently reusing the original handle.
    Exit,
}

/// The original typed cause of a PM1 name-update session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Pm1NameUpdateSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// The update's narrow scope or evidence sequence rejected the operation.
    #[error(transparent)]
    Evidence(#[from] Pm1NameUpdateError),
    /// The caller could not durably synchronize the intent before dispatch.
    #[error("PM1 name update durable intent failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// Evidence about the only possible memory write, separate from verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm1NameUpdateWriteDisposition {
    /// No W dispatch was attempted during this session.
    NotAttempted,
    /// Dispatch began; an error or silence cannot prove that no bytes arrived.
    PossiblyDispatched,
    /// The radio acknowledged W; whole-page readback and external finalization
    /// remain separate requirements.
    Acknowledged,
}

/// Completion, safe pre-write cancellation, or failure of one update session.
#[derive(Debug)]
pub enum Pm1NameUpdateSessionOutcome {
    /// Required reads, any write/readback, and E/ACK completed.
    ///
    /// The caller must close/drop, verify fresh CAT, close the fresh transport,
    /// and durably synchronize complete evidence before `SessionFinalized`.
    AwaitingCatVerification,
    /// Cancellation was honored before accepting the only write intent.
    ///
    /// If entered, the synchronized MCP session exited successfully. This
    /// disposition never represents cancellation after a possible write.
    Cancelled,
    /// The first failed requirement, retaining all earlier complete evidence.
    Failed {
        /// Exact failed step.
        stage: Pm1NameUpdateSessionStage,
        /// Original typed cause.
        error: Pm1NameUpdateSessionError,
    },
}

/// Evidence from one bounded PM1 name-update session.
///
/// This report does not attest physical-unit continuity, durable capture,
/// external cleanup, a full power cycle, or general firmware schema support.
/// Preserve the raw transcript as well as these complete read segments; failed
/// exchanges and any secondary exit error remain material evidence.
#[derive(Debug)]
pub struct Pm1NameUpdateSessionReport {
    /// Engine-selected session, absent when preparation failed.
    pub session: Option<Pm1NameUpdateSession>,
    /// New full CAT identity, including a mismatching tuple if obtained.
    pub identity: Option<Identity>,
    /// Accepted programming-entry reply, excluding its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Complete acknowledged reads: format, target, then any immediate readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of the original session failure.
    pub exit: McpProbeExit,
    /// Whether this session attempted or acknowledged the sole W command.
    pub write: Pm1NameUpdateWriteDisposition,
    /// Conservative engine status; external finalization is still required.
    pub status: Pm1NameUpdateStatus,
    /// First failure, safe cancellation, or outstanding fresh CAT verification.
    pub outcome: Pm1NameUpdateSessionOutcome,
    /// Additional exit error when another failure was already recorded.
    pub cleanup_error: Option<Error>,
}

impl Pm1NameUpdateSessionReport {
    const fn pending(status: Pm1NameUpdateStatus) -> Self {
        Self {
            session: None,
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: Pm1NameUpdateWriteDisposition::NotAttempted,
            status,
            outcome: Pm1NameUpdateSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed engine session ID: apply 1, separate-session verification 2.
    ///
    /// This number alone proves neither a fresh connection nor physical owner.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = Pm1NameUpdateSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

struct Failure {
    stage: Pm1NameUpdateSessionStage,
    error: Pm1NameUpdateSessionError,
}

fn failure(
    stage: Pm1NameUpdateSessionStage,
    error: impl Into<Pm1NameUpdateSessionError>,
) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: Pm1NameUpdateSession) -> NonZeroU64 {
    match session {
        Pm1NameUpdateSession::Apply => NonZeroU64::MIN,
        Pm1NameUpdateSession::Verify => NonZeroU64::MIN.saturating_add(1),
    }
}

fn cancelled(update: &Pm1NameUpdate, should_cancel: &mut impl FnMut() -> bool) -> bool {
    update.status() == Pm1NameUpdateStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Run one session of a typed, narrowly scoped PM1 name update.
    ///
    /// The immutable update chooses apply or read-only verification. It exposes
    /// no caller-selected address, alternate field, force-write option, or
    /// automatic rollback. This driver does not relax the generic schema-write
    /// gate. Callers must establish approval for the requested name, physical
    /// unit continuity, and a fully captured baseline, and use fail-closed raw
    /// transport capture. The tested connection is main-unit USB at 9600 baud
    /// with RTS/CTS and DTR/RTS asserted. This generic transport API cannot
    /// establish the USB connector identity; the caller must enforce that scope.
    ///
    /// Each invocation obtains a fresh complete identity before MCP entry,
    /// reads format byte 10 and the complete canonical PM1 page, and requires
    /// exact equality with the appropriate immutable page. No merge or rebase
    /// occurs. In the apply session, `before_write` must durably synchronize the
    /// identity, both pages, and exact intent. Its successful return permits
    /// recording the conservative possible-write status and dispatching one W
    /// frame. The immediate readback compares all 256 bytes. Verification is a
    /// separate read-only session; no RF command is sent in either session.
    ///
    /// Known-boundary comparison or journal errors permit detached E/ACK;
    /// uncertain exchanges permit no speculative exit. After E/ACK, this method
    /// performs no CAT, baud, or close operation on the retired handle. The
    /// caller must close/drop, verify a matching identity on a fresh connection,
    /// close that connection, and synchronize complete evidence before recording
    /// [`Pm1NameUpdateEvent::SessionFinalized`]. Only then may the caller open
    /// the next session. This establishes observations across MCP exit/re-entry,
    /// not independently observed power-cycle persistence.
    ///
    /// # Cancellation
    ///
    /// Await this future to completion; never drop it to cancel live I/O.
    /// Cooperative cancellation is honored only at complete exchange boundaries
    /// before the write intent. After intent, finish the safe current phase and
    /// subsequent verification. On failure the engine halts, preserves possible
    /// change, and sends no speculative rollback or retry.
    pub async fn set_pm1_name_session_until_exit(
        &mut self,
        update: &mut Pm1NameUpdate,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&Pm1NameUpdate) -> Result<(), std::io::Error>,
    ) -> Pm1NameUpdateSessionReport {
        let mut report = Pm1NameUpdateSessionReport::pending(update.status());
        if let Err(error) = self
            .run_pm1_update_session(update, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            update.halt();
            report.fail(error);
        }
        if matches!(report.outcome, Pm1NameUpdateSessionOutcome::Cancelled) {
            update.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_pm1_update_session(&mut report).await;
        }
        if matches!(report.outcome, Pm1NameUpdateSessionOutcome::Failed { .. }) {
            update.halt();
        }
        report.status = update.status();
        report
    }

    async fn run_pm1_update_session(
        &mut self,
        update: &mut Pm1NameUpdate,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&Pm1NameUpdate) -> Result<(), std::io::Error>,
        report: &mut Pm1NameUpdateSessionReport,
    ) -> Result<(), Failure> {
        use Pm1NameUpdateSessionStage as Stage;

        let phase = update
            .next_session()
            .map_err(|error| failure(Stage::Preparation, error))?;
        report.session = Some(phase);
        if cancelled(update, should_cancel) {
            report.outcome = Pm1NameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        let identity = self
            .identify()
            .await
            .map_err(|error| failure(Stage::Identity, error))?;
        report.identity = Some(identity.clone());
        if &identity != update.identity() {
            return Err(failure(
                Stage::Identity,
                Pm1NameUpdateError::IdentityMismatch,
            ));
        }
        if cancelled(update, should_cancel) {
            report.outcome = Pm1NameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_pm1_update_session(update, phase, &identity, should_cancel, report)
            .await?;
        if matches!(report.outcome, Pm1NameUpdateSessionOutcome::Cancelled) {
            return Ok(());
        }
        if phase == Pm1NameUpdateSession::Apply {
            before_write(update).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    Pm1NameUpdateSessionError::DurableIntent(error),
                )
            })?;
            update
                .record(Pm1NameUpdateEvent::DurableWriteIntent {
                    id: NonZeroU64::MIN,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_pm1_update(update, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_pm1_update_segment(update.page(), Stage::ImmediateReadback, report)
                .await?;
            update
                .record(Pm1NameUpdateEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_pm1_update_session(
        &mut self,
        update: &mut Pm1NameUpdate,
        phase: Pm1NameUpdateSession,
        identity: &Identity,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut Pm1NameUpdateSessionReport,
    ) -> Result<(), Failure> {
        use Pm1NameUpdateSessionStage as Stage;

        if cancelled(update, should_cancel) {
            report.outcome = Pm1NameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        let format = self
            .read_pm1_update_segment(format_page, Stage::Read { page: format_page }, report)
            .await?;
        if cancelled(update, should_cancel) {
            report.outcome = Pm1NameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        let page = update.page();
        let data = self
            .read_pm1_update_segment(page, Stage::Read { page }, report)
            .await?;
        let memory_format = format.get(2).copied().ok_or_else(|| {
            failure(
                Stage::FreshComparison,
                Pm1NameUpdateError::UnsupportedDescriptor,
            )
        })?;
        update
            .record(Pm1NameUpdateEvent::FreshSession {
                id: session_id(phase),
                identity,
                memory_format,
                whole_page: &data,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(update, should_cancel) {
            report.outcome = Pm1NameUpdateSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn read_pm1_update_segment(
        &mut self,
        page: Page,
        stage: Pm1NameUpdateSessionStage,
        report: &mut Pm1NameUpdateSessionReport,
    ) -> Result<Vec<u8>, Failure> {
        let mut session = self.mcp_session().map_err(|error| failure(stage, error))?;
        let data = session
            .read_page(page)
            .await
            .map_err(|error| failure(stage, error))?;
        report.segments.push(McpProbeSegment {
            page,
            data: data.clone(),
        });
        Ok(data)
    }

    async fn write_pm1_update(
        &mut self,
        update: &Pm1NameUpdate,
        report: &mut Pm1NameUpdateSessionReport,
    ) -> Result<(), Pm1NameUpdateSessionError> {
        self.require_mcp_ready()?;
        if self.identity() != Some(update.identity())
            || update.identity().model != RadioModel::TmD750
            || update.identity().firmware.as_str() != "1.02"
            || update.identity().radio_type.as_str() != "K,2,1"
        {
            return Err(Pm1NameUpdateError::IdentityMismatch.into());
        }
        if update.page() != Pm1NameUpdate::required_page()?
            || update.status() != Pm1NameUpdateStatus::PossiblyChanged
            || report.session != Some(Pm1NameUpdateSession::Apply)
        {
            return Err(Pm1NameUpdateError::UnsupportedDescriptor.into());
        }
        if !report.segments.last().is_some_and(|segment| {
            segment.page == update.page() && segment.data == update.original_page().as_slice()
        }) {
            return Err(Pm1NameUpdateError::PageMismatch.into());
        }
        report.write = Pm1NameUpdateWriteDisposition::PossiblyDispatched;
        self.write_pm1_frame(update.desired_page(), "PM1 name page write")
            .await?;
        report.write = Pm1NameUpdateWriteDisposition::Acknowledged;
        Ok(())
    }

    async fn finish_pm1_update_session(&mut self, report: &mut Pm1NameUpdateSessionReport) {
        let result = match self.mcp_session() {
            Ok(session) => session.exit().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => report.exit = McpProbeExit::Acknowledged,
            Err(error) => {
                report.exit = McpProbeExit::NotAcknowledged;
                if matches!(report.outcome, Pm1NameUpdateSessionOutcome::Failed { .. }) {
                    report.cleanup_error = Some(error);
                } else {
                    report.fail(failure(Pm1NameUpdateSessionStage::Exit, error));
                }
            }
        }
    }
}
