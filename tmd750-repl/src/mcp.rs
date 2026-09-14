//! Startup-only MCP workflows and offline configuration inspection.

pub(crate) mod backup;
pub(crate) mod fixed;
mod menu;
mod menu_apply;
mod pm1_trial;
pub(crate) mod reconnect;
mod reconnect_policy;
mod reentry_probe;
pub(crate) mod snapshot;
mod terminal;
mod terminal_exit_trial;
mod text;
mod text_set;

use std::fs::File;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::{Parser, Subcommand};
use kenwood_tmd750::transport::SerialCandidate;
use kenwood_tmd750::{Identity, McpProbeExit, McpProbeOutcome, McpProbeReport, McpProbeStage};
use kenwood_transport::Transport;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::capture::{
    Artifacts, CaptureKind, CaptureTransport, Failure, Recorder, TranscriptSummary,
};
use crate::{AppResult, output};
use reconnect::{Backend, ReadinessVerification, SkipReason, SystemBackend};

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
    /// Run one approved pair of read-only MCP sessions with Gateway Off.
    ReentryProbe(reentry_probe::Request),
    /// Back up every standard configuration region, then verify fresh CAT.
    Backup(backup::BackupRequest),
    /// Discover, inspect, preview, or explicitly apply registered menu fields.
    Menu(menu::MenuRequest),
    /// Inspect text offline or explicitly update PM1's name or PM-Off MY1.
    Text(text::TextRequest),
    /// Inspect captured Terminal settings offline; never activates the gateway.
    Terminal(terminal::TerminalRequest),
    /// Run the separately approved, fixed PM1 rename-and-restore experiment.
    Pm1Trial(pm1_trial::TrialRequest),
    /// Run the separately approved PM Off MY1 KQ4NIT-and-restore experiment.
    My1Trial(pm1_trial::My1TrialRequest),
    /// Run one separately approved, guarded Terminal-to-Off experiment.
    TerminalExitTrial(terminal_exit_trial::Request),
}

impl McpCommand {
    /// Require a pinned endpoint for every workflow that reconnects.
    pub(crate) fn validate_endpoint_selection(&self, explicit_port: bool) -> AppResult<()> {
        match self {
            Self::Text(request) => request.validate_endpoint_selection(explicit_port),
            Self::Menu(request) => request.validate_endpoint_selection(explicit_port),
            Self::Terminal(_) => Ok(()),
            Self::Probe(_)
            | Self::Backup(_)
            | Self::ReentryProbe(_)
            | Self::Pm1Trial(_)
            | Self::My1Trial(_)
            | Self::TerminalExitTrial(_)
                if explicit_port =>
            {
                Ok(())
            }
            Self::Probe(_) => Err(Box::new(crate::CommandError(
                "mcp probe requires an explicit --port before mcp; USB paths are not physical radio identities"
                    .to_owned(),
            ))),
            Self::Backup(_) => Err(Box::new(crate::CommandError(
                "mcp backup requires an explicit --port before mcp".to_owned(),
            ))),
            Self::ReentryProbe(_) => Err(Box::new(crate::CommandError(
                "mcp reentry-probe requires an explicit --port before mcp".to_owned(),
            ))),
            Self::Pm1Trial(_) => Err(Box::new(crate::CommandError(
                "mcp pm1-trial requires an explicit --port before mcp".to_owned(),
            ))),
            Self::My1Trial(_) => Err(Box::new(crate::CommandError(
                "mcp my1-trial requires an explicit --port before mcp".to_owned(),
            ))),
            Self::TerminalExitTrial(_) => Err(Box::new(crate::CommandError(
                "mcp terminal-exit-trial requires an explicit --port before mcp".to_owned(),
            ))),
        }
    }
}

