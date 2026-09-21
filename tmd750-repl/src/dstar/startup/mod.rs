//! Automatic Bluetooth Terminal startup, with restoration over USB.

mod control;

mod journal;
mod program;
mod reopen;
#[cfg(test)]
pub(super) mod tests;

use std::fs::File;
use std::future::Future;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_tmd750::radio::terminal::TerminalPlan;
use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate};
use kenwood_tmd750::{DvGatewayMode, Identity};
use kenwood_transport::Transport;
use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use serde::Serialize;

use crate::capture::{
    Artifacts, CaptureKind, CaptureTransport, Failure, Recorder, create_private_file,
};
use crate::connection::Connection;
use crate::mcp::reconnect::{self, Backend as UsbBackend};
use crate::native::{self, opening};
use crate::{AppResult, CommandError, output};

use super::endpoints::Endpoints;
use super::modem::ProvenModem;
use super::transition;
use journal::Journal;

pub(super) type ModemConnection = CaptureTransport<Connection, File>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RestorationState {
    NotRequired,
    Owed,
    Verified,
    Blocked,
}

#[derive(Debug, Serialize)]
struct Report {
    format_version: u8,
    operation: &'static str,
    identity_assurance: &'static str,
    bluetooth_address: String,
    control_path: String,
    modem_proved: bool,
    restoration: RestorationState,
    original_opening: Option<opening::History>,
    reopens: Vec<opening::History>,
    transition: Vec<transition::Attempt>,
    transition_cleanup: Option<Failure>,
    control: Vec<native::cat::Observation>,
    control_admission_errors: Vec<Failure>,
    retirements: Vec<control::Retirement>,
    readiness: Vec<reconnect::ReadinessVerification>,
    restore_errors: Vec<Failure>,
    runtime_capture_error: Option<Failure>,
    /// Whether every command connection was closed with a complete transcript.
    /// `None` until the startup workflow finishes. Covers host handles only;
    /// the RFCOMM link state is not observed.
    owner_released: Option<bool>,
    entry_plan_retained: bool,
    entry_exit_acknowledged: bool,
    errors: Vec<Failure>,
}

/// The captured page images and open capture files used to restore the radio.
///
/// It outlives the modem session, so restoration is still possible after a
/// failed runtime initialization.
pub(super) struct Recovery {
    directory: PathBuf,
    report_file: File,
    report: Report,
    journal: Journal,
    control: SerialCandidate,
    plan: Option<TerminalPlan>,
    cancelled: Arc<AtomicBool>,
    restore_capture: Option<Recorder<File>>,
    before_restore_readiness: Option<Recorder<File>>,
    restore_readiness: Option<Recorder<File>>,
    restore_verification: Option<Recorder<File>>,
    runtime_capture: Option<File>,
}

impl Recovery {
    fn recorder(&self, name: &'static str) -> AppResult<Recorder<File>> {
        Ok(Recorder::named(
            create_private_file(&self.directory.join(name))?,
            Arc::clone(&self.cancelled),
            name,
        ))
    }

