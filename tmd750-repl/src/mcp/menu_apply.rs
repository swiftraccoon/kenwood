//! Ordinary menu updates using the library's immutable complete-page plan.
//!
//! One MCP handle performs comparison, writes, immediate readback, and detached
//! exit. A second handle verifies CAT identity and Gateway Off. This is not
//! persistence verification across another MCP session or a power cycle.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, TMD750_MAIN_PID, Transport};
use kenwood_tmd750::{DvGatewayMode, MenuUpdatePlan, Page, PageReplacement, Radio};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::capture::{
    Artifacts, CaptureTransport, Event, Recorder, TranscriptSummary, create_private_file,
};
use super::menu::{ApplyRequest, Value};
use super::reconnect::{self, Backend, PostExitVerification, SkipReason, SystemBackend};
use super::{
    Endpoint, ExitDisposition, Failure, IdentityEvidence, close_transport, finish_on_interrupt,
    write_report,
};
use crate::{AppResult, CommandError, output};

struct Captures {
    original: Recorder<File>,
    post_exit: Recorder<File>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct PageEvidence {
    address: u32,
    length: usize,
}

impl From<Page> for PageEvidence {
    fn from(page: Page) -> Self {
        Self {
            address: page.address().as_u32(),
            length: page.len(),
        }
    }
}

/// Possible pages include verified writes; a failure never implies rollback.
#[derive(Debug, Serialize)]
struct WorkflowResult {
    identity: Option<IdentityEvidence>,
    gateway_mode: Option<u8>,
    exit: ExitDisposition,
    /// Whole-batch success only; partial comparison evidence stays in the transcript.
    compared_pages: Vec<PageEvidence>,
    possible_pages: Vec<PageEvidence>,
    verified_pages: Vec<PageEvidence>,
    open_error: Option<Failure>,
    operation_error: Option<Failure>,
    cleanup_error: Option<Failure>,
    close_error: Option<Failure>,
    capture_error: Option<Failure>,
    journal_error: Option<Failure>,
    transcript: TranscriptSummary,
    post_exit: PostExitVerification,
}

impl WorkflowResult {
    fn new(captures: &Captures) -> Self {
        Self {
            identity: None,
            gateway_mode: None,
            exit: ExitDisposition::NotEntered,
            compared_pages: Vec::new(),
            possible_pages: Vec::new(),
            verified_pages: Vec::new(),
            open_error: None,
            operation_error: None,
            cleanup_error: None,
            close_error: None,
            capture_error: None,
            journal_error: None,
            transcript: captures.original.summary(),
            post_exit: PostExitVerification::skipped(
                SkipReason::OriginalUpdateIncomplete,
                captures.post_exit.summary(),
            ),
        }
    }

    const fn ineligible(&self) -> Option<SkipReason> {
        if self.journal_error.is_some() {
            Some(SkipReason::OriginalUpdateJournalIncomplete)
        } else if self.open_error.is_some() {
            Some(SkipReason::OriginalOpenFailed)
        } else if self.capture_error.is_some() || !self.transcript.complete {
            Some(SkipReason::OriginalCaptureIncomplete)
        } else if self.close_error.is_some() {
            Some(SkipReason::OriginalCloseFailed)
        } else if self.operation_error.is_some()
            || self.cleanup_error.is_some()
            || !matches!(self.exit, ExitDisposition::Acknowledged)
        {
            Some(SkipReason::OriginalUpdateIncomplete)
        } else {
            None
        }
    }

    const fn succeeded(&self) -> bool {
        self.ineligible().is_none() && self.post_exit.succeeded()
    }

    fn print_failures(&self) {
        for (stage, error) in [
            ("open", &self.open_error),
            ("operation", &self.operation_error),
            ("MCP exit", &self.cleanup_error),
            ("close", &self.close_error),
            ("capture", &self.capture_error),
            ("journal", &self.journal_error),
        ] {
            if let Some(error) = error {
                output::error(format_args!("Menu update {stage} failed: {error}"));
            }
        }
        if !self.post_exit.succeeded() {
            output::error(format_args!(
                "Fresh verification: {}.",
                self.post_exit.outcome
            ));
        }
    }
}

#[derive(Serialize)]
struct IntendedPage<'a> {
    address: u32,
    length: usize,
    expected: &'a [u8],
    replacement: &'a [u8],
}

