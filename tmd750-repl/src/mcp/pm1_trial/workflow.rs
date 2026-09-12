//! Complete-session orchestration; no live future is dropped for cancellation.

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod my1_tests;

use std::fs::File;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicBool;

#[cfg(all(test, unix))]
use kenwood_tmd750::memory::PmNameTrial;
use kenwood_tmd750::memory::{PmNameTrialStatus, PmNameTrialWrite};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    DvGatewayMode, McpProbeExit, PmNameTrialSessionOutcome, PmNameTrialSessionReport,
    PmNameTrialSessionStage, PmNameTrialWriteDisposition, Radio,
};
use serde::Serialize;

use super::super::capture::{CaptureTransport, Recorder, TranscriptSummary};
use super::super::reconnect::{self, Backend, PostExitVerification, SkipReason};
use super::super::{ExitDisposition, Failure, IdentityEvidence, SegmentEvidence, close_transport};
use super::RestorationStatus;
use super::journal::Journal;
use super::target::Trial;
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
    GatewayGuard,
    Entry,
    Read { address: u32, length: usize },
    FreshComparison,
    DurableIntent,
    Write,
    ImmediateReadback,
    Exit,
}

impl From<PmNameTrialSessionStage> for Stage {
    fn from(stage: PmNameTrialSessionStage) -> Self {
        match stage {
            PmNameTrialSessionStage::Preparation => Self::Preparation,
            PmNameTrialSessionStage::Identity => Self::Identity,
            PmNameTrialSessionStage::GatewayGuard => Self::GatewayGuard,
            PmNameTrialSessionStage::Entry => Self::Entry,
            PmNameTrialSessionStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            PmNameTrialSessionStage::FreshComparison => Self::FreshComparison,
            PmNameTrialSessionStage::DurableIntent => Self::DurableIntent,
            PmNameTrialSessionStage::Write => Self::Write,
            PmNameTrialSessionStage::ImmediateReadback => Self::ImmediateReadback,
            PmNameTrialSessionStage::Exit => Self::Exit,
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

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum WriteKind {
    Rename,
    Restore,
}

impl From<PmNameTrialWrite> for WriteKind {
    fn from(write: PmNameTrialWrite) -> Self {
        match write {
            PmNameTrialWrite::Rename => Self::Rename,
            PmNameTrialWrite::Restore => Self::Restore,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WriteDisposition {
    NotAttempted,
    PossiblyDispatched { write: WriteKind },
    Acknowledged { write: WriteKind },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_mode: Option<GatewayEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: ExitDisposition,
    write: WriteDisposition,
    restoration: RestorationStatus,
    outcome: Outcome,
    cleanup_error: Option<Failure>,
}

impl From<&PmNameTrialSessionReport> for CoreEvidence {
    fn from(report: &PmNameTrialSessionReport) -> Self {
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
                PmNameTrialWriteDisposition::NotAttempted => WriteDisposition::NotAttempted,
                PmNameTrialWriteDisposition::PossiblyDispatched { write } => {
                    WriteDisposition::PossiblyDispatched {
                        write: write.into(),
                    }
                }
                PmNameTrialWriteDisposition::Acknowledged { write } => {
                    WriteDisposition::Acknowledged {
                        write: write.into(),
                    }
                }
            },
            restoration: report.status.into(),
            outcome: match &report.outcome {
                PmNameTrialSessionOutcome::AwaitingCatVerification => {
                    Outcome::AwaitingCatVerification
                }
                PmNameTrialSessionOutcome::Cancelled => Outcome::Cancelled,
                PmNameTrialSessionOutcome::Failed { stage, error } => Outcome::Failed {
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
    fn succeeded(&self) -> bool {
        self.open_error.is_none()
            && self.close_error.is_none()
            && self.synchronization_error.is_none()
            && self.transcript.complete
            && self.post_exit.succeeded()
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
    pub(super) fn succeeded(&self, trial: &impl Trial) -> bool {
        trial.status() == PmNameTrialStatus::RestorationVerified
            && self.sessions.len() == 3
            && self.sessions.iter().all(SessionEvidence::succeeded)
            && self.finalization_error.is_none()
    }

    pub(super) fn print_failures(&self) {
        for (index, session) in self.sessions.iter().enumerate() {
            if let Some(core) = &session.core {
                if let Outcome::Failed { stage, error } = &core.outcome {
                    output::error(format_args!(
                        "Text-trial session {} failed at {stage:?}: {error}",
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
                    output::error(format_args!("Text-trial {label} failed: {error}"));
                }
            }
            if !session.post_exit.succeeded() {
                output::error(format_args!(
                    "Text-trial post-exit verification: {}.",
                    session.post_exit.outcome
                ));
            }
        }
        if let Some(error) = &self.finalization_error {
            output::error(format_args!(
                "Text-trial session finalization failed: {error}"
            ));
        }
    }
}

pub(super) async fn run(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    trial: &mut impl Trial,
    journal: &mut Journal,
    captures: Vec<SessionCaptures>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult {
        sessions: Vec::new(),
        finalization_error: None,
    };
    for captures in captures {
        output::line(format_args!(
            "Text-trial session {} of 3: fresh identity, full-page comparison, and independent post-exit verification.",
            result.sessions.len() + 1
        ));
        let session =
            run_session(backend, endpoint, baud, trial, journal, captures, cancelled).await;
        let succeeded = session.succeeded();
        let id = session.core.as_ref().and_then(|core| core.session_id);
        let durable = journal.evidence(&session);
        result.sessions.push(session);
        if let Err(error) = durable {
            result.finalization_error = Some(Failure::from_error(&error));
            trial.halt();
            break;
        }
        if !succeeded {
            trial.halt();
            break;
        }
        let finalized = id
            .ok_or_else(|| std::io::Error::other("complete trial session lacks its fixed ID"))
            .and_then(|id| trial.finalize_session(id).map_err(std::io::Error::other));
        if let Err(error) = finalized {
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
    trial: &mut impl Trial,
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
    if let Err(error) = original.synchronize() {
        result.synchronization_error = Some(Failure::from_error(&error));
        result.transcript = original.summary();
        return result;
    }
    let connection = match backend.open(endpoint, baud) {
        Ok(connection) => connection,
        Err(error) => {
            result.open_error = Some(Failure::from_error(&error));
            return result;
        }
    };
    let mut radio = Radio::new(CaptureTransport::required(connection, original));
    let report = trial.run_session(&mut radio, cancelled, journal).await;
    let mut transport = radio.into_transport();
    result.close_error = close_transport(&mut transport).await;
    let mut original = transport.into_recorder();
    result.synchronization_error = original
        .synchronize()
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    result.transcript = original.summary();
    let eligibility = if result.close_error.is_some() {
        Err(SkipReason::OriginalCloseFailed)
    } else if !result.transcript.complete {
        Err(SkipReason::OriginalCaptureIncomplete)
    } else if report.exit != McpProbeExit::Acknowledged {
        Err(SkipReason::OriginalTrialIncomplete)
    } else {
        report
            .identity
            .as_ref()
            .ok_or(SkipReason::OriginalTrialIncomplete)
    };
    // Signal cancellation must not strand a possible rename; capture failures
    // remain independently fatal in the required recorder and transport.
    result.post_exit = match eligibility {
        Ok(identity) => {
            reconnect::verify_required(
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
