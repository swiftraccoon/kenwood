//! Explicit USB Terminal ownership around one diagnostic-only modem lease.
//!
//! Full-page intent precedes writes. Every MCP owner is closed and dropped before
//! bounded identity-only reacquisition. Restoration uses inverse exact images on
//! a fresh control owner, never a blind write or an uncertain previous handle.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::radio::terminal::{TerminalPlan, TerminalTarget};
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate};
use kenwood_transport::TransportError;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::capture::{Artifacts, CaptureKind, Failure, Recorder, create_private_file};
use crate::mcp::reconnect::{Backend, SystemBackend, endpoint_is_unambiguous};
use crate::mcp::snapshot::Snapshot;
use crate::{AppResult, CommandError, output};

use super::report::{Endpoint, IdentityEvidence};
use super::{Limits, Request, finish_on_interrupt};

mod admission;
mod evidence;
mod phase;
#[cfg(test)]
mod tests;

use admission::Endpoints;
use evidence::{
    ConnectionResult, FailureStage, IntendedPage, Problem, Restoration, WorkflowResult,
};

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Entry,
    Restore,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalEvent<'a> {
    Prepared {
        identity: IdentityEvidence,
        control: Endpoint,
        modem: Endpoint,
        active_pm: u8,
        gateway_before: u8,
        pages: Vec<IntendedPage<'a>>,
    },
    BeforeWrite {
        phase: Phase,
        page: IntendedPage<'a>,
    },
    ExchangeFinished {
        phase: Phase,
        evidence: &'a ConnectionResult,
    },
    Checkpoint {
        evidence: &'a WorkflowResult,
    },
}

fn record_journal(journal: &mut Recorder<File>, event: JournalEvent<'_>) -> io::Result<()> {
    journal.record(event);
    journal.synchronize()
}

struct Captures {
    entry: phase::Captures,
    probe: Recorder<File>,
    restore: phase::Captures,
}

impl Captures {
    fn reserve(
        directory: &Path,
        entry: Recorder<File>,
        cancelled: &Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let named = |name| -> io::Result<_> {
            Ok(Recorder::named(
                create_private_file(&directory.join(name))?,
                Arc::clone(cancelled),
                name,
            ))
        };
        Ok(Self {
            entry: phase::Captures {
                exchange: entry,
                readiness: named("entry-readiness.jsonl")?,
                verification: named("entry-verification.jsonl")?,
            },
            probe: named("modem-transcript.jsonl")?,
            restore: phase::Captures {
                exchange: named("restore-transcript.jsonl")?,
                readiness: named("restore-readiness.jsonl")?,
                verification: named("restore-verification.jsonl")?,
            },
        })
    }
}

/// Adapt the existing diagnostic owner without introducing another probe driver.
struct ProbeBackend<'a, B>(&'a mut B);

impl<B: Backend> super::Backend for ProbeBackend<'_, B> {
    type Connection = B::Connection;

    fn open(&mut self, endpoint: &SerialCandidate) -> Result<Self::Connection, TransportError> {
        let candidates = self.0.enumerate()?;
        let selected =
            super::select_endpoint(&endpoint.path, candidates.clone()).map_err(|error| {
                TransportError::Open {
                    path: endpoint.path.clone(),
                    source: io::Error::other(error),
                }
            })?;
        if selected != *endpoint || !endpoint_is_unambiguous(endpoint, &candidates) {
            return Err(TransportError::Open {
                path: endpoint.path.clone(),
                source: io::Error::other("modem endpoint metadata changed or became ambiguous"),
            });
        }
        self.0.open(endpoint, DEFAULT_BAUD)
    }
}

fn checkpoint(journal: &mut Recorder<File>, result: &mut WorkflowResult) -> bool {
    match record_journal(journal, JournalEvent::Checkpoint { evidence: result }) {
        Ok(()) => true,
        Err(error) => {
            result
                .problems
                .push(Problem::new(FailureStage::Journal, &error));
            if result.restoration == Restoration::Verified {
                result.restoration = Restoration::Owed;
            }
            false
        }
    }
}

