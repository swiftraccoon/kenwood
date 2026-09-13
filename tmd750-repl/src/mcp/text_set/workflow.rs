//! One typed text update followed by independent fresh-session verification.

use std::fs::File;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    DvGatewayMode, McpProbeExit, My1CallsignUpdateSessionOutcome, My1CallsignUpdateSessionReport,
    My1CallsignUpdateSessionStage, My1CallsignUpdateWriteDisposition, Pm1NameUpdateSessionOutcome,
    Pm1NameUpdateSessionReport, Pm1NameUpdateSessionStage, Pm1NameUpdateWriteDisposition, Radio,
};
use kenwood_transport::Transport;
use serde::Serialize;

use super::super::reconnect::{self, Backend, PostExitVerification, SkipReason};
use super::super::{ExitDisposition, Failure, IdentityEvidence, SegmentEvidence, close_transport};
use super::UpdateStatus;
use super::journal::UpdateJournal;
use super::target::{SessionReport, Update, UpdateKind};
use crate::capture::{CaptureTransport, Event, Recorder, TranscriptSummary};
use crate::output;

#[derive(Debug)]
pub(super) struct SessionCaptures {
    pub(super) original: Recorder<File>,
    pub(super) post_exit: Recorder<File>,
}

/// A cloned descriptor used only to synchronize raw evidence before write intent.
/// The first failure remains material even if later cleanup synchronization works.
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

