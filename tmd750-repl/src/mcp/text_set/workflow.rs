//! Runs the write session, then the confirmation session.
//!
//! The first session compares the page, writes it and reads it back; the second
//! reopens on a fresh connection and re-reads the page.

use std::fs::File;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::{
    DvGatewayMode, McpProbeExit, Radio, TextFieldUpdateSessionOutcome,
    TextFieldUpdateSessionReport, TextFieldUpdateSessionStage, TextFieldUpdateWriteDisposition,
};
use kenwood_transport::Transport;
use serde::Serialize;

use super::super::link::{Opening, Refusal, WriteLink};
pub(super) use super::super::reconnect::PostExit;
use super::super::reconnect::SkipReason;
use super::super::{ExitDisposition, Failure, IdentityEvidence, SegmentEvidence, close_transport};
use super::UpdateStatus;
use super::journal::UpdateJournal;
use super::target::{Update, UpdateKind};
use crate::capture::{CaptureTransport, Recorder, TranscriptSummary};
use crate::native::opening::History;
use crate::output;

#[derive(Debug)]
pub(super) struct SessionCaptures {
    pub(super) original: Recorder<File>,
    pub(super) post_exit: Recorder<File>,
}

/// Second descriptor on the transcript file, fsynced before a write intent.
///
/// The first failure is retained even when a later synchronization succeeds.
pub(super) struct CaptureSynchronization {
    file: File,
    error: Option<Failure>,
    #[cfg(all(test, unix))]
    fail_next: bool,
}

impl CaptureSynchronization {
    pub(super) fn synchronize(&mut self) -> std::io::Result<()> {
        if let Some(error) = &self.error {
            return Err(std::io::Error::other(error.to_string()));
        }
        let result = self.synchronize_file();
        if let Err(error) = &result {
            self.error = Some(Failure::from_error(error));
        }
        result
    }

    #[cfg(all(test, unix))]
    fn synchronize_file(&mut self) -> std::io::Result<()> {
        if std::mem::take(&mut self.fail_next) {
            return Err(std::io::Error::other(
                "injected pre-write raw-capture synchronization failure",
            ));
        }
        self.file.sync_all()
    }

