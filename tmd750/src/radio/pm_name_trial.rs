//! Fixed, separately approved PM1 experiment with conservative write evidence.

use std::num::NonZeroU64;

use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::{
    PmNameTrial, PmNameTrialError, PmNameTrialEvent, PmNameTrialSession, PmNameTrialStatus,
    PmNameTrialWrite,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::transport::Transport;
use crate::types::{Address, Page, RadioModel};

/// The step at which a fixed PM1 session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialSessionStage {
    /// Obtain the next fixed session from the evidence engine before any I/O.
    Preparation,
    /// Obtain and compare a new complete CAT identity before MCP entry.
    Identity,
    /// Enter MCP with its exact required reply.
    Entry,
    /// Read a complete, acknowledged format fragment or canonical PM1 page.
    Read {
        /// Exact fragment or page requested.
        page: Page,
    },
    /// Compare the new format byte and whole page with the required baseline.
    FreshComparison,
    /// Synchronize the exact intent and recovery bytes before permitting W.
    DurableIntent,
    /// Dispatch the sole fixed page and receive its acknowledgment.
    Write,
    /// Obtain and compare the complete immediate post-write page.
    ImmediateReadback,
    /// Acknowledge MCP exit without further access to the old handle.
    Exit,
}

/// The original typed cause of a fixed PM1 session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PmNameTrialSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// The fixed trial's scope or evidence sequence rejected the operation.
    #[error(transparent)]
    Evidence(#[from] PmNameTrialError),
    /// The caller could not durably synchronize the intent before dispatch.
    #[error("PM1 trial durable intent failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// Wire-write evidence, distinct from the conservative restoration obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialWriteDisposition {
    /// No W dispatch was attempted during this session.
    NotAttempted,
    /// Dispatch began; failure or silence cannot establish that no bytes arrived.
    PossiblyDispatched {
        /// The sole fixed intent being dispatched.
        write: PmNameTrialWrite,
    },
    /// The radio acknowledged W; immediate and fresh-session readbacks remain
    /// separate requirements.
    Acknowledged {
        /// The acknowledged fixed intent.
        write: PmNameTrialWrite,
    },
}

/// Completion, pre-write cancellation, or failure of one fixed session.
#[derive(Debug)]
pub enum PmNameTrialSessionOutcome {
    /// Required reads, any fixed write/readback, and E/ACK completed.
    ///
    /// The caller must close/drop, verify fresh CAT, close the fresh transport,
    /// synchronize complete evidence, and only then record `SessionFinalized`.
    AwaitingCatVerification,
    /// Cancellation was honored before any trial write intent was accepted.
    ///
    /// If entered, the synchronized session exited successfully. Cancellation
    /// after a rename intent is never reported as this harmless disposition.
    Cancelled,
    /// The first failed requirement, retaining all earlier complete evidence.
    Failed {
        /// Exact failed step.
        stage: PmNameTrialSessionStage,
        /// Original typed cause.
        error: PmNameTrialSessionError,
    },
}

/// Evidence from one session of the separately approved fixed PM1 experiment.
///
/// This is not firmware-wide qualification, a complete transaction result, or
/// independent proof of durable capture and physical-unit continuity. The raw
/// transport capture must also retain incomplete/rejected exchanges. Callers
/// must preserve both the initial failure and any additional exit failure.
#[derive(Debug)]
pub struct PmNameTrialSessionReport {
    /// Fixed session selected from the engine, absent if preparation failed.
    pub session: Option<PmNameTrialSession>,
    /// Freshly obtained full identity, including a mismatching tuple if read.
    pub identity: Option<Identity>,
    /// Accepted programming-entry reply without its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Fully acknowledged reads in order: format, target, then any readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition independent of the initial session outcome.
    pub exit: McpProbeExit,
    /// Whether this session attempted or acknowledged the fixed W command.
    pub write: PmNameTrialWriteDisposition,
    /// Conservative engine status on return; finalization remains external.
    pub status: PmNameTrialStatus,
    /// First failure, safe pre-write cancellation, or pending CAT verification.
    pub outcome: PmNameTrialSessionOutcome,
    /// Additional exit failure when an earlier failure was already recorded.
    pub cleanup_error: Option<Error>,
}

