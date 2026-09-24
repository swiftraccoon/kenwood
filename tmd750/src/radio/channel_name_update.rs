//! Narrowly scoped channel name updates over one complete name-table page.
//!
//! An update writes the new name in one session and re-reads the page in a
//! separate read-only session, so it verifies persistence across MCP exit and
//! re-entry, not across a power cycle.

use std::num::NonZeroU64;

use super::pm1_page::FixedTextTarget;
use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::{
    ChannelNameUpdate, ChannelNameUpdateError, ChannelNameUpdateEvent, ChannelNameUpdateSession,
    ChannelNameUpdateStatus,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, Page, RadioModel};
use kenwood_transport::Transport;

/// The step at which a bounded channel name-update session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelNameUpdateSessionStage {
    /// Obtain the update-selected session before any I/O.
    Preparation,
    /// Obtain and compare a complete, new CAT identity before MCP entry.
    Identity,
    /// Enter MCP and require the exact reply.
    Entry,
    /// Obtain one complete, acknowledged format fragment or name page.
    Read {
        /// Exact fragment or page requested.
        page: Page,
    },
    /// Compare the format byte and complete page with the required before-image.
    FreshComparison,
    /// Durably record the caller's intent before the only W command.
    DurableIntent,
    /// Dispatch the single complete page and require its acknowledgment.
    Write,
    /// Obtain and compare every byte of the immediate post-write page.
    ImmediateReadback,
    /// Acknowledge MCP exit without subsequently reusing the original handle.
    Exit,
}

/// The original typed cause of a channel name-update session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ChannelNameUpdateSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// The update's narrow scope or session sequence rejected the operation.
    #[error(transparent)]
    Evidence(#[from] ChannelNameUpdateError),
    /// The `before_write` callback failed to record the intent before dispatch.
    #[error("channel name update journal record failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// How far the sole W frame reached on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelNameUpdateWriteDisposition {
    /// No W dispatch was attempted during this session.
    NotAttempted,
    /// Dispatch began; an error or timeout leaves it unknown whether the radio
    /// received the frame.
    PossiblyDispatched,
    /// The radio acknowledged W; whole-page readback and external finalization
    /// remain separate requirements.
    Acknowledged,
}

/// Completion, safe pre-write cancellation, or failure of one update session.
#[derive(Debug)]
pub enum ChannelNameUpdateSessionOutcome {
    /// Required reads, any write/readback, and E/ACK completed.
    ///
    /// The caller closes and drops the transport, verifies a fresh CAT identity,
    /// closes that connection, and durably records the report before
    /// `SessionFinalized`.
    AwaitingCatVerification,
    /// Cancellation was honored before accepting the only write intent.
    ///
    /// If MCP was entered, that session exited successfully. Cancellation after
    /// a possible write is never reported here.
    Cancelled,
    /// The first failed requirement, retaining every earlier completed exchange.
    Failed {
        /// Exact failed step.
        stage: ChannelNameUpdateSessionStage,
        /// Original typed cause.
        error: ChannelNameUpdateSessionError,
    },
}

/// The wire exchanges of one bounded channel name-update session.
///
/// Preserve the raw transcript alongside these complete read segments: failed
/// exchanges and any secondary exit error appear only there.
#[derive(Debug)]
pub struct ChannelNameUpdateSessionReport {
    /// Session selected by the update, absent when preparation failed.
    pub session: Option<ChannelNameUpdateSession>,
    /// New full CAT identity, including a mismatching tuple if obtained.
    pub identity: Option<Identity>,
    /// Accepted programming-entry reply, excluding its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Complete acknowledged reads: format, target, then any immediate readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of the original session failure.
    pub exit: McpProbeExit,
    /// Whether this session attempted or acknowledged the sole W command.
    pub write: ChannelNameUpdateWriteDisposition,
    /// Update status on return; the caller performs finalization.
    pub status: ChannelNameUpdateStatus,
    /// First failure, safe cancellation, or outstanding fresh CAT verification.
    pub outcome: ChannelNameUpdateSessionOutcome,
    /// Additional exit error when another failure was already recorded.
    pub cleanup_error: Option<Error>,
}

