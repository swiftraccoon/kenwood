//! Separately approved fixed PM1 experiment, with durable recovery evidence.

mod journal;
mod workflow;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Parser;
use kenwood_tmd750::memory::{PmNameTrial, PmNameTrialStatus};
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

/// One explicitly approved experiment; no arbitrary field, value, or address.
#[derive(Debug, Parser)]
pub(crate) struct TrialRequest {
    /// Successful standard configuration report belonging to this radio.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// PM1 name independently observed on the radio, without recalling a PM.
    #[arg(long, value_name = "DISPLAYED_NAME")]
    confirmed_name: String,
    /// Approve PM1 -> PC TEXT TEST -> original name across MCP exit/re-entry.
    #[arg(long, required = true)]
    approve_live_test: bool,
    /// New private capture directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
}

impl TrialRequest {
    fn prepare(&self, endpoint: &SerialCandidate, baud: u32) -> AppResult<PmNameTrial> {
        if !self.approve_live_test {
            return Err(Box::new(CommandError(
                "explicit PM1 trial approval is required".to_owned(),
            )));
        }
        if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
            return Err(Box::new(CommandError(
                "PM1 trial requires the pinned TM-D750 main-unit USB endpoint at 9600 baud"
                    .to_owned(),
            )));
        }
        let snapshot = Snapshot::load(&self.backup)?;
        let page = PmNameTrial::required_page()?;
        Ok(PmNameTrial::prepare_unqualified_offline(
            &snapshot.identity,
            snapshot.captured_bytes(page.region())?,
            &self.confirmed_name,
        )?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RestorationStatus {
    NotWritten,
    PossiblyChanged,
    RestorationVerified,
}

impl From<PmNameTrialStatus> for RestorationStatus {
    fn from(status: PmNameTrialStatus) -> Self {
        match status {
            PmNameTrialStatus::NotWritten => Self::NotWritten,
            PmNameTrialStatus::PossiblyChanged => Self::PossiblyChanged,
            PmNameTrialStatus::RestorationVerified => Self::RestorationVerified,
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
    independently_confirmed_name: String,
    scope: &'static str,
    restoration: RestorationStatus,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
    journal_error: Option<Failure>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "could not finalize PM1 report {path}: {source}; restoration status {restoration:?}. Retain the adjacent recovery journal and transcripts; do not retry or restore blindly"
)]
struct ReportError {
    path: PathBuf,
    restoration: RestorationStatus,
    #[source]
    source: std::io::Error,
}

impl Report {
    fn save(&self, writer: &mut File, directory: &Path) -> Result<(), ReportError> {
        super::write_report(writer, self)
            .and_then(|()| writer.sync_all())
            .map_err(|source| ReportError {
                path: directory.join("report.json"),
                restoration: self.restoration,
                source,
            })
    }
}

fn additional_captures(
    directory: &Path,
    failed: &Arc<AtomicBool>,
) -> AppResult<Vec<SessionCaptures>> {
    [
        (
            "session-2-transcript.jsonl",
            "session-2-post-exit-transcript.jsonl",
        ),
        (
            "session-3-transcript.jsonl",
            "session-3-post-exit-transcript.jsonl",
        ),
    ]
    .into_iter()
    .map(|(original, post_exit)| {
        let reserve = |filename| -> AppResult<Recorder<File>> {
            Ok(Recorder::named(
                create_private_file(&directory.join(filename))?,
                Arc::clone(failed),
                filename,
            ))
        };
        Ok(SessionCaptures {
            original: reserve(original)?,
            post_exit: reserve(post_exit)?,
        })
    })
    .collect()
}

/// Reserve all evidence, run exactly the approved trial, and retain failures.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &TrialRequest,
) -> AppResult<()> {
    let mut trial = request.prepare(endpoint, baud)?;
    let cancelled = AtomicBool::new(false);
    let capture_failed = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(request.output.as_deref(), Arc::clone(&capture_failed))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
    let extra = additional_captures(&artifacts.directory, &capture_failed)?;
    let mut journal = Journal::create(&artifacts.directory)?;
    journal.prepare(&trial, &request.backup, &request.confirmed_name)?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    let mut captures = vec![SessionCaptures {
        original: transcript,
        post_exit,
    }];
    captures.extend(extra);
    output::line(format_args!("PM1 trial evidence: {}.", directory.display()));
    output::line(format_args!(
        "Approved fixed test: {} -> {} -> {}. Three MCP sessions with fresh-connection checks; no RF commands.",
        request.confirmed_name,
        PmNameTrial::TEMPORARY_NAME,
        request.confirmed_name,
    ));
    output::line(format_args!(
        "Keep this radio connected. After a write intent, Ctrl-C cannot abandon restoration; any uncertain exchange stops further commands."
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
    workflow.print_failures();
    if let Some(error) = &journal_error {
        output::error(format_args!(
            "PM1 recovery journal could not be finalized: {error}"
        ));
    }
    let succeeded = workflow.succeeded(&trial) && signal_error.is_none() && journal_error.is_none();
    let report = Report {
        format_version: 4,
        operation: "pm1_name_trial",
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
        independently_confirmed_name: request.confirmed_name.clone(),
        scope: "fixed PM1 page only; not firmware-wide schema qualification; endpoint and CAT tuple do not prove physical continuity",
        restoration: trial.status().into(),
        workflow,
        signal_error,
        journal_error,
    };
    report.save(&mut report_file, &directory)?;
    output::line(format_args!(
        "PM1 trial report: {}.",
        directory.join("report.json").display()
    ));
    if succeeded {
        output::line(format_args!(
            "PM1 rename and exact whole-page restoration verified across MCP exit/re-entry. Full-radio reboot or power-cycle persistence is not proved. General settings writes remain disabled for firmware 1.02."
        ));
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "PM1 trial incomplete; restoration status {:?}. Do not retry or restore blindly. Inspect retained evidence in {} before further radio commands.",
            report.restoration,
            directory.display(),
        ))))
    }
}

#[cfg(test)]
mod tests;
