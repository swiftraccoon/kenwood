//! Startup-only MCP workflows and offline configuration inspection.

mod backup;
mod capture;
mod pm1_trial;
mod reconnect;
mod reconnect_policy;
mod snapshot;
mod terminal;
mod text;
mod text_set;

use std::error::Error as StdError;
use std::fs::File;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser, Subcommand};
use kenwood_tmd750::transport::{SerialCandidate, Transport};
use kenwood_tmd750::{
    Identity, McpProbeExit, McpProbeOutcome, McpProbeReport, McpProbeStage, Radio,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{AppResult, output};
use capture::{Artifacts, CaptureTransport, Recorder, TranscriptSummary};
use reconnect::{Backend, PostExitVerification, SkipReason, SystemBackend};

/// Dedicated MCP workflows; none accept arbitrary requests or write addresses.
#[derive(Debug, Parser)]
#[command(name = "mcp", about, color = clap::ColorChoice::Never)]
struct McpCli {
    #[command(subcommand)]
    command: McpCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Capture two fixed memory fragments, MCP exit, and return to CAT.
    Probe(ProbeRequest),
    /// Back up every standard configuration region, then verify fresh CAT.
    Backup(backup::BackupRequest),
    /// Inspect text offline or explicitly apply the qualified PM1 name setting.
    Text(text::TextRequest),
    /// Inspect captured Terminal settings offline; never activates the gateway.
    Terminal(terminal::TerminalRequest),
    /// Run the separately approved, fixed PM1 rename-and-restore experiment.
    Pm1Trial(pm1_trial::TrialRequest),
}

impl McpCommand {
    /// Require a pinned endpoint for every workflow that reconnects.
    pub(crate) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        match self {
            Self::Probe(request) => request.validate_endpoint_selection(explicit_port),
            Self::Text(request) => request.validate_endpoint_selection(explicit_port),
            Self::Terminal(_) => Ok(()),
            Self::Backup(_) | Self::Pm1Trial(_) if explicit_port => Ok(()),
            Self::Backup(_) => Err(Box::new(crate::CommandError(
                "mcp backup requires an explicit --port before mcp".to_owned(),
            ))),
            Self::Pm1Trial(_) => Err(Box::new(crate::CommandError(
                "mcp pm1-trial requires an explicit --port before mcp".to_owned(),
            ))),
        }
    }
}

/// Execute commands that must not enumerate or open radio endpoints.
pub(crate) fn run_offline(request: &McpCommand) -> Option<AppResult<()>> {
    match request {
        McpCommand::Text(request) => text::run_offline(request),
        McpCommand::Terminal(request) => Some(terminal::run(request)),
        McpCommand::Probe(_) | McpCommand::Backup(_) | McpCommand::Pm1Trial(_) => None,
    }
}

/// Capture destination, independent of radio settings or memory addresses.
#[derive(Debug, Parser)]
pub(crate) struct ProbeRequest {
    /// New capture directory; its parent must exist. Never overwrite a capture.
    ///
    /// If omitted, reserve a unique directory below ./captures/.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,

    /// Close after MCP exit, then verify CAT on one fresh connection.
    ///
    /// Requires an explicit --port before mcp. Never follows a changed path.
    #[arg(long)]
    verify_reconnect: bool,
}

impl ProbeRequest {
    /// Reject an unpinned reconnect workflow before enumeration or capture.
    pub(crate) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        if self.verify_reconnect && !explicit_port {
            return Err(Box::new(crate::CommandError(
                "--verify-reconnect requires an explicit --port before mcp; USB paths are not physical radio identities"
                    .to_owned(),
            )));
        }
        Ok(())
    }
}

/// Parse original OS arguments without lowercasing paths or splitting spaces.
pub(crate) fn parse(arguments: &[String]) -> Result<McpCommand, clap::Error> {
    Ok(McpCli::try_parse_from(arguments)?.command)
}

/// Preserve both the outer error and its source chain in machine-readable form.
#[derive(Clone, Debug, Serialize)]
struct Failure {
    message: String,
    causes: Vec<String>,
}