async fn run_workflow(
    backend: &mut impl Backend,
    endpoints: &Endpoints,
    plan: &TerminalPlan,
    captures: Captures,
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
    limits: Limits,
) -> WorkflowResult {
    let mut result = WorkflowResult::default();
    let prepared = record_journal(
        journal,
        JournalEvent::Prepared {
            identity: plan.identity().into(),
            control: (&endpoints.control).into(),
            modem: (&endpoints.modem).into(),
            active_pm: plan.slot().index(),
            gateway_before: plan.before().into(),
            pages: plan.replacements().iter().map(Into::into).collect(),
        },
    );
    if let Err(error) = prepared {
        result
            .problems
            .push(Problem::new(FailureStage::Journal, &error));
        return result;
    }
    let entry = phase::run(
        backend,
        phase::Request {
            endpoint: &endpoints.control,
            plan,
            phase: Phase::Entry,
            cancelled,
            limits,
        },
        captures.entry,
        journal,
    )
    .await;
    if entry.exchange.intent_recorded {
        result.restoration = Restoration::Owed;
    }
    let control_ready = entry.control_ready();
    let entry_succeeded = entry.succeeded();
    result.entry = Some(entry);
    if !checkpoint(journal, &mut result) || !control_ready {
        return result;
    }
    if entry_succeeded && !cancelled.load(Ordering::Relaxed) {
        result.probe = Some(
            super::run_workflow(
                &mut ProbeBackend(backend),
                &endpoints.modem,
                captures.probe,
                cancelled,
                limits,
            )
            .await,
        );
    }
    if !checkpoint(journal, &mut result) {
        return result;
    }
    if result.restoration == Restoration::Owed {
        match plan.restoration() {
            Err(error) => result
                .problems
                .push(Problem::new(FailureStage::Operation, &error)),
            Ok(restore) => {
                // Cancellation suppresses new diagnostic work, not an already
                // owned restoration. All evidence and fresh comparison gates
                // still apply, with one attempt and no blind retry.
                let finish_required = AtomicBool::new(false);
                let restored = phase::run(
                    backend,
                    phase::Request {
                        endpoint: &endpoints.control,
                        plan: &restore,
                        phase: Phase::Restore,
                        cancelled: &finish_required,
                        limits,
                    },
                    captures.restore,
                    journal,
                )
                .await;
                if restored.succeeded() {
                    result.restoration = Restoration::Verified;
                }
                result.restore = Some(restored);
            }
        }
    }
    let _durable = checkpoint(journal, &mut result);
    result
}

#[derive(Serialize)]
struct Report<'a> {
    format_version: u8,
    operation: &'static str,
    software_version: &'static str,
    identity_assurance: &'static str,
    verification_scope: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    source_backup: &'a Path,
    control: Endpoint,
    modem: Endpoint,
    limits: Limits,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
    cancelled: bool,
}

impl Report<'_> {
    fn publish(&self, file: &mut File, path: &Path) -> AppResult<()> {
        serde_json::to_writer_pretty(&mut *file, self)
            .map_err(io::Error::from)
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|source| super::PublicationError {
                path: path.to_owned(),
                source,
            })?;
        Ok(())
    }
}

/// Admit all offline guards before reserving evidence or opening either endpoint.
pub(super) async fn run(endpoint: &SerialCandidate, request: &Request) -> AppResult<()> {
    let _path = request.validate(Some(&endpoint.path), DEFAULT_BAUD)?;
    if !cfg!(unix) {
        return Err(CommandError(
            "managed diagnostics require private Unix files and synchronized directories"
                .to_owned(),
        )
        .into());
    }
    let control = request
        .control_port
        .as_deref()
        .ok_or("missing explicit control endpoint")?;
    let backup = request
        .backup
        .as_deref()
        .ok_or("missing completed configuration backup")?;
    let snapshot = Snapshot::load_for_usb_write(backup)?;
    let plan = TerminalPlan::new(
        &snapshot.identity,
        &snapshot.menu_snapshot()?,
        TerminalTarget::ReflectorTerminal,
    )?;
    let mut backend = SystemBackend::new();
    let endpoints = Endpoints::admit(endpoint, control, plan.route(), &backend.enumerate()?)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = Artifacts::create(
        CaptureKind::DstarProbe,
        request.output.as_deref(),
        Arc::clone(&cancelled),
    )?;
    let captures = Captures::reserve(&directory, transcript, &cancelled)?;
    let mut journal = Recorder::named(
        create_private_file(&directory.join("terminal-journal.jsonl"))?,
        Arc::clone(&cancelled),
        "terminal-journal.jsonl",
    );
    File::open(&directory)?.sync_all()?;
    File::open(
        directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()?;
    output::line(format_args!(
        "Managed Terminal diagnostic evidence: {}. Ctrl-C suppresses diagnostics but preserves owed restoration.",
        directory.display()
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = Box::pin(finish_on_interrupt(
        run_workflow(
            &mut backend,
            &endpoints,
            &plan,
            captures,
            &mut journal,
            &cancelled,
            Limits::DEFAULT,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    ))
    .await;
    let report = Report {
        format_version: 1,
        operation: "managed_terminal_diagnostic",
        software_version: env!("CARGO_PKG_VERSION"),
        identity_assurance: "explicit_usb_roles_and_control_cat_tuple_only_not_physical_unit_continuity",
        verification_scope: "guarded_complete_page_readback_and_fresh_control_identity_gateway_not_power_cycle_persistence",
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        source_backup: backup,
        control: (&endpoints.control).into(),
        modem: (&endpoints.modem).into(),
        limits: Limits::DEFAULT,
        workflow,
        signal_error,
        cancelled: cancelled.load(Ordering::Relaxed),
    };
    let path = directory.join("report.json");
    report.publish(&mut report_file, &path)?;
    output::line(format_args!(
        "Managed diagnostic report: {}. Restoration: {:?}.",
        path.display(),
        report.workflow.restoration
    ));
    if report.workflow.succeeded() && !report.cancelled && report.signal_error.is_none() {
        Ok(())
    } else {
        Err(CommandError(format!("managed diagnostic incomplete; retain {} and its recovery journal; do not retry or restore blindly", directory.display())).into())
    }
}