impl From<My1CallsignUpdateSessionStage> for Stage {
    fn from(stage: My1CallsignUpdateSessionStage) -> Self {
        match stage {
            My1CallsignUpdateSessionStage::Preparation => Self::Preparation,
            My1CallsignUpdateSessionStage::Identity => Self::Identity,
            My1CallsignUpdateSessionStage::GatewayGuard => Self::GatewayGuard,
            My1CallsignUpdateSessionStage::Entry => Self::Entry,
            My1CallsignUpdateSessionStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            My1CallsignUpdateSessionStage::FreshComparison => Self::FreshComparison,
            My1CallsignUpdateSessionStage::DurableIntent => Self::DurableIntent,
            My1CallsignUpdateSessionStage::Write => Self::Write,
            My1CallsignUpdateSessionStage::ImmediateReadback => Self::ImmediateReadback,
            My1CallsignUpdateSessionStage::Exit => Self::Exit,
        }
    }
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

impl From<My1CallsignUpdateWriteDisposition> for WriteDisposition {
    fn from(write: My1CallsignUpdateWriteDisposition) -> Self {
        match write {
            My1CallsignUpdateWriteDisposition::NotAttempted => Self::NotAttempted,
            My1CallsignUpdateWriteDisposition::PossiblyDispatched => Self::PossiblyDispatched,
            My1CallsignUpdateWriteDisposition::Acknowledged => Self::Acknowledged,
        }
    }
}

/// Preserve the observed raw Gateway value without serializing a guessed label.
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

impl From<&Pm1NameUpdateSessionReport> for CoreEvidence {
    fn from(report: &Pm1NameUpdateSessionReport) -> Self {
        Self {
            session_id: report.session_id(),
            identity: report.identity.as_ref().map(IdentityEvidence::from),
            gateway_mode: None,
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

impl From<&My1CallsignUpdateSessionReport> for CoreEvidence {
    fn from(report: &My1CallsignUpdateSessionReport) -> Self {
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
                My1CallsignUpdateSessionOutcome::AwaitingCatVerification => {
                    Outcome::AwaitingCatVerification
                }
                My1CallsignUpdateSessionOutcome::Cancelled => Outcome::Cancelled,
                My1CallsignUpdateSessionOutcome::Failed { stage, error } => Outcome::Failed {
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

impl From<&SessionReport> for CoreEvidence {
    fn from(report: &SessionReport) -> Self {
        match report {
            SessionReport::Pm1(report) => Self::from(report),
            SessionReport::My1(report) => Self::from(report),
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
    /// Retain the first synchronization failure through later successful cleanup.
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

    /// An opened handle with failed admission evidence may only be closed.
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
                ("connection close", &session.close_error),
                ("capture synchronization", &session.synchronization_error),
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
                    session.post_exit.outcome
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
            "Programming exit is unconfirmed. Retain the journal and transcripts, and fully power-cycle the radio before reconnecting. A power cycle does not establish which text is stored; do not retry or restore blindly.",
        ),
        ExitDisposition::NotEntered | ExitDisposition::Acknowledged => None,
    }
}

pub(super) async fn run(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
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
        let session = run_session(
            backend, endpoint, baud, update, journal, captures, cancelled,
        )
        .await;
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

async fn run_session(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    update: &mut impl Update,
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
    if cancelled.load(Ordering::Relaxed) && update.status() == UpdateStatus::NotWritten {
        result.post_exit =
            PostExitVerification::skipped(SkipReason::Cancelled, post_exit.summary());
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
    let finish_verification = AtomicBool::new(false);
    let opening_cancelled = if update.status() == UpdateStatus::NotWritten {
        cancelled
    } else {
        &finish_verification
    };
    let Some(connection) = open_original(
        backend,
        endpoint,
        baud,
        &mut original,
        &mut result,
        opening_cancelled,
    ) else {
        return result;
    };
    let Some(mut radio) = result.admit_original(connection, original).await else {
        result.post_exit = PostExitVerification::skipped(
            SkipReason::OriginalCaptureIncomplete,
            post_exit.summary(),
        );
        return result;
    };
    let report = update
        .run_session(&mut radio, cancelled, journal, &mut capture)
        .await;
    result.synchronization_error = capture.error;
    result.close_original(radio.into_transport()).await;
    let eligibility = verification_identity(&result, journal, &report);
    // After any possible write, user cancellation cannot abandon verification.
    // Required capture independently stops protocol traffic after capture loss.
    let verification_cancelled = if update.status() == UpdateStatus::NotWritten {
        cancelled
    } else {
        &finish_verification
    };
    result.post_exit = match eligibility {
        Ok(identity) => match update.kind() {
            UpdateKind::Pm1Name => {
                reconnect::verify_required(
                    backend,
                    endpoint,
                    baud,
                    identity,
                    post_exit,
                    verification_cancelled,
                )
                .await
            }
            UpdateKind::PmOffMy1 => {
                reconnect::verify_required_gateway_off(
                    backend,
                    endpoint,
                    baud,
                    identity,
                    post_exit,
                    verification_cancelled,
                )
                .await
            }
        },
        Err(reason) => PostExitVerification::skipped(reason, post_exit.summary()),
    };
    result.core = Some(CoreEvidence::from(&report));
    result
}

fn verification_identity<'a>(
    result: &SessionEvidence,
    journal: &UpdateJournal,
    report: &'a SessionReport,
) -> Result<&'a kenwood_tmd750::Identity, SkipReason> {
    if result.close_error.is_some() {
        Err(SkipReason::OriginalCloseFailed)
    } else if !result.transcript.complete || result.synchronization_error.is_some() {
        Err(SkipReason::OriginalCaptureIncomplete)
    } else if journal.ensure_complete().is_err() {
        Err(SkipReason::OriginalUpdateJournalIncomplete)
    } else if report.exit() != McpProbeExit::Acknowledged {
        Err(SkipReason::OriginalUpdateIncomplete)
    } else {
        report
            .identity()
            .ok_or(SkipReason::OriginalUpdateIncomplete)
    }
}

/// Synchronize requested open and its outcome before admitting any protocol work.
fn open_original<B: Backend>(
    backend: &mut B,
    endpoint: &SerialCandidate,
    baud: u32,
    original: &mut Recorder<File>,
    result: &mut SessionEvidence,
    cancelled: &AtomicBool,
) -> Option<B::Connection> {
    original.record(Event::OpenRequested {
        path: &endpoint.path,
        baud,
    });
    result.synchronize_original(original);
    if result.synchronization_error.is_some() {
        return None;
    }
    if cancelled.load(Ordering::Relaxed) {
        result.post_exit.outcome = reconnect::VerificationOutcome::Skipped {
            reason: SkipReason::Cancelled,
        };
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

#[cfg(all(test, unix))]
mod my1_tests;