impl<'a> From<&'a PageReplacement> for IntendedPage<'a> {
    fn from(replacement: &'a PageReplacement) -> Self {
        Self {
            address: replacement.page().address().as_u32(),
            length: replacement.page().len(),
            expected: replacement.expected(),
            replacement: replacement.replacement(),
        }
    }
}

#[derive(Serialize)]
struct AssignmentEvidence {
    field: &'static str,
    slot: Option<u8>,
    value: Value,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalEvent<'a> {
    Prepared {
        identity: IdentityEvidence,
        assignments: Vec<AssignmentEvidence>,
        pages: Vec<IntendedPage<'a>>,
    },
    BeforeWrite {
        page: IntendedPage<'a>,
    },
    OriginalFinished {
        evidence: &'a WorkflowResult,
    },
    Finished {
        evidence: &'a WorkflowResult,
    },
}

fn record_journal(journal: &mut Recorder<File>, event: JournalEvent<'_>) -> io::Result<()> {
    journal.record(event);
    journal.synchronize()
}

fn prepare_journal(journal: &mut Recorder<File>, plan: &MenuUpdatePlan) -> io::Result<()> {
    record_journal(
        journal,
        JournalEvent::Prepared {
            identity: IdentityEvidence::from(plan.identity()),
            assignments: plan
                .assignments()
                .iter()
                .map(|assignment| AssignmentEvidence {
                    field: assignment.field().descriptor.name,
                    slot: assignment.slot().map(kenwood_tmd750::SlotIndex::index),
                    value: Value::from(assignment.value()),
                })
                .collect(),
            pages: plan.replacements().iter().map(IntendedPage::from).collect(),
        },
    )
}

/// Synchronization failures through a cloned descriptor must remain sticky too.
struct RawSynchronization {
    file: File,
    error: Option<Failure>,
    #[cfg(test)]
    fail_next: bool,
}

impl RawSynchronization {
    const fn new(file: File) -> Self {
        Self {
            file,
            error: None,
            #[cfg(test)]
            fail_next: false,
        }
    }

    fn synchronize(&mut self) -> io::Result<()> {
        if let Some(error) = &self.error {
            return Err(io::Error::other(error.to_string()));
        }
        let result = self.synchronize_file();
        if let Err(error) = &result {
            self.error = Some(Failure::from_error(error));
        }
        result
    }

    #[cfg(test)]
    fn synchronize_file(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.fail_next) {
            return Err(io::Error::other(
                "injected pre-write capture synchronization failure",
            ));
        }
        self.file.sync_all()
    }

    #[cfg(not(test))]
    fn synchronize_file(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

fn check_cancellation(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "menu update cancelled before write intent",
        ))
    } else {
        Ok(())
    }
}

