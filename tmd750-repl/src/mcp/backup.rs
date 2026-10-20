//! Reads every standard configuration region in one MCP session, then the GPS
//! position memory when `--position-memory` asks for it.
//!
//! After the acknowledged MCP exit the original connection is closed and the
//! CAT identity tuple is re-read on a fresh connection.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    Identity, McpBackupOutcome, McpBackupReport, McpBackupStage, McpProbeExit, Progress, Radio,
};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::reconnect::{self, Backend, ReadinessVerification, SkipReason, SystemBackend};
use super::{
    Endpoint, Failure, IdentityEvidence, SegmentEvidence, close_transport, finish_on_interrupt,
};
use crate::capture::{Artifacts, CaptureKind, CaptureTransport, Recorder, TranscriptSummary};
use crate::{AppResult, CommandError, output};

/// Capture destination and scope of the configuration backup.
///
/// The captured regions cover the standard configuration and, on request, the
/// GPS position memory; custom startup-screen pixels are outside them.
#[derive(Debug, Parser)]
pub(crate) struct BackupRequest {
    /// New private capture directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
    /// Also read the GPS position memory, after the standard configuration
    /// in the same session; USB only.
    #[arg(long)]
    position_memory: bool,
}

impl BackupRequest {
    /// Capture directory given on the command line, if any.
    pub(crate) fn output(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    /// The regions the backup reads.
    pub(crate) const fn scope(&self) -> Scope {
        if self.position_memory {
            Scope::WithPositionMemory
        } else {
            Scope::Standard
        }
    }
}

/// The regions a backup reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The standard configuration schedule.
    Standard,
    /// The standard schedule, then the GPS position memory.
    WithPositionMemory,
}

impl Scope {
    /// The report's `scope` string.
    const fn label(self) -> &'static str {
        match self {
            Self::Standard => {
                "standard_configuration_without_startup_screen; unread gaps are absent, not zero-filled"
            }
            Self::WithPositionMemory => {
                "standard_configuration_and_position_memory_without_startup_screen; unread gaps are absent, not zero-filled"
            }
        }
    }
}

/// Serialized standard-page read result, shared by the transport workflows.
///
/// Holds the identity tuple, the entry reply, every acknowledged page, the MCP
/// exit disposition and the outcome.
#[derive(Debug, Serialize)]
pub(crate) struct BackupEvidence {
    identity: Option<IdentityEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position_memory: Option<Vec<SegmentEvidence>>,
    exit: super::ExitDisposition,
    complete_configuration: bool,
    outcome: BackupOutcome,
}

impl BackupEvidence {
    /// True when the library's page-order, length, entry and exit checks passed.
    pub(crate) const fn has_complete_configuration(&self) -> bool {
        self.complete_configuration
    }

    /// Number of fully acknowledged pages retained, including partial backups.
    pub(crate) const fn page_count(&self) -> usize {
        self.segments.len()
    }

