//! Explicit PM1 text updates with full-page evidence and a separate verifier.

mod journal;
mod workflow;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Args;
use kenwood_tmd750::memory::{Pm1Name, Pm1NameUpdate, Pm1NameUpdateStatus, TextSetting};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, TMD750_MAIN_PID};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::capture::{Artifacts, Recorder, create_private_file};
use super::reconnect::SystemBackend;
use super::snapshot::Snapshot;
use super::{Endpoint, Failure, finish_on_interrupt};
use crate::{AppResult, CommandError, output};
use journal::UpdateJournal;
use workflow::{SessionCaptures, WorkflowResult};

/// Leave one requested PM1 name in place; no arbitrary page or field writes.
#[derive(Debug, Args)]
pub(super) struct SetRequest {
    /// Successful, current standard configuration-backup report for this radio.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Expected current PM1 name; must match the backup and fresh whole-page read.
    #[arg(long = "expect", value_parser = parse_name, value_name = "CURRENT_NAME")]
    expected: Pm1Name,
    /// Approve the requested persistent name change, without automatic rollback.
    #[arg(long, required = true)]
    apply: bool,
    /// New private evidence directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
    /// Only pm-name-1 is qualified for this live operation.
    setting: TextSetting,
    /// Exact new name: 1–16 printable ASCII bytes, with no trimming or folding.
    #[arg(value_parser = parse_name, value_name = "NEW_NAME")]
    value: Pm1Name,
}

fn parse_name(value: &str) -> Result<Pm1Name, String> {
    Pm1Name::new(value).map_err(|error| error.to_string())
}

impl SetRequest {
    pub(super) fn validate_options(&self) -> AppResult<()> {
        if !self.apply {
            return Err(Box::new(CommandError(
                "mcp text set requires explicit --apply".to_owned(),
            )));
        }
        if self.setting != TextSetting::PmName1 {
            return Err(Box::new(CommandError(format!(
                "{} has no qualified live setter; only pm-name-1 is supported. Other fields remain available for offline show/preview.",
                self.setting,
            ))));
        }
        if self.expected == self.value {
            return Err(Box::new(CommandError(
                "no name change requested; the radio was not opened or checked".to_owned(),
            )));
        }
        Ok(())
    }

    fn prepare(&self, endpoint: &SerialCandidate, baud: u32) -> AppResult<Pm1NameUpdate> {
        self.validate_options()?;
        if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
            return Err(Box::new(CommandError(
                "PM1 name updates require the pinned main-unit USB endpoint at 9600 baud"
                    .to_owned(),
            )));
        }
        let snapshot = Snapshot::load(&self.backup)?;
        let page = Pm1NameUpdate::required_page()?;
        Ok(Pm1NameUpdate::prepare(
            &snapshot.identity,
            snapshot.captured_bytes(page.region())?,
            &self.expected,
            &self.value,
        )?)
    }

    fn print_start(&self, directory: &Path) {
        output::line(format_args!(
            "PM1 update evidence: {}.",
            directory.display()
        ));
        output::line(format_args!(
            "Requested PM1 name: {:?} -> {:?}. The requested name will remain in place; no RF commands or automatic rollback.",
            self.expected.as_str(),
            self.value.as_str()
        ));
        output::line(format_args!(
            "Keep this radio connected. Ctrl-C cancels before write intent; afterward, safe verification finishes before stopping."
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UpdateStatus {
    NotWritten,
    PossiblyChanged,
    VerifiedAcrossSessions,
}

impl From<Pm1NameUpdateStatus> for UpdateStatus {
    fn from(status: Pm1NameUpdateStatus) -> Self {
        match status {
            Pm1NameUpdateStatus::NotWritten => Self::NotWritten,
            Pm1NameUpdateStatus::PossiblyChanged => Self::PossiblyChanged,
            Pm1NameUpdateStatus::VerifiedAcrossSessions => Self::VerifiedAcrossSessions,
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
    setting: &'static str,
    expected_name: String,
    requested_name: String,
    scope: &'static str,
    status: UpdateStatus,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
    journal_error: Option<Failure>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "could not finalize PM1 update report {path}: {source}; update status {status:?}. Retain the adjacent recovery journal and transcripts; do not retry or restore blindly"
)]
struct ReportError {
    path: PathBuf,
    status: UpdateStatus,
    #[source]
    source: std::io::Error,
}

impl Report {
    fn save(&self, writer: &mut File, directory: &Path) -> Result<(), ReportError> {
        super::write_report(writer, self)
            .and_then(|()| writer.sync_all())
            .map_err(|source| ReportError {
                path: directory.join("report.json"),
                status: self.status,
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
        original: reserve("verify-transcript.jsonl")?,
        post_exit: reserve("verify-post-exit-transcript.jsonl")?,
    })
}

/// Apply one requested name and verify the exact desired page after re-entry.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &SetRequest,
) -> AppResult<()> {
    let mut update = request.prepare(endpoint, baud)?;
    let cancelled = AtomicBool::new(false);
    let capture_failed = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(request.output.as_deref(), Arc::clone(&capture_failed))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
    let verification = verification_captures(&artifacts.directory, &capture_failed)?;
    let mut journal = UpdateJournal::create(&artifacts.directory, capture_failed)?;
    journal.prepare(&update, &request.backup)?;
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
    request.print_start(&directory);
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = finish_on_interrupt(
        workflow::run(
            &mut SystemBackend::new(),
            endpoint,
            baud,
            &mut update,
            &mut journal,
            captures,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let journal_error = journal
        .finish(&update)
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    workflow.print_failures();
    if let Some(error) = &journal_error {
        output::error(format_args!(
            "PM1 update journal could not be finalized: {error}"
        ));
    }
    let succeeded =
        workflow.succeeded(&update) && journal_error.is_none() && signal_error.is_none();
    let report = Report {
        format_version: 5,
        operation: "pm1_name_update",
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
        setting: request.setting.key(),
        expected_name: request.expected.as_str().to_owned(),
        requested_name: request.value.as_str().to_owned(),
        scope: "global PM1 only; verification targets MCP exit/re-entry, not a power cycle; endpoint and CAT tuple do not prove physical continuity",
        status: update.status().into(),
        workflow,
        signal_error,
        journal_error,
    };
    report.save(&mut report_file, &directory)?;
    output::line(format_args!(
        "PM1 update report: {}.",
        directory.join("report.json").display()
    ));
    if succeeded {
        output::line(format_args!(
            "PM1 is now {:?}; the entire desired page was verified across MCP exit/re-entry. Refresh the configuration backup before another edit.",
            request.value.as_str()
        ));
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "PM1 update incomplete; status {:?}. Do not retry or restore blindly. Inspect retained evidence in {}.",
            report.status,
            directory.display()
        ))))
    }
}

#[cfg(test)]
mod tests;
