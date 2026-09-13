//! Approved, fixed read-only re-entry control with continuous passive evidence.

mod observer;
mod workflow;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, TMD750_MAIN_PID};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::reconnect::SystemBackend;
use super::{Endpoint, Failure, finish_on_interrupt, write_report};
use crate::capture::{Artifacts, CaptureKind, Recorder, create_private_file};
use crate::{AppResult, CommandError, output};
use workflow::{Captures, Workflow};

const SCOPE: &str = "exactly two fixed read-only MCP sessions; TM-D750 / 1.02 / K,2,1; main USB at 9600; fresh Gateway Off before entry and after each exit; reads 8..48 and 327681..327936 only; no settings writes, fill, RF, retry, restoration, or inferred physical continuity";
// Observation after failure adds no protocol traffic and cannot authorize retry.
const FAILURE_OBSERVATION: Duration = Duration::from_secs(20);

/// Explicit approval for two programming interruptions, never a settings edit.
#[derive(Debug, Parser)]
pub(crate) struct Request {
    /// Approve one fixed pair of read-only programming sessions.
    #[arg(long, required = true)]
    approve_live_test: bool,
    /// New private capture directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
}

impl Request {
    fn validate(&self, endpoint: &SerialCandidate, baud: u32) -> AppResult<()> {
        if !self.approve_live_test {
            return Err(
                CommandError("explicit re-entry probe approval is required".to_owned()).into(),
            );
        }
        if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
            return Err(CommandError(
                "re-entry probe requires the pinned TM-D750 main-unit USB endpoint at 9600 baud"
                    .to_owned(),
            )
            .into());
        }
        if !cfg!(target_os = "macos") {
            return Err(CommandError(
                "this re-entry experiment requires the macOS passive serial-registry observer"
                    .to_owned(),
            )
            .into());
        }
        Ok(())
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
    scope: &'static str,
    workflow: Workflow,
    observer: observer::Summary,
    signal_error: Option<Failure>,
    cancelled: bool,
}

/// Files for all possible sessions exist before the first radio open.
struct Reserved {
    sessions: [Captures; 2],
    journal: Recorder<File>,
    observer: Recorder<File>,
}

impl Reserved {
    fn create(
        directory: &Path,
        original: Recorder<File>,
        cancelled: &Arc<AtomicBool>,
    ) -> AppResult<Self> {
        let reserve = |name| -> AppResult<Recorder<File>> {
            Ok(Recorder::named(
                create_private_file(&directory.join(name))?,
                Arc::clone(cancelled),
                name,
            ))
        };
        let sessions = [
            Captures {
                original,
                post_exit: reserve("post-exit-transcript.jsonl")?,
            },
            Captures {
                original: reserve("session-2-transcript.jsonl")?,
                post_exit: reserve("session-2-post-exit-transcript.jsonl")?,
            },
        ];
        let mut journal = reserve("session-journal.jsonl")?;
        journal.record(workflow::JournalEvent::Prepared { scope: SCOPE });
        journal.synchronize()?;
        let observer = reserve("serial-registry.jsonl")?;
        #[cfg(unix)]
        {
            File::open(directory)?.sync_all()?;
            let parent = directory
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            File::open(parent)?.sync_all()?;
        }
        Ok(Self {
            sessions,
            journal,
            observer,
        })
    }
}

/// Reserve evidence, observe passively, and run only the approved fixed pair.
pub(super) async fn run(endpoint: &SerialCandidate, baud: u32, request: &Request) -> AppResult<()> {
    request.validate(endpoint, baud)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(
        CaptureKind::Mcp,
        request.output.as_deref(),
        Arc::clone(&cancelled),
    )?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    let reserved = Reserved::create(&directory, transcript, &cancelled)?;
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    output::line(format_args!(
        "Read-only re-entry evidence: {}.",
        directory.display()
    ));
    output::line(format_args!(
        "Two Gateway-Off MCP read sessions; no settings or RF commands. Initial passive observation is required before opening. Ctrl-C finishes synchronized exchanges; uncertain exchanges permit only close/drop."
    ));
    let ((workflow, observer), signal_error) = finish_on_interrupt(
        run_observed(endpoint, baud, reserved, &cancelled),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let report = Report {
        format_version: 1,
        operation: "gateway_off_reentry_probe",
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: Endpoint {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
            cat_baud: baud,
        },
        scope: SCOPE,
        workflow,
        observer,
        signal_error,
        cancelled: cancelled.load(Ordering::Relaxed),
    };
    write_report(&mut report_file, &report)
        .and_then(|()| report_file.sync_all())
        .map_err(|source| super::ProbeError::Report {
            path: directory.join("report.json"),
            source,
        })?;
    output::line(format_args!(
        "Read-only re-entry report: {}.",
        directory.join("report.json").display()
    ));
    if report.workflow.succeeded()
        && report.observer.succeeded()
        && report.signal_error.is_none()
        && !report.cancelled
    {
        output::line(format_args!(
            "Both fixed read sessions and fresh CAT Off checks completed. This is one measured sequence, not a readiness deadline or qualification of automatic Terminal switching."
        ));
        Ok(())
    } else {
        Err(CommandError(format!(
            "re-entry control incomplete; no automatic retry or recovery commands. Inspect {} before further radio commands",
            directory.display(),
        )).into())
    }
}

async fn run_observed(
    endpoint: &SerialCandidate,
    baud: u32,
    reserved: Reserved,
    cancelled: &Arc<AtomicBool>,
) -> (Workflow, observer::Summary) {
    let Reserved {
        sessions,
        mut journal,
        observer,
    } = reserved;
    let observer = match observer::Observer::start(observer, Arc::clone(cancelled)).await {
        Ok(observer) => observer,
        Err(summary) => return (Workflow::empty(&journal), summary),
    };
    let result = workflow::run(
        &mut SystemBackend::new(),
        endpoint,
        baud,
        sessions,
        &mut journal,
        cancelled,
    )
    .await;
    if !result.succeeded() && !cancelled.load(Ordering::Relaxed) {
        output::line(format_args!(
            "Active commands stopped. Recording passive serial registration for 20 more seconds; this does not establish recovery or authorize another attempt."
        ));
        tokio::time::sleep(FAILURE_OBSERVATION).await;
    }
    (result, observer.stop().await)
}

#[cfg(test)]
mod tests;