/// Cancellation cannot drop an exchange or interrupt an already approved batch.
async fn operate(
    radio: &mut Radio<impl Transport>,
    plan: &MenuUpdatePlan,
    journal: &mut Recorder<File>,
    raw: &mut RawSynchronization,
    cancelled: &AtomicBool,
    result: &mut WorkflowResult,
) -> AppResult<()> {
    check_cancellation(cancelled)?;
    let identity = radio.identify().await?;
    result.identity = Some(IdentityEvidence::from(&identity));
    if &identity != plan.identity() {
        return Err(CommandError(
            "current radio identity does not match the source backup".to_owned(),
        )
        .into());
    }
    check_cancellation(cancelled)?;
    let gateway = radio.get_dv_gateway_mode().await?;
    result.gateway_mode = Some(match gateway {
        DvGatewayMode::Off => 0,
        DvGatewayMode::Terminal => 2,
        DvGatewayMode::Unqualified(raw) => raw,
    });
    if gateway != DvGatewayMode::Off {
        return Err(
            CommandError("ordinary menu updates require current Gateway Off".to_owned()).into(),
        );
    }
    check_cancellation(cancelled)?;
    result.exit = ExitDisposition::RecoveryRequired;
    let mut session = radio.enter_mcp().await?;
    let mut intent_recorded = false;
    let outcome = session
        .compare_exchange_menu_update(
            plan,
            |replacement| {
                if !intent_recorded {
                    check_cancellation(cancelled)?;
                }
                raw.synchronize()?;
                record_journal(
                    journal,
                    JournalEvent::BeforeWrite {
                        page: replacement.into(),
                    },
                )?;
                intent_recorded = true;
                Ok(())
            },
            |_progress| {},
        )
        .await;
    result.possible_pages = session
        .journal()
        .possibly_written
        .iter()
        .copied()
        .map(PageEvidence::from)
        .collect();
    result.verified_pages = session
        .journal()
        .verified
        .iter()
        .copied()
        .map(PageEvidence::from)
        .collect();
    if session.is_ready() {
        result.exit = ExitDisposition::NotAcknowledged;
        match session.exit().await {
            Ok(()) => result.exit = ExitDisposition::Acknowledged,
            Err(error) => result.cleanup_error = Some(Failure::from_error(&error)),
        }
    }
    result.compared_pages = outcome?
        .compared_pages
        .into_iter()
        .map(PageEvidence::from)
        .collect();
    Ok(())
}

/// Own, release, and drop the original handle before returning its recorder.
async fn original(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    plan: &MenuUpdatePlan,
    mut recorder: Recorder<File>,
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
    result: &mut WorkflowResult,
) -> Recorder<File> {
    let synchronization = match recorder.synchronization_handle() {
        Ok(file) => file,
        Err(error) => {
            result.capture_error = Some(Failure::from_error(&error));
            return recorder;
        }
    };
    recorder.record(Event::OpenRequested {
        path: &endpoint.path,
        baud: DEFAULT_BAUD,
    });
    if let Err(error) = recorder.synchronize() {
        result.capture_error = Some(Failure::from_error(&error));
        return recorder;
    }
    let connection = match backend.open(endpoint, DEFAULT_BAUD) {
        Ok(connection) => connection,
        Err(error) => {
            let error = Failure::from_error(&error);
            recorder.record(Event::OpenFailed {
                error: error.clone(),
            });
            result.open_error = Some(error);
            return recorder;
        }
    };
    recorder.record(Event::OpenCompleted);
    let opening = recorder.synchronize();
    let mut radio = Radio::new(CaptureTransport::required(connection, recorder));
    let mut raw = RawSynchronization::new(synchronization);
    match opening {
        Ok(()) => {
            if let Err(error) =
                operate(&mut radio, plan, journal, &mut raw, cancelled, result).await
            {
                result.operation_error = Some(Failure::from_error(error.as_ref()));
            }
        }
        Err(error) => result.capture_error = Some(Failure::from_error(&error)),
    }
    if let Some(error) = raw.error {
        result.capture_error = Some(error);
    }
    let mut transport = radio.into_transport();
    result.close_error = close_transport(&mut transport).await;
    transport.into_recorder()
}

async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    plan: &MenuUpdatePlan,
    captures: Captures,
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult::new(&captures);
    let Captures {
        mut original,
        mut post_exit,
    } = captures;
    match prepare_journal(journal, plan) {
        Err(error) => result.journal_error = Some(Failure::from_error(&error)),
        Ok(()) => match check_cancellation(cancelled) {
            Err(error) => result.operation_error = Some(Failure::from_error(&error)),
            Ok(()) => {
                original = self::original(
                    backend,
                    endpoint,
                    plan,
                    original,
                    journal,
                    cancelled,
                    &mut result,
                )
                .await;
            }
        },
    }
    if let Err(error) = original.synchronize() {
        let _first = result
            .capture_error
            .get_or_insert_with(|| Failure::from_error(&error));
    }
    result.transcript = original.summary();
    drop(original);
    if let Err(error) = record_journal(
        journal,
        JournalEvent::OriginalFinished { evidence: &result },
    ) {
        let _first = result
            .journal_error
            .get_or_insert_with(|| Failure::from_error(&error));
    }
    let skip = result.ineligible().or_else(|| {
        (result.possible_pages.is_empty() && cancelled.load(Ordering::Relaxed))
            .then_some(SkipReason::Cancelled)
    });
    result.post_exit = if let Some(reason) = skip {
        if let Err(error) = post_exit.synchronize() {
            let _first = result
                .capture_error
                .get_or_insert_with(|| Failure::from_error(&error));
        }
        PostExitVerification::skipped(reason, post_exit.summary())
    } else {
        // An accepted write owes verification even when the user pressed Ctrl-C.
        // Required capture failures still independently prohibit protocol I/O.
        let finish_required = AtomicBool::new(false);
        let cancellation = if result.possible_pages.is_empty() {
            cancelled
        } else {
            &finish_required
        };
        reconnect::verify_required_gateway_off(
            backend,
            endpoint,
            DEFAULT_BAUD,
            plan.identity(),
            post_exit,
            cancellation,
        )
        .await
    };
    if let Err(error) = record_journal(journal, JournalEvent::Finished { evidence: &result }) {
        let _first = result
            .journal_error
            .get_or_insert_with(|| Failure::from_error(&error));
    }
    result
}