impl PmNameTrialSessionReport {
    const fn pending(status: PmNameTrialStatus) -> Self {
        Self {
            session: None,
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: PmNameTrialWriteDisposition::NotAttempted,
            status,
            outcome: PmNameTrialSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed engine connection ID: rename 1, restore 2, fresh-session readback 3.
    ///
    /// The number does not establish a fresh connection or its physical owner.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = PmNameTrialSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

struct Failure {
    stage: PmNameTrialSessionStage,
    error: PmNameTrialSessionError,
}

fn failure(stage: PmNameTrialSessionStage, error: impl Into<PmNameTrialSessionError>) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: PmNameTrialSession) -> NonZeroU64 {
    match session {
        PmNameTrialSession::Rename => NonZeroU64::MIN,
        PmNameTrialSession::Restore => NonZeroU64::MIN.saturating_add(1),
        PmNameTrialSession::VerifyRestoration => NonZeroU64::MIN.saturating_add(2),
    }
}

const fn write_for(session: PmNameTrialSession) -> Option<PmNameTrialWrite> {
    match session {
        PmNameTrialSession::Rename => Some(PmNameTrialWrite::Rename),
        PmNameTrialSession::Restore => Some(PmNameTrialWrite::Restore),
        PmNameTrialSession::VerifyRestoration => None,
    }
}

fn cancelled(trial: &PmNameTrial, should_cancel: &mut impl FnMut() -> bool) -> bool {
    trial.status() == PmNameTrialStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Run one session of the separately owner-approved fixed PM1 experiment.
    ///
    /// The engine chooses rename, restore, or read-only fresh-session proof; the
    /// caller cannot supply addresses, pages, replacement text, or phase. This
    /// narrowly scoped experimental driver is not schema support for firmware
    /// 1.02 and does not change the generic verified-write compatibility gate.
    /// The caller must independently obtain approval for the temporary label
    /// and exact restoration, establish physical continuity and the approved
    /// captured baseline, and wrap the transport with fail-closed capture.
    ///
    /// Each invocation obtains a new CAT identity before entry, reads format
    /// byte 10 and the complete canonical PM1 page, and requires exact equality
    /// with the immutable appropriate before-image. No merge or rebase occurs.
    /// Before W, `before_write` must durably synchronize identity, both complete
    /// pages, and the exact intent. Only its successful return permits recording
    /// the conservative write obligation and then dispatching the fixed frame.
    /// Immediate readback compares every byte. No RF command is sent.
    ///
    /// Every completed exchange is retained in the report. Known-boundary
    /// comparison or journal failures permit detached exit; uncertain exchanges
    /// permit no speculative E. After E/ACK the old handle receives no CAT, baud,
    /// or close operation here. The caller must close/drop and establish all
    /// fresh-connection, capture, and durable-evidence requirements before
    /// recording [`PmNameTrialEvent::SessionFinalized`].
    /// These observations establish persistence across MCP exit and re-entry
    /// through fresh connections, not an independently witnessed full-radio
    /// reboot or power cycle.
    ///
    /// # Cancellation
    ///
    /// Await to completion; never drop this future to cancel a live exchange.
    /// Cooperative checks occur at complete boundaries before the first intent.
    /// Once rename intent is accepted, cancellation cannot abandon safe current
    /// phase completion or the subsequent restore/verification sessions. Any
    /// failure halts the engine without clearing its restoration obligation.
    pub async fn run_approved_pm1_trial_session_until_exit(
        &mut self,
        trial: &mut PmNameTrial,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&PmNameTrial, PmNameTrialWrite) -> Result<(), std::io::Error>,
    ) -> PmNameTrialSessionReport {
        let mut report = PmNameTrialSessionReport::pending(trial.status());
        if let Err(error) = self
            .run_pm1_session(trial, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            trial.halt();
            report.fail(error);
        }
        if matches!(report.outcome, PmNameTrialSessionOutcome::Cancelled) {
            trial.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_pm1_session(&mut report).await;
        }
        if matches!(report.outcome, PmNameTrialSessionOutcome::Failed { .. }) {
            trial.halt();
        }
        report.status = trial.status();
        report
    }

    async fn run_pm1_session(
        &mut self,
        trial: &mut PmNameTrial,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&PmNameTrial, PmNameTrialWrite) -> Result<(), std::io::Error>,
        report: &mut PmNameTrialSessionReport,
    ) -> Result<(), Failure> {
        use PmNameTrialSessionStage as Stage;

        let phase = trial
            .next_session()
            .map_err(|error| failure(Stage::Preparation, error))?;
        report.session = Some(phase);
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let identity = self
            .identify()
            .await
            .map_err(|error| failure(Stage::Identity, error))?;
        report.identity = Some(identity.clone());
        if &identity != trial.identity() {
            return Err(failure(Stage::Identity, PmNameTrialError::IdentityMismatch));
        }
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_pm1_session(trial, phase, &identity, should_cancel, report)
            .await?;
        if matches!(report.outcome, PmNameTrialSessionOutcome::Cancelled) {
            return Ok(());
        }
        if let Some(write) = write_for(phase) {
            before_write(trial, write).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    PmNameTrialSessionError::DurableIntent(error),
                )
            })?;
            trial
                .record(PmNameTrialEvent::DurableWriteIntent {
                    id: session_id(phase),
                    write,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_fixed_pm1_page(trial, write, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_pm1_segment(trial.page(), Stage::ImmediateReadback, report)
                .await?;
            trial
                .record(PmNameTrialEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_pm1_session(
        &mut self,
        trial: &mut PmNameTrial,
        phase: PmNameTrialSession,
        identity: &Identity,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut PmNameTrialSessionReport,
    ) -> Result<(), Failure> {
        use PmNameTrialSessionStage as Stage;

        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        let format = self
            .read_pm1_segment(format_page, Stage::Read { page: format_page }, report)
            .await?;
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let page = trial.page();
        let data = self
            .read_pm1_segment(page, Stage::Read { page }, report)
            .await?;
        let memory_format = format.get(2).copied().ok_or_else(|| {
            failure(
                Stage::FreshComparison,
                PmNameTrialError::UnsupportedDescriptor,
            )
        })?;
        trial
            .record(PmNameTrialEvent::FreshSession {
                id: session_id(phase),
                identity,
                memory_format,
                whole_page: &data,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn read_pm1_segment(
        &mut self,
        page: Page,
        stage: PmNameTrialSessionStage,
        report: &mut PmNameTrialSessionReport,
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

    async fn write_fixed_pm1_page(
        &mut self,
        trial: &PmNameTrial,
        write: PmNameTrialWrite,
        report: &mut PmNameTrialSessionReport,
    ) -> Result<(), PmNameTrialSessionError> {
        self.require_mcp_ready()?;
        if self.identity() != Some(trial.identity())
            || trial.identity().model != RadioModel::TmD750
            || trial.identity().firmware.as_str() != "1.02"
            || trial.identity().radio_type.as_str() != "K,2,1"
        {
            return Err(PmNameTrialError::IdentityMismatch.into());
        }
        if trial.page() != PmNameTrial::required_page()?
            || trial.status() != PmNameTrialStatus::PossiblyChanged
            || report.session.and_then(write_for) != Some(write)
        {
            return Err(PmNameTrialError::UnsupportedDescriptor.into());
        }
        let (before, after) = match write {
            PmNameTrialWrite::Rename => (trial.original_page(), trial.expected_page()),
            PmNameTrialWrite::Restore => (trial.expected_page(), trial.original_page()),
        };
        if !report.segments.last().is_some_and(|segment| {
            segment.page == trial.page() && segment.data == before.as_slice()
        }) {
            return Err(PmNameTrialError::PageMismatch.into());
        }
        report.write = PmNameTrialWriteDisposition::PossiblyDispatched { write };
        self.write_pm1_frame(after, "PM1 trial page write").await?;
        report.write = PmNameTrialWriteDisposition::Acknowledged { write };
        Ok(())
    }

    async fn finish_pm1_session(&mut self, report: &mut PmNameTrialSessionReport) {
        let result = match self.mcp_session() {
            Ok(session) => session.exit_detached().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => report.exit = McpProbeExit::Acknowledged,
            Err(error) => {
                report.exit = McpProbeExit::NotAcknowledged;
                if matches!(report.outcome, PmNameTrialSessionOutcome::Failed { .. }) {
                    report.cleanup_error = Some(error);
                } else {
                    report.fail(failure(PmNameTrialSessionStage::Exit, error));
                }
            }
        }
    }
}
