//! Captured diagnostic queries with optional separately controlled mode handling.
//!
//! A normal CAT reply stops protocol fallback. Only a completely silent initial
//! ID timeout after its completed write admits binary queries on that same
//! handle. This workflow never instantiates a modem runtime or opens a network
//! connection. Current routing must be established before an approved live run.
//! Managed operation retains a distinct control interface for guarded entry and
//! exact restoration; the diagnostic connection never sends an escape command.

use std::fs::File;
use std::future::Future;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate, SerialTransport, open_serial};
use kenwood_tmd750::{Error as RadioError, Radio};
use kenwood_transport::{Transport, TransportError};
use mmdvm::probe::{ProbeError, probe_diagnostics_until};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::capture::{Artifacts, CaptureKind, CaptureTransport, Event, Failure, Recorder};
use crate::{AppResult, CommandError, output};

mod managed;
mod report;
#[cfg(test)]
mod tests;

use report::{Endpoint, Outcome, Report, Stage, WorkflowResult};

/// Diagnostic dispatch bounds, not firmware readiness guarantees.
#[derive(Clone, Copy, Debug, Serialize)]
struct Limits {
    cat_io_step: Duration,
    binary_total: Duration,
    close: Duration,
}

impl Limits {
    const DEFAULT: Self = Self {
        cat_io_step: Duration::from_millis(1_500),
        binary_total: Duration::from_secs(4),
        close: Duration::from_secs(2),
    };
}

/// Arguments parsed before any endpoint enumeration or connection opening.
#[derive(Debug, Parser)]
#[command(name = "dstar probe", color = clap::ColorChoice::Never)]
#[command(about = "Capture CAT or bounded MMDVM version/status queries; no modem setup")]
pub(crate) struct Request {
    /// New private capture directory; its parent must exist. Never overwrite.
    #[arg(long, value_name = "NEW_DIRECTORY")]
    output: Option<PathBuf>,

    /// Confirm the selected radio route and explicitly scoped live test.
    ///
    /// Without --manage-terminal this authorizes no mode changes. Host bytes
    /// on another DV route may transmit RF; establish routing before this probe.
    #[arg(long)]
    approve_live_test: bool,

    /// Enter Reflector Terminal for this diagnostic, then restore prior settings.
    ///
    /// Requires a complete backup and an independent USB control interface.
    /// Routing, active PM, callsigns, and repeater settings are never changed.
    #[arg(long, requires_all = ["control_port", "backup"])]
    manage_terminal: bool,

    /// Exact enumerated USB control endpoint, distinct from the modem --port.
    #[arg(long, value_name = "CONTROL_PORT", requires = "manage_terminal")]
    control_port: Option<String>,

    /// Complete successful configuration backup used for fresh page comparisons.
    #[arg(long, value_name = "REPORT_JSON", requires = "manage_terminal")]
    backup: Option<PathBuf>,
}

impl Request {
    /// Reject missing authority, implicit endpoints, and unqualified baud rates.
    pub(crate) fn validate<'a>(
        &self,
        path: Option<&'a str>,
        baud: u32,
    ) -> Result<&'a str, CommandError> {
        if !self.approve_live_test {
            return Err(CommandError(
                "dstar probe requires --approve-live-test after establishing the selected radio route; only --manage-terminal permits guarded mode changes".to_owned(),
            ));
        }
        if baud != DEFAULT_BAUD {
            return Err(CommandError(format!(
                "dstar probe requires the qualified CAT rate {DEFAULT_BAUD}; no baud fallback is attempted"
            )));
        }
        let path = path.filter(|path| !path.is_empty()).ok_or_else(|| {
            CommandError("dstar probe requires an explicit --port before dstar".to_owned())
        })?;
        if self.manage_terminal
            && (self
                .control_port
                .as_deref()
                .is_none_or(|control| control.is_empty() || control == path)
                || self
                    .backup
                    .as_deref()
                    .is_none_or(|backup| backup.as_os_str().is_empty()))
        {
            return Err(CommandError(
                "--manage-terminal requires a distinct nonempty --control-port and a nonempty --backup path".to_owned(),
            ));
        }
        Ok(path)
    }
}

/// Recognize this startup command without changing path or argument case.
pub(crate) fn matches_arguments(arguments: &[String]) -> bool {
    arguments.first().is_some_and(|word| {
        word.eq_ignore_ascii_case("dstar") || word.eq_ignore_ascii_case("d-star")
    }) && arguments
        .get(1)
        .is_some_and(|word| word.eq_ignore_ascii_case("probe"))
}