    /// Protocol failure recorded by the backup exchange, if it failed.
    ///
    /// Capture and close failures are reported by their own fields.
    pub(crate) const fn error(&self) -> Option<&Failure> {
        match &self.outcome {
            BackupOutcome::Failed { error, .. } => Some(error),
            BackupOutcome::AwaitingCatVerification | BackupOutcome::Cancelled => None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum BackupOutcome {
    AwaitingCatVerification,
    Cancelled,
    Failed { stage: BackupStage, error: Failure },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BackupStage {
    Identity,
    Gateway,
    Entry,
    Read { address: u32, length: usize },
    Exit,
}

impl From<McpBackupStage> for BackupStage {
    fn from(stage: McpBackupStage) -> Self {
        match stage {
            McpBackupStage::Identity => Self::Identity,
            McpBackupStage::Gateway => Self::Gateway,
            McpBackupStage::Entry => Self::Entry,
            McpBackupStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            McpBackupStage::Exit => Self::Exit,
        }
    }
}

impl From<&McpBackupReport> for BackupEvidence {
    fn from(report: &McpBackupReport) -> Self {
        Self {
            identity: report.identity.as_ref().map(IdentityEvidence::from),
            entry_reply: report.entry_reply.clone(),
            segments: report.segments.iter().map(SegmentEvidence::from).collect(),
            position_memory: (!report.position_memory.is_empty()).then(|| {
                report
                    .position_memory
                    .iter()
                    .map(SegmentEvidence::from)
                    .collect()
            }),
            exit: report.exit.into(),
            complete_configuration: report.has_complete_configuration(),
            outcome: match &report.outcome {
                McpBackupOutcome::AwaitingCatVerification => BackupOutcome::AwaitingCatVerification,
                McpBackupOutcome::Cancelled => BackupOutcome::Cancelled,
                McpBackupOutcome::Failed { stage, error } => BackupOutcome::Failed {
                    stage: (*stage).into(),
                    error: Failure::from_error(error),
                },
            },
        }
    }
}

/// Format-4 report of a complete or interrupted standard-region backup.
#[derive(Debug, Serialize)]
pub(super) struct ArtifactReport {
    format_version: u8,
    operation: &'static str,
    software_version: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    endpoint: Endpoint,
    scope: &'static str,
    transcript: TranscriptSummary,
    backup: Option<BackupEvidence>,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    signal_error: Option<Failure>,
    post_exit_verification: ReadinessVerification,
}

impl ArtifactReport {
    /// Build the report from the workflow result and any signal failure.
    pub(super) fn new(
        endpoint: &SerialCandidate,
        baud: std::num::NonZeroU32,
        started_at_utc: String,
        finished_at_utc: String,
        result: WorkflowResult,
        signal_error: Option<Failure>,
    ) -> Self {
        Self {
            format_version: 4,
            operation: "configuration_backup",
            software_version: env!("CARGO_PKG_VERSION"),
            started_at_utc,
            finished_at_utc,
            endpoint: Endpoint {
                path: endpoint.path.clone(),
                usb_vendor_id: endpoint.vid,
                usb_product_id: endpoint.pid,
                cat_baud: baud,
            },
            scope: result.scope.label(),
            transcript: result.transcript,
            backup: result.backup.as_ref().map(BackupEvidence::from),
            open_error: result.open_error,
            close_error: result.close_error,
            signal_error,
            post_exit_verification: result.post_exit,
        }
    }
}

/// Back up the standard regions, then verify CAT on a fresh connection.
///
/// Every acknowledged page is written to the capture even when a later read,
/// the MCP exit or the close fails.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    request: &BackupRequest,
) -> AppResult<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(
        CaptureKind::Mcp,
        request.output.as_deref(),
        Arc::clone(&cancelled),
    )?;
    let post_exit = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
    output::line(format_args!(
        "MCP configuration capture: {}.",
        artifacts.directory.display()
    ));
    output::line(format_args!(
        "Reading {}; no settings writes or startup-screen pixels. Normal operation pauses during programming.",
        match request.scope() {
            Scope::Standard => "standard configuration regions only",
            Scope::WithPositionMemory => {
                "standard configuration regions, then the GPS position memory"
            }
        }
    ));
    output::line(format_args!(
        "Ctrl-C requests cancellation after the current complete exchange."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    let (result, signal_error) = finish_on_interrupt(
        run_workflow(
            &mut SystemBackend::new(),
            endpoint,
            baud,
            request.scope(),
            transcript,
            post_exit,
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let succeeded = result.succeeded() && signal_error.is_none();
    print_result(&result);
    let report = ArtifactReport::new(
        endpoint,
        baud,
        started_at_utc,
        OffsetDateTime::now_utc().format(&Rfc3339)?,
        result,
        signal_error,
    );
    super::write_report(&mut report_file, &report)?;
    report_file.sync_all()?;
    output::line(format_args!(
        "Configuration report: {}.",
        directory.join("report.json").display()
    ));
    if succeeded {
        output::line(format_args!(
            "{} complete.",
            match request.scope() {
                Scope::Standard => "Standard configuration backup",
                Scope::WithPositionMemory => "Standard configuration and position memory backup",
            }
        ));
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "MCP backup incomplete; capture retained in {}",
            directory.display()
        ))))
    }
}

#[derive(Debug)]
pub(super) struct WorkflowResult {
    pub(super) scope: Scope,
    pub(super) backup: Option<McpBackupReport>,
    pub(super) transcript: TranscriptSummary,
    pub(super) open_error: Option<Failure>,
    pub(super) close_error: Option<Failure>,
    pub(super) post_exit: ReadinessVerification,
}

impl WorkflowResult {
    pub(super) fn succeeded(&self) -> bool {
        self.open_error.is_none()
            && self.close_error.is_none()
            && self.transcript.complete
            && self.backup.as_ref().is_some_and(|backup| {
                backup.has_complete_configuration()
                    && (self.scope == Scope::Standard || backup.has_complete_position_memory())
            })
            && self.post_exit.succeeded()
    }
}

pub(super) async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    scope: Scope,
    mut original: Recorder<File>,
    post_exit: Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult {
        scope,
        backup: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        post_exit: ReadinessVerification::skipped(SkipReason::Cancelled, post_exit.summary()),
    };
    if cancelled.load(Ordering::Relaxed) {
        return result;
    }
    if let Err(error) =
        reconnect::settle_before_entry(backend, endpoint, &mut original, cancelled).await
    {
        result.open_error = Some(Failure::from_error(&error));
        result.transcript = original.summary();
        result.post_exit =
            ReadinessVerification::skipped(SkipReason::OriginalOpenFailed, post_exit.summary());
        return result;
    }
    let transport = match backend.open(endpoint, baud) {
        Ok(transport) => transport,
        Err(error) => {
            result.open_error = Some(Failure::from_error(&error));
            result.post_exit =
                ReadinessVerification::skipped(SkipReason::OriginalOpenFailed, post_exit.summary());
            return result;
        }
    };
    let mut radio = Radio::new(CaptureTransport::new(transport, original));
    let should_cancel = || cancelled.load(Ordering::Relaxed);
    let backup = match scope {
        Scope::Standard => {
            radio
                .backup_mcp_until_exit(should_cancel, print_progress)
                .await
        }
        Scope::WithPositionMemory => {
            radio
                .backup_mcp_with_position_memory_until_exit(should_cancel, print_progress)
                .await
        }
    };
    let mut transport = radio.into_transport();
    result.close_error = close_transport(&mut transport).await;
    result.transcript = transport.into_recorder().summary();
    result.post_exit = match verification_eligibility(
        &backup,
        result.close_error.as_ref(),
        &result.transcript,
        cancelled,
    ) {
        Ok(identity) => {
            reconnect::verify_readiness(backend, endpoint, baud, identity, post_exit, cancelled)
                .await
        }
        Err(reason) => ReadinessVerification::skipped(reason, post_exit.summary()),
    };
    result.backup = Some(backup);
    result
}

/// Print the first, every 64th and the last page count.
fn print_progress(progress: Progress) {
    if progress.done == 1 || progress.done.is_multiple_of(64) || progress.done == progress.total {
        output::line(format_args!(
            "Configuration pages: {}/{}.",
            progress.done, progress.total
        ));
    }
}

fn verification_eligibility<'a>(
    backup: &'a McpBackupReport,
    close_error: Option<&Failure>,
    transcript: &TranscriptSummary,
    cancelled: &AtomicBool,
) -> Result<&'a Identity, SkipReason> {
    if cancelled.load(Ordering::Relaxed) {
        return Err(SkipReason::Cancelled);
    }
    if close_error.is_some() {
        return Err(SkipReason::OriginalCloseFailed);
    }
    if !transcript.complete {
        return Err(SkipReason::OriginalCaptureIncomplete);
    }
    if !backup.has_complete_configuration() {
        return Err(SkipReason::OriginalBackupIncomplete);
    }
    backup
        .identity
        .as_ref()
        .ok_or(SkipReason::OriginalBackupIncomplete)
}

fn print_result(result: &WorkflowResult) {
    if let Some(backup) = &result.backup {
        output::line(format_args!(
            "Captured {} acknowledged configuration pages; exit: {:?}.",
            backup.segments.len(),
            backup.exit
        ));
        if !backup.position_memory.is_empty() {
            output::line(format_args!(
                "Captured {} acknowledged position memory pages.",
                backup.position_memory.len()
            ));
        }
        if let McpBackupOutcome::Failed { stage, error } = &backup.outcome {
            output::error(format_args!(
                "Configuration read failed at {stage:?}: {error}"
            ));
        }
        if matches!(
            backup.exit,
            McpProbeExit::RecoveryRequired | McpProbeExit::NotAcknowledged
        ) {
            output::error(format_args!(
                "MCP framing is uncertain. Stop using this connection and retain the capture. No recovery commands or automatic retry were sent."
            ));
        }
    }
    if let Some(error) = &result.open_error {
        output::error(format_args!("Original open failed: {error}"));
    }
    if let Some(error) = &result.close_error {
        output::error(format_args!("Original close failed: {error}"));
    }
    output::line(format_args!(
        "Fresh CAT verification: {}.",
        result.post_exit.outcome
    ));
}
