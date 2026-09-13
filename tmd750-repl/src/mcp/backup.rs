//! Standard configuration backup, with independent post-exit verification.

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{
    Identity, McpBackupOutcome, McpBackupReport, McpBackupStage, McpProbeExit, Radio,
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

/// Standard configuration only; custom startup-screen pixels are excluded.
#[derive(Debug, Parser)]
pub(crate) struct BackupRequest {
    /// New private capture directory; its parent must exist.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Evidence {
    identity: Option<IdentityEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: super::ExitDisposition,
    complete_configuration: bool,
    outcome: BackupOutcome,
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
    Entry,
    Read { address: u32, length: usize },
    Exit,
}

impl From<McpBackupStage> for BackupStage {
    fn from(stage: McpBackupStage) -> Self {
        match stage {
            McpBackupStage::Identity => Self::Identity,
            McpBackupStage::Entry => Self::Entry,
            McpBackupStage::Read { page } => Self::Read {
                address: page.address().as_u32(),
                length: page.len(),
            },
            McpBackupStage::Exit => Self::Exit,
        }
    }
}

impl From<&McpBackupReport> for Evidence {
    fn from(report: &McpBackupReport) -> Self {
        Self {
            identity: report.identity.as_ref().map(IdentityEvidence::from),
            entry_reply: report.entry_reply.clone(),
            segments: report
                .segments
                .iter()
                .map(|segment| SegmentEvidence {
                    address: segment.page.address().as_u32(),
                    length: segment.page.len(),
                    data: segment.data.clone(),
                })
                .collect(),
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

/// Serialized evidence from a complete or interrupted standard-region backup.
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
    backup: Option<Evidence>,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    signal_error: Option<Failure>,
    post_exit_verification: ReadinessVerification,
}

impl ArtifactReport {
    /// Preserve original read evidence and the complete fresh-CAT result.
    pub(super) fn new(
        endpoint: &SerialCandidate,
        baud: u32,
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
            scope: "standard_configuration_without_startup_screen; unread gaps are absent, not zero-filled",
            transcript: result.transcript,
            backup: result.backup.as_ref().map(Evidence::from),
            open_error: result.open_error,
            close_error: result.close_error,
            signal_error,
            post_exit_verification: result.post_exit,
        }
    }
}

/// Capture every acknowledged page even when cleanup or later reads fail.
pub(super) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
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
        "Reading standard configuration regions only; no settings writes or startup-screen pixels. Normal operation pauses during programming."
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
            "Standard configuration backup complete. Firmware schema compatibility and physical-unit continuity are not proved."
        ));
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "MCP backup incomplete; retained evidence in {}",
            directory.display()
        ))))
    }
}

#[derive(Debug)]
pub(super) struct WorkflowResult {
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
            && self
                .backup
                .as_ref()
                .is_some_and(McpBackupReport::has_complete_configuration)
            && self.post_exit.succeeded()
    }
}

pub(super) async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original: Recorder<File>,
    post_exit: Recorder<File>,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let mut result = WorkflowResult {
        backup: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        post_exit: ReadinessVerification::skipped(SkipReason::Cancelled, post_exit.summary()),
    };
    if cancelled.load(Ordering::Relaxed) {
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
    let backup = radio
        .backup_mcp_until_exit(
            || cancelled.load(Ordering::Relaxed),
            |progress| {
                if progress.done == 1 || progress.done % 64 == 0 || progress.done == progress.total
                {
                    output::line(format_args!(
                        "Configuration pages: {}/{}.",
                        progress.done, progress.total
                    ));
                }
            },
        )
        .await;
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
                "Radio state is uncertain. Fully power-cycle before reconnecting; no recovery commands were sent."
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