/// Preserve the original argument boundaries, including paths containing spaces.
pub(crate) fn parse(arguments: &[String]) -> Result<Request, clap::Error> {
    Request::try_parse_from(
        std::iter::once("dstar probe").chain(arguments.iter().skip(2).map(String::as_str)),
    )
}

/// Select only the exact enumerated path; aliases and conflicting metadata fail.
pub(crate) fn select_endpoint(
    path: &str,
    candidates: Vec<SerialCandidate>,
) -> Result<SerialCandidate, CommandError> {
    let mut matches = candidates
        .into_iter()
        .filter(|candidate| candidate.path == path);
    let selected = matches.next().filter(SerialCandidate::is_tmd750);
    match selected {
        Some(endpoint) if matches.next().is_none() => Ok(endpoint),
        _ => Err(CommandError(format!(
            "dstar probe requires one unambiguous enumerated TM-D750 USB endpoint at {path}; no substitute port was selected"
        ))),
    }
}

/// Open only the selected endpoint; mock implementations pin every operation.
trait Backend {
    type Connection: Transport;

    fn open(&mut self, endpoint: &SerialCandidate) -> Result<Self::Connection, TransportError>;
}

/// Report publication is independent of the preceding radio observations.
#[derive(Debug, thiserror::Error)]
#[error("diagnostic report publication failed at {path}: {source}")]
struct PublicationError {
    path: PathBuf,
    source: io::Error,
}

struct SystemBackend;

impl Backend for SystemBackend {
    type Connection = SerialTransport;

    fn open(&mut self, endpoint: &SerialCandidate) -> Result<SerialTransport, TransportError> {
        open_serial(&endpoint.path, DEFAULT_BAUD)
    }
}

/// Run the explicitly selected diagnostic lifecycle and finish durable reporting.
pub(crate) async fn run(endpoint: &SerialCandidate, request: &Request) -> AppResult<()> {
    let _path = request.validate(Some(&endpoint.path), DEFAULT_BAUD)?;
    if request.manage_terminal {
        return managed::run(endpoint, request).await;
    }
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
    output::line(format_args!(
        "D-STAR diagnostic capture: {}.",
        directory.display()
    ));
    output::line(format_args!(
        "Diagnostic queries only. Ctrl-C finishes the current bounded phase, then closes."
    ));
    let started_at_utc = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let (result, signal_error) = finish_on_interrupt(
        run_workflow(
            &mut SystemBackend,
            endpoint,
            transcript,
            &cancelled,
            Limits::DEFAULT,
        ),
        tokio::signal::ctrl_c(),
        &cancelled,
    )
    .await;
    let report = Report {
        format_version: 1,
        operation: "dstar_probe",
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc,
        finished_at_utc: OffsetDateTime::now_utc().format(&Rfc3339)?,
        endpoint: Endpoint::from(endpoint),
        limits: Limits::DEFAULT,
        result,
        signal_error,
        cancelled: cancelled.load(Ordering::Relaxed),
    };
    let path = directory.join("report.json");
    publish_report(&mut report_file, &report).map_err(|source| PublicationError {
        path: path.clone(),
        source,
    })?;
    report.print();
    output::line(format_args!(
        "D-STAR diagnostic report: {}.",
        path.display()
    ));
    if report.succeeded() {
        Ok(())
    } else {
        Err(Box::new(CommandError(format!(
            "diagnostic incomplete; retain {} and its transcript",
            path.display()
        ))))
    }
}

/// Synchronize the report after connection release; failed publication is failure.
fn publish_report(file: &mut File, report: &Report) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *file, report)?;
    file.write_all(b"\n")?;
    file.flush()?;
    file.sync_all()
}

/// Preserve the owner future when a signal arrives; only the signal is disposable.
async fn finish_on_interrupt<F, S>(
    workflow: F,
    signal: S,
    cancelled: &AtomicBool,
) -> (F::Output, Option<Failure>)
where
    F: Future,
    S: Future<Output = io::Result<()>>,
{
    tokio::pin!(workflow);
    tokio::pin!(signal);
    tokio::select! {
        biased;
        result = &mut signal => {
            cancelled.store(true, Ordering::Relaxed);
            output::line(format_args!(
                "Stopping diagnostic; finishing the current bounded phase before closing."
            ));
            (workflow.await, result.err().as_ref().map(|error| Failure::from_error(error)))
        }
        result = &mut workflow => (result, None),
    }
}