impl Failure {
    fn from_error(error: &(dyn StdError + 'static)) -> Self {
        let mut causes = Vec::new();
        let mut source = error.source();
        while let Some(cause) = source {
            causes.push(cause.to_string());
            source = cause.source();
        }
        Self {
            message: error.to_string(),
            causes,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)?;
        for cause in &self.causes {
            write!(formatter, ": {cause}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct Endpoint {
    path: String,
    usb_vendor_id: Option<u16>,
    usb_product_id: Option<u16>,
    cat_baud: u32,
}

#[derive(Debug, Serialize)]
struct ArtifactReport {
    format_version: u8,
    software_version: &'static str,
    started_at_utc: String,
    finished_at_utc: String,
    endpoint: Endpoint,
    transcript: TranscriptSummary,
    probe: Option<ProbeEvidence>,
    open_error: Option<Failure>,
    signal_error: Option<Failure>,
    close_error: Option<Failure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_exit_verification: Option<PostExitVerification>,
}

#[derive(Debug, Serialize)]
struct ProbeEvidence {
    identity: Option<IdentityEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: ExitDisposition,
    cat_identity: Option<IdentityEvidence>,
    outcome: Outcome,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct IdentityEvidence {
    model: String,
    firmware: String,
    radio_type: String,
}

impl From<&Identity> for IdentityEvidence {
    fn from(identity: &Identity) -> Self {
        Self {
            model: identity.model.to_string(),
            firmware: identity.firmware.to_string(),
            radio_type: identity.radio_type.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SegmentEvidence {
    address: u32,
    length: usize,
    data: Vec<u8>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExitDisposition {
    NotEntered,
    RecoveryRequired,
    NotAcknowledged,
    Acknowledged,
}

impl From<McpProbeExit> for ExitDisposition {
    fn from(exit: McpProbeExit) -> Self {
        match exit {
            McpProbeExit::NotEntered => Self::NotEntered,
            McpProbeExit::RecoveryRequired => Self::RecoveryRequired,
            McpProbeExit::NotAcknowledged => Self::NotAcknowledged,
            McpProbeExit::Acknowledged => Self::Acknowledged,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Identity,
    Entry,
    GlobalRead,
    SlotRead,
    Exit,
    CatVerification,
}

impl From<McpProbeStage> for Stage {
    fn from(stage: McpProbeStage) -> Self {
        match stage {
            McpProbeStage::Identity => Self::Identity,
            McpProbeStage::Entry => Self::Entry,
            McpProbeStage::GlobalRead => Self::GlobalRead,
            McpProbeStage::SlotRead => Self::SlotRead,
            McpProbeStage::Exit => Self::Exit,
            McpProbeStage::CatVerification => Self::CatVerification,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
    Complete,
    AwaitingCatVerification,
    Cancelled,
    Failed { stage: Stage, error: Failure },
}

impl From<&McpProbeReport> for ProbeEvidence {
    fn from(report: &McpProbeReport) -> Self {
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
            cat_identity: report.cat_identity.as_ref().map(IdentityEvidence::from),
            outcome: match &report.outcome {
                McpProbeOutcome::Complete => Outcome::Complete,
                McpProbeOutcome::AwaitingCatVerification => Outcome::AwaitingCatVerification,
                McpProbeOutcome::Cancelled => Outcome::Cancelled,
                McpProbeOutcome::Failed { stage, error } => Outcome::Failed {
                    stage: (*stage).into(),
                    error: Failure::from_error(error),
                },
            },
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum ProbeError {
    #[error("MCP capture could not be reserved at {path}: {source}")]
    Capture { path: PathBuf, source: io::Error },
    #[error("MCP probe did not complete successfully; inspect {report}")]
    Incomplete { report: PathBuf },
    #[error(
        "could not finish MCP report {path}: {source}; transcript may contain partial evidence"
    )]
    Report { path: PathBuf, source: io::Error },
}

/// Open only the selected USB connection, capture, close, and publish the report.
pub(crate) async fn run(
    endpoint: &SerialCandidate,
    baud: u32,
    request: &McpCommand,
) -> AppResult<()> {
    match request {
        McpCommand::Probe(request) => run_probe(endpoint, baud, request).await,
        McpCommand::Backup(request) => backup::run(endpoint, baud, request).await,
        McpCommand::Text(request) => text::run_selected(endpoint, baud, request).await,
        McpCommand::Terminal(request) => terminal::run(request),
        McpCommand::Pm1Trial(request) => pm1_trial::run(endpoint, baud, request).await,
    }
}

async fn run_probe(endpoint: &SerialCandidate, baud: u32, request: &ProbeRequest) -> AppResult<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts =
        Artifacts::create(request.output.as_deref(), Arc::clone(&cancelled)).map_err(|source| {
            ProbeError::Capture {
                path: request
                    .output
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("captures")),
                source,
            }
        })?;
    let post_exit = if request.verify_reconnect {
        Some(
            artifacts
                .reserve_post_exit(Arc::clone(&cancelled))
                .map_err(|source| ProbeError::Capture {
                    path: artifacts.directory.clone(),
                    source,
                })?,
        )
    } else {
        None
    };
    output::line(format_args!(
        "MCP capture: {}.",
        artifacts.directory.display()
    ));
    output::line(format_args!(
        "Fixed MCP reads only; no settings writes. Ctrl-C requests safe cancellation."
    ));
    output::line(format_args!(
        "Programming mode temporarily interrupts normal radio operation."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let Artifacts {
        directory,
        report: mut report_file,
        transcript,
    } = artifacts;
    let mut backend = SystemBackend::new();
    let (result, signal_error) = finish_on_interrupt(
        run_workflow(
            &mut backend,
            endpoint,
            baud,
            WorkflowCaptures {
                original: transcript,
                post_exit,
            },
            &cancelled,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    print_workflow_result(&result);
    let report = ArtifactReport {
        format_version: if request.verify_reconnect { 2 } else { 1 },
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: Endpoint {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
            cat_baud: baud,
        },
        transcript: result.transcript,
        probe: result.probe.as_ref().map(ProbeEvidence::from),
        open_error: result.open_error,
        signal_error,
        close_error: result.close_error,
        post_exit_verification: result.post_exit,
    };
    let report_path = directory.join("report.json");
    write_report(&mut report_file, &report).map_err(|source| ProbeError::Report {
        path: report_path.clone(),
        source,
    })?;
    output::line(format_args!("MCP report: {}.", report_path.display()));
    if report.succeeded() {
        Ok(())
    } else {
        Err(Box::new(ProbeError::Incomplete {
            report: report_path,
        }))
    }
}

impl ArtifactReport {
    const fn succeeded(&self) -> bool {
        self.open_error.is_none()
            && self.signal_error.is_none()
            && self.close_error.is_none()
            && self.transcript.complete
            && match (&self.probe, &self.post_exit_verification) {
                (Some(probe), None) => matches!(probe.outcome, Outcome::Complete),
                (Some(probe), Some(verification)) => {
                    matches!(probe.outcome, Outcome::AwaitingCatVerification)
                        && matches!(probe.exit, ExitDisposition::Acknowledged)
                        && verification.succeeded()
                }
                (None, _) => false,
            }
    }
}

#[derive(Debug)]
struct WorkflowCaptures {
    original: Recorder<File>,
    post_exit: Option<Recorder<File>>,
}

#[derive(Debug)]
struct WorkflowResult {
    probe: Option<McpProbeReport>,
    transcript: TranscriptSummary,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    post_exit: Option<PostExitVerification>,
}

async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    captures: WorkflowCaptures,
    cancelled: &AtomicBool,
) -> WorkflowResult {
    let WorkflowCaptures {
        original,
        post_exit,
    } = captures;
    let mut result = WorkflowResult {
        probe: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        post_exit: None,
    };
    if cancelled.load(Ordering::Relaxed) {
        result.probe = Some(McpProbeReport {
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            cat_identity: None,
            outcome: McpProbeOutcome::Cancelled,
        });
        result.post_exit = post_exit.map(|recorder| {
            PostExitVerification::skipped(SkipReason::Cancelled, recorder.summary())
        });
        return result;
    }
    let transport = match backend.open(endpoint, baud) {
        Ok(transport) => transport,
        Err(error) => {
            result.open_error = Some(Failure::from_error(&error));
            result.post_exit = post_exit.map(|recorder| {
                PostExitVerification::skipped(SkipReason::OriginalOpenFailed, recorder.summary())
            });
            return result;
        }
    };
    let mut radio = Radio::new(CaptureTransport::new(transport, original));
    radio.set_cat_baud(baud);
    let mut probe = if post_exit.is_some() {
        radio
            .probe_mcp_until_exit(|| cancelled.load(Ordering::Relaxed))
            .await
    } else {
        radio.probe_mcp(|| cancelled.load(Ordering::Relaxed)).await
    };
    let mut transport = radio.into_transport();
    result.close_error = close_transport(&mut transport).await;
    result.transcript = transport.into_recorder().summary();
    if post_exit.is_none()
        && cancelled.load(Ordering::Relaxed)
        && matches!(probe.outcome, McpProbeOutcome::Complete)
    {
        probe.outcome = McpProbeOutcome::Cancelled;
    }
    if let Some(recorder) = post_exit {
        result.post_exit = Some(
            match verification_eligibility(
                &probe,
                result.close_error.as_ref(),
                &result.transcript,
                cancelled,
            ) {
                Ok(identity) => {
                    reconnect::verify(backend, endpoint, baud, identity, recorder, cancelled).await
                }
                Err(reason) => PostExitVerification::skipped(reason, recorder.summary()),
            },
        );
    }
    result.probe = Some(probe);
    result
}

fn verification_eligibility<'a>(
    probe: &'a McpProbeReport,
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
    if !matches!(probe.outcome, McpProbeOutcome::AwaitingCatVerification)
        || probe.exit != McpProbeExit::Acknowledged
        || probe.segments.len() != 2
        || !probe.segments.iter().zip([(8, 40), (327_681, 255)]).all(
            |(segment, (address, length))| {
                segment.page.address().as_u32() == address
                    && segment.page.len() == length
                    && segment.data.len() == length
            },
        )
    {
        return Err(SkipReason::OriginalProbeIncomplete);
    }
    probe
        .identity
        .as_ref()
        .ok_or(SkipReason::OriginalProbeIncomplete)
}

fn write_report(writer: &mut impl Write, report: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *writer, report)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

async fn close_transport(transport: &mut impl Transport) -> Option<Failure> {
    match tokio::time::timeout(Duration::from_secs(2), transport.close()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(Failure::from_error(&error)),
        Err(error) => Some(Failure::from_error(&error)),
    }
}

/// Only the signal wait is disposable; the same probe is awaited on both paths.
async fn finish_on_interrupt<F, S>(
    probe: F,
    signal: S,
    cancelled: &AtomicBool,
) -> (F::Output, Option<Failure>)
where
    F: Future,
    S: Future<Output = io::Result<()>>,
{
    tokio::pin!(probe);
    tokio::pin!(signal);
    tokio::select! {
        biased;
        signal_result = &mut signal => {
            cancelled.store(true, Ordering::Relaxed);
            output::line(format_args!(
                "Cancellation requested. Finishing complete exchanges and any required restoration or exit verification."
            ));
            (probe.await, signal_result.err().as_ref().map(|error| Failure::from_error(error)))
        }
        result = &mut probe => (result, None),
    }
}

fn print_workflow_result(result: &WorkflowResult) {
    if let Some(error) = &result.open_error {
        output::error(format_args!(
            "Original MCP connection could not be opened: {error}"
        ));
    }
    if let Some(error) = &result.close_error {
        output::error(format_args!(
            "Original MCP connection close failed: {error}"
        ));
    }
    if let Some(probe) = &result.probe {
        print_probe_result(probe);
    }
    if let Some(verification) = &result.post_exit {
        if verification.succeeded() {
            output::line(format_args!(
                "Fresh CAT connection verified: selected USB endpoint and identity tuple match. Physical-unit continuity is not proved."
            ));
        } else {
            output::error(format_args!(
                "Post-exit verification did not complete: {}. Fully power-cycle the radio before another attempt.",
                verification.outcome
            ));
        }
    }
}

fn print_probe_result(report: &McpProbeReport) {
    match &report.outcome {
        McpProbeOutcome::Complete => output::line(format_args!(
            "MCP probe completed: two fragments captured; exit and unchanged CAT identity verified. This does not qualify a settings schema."
        )),
        McpProbeOutcome::AwaitingCatVerification => output::line(format_args!(
            "MCP fragments and exit ACK captured. Original handle retirement was attempted without post-exit CAT; fresh-connection evidence is reported separately."
        )),
        McpProbeOutcome::Cancelled => output::line(format_args!(
            "MCP probe cancelled at a complete exchange boundary."
        )),
        McpProbeOutcome::Failed { stage, error } => {
            output::error(format_args!("MCP probe failed at {stage:?}: {error}"));
        }
    }
    if matches!(
        report.exit,
        McpProbeExit::RecoveryRequired | McpProbeExit::NotAcknowledged
    ) || matches!(
        report.outcome,
        McpProbeOutcome::Failed {
            stage: McpProbeStage::CatVerification,
            ..
        }
    ) {
        output::error(format_args!(
            "Radio state is not verified. Stop using this connection and fully power-cycle the radio before reconnecting. No recovery commands will be sent."
        ));
    }
}

#[cfg(test)]
mod workflow_tests;

#[cfg(test)]
mod eligibility_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_tmd750::{Address, McpProbeSegment, Page};
    use tokio::sync::oneshot;

    type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

    #[test]
    fn parser_preserves_capture_path_and_rejects_write_surface() -> TestResult {
        let arguments = ["mcp", "probe", "--output", "Mixed Case/Capture One"].map(str::to_owned);
        let McpCommand::Probe(request) = parse(&arguments)? else {
            return Err("expected probe request".into());
        };
        assert_eq!(
            request.output,
            Some(PathBuf::from("Mixed Case/Capture One"))
        );
        for arguments in [
            vec!["mcp", "write"],
            vec!["mcp", "probe", "--address", "8"],
            vec!["mcp", "probe", "--force"],
        ] {
            assert!(
                parse(&arguments.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err(),
                "probe must expose neither writes nor arbitrary reads"
            );
        }
        Ok(())
    }

    #[test]
    fn fresh_verification_requires_an_explicit_endpoint() -> TestResult {
        let McpCommand::Probe(request) =
            parse(&["mcp", "probe", "--verify-reconnect"].map(str::to_owned))?
        else {
            return Err("expected probe request".into());
        };
        assert!(
            request.verify_reconnect,
            "fresh verification must be explicitly selected"
        );
        assert!(
            request.validate_endpoint_selection(false).is_err(),
            "fresh verification must not auto-select its endpoint"
        );
        request.validate_endpoint_selection(true)?;
        let McpCommand::Probe(default) = parse(&["mcp", "probe"].map(str::to_owned))? else {
            return Err("expected probe request".into());
        };
        assert!(
            !default.verify_reconnect,
            "default probes must never reconnect"
        );
        default.validate_endpoint_selection(false)?;
        Ok(())
    }

    #[test]
    fn backup_requires_explicit_endpoint_and_has_no_arbitrary_write_surface() -> TestResult {
        let request = parse(&["mcp", "backup"].map(str::to_owned))?;
        assert!(
            request.validate_endpoint_selection(false).is_err(),
            "backup reconnect must be explicitly pinned"
        );
        request.validate_endpoint_selection(true)?;
        for option in ["--address", "--force", "--write", "--startup-bitmap"] {
            assert!(
                parse(&["mcp", "backup", option].map(str::to_owned)).is_err(),
                "backup cannot expose {option}"
            );
        }
        Ok(())
    }

    #[test]
    fn offline_text_dispatch_requires_no_endpoint() -> TestResult {
        let request = parse(&["mcp", "text", "list"].map(str::to_owned))?;
        request.validate_endpoint_selection(false)?;
        run_offline(&request).ok_or("text must dispatch before discovery")??;
        assert!(
            run_offline(&parse(&["mcp", "backup"].map(str::to_owned))?).is_none(),
            "backup must stay on explicit hardware branch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interruption_finishes_the_in_flight_probe() -> TestResult {
        let cancelled = AtomicBool::new(false);
        let completed = AtomicBool::new(false);
        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let probe = async {
            entered_tx
                .send(())
                .map_err(|()| "probe marker receiver dropped")?;
            release_rx.await?;
            tokio::task::yield_now().await;
            assert!(
                cancelled.load(Ordering::Relaxed),
                "interruption must reach the boundary callback"
            );
            completed.store(true, Ordering::Relaxed);
            Ok::<_, Box<dyn StdError + Send + Sync>>(17)
        };
        let signal = async {
            entered_rx.await.map_err(io::Error::other)?;
            release_tx
                .send(())
                .map_err(|()| io::Error::other("probe was dropped"))?;
            Ok(())
        };
        let (result, signal_error) = finish_on_interrupt(probe, signal, &cancelled).await;
        assert_eq!(result?, 17);
        assert!(
            completed.load(Ordering::Relaxed),
            "probe must finish instead of being cancelled by drop"
        );
        assert!(
            signal_error.is_none(),
            "successful interruption must not invent a signal error"
        );
        Ok(())
    }

    #[tokio::test]
    async fn signal_registration_failure_still_finishes_probe() {
        let cancelled = AtomicBool::new(false);
        let completed = AtomicBool::new(false);
        let probe = async {
            assert!(
                cancelled.load(Ordering::Relaxed),
                "signal failure requests cooperative cancellation"
            );
            completed.store(true, Ordering::Relaxed);
        };
        let ((), error) = finish_on_interrupt(
            probe,
            std::future::ready(Err(io::Error::other("signal registration failed"))),
            &cancelled,
        )
        .await;
        assert!(error.is_some(), "signal failure must appear in the report");
        assert!(
            completed.load(Ordering::Relaxed),
            "cleanup must run despite signal failure"
        );
    }

    #[test]
    fn partial_evidence_serializes_without_inventing_image_bytes() -> TestResult {
        let report = McpProbeReport {
            identity: None,
            entry_reply: Some(b"0M".to_vec()),
            segments: vec![McpProbeSegment {
                page: Page::new(Address::new(8)?, 40)?,
                data: vec![0x42; 40],
            }],
            exit: McpProbeExit::RecoveryRequired,
            cat_identity: None,
            outcome: McpProbeOutcome::Failed {
                stage: McpProbeStage::SlotRead,
                error: kenwood_tmd750::transport::TransportError::Read(io::Error::other(
                    "USB disconnected mid-page",
                ))
                .into(),
            },
        };
        let json = serde_json::to_value(ProbeEvidence::from(&report))?;
        let segments = json
            .get("segments")
            .and_then(serde_json::Value::as_array)
            .ok_or("serialized segments missing")?;
        assert_eq!(segments.len(), 1);
        let segment = segments.first().ok_or("first segment missing")?;
        assert_eq!(segment.get("address"), Some(&serde_json::json!(8)));
        assert_eq!(segment.get("length"), Some(&serde_json::json!(40)));
        assert_eq!(
            segment.get("data"),
            Some(&serde_json::json!(vec![0x42; 40]))
        );
        assert_eq!(
            json.get("exit"),
            Some(&serde_json::json!("recovery_required"))
        );
        let outcome = json.get("outcome").ok_or("serialized outcome missing")?;
        assert_eq!(outcome.get("status"), Some(&serde_json::json!("failed")));
        assert_eq!(outcome.get("stage"), Some(&serde_json::json!("slot_read")));
        assert!(
            outcome.to_string().contains("USB disconnected mid-page"),
            "the original transport cause must survive serialization"
        );
        Ok(())
    }
}
