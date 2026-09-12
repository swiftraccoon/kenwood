//! Separately approved, fixed Terminal-to-Off experiment with durable evidence.

mod journal;
mod workflow;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use kenwood_tmd750::Region;
use kenwood_tmd750::memory::{TerminalExitTrial, TerminalExitTrialStatus};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, TMD750_MAIN_PID};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::capture::{Artifacts, Recorder, create_private_file};
use super::reconnect::SystemBackend;
use super::snapshot::Snapshot;
use super::{Endpoint, Failure, finish_on_interrupt};
use crate::{AppResult, CommandError, output};
use journal::Journal;
use workflow::{SessionCaptures, WorkflowResult};

/// One fixed experiment, not a general Gateway setter or a D-STAR session.
#[derive(Debug, Parser)]
pub(crate) struct Request {
    /// Complete successful configuration report captured with Gateway Off.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Approve one guarded Terminal-to-Off write and independent verification.
    #[arg(long, required = true)]
    approve_live_test: bool,
    /// New private capture directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
}

impl Request {
    fn prepare(&self, endpoint: &SerialCandidate, baud: u32) -> AppResult<TerminalExitTrial> {
        if !self.approve_live_test {
            return Err(CommandError(
                "explicit Terminal exit trial approval is required".to_owned(),
            )
            .into());
        }
        if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
            return Err(CommandError(
                "Terminal exit trial requires the pinned TM-D750 main-unit USB endpoint at 9600 baud".to_owned(),
            ).into());
        }
        let snapshot = Snapshot::load(&self.backup)?;
        if snapshot.captured_bytes(Region::new(10, 11)?)? != [0] {
            return Err(CommandError(
                "Terminal exit trial requires captured memory format zero".to_owned(),
            )
            .into());
        }
        Ok(TerminalExitTrial::prepare_unqualified_offline(
            &snapshot.identity,
            snapshot.captured_bytes(TerminalExitTrial::required_page()?.region())?,
            snapshot.captured_bytes(TerminalExitTrial::required_control_page()?.region())?,
            snapshot.captured_bytes(TerminalExitTrial::required_routing_page()?.region())?,
        )?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    NotWritten,
    PossiblyChanged,
    OffVerifiedAcrossSessions,
}

impl From<TerminalExitTrialStatus> for Status {
    fn from(status: TerminalExitTrialStatus) -> Self {
        match status {
            TerminalExitTrialStatus::NotWritten => Self::NotWritten,
            TerminalExitTrialStatus::PossiblyChanged => Self::PossiblyChanged,
            TerminalExitTrialStatus::OffVerifiedAcrossSessions => Self::OffVerifiedAcrossSessions,
        }
    }
}

#[derive(Debug, Serialize)]
struct Report {
    format_version: u8,
    operation: &'static str,
    software_version: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    endpoint: Endpoint,
    source_backup: PathBuf,
    scope: &'static str,
    gateway_exit: Status,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
    journal_error: Option<Failure>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "could not finalize Terminal exit report {path}: {source}; status {status:?}. Retain the journal and transcripts; do not retry or restore blindly"
)]
struct ReportError {
    path: PathBuf,
    status: Status,
    #[source]
    source: std::io::Error,
}

impl Report {
    fn save(&self, writer: &mut File, directory: &Path) -> Result<(), ReportError> {
        super::write_report(writer, self)
            .and_then(|()| writer.sync_all())
            .map_err(|source| ReportError {
                path: directory.join("report.json"),
                status: self.gateway_exit,
                source,
            })
    }
}

fn verification_captures(directory: &Path, failed: &Arc<AtomicBool>) -> AppResult<SessionCaptures> {
    let reserve = |filename| -> AppResult<Recorder<File>> {
        Ok(Recorder::named(
            create_private_file(&directory.join(filename))?,
            Arc::clone(failed),
            filename,
        ))
    };
    Ok(SessionCaptures {
        original: reserve("session-2-transcript.jsonl")?,
        post_exit: reserve("session-2-post-exit-transcript.jsonl")?,
    })
}

/// Reserve evidence before opening, then execute only the approved fixed scope.
pub(super) async fn run(endpoint: &SerialCandidate, baud: u32, request: &Request) -> AppResult<()> {
    let mut trial = request.prepare(endpoint, baud)?;
    let cancelled = AtomicBool::new(false);
    let capture_failed = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(request.output.as_deref(), Arc::clone(&capture_failed))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
    let verification = verification_captures(&artifacts.directory, &capture_failed)?;
    let mut journal = Journal::create(&artifacts.directory)?;
    journal.prepare(&trial, &request.backup)?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    let captures = [
        SessionCaptures {
            original: transcript,
            post_exit,
        },
        verification,
    ];
    output::line(format_args!(
        "Terminal exit trial evidence: {}.",
        directory.display()
    ));
    output::line(format_args!(
        "Approved experimental Terminal-to-Off trial: one fixed page write after complete guards, then independent readback and fresh CAT Off. No callsign, PM, routing, reflector, or RF commands."
    ));
    output::line(format_args!(
        "Keep this radio connected. After intent, Ctrl-C cannot abandon verification; uncertain exchanges stop further commands."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = finish_on_interrupt(
        workflow::run(
            &mut SystemBackend::new(),
            endpoint,
            baud,
            &mut trial,
            &mut journal,
            captures,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let journal_error = journal
        .finish(&trial)
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    workflow.print_observations(&trial);
    workflow.print_failures();
    if let Some(error) = &journal_error {
        output::error(format_args!(
            "Terminal exit journal finalization failed: {error}"
        ));
    }
    let succeeded = workflow.succeeded(&trial) && signal_error.is_none() && journal_error.is_none();
    let report = Report {
        format_version: 1,
        operation: "terminal_to_off_trial",
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: Endpoint {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
            cat_baud: baud,
        },
        source_backup: request.backup.clone(),
        scope: "fixed PM Off Reflector Terminal-to-Off only; exact target/control/routing pages, panel Gateway route and COM+AF USB; no retry, rollback, RF commands, or generic firmware qualification; endpoint and CAT tuple do not prove physical continuity",
        gateway_exit: trial.status().into(),
        workflow,
        signal_error,
        journal_error,
    };
    report.save(&mut report_file, &directory)?;
    output::line(format_args!(
        "Terminal exit report: {}.",
        directory.join("report.json").display()
    ));
    if succeeded {
        output::line(format_args!(
            "Gateway Off and all three guarded pages verified across two MCP sessions and fresh CAT connections. This does not prove a full power cycle or enable general Terminal control."
        ));
        Ok(())
    } else {
        Err(CommandError(format!(
            "Terminal exit trial incomplete; status {:?}. Do not retry or restore blindly. Inspect retained evidence in {} before further radio commands.",
            report.gateway_exit, directory.display(),
        )).into())
    }
}

#[cfg(test)]
mod tests;