/// Keep every failure independent of protocol evidence and always retire the handle.
async fn run_workflow(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
    limits: Limits,
) -> WorkflowResult {
    if cancelled.load(Ordering::Relaxed) {
        return finish_capture(recorder, Outcome::cancelled(), None);
    }
    recorder.record(Event::OpenRequested {
        path: &endpoint.path,
        baud: DEFAULT_BAUD,
    });
    if let Err(error) = recorder.synchronize() {
        return finish_capture(recorder, Outcome::failed(Stage::Capture, &error), None);
    }
    let connection = match backend.open(endpoint) {
        Ok(connection) => connection,
        Err(error) => {
            recorder.record(Event::OpenFailed {
                error: Failure::from_error(&error),
            });
            return finish_capture(recorder, Outcome::failed(Stage::Open, &error), None);
        }
    };
    recorder.record(Event::OpenCompleted);
    let mut transport = CaptureTransport::required(connection, recorder);
    let outcome = match transport.synchronize() {
        Ok(()) => observe(&mut transport, cancelled, limits).await,
        Err(error) => Outcome::failed(Stage::Capture, &error),
    };
    let close_error = match tokio::time::timeout(limits.close, transport.close()).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(Failure::from_error(&error)),
        Err(error) => Some(Failure::from_error(&error)),
    };
    // Consuming the wrapper drops its connection before evidence is accepted.
    finish_capture(transport.into_recorder(), outcome, close_error)
}

fn finish_capture(
    mut recorder: Recorder<File>,
    outcome: Outcome,
    close_error: Option<Failure>,
) -> WorkflowResult {
    let synchronization_error = recorder
        .synchronize()
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    WorkflowResult {
        outcome,
        close_error,
        synchronization_error,
        transcript: recorder.summary(),
    }
}

/// Borrow only this connection for CAT admission and optional binary observations.
async fn observe<T: Transport>(
    transport: &mut CaptureTransport<T, File>,
    cancelled: &AtomicBool,
    limits: Limits,
) -> Outcome {
    if cancelled.load(Ordering::Relaxed) {
        return Outcome::cancelled();
    }
    let mut radio = Radio::new(Borrowed(transport));
    radio.set_timeout(limits.cat_io_step);
    let identity = radio.identify().await;
    if let Ok(identity) = identity {
        if cancelled.load(Ordering::Relaxed) {
            return Outcome::Cancelled {
                identity: Some((&identity).into()),
                version: None,
            };
        }
        return match radio.get_dv_gateway_mode().await {
            Ok(gateway) => Outcome::CatObserved {
                identity: (&identity).into(),
                gateway: gateway.into(),
            },
            Err(error) => Outcome::CatGatewayFailed {
                identity: (&identity).into(),
                error: Failure::from_error(&error),
            },
        };
    }
    let Err(error) = identity else {
        unreachable!("successful identity returned above")
    };
    let transport = radio.into_transport().0;
    if !matches!(
        &error,
        RadioError::Timeout {
            operation: "ID",
            ..
        }
    ) || !transport.activity().silent_first_exchange()
    {
        return Outcome::failed(Stage::CatIdentity, &error);
    }
    let cat_silence = Failure::from_error(&error);
    if let Err(error) = transport.synchronize() {
        return Outcome::failed(Stage::Capture, &error);
    }
    match probe_diagnostics_until(transport, limits.binary_total, || {
        cancelled.load(Ordering::Relaxed)
    })
    .await
    {
        Ok(response) => {
            let version = (&response.version).into();
            match response.status {
                Ok(status) => Outcome::MmdvmObserved {
                    cat_silence,
                    version,
                    status: status.into(),
                },
                Err(ProbeError::Cancelled) => Outcome::Cancelled {
                    identity: None,
                    version: Some(version),
                },
                Err(error) => Outcome::StatusFailed {
                    cat_silence,
                    version,
                    error: Failure::from_error(&error),
                },
            }
        }
        Err(ProbeError::Cancelled) => Outcome::cancelled(),
        Err(error) => Outcome::VersionFailed {
            cat_silence,
            error: Failure::from_error(&error),
        },
    }
}

/// Lend the captured connection to CAT without transferring its cleanup owner.
struct Borrowed<'a, T>(&'a mut T);

impl<T: Transport> Transport for Borrowed<'_, T> {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.0.write(bytes).await
    }
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.0.read(bytes).await
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.0.close().await
    }
}
