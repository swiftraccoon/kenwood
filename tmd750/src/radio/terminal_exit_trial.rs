//! Bounded, explicitly experimental Terminal exit with immutable page guards.

use std::num::NonZeroU64;

use super::{Identity, Radio};
use crate::error::{Error, ProtocolError};
use crate::memory::{
    TerminalExitTrial, TerminalExitTrialError, TerminalExitTrialEvent, TerminalExitTrialSession,
    TerminalExitTrialStatus,
};
use crate::protocol::mcp::{ACK, write_request};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, DvGatewayMode, Page};
use kenwood_transport::Transport;

/// Step reached by one session of the experimental Terminal exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExitTrialSessionStage {
    /// Select the next fixed session before sending any bytes.
    Preparation,
    /// Obtain and compare the complete fresh CAT identity.
    Identity,
    /// Require Terminal for application, or Off for read-only verification.
    Gateway,
    /// Enter programming mode and require its exact reply.
    Entry,
    /// Read one complete, fixed fragment or page.
    Read {
        /// Exact region requested, retained on failure.
        page: Page,
    },
    /// Compare the full target and both immutable guard pages.
    FreshComparison,
    /// Synchronize the exact intent before permitting the only write.
    DurableIntent,
    /// Dispatch one complete page frame and require its acknowledgment.
    Write,
    /// Read and compare the complete desired page immediately after writing.
    ImmediateReadback,
    /// Acknowledge programming exit and retire the original handle.
    Exit,
}

/// Original cause of an experimental Terminal-exit session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalExitTrialSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// Fixed scope or ordered evidence failed validation.
    #[error(transparent)]
    Evidence(#[from] TerminalExitTrialError),
    /// The caller could not synchronize the required intent before dispatch.
    #[error("Terminal exit durable intent failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// Actual dispatch evidence, separate from the conservative engine status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExitTrialWriteDisposition {
    /// No memory-write dispatch was attempted in this session.
    NotAttempted,
    /// Dispatch began; failure cannot establish that no bytes arrived.
    PossiblyDispatched,
    /// The write was acknowledged; readback and lifecycle verification remain.
    Acknowledged,
}

/// Result of one session, before external close and fresh CAT verification.
#[derive(Debug)]
pub enum TerminalExitTrialSessionOutcome {
    /// Required exchanges completed; the caller still owes fresh identity and
    /// Gateway Off, both closes, and complete durable evidence.
    AwaitingCatVerification,
    /// Cancellation was honored before any write intent was accepted.
    Cancelled,
    /// First failure, without hiding a separate cleanup failure.
    Failed {
        /// Exact failed requirement.
        stage: TerminalExitTrialSessionStage,
        /// Typed original cause.
        error: TerminalExitTrialSessionError,
    },
}

/// Evidence from one fixed Terminal-exit session, including incomplete runs.
///
/// This is not proof of durable capture, physical-unit continuity, operator
/// approval, automatic-exit qualification, or completed external finalization.
#[derive(Debug)]
pub struct TerminalExitTrialSessionReport {
    /// Engine-selected session, absent if preparation failed.
    pub session: Option<TerminalExitTrialSession>,
    /// Complete freshly read identity, including a mismatch if obtained.
    pub identity: Option<Identity>,
    /// Fresh pre-entry Gateway observation, including a refused value.
    pub gateway_mode: Option<DvGatewayMode>,
    /// Exact accepted programming-entry reply without its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Acknowledged reads: format, control, routing, target, and any readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit evidence, separate from the first session failure.
    pub exit: McpProbeExit,
    /// Whether the sole fixed memory write was attempted or acknowledged.
    pub write: TerminalExitTrialWriteDisposition,
    /// Conservative state; finalization is the caller's responsibility.
    pub status: TerminalExitTrialStatus,
    /// First failure, safe cancellation, or outstanding external verification.
    pub outcome: TerminalExitTrialSessionOutcome,
    /// Additional exit error when another failure was already recorded.
    pub cleanup_error: Option<Error>,
}

