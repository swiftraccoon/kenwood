//! Fixed-scope PM1 name and MY1 callsign write trials: one page, one W frame.
//!
//! Each trial replaces a single fixed field, restores the exact original page,
//! and reads that page back on a fresh session. Verification spans MCP exit and
//! re-entry, not a power cycle, and these drivers leave the generic
//! verified-write firmware gate unchanged.

use std::num::NonZeroU64;

use super::pm1_page::FixedTextTarget;
use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::fixed_text_trial::{
    FixedTextTrial, FixedTextTrialObservation, FixedTextTrialScope,
};
use crate::memory::{
    MyCallsignTrial, PmNameTrial, PmNameTrialError, PmNameTrialEvent, PmNameTrialSession,
    PmNameTrialStatus, PmNameTrialWrite,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, DvGatewayMode, Page, RadioModel};
use kenwood_transport::Transport;

/// The step at which a fixed text-trial session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialSessionStage {
    /// Obtain the next fixed session from the trial before any I/O.
    Preparation,
    /// Obtain and compare a new complete CAT identity before MCP entry.
    Identity,
    /// Require a fresh read-only CAT observation of Gateway Off before entry.
    ///
    /// This additional guard applies only to the fixed MY1 callsign trial.
    GatewayGuard,
    /// Enter MCP with its exact required reply.
    Entry,
    /// Read a complete, acknowledged format fragment, guard page, or target page.
    Read {
        /// Exact fragment or page requested.
        page: Page,
    },
    /// Compare the new format byte and whole page with the required baseline.
    FreshComparison,
    /// Durably record the exact intent and recovery bytes before W.
    DurableIntent,
    /// Dispatch the sole fixed page and receive its acknowledgment.
    Write,
    /// Obtain and compare the complete immediate post-write page.
    ImmediateReadback,
    /// Acknowledge MCP exit without further access to the old handle.
    Exit,
}

/// The original typed cause of a fixed text-trial session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PmNameTrialSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// The fixed trial's scope or session sequence rejected the operation.
    #[error(transparent)]
    Evidence(#[from] PmNameTrialError),
    /// The `before_write` callback failed to record the intent before dispatch.
    #[error("fixed text trial journal record failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// How far the sole fixed W frame reached on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialWriteDisposition {
    /// No W dispatch was attempted during this session.
    NotAttempted,
    /// Dispatch began; a failure or timeout leaves it unknown whether the radio
    /// received the frame, so the restoration obligation still stands.
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
    /// The caller closes and drops the transport, verifies a fresh CAT identity,
    /// closes that connection, durably records the report, and only then records
    /// `SessionFinalized`.
    AwaitingCatVerification,
    /// Cancellation was honored before any trial write intent was accepted.
    ///
    /// If MCP was entered, that session exited successfully. Cancellation after
    /// a rename intent is never reported here.
    Cancelled,
    /// The first failed requirement, retaining every earlier completed exchange.
    Failed {
        /// Exact failed step.
        stage: PmNameTrialSessionStage,
        /// Original typed cause.
        error: PmNameTrialSessionError,
    },
}

