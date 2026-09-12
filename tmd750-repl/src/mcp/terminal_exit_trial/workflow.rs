//! Fixed two-session workflow with required captures and fresh Gateway evidence.

use std::fs::File;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{TerminalExitTrial, TerminalExitTrialEvent, TerminalExitTrialStatus};
use kenwood_tmd750::transport::{SerialCandidate, Transport};
use kenwood_tmd750::{
    DvGatewayMode, McpProbeExit, Radio, TerminalExitTrialSessionOutcome,
    TerminalExitTrialSessionReport, TerminalExitTrialSessionStage,
    TerminalExitTrialWriteDisposition,
};
use serde::Serialize;

use super::super::capture::{CaptureTransport, Event, Recorder, TranscriptSummary};
use super::super::reconnect::{self, Backend, PostExitVerification, SkipReason};
use super::super::{ExitDisposition, Failure, IdentityEvidence, SegmentEvidence, close_transport};
use super::Status;
use super::journal::Journal;
use crate::output;

#[derive(Debug)]
pub(super) struct SessionCaptures {
    pub(super) original: Recorder<File>,
    pub(super) post_exit: Recorder<File>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Stage {
    Preparation,
    Identity,
    Gateway,
    Entry,
    Read { address: u32, length: usize },
    FreshComparison,
    DurableIntent,
    Write,
    ImmediateReadback,
    Exit,
}

impl From<TerminalExitTrialSessionStage> for Stage {
    fn from(stage: TerminalExitTrialSessionStage) -> Self {
        match stage {
            TerminalExitTrialSessionStage::Preparation => Self::Preparation,
            TerminalExitTrialSessionStage::Identity => Self::Identity,
            TerminalExitTrialSessionStage::Gateway => Self::Gateway,
            TerminalExitTrialSessionStage::Entry => Self::Entry,
            TerminalExitTrialSessionStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            TerminalExitTrialSessionStage::FreshComparison => Self::FreshComparison,
            TerminalExitTrialSessionStage::DurableIntent => Self::DurableIntent,
            TerminalExitTrialSessionStage::Write => Self::Write,
            TerminalExitTrialSessionStage::ImmediateReadback => Self::ImmediateReadback,
            TerminalExitTrialSessionStage::Exit => Self::Exit,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    AwaitingCatVerification,
    Cancelled,
    Failed { stage: Stage, error: Failure },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum WriteDisposition {
    NotAttempted,
    PossiblyDispatched,
    Acknowledged,
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum GatewayEvidence {
    Off { raw: u8 },
    Terminal { raw: u8 },
    Unqualified { raw: u8 },
}

impl From<DvGatewayMode> for GatewayEvidence {
    fn from(mode: DvGatewayMode) -> Self {
        match mode {
            DvGatewayMode::Off => Self::Off { raw: 0 },
            DvGatewayMode::Terminal => Self::Terminal { raw: 2 },
            DvGatewayMode::Unqualified(raw) => Self::Unqualified { raw },
        }
    }
}

#[derive(Debug, Serialize)]
struct CoreEvidence {
    session_id: Option<NonZeroU64>,
    identity: Option<IdentityEvidence>,
    gateway_mode: Option<GatewayEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: ExitDisposition,
    write: WriteDisposition,
    gateway_exit: Status,
    outcome: Outcome,
    cleanup_error: Option<Failure>,
}

impl From<&TerminalExitTrialSessionReport> for CoreEvidence {
    fn from(report: &TerminalExitTrialSessionReport) -> Self {
        Self {
            session_id: report.session_id(),
            identity: report.identity.as_ref().map(IdentityEvidence::from),
            gateway_mode: report.gateway_mode.map(GatewayEvidence::from),
            entry_reply: report.entry_reply.clone(),
            segments: report
                .segments
                .iter()
                .map(|segment| SegmentEvidence {
                    address: segment.page.address().as_u32(),
                    length: segment.page.len(),
                    data: segment.data.clone(),
                })
                .collect(),
            exit: report.exit.into(),
            write: match report.write {
                TerminalExitTrialWriteDisposition::NotAttempted => WriteDisposition::NotAttempted,
                TerminalExitTrialWriteDisposition::PossiblyDispatched => {
                    WriteDisposition::PossiblyDispatched
                }
                TerminalExitTrialWriteDisposition::Acknowledged => WriteDisposition::Acknowledged,
            },
            gateway_exit: report.status.into(),
            outcome: match &report.outcome {
                TerminalExitTrialSessionOutcome::AwaitingCatVerification => {
                    Outcome::AwaitingCatVerification
                }
                TerminalExitTrialSessionOutcome::Cancelled => Outcome::Cancelled,
                TerminalExitTrialSessionOutcome::Failed { stage, error } => Outcome::Failed {
                    stage: (*stage).into(),
                    error: Failure::from_error(error),
                },
            },
            cleanup_error: report
                .cleanup_error
                .as_ref()
                .map(|error| Failure::from_error(error)),
        }
    }
}

impl CoreEvidence {
    /// The Apply driver's second complete target read follows its sole W/ACK.
    fn immediate_off_readback(&self, trial: &TerminalExitTrial) -> bool {
        self.session_id == Some(NonZeroU64::MIN)
            && matches!(self.write, WriteDisposition::Acknowledged)
            && self
                .segments
                .iter()
                .filter(|segment| segment.address == trial.page().address().as_u32())
                .nth(1)
                .is_some_and(|segment| {
                    segment.length == trial.page().len()
                        && segment.data.as_slice() == trial.off_page().as_slice()
                })
    }
}

#[derive(Debug, Serialize)]
struct SessionEvidence {
    core: Option<CoreEvidence>,
    transcript: TranscriptSummary,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    synchronization_error: Option<Failure>,
    post_exit: PostExitVerification,
}

impl SessionEvidence {
    /// Retain the first durability failure and the current capture summary.
    fn synchronize_original(&mut self, original: &mut Recorder<File>) {
        if let Err(error) = original.synchronize()
            && self.synchronization_error.is_none()
        {
            self.synchronization_error = Some(Failure::from_error(&error));
        }
        self.transcript = original.summary();
    }

    /// Release even an unproven handle before retaining its final transcript.
    async fn close_original(&mut self, mut transport: CaptureTransport<impl Transport, File>) {
        self.close_error = close_transport(&mut transport).await;
        let mut original = transport.into_recorder();
        self.synchronize_original(&mut original);
    }

    /// Admit CAT only after durable opening evidence; otherwise only retire.
    async fn admit_original<T: Transport>(
        &mut self,
        connection: T,
        original: Recorder<File>,
    ) -> Option<Radio<CaptureTransport<T, File>>> {
        let transport = CaptureTransport::required(connection, original);
        if self.synchronization_error.is_some() {
            self.close_original(transport).await;
            None
        } else {
            Some(Radio::new(transport))
        }
    }

    fn succeeded(&self) -> bool {
        self.open_error.is_none()
            && self.close_error.is_none()
            && self.synchronization_error.is_none()
            && self.transcript.complete
            && self.post_exit.gateway_off_evidence().is_some()
            && self.core.as_ref().is_some_and(|core| {
                matches!(core.outcome, Outcome::AwaitingCatVerification)
                    && core.cleanup_error.is_none()
            })
    }
}

#[derive(Debug, Serialize)]
pub(super) struct WorkflowResult {
    sessions: Vec<SessionEvidence>,
    finalization_error: Option<Failure>,
}

impl WorkflowResult {
    pub(super) fn succeeded(&self, trial: &TerminalExitTrial) -> bool {
        trial.status() == TerminalExitTrialStatus::OffVerifiedAcrossSessions
            && self.sessions.len() == 2
            && self.sessions.iter().all(SessionEvidence::succeeded)
            && self.finalization_error.is_none()
    }

    /// Explain retained observations without upgrading the trial's final status.
    pub(super) fn print_observations(&self, trial: &TerminalExitTrial) {
        for line in self.observation_lines(trial) {
            output::line(format_args!("{line}"));
        }
    }

    fn observation_lines(&self, trial: &TerminalExitTrial) -> Vec<String> {
        let mut lines = vec![
            "Recorded session observations are historical, not a current-state check or proof that the whole trial completed.".to_owned(),
        ];
        for (index, session) in self.sessions.iter().enumerate() {
            let number = index + 1;
            if !session.transcript.complete || session.synchronization_error.is_some() {
                lines.push(format!("Session {number}: original capture is incomplete or unsynchronized; inspect the retained failure."));
                continue;
            }
            if let Some(core) = &session.core {
                let write = match core.write {
                    WriteDisposition::NotAttempted => "not attempted",
                    WriteDisposition::PossiblyDispatched => {
                        "possibly dispatched; no acknowledgment proved"
                    }
                    WriteDisposition::Acknowledged => "acknowledged",
                };
                lines.push(format!("Session {number} memory write: {write}."));
                if core.immediate_off_readback(trial) {
                    lines.push(format!("Session {number}: immediate acknowledged full Gateway-page readback matched the exact Off page."));
                }
            }
            if session.post_exit.gateway_off_evidence().is_some() {
                lines.push(format!("Session {number}: fresh matching CAT identity and Gateway Off were captured after MCP exit, followed by close."));
            }
        }
        lines
    }

    pub(super) fn print_failures(&self) {
        for (index, session) in self.sessions.iter().enumerate() {
            if let Some(core) = &session.core {
                if let Outcome::Failed { stage, error } = &core.outcome {
                    output::error(format_args!(
                        "Terminal exit session {} failed at {stage:?}: {error}",
                        index + 1
                    ));
                }
                if let Some(error) = &core.cleanup_error {
                    output::error(format_args!("Additional MCP exit failure: {error}"));
                }
            }
            for (label, error) in [
                ("connection open", &session.open_error),
                ("connection close", &session.close_error),
                ("capture synchronization", &session.synchronization_error),
            ] {
                if let Some(error) = error {
                    output::error(format_args!("Terminal exit {label} failed: {error}"));
                }
            }
            if session.post_exit.gateway_off_evidence().is_none() {
                output::error(format_args!(
                    "Terminal exit post-exit verification: {}.",
                    session.post_exit.outcome
                ));
            }
        }
        if let Some(error) = &self.finalization_error {
            output::error(format_args!("Terminal exit finalization failed: {error}"));
        }
    }
}

pub(super) async fn run(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    trial: &mut TerminalExitTrial,
    journal: &mut Journal,
    captures: [SessionCaptures; 2],
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult {
        sessions: Vec::new(),
        finalization_error: None,
    };
    for captures in captures {
        output::line(format_args!(
            "Terminal exit session {} of 2: fresh identity, full-page guards, and independent CAT Off verification.",
            result.sessions.len() + 1
        ));
        let session =
            run_session(backend, endpoint, baud, trial, journal, captures, cancelled).await;
        let durable = journal.evidence(&session);
        let finalization = durable.and_then(|()| {
            if !session.succeeded() {
                return Err(std::io::Error::other(
                    "Terminal exit session evidence is incomplete",
                ));
            }
            let id = session
                .core
                .as_ref()
                .and_then(|core| core.session_id)
                .ok_or_else(|| std::io::Error::other("complete exit session lacks its fixed ID"))?;
            let (identity, gateway_mode) =
                session.post_exit.gateway_off_evidence().ok_or_else(|| {
                    std::io::Error::other("fresh matching identity and Gateway Off are required")
                })?;
            trial
                .record(TerminalExitTrialEvent::SessionFinalized {
                    id,
                    identity,
                    gateway_mode,
                })
                .map_err(std::io::Error::other)
        });
        result.sessions.push(session);
        if let Err(error) = finalization {
            result.finalization_error = Some(Failure::from_error(&error));
            trial.halt();
            break;
        }
    }
    result
}

async fn run_session(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    trial: &mut TerminalExitTrial,
    journal: &mut Journal,
    captures: SessionCaptures,
    cancelled: &AtomicBool,
) -> SessionEvidence {
    let SessionCaptures {
        mut original,
        post_exit,
    } = captures;
    let mut result = SessionEvidence {
        core: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        synchronization_error: None,
        post_exit: PostExitVerification::skipped(
            SkipReason::OriginalOpenFailed,
            post_exit.summary(),
        ),
    };
    if cancelled.load(Ordering::Relaxed) && trial.status() == TerminalExitTrialStatus::NotWritten {
        result.post_exit =
            PostExitVerification::skipped(SkipReason::Cancelled, post_exit.summary());
        return result;
    }
    let Some(connection) = open_original(backend, endpoint, baud, &mut original, &mut result)
    else {
        if result.synchronization_error.is_some() {
            result.post_exit = PostExitVerification::skipped(
                SkipReason::OriginalCaptureIncomplete,
                post_exit.summary(),
            );
        }
        return result;
    };
    let Some(mut radio) = result.admit_original(connection, original).await else {
        result.post_exit = PostExitVerification::skipped(
            SkipReason::OriginalCaptureIncomplete,
            post_exit.summary(),
        );
        return result;
    };
    let report = radio
        .run_approved_terminal_exit_trial_session_until_exit(
            trial,
            || cancelled.load(Ordering::Relaxed),
            |trial| journal.intent(trial),
        )
        .await;
    result.close_original(radio.into_transport()).await;
    let eligibility = if result.close_error.is_some() {
        Err(SkipReason::OriginalCloseFailed)
    } else if !result.transcript.complete || result.synchronization_error.is_some() {
        Err(SkipReason::OriginalCaptureIncomplete)
    } else if report.exit != McpProbeExit::Acknowledged {
        Err(SkipReason::OriginalTrialIncomplete)
    } else {
        report
            .identity
            .as_ref()
            .ok_or(SkipReason::OriginalTrialIncomplete)
    };
    // Required cleanup must not be abandoned after the sole possible write.
    // Capture failures remain independently fatal; no write is retried here.
    result.post_exit = match eligibility {
        Ok(identity) => {
            reconnect::verify_required_gateway_off(
                backend,
                endpoint,
                baud,
                identity,
                post_exit,
                &AtomicBool::new(false),
            )
            .await
        }
        Err(reason) => PostExitVerification::skipped(reason, post_exit.summary()),
    };
    result.core = Some(CoreEvidence::from(&report));
    result
}

/// Capture and synchronize both opening boundaries before any protocol traffic.
/// A returned handle still needs closing when recording its opening failed.
fn open_original<B: Backend>(
    backend: &mut B,
    endpoint: &SerialCandidate,
    baud: u32,
    original: &mut Recorder<File>,
    result: &mut SessionEvidence,
) -> Option<B::Connection> {
    original.record(Event::OpenRequested {
        path: &endpoint.path,
        baud,
    });
    result.synchronize_original(original);
    if result.synchronization_error.is_some() {
        return None;
    }
    match backend.open(endpoint, baud) {
        Ok(connection) => {
            original.record(Event::OpenCompleted);
            result.synchronize_original(original);
            Some(connection)
        }
        Err(error) => {
            let failure = Failure::from_error(&error);
            original.record(Event::OpenFailed {
                error: failure.clone(),
            });
            result.open_error = Some(failure);
            result.synchronize_original(original);
            None
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
