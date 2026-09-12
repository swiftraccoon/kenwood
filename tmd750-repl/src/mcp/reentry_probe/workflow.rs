//! Two fixed read sessions; a failed boundary never admits another session.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{DvGatewayMode, McpGatewayOffProbeReport, Radio};
use kenwood_transport::Transport;
use serde::Serialize;

use super::super::capture::{CaptureTransport, Event, Recorder, TranscriptSummary};
use super::super::reconnect::{self, Backend, PostExitVerification, SkipReason};
use super::super::{Failure, ProbeEvidence, close_transport, verification_eligibility};
use crate::output;

/// The two exclusive transcripts reserved for one original/fresh-handle pair.
pub(super) struct Captures {
    pub(super) original: Recorder<File>,
    pub(super) post_exit: Recorder<File>,
}

#[derive(Debug, Serialize)]
pub(super) struct Session {
    probe: Option<ProbeEvidence>,
    gateway_mode: Option<u8>,
    transcript: TranscriptSummary,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    synchronization_error: Option<Failure>,
    post_exit: PostExitVerification,
}

impl Session {
    fn pending(captures: &Captures) -> Self {
        Self {
            probe: None,
            gateway_mode: None,
            transcript: captures.original.summary(),
            open_error: None,
            close_error: None,
            synchronization_error: None,
            post_exit: PostExitVerification::skipped(
                SkipReason::OriginalOpenFailed,
                captures.post_exit.summary(),
            ),
        }
    }

    fn synchronize(&mut self, recorder: &mut Recorder<File>) {
        if let Err(error) = recorder.synchronize()
            && self.synchronization_error.is_none()
        {
            self.synchronization_error = Some(Failure::from_error(&error));
        }
        self.transcript = recorder.summary();
    }

    async fn close(&mut self, mut transport: CaptureTransport<impl Transport, File>) {
        self.close_error = close_transport(&mut transport).await;
        let mut recorder = transport.into_recorder();
        self.synchronize(&mut recorder);
    }

    fn succeeded(&self) -> bool {
        self.probe.as_ref().is_some_and(|probe| {
            matches!(
                probe.outcome,
                super::super::Outcome::AwaitingCatVerification
            ) && matches!(probe.exit, super::super::ExitDisposition::Acknowledged)
        }) && self.gateway_mode == Some(0)
            && self.transcript.complete
            && self.open_error.is_none()
            && self.close_error.is_none()
            && self.synchronization_error.is_none()
            && self.post_exit.gateway_off_evidence().is_some()
    }

    fn print(&self, number: usize) {
        if self.succeeded() {
            output::line(format_args!(
                "Session {number}: both fixed reads, MCP exit, original close, fresh matching CAT/Gateway Off, and fresh close captured."
            ));
        }
        if let Some(probe) = &self.probe
            && let super::super::Outcome::Failed { stage, error } = &probe.outcome
        {
            output::error(format_args!(
                "Session {number} failed at {stage:?}: {error}"
            ));
        }
        for (label, error) in [
            ("open", &self.open_error),
            ("close", &self.close_error),
            ("capture synchronization", &self.synchronization_error),
        ] {
            if let Some(error) = error {
                output::error(format_args!("Session {number} {label} failed: {error}"));
            }
        }
        if self.post_exit.gateway_off_evidence().is_none() {
            output::error(format_args!(
                "Session {number} fresh Off verification: {}.",
                self.post_exit.outcome
            ));
        }
    }
}

/// Append-only session evidence; synchronization precedes the next opening.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum JournalEvent<'a> {
    Prepared {
        scope: &'a str,
    },
    SessionFinished {
        number: usize,
        evidence: &'a Session,
    },
}

/// Historical observations, never a current-state or firmware-readiness claim.
#[derive(Debug, Serialize)]
pub(super) struct Workflow {
    sessions: Vec<Session>,
    journal: TranscriptSummary,
    synchronization_error: Option<Failure>,
}

