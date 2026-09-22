//! `dstar start` over Bluetooth: drive the library-owned Terminal lifecycle
//! and record its typed outputs.
//!
//! `kenwood_tmd750::TerminalLifecycle` owns the protocol; this coordinator
//! reserves the capture directory and report, wires the control endpoint,
//! Bluetooth modem link and recovery journal to the library host traits
//! (`super::hosts`), and serializes the run. `Recovery` holds the library
//! `TerminalRecovery` and restores over the control endpoint on shutdown.

pub(crate) mod journal;
#[cfg(test)]
pub(super) mod tests;

use std::fs::File;
use std::future::Future;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::radio::readiness::{ReadinessEnding, ReadinessReport};
use kenwood_tmd750::radio::terminal::lifecycle::{RestorationReport, RestorationState};
use kenwood_tmd750::transport::DEFAULT_BAUD;
use kenwood_tmd750::{
    Identity, ObservationReport, ProvenModem, TerminalGatewayRoute, TerminalLifecycle,
    TerminalRecovery,
};
use serde::Serialize;
use serde_json::Value;

use crate::capture::{Artifacts, CaptureKind, Failure};
use crate::connection::Connection;
use crate::{AppResult, CommandError, output};

use super::endpoints::Endpoints;
use super::hosts::{ControlHostImpl, ControlOpen, ModemHostImpl};
use journal::Journal;

/// The captured modem connection the proved link and the runtime own.
pub(super) type ModemConnection = crate::capture::CaptureTransport<Connection, File>;

/// The library-owned restoration owner plus the report it writes.
pub(super) struct Recovery {
    directory: PathBuf,
    report_file: File,
    cancelled: Arc<AtomicBool>,
    control: ControlHostImpl,
    journal: Journal,
    terminal: TerminalRecovery,
    bluetooth_address: String,
    control_path: String,
    modem_proved: bool,
    preflight: Option<ObservationSummary>,
    entry: Option<EntrySummary>,
    transition: TransitionSummary,
    modem_openings: Value,
    readiness: Vec<ReadinessSummary>,
    owner_released: Option<bool>,
    errors: Vec<Failure>,
}

/// Build a report failure from a message, without a source chain.
fn failure(message: impl Into<String>) -> Failure {
    Failure {
        message: message.into(),
        causes: Vec::new(),
    }
}

/// The identity and Gateway a control observation confirmed.
#[derive(Debug, Serialize)]
struct ObservationSummary {
    succeeded: bool,
    identity: Option<IdentitySummary>,
    gateway: Option<String>,
}

#[derive(Debug, Serialize)]
struct IdentitySummary {
    model: String,
    firmware: String,
    radio_type: String,
}

impl ObservationSummary {
    fn from_report(report: &ObservationReport) -> Self {
        let observed = report.observed();
        Self {
            succeeded: report.succeeded(),
            identity: observed.map(|(identity, _)| IdentitySummary::from(identity)),
            gateway: observed.map(|(_, gateway)| gateway.to_string()),
        }
    }
}

impl IdentitySummary {
    fn from(identity: &Identity) -> Self {
        Self {
            model: identity.model.to_string(),
            firmware: identity.firmware.as_str().to_owned(),
            radio_type: identity.radio_type.as_str().to_owned(),
        }
    }
}

/// Whether the Terminal programming pass began a write and acknowledged exit.
#[derive(Debug, Serialize)]
struct EntrySummary {
    write_started: bool,
    exit_acknowledged: bool,
}

/// The MMDVM acquisition attempts and whether one proved the framing.
#[derive(Debug, Default, Serialize)]
struct TransitionSummary {
    attempts: usize,
    version_proved: bool,
}

/// One readiness verification result during restoration.
#[derive(Debug, Serialize)]
struct ReadinessSummary {
    succeeded: bool,
    ending: &'static str,
}