    fn publish(&mut self) -> AppResult<()> {
        let journal = self.journal.synchronize();
        if let Err(error) = &journal {
            self.problem(error);
            self.retain_unpublished_debt();
        }
        let publication = self.write_report();
        if let Err(error) = &publication {
            self.problem(error.as_ref());
            self.retain_unpublished_debt();
        }
        match (journal, publication) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Err(error)) => Err(error),
            (Err(journal), Err(report)) => Err(CommandError(format!(
                "journal synchronization: {journal}; report publication: {report}"
            ))
            .into()),
        }
    }

    const fn retain_unpublished_debt(&mut self) {
        if matches!(self.report.restoration, RestorationState::Verified) {
            self.report.restoration = RestorationState::Owed;
        }
    }

    fn write_report(&mut self) -> AppResult<()> {
        let _position = self.report_file.seek(SeekFrom::Start(0))?;
        self.report_file.set_len(0)?;
        serde_json::to_writer_pretty(&mut self.report_file, &self.report)?;
        self.report_file.write_all(b"\n")?;
        self.report_file.sync_all()?;
        Ok(())
    }

    fn problem(&mut self, error: &(dyn std::error::Error + 'static)) {
        self.report.errors.push(Failure::from_error(error));
    }

    /// Record the cause and the close result of a failed control open.
    ///
    /// The original error is returned unchanged.
    fn retain_control_result<T>(&mut self, outcome: AppResult<T>) -> AppResult<T> {
        if let Err(error) = &outcome
            && let Some(admission) = error.downcast_ref::<control::OpenFailure>()
        {
            self.report
                .control_admission_errors
                .push(admission.primary.clone());
            self.report.retirements.push(admission.retirement.clone());
        }
        outcome
    }

    /// Add a runtime initialization, link, or shutdown failure to the report.
    pub(super) fn record_runtime_failure(&mut self, message: &str) {
        self.report.errors.push(Failure {
            message: message.to_owned(),
            causes: Vec::new(),
        });
    }

    const fn entry_evidence(&mut self) {
        self.report.entry_plan_retained = self.journal.entry_plan().is_some();
        self.report.entry_exit_acknowledged = self.journal.entry_exit_acknowledged;
        if self.journal.write_started {
            self.report.restoration = RestorationState::Owed;
        }
    }

    fn preparation_released(&self) -> bool {
        self.report.transition_cleanup.is_none()
            && self
                .report
                .retirements
                .iter()
                .all(control::Retirement::succeeded)
            && self
                .report
                .readiness
                .iter()
                .all(reconnect::ReadinessVerification::owners_released)
            && self
                .report
                .transition
                .iter()
                .all(|attempt| attempt.retirement_error.is_none())
            && self.report.control.iter().all(|observed| {
                observed.close_error.is_none()
                    && observed.capture_error.is_none()
                    && observed.transcript.complete
            })
            && self
                .report
                .original_opening
                .iter()
                .chain(&self.report.reopens)
                .all(|history| {
                    history.capture_error.is_none()
                        && history.transcript.complete
                        && history.attempts.iter().all(|attempt| {
                            attempt
                                .error
                                .as_ref()
                                .is_none_or(native::OpenFailure::host_retirement_confirmed)
                        })
                })
    }

    /// Restore the pre-startup Terminal settings over USB and write the report.
    ///
    /// `owner_released` must be true only when the modem connection was closed
    /// and dropped. When it is false, or when a page write's outcome is
    /// unknown, restoration is skipped and the report records it as still
    /// owed; no page is rewritten without comparing it again first.
    pub(super) async fn finish(mut self, owner_released: bool) -> Result<(), String> {
        let mut backend = reconnect::SystemBackend::new();
        Box::pin(self.finish_and_publish(&mut backend, owner_released)).await
    }

    async fn finish_with(
        &mut self,
        backend: &mut impl UsbBackend,
        released: bool,
    ) -> AppResult<()> {
        self.report.owner_released = Some(released && self.preparation_released());
        let outcome = self.restore_with(backend).await;
        self.report.owner_released = Some(released && self.preparation_released());
        if self.report.owner_released == Some(false)
            && self.report.restoration == RestorationState::Owed
        {
            self.report.restoration = RestorationState::Blocked;
        }
        if let Err(error) = &outcome {
            self.report
                .restore_errors
                .push(Failure::from_error(error.as_ref()));
        }
        outcome
    }

    async fn finish_and_publish(
        &mut self,
        backend: &mut impl UsbBackend,
        released: bool,
    ) -> Result<(), String> {
        let outcome = self.finish_with(backend, released).await;
        let published = self.publish();
        match (outcome, published) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(format!(
                "{error}; recovery capture: {}",
                self.directory.display()
            )),
            (Err(error), Err(capture)) => Err(format!(
                "{error}; report publication: {capture}; recovery capture: {}",
                self.directory.display()
            )),
        }
    }

    async fn restore_with(&mut self, backend: &mut impl UsbBackend) -> AppResult<()> {
        if self.report.owner_released != Some(true) {
            if self.report.restoration == RestorationState::Owed {
                self.report.restoration = RestorationState::Blocked;
            }
            return Err(CommandError("the modem connection was not confirmed closed with a complete transcript; no restoration traffic was sent".to_owned()).into());
        }
        if let Some(capture) = self.runtime_capture.take()
            && let Err(error) = capture.sync_all()
        {
            self.report.runtime_capture_error = Some(Failure::from_error(&error));
            if self.report.restoration == RestorationState::Owed {
                self.report.restoration = RestorationState::Blocked;
            }
            return Err(error.into());
        }
        if self.report.restoration != RestorationState::Owed {
            return Ok(());
        }
        if self.plan.is_none() {
            self.report.restoration = RestorationState::Blocked;
            return Err(CommandError("Terminal restoration is still owed: the entry write or the modem close was not confirmed, so no MCP write was attempted".to_owned()).into());
        }
        let plan = self
            .plan
            .clone()
            .ok_or_else(|| CommandError("restoration plan missing".to_owned()))?;
        let finish_required = AtomicBool::new(false);
        let before = self.before_restore_readiness.take().ok_or_else(|| {
            CommandError("pre-restoration readiness capture already consumed".to_owned())
        })?;
        let ready = reconnect::verify_readiness(
            backend,
            &self.control,
            DEFAULT_BAUD,
            plan.identity(),
            before,
            &finish_required,
        )
        .await;
        let matched = ready.succeeded();
        self.report.readiness.push(ready);
        if !matched {
            return Err(CommandError(
                "independent USB control did not regain CAT readiness; restoration remains owed"
                    .to_owned(),
            )
            .into());
        }
        output::line(format_args!(
            "Restoring the original Gateway mode and route through {}.",
            self.control.path
        ));
        self.restore_pages(backend, &plan, &finish_required).await?;
        self.verify_restored(backend, &plan, &finish_required).await
    }

    async fn restore_pages(
        &mut self,
        backend: &mut impl UsbBackend,
        plan: &TerminalPlan,
        finish_required: &AtomicBool,
    ) -> AppResult<()> {
        let transcript = self
            .restore_capture
            .take()
            .ok_or_else(|| CommandError("restoration capture already consumed".to_owned()))?;
        let opened = control::open(backend, &self.control, transcript, finish_required).await;
        let mut transport = self.retain_control_result(opened)?;
        let applied = program::apply(
            &mut transport,
            plan.identity(),
            DvGatewayMode::Terminal,
            Some(plan),
            &mut self.journal,
            finish_required,
        )
        .await;
        let retired = control::retire(transport).await;
        let released = retired.succeeded();
        self.report.retirements.push(retired);
        let _restored = applied?;
        if !released {
            return Err(CommandError(
                "restoration exit was followed by failed connection or capture cleanup".to_owned(),
            )
            .into());
        }
        Ok(())
    }

    async fn verify_restored(
        &mut self,
        backend: &mut impl UsbBackend,
        plan: &TerminalPlan,
        finish_required: &AtomicBool,
    ) -> AppResult<()> {
        let readiness = self.restore_readiness.take().ok_or_else(|| {
            CommandError("restoration readiness capture already consumed".to_owned())
        })?;
        let ready = reconnect::verify_readiness(
            backend,
            &self.control,
            DEFAULT_BAUD,
            plan.identity(),
            readiness,
            finish_required,
        )
        .await;
        let matched = ready.succeeded();
        self.report.readiness.push(ready);
        if !matched {
            return Err(CommandError(
                "restored settings have not passed fresh CAT readiness".to_owned(),
            )
            .into());
        }
        let capture = self.restore_verification.take().ok_or_else(|| {
            CommandError("restoration verification capture already consumed".to_owned())
        })?;
        let expected_gateway = match plan.target() {
            kenwood_tmd750::radio::terminal::TerminalTarget::Off => DvGatewayMode::Off,
            kenwood_tmd750::radio::terminal::TerminalTarget::ReflectorTerminal => {
                DvGatewayMode::Terminal
            }
        };
        let observed = control::observe(
            backend,
            &self.control,
            Some(plan.identity()),
            Some(expected_gateway),
            capture,
            finish_required,
        )
        .await;
        let observed = self.retain_control_result(observed)?;
        let verified = observed.succeeded();
        self.report.control.push(observed);
        if !verified {
            return Err(CommandError(
                "restored Gateway state did not verify on fresh USB CAT".to_owned(),
            )
            .into());
        }
        self.report.restoration = RestorationState::Verified;
        output::line(format_args!(
            "Original Gateway settings restored and verified."
        ));
        Ok(())
    }
}