impl Workflow {
    pub(super) fn empty(journal: &Recorder<File>) -> Self {
        Self {
            sessions: Vec::new(),
            journal: journal.summary(),
            synchronization_error: None,
        }
    }

    pub(super) fn succeeded(&self) -> bool {
        self.sessions.len() == 2
            && self.sessions.iter().all(Session::succeeded)
            && self.journal.complete
            && self.synchronization_error.is_none()
    }
}

pub(super) async fn run(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    captures: [Captures; 2],
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> Workflow {
    let mut result = Workflow::empty(journal);
    for captures in captures {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let number = result.sessions.len() + 1;
        output::line(format_args!("Read-only Gateway-Off session {number} of 2."));
        let session = run_session(backend, endpoint, baud, captures, cancelled).await;
        session.print(number);
        journal.record(JournalEvent::SessionFinished {
            number,
            evidence: &session,
        });
        if let Err(error) = journal.synchronize() {
            result.synchronization_error = Some(Failure::from_error(&error));
        }
        result.journal = journal.summary();
        let may_continue = session.succeeded() && result.synchronization_error.is_none();
        result.sessions.push(session);
        if !may_continue {
            break;
        }
    }
    result
}

async fn run_session(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    captures: Captures,
    cancelled: &AtomicBool,
) -> Session {
    let mut result = Session::pending(&captures);
    let Captures {
        mut original,
        post_exit,
    } = captures;
    original.record(Event::OpenRequested {
        path: &endpoint.path,
        baud,
    });
    result.synchronize(&mut original);
    if result.synchronization_error.is_some() || cancelled.load(Ordering::Relaxed) {
        let reason = if result.synchronization_error.is_some() {
            SkipReason::OriginalCaptureIncomplete
        } else {
            SkipReason::Cancelled
        };
        result.post_exit = PostExitVerification::skipped(reason, post_exit.summary());
        return result;
    }
    let connection = match backend.open(endpoint, baud) {
        Ok(connection) => {
            original.record(Event::OpenCompleted);
            connection
        }
        Err(error) => {
            let failure = Failure::from_error(&error);
            original.record(Event::OpenFailed {
                error: failure.clone(),
            });
            result.open_error = Some(failure);
            result.synchronize(&mut original);
            return result;
        }
    };
    result.synchronize(&mut original);
    let mut radio = Radio::new(CaptureTransport::required(connection, original));
    if result.synchronization_error.is_some() || cancelled.load(Ordering::Relaxed) {
        result.close(radio.into_transport()).await;
        let reason = if result.synchronization_error.is_some() {
            SkipReason::OriginalCaptureIncomplete
        } else {
            SkipReason::Cancelled
        };
        result.post_exit = PostExitVerification::skipped(reason, post_exit.summary());
        return result;
    }
    let core = radio
        .probe_mcp_gateway_off_until_exit(|| cancelled.load(Ordering::Relaxed))
        .await;
    result.close(radio.into_transport()).await;
    result.post_exit = verify(
        backend, endpoint, baud, &core, &result, post_exit, cancelled,
    )
    .await;
    result.gateway_mode = core.gateway_mode.map(u8::from);
    result.probe = Some(ProbeEvidence::from(&core.probe));
    result
}

async fn verify(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    core: &McpGatewayOffProbeReport,
    result: &Session,
    post_exit: Recorder<File>,
    cancelled: &AtomicBool,
) -> PostExitVerification {
    let eligibility = if result.synchronization_error.is_some() {
        Err(SkipReason::OriginalCaptureIncomplete)
    } else if core.gateway_mode != Some(DvGatewayMode::Off) {
        Err(SkipReason::OriginalProbeIncomplete)
    } else {
        verification_eligibility(
            &core.probe,
            result.close_error.as_ref(),
            &result.transcript,
            cancelled,
        )
    };
    match eligibility {
        Ok(identity) => {
            reconnect::verify_required_gateway_off(
                backend, endpoint, baud, identity, post_exit, cancelled,
            )
            .await
        }
        Err(reason) => PostExitVerification::skipped(reason, post_exit.summary()),
    }
}

#[cfg(test)]
mod tests;