impl ReadinessSummary {
    fn from_report(report: &ReadinessReport) -> Self {
        Self {
            succeeded: report.succeeded(),
            ending: readiness_ending(report.ending),
        }
    }
}

const fn readiness_ending(ending: ReadinessEnding) -> &'static str {
    match ending {
        ReadinessEnding::Matched => "matched",
        ReadinessEnding::Cancelled => "cancelled",
        ReadinessEnding::Failed => "failed",
        ReadinessEnding::AttemptCapExhausted => "attempt_cap_exhausted",
        ReadinessEnding::BudgetExhausted => "budget_exhausted",
    }
}

const fn restoration_label(state: RestorationState) -> &'static str {
    match state {
        RestorationState::NotRequired => "not_required",
        RestorationState::Owed => "owed",
        RestorationState::Verified => "verified",
        RestorationState::Blocked => "blocked",
    }
}

/// The serialized view of one startup, borrowing live control records.
#[derive(Serialize)]
struct ReportView<'a> {
    format_version: u8,
    operation: &'static str,
    identity_assurance: &'static str,
    bluetooth_address: &'a str,
    control_path: &'a str,
    modem_proved: bool,
    restoration: &'static str,
    owner_released: Option<bool>,
    preflight: &'a Option<ObservationSummary>,
    entry: &'a Option<EntrySummary>,
    transition: &'a TransitionSummary,
    readiness: &'a [ReadinessSummary],
    control_opens: &'a [ControlOpen],
    modem_openings: &'a Value,
    errors: &'a [Failure],
}

impl Recovery {
    fn view(&self) -> ReportView<'_> {
        ReportView {
            format_version: 2,
            operation: "dstar_start",
            identity_assurance: "exact_endpoint_and_cat_tuple_only",
            bluetooth_address: &self.bluetooth_address,
            control_path: &self.control_path,
            modem_proved: self.modem_proved,
            restoration: restoration_label(self.terminal.state()),
            owner_released: self.owner_released,
            preflight: &self.preflight,
            entry: &self.entry,
            transition: &self.transition,
            readiness: &self.readiness,
            control_opens: self.control.records(),
            modem_openings: &self.modem_openings,
            errors: &self.errors,
        }
    }

    fn write_report(&mut self) -> AppResult<()> {
        let mut bytes = serde_json::to_vec_pretty(&self.view())?;
        bytes.push(b'\n');
        let _position = self.report_file.seek(SeekFrom::Start(0))?;
        self.report_file.set_len(0)?;
        self.report_file.write_all(&bytes)?;
        self.report_file.sync_all()?;
        Ok(())
    }

    fn publish(&mut self) -> AppResult<()> {
        let journal = self.journal.synchronize();
        if let Err(error) = &journal {
            self.errors.push(Failure::from_error(error));
        }
        let report = self.write_report();
        match (journal, report) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Err(error)) => Err(error),
            (Err(journal), Err(report)) => Err(CommandError(format!(
                "journal synchronization: {journal}; report publication: {report}"
            ))
            .into()),
        }
    }

    /// Record a runtime initialization, link, or shutdown failure.
    pub(super) fn record_runtime_failure(&mut self, message: &str) {
        self.errors.push(failure(message));
    }

    /// Restore the original Gateway settings over the control endpoint.
    ///
    /// `released` must be true only when the modem connection was closed and
    /// dropped. It runs the library restoration, records its outcome, and
    /// publishes the report.
    pub(super) async fn finish(mut self, released: bool) -> Result<(), String> {
        let report = self
            .terminal
            .finish(
                &mut self.control,
                &mut self.journal,
                released,
                &self.cancelled,
            )
            .await;
        self.absorb_restoration(released, &report);
        let outcome = restoration_outcome(&report);
        let published = self.publish();
        let directory = self.directory.display();
        match (outcome, published) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(format!("{error}; recovery capture: {directory}")),
            (Ok(()), Err(error)) => Err(format!("{error}; recovery capture: {directory}")),
            (Err(error), Err(capture)) => Err(format!(
                "{error}; report publication: {capture}; recovery capture: {directory}"
            )),
        }
    }

    fn absorb_restoration(&mut self, released: bool, report: &RestorationReport) {
        self.owner_released = Some(released && self.control.released());
        for readiness in [&report.readiness_before, &report.readiness_after]
            .into_iter()
            .flatten()
        {
            self.readiness
                .push(ReadinessSummary::from_report(readiness));
        }
        if let Some(verification) = &report.verification {
            self.preflight = Some(ObservationSummary::from_report(verification));
        }
        // The library owns the state; mirror any residual failure into errors.
        if let Some(error) = &report.error {
            self.errors.push(failure(error.to_string()));
        }
    }
}