/// The wire exchanges of one session of a fixed text trial.
///
/// The raw transport capture must retain incomplete and rejected exchanges;
/// preserve both the first failure and any additional exit failure. This report
/// covers one session, not the whole multi-session trial.
#[derive(Debug)]
pub struct PmNameTrialSessionReport {
    /// Fixed session selected by the trial, absent if preparation failed.
    pub session: Option<PmNameTrialSession>,
    /// Freshly obtained full identity, including a mismatching tuple if read.
    pub identity: Option<Identity>,
    /// Fresh CAT gateway observation for MY1, including a refused active or
    /// unknown mode. PM1 does not query this command and leaves this absent.
    pub gateway_mode: Option<DvGatewayMode>,
    /// Accepted programming-entry reply without its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Fully acknowledged reads in order: format, any required guard page,
    /// target, then any immediate readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition independent of the initial session outcome.
    pub exit: McpProbeExit,
    /// Whether this session attempted or acknowledged the fixed W command.
    pub write: PmNameTrialWriteDisposition,
    /// Trial status on return; the caller performs finalization.
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
            gateway_mode: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: PmNameTrialWriteDisposition::NotAttempted,
            status,
            outcome: PmNameTrialSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed session number: rename 1, restore 2, fresh-session readback 3.
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

fn cancelled(trial: &impl FixedTextTrial, should_cancel: &mut impl FnMut() -> bool) -> bool {
    trial.status() == PmNameTrialStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Run one session of the fixed PM1 rename-and-restore sequence.
    ///
    /// The trial selects the phase (rename, restore, or read-only readback on a
    /// fresh session); the caller supplies no address, page, replacement text, or
    /// phase. Caller preconditions: a complete captured baseline page for the
    /// target, and a transport wrapped in fail-closed capture. The identity tuple
    /// identifies model, firmware, and type, not the physical unit.
    ///
    /// Each invocation obtains a new CAT identity before entry, reads format
    /// byte 10 and the complete canonical PM1 page, and requires exact equality
    /// with the immutable appropriate before-image. No merge or rebase occurs.
    /// Before W, `before_write` must durably record the identity, both complete
    /// pages, and the exact intent. Only its successful return permits recording
    /// the write obligation and then dispatching the fixed frame. Immediate
    /// readback compares every byte. No RF command is sent.
    ///
    /// Every completed exchange is retained in the report. A comparison or
    /// journal failure at a known exchange boundary still exits; an uncertain
    /// exchange sends no E. After E/ACK the old handle receives no CAT, baud, or
    /// close operation here: the caller closes and drops it, verifies a fresh
    /// connection, durably records the report, and only then records
    /// [`PmNameTrialEvent::SessionFinalized`].
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
        should_cancel: impl FnMut() -> bool,
        before_write: impl FnMut(&PmNameTrial, PmNameTrialWrite) -> Result<(), std::io::Error>,
    ) -> PmNameTrialSessionReport {
        self.run_fixed_trial_session_until_exit(trial, should_cancel, before_write)
            .await
    }

    /// Run one session of the fixed PM-Off MY1 callsign trial.
    ///
    /// Only an empty MY1 field can be temporarily replaced with
    /// [`MyCallsignTrial::TEMPORARY_CALLSIGN`], then exactly restored. The trial
    /// selects the three-session sequence; no caller-supplied address, slot,
    /// callsign, or phase is accepted. It covers neither general callsign
    /// editing nor Terminal Mode.
    ///
    /// Each fresh connection must identify the exact captured radio tuple and
    /// report Gateway Off through CAT before MCP entry. The driver reads format
    /// byte 10, the complete immutable PM-control page, and the complete target
    /// page. PM Off, MY1 selection, and Gateway Off must remain unchanged.
    /// Every target byte outside MY1, including its memo, is preserved. The
    /// callback must durably record the exact immutable scope and write intent
    /// before dispatch. Both writes require complete immediate readback.
    /// No gateway-setting, fill, or RF command is sent.
    ///
    /// Caller ordering after each E/ACK: close and drop the original transport,
    /// verify a matching CAT identity on a fresh connection, close it, durably
    /// record the report, then call [`MyCallsignTrial::finalize_session`].
    /// Caller preconditions: fail-closed transport capture on every connection.
    ///
    /// # Cancellation
    ///
    /// Await completion; do not drop a live exchange. Cooperative cancellation
    /// applies only before the first `before_write` call. Once a change is
    /// possible, the current phase and the exact restore/verification remain owed.
    /// Any failed comparison or uncertain exchange halts without a stale retry.
    pub async fn run_approved_my1_trial_session_until_exit(
        &mut self,
        trial: &mut MyCallsignTrial,
        should_cancel: impl FnMut() -> bool,
        before_write: impl FnMut(&MyCallsignTrial, PmNameTrialWrite) -> Result<(), std::io::Error>,
    ) -> PmNameTrialSessionReport {
        self.run_fixed_trial_session_until_exit(trial, should_cancel, before_write)
            .await
    }

