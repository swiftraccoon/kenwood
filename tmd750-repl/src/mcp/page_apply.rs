//! Page plans applied over one MCP connection followed by a fresh CAT check.
//!
//! The first connection compares each complete page, writes the changed ones,
//! reads each back and exits MCP; the fresh check reads the identity tuple and
//! Gateway Off, with one open on the main-unit endpoint, the bounded
//! silent-`ID` retry on the operation-panel endpoint, or one reopen of the
//! pinned channel over native Bluetooth. Persistence across a later MCP
//! session or a power cycle is not checked. Menu updates, repeater-list edits
//! and channel-name updates share this workflow; each supplies its
//! [`PagePlan`].

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID};
use kenwood_tmd750::{
    DvGatewayMode, Identity, McpCompareExchangeReport, MenuUpdatePlan, Page, PageReplacement, Radio,
};
use kenwood_transport::Transport;
use serde::Serialize;
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::link::{Opening, Refusal, ReportEndpoint, UsbLink, WriteLink};
use super::reconnect::{PostExit, SkipReason, SystemBackend};
use super::{
    ExitDisposition, Failure, IdentityEvidence, close_transport, finish_on_interrupt, write_report,
};
use crate::capture::{Artifacts, CaptureKind, Recorder, TranscriptSummary, create_private_file};
use crate::native::opening::History;
use crate::{AppResult, CommandError, output};

/// The operations that run as page plans, with their report vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlanKind {
    /// `mcp menu apply`.
    Menu,
    /// `mcp repeater set`.
    Repeater,
    /// `mcp channel-names apply`.
    ChannelNames,
    /// `mcp callsign-list apply`.
    CallsignList,
}

impl PlanKind {
    /// Sentence-initial name of the operation.
    const fn label(self) -> &'static str {
        match self {
            Self::Menu => "Menu update",
            Self::Repeater => "Repeater update",
            Self::ChannelNames => "Channel name update",
            Self::CallsignList => "Callsign list update",
        }
    }

    /// Name of the operation inside a sentence.
    const fn noun(self) -> &'static str {
        match self {
            Self::Menu => "menu update",
            Self::Repeater => "repeater update",
            Self::ChannelNames => "channel name update",
            Self::CallsignList => "callsign list update",
        }
    }

    /// Sentence-initial name of the report.
    const fn report_label(self) -> &'static str {
        match self {
            Self::Menu => "Menu report",
            Self::Repeater => "Repeater report",
            Self::ChannelNames => "Channel name report",
            Self::CallsignList => "Callsign list report",
        }
    }

    /// Name of the report inside a sentence.
    const fn report_noun(self) -> &'static str {
        match self {
            Self::Menu => "menu report",
            Self::Repeater => "repeater report",
            Self::ChannelNames => "channel name report",
            Self::CallsignList => "callsign list report",
        }
    }

    /// The refusal of a radio whose DV Gateway is not Off.
    const fn gateway_refusal(self) -> &'static str {
        match self {
            Self::Menu => "ordinary menu updates require current Gateway Off",
            Self::Repeater => "repeater updates require current Gateway Off",
            Self::ChannelNames => "channel name updates require current Gateway Off",
            Self::CallsignList => "callsign list updates require current Gateway Off",
        }
    }

    /// The report's `operation` string.
    const fn operation(self) -> &'static str {
        match self {
            Self::Menu => "menu_update",
            Self::Repeater => "repeater_update",
            Self::ChannelNames => "channel_names_update",
            Self::CallsignList => "callsign_list_update",
        }
    }

    /// The journal's file name in the capture directory.
    pub(super) const fn journal(self) -> &'static str {
        match self {
            Self::Menu => "menu-journal.jsonl",
            Self::Repeater => "repeater-journal.jsonl",
            Self::ChannelNames => "channel-names-journal.jsonl",
            Self::CallsignList => "callsign-list-journal.jsonl",
        }
    }

    /// The report's `format_version` for the link the plan runs over.
    const fn format_version(self, native: bool) -> u8 {
        match (self, native) {
            (Self::Menu, true) => 2,
            (Self::Menu, false) => 3,
            (Self::Repeater | Self::ChannelNames | Self::CallsignList, _) => 1,
        }
    }
}

/// One prepared page plan: its identity, its complete page replacements and
/// the description its journal records.
pub(super) struct PagePlan<'a> {
    kind: PlanKind,
    identity: &'a Identity,
    replacements: &'a [PageReplacement],
    /// Fields flattened into the journal's `prepared` event.
    description: Map<String, Value>,
    /// The menu plan, applied through its own guarded session call.
    menu: Option<&'a MenuUpdatePlan>,
}