fn reserve(
    endpoints: &Endpoints,
    cancelled: Arc<AtomicBool>,
) -> AppResult<(Recovery, Recorder<File>)> {
    reserve_at(endpoints, cancelled, None, synchronize_directory)
}

/// Open the directory and `sync_all` it, so its new entries are durable.
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

/// Synchronize `directory`, then its parent, in that order.
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

/// Create the capture directory, report file, journal and recorders.
///
/// `synchronize` makes the new directory entries durable: production passes
/// `synchronize_directory`, and offline fixtures pass a stub that skips only
/// that step, leaving file I/O real.
fn reserve_at(
    endpoints: &Endpoints,
    cancelled: Arc<AtomicBool>,
    requested: Option<&Path>,
    synchronize: impl FnMut(&Path) -> io::Result<()>,
) -> AppResult<(Recovery, Recorder<File>)> {
    let Artifacts {
        directory,
        report: report_file,
        transcript,
    } = Artifacts::create(CaptureKind::DstarStart, requested, Arc::clone(&cancelled))?;
    let journal = Journal::create(&directory, Arc::clone(&cancelled))?;
    let mut recovery = Recovery {
        directory,
        report_file,
        journal,
        control: endpoints.control.clone(),
        plan: None,
        cancelled,
        restore_capture: None,
        before_restore_readiness: None,
        restore_readiness: None,
        restore_verification: None,
        runtime_capture: None,
        report: Report {
            format_version: 1,
            operation: "dstar_start",
            identity_assurance: "exact_endpoint_and_cat_tuple_only",
            bluetooth_address: endpoints.bluetooth.address.to_string(),
            control_path: endpoints.control.path.clone(),
            modem_proved: false,
            restoration: RestorationState::NotRequired,
            original_opening: None,
            reopens: Vec::new(),
            transition: Vec::new(),
            control: Vec::new(),
            control_admission_errors: Vec::new(),
            retirements: Vec::new(),
            transition_cleanup: None,
            readiness: Vec::new(),
            errors: Vec::new(),
            restore_errors: Vec::new(),
            runtime_capture_error: None,
            owner_released: None,
            entry_plan_retained: false,
            entry_exit_acknowledged: false,
        },
    };
    recovery.restore_capture = Some(recovery.recorder("restore-transcript.jsonl")?);
    recovery.before_restore_readiness = Some(recovery.recorder("before-restore-readiness.jsonl")?);
    recovery.restore_readiness = Some(recovery.recorder("restore-readiness.jsonl")?);
    recovery.restore_verification = Some(recovery.recorder("restore-verification.jsonl")?);
    synchronize_directories(&recovery.directory, synchronize)?;
    output::line(format_args!(
        "D-STAR startup and recovery capture: {}.",
        recovery.directory.display()
    ));
    Ok((recovery, transcript))
}

