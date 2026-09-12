//! Typed text updates with full-page evidence and a separate verifier.

mod journal;
mod target;
mod workflow;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use clap::Args;
use kenwood_tmd750::Region;
use kenwood_tmd750::memory::{
    My1Callsign, My1CallsignUpdate, Pm1Name, Pm1NameUpdate, Pm1NameUpdateStatus, TextSetting,
};
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
use target::{PreparedUpdate, Update, UpdateKind};
use workflow::{SessionCaptures, WorkflowResult};

/// Leave a PM1 name or PM-Off MY1 callsign in place; no arbitrary fields.
#[derive(Debug, Args)]
pub(super) struct SetRequest {
    /// Successful, current standard configuration-backup report for this radio.
    #[arg(long, value_name = "REPORT")]
    backup: PathBuf,
    /// Exact current text; MY1 alone accepts "" for eight captured NUL bytes.
    #[arg(long = "expect", value_parser = parse_expected, value_name = "CURRENT_TEXT")]
    expected: String,
    /// Approve leaving the requested text in place, without automatic rollback.
    #[arg(long, required = true)]
    apply: bool,
    /// New private evidence directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
    /// Only pm-name-1 or dstar-my-callsign-1; MY1 is fixed to PM Off/Gateway Off.
    setting: TextSetting,
    /// PM1: 1–16 printable ASCII bytes. MY1: 1–8 uppercase letters/digits/spaces.
    #[arg(value_parser = parse_value, value_name = "NEW_TEXT")]
    value: String,
}

fn parse_value(value: &str) -> Result<String, String> {
    Pm1Name::new(value)
        .map(|value| value.as_str().to_owned())
        .map_err(|error| error.to_string())
}

fn parse_expected(value: &str) -> Result<String, String> {
    if value.is_empty() {
        Ok(String::new())
    } else {
        parse_value(value)
    }
}

/// All untyped CLI text is converted before enumeration or capture creation.
enum RequestedChange {
    Pm1 {
        expected: Pm1Name,
        desired: Pm1Name,
    },
    My1 {
        expected: Option<My1Callsign>,
        desired: My1Callsign,
    },
}

impl SetRequest {
    pub(super) fn validate_options(&self) -> AppResult<()> {
        self.requested_change().map(|_change| ())
    }

    fn requested_change(&self) -> AppResult<RequestedChange> {
        if !self.apply {
            return Err(Box::new(CommandError(
                "mcp text set requires explicit --apply".to_owned(),
            )));
        }
        match self.setting {
            TextSetting::PmName1 => {
                let expected = Pm1Name::new(&self.expected)?;
                let desired = Pm1Name::new(&self.value)?;
                if expected == desired {
                    return Err(Box::new(CommandError(
                        "no name change requested; the radio was not opened or checked".to_owned(),
                    )));
                }
                Ok(RequestedChange::Pm1 { expected, desired })
            }
            TextSetting::DstarMyCallsign1 => {
                let expected = if self.expected.is_empty() {
                    None
                } else {
                    Some(My1Callsign::new(&self.expected)?)
                };
                let desired = My1Callsign::new(&self.value)?;
                if expected.as_ref() == Some(&desired) {
                    return Err(Box::new(CommandError(
                        "no MY1 change requested; the radio was not opened or checked".to_owned(),
                    )));
                }
                Ok(RequestedChange::My1 { expected, desired })
            }
            _ => Err(Box::new(CommandError(format!(
                "{} has no dedicated text setter; this command supports only pm-name-1 and the experimental PM-Off dstar-my-callsign-1 update. Use mcp menu for general field discovery, preview, and ordinary-update policy.",
                self.setting,
            )))),
        }
    }