    async fn run_fixed_trial_session_until_exit<F: FixedTextTrial>(
        &mut self,
        trial: &mut F,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&F, PmNameTrialWrite) -> Result<(), std::io::Error>,
    ) -> PmNameTrialSessionReport {
        let mut report = PmNameTrialSessionReport::pending(trial.status());
        if let Err(error) = self
            .run_fixed_trial_session(trial, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            trial.halt();
            report.fail(error);
        }
        if matches!(report.outcome, PmNameTrialSessionOutcome::Cancelled) {
            trial.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_fixed_trial_session(&mut report).await;
        }
        if matches!(report.outcome, PmNameTrialSessionOutcome::Failed { .. }) {
            trial.halt();
        }
        report.status = trial.status();
        report
    }

    async fn run_fixed_trial_session<F: FixedTextTrial>(
        &mut self,
        trial: &mut F,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&F, PmNameTrialWrite) -> Result<(), std::io::Error>,
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
        self.read_gateway_guard(trial, should_cancel, report)
            .await?;
        if matches!(report.outcome, PmNameTrialSessionOutcome::Cancelled) {
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_fixed_trial_session(trial, phase, &identity, should_cancel, report)
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
            self.write_fixed_trial_page(trial, write, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_trial_segment(trial.page(), Stage::ImmediateReadback, report)
                .await?;
            trial
                .record(PmNameTrialEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn read_gateway_guard(
        &mut self,
        trial: &impl FixedTextTrial,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut PmNameTrialSessionReport,
    ) -> Result<(), Failure> {
        if trial.scope() != FixedTextTrialScope::My1 {
            return Ok(());
        }
        let mode = self
            .get_dv_gateway_mode()
            .await
            .map_err(|error| failure(PmNameTrialSessionStage::GatewayGuard, error))?;
        report.gateway_mode = Some(mode);
        if mode != DvGatewayMode::Off {
            return Err(failure(
                PmNameTrialSessionStage::GatewayGuard,
                PmNameTrialError::GatewayMode { actual: mode },
            ));
        }
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn compare_fixed_trial_session(
        &mut self,
        trial: &mut impl FixedTextTrial,
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
            .read_trial_segment(format_page, Stage::Read { page: format_page }, report)
            .await?;
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let control_page = if let Some(page) = trial.guard_page() {
            let bytes = self
                .read_trial_segment(page, Stage::Read { page }, report)
                .await?;
            if cancelled(trial, should_cancel) {
                report.outcome = PmNameTrialSessionOutcome::Cancelled;
                return Ok(());
            }
            Some(bytes)
        } else {
            None
        };
        let page = trial.page();
        let data = self
            .read_trial_segment(page, Stage::Read { page }, report)
            .await?;
        let memory_format = format.get(2).copied().ok_or_else(|| {
            failure(
                Stage::FreshComparison,
                PmNameTrialError::UnsupportedDescriptor,
            )
        })?;
        trial
            .fresh_session(FixedTextTrialObservation {
                id: session_id(phase),
                identity,
                memory_format,
                whole_page: &data,
                control_page: control_page.as_deref(),
                gateway_mode: report.gateway_mode,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(trial, should_cancel) {
            report.outcome = PmNameTrialSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn read_trial_segment(
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

    async fn write_fixed_trial_page(
        &mut self,
        trial: &impl FixedTextTrial,
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
        let (target, required_page) = match trial.scope() {
            FixedTextTrialScope::Pm1 => (FixedTextTarget::Pm1, PmNameTrial::required_page()?),
            FixedTextTrialScope::My1 => (FixedTextTarget::My1, MyCallsignTrial::required_page()?),
        };
        if trial.page() != required_page
            || required_page != target.page()?
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
        let operation = match target {
            FixedTextTarget::Pm1 => "PM1 trial page write",
            FixedTextTarget::My1 => "MY1 trial page write",
        };
        self.write_fixed_text_frame(target, after, operation)
            .await?;
        report.write = PmNameTrialWriteDisposition::Acknowledged { write };
        Ok(())
    }

    async fn finish_fixed_trial_session(&mut self, report: &mut PmNameTrialSessionReport) {
        let result = match self.mcp_session() {
            Ok(session) => session.exit().await,
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