#[derive(Serialize)]
struct Report {
    format_version: u8,
    operation: &'static str,
    verification_scope: &'static str,
    software_version: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    endpoint: Endpoint,
    source_backup: PathBuf,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
}

/// Reserve private durable evidence before obtaining any radio handle.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &ApplyRequest,
) -> AppResult<()> {
    if !cfg!(unix) {
        return Err(CommandError(
            "menu updates require Unix private files and directory synchronization".to_owned(),
        )
        .into());
    }
    if !endpoint.is_tmd750() || endpoint.pid != Some(TMD750_MAIN_PID) || baud != DEFAULT_BAUD {
        return Err(CommandError(
            "menu updates require the pinned main-unit USB endpoint at 9600 baud".to_owned(),
        )
        .into());
    }
    let plan = request.prepare()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(request.output(), Arc::clone(&cancelled))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
    let mut journal = Recorder::named(
        create_private_file(&artifacts.directory.join("menu-journal.jsonl"))?,
        Arc::clone(&cancelled),
        "menu-journal.jsonl",
    );
    File::open(&artifacts.directory)?.sync_all()?;
    File::open(
        artifacts
            .directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new(".")),
    )?
    .sync_all()?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    output::line(format_args!(
        "Menu update evidence: {}. The requested setting remains in place; no automatic rollback.",
        directory.display()
    ));
    output::line(format_args!(
        "Ctrl-C cancels before write intent. Afterward the approved batch and safe verification finish unless a protocol or evidence failure prevents them."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = finish_on_interrupt(
        run_workflow(
            &mut SystemBackend::new(),
            endpoint,
            &plan,
            Captures {
                original: transcript,
                post_exit,
            },
            &mut journal,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    workflow.print_failures();
    let succeeded = workflow.succeeded() && signal_error.is_none();
    let report = Report {
        format_version: 1,
        operation: "menu_update",
        verification_scope: "immediate_complete_page_readback_and_fresh_cat_gateway_off",
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: Endpoint {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
            cat_baud: baud,
        },
        source_backup: request.source_backup().to_owned(),
        workflow,
        signal_error,
    };
    write_report(&mut report_file, &report).and_then(|()| report_file.sync_all()).map_err(|error| CommandError(format!("menu report could not be finalized: {error}; retain {}. Writes may have occurred; do not retry or restore blindly", directory.display())))?;
    output::line(format_args!(
        "Menu report: {}. {} complete pages compared; {} written pages immediately verified.",
        directory.join("report.json").display(),
        report.workflow.compared_pages.len(),
        report.workflow.verified_pages.len()
    ));
    if succeeded {
        output::line(format_args!(
            "Fresh CAT identity and Gateway Off verified. Persistence across MCP re-entry or power cycling was not tested. Refresh the backup before another edit."
        ));
        Ok(())
    } else {
        Err(CommandError(format!("menu update incomplete; {} pages possibly written. Do not retry or restore blindly. Inspect {}", report.workflow.possible_pages.len(), directory.display())).into())
    }
}

#[cfg(test)]
mod tests;