    fn prepare(&self, endpoint: &SerialCandidate, baud: u32) -> AppResult<PreparedUpdate> {
        let change = self.requested_change()?;
        if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
            return Err(Box::new(CommandError(
                "text updates require the pinned main-unit USB endpoint at 9600 baud".to_owned(),
            )));
        }
        let snapshot = Snapshot::load(&self.backup)?;
        match change {
            RequestedChange::Pm1 { expected, desired } => {
                let page = Pm1NameUpdate::required_page()?;
                Ok(PreparedUpdate::Pm1(Box::new(Pm1NameUpdate::prepare(
                    &snapshot.identity,
                    snapshot.captured_bytes(page.region())?,
                    &expected,
                    &desired,
                )?)))
            }
            RequestedChange::My1 { expected, desired } => {
                if snapshot.captured_bytes(Region::new(10, 11)?)? != [0] {
                    return Err("MY1 update requires captured memory-format byte zero".into());
                }
                let target = My1CallsignUpdate::required_page()?;
                let control = My1CallsignUpdate::required_control_page()?;
                Ok(PreparedUpdate::My1(Box::new(My1CallsignUpdate::prepare(
                    &snapshot.identity,
                    snapshot.captured_bytes(target.region())?,
                    snapshot.captured_bytes(control.region())?,
                    expected.as_ref(),
                    &desired,
                )?)))
            }
        }
    }

    fn print_start(&self, directory: &Path, kind: UpdateKind) {
        output::line(format_args!(
            "{} update evidence: {}.",
            kind.label(),
            directory.display()
        ));
        output::line(format_args!(
            "Requested {} text: {:?} -> {:?}. The requested text will remain in place; no RF commands or automatic rollback.",
            kind.label(),
            self.expected.as_str(),
            self.value.as_str()
        ));
        output::line(format_args!(
            "Keep this radio connected. Ctrl-C cancels before write intent; afterward, safe verification finishes before stopping."
        ));
        if kind == UpdateKind::PmOffMy1 {
            output::line(format_args!(
                "Configurable MY1 updates are mock-tested, not hardware-qualified. This does not enable Terminal Mode or establish callsign acceptance."
            ));
        }
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
    "could not finalize text update report {path}: {source}; update status {status:?}. Retain the adjacent recovery journal and transcripts; do not retry or restore blindly"
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

/// Prepare a closed typed target before reserving any captures or opening USB.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &SetRequest,
) -> AppResult<()> {
    match request.prepare(endpoint, baud)? {
        PreparedUpdate::Pm1(mut update) => {
            run_prepared(update.as_mut(), endpoint, baud, request).await
        }
        PreparedUpdate::My1(mut update) => {
            run_prepared(update.as_mut(), endpoint, baud, request).await
        }
    }
}

/// Share lifecycle and persistence without sharing the target's admission policy.
async fn run_prepared(
    update: &mut impl Update,
    endpoint: &SerialCandidate,
    baud: u32,
    request: &SetRequest,
) -> AppResult<()> {
    let kind = update.kind();
    let cancelled = AtomicBool::new(false);
    let capture_failed = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(request.output.as_deref(), Arc::clone(&capture_failed))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
    let verification = verification_captures(&artifacts.directory, &capture_failed)?;
    let mut journal = UpdateJournal::create(&artifacts.directory, capture_failed)?;
    journal.prepare(update, &request.backup)?;
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
    request.print_start(&directory, kind);
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = finish_on_interrupt(
        workflow::run(
            &mut SystemBackend::new(),
            endpoint,
            baud,
            update,
            &mut journal,
            captures,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let journal_error = journal
        .finish(update)
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    workflow.print_failures(kind);
    if let Some(error) = &journal_error {
        output::error(format_args!(
            "{} update journal could not be finalized: {error}",
            kind.label()
        ));
    }
    let succeeded = workflow.succeeded(update) && journal_error.is_none() && signal_error.is_none();
    let report = Report {
        format_version: kind.format_version(),
        operation: kind.operation(),
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
        scope: kind.scope(),
        status: update.status(),
        workflow,
        signal_error,
        journal_error,
    };
    report.save(&mut report_file, &directory)?;
    output::line(format_args!(
        "{} update report: {}.",
        kind.label(),
        directory.join("report.json").display()
    ));
    if succeeded {
        output::line(format_args!(
            "{} is now {:?}; the entire desired page was verified across MCP exit/re-entry. Refresh the configuration backup before another edit.",
            kind.label(),
            request.value.as_str()
        ));
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "{} update incomplete; status {:?}. Do not retry or restore blindly. Inspect retained evidence in {}.",
            kind.label(),
            report.status,
            directory.display()
        ))))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod my1_tests;