/// Open both endpoints, write the Terminal pages, and acquire MMDVM framing.
///
/// Returns the proved modem connection and the `Recovery` that restores the
/// original settings. `cancelled` is shared with the caller and is checked
/// before each new stage.
pub(super) async fn prepare(
    bluetooth: native::discovery::Request,
    control_port: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<(ProvenModem<ModemConnection>, Recovery), String> {
    Box::pin(prepare_system(bluetooth, control_port, cancelled)).await
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

async fn prepare_system(
    bluetooth: native::discovery::Request,
    control_port: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> Result<(ProvenModem<ModemConnection>, Recovery), String> {
    let endpoints = super::endpoints::resolve(&bluetooth, control_port, &cancelled)
        .await
        .map_err(|error| Failure::from_error(error.as_ref()).to_string())?;
    let (mut recovery, original) =
        reserve(&endpoints, Arc::clone(&cancelled)).map_err(|error| error.to_string())?;
    let mut native = native::SystemBackend;
    let mut usb = reconnect::SystemBackend::new();
    let result = prepare_with(
        &mut native,
        &mut usb,
        &endpoints,
        original,
        &mut recovery,
        &cancelled,
    )
    .await;
    finish_preparation(result, recovery, &mut usb, &cancelled).await
}

async fn finish_preparation<T: Transport>(
    result: AppResult<ProvenModem<CaptureTransport<T, File>>>,
    mut recovery: Recovery,
    usb: &mut impl UsbBackend,
    cancelled: &AtomicBool,
) -> Result<(ProvenModem<CaptureTransport<T, File>>, Recovery), String> {
    let result = match result {
        Ok(proof) if cancelled.load(Ordering::Relaxed) => {
            recovery
                .report
                .retirements
                .push(control::retire(proof.into_transport()).await);
            Err(CommandError(
                "D-STAR preparation cancelled after the modem answered; its connection was closed"
                    .to_owned(),
            )
            .into())
        }
        result => result,
    };
    match result {
        Ok(proof) => Ok((proof, recovery)),
        Err(error) => {
            recovery.problem(error.as_ref());
            let failure = error.to_string();
            let released = recovery.preparation_released();
            match recovery.finish_and_publish(usb, released).await {
                Ok(()) => Err(failure),
                Err(cleanup) => Err(format!("{failure}; {cleanup}")),
            }
        }
    }
}

async fn prepare_with<B: native::Backend, U: UsbBackend>(
    native: &mut B,
    usb: &mut U,
    endpoints: &Endpoints,
    original: Recorder<File>,
    recovery: &mut Recovery,
    cancelled: &AtomicBool,
) -> AppResult<ProvenModem<CaptureTransport<B::Connection, File>>> {
    let (identity, gateway) = observe_control(usb, endpoints, recovery, cancelled).await?;
    let selected = opening::open_selected(
        native,
        &endpoints.bluetooth,
        BluetoothService::SerialPort,
        original,
        cancelled,
    )
    .await;
    recovery.report.original_opening = Some(selected.history);
    let opened = selected
        .opened
        .ok_or_else(|| CommandError("Bluetooth opening failed before Terminal entry".to_owned()))?;
    let channel = opened.channel;
    let mut owner = opened.transport;
    output::line(format_args!(
        "Preparing Reflector Terminal Mode on Bluetooth; preserving the original settings for shutdown."
    ));
    let entry = if gateway == DvGatewayMode::Off {
        program::apply(
            &mut owner,
            &identity,
            gateway,
            None,
            &mut recovery.journal,
            cancelled,
        )
        .await
    } else {
        enter_from_active(usb, endpoints, &identity, recovery, cancelled).await
    };
    recovery.entry_evidence();
    let plan = match entry {
        Ok(plan) => plan,
        Err(error) => {
            recovery
                .report
                .retirements
                .push(control::retire(owner).await);
            return Err(error);
        }
    };
    match plan.restoration() {
        Ok(plan) => recovery.plan = Some(plan),
        Err(error) => {
            recovery
                .report
                .retirements
                .push(control::retire(owner).await);
            return Err(error.into());
        }
    }
    if let Err(error) = recovery.publish() {
        recovery
            .report
            .retirements
            .push(control::retire(owner).await);
        return Err(error);
    }
    prove_transition(native, endpoints, owner, channel, recovery, cancelled).await
}

async fn observe_control(
    usb: &mut impl UsbBackend,
    endpoints: &Endpoints,
    recovery: &mut Recovery,
    cancelled: &AtomicBool,
) -> AppResult<(Identity, DvGatewayMode)> {
    let capture = recovery.recorder("control-preflight.jsonl")?;
    let observation =
        control::observe(usb, &endpoints.control, None, None, capture, cancelled).await;
    let observation = recovery.retain_control_result(observation)?;
    let state = if observation.succeeded() {
        observation
            .identity
            .as_ref()
            .zip(observation.gateway)
            .map(|(identity, gateway)| (identity.0.clone(), DvGatewayMode::from(gateway)))
    } else {
        None
    };
    recovery.report.control.push(observation);
    let (identity, gateway) = state.ok_or_else(|| {
        CommandError(
            "the USB CAT endpoint did not answer identity and Gateway queries; no Terminal change was attempted"
                .to_owned(),
        )
    })?;
    program::validate_identity(&identity)?;
    if !matches!(gateway, DvGatewayMode::Off | DvGatewayMode::Terminal) {
        return Err(
            CommandError("USB Gateway state is neither Off nor Terminal".to_owned()).into(),
        );
    }
    Ok((identity, gateway))
}

async fn prove_transition<B: native::Backend>(
    native: &mut B,
    endpoints: &Endpoints,
    owner: CaptureTransport<B::Connection, File>,
    channel: RfcommChannel,
    recovery: &mut Recovery,
    cancelled: &AtomicBool,
) -> AppResult<ProvenModem<CaptureTransport<B::Connection, File>>> {
    native.wait(Duration::from_secs(2)).await;
    let mut backend = reopen::Reopen {
        backend: native,
        endpoint: &endpoints.bluetooth,
        channel,
        directory: &recovery.directory,
        cancelled: Arc::clone(&recovery.cancelled),
        openings: &mut recovery.report.reopens,
        retirements: &mut recovery.report.retirements,
    };
    let outcome = transition::run(&mut backend, Some(owner), cancelled).await;
    recovery.report.transition = outcome.attempts;
    if let Some(error) = outcome.error {
        recovery.report.errors.push(error);
    }
    recovery.report.transition_cleanup = outcome.cleanup_error;
    let Some(proof) = outcome.proof else {
        return Err(CommandError("Bluetooth did not answer a complete MMDVM version exchange before startup stopped; every attempt is in the startup report".to_owned()).into());
    };
    recovery.report.modem_proved = true;
    let synchronized = proof
        .transport()
        .synchronization_handle()
        .and_then(|handle| {
            handle.sync_all()?;
            Ok(handle)
        });
    match synchronized {
        Ok(handle) => recovery.runtime_capture = Some(handle),
        Err(error) => {
            recovery
                .report
                .retirements
                .push(control::retire(proof.into_transport()).await);
            return Err(error.into());
        }
    }
    if let Err(error) = recovery.publish() {
        recovery
            .report
            .retirements
            .push(control::retire(proof.into_transport()).await);
        return Err(error);
    }
    Ok(proof)
}

async fn enter_from_active(
    usb: &mut impl UsbBackend,
    endpoints: &Endpoints,
    identity: &Identity,
    recovery: &mut Recovery,
    cancelled: &AtomicBool,
) -> AppResult<TerminalPlan> {
    // An active Bluetooth route may already carry MMDVM, so the Terminal pages
    // are written over the separate USB endpoint; no CAT goes to that link.
    let capture = recovery.recorder("active-terminal-control.jsonl")?;
    let opened = control::open(usb, &endpoints.control, capture, cancelled).await;
    let mut control = recovery.retain_control_result(opened)?;
    let result = program::apply(
        &mut control,
        identity,
        DvGatewayMode::Terminal,
        None,
        &mut recovery.journal,
        cancelled,
    )
    .await;
    let retired = control::retire(control).await;
    let released = retired.succeeded();
    recovery.report.retirements.push(retired);
    let plan = result?;
    if !released {
        return Err(
            CommandError("active Terminal control did not close cleanly".to_owned()).into(),
        );
    }
    Ok(plan)
}