impl<'a> PagePlan<'a> {
    /// A menu plan; its journal records `assignments`.
    pub(super) fn menu(plan: &'a MenuUpdatePlan, description: Map<String, Value>) -> Self {
        Self {
            kind: PlanKind::Menu,
            identity: plan.identity(),
            replacements: plan.replacements(),
            description,
            menu: Some(plan),
        }
    }

    /// A plan of complete page replacements against `identity`'s backup.
    pub(super) const fn pages(
        kind: PlanKind,
        identity: &'a Identity,
        replacements: &'a [PageReplacement],
        description: Map<String, Value>,
    ) -> Self {
        Self {
            kind,
            identity,
            replacements,
            description,
            menu: None,
        }
    }
}

pub(super) struct Captures {
    pub(super) original: Recorder<File>,
    pub(super) post_exit: Recorder<File>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct PageEvidence {
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

/// Serialized outcome of one apply session.
///
/// Holds the identity tuple, the observed Gateway mode, the MCP exit
/// disposition, the pages compared before the write, the pages that may have
/// been written, the pages whose readback matched and the fresh CAT check. A
/// page stays in `possible_pages` whether or not its readback matched, and
/// nothing is rolled back.
#[derive(Debug, Serialize)]
pub(super) struct WorkflowResult {
    pub(super) identity: Option<IdentityEvidence>,
    pub(super) gateway_mode: Option<u8>,
    pub(super) exit: ExitDisposition,
    /// Pages compared before the write, filled only when the whole batch
    /// succeeded; partial comparisons stay in the transcript.
    pub(super) compared_pages: Vec<PageEvidence>,
    pub(super) possible_pages: Vec<PageEvidence>,
    pub(super) verified_pages: Vec<PageEvidence>,
    /// Native Bluetooth opening attempts; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) opening: Option<History>,
    pub(super) open_error: Option<Failure>,
    pub(super) operation_error: Option<Failure>,
    pub(super) cleanup_error: Option<Failure>,
    /// The failed hold of the connection after the exit; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) hold_error: Option<Failure>,
    pub(super) close_error: Option<Failure>,
    pub(super) capture_error: Option<Failure>,
    pub(super) journal_error: Option<Failure>,
    pub(super) transcript: TranscriptSummary,
    pub(super) post_exit: PostExit,
}

impl WorkflowResult {
    fn new(captures: &Captures, link: &impl WriteLink) -> Self {
        Self {
            identity: None,
            gateway_mode: None,
            exit: ExitDisposition::NotEntered,
            compared_pages: Vec::new(),
            possible_pages: Vec::new(),
            verified_pages: Vec::new(),
            opening: None,
            open_error: None,
            operation_error: None,
            cleanup_error: None,
            hold_error: None,
            close_error: None,
            capture_error: None,
            journal_error: None,
            transcript: captures.original.summary(),
            post_exit: link.skipped(
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
        } else if self.hold_error.is_some() {
            Some(SkipReason::PostExitHoldFailed)
        } else {
            None
        }
    }

    pub(super) const fn succeeded(&self) -> bool {
        self.ineligible().is_none() && self.post_exit.succeeded()
    }

    fn print_failures(&self, kind: PlanKind) {
        for (stage, error) in [
            ("open", &self.open_error),
            ("operation", &self.operation_error),
            ("MCP exit", &self.cleanup_error),
            ("post-exit hold", &self.hold_error),
            ("close", &self.close_error),
            ("capture", &self.capture_error),
            ("journal", &self.journal_error),
        ] {
            if let Some(error) = error {
                output::error(format_args!("{} {stage} failed: {error}", kind.label()));
            }
        }
        if !self.post_exit.succeeded() {
            output::error(format_args!(
                "Fresh verification: {}.",
                self.post_exit.outcome()
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
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalEvent<'a> {
    Prepared {
        identity: IdentityEvidence,
        #[serde(flatten)]
        description: &'a Map<String, Value>,
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

fn prepare_journal(journal: &mut Recorder<File>, plan: &PagePlan<'_>) -> io::Result<()> {
    record_journal(
        journal,
        JournalEvent::Prepared {
            identity: IdentityEvidence::from(plan.identity),
            description: &plan.description,
            pages: plan.replacements.iter().map(IntendedPage::from).collect(),
        },
    )
}

/// Second descriptor on the transcript file, fsynced before a write intent.
///
/// The first failure is retained and returned by every later call.
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

fn cancellation(kind: PlanKind) -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        format!("{} cancelled before write intent", kind.noun()),
    )
}

fn check_cancellation(kind: PlanKind, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(cancellation(kind))
    } else {
        Ok(())
    }
}

/// Run the identity, Gateway and MCP write sequence on one open connection.
///
/// Cancellation is checked between exchanges only: an in-flight exchange is
/// always awaited, and a batch whose first write intent is journaled runs to
/// its MCP exit.
async fn operate(
    radio: &mut Radio<impl Transport>,
    plan: &PagePlan<'_>,
    journal: &mut Recorder<File>,
    raw: &mut RawSynchronization,
    cancelled: &AtomicBool,
    result: &mut WorkflowResult,
) -> AppResult<()> {
    let kind = plan.kind;
    check_cancellation(kind, cancelled)?;
    let identity = radio.identify().await?;
    result.identity = Some(IdentityEvidence::from(&identity));
    if &identity != plan.identity {
        return Err(CommandError(
            "current radio identity does not match the source backup".to_owned(),
        )
        .into());
    }
    check_cancellation(kind, cancelled)?;
    let gateway = radio.get_dv_gateway_mode().await?;
    result.gateway_mode = Some(match gateway {
        DvGatewayMode::Off => 0,
        DvGatewayMode::Terminal => 2,
        DvGatewayMode::Unnamed(raw) => raw,
    });
    if gateway != DvGatewayMode::Off {
        return Err(CommandError(kind.gateway_refusal().to_owned()).into());
    }
    check_cancellation(kind, cancelled)?;
    result.exit = ExitDisposition::RecoveryRequired;
    let mut session = radio.enter_mcp().await?;
    let mut intent_recorded = false;
    let before_write = |replacement: &PageReplacement| {
        if !intent_recorded {
            check_cancellation(kind, cancelled)?;
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
    };
    let outcome: AppResult<McpCompareExchangeReport> = match plan.menu {
        Some(menu) => session
            .compare_exchange_menu_update(menu, before_write, |_progress| {})
            .await
            .map_err(Into::into),
        None => session
            .compare_exchange_pages(plan.replacements, before_write, |_progress| {})
            .await
            .map_err(Into::into),
    };
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

/// Open the session over `link`, run the plan, hold after an acknowledged
/// exit, close the connection, and return its recorder.
async fn original(
    link: &mut impl WriteLink,
    plan: &PagePlan<'_>,
    recorder: Recorder<File>,
    post_exit: &mut Recorder<File>,
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
    let opened = link.open_session(recorder, false, cancelled).await;
    result.opening = link.take_opening_history();
    let mut transport = match opened {
        Opening::Opened(transport) => transport,
        Opening::Refused { recorder, refusal } => {
            match refusal {
                Refusal::Capture(failure) => result.capture_error = Some(failure),
                Refusal::Cancelled => {
                    result.operation_error = Some(Failure::from_error(&cancellation(plan.kind)));
                }
                Refusal::Open(failure) => result.open_error = Some(failure),
            }
            return recorder;
        }
    };
    let opening = transport.synchronize();
    let mut radio = link.radio(transport);
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
    if matches!(result.exit, ExitDisposition::Acknowledged) {
        // Once a page may have been written, a cancellation signal no longer
        // interrupts the hold.
        let finish = AtomicBool::new(false);
        let continuing = if result.possible_pages.is_empty() {
            cancelled
        } else {
            &finish
        };
        if let Err(error) = link.hold_after_exit(post_exit, continuing).await
            && error.kind() != io::ErrorKind::Interrupted
        {
            result.hold_error = Some(Failure::from_error(&error));
        }
    }
    let mut transport = radio.into_transport();
    result.close_error = close_transport(&mut transport).await;
    transport.into_recorder()
}

pub(super) async fn run_workflow(
    link: &mut impl WriteLink,
    plan: &PagePlan<'_>,
    captures: Captures,
    journal: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult::new(&captures, &*link);
    let Captures {
        mut original,
        mut post_exit,
    } = captures;
    match prepare_journal(journal, plan) {
        Err(error) => result.journal_error = Some(Failure::from_error(&error)),
        Ok(()) => match check_cancellation(plan.kind, cancelled) {
            Err(error) => result.operation_error = Some(Failure::from_error(&error)),
            Ok(()) => {
                original = self::original(
                    link,
                    plan,
                    original,
                    &mut post_exit,
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
        link.skipped(reason, post_exit.summary())
    } else {
        // Once a page may have been written, the post-exit check runs even after
        // Ctrl-C; a capture failure still stops all protocol I/O.
        let finish_required = AtomicBool::new(false);
        let cancellation = if result.possible_pages.is_empty() {
            cancelled
        } else {
            &finish_required
        };
        link.verify_post_exit(plan.identity, true, post_exit, cancellation)
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
    /// `native_bluetooth` over native Bluetooth; absent over USB.
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<&'static str>,
    verification_scope: &'static str,
    software_version: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    endpoint: ReportEndpoint,
    source_backup: PathBuf,
    workflow: WorkflowResult,
    signal_error: Option<Failure>,
}

/// Check that `endpoint` and `baud` admit a USB page plan of `kind`.
///
/// Returns `CommandError` on a non-Unix host, or when the endpoint is not a
/// TM-D750 main-unit or operation-panel USB endpoint at 9600 baud.
pub(super) fn admit_usb(endpoint: &SerialCandidate, baud: u32, kind: PlanKind) -> AppResult<()> {
    if !cfg!(unix) {
        return Err(CommandError(format!(
            "{}s require Unix private files and directory synchronization",
            kind.noun()
        ))
        .into());
    }
    if !endpoint.is_tmd750()
        || !matches!(endpoint.pid, Some(TMD750_MAIN_PID | TMD750_PANEL_PID))
        || baud != DEFAULT_BAUD
    {
        return Err(CommandError(format!(
            "{}s require a TM-D750 main-unit or operation-panel USB endpoint at 9600 baud",
            kind.noun()
        ))
        .into());
    }
    Ok(())
}

/// Run `plan` over the USB endpoint that [`admit_usb`] admitted.
pub(super) async fn run_usb(
    endpoint: &SerialCandidate,
    baud: u32,
    plan: &PagePlan<'_>,
    output: Option<&Path>,
    source_backup: &Path,
) -> AppResult<()> {
    let mut backend = SystemBackend::new();
    run_prepared(
        plan,
        &mut UsbLink::new(&mut backend, endpoint, baud),
        output,
        source_backup,
    )
    .await
}

/// Reserve the capture files and journal, then run the planned update over
/// `link` and write its report.
///
/// Nothing is opened until every capture file and the journal exist and
/// their directories are synchronized.
pub(super) async fn run_prepared(
    plan: &PagePlan<'_>,
    link: &mut impl WriteLink,
    output: Option<&Path>,
    source_backup: &Path,
) -> AppResult<()> {
    let kind = plan.kind;
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(CaptureKind::Mcp, output, Arc::clone(&cancelled))?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
    let mut journal = Recorder::named(
        create_private_file(&artifacts.directory.join(kind.journal()))?,
        Arc::clone(&cancelled),
        kind.journal(),
    );
    File::open(&artifacts.directory)?.sync_all()?;
    File::open(
        artifacts
            .directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?
    .sync_all()?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    output::line(format_args!(
        "{} capture: {}. The requested setting remains in place; no automatic rollback.",
        kind.label(),
        directory.display()
    ));
    output::line(format_args!(
        "Ctrl-C cancels before the write is journaled. After that the batch and its verification finish unless a protocol or capture failure stops them."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (workflow, signal_error) = finish_on_interrupt(
        Box::pin(run_workflow(
            link,
            plan,
            Captures {
                original: transcript,
                post_exit,
            },
            &mut journal,
            &cancelled,
        )),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    workflow.print_failures(kind);
    let succeeded = workflow.succeeded() && signal_error.is_none();
    let report = Report {
        format_version: kind.format_version(link.native()),
        operation: kind.operation(),
        transport: link.native().then_some("native_bluetooth"),
        verification_scope: "immediate_complete_page_readback_and_fresh_cat_gateway_off",
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: link.report_endpoint(),
        source_backup: source_backup.to_owned(),
        workflow,
        signal_error,
    };
    write_report(&mut report_file, &report).and_then(|()| report_file.sync_all()).map_err(|error| CommandError(format!("{} could not be finalized: {error}; retain {}. Writes may have occurred; do not retry or restore blindly", kind.report_noun(), directory.display())))?;
    output::line(format_args!(
        "{}: {}. {} complete pages compared; {} written pages immediately verified.",
        kind.report_label(),
        directory.join("report.json").display(),
        report.workflow.compared_pages.len(),
        report.workflow.verified_pages.len()
    ));
    if succeeded {
        output::line(format_args!(
            "Fresh CAT identity and Gateway Off verified. Refresh the backup before another edit."
        ));
        Ok(())
    } else {
        Err(CommandError(format!(
            "{} incomplete; {} pages possibly written. Do not retry or restore blindly. Inspect {}",
            kind.noun(),
            report.workflow.possible_pages.len(),
            directory.display()
        ))
        .into())
    }
}

#[cfg(test)]
mod tests;