    #[cfg(not(all(test, unix)))]
    fn synchronize_file(&self) -> std::io::Result<()> {
        self.file.sync_all()
    }
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

impl From<TextFieldUpdateSessionStage> for Stage {
    fn from(stage: TextFieldUpdateSessionStage) -> Self {
        match stage {
            TextFieldUpdateSessionStage::Preparation => Self::Preparation,
            TextFieldUpdateSessionStage::Identity => Self::Identity,
            TextFieldUpdateSessionStage::GatewayGuard => Self::GatewayGuard,
            TextFieldUpdateSessionStage::Entry => Self::Entry,
            TextFieldUpdateSessionStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            TextFieldUpdateSessionStage::FreshComparison => Self::FreshComparison,
            TextFieldUpdateSessionStage::DurableIntent => Self::DurableIntent,
            TextFieldUpdateSessionStage::Write => Self::Write,
            TextFieldUpdateSessionStage::ImmediateReadback => Self::ImmediateReadback,
            TextFieldUpdateSessionStage::Exit => Self::Exit,
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

impl From<TextFieldUpdateWriteDisposition> for WriteDisposition {
    fn from(write: TextFieldUpdateWriteDisposition) -> Self {
        match write {
            TextFieldUpdateWriteDisposition::NotAttempted => Self::NotAttempted,
            TextFieldUpdateWriteDisposition::PossiblyDispatched => Self::PossiblyDispatched,
            TextFieldUpdateWriteDisposition::Acknowledged => Self::Acknowledged,
        }
    }
}

/// Observed DV Gateway mode, serialized as a state name plus the raw wire byte.
///
/// A value with no name serializes as `unqualified` with its byte preserved.
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
    status: UpdateStatus,
    outcome: Outcome,
    cleanup_error: Option<Failure>,
}

impl From<&TextFieldUpdateSessionReport> for CoreEvidence {
    fn from(report: &TextFieldUpdateSessionReport) -> Self {
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
            write: report.write.into(),
            status: report.status.into(),
            outcome: match &report.outcome {
                TextFieldUpdateSessionOutcome::AwaitingCatVerification => {
                    Outcome::AwaitingCatVerification
                }
                TextFieldUpdateSessionOutcome::Cancelled => Outcome::Cancelled,
                TextFieldUpdateSessionOutcome::Failed { stage, error } => Outcome::Failed {
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
    /// Native Bluetooth opening attempts; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    opening: Option<History>,
    /// Gateway mode read before programming entry; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_before: Option<GatewayEvidence>,
    open_error: Option<Failure>,
    /// The failed pre-entry check of the link; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    admission_error: Option<Failure>,
    close_error: Option<Failure>,
    synchronization_error: Option<Failure>,
    /// The failed hold of the original connection after the exit; absent over
    /// USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    hold_error: Option<Failure>,
    post_exit: PostExit,
}

impl SessionEvidence {
    /// Synchronize the transcript and store its summary, keeping the first
    /// synchronization failure.
    fn synchronize_original(&mut self, recorder: &mut Recorder<File>) {
        if let Err(error) = recorder.synchronize()
            && self.synchronization_error.is_none()
        {
            self.synchronization_error = Some(Failure::from_error(&error));
        }
        self.transcript = recorder.summary();
    }

    async fn close_original<T: Transport>(&mut self, mut transport: CaptureTransport<T, File>) {
        self.close_error = close_transport(&mut transport).await;
        let mut recorder = transport.into_recorder();
        self.synchronize_original(&mut recorder);
    }

    fn succeeded(&self) -> bool {
        self.open_error.is_none()
            && self.admission_error.is_none()
            && self.close_error.is_none()
            && self.synchronization_error.is_none()
            && self.hold_error.is_none()
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
    pub(super) fn succeeded(&self, update: &impl Update) -> bool {
        update.status() == UpdateStatus::VerifiedAcrossSessions
            && self.sessions.len() == 2
            && self.sessions.iter().all(|session| {
                session.succeeded() && update.kind().verification_succeeded(&session.post_exit)
            })
            && self.finalization_error.is_none()
    }

    pub(super) fn print_failures(&self, kind: UpdateKind) {
        for (index, session) in self.sessions.iter().enumerate() {
            if let Some(core) = &session.core {
                if let Outcome::Failed { stage, error } = &core.outcome {
                    output::error(format_args!(
                        "{} update session {} failed at {stage:?}: {error}",
                        kind.label(),
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
                ("pre-entry check", &session.admission_error),
                ("connection close", &session.close_error),
                ("capture synchronization", &session.synchronization_error),
                ("post-exit hold", &session.hold_error),
            ] {
                if let Some(error) = error {
                    output::error(format_args!(
                        "{} update {label} failed: {error}",
                        kind.label()
                    ));
                }
            }
            if !session.post_exit.succeeded() {
                output::error(format_args!(
                    "{} update post-exit verification: {}.",
                    kind.label(),
                    session.post_exit.outcome()
                ));
            }
        }
        if let Some(error) = &self.finalization_error {
            output::error(format_args!(
                "{} update session finalization failed: {error}",
                kind.label()
            ));
        }
    }
}

const fn exit_guidance(exit: &ExitDisposition) -> Option<&'static str> {
    match exit {
        ExitDisposition::RecoveryRequired | ExitDisposition::NotAcknowledged => Some(
            "Programming exit is unconfirmed. Retain the journal and transcripts, and fully power-cycle the radio before reconnecting. A power cycle does not report which text is stored; do not retry or restore blindly.",
        ),
        ExitDisposition::NotEntered | ExitDisposition::Acknowledged => None,
    }
}

pub(super) async fn run(
    link: &mut impl WriteLink,
    update: &mut impl Update,
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
            "{} update session {} of 2: fresh identity, full-page comparison, and independent exit verification.",
            update.kind().label(),
            result.sessions.len() + 1
        ));
        // The second session follows the first exit, which the link settles.
        let later = !result.sessions.is_empty();
        let session = run_session(link, update, journal, captures, cancelled, later).await;
        let succeeded =
            session.succeeded() && update.kind().verification_succeeded(&session.post_exit);
        let id = session.core.as_ref().and_then(|core| core.session_id);
        let durable = journal.evidence(&session);
        if let Err(error) = durable {
            result.sessions.push(session);
            result.finalization_error = Some(Failure::from_error(&error));
            update.halt();
            break;
        }
        if !succeeded {
            result.sessions.push(session);
            update.halt();
            break;
        }
        let finalized = id
            .ok_or_else(|| std::io::Error::other("complete text update session lacks its fixed ID"))
            .and_then(|id| {
                update
                    .finalize_session(id, &session.post_exit)
                    .map_err(std::io::Error::other)
            });
        result.sessions.push(session);
        if let Err(error) = finalized {
            result.finalization_error = Some(Failure::from_error(&error));
            update.halt();
            break;
        }
    }
    result
}

/// Run one programming session over `link`: open, admit, run the update's
/// session, hold after an acknowledged exit, close, and check a fresh
/// connection. `later` marks the session after the first exit.
async fn run_session(
    link: &mut impl WriteLink,
    update: &mut impl Update,
    journal: &mut UpdateJournal,
    captures: SessionCaptures,
    cancelled: &AtomicBool,
    later: bool,
) -> SessionEvidence {
    let SessionCaptures {
        original,
        mut post_exit,
    } = captures;
    let mut result = SessionEvidence {
        core: None,
        transcript: original.summary(),
        opening: None,
        gateway_before: None,
        open_error: None,
        admission_error: None,
        close_error: None,
        synchronization_error: None,
        hold_error: None,
        post_exit: link.skipped(SkipReason::OriginalOpenFailed, post_exit.summary()),
    };
    if cancelled.load(Ordering::Relaxed) && update.status() == UpdateStatus::NotWritten {
        result.post_exit = link.skipped(SkipReason::Cancelled, post_exit.summary());
        return result;
    }
    let synchronization_handle = match original.synchronization_handle() {
        Ok(file) => file,
        Err(error) => {
            result.synchronization_error = Some(Failure::from_error(&error));
            return result;
        }
    };
    let mut capture = CaptureSynchronization {
        file: synchronization_handle,
        error: None,
        #[cfg(all(test, unix))]
        fail_next: journal.take_raw_sync_failure_for_test(),
    };
    // Once the page may have been written, a cancellation signal no longer
    // stops the session's remaining verification; a capture failure still
    // stops all protocol traffic.
    let finish_verification = AtomicBool::new(false);
    let continuing = if update.status() == UpdateStatus::NotWritten {
        cancelled
    } else {
        &finish_verification
    };
    let Some(mut radio) =
        open_admitted(link, original, later, continuing, &post_exit, &mut result).await
    else {
        return result;
    };
    let report = update
        .run_session(&mut radio, cancelled, journal, &mut capture)
        .await;
    result.synchronization_error = capture.error;
    let continuing = if update.status() == UpdateStatus::NotWritten {
        cancelled
    } else {
        &finish_verification
    };
    let hold = if report.exit == McpProbeExit::Acknowledged {
        link.hold_after_exit(&mut post_exit, continuing).await
    } else {
        Ok(())
    };
    result.close_original(radio.into_transport()).await;
    result.post_exit = match (verification_identity(&result, journal, &report), hold) {
        (Err(reason), _) => link.skipped(reason, post_exit.summary()),
        (Ok(_), Err(error)) => {
            let reason = if error.kind() == std::io::ErrorKind::Interrupted {
                SkipReason::Cancelled
            } else {
                SkipReason::PostExitHoldFailed
            };
            result.hold_error = Some(Failure::from_error(&error));
            link.skipped(reason, post_exit.summary())
        }
        (Ok(identity), Ok(())) => {
            let gateway_off = update.kind() == UpdateKind::PmOffMy1;
            link.verify_post_exit(identity, gateway_off, post_exit, continuing)
                .await
        }
    };
    result.core = Some(CoreEvidence::from(&report));
    result
}

/// Open the session's connection and pass the link's pre-entry check,
/// recording every refusal in `result`; `None` when no programming session
/// may start.
async fn open_admitted<L: WriteLink>(
    link: &mut L,
    original: Recorder<File>,
    later: bool,
    continuing: &AtomicBool,
    post_exit: &Recorder<File>,
    result: &mut SessionEvidence,
) -> Option<Radio<CaptureTransport<L::Connection, File>>> {
    let opened = link.open_session(original, later, continuing).await;
    result.opening = link.take_opening_history();
    let mut transport = match opened {
        Opening::Opened(transport) => transport,
        Opening::Refused {
            mut recorder,
            refusal,
        } => {
            match refusal {
                Refusal::Capture(failure) => result.synchronization_error = Some(failure),
                Refusal::Cancelled => result.post_exit.skip(SkipReason::Cancelled),
                Refusal::Open(failure) => result.open_error = Some(failure),
            }
            result.synchronize_original(&mut recorder);
            return None;
        }
    };
    if let Err(error) = transport.synchronize() {
        result.synchronization_error = Some(Failure::from_error(&error));
        result.close_original(transport).await;
        result.post_exit = link.skipped(SkipReason::OriginalCaptureIncomplete, post_exit.summary());
        return None;
    }
    let mut radio = link.radio(transport);
    let admission = link.admit(&mut radio).await;
    result.gateway_before = admission.gateway.map(GatewayEvidence::from);
    if let Some(error) = admission.error {
        result.admission_error = Some(error);
        result.close_original(radio.into_transport()).await;
        result.post_exit = link.skipped(SkipReason::OriginalAdmissionFailed, post_exit.summary());
        return None;
    }
    Some(radio)
}

fn verification_identity<'a>(
    result: &SessionEvidence,
    journal: &UpdateJournal,
    report: &'a TextFieldUpdateSessionReport,
) -> Result<&'a kenwood_tmd750::Identity, SkipReason> {
    if result.close_error.is_some() {
        Err(SkipReason::OriginalCloseFailed)
    } else if !result.transcript.complete || result.synchronization_error.is_some() {
        Err(SkipReason::OriginalCaptureIncomplete)
    } else if journal.ensure_complete().is_err() {
        Err(SkipReason::OriginalUpdateJournalIncomplete)
    } else if report.exit != McpProbeExit::Acknowledged {
        Err(SkipReason::OriginalUpdateIncomplete)
    } else {
        report
            .identity
            .as_ref()
            .ok_or(SkipReason::OriginalUpdateIncomplete)
    }
}

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, unix))]
mod my1_tests;