/// Execute commands that must not enumerate or open radio endpoints.
pub(crate) fn run_offline(request: &McpCommand) -> Option<AppResult<()>> {
    match request {
        McpCommand::Text(request) => text::run_offline(request),
        McpCommand::Menu(request) => menu::run_offline(request),
        McpCommand::Terminal(request) => Some(terminal::run(request)),
        McpCommand::Probe(_)
        | McpCommand::ReentryProbe(_)
        | McpCommand::Backup(_)
        | McpCommand::Pm1Trial(_)
        | McpCommand::My1Trial(_)
        | McpCommand::TerminalExitTrial(_) => None,
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
}

impl ProbeRequest {
    /// Selected output location; transport policy does not change its meaning.
    pub(crate) fn output(&self) -> Option<&std::path::Path> {
        self.output.as_deref()
    }
}

/// Parse original OS arguments without lowercasing paths or splitting spaces.
pub(crate) fn parse(arguments: &[String]) -> Result<McpCommand, clap::Error> {
    Ok(McpCli::try_parse_from(arguments)?.command)
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
    post_exit_verification: ReadinessVerification,
}

#[derive(Debug, Serialize)]
pub(crate) struct ProbeEvidence {
    identity: Option<IdentityEvidence>,
    entry_reply: Option<Vec<u8>>,
    segments: Vec<SegmentEvidence>,
    exit: ExitDisposition,
    outcome: Outcome,
}

impl ProbeEvidence {
    /// Original protocol failure, independent of capture and owner cleanup.
    pub(crate) const fn error(&self) -> Option<&Failure> {
        match &self.outcome {
            Outcome::Failed { error, .. } => Some(error),
            Outcome::AwaitingCatVerification | Outcome::Cancelled => None,
        }
    }

    pub(crate) const fn completed_with_acknowledged_exit(&self) -> bool {
        self.identity.is_some()
            && matches!(self.outcome, Outcome::AwaitingCatVerification)
            && matches!(self.exit, ExitDisposition::Acknowledged)
    }
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
    Gateway,
    Entry,
    GlobalRead,
    SlotRead,
    Exit,
}

impl From<McpProbeStage> for Stage {
    fn from(stage: McpProbeStage) -> Self {
        match stage {
            McpProbeStage::Identity => Self::Identity,
            McpProbeStage::Gateway => Self::Gateway,
            McpProbeStage::Entry => Self::Entry,
            McpProbeStage::GlobalRead => Self::GlobalRead,
            McpProbeStage::SlotRead => Self::SlotRead,
            McpProbeStage::Exit => Self::Exit,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Outcome {
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
            outcome: match &report.outcome {
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
        McpCommand::ReentryProbe(request) => reentry_probe::run(endpoint, baud, request).await,
        McpCommand::Backup(request) => backup::run(endpoint, baud, request).await,
        McpCommand::Text(request) => text::run_selected(endpoint, baud, request).await,
        McpCommand::Menu(request) => menu::run_selected(endpoint, baud, request).await,
        McpCommand::Terminal(request) => terminal::run(request),
        McpCommand::Pm1Trial(request) => pm1_trial::run(endpoint, baud, request).await,
        McpCommand::My1Trial(request) => pm1_trial::run_my1(endpoint, baud, request).await,
        McpCommand::TerminalExitTrial(request) => {
            terminal_exit_trial::run(endpoint, baud, request).await
        }
    }
}

async fn run_probe(endpoint: &SerialCandidate, baud: u32, request: &ProbeRequest) -> AppResult<()> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(
        CaptureKind::Mcp,
        request.output.as_deref(),
        Arc::clone(&cancelled),
    )
    .map_err(|source| ProbeError::Capture {
        path: request
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from("captures")),
        source,
    })?;
    let post_exit = artifacts
        .reserve_post_exit(Arc::clone(&cancelled))
        .map_err(|source| ProbeError::Capture {
            path: artifacts.directory.clone(),
            source,
        })?;
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
        format_version: 3,
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
            && match &self.probe {
                Some(probe) => {
                    matches!(probe.outcome, Outcome::AwaitingCatVerification)
                        && matches!(probe.exit, ExitDisposition::Acknowledged)
                        && self.post_exit_verification.succeeded()
                }
                None => false,
            }
    }
}

#[derive(Debug)]
struct WorkflowCaptures {
    original: Recorder<File>,
    post_exit: Recorder<File>,
}

#[derive(Debug)]
struct WorkflowResult {
    probe: Option<McpProbeReport>,
    transcript: TranscriptSummary,
    open_error: Option<Failure>,
    close_error: Option<Failure>,
    post_exit: ReadinessVerification,
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
        post_exit: ReadinessVerification::skipped(
            SkipReason::OriginalOpenFailed,
            post_exit.summary(),
        ),
    };
    if cancelled.load(Ordering::Relaxed) {
        result.probe = Some(McpProbeReport {
            identity: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            outcome: McpProbeOutcome::Cancelled,
        });
        result.post_exit =
            ReadinessVerification::skipped(SkipReason::Cancelled, post_exit.summary());
        return result;
    }
    let transport = match backend.open(endpoint, baud) {
        Ok(transport) => transport,
        Err(error) => {
            result.open_error = Some(Failure::from_error(&error));
            return result;
        }
    };
    let observed = fixed::observe(
        CaptureTransport::required(transport, original),
        fixed::Admission::FixedIdentity,
        cancelled,
    )
    .await;
    result.close_error = observed.close_error;
    result.transcript = observed.transcript;
    let Some(probe) = observed.probe else {
        return result;
    };
    result.post_exit = match verification_eligibility(
        &probe,
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

/// Host cleanup bound, also reserved by read-only MCP readiness checks.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

async fn close_transport(transport: &mut impl Transport) -> Option<Failure> {
    match tokio::time::timeout(CLOSE_TIMEOUT, transport.close()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(Failure::from_error(&error)),
        Err(error) => Some(Failure::from_error(&error)),
    }
}

/// Only the signal wait is disposable; the same probe is awaited on both paths.
pub(crate) async fn finish_on_interrupt<F, S>(
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
    if result.post_exit.succeeded() {
        output::line(format_args!(
            "Fresh CAT connection verified: selected USB endpoint and identity tuple match. Physical-unit continuity is not proved."
        ));
    } else {
        output::error(format_args!(
            "Post-exit verification did not complete: {}. No further radio commands will be sent; inspect the report before reconnecting.",
            result.post_exit.outcome
        ));
    }
}

fn print_probe_result(report: &McpProbeReport) {
    match &report.outcome {
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
    use std::error::Error as StdError;
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
        let request = parse(&["mcp", "probe"].map(str::to_owned))?;
        assert!(
            request.validate_endpoint_selection(false).is_err(),
            "every probe requires explicit endpoint selection before reconnecting"
        );
        request.validate_endpoint_selection(true)?;
        assert!(
            parse(&["mcp", "probe", "--verify-reconnect"].map(str::to_owned)).is_err(),
            "obsolete optional verification must not leave a compatibility path"
        );
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
            outcome: McpProbeOutcome::Failed {
                stage: McpProbeStage::SlotRead,
                error: kenwood_transport::TransportError::Read(io::Error::other(
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