impl TerminalExitTrialSessionReport {
    const fn pending(status: TerminalExitTrialStatus) -> Self {
        Self {
            session: None,
            identity: None,
            gateway_mode: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: TerminalExitTrialWriteDisposition::NotAttempted,
            status,
            outcome: TerminalExitTrialSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed session ID: apply 1, separate-session verification 2.
    /// This number alone does not prove a fresh connection or physical owner.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = TerminalExitTrialSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

struct Failure {
    stage: TerminalExitTrialSessionStage,
    error: TerminalExitTrialSessionError,
}

fn failure(
    stage: TerminalExitTrialSessionStage,
    error: impl Into<TerminalExitTrialSessionError>,
) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: TerminalExitTrialSession) -> NonZeroU64 {
    match session {
        TerminalExitTrialSession::Apply => NonZeroU64::MIN,
        TerminalExitTrialSession::Verify => NonZeroU64::MIN.saturating_add(1),
    }
}

const fn expected_gateway(session: TerminalExitTrialSession) -> DvGatewayMode {
    match session {
        TerminalExitTrialSession::Apply => DvGatewayMode::Terminal,
        TerminalExitTrialSession::Verify => DvGatewayMode::Off,
    }
}

fn cancelled(trial: &TerminalExitTrial, should_cancel: &mut impl FnMut() -> bool) -> bool {
    trial.status() == TerminalExitTrialStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Run one session of a separately approved, fixed Terminal-exit experiment.
    ///
    /// This is not a generally qualified Terminal setter. The immutable trial
    /// pins firmware/type, PM Off, panel routing, and the exact captured Off
    /// target and guard pages. The caller must separately establish approval,
    /// main-unit USB at 9600 baud, baseline provenance, and physical continuity,
    /// and wrap the transport with fail-closed raw capture. The generic schema
    /// write gate is unchanged. No callsign, PM, route, or RF command is sent.
    ///
    /// A fresh complete identity and the required Gateway state are checked
    /// before entry. Every session reads format, control, routing, and target,
    /// in that order. The apply session requires the entire target to equal
    /// the captured Off page with only its mode byte changed to Terminal. This
    /// expectation is not treated as an observed before-image until it matches
    /// that complete fresh read. No changed byte is merged or ignored.
    ///
    /// Only successful durable synchronization by `before_write` permits the
    /// single full-page write restoring Gateway Off. Immediate readback covers
    /// every byte. A separate session verifies the Off page without writing.
    /// After each E/ACK, the caller must close/drop, obtain fresh matching CAT
    /// identity and Gateway Off, close, and synchronize complete evidence before
    /// recording [`TerminalExitTrialEvent::SessionFinalized`]. This method does
    /// not perform those external steps or claim that they succeeded.
    ///
    /// Known-boundary comparison or journal failures permit detached exit;
    /// incomplete exchanges permit no speculative E, CAT, or rollback. No old
    /// handle operation follows exit ACK here. Hardware qualification of this
    /// write and recovery path remains separate from these software checks.
    ///
    /// # Cancellation
    ///
    /// Await to completion, never drop an in-flight future. Cooperative
    /// cancellation applies before the write intent at complete boundaries.
    /// After intent, safe completion and verification remain owed; failures
    /// halt the engine without erasing possible change or retrying a write.
    pub async fn run_approved_terminal_exit_trial_session_until_exit(
        &mut self,
        trial: &mut TerminalExitTrial,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&TerminalExitTrial) -> Result<(), std::io::Error>,
    ) -> TerminalExitTrialSessionReport {
        let mut report = TerminalExitTrialSessionReport::pending(trial.status());
        if let Err(error) = self
            .run_terminal_exit_session(trial, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            trial.halt();
            report.fail(error);
        }
        if matches!(report.outcome, TerminalExitTrialSessionOutcome::Cancelled) {
            trial.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_terminal_exit_session(&mut report).await;
        }
        if matches!(
            report.outcome,
            TerminalExitTrialSessionOutcome::Failed { .. }
        ) {
            trial.halt();
        }
        report.status = trial.status();
        report
    }

    async fn run_terminal_exit_session(
        &mut self,
        trial: &mut TerminalExitTrial,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&TerminalExitTrial) -> Result<(), std::io::Error>,
        report: &mut TerminalExitTrialSessionReport,
    ) -> Result<(), Failure> {
        use TerminalExitTrialSessionStage as Stage;

        let phase = trial
            .next_session()
            .map_err(|error| failure(Stage::Preparation, error))?;
        report.session = Some(phase);
        if cancelled(trial, should_cancel) {
            report.outcome = TerminalExitTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let identity = self
            .identify()
            .await
            .map_err(|error| failure(Stage::Identity, error))?;
        report.identity = Some(identity.clone());
        trial
            .validate_identity(&identity)
            .map_err(|error| failure(Stage::Identity, error))?;
        if cancelled(trial, should_cancel) {
            report.outcome = TerminalExitTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        let gateway_mode = self
            .get_dv_gateway_mode()
            .await
            .map_err(|error| failure(Stage::Gateway, error))?;
        report.gateway_mode = Some(gateway_mode);
        let expected = expected_gateway(phase);
        if gateway_mode != expected {
            return Err(failure(
                Stage::Gateway,
                TerminalExitTrialError::GatewayModeMismatch {
                    expected,
                    actual: gateway_mode,
                },
            ));
        }
        if cancelled(trial, should_cancel) {
            report.outcome = TerminalExitTrialSessionOutcome::Cancelled;
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_terminal_exit_session(trial, should_cancel, report)
            .await?;
        if matches!(report.outcome, TerminalExitTrialSessionOutcome::Cancelled) {
            return Ok(());
        }
        if phase == TerminalExitTrialSession::Apply {
            before_write(trial).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    TerminalExitTrialSessionError::DurableIntent(error),
                )
            })?;
            trial
                .record(TerminalExitTrialEvent::DurableWriteIntent {
                    id: NonZeroU64::MIN,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_terminal_exit(trial, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_terminal_exit_segment(trial.page(), Stage::ImmediateReadback, report)
                .await?;
            trial
                .record(TerminalExitTrialEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_terminal_exit_session(
        &mut self,
        trial: &mut TerminalExitTrial,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut TerminalExitTrialSessionReport,
    ) -> Result<(), Failure> {
        use TerminalExitTrialSessionStage as Stage;

        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        for page in [
            format_page,
            trial.control_page_spec(),
            trial.routing_page_spec(),
            trial.page(),
        ] {
            if cancelled(trial, should_cancel) {
                report.outcome = TerminalExitTrialSessionOutcome::Cancelled;
                return Ok(());
            }
            let data = self
                .read_terminal_exit_segment(page, Stage::Read { page }, report)
                .await?;
            if page == format_page && data.get(2) != Some(&0) {
                return Err(failure(
                    Stage::FreshComparison,
                    TerminalExitTrialError::MemoryFormat {
                        actual: data.get(2).copied().unwrap_or(u8::MAX),
                    },
                ));
            }
        }
        let invalid = || {
            failure(
                Stage::FreshComparison,
                TerminalExitTrialError::UnexpectedEvent,
            )
        };
        let identity = report.identity.as_ref().ok_or_else(invalid)?;
        let phase = report.session.ok_or_else(invalid)?;
        let gateway_mode = report.gateway_mode.ok_or_else(invalid)?;
        let [_, control, routing, target] = report.segments.as_slice() else {
            return Err(invalid());
        };
        trial
            .record(TerminalExitTrialEvent::FreshSession {
                id: session_id(phase),
                identity,
                memory_format: 0,
                gateway_mode,
                whole_page: &target.data,
                control_page: &control.data,
                routing_page: &routing.data,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(trial, should_cancel) {
            report.outcome = TerminalExitTrialSessionOutcome::Cancelled;
        }
        Ok(())
    }

    async fn read_terminal_exit_segment(
        &mut self,
        page: Page,
        stage: TerminalExitTrialSessionStage,
        report: &mut TerminalExitTrialSessionReport,
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

    async fn write_terminal_exit(
        &mut self,
        trial: &TerminalExitTrial,
        report: &mut TerminalExitTrialSessionReport,
    ) -> Result<(), TerminalExitTrialSessionError> {
        self.require_mcp_ready()?;
        let identity = self
            .identity()
            .ok_or(TerminalExitTrialError::IdentityMismatch)?;
        trial.validate_identity(identity)?;
        if trial.page() != TerminalExitTrial::required_page()?
            || trial.status() != TerminalExitTrialStatus::PossiblyChanged
            || report.session != Some(TerminalExitTrialSession::Apply)
            || report.gateway_mode != Some(DvGatewayMode::Terminal)
        {
            return Err(TerminalExitTrialError::UnexpectedEvent.into());
        }
        if !report.segments.last().is_some_and(|segment| {
            segment.page == trial.page() && segment.data == trial.expected_active_page().as_slice()
        }) {
            return Err(TerminalExitTrialError::PageMismatch.into());
        }
        let mut frame = write_request(trial.page()).to_vec();
        frame.extend_from_slice(trial.off_page());
        report.write = TerminalExitTrialWriteDisposition::PossiblyDispatched;
        self.mark_mcp_uncertain();
        self.write_all(&frame).await?;
        let stage = "Terminal exit page write";
        let reply = self.read_exact(1, stage).await?;
        if reply.as_slice() != [ACK] {
            return Err(Error::Protocol(ProtocolError::MissingAck {
                stage,
                byte: reply.first().copied().unwrap_or_default(),
            })
            .into());
        }
        self.mark_mcp_ready();
        report.write = TerminalExitTrialWriteDisposition::Acknowledged;
        Ok(())
    }

    async fn finish_terminal_exit_session(&mut self, report: &mut TerminalExitTrialSessionReport) {
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
                    TerminalExitTrialSessionOutcome::Failed { .. }
                ) {
                    report.cleanup_error = Some(error);
                } else {
                    report.fail(failure(TerminalExitTrialSessionStage::Exit, error));
                }
            }
        }
    }
}