impl ChannelNameUpdateSessionReport {
    const fn pending(status: ChannelNameUpdateStatus) -> Self {
        Self {
            session: None,
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: ChannelNameUpdateWriteDisposition::NotAttempted,
            status,
            outcome: ChannelNameUpdateSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed session number: apply 1, separate-session verification 2.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = ChannelNameUpdateSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

struct Failure {
    stage: ChannelNameUpdateSessionStage,
    error: ChannelNameUpdateSessionError,
}

fn failure(
    stage: ChannelNameUpdateSessionStage,
    error: impl Into<ChannelNameUpdateSessionError>,
) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: ChannelNameUpdateSession) -> NonZeroU64 {
    match session {
        ChannelNameUpdateSession::Apply => NonZeroU64::MIN,
        ChannelNameUpdateSession::Verify => NonZeroU64::MIN.saturating_add(1),
    }
}

fn cancelled(update: &ChannelNameUpdate, should_cancel: &mut impl FnMut() -> bool) -> bool {
    update.status() == ChannelNameUpdateStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Run one session of a typed, narrowly scoped channel name update.
    ///
    /// The immutable update chooses apply or read-only verification. It exposes
    /// no caller-selected address outside the name table, no force-write
    /// option, and no automatic rollback, and leaves the generic schema-write
    /// gate unchanged. Caller preconditions: a fully captured baseline for the
    /// target page and a transport wrapped in fail-closed raw capture. This
    /// generic transport API cannot check the USB connector identity, so the
    /// caller selects the endpoint.
    ///
    /// Each invocation obtains a fresh complete identity before MCP entry,
    /// reads format byte 10 and the complete name page, and requires exact
    /// equality with the appropriate immutable page. No merge or rebase occurs.
    /// In the apply session, `before_write` must durably record the identity,
    /// both pages, and exact intent. Its successful return permits recording
    /// the possible-write status and dispatching one W frame. The immediate
    /// readback compares all 256 bytes. Verification is a separate read-only
    /// session; no RF command is sent in either session.
    ///
    /// A comparison or journal error at a known exchange boundary still exits;
    /// an uncertain exchange sends no E. After E/ACK, this method performs no
    /// CAT, baud, or close operation on the retired handle. The caller closes
    /// and drops it, verifies a matching identity on a fresh connection, closes
    /// that connection, and durably records the report before recording
    /// [`ChannelNameUpdateEvent::SessionFinalized`]. Only then may the caller
    /// open the next session.
    ///
    /// # Cancellation
    ///
    /// Await this future to completion; never drop it to cancel live I/O.
    /// Cooperative cancellation is honored only at complete exchange boundaries
    /// before the write intent. After intent, finish the safe current phase and
    /// subsequent verification. On failure the update halts, keeps the
    /// possibly-changed status, and sends no rollback or retry.
    pub async fn set_channel_name_session_until_exit(
        &mut self,
        update: &mut ChannelNameUpdate,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&ChannelNameUpdate) -> Result<(), std::io::Error>,
    ) -> ChannelNameUpdateSessionReport {
        let mut report = ChannelNameUpdateSessionReport::pending(update.status());
        if let Err(error) = self
            .run_channel_name_update_session(
                update,
                &mut should_cancel,
                &mut before_write,
                &mut report,
            )
            .await
        {
            update.halt();
            report.fail(error);
        }
        if matches!(report.outcome, ChannelNameUpdateSessionOutcome::Cancelled) {
            update.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_channel_name_update_session(&mut report).await;
        }
        if matches!(
            report.outcome,
            ChannelNameUpdateSessionOutcome::Failed { .. }
        ) {
            update.halt();
        }
        report.status = update.status();
        report
    }

    async fn run_channel_name_update_session(
        &mut self,
        update: &mut ChannelNameUpdate,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&ChannelNameUpdate) -> Result<(), std::io::Error>,
        report: &mut ChannelNameUpdateSessionReport,
    ) -> Result<(), Failure> {
        use ChannelNameUpdateSessionStage as Stage;

        let phase = update
            .next_session()
            .map_err(|error| failure(Stage::Preparation, error))?;
        report.session = Some(phase);
        if cancelled(update, should_cancel) {
            report.outcome = ChannelNameUpdateSessionOutcome::Cancelled;
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
                ChannelNameUpdateError::IdentityMismatch,
            ));
        }
        if cancelled(update, should_cancel) {
            report.outcome = ChannelNameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_channel_name_update_session(update, phase, &identity, should_cancel, report)
            .await?;
        if matches!(report.outcome, ChannelNameUpdateSessionOutcome::Cancelled) {
            return Ok(());
        }
        if phase == ChannelNameUpdateSession::Apply {
            before_write(update).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    ChannelNameUpdateSessionError::DurableIntent(error),
                )
            })?;
            update
                .record(ChannelNameUpdateEvent::DurableWriteIntent {
                    id: NonZeroU64::MIN,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_channel_name_update(update, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_channel_name_update_segment(update.page(), Stage::ImmediateReadback, report)
                .await?;
            update
                .record(ChannelNameUpdateEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_channel_name_update_session(
        &mut self,
        update: &mut ChannelNameUpdate,
        phase: ChannelNameUpdateSession,
        identity: &Identity,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut ChannelNameUpdateSessionReport,
    ) -> Result<(), Failure> {
        use ChannelNameUpdateSessionStage as Stage;

        if cancelled(update, should_cancel) {
            report.outcome = ChannelNameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        let format = self
            .read_channel_name_update_segment(
                format_page,
                Stage::Read { page: format_page },
                report,
            )
            .await?;
        if cancelled(update, should_cancel) {
            report.outcome = ChannelNameUpdateSessionOutcome::Cancelled;
            return Ok(());
        }
        let page = update.page();
        let data = self
            .read_channel_name_update_segment(page, Stage::Read { page }, report)
            .await?;
        let memory_format = format.get(2).copied().ok_or_else(|| {
            failure(
                Stage::FreshComparison,
                ChannelNameUpdateError::UnsupportedPage,
            )
        })?;
        update
            .record(ChannelNameUpdateEvent::FreshSession {
                id: session_id(phase),
                identity,
                memory_format,
                whole_page: &data,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(update, should_cancel) {
            report.outcome = ChannelNameUpdateSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn read_channel_name_update_segment(
        &mut self,
        page: Page,
        stage: ChannelNameUpdateSessionStage,
        report: &mut ChannelNameUpdateSessionReport,
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

    async fn write_channel_name_update(
        &mut self,
        update: &ChannelNameUpdate,
        report: &mut ChannelNameUpdateSessionReport,
    ) -> Result<(), ChannelNameUpdateSessionError> {
        self.require_mcp_ready()?;
        if self.identity() != Some(update.identity())
            || update.identity().model != RadioModel::TmD750
            || update.identity().firmware.as_str() != "1.02"
            || update.identity().radio_type.as_str() != "K,2,1"
        {
            return Err(ChannelNameUpdateError::IdentityMismatch.into());
        }
        if update.page() != ChannelNameUpdate::required_page(update.channel())?
            || update.status() != ChannelNameUpdateStatus::PossiblyChanged
            || report.session != Some(ChannelNameUpdateSession::Apply)
        {
            return Err(ChannelNameUpdateError::UnsupportedPage.into());
        }
        if !report.segments.last().is_some_and(|segment| {
            segment.page == update.page() && segment.data == update.original_page().as_slice()
        }) {
            return Err(ChannelNameUpdateError::PageMismatch.into());
        }
        let target = FixedTextTarget::channel_names(update.page())
            .ok_or(ChannelNameUpdateError::UnsupportedPage)?;
        report.write = ChannelNameUpdateWriteDisposition::PossiblyDispatched;
        self.write_fixed_text_frame(target, update.desired_page(), "channel name page write")
            .await?;
        report.write = ChannelNameUpdateWriteDisposition::Acknowledged;
        Ok(())
    }

    async fn finish_channel_name_update_session(
        &mut self,
        report: &mut ChannelNameUpdateSessionReport,
    ) {
        let result = match self.mcp_session() {
            Ok(session) => session.exit().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => report.exit = McpProbeExit::Acknowledged,
            Err(error) => {
                report.exit = McpProbeExit::NotAcknowledged;
                if matches!(
                    report.outcome,
                    ChannelNameUpdateSessionOutcome::Failed { .. }
                ) {
                    report.cleanup_error = Some(error);
                } else {
                    report.fail(failure(ChannelNameUpdateSessionStage::Exit, error));
                }
            }
        }
    }
}
