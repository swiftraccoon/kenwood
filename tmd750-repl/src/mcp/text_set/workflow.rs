//! Exactly one PM1 update followed by independent fresh-session verification.

use std::fs::File;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{Pm1NameUpdate, Pm1NameUpdateEvent, Pm1NameUpdateStatus};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    McpProbeExit, Pm1NameUpdateSessionOutcome, Pm1NameUpdateSessionReport,
    Pm1NameUpdateSessionStage, Pm1NameUpdateWriteDisposition, Radio,
};
use serde::Serialize;

use super::super::capture::{CaptureTransport, Recorder, TranscriptSummary};
use super::super::reconnect::{self, Backend, PostExitVerification, SkipReason};
use super::super::{ExitDisposition, Failure, IdentityEvidence, SegmentEvidence, close_transport};
use super::UpdateStatus;
use super::journal::UpdateJournal;
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
    Entry,
    Read { address: u32, length: usize },
    FreshComparison,
    DurableIntent,
    Write,
    ImmediateReadback,
    Exit,
}

impl From<Pm1NameUpdateSessionStage> for Stage {
    fn from(stage: Pm1NameUpdateSessionStage) -> Self {
        match stage {
            Pm1NameUpdateSessionStage::Preparation => Self::Preparation,
            Pm1NameUpdateSessionStage::Identity => Self::Identity,
            Pm1NameUpdateSessionStage::Entry => Self::Entry,
            Pm1NameUpdateSessionStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            Pm1NameUpdateSessionStage::FreshComparison => Self::FreshComparison,
            Pm1NameUpdateSessionStage::DurableIntent => Self::DurableIntent,
            Pm1NameUpdateSessionStage::Write => Self::Write,
            Pm1NameUpdateSessionStage::ImmediateReadback => Self::ImmediateReadback,
            Pm1NameUpdateSessionStage::Exit => Self::Exit,
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

impl From<Pm1NameUpdateWriteDisposition> for WriteDisposition {
    fn from(write: Pm1NameUpdateWriteDisposition) -> Self {
        match write {
            Pm1NameUpdateWriteDisposition::NotAttempted => Self::NotAttempted,
            Pm1NameUpdateWriteDisposition::PossiblyDispatched => Self::PossiblyDispatched,
            Pm1NameUpdateWriteDisposition::Acknowledged => Self::Acknowledged,
        }
    }
}

#[derive(Debug, Serialize)]
struct CoreEvidence {
    session_id: Option<NonZeroU64>,
    identity: Option<IdentityEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: ExitDisposition,
    write: WriteDisposition,
    status: UpdateStatus,
    outcome: Outcome,
    cleanup_error: Option<Failure>,
}

impl From<&Pm1NameUpdateSessionReport> for CoreEvidence {
    fn from(report: &Pm1NameUpdateSessionReport) -> Self {
        Self {
            session_id: report.session_id(),
            identity: report.identity.as_ref().map(IdentityEvidence::from),
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
            write: report.write.into(),
            status: report.status.into(),
            outcome: match &report.outcome {
                Pm1NameUpdateSessionOutcome::AwaitingCatVerification => {
                    Outcome::AwaitingCatVerification
                }
                Pm1NameUpdateSessionOutcome::Cancelled => Outcome::Cancelled,
                Pm1NameUpdateSessionOutcome::Failed { stage, error } => Outcome::Failed {
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
    pub(super) fn succeeded(&self, update: &Pm1NameUpdate) -> bool {
        update.status() == Pm1NameUpdateStatus::VerifiedAcrossSessions
            && self.sessions.len() == 2
            && self.sessions.iter().all(SessionEvidence::succeeded)
            && self.finalization_error.is_none()
    }

    pub(super) fn print_failures(&self) {
        for (index, session) in self.sessions.iter().enumerate() {
            if let Some(core) = &session.core {
                if let Outcome::Failed { stage, error } = &core.outcome {
                    output::error(format_args!(
                        "PM1 update session {} failed at {stage:?}: {error}",
                        index + 1
                    ));
                }
                if let Some(error) = &core.cleanup_error {
                    output::error(format_args!("Additional MCP exit failure: {error}"));
                }
                if let Some(guidance) = exit_guidance(&core.exit) {
                    output::error(format_args!("{guidance}"));
                }
            }
            for (label, error) in [
                ("connection open", &session.open_error),
                ("connection close", &session.close_error),
                ("capture synchronization", &session.synchronization_error),
            ] {
                if let Some(error) = error {
                    output::error(format_args!("PM1 update {label} failed: {error}"));
                }
            }
            if !session.post_exit.succeeded() {
                output::error(format_args!(
                    "PM1 update post-exit verification: {}.",
                    session.post_exit.outcome
                ));
            }
        }
        if let Some(error) = &self.finalization_error {
            output::error(format_args!(
                "PM1 update session finalization failed: {error}"
            ));
        }
    }
}

const fn exit_guidance(exit: &ExitDisposition) -> Option<&'static str> {
    match exit {
        ExitDisposition::RecoveryRequired | ExitDisposition::NotAcknowledged => Some(
            "Programming exit is unconfirmed. Retain the journal and transcripts, and fully power-cycle the radio before reconnecting. A power cycle does not establish which name is stored; do not retry or restore blindly.",
        ),
        ExitDisposition::NotEntered | ExitDisposition::Acknowledged => None,
    }
}

pub(super) async fn run(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    update: &mut Pm1NameUpdate,
    journal: &mut UpdateJournal,
    captures: [SessionCaptures; 2],
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult {
        sessions: Vec::new(),
        finalization_error: None,
    };
    for captures in captures {
        output::line(format_args!(
            "PM1 update session {} of 2: fresh identity, full-page comparison, and independent exit verification.",
            result.sessions.len() + 1
        ));
        let session = run_session(
            backend, endpoint, baud, update, journal, captures, cancelled,
        )
        .await;
        let succeeded = session.succeeded();
        let id = session.core.as_ref().and_then(|core| core.session_id);
        let durable = journal.evidence(&session);
        result.sessions.push(session);
        if let Err(error) = durable {
            result.finalization_error = Some(Failure::from_error(&error));
            update.halt();
            break;
        }
        if !succeeded {
            update.halt();
            break;
        }
        let finalized = id
            .ok_or_else(|| std::io::Error::other("complete PM1 update session lacks its fixed ID"))
            .and_then(|id| {
                update
                    .record(Pm1NameUpdateEvent::SessionFinalized { id })
                    .map_err(std::io::Error::other)
            });
        if let Err(error) = finalized {
            result.finalization_error = Some(Failure::from_error(&error));
            update.halt();
            break;
        }
    }
    result
}

async fn run_session(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    update: &mut Pm1NameUpdate,
    journal: &mut UpdateJournal,
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
    radio.set_cat_baud(baud);
    let report = radio
        .set_pm1_name_session_until_exit(
            update,
            || cancelled.load(Ordering::Relaxed),
            |update| journal.intent(update),
        )
        .await;
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
        Err(SkipReason::OriginalUpdateIncomplete)
    } else {
        report
            .identity
            .as_ref()
            .ok_or(SkipReason::OriginalUpdateIncomplete)
    };
    // After any possible write, user cancellation cannot abandon verification.
    // Required capture independently stops protocol traffic after capture loss.
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

#[cfg(all(test, unix))]
mod tests;