fn restoration_outcome(report: &RestorationReport) -> Result<(), String> {
    match report.state {
        RestorationState::Verified | RestorationState::NotRequired => Ok(()),
        RestorationState::Owed => Err(report.error.map_or_else(
            || "Terminal restoration is still owed".to_owned(),
            |error| error.to_string(),
        )),
        RestorationState::Blocked => Err(report.error.map_or_else(
            || "Terminal restoration is blocked".to_owned(),
            |error| error.to_string(),
        )),
    }
}

/// Open both endpoints, write the Terminal pages, and acquire MMDVM framing.
///
/// Returns the proved modem connection and the `Recovery` that restores the
/// original settings. On failure the recovery is finished before returning the
/// error, so any owed restoration runs.
pub(super) async fn prepare(
    bluetooth: crate::native::discovery::Request,
    control_port: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<(ProvenModem<ModemConnection>, Recovery), String> {
    let endpoints = super::endpoints::resolve(&bluetooth, control_port, &cancelled)
        .await
        .map_err(|error| Failure::from_error(error.as_ref()).to_string())?;
    let (directory, report_file) =
        reserve(&endpoints, Arc::clone(&cancelled)).map_err(|error| error.to_string())?;
    prepare_in(endpoints, directory, report_file, cancelled).await
}

async fn prepare_in(
    endpoints: Endpoints,
    directory: PathBuf,
    report_file: File,
    cancelled: Arc<AtomicBool>,
) -> Result<(ProvenModem<ModemConnection>, Recovery), String> {
    let mut control = ControlHostImpl::new(&directory, Arc::clone(&cancelled));
    let mut modem = ModemHostImpl::new(
        endpoints.bluetooth.clone(),
        &directory,
        Arc::clone(&cancelled),
    );
    let mut journal = match Journal::create(&directory, Arc::clone(&cancelled)) {
        Ok(journal) => journal,
        Err(error) => return Err(error.to_string()),
    };
    output::line(format_args!(
        "Preparing Reflector Terminal Mode on Bluetooth; preserving the original settings for shutdown."
    ));
    let lifecycle = TerminalLifecycle::new(
        endpoints.control.clone(),
        DEFAULT_BAUD,
        TerminalGatewayRoute::Bluetooth,
    );
    let startup = lifecycle
        .prepare(&mut control, &mut modem, &mut journal, &cancelled)
        .await;
    let modem_openings = serde_json::to_value(modem.openings()).unwrap_or(Value::Null);
    let mut recovery = Recovery {
        directory,
        report_file,
        cancelled: Arc::clone(&cancelled),
        control,
        journal,
        terminal: startup.recovery,
        bluetooth_address: endpoints.bluetooth.address.to_string(),
        control_path: endpoints.control.path.clone(),
        modem_proved: startup.proof.is_some(),
        preflight: startup
            .preflight
            .as_ref()
            .map(ObservationSummary::from_report),
        entry: startup.entry.as_ref().map(|entry| EntrySummary {
            write_started: entry.write_started,
            exit_acknowledged: entry.exit_acknowledged,
        }),
        transition: TransitionSummary {
            attempts: startup.transition_attempts.len(),
            version_proved: startup
                .transition_attempts
                .iter()
                .any(|attempt| attempt.version_proved),
        },
        modem_openings,
        readiness: Vec::new(),
        owner_released: None,
        errors: startup
            .error
            .iter()
            .map(|error| failure(error.to_string()))
            .collect(),
    };
    if let Some(error) = &startup.cleanup_error {
        recovery.errors.push(failure(error.to_string()));
    }
    match startup.proof {
        Some(proof) if !cancelled.load(Ordering::Relaxed) => {
            if let Err(error) = recovery.publish() {
                let released = recovery.control.released();
                return Err(finish_after_error(recovery, released, error.to_string()).await);
            }
            Ok((proof, recovery))
        }
        Some(proof) => {
            // Cancelled after the modem answered: release it and restore.
            let message = "D-STAR preparation cancelled after the modem answered".to_owned();
            recovery.errors.push(failure(message.clone()));
            let released = crate::native::close(&mut proof.into_transport())
                .await
                .is_none()
                && recovery.control.released();
            Err(finish_after_error(recovery, released, message).await)
        }
        None => {
            let message = recovery.errors.last().map_or_else(
                || "Bluetooth Terminal startup did not complete".to_owned(),
                |failure| failure.message.clone(),
            );
            let released = recovery.control.released();
            Err(finish_after_error(recovery, released, message).await)
        }
    }
}

async fn finish_after_error(recovery: Recovery, released: bool, message: String) -> String {
    match recovery.finish(released).await {
        Ok(()) => message,
        Err(cleanup) => format!("{message}; {cleanup}"),
    }
}

fn reserve(endpoints: &Endpoints, cancelled: Arc<AtomicBool>) -> AppResult<(PathBuf, File)> {
    reserve_at(endpoints, cancelled, None, synchronize_directory)
}

/// Create the capture directory and report file, syncing their parent entries.
///
/// `synchronize` makes the new directory entries durable: production passes
/// `synchronize_directory`; offline fixtures pass a stub that skips only that
/// step, leaving file I/O real.
fn reserve_at(
    _endpoints: &Endpoints,
    cancelled: Arc<AtomicBool>,
    requested: Option<&Path>,
    synchronize: impl FnMut(&Path) -> io::Result<()>,
) -> AppResult<(PathBuf, File)> {
    let Artifacts {
        directory,
        report,
        transcript,
    } = Artifacts::create(CaptureKind::DstarStart, requested, cancelled)?;
    // The lifecycle hosts create their own per-stage transcripts.
    drop(transcript);
    synchronize_directories(&directory, synchronize)?;
    output::line(format_args!(
        "D-STAR startup and recovery capture: {}.",
        directory.display()
    ));
    Ok((directory, report))
}

#[cfg(unix)]
fn synchronize_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn synchronize_directory(_directory: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "automatic Terminal startup requires Unix directory synchronization",
    ))
}

fn synchronize_directories(
    directory: &Path,
    mut synchronize: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    synchronize(directory)?;
    synchronize(
        directory
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )
}

/// Await `operation` to completion, setting `cancelled` when `signal` fires.
///
/// `operation` is never dropped, so it closes any connection it opened; its
/// output is returned unchanged. A signal-listener error is returned beside
/// that output rather than replacing it.
pub(super) async fn finish_on_interrupt<F, S>(
    operation: F,
    signal: S,
    cancelled: &AtomicBool,
) -> (F::Output, Option<Failure>)
where
    F: Future,
    S: Future<Output = io::Result<()>>,
{
    tokio::pin!(operation);
    let (result, interrupt) = tokio::select! {
        biased;
        signal = signal => {
            cancelled.store(true, Ordering::Relaxed);
            output::line(format_args!("Stopping startup; finishing the current exchange, then closing the connection."));
            (operation.await, Some(signal))
        }
        result = &mut operation => (result, None),
    };
    let failure = interrupt
        .and_then(Result::err)
        .as_ref()
        .map(|error| Failure::from_error(error));
    (result, failure)
}
