//! Retained radio task, checked reconnect identity, CAT observations and APRS cleanup.

use std::fmt;
use std::future::Future;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use kenwood_thd75::radio::LinkState;
use kenwood_thd75::transport::{EitherTransport, SerialTransport};
use kenwood_thd75::types::{Band, DvGatewayMode, GpsRadioMode, SerialNumber};
use kenwood_thd75::{Error, FirmwareProfile, LinkDiagnosis, Radio, StateMonitor};
use kenwood_transport::{Transport, TransportError};
use tokio::sync::{mpsc, watch};

use crate::app::{BandState, Message, RadioState};
use crate::event::{CommandRejection, RadioCommand};

/// Interval between complete CAT observation passes.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Delay between unsuccessful replacement searches.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// MCP failure retaining the model's full progress and independent cleanup errors.
#[derive(Debug, Clone)]
pub(crate) enum McpFailure {
    /// Reading a complete memory image failed.
    Read(Arc<Error>),
    /// Schema identification failed before the write transaction.
    Preflight(Arc<Error>),
    /// Comparing or writing a page failed.
    Write(Arc<kenwood_thd75::radio::programming::McpPageExchangeError>),
}
impl fmt::Display for McpFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "MCP read failed: {error}"),
            Self::Preflight(error) => write!(formatter, "MCP write preflight failed: {error}"),
            Self::Write(error) => write!(formatter, "guarded MCP write failed: {error}"),
        }
    }
}
impl std::error::Error for McpFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read(error) | Self::Preflight(error) => Some(error.as_ref()),
            Self::Write(error) => Some(error.as_ref()),
        }
    }
}

/// One failed radio operation, without erasing the library error classification.
#[derive(Debug, Clone)]
pub(crate) struct RadioFailure(pub(crate) Arc<Error>);
impl fmt::Display for RadioFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl std::error::Error for RadioFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Backend discovery or opening refusal with its original transport diagnostics.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// The selected backend failed discovery or opening.
    Transport(TransportError),
    /// More than one distinct serial endpoint matched discovery.
    AmbiguousPorts(Vec<String>),
}
impl OpenFailure {
    /// Whether the backend reported an unconfirmed release that forbids another open.
    const fn prevents_replacement(&self) -> bool {
        matches!(
            self,
            Self::Transport(
                TransportError::BluetoothClose { .. }
                    | TransportError::BluetoothOpenWithCleanup { .. }
            )
        )
    }
}
impl fmt::Display for OpenFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => error.fmt(formatter),
            Self::AmbiguousPorts(paths) => write!(
                formatter,
                "multiple TH-D75 ports found ({}); select --port explicitly",
                paths.join(", ")
            ),
        }
    }
}
impl std::error::Error for OpenFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::AmbiguousPorts(_) => None,
        }
    }
}

#[derive(Debug)]
enum ConnectionCause {
    Open(OpenFailure),
    Radio(Error),
}
impl fmt::Display for ConnectionCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(error) => error.fmt(formatter),
            Self::Radio(error) => error.fmt(formatter),
        }
    }
}
impl std::error::Error for ConnectionCause {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) => Some(error),
            Self::Radio(error) => Some(error),
        }
    }
}

/// Connection failure and any independent failure to close its open connection.
#[derive(Debug)]
pub(crate) struct ConnectFailure {
    cause: ConnectionCause,
    /// Operator-facing connection failure.
    pub(crate) message: String,
    /// Protocol probe classification, when a probe completed.
    pub(crate) diagnosis: Option<LinkDiagnosis>,
    /// Failure to close the rejected connection.
    pub(crate) release: Option<Error>,
    retry: Option<Endpoint>,
}
impl ConnectFailure {
    /// Describe a failure before an open connection exists.
    pub(crate) fn generic(error: OpenFailure) -> Self {
        Self {
            message: error.to_string(),
            cause: ConnectionCause::Open(error),
            diagnosis: None,
            release: None,
            retry: None,
        }
    }
    /// Transfer the exact selected endpoint for a guided retry without discovery.
    pub(crate) const fn take_retry_endpoint(&mut self) -> Option<Endpoint> {
        self.retry.take()
    }
}
impl fmt::Display for ConnectFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(error) = &self.release {
            write!(formatter, "; connection close failed: {error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ConnectFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Radio operation and connection-release failures observed during task completion.
#[derive(Debug, Default)]
pub(crate) struct WorkerCompletion {
    /// Every failed operation that prevented orderly CAT restoration.
    pub(crate) operations: Vec<Error>,
    /// Failure to close the last retained connection.
    pub(crate) release: Option<Error>,
    /// A retained blocking/open task ended by unwinding or cancellation.
    pub(crate) task: Option<tokio::task::JoinError>,
    /// An open failure whose backend could not confirm resource release.
    pub(crate) open: Option<OpenFailure>,
}
impl WorkerCompletion {
    /// Whether all required mode restoration and connection release completed.
    pub(crate) const fn is_clean(&self) -> bool {
        self.operations.is_empty()
            && self.release.is_none()
            && self.task.is_none()
            && self.open.is_none()
    }
}
impl fmt::Display for WorkerCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for error in &self.operations {
            write!(formatter, "radio operation failed: {error}; ")?;
        }
        if let Some(error) = &self.task {
            write!(formatter, "transport task failed: {error}; ")?;
        }
        if let Some(error) = &self.open {
            write!(formatter, "transport open failed: {error}; ")?;
        }
        match &self.release {
            Some(error) => write!(formatter, "connection close failed: {error}"),
            None => formatter.write_str("connection closed"),
        }
    }
}

/// Stop notification and retained task handle; shutdown may be cancelled and retried.
#[derive(Debug)]
pub(crate) struct RadioWorker {
    stop: watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<WorkerCompletion>>,
}
impl RadioWorker {
    /// Whether the retained task has a completion or panic ready to observe.
    pub(crate) fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }

    /// Signal shutdown without dropping the retained task or its open connection.
    pub(crate) fn request_stop(&self) {
        let _previous = self.stop.send_replace(true);
    }

    /// Finish the current operation, restore CAT from APRS, close, then join.
    ///
    /// Completion is consumed once. A cancelled wait retains the task for retry;
    /// calls after a completed join return an empty completion record.
    pub(crate) async fn shutdown(&mut self) -> Result<WorkerCompletion, tokio::task::JoinError> {
        self.request_stop();
        let result = match self.task.as_mut() {
            Some(task) => task.await,
            None => return Ok(WorkerCompletion::default()),
        };
        self.task = None;
        result
    }
}
impl Drop for RadioWorker {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Endpoint used for replacement searches after the original connection closes.
#[derive(Debug, Clone)]
pub(crate) enum Endpoint {
    Serial {
        path: String,
        baud: NonZeroU32,
    },
    #[cfg(target_os = "macos")]
    Bluetooth(kenwood_transport::bluetooth::BluetoothAddress),
}

/// Fresh discovery or a retry pinned to the endpoint that already opened.
#[derive(Debug)]
pub(crate) enum ConnectionRequest {
    /// Select one initial connection using the requested serial options.
    Discover {
        port: Option<String>,
        baud: NonZeroU32,
    },
    /// Reopen the exact previously selected endpoint without rediscovery.
    Retained(Endpoint),
}
impl ConnectionRequest {
    /// Open through the selected model transport policy.
    pub(crate) fn open(self) -> Result<(String, EitherTransport), OpenFailure> {
        self.open_with(discover_and_open_transport, open_retry)
    }
    fn open_with(
        self,
        discover: impl FnOnce(
            Option<&str>,
            NonZeroU32,
        ) -> Result<(String, EitherTransport), OpenFailure>,
        retained: impl FnOnce(Endpoint) -> Result<(String, EitherTransport), OpenFailure>,
    ) -> Result<(String, EitherTransport), OpenFailure> {
        match self {
            Self::Discover { port, baud } => discover(port.as_deref(), baud),
            Self::Retained(endpoint) => retained(endpoint),
        }
    }
}
fn open_retry(endpoint: Endpoint) -> Result<(String, EitherTransport), OpenFailure> {
    let path = match &endpoint {
        Endpoint::Serial { path, .. } => path.clone(),
        #[cfg(target_os = "macos")]
        Endpoint::Bluetooth(address) => format!("bluetooth:{address}"),
    };
    open_endpoint(endpoint).map(|transport| (path, transport))
}
impl Endpoint {
    #[cfg(target_os = "macos")]
    fn from_transport(path: String, baud: NonZeroU32, transport: &EitherTransport) -> Self {
        if let EitherTransport::Bluetooth(bluetooth) = transport {
            return Self::Bluetooth(bluetooth.address().clone());
        }
        Self::Serial { path, baud }
    }

    #[cfg(not(target_os = "macos"))]
    const fn from_transport(path: String, baud: NonZeroU32, _transport: &EitherTransport) -> Self {
        Self::Serial { path, baud }
    }
}

/// Open exactly one discovered serial port, or the native TH-D75 Bluetooth name.
///
/// Serial ambiguity is rejected before opening. Native name selection rejects
/// duplicate paired names; reconnect retains the actual opened Bluetooth address.
pub(crate) fn discover_and_open_transport(
    port: Option<&str>,
    baud: NonZeroU32,
) -> Result<(String, EitherTransport), OpenFailure> {
    if let Some(path) = port.filter(|path| *path != "auto") {
        return open_serial(path, baud);
    }
    let usb = SerialTransport::discover_usb().map_err(OpenFailure::Transport)?;
    if let Some(path) = unique_port(usb.into_iter().map(|port| port.port_name).collect())? {
        return open_serial(&path, baud);
    }
    #[cfg(target_os = "macos")]
    {
        let bluetooth =
            kenwood_thd75::BluetoothTransport::open(None).map_err(OpenFailure::Transport)?;
        Ok((
            format!("bluetooth:{}", bluetooth.address()),
            EitherTransport::Bluetooth(bluetooth),
        ))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let bluetooth = SerialTransport::discover_bluetooth().map_err(OpenFailure::Transport)?;
        let path = unique_port(bluetooth.into_iter().map(|port| port.port_name).collect())?
            .ok_or(OpenFailure::Transport(TransportError::NotFound))?;
        open_serial(&path, baud)
    }
}
fn unique_port(mut paths: Vec<String>) -> Result<Option<String>, OpenFailure> {
    paths.sort();
    paths.dedup();
    match paths.len() {
        0 => Ok(None),
        1 => Ok(paths.pop()),
        _ => Err(OpenFailure::AmbiguousPorts(paths)),
    }
}
fn open_serial(path: &str, baud: NonZeroU32) -> Result<(String, EitherTransport), OpenFailure> {
    SerialTransport::open_with_baud(path, baud)
        .map(|transport| (path.to_owned(), EitherTransport::Serial(transport)))
        .map_err(OpenFailure::Transport)
}

/// Prove CAT identity and retain the task before enabling UI keyboard input.
pub(crate) async fn spawn_with_transport(
    path: String,
    baud: NonZeroU32,
    transport: EitherTransport,
    tx: mpsc::UnboundedSender<Message>,
    cmd_rx: mpsc::Receiver<RadioCommand>,
) -> Result<(String, RadioWorker), Box<ConnectFailure>> {
    let endpoint = Endpoint::from_transport(path.clone(), baud, &transport);
    let mut radio = Radio::new(transport);
    if let Err(error) = radio.identify().await {
        let diagnosis = radio.probe_silent_link().await;
        let release = radio.disconnect().await.err();
        return Err(Box::new(ConnectFailure {
            message: format!(
                "Could not establish CAT control: {error}. {}",
                diagnosis.guidance()
            ),
            cause: ConnectionCause::Radio(error),
            diagnosis: Some(diagnosis),
            release,
            retry: Some(endpoint),
        }));
    }
    let serial = match radio.get_serial_information().await {
        Ok(identity) => identity.serial_number().clone(),
        Err(error) => {
            return Err(Box::new(ConnectFailure {
                message: format!("CAT serial identity unavailable: {error}"),
                cause: ConnectionCause::Radio(error),
                diagnosis: None,
                release: radio.disconnect().await.err(),
                retry: Some(endpoint),
            }));
        }
    };
    if let Err(error) = radio.set_auto_info(true).await {
        return Err(Box::new(ConnectFailure {
            message: format!("Auto-information setup failed: {error}"),
            cause: ConnectionCause::Radio(error),
            diagnosis: None,
            release: radio.disconnect().await.err(),
            retry: Some(endpoint),
        }));
    }
    let (stop, stopped) = watch::channel(false);
    let inputs = WorkerInputs {
        endpoint,
        serial,
        tx,
        commands: cmd_rx,
        stop: stopped,
    };
    let task = tokio::spawn(run_radio(
        radio,
        inputs,
        |endpoint, serial, tx, mut stop| async move {
            reconnect(&endpoint, &serial, &tx, &mut stop).await
        },
    ));
    Ok((
        path,
        RadioWorker {
            stop,
            task: Some(task),
        },
    ))
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    let _changed = stop.wait_for(|stopped| *stopped).await;
}

struct WorkerInputs {
    endpoint: Endpoint,
    serial: SerialNumber,
    tx: mpsc::UnboundedSender<Message>,
    commands: mpsc::Receiver<RadioCommand>,
    stop: watch::Receiver<bool>,
}

async fn run_radio<T, F, Fut>(
    radio: Radio<T>,
    inputs: WorkerInputs,
    mut replace: F,
) -> WorkerCompletion
where
    T: Transport,
    F: FnMut(Endpoint, SerialNumber, mpsc::UnboundedSender<Message>, watch::Receiver<bool>) -> Fut
        + Send,
    Fut: Future<Output = Result<Option<Radio<T>>, Box<WorkerCompletion>>> + Send,
{
    let WorkerInputs {
        endpoint,
        serial,
        tx,
        mut commands,
        mut stop,
    } = inputs;
    let mut radio = radio;
    let mut completion = WorkerCompletion::default();
    loop {
        let action = run_cat(&mut radio, &tx, &mut commands, &mut stop).await;
        match action {
            CatExit::Stop => break,
            CatExit::Aprs(config) => {
                let (restored, errors) =
                    enter_aprs_session(radio, *config, &tx, &mut commands, &mut stop).await;
                radio = restored;
                if *stop.borrow() || commands.is_closed() || tx.is_closed() {
                    completion.operations = errors;
                    break;
                }
                for error in errors {
                    let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error))));
                }
                if !reconnect_required_after_mcp(&radio) {
                    continue;
                }
            }
            CatExit::Reconnect(error) => {
                let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error))));
            }
        }
        let _send = tx.send(Message::Disconnected);
        if let Err(error) = radio.disconnect().await {
            completion.release = Some(error);
            return completion;
        }
        match replace(endpoint.clone(), serial.clone(), tx.clone(), stop.clone()).await {
            Ok(Some(replacement)) => {
                radio = replacement;
                let _send = tx.send(Message::Reconnected);
            }
            Ok(None) => return completion,
            Err(failure) => return *failure,
        }
    }
    completion.release = radio.disconnect().await.err();
    completion
}

enum CatExit {
    Stop,
    Reconnect(Error),
    Aprs(Box<kenwood_thd75::AprsClientConfig>),
}

async fn run_cat<T: Transport>(
    radio: &mut Radio<T>,
    tx: &mpsc::UnboundedSender<Message>,
    commands: &mut mpsc::Receiver<RadioCommand>,
    stop: &mut watch::Receiver<bool>,
) -> CatExit {
    if *stop.borrow() || commands.is_closed() || tx.is_closed() {
        return CatExit::Stop;
    }
    let (firmware_version, profile) = read_firmware_profile(radio).await;
    if radio.cat_recovery_required() {
        return CatExit::Reconnect(Error::CatRecoveryRequired);
    }
    let radio_type = radio
        .get_radio_type()
        .await
        .map(|kind| format!("{} v{}", kind.region(), kind.hardware_variant()))
        .unwrap_or_default();
    let _send = tx.send(Message::RadioIdentity {
        firmware_version,
        radio_type,
    });
    let mut monitor = StateMonitor::new();
    let mut notifications = radio.subscribe();
    let mut poll = tokio::time::interval(POLL_INTERVAL);
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() || commands.is_closed() || tx.is_closed() {
            return CatExit::Stop;
        }
        if reconnect_required_after_mcp(radio) {
            return CatExit::Reconnect(Error::CatRecoveryRequired);
        }
        tokio::select! {
            biased;
            () = stopped(stop) => return CatExit::Stop,
            command = commands.recv() => {
                let Some(command) = command else { return CatExit::Stop; };
                match command {
                    RadioCommand::EnterAprs { config } => return CatExit::Aprs(config),
                    RadioCommand::ReadMemory => {
                        let progress = tx.clone();
                        match radio.read_memory_image_with_progress(move |page,total| { let _send = progress.send(Message::McpProgress {page,total}); }).await {
                            Ok(bytes) => { let _send = tx.send(Message::McpReadComplete(bytes)); }
                            Err(error) => { let _send = tx.send(Message::McpError(McpFailure::Read(Arc::new(error)))); }
                        }
                        notifications = radio.subscribe();
                        monitor = StateMonitor::new();
                    }
                    RadioCommand::ApplySetting(edit) => {
                        let result = match radio.verify_mcp_schema_target().await {
                            Ok(()) => radio.compare_exchange_memory_pages(std::slice::from_ref(edit.exchange())).await.map_err(|error| McpFailure::Write(Arc::new(error))),
                            Err(error) => Err(McpFailure::Preflight(Arc::new(error))),
                        };
                        match result {
                            Ok(_) => { let _send = tx.send(Message::McpSettingApplied(edit)); }
                            Err(error) => { let _send = tx.send(Message::McpError(error)); }
                        }
                        notifications = radio.subscribe();
                        monitor = StateMonitor::new();
                    }
                    command @ (RadioCommand::ExitAprs | RadioCommand::SendAprsMessage {..}) => {
                        let _send = tx.send(Message::CommandRejected { command, reason: CommandRejection::WrongMode });
                    }
                    command => match execute_cat(radio, command).await {
                        Ok(()) => { let _send = tx.send(Message::Status("CAT command completed".to_owned())); }
                        Err(error) => { let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error)))); }
                    }
                }
            }
            notification = notifications.recv() => {
                if matches!(notification,Err(tokio::sync::broadcast::error::RecvError::Closed)) { return CatExit::Reconnect(Error::CatRecoveryRequired); }
                if let Ok(notification) = notification
                    && let Some(kenwood_thd75::StateChange::BecameBusy { band }) = monitor.apply(&notification) {
                    match radio.get_smeter(band).await {
                        Ok(level) => monitor.apply_smeter(band, level),
                        Err(error) => { let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error)))); }
                    }
                }
            }
            _instant = poll.tick() => {
                match poll_once(radio, profile, &monitor).await {
                    Ok(state) => { if tx.send(Message::RadioUpdate(state)).is_err() { return CatExit::Stop; } }
                    Err(error) => return CatExit::Reconnect(error),
                }
            }
        }
        if reconnect_required_after_mcp(radio) {
            return CatExit::Reconnect(Error::CatRecoveryRequired);
        }
    }
}

async fn execute_cat<T: Transport>(
    radio: &mut Radio<T>,
    command: RadioCommand,
) -> Result<(), Error> {
    match command {
        RadioCommand::TuneChannel { band, channel } => radio.tune_channel(band, channel).await?,
        RadioCommand::FreqUp(band) => {
            radio.set_band(band).await?;
            let _frequency = radio.frequency_up().await?;
        }
        RadioCommand::FreqDown(band) => {
            radio.set_band(band).await?;
            let _frequency = radio.frequency_down().await?;
        }
        RadioCommand::SetSquelch { band, level } => radio.set_squelch(band, level).await?,
        RadioCommand::SetAttenuator { band, enabled } => {
            radio.set_attenuator(band, enabled).await?;
        }
        RadioCommand::SetOperatingMode { band, mode } => {
            radio.set_operating_mode(band, mode).await?;
        }
        RadioCommand::SetBandMode(mode) => radio.set_band_mode(mode).await?,
        RadioCommand::SetBluetooth(enabled) => radio.set_bluetooth(enabled).await?,
        RadioCommand::SetVox(enabled) => radio.set_vox(enabled).await?,
        RadioCommand::SetVoxGain(gain) => radio.set_vox_gain(gain).await?,
        RadioCommand::SetVoxDelay(delay) => radio.set_vox_delay(delay).await?,
        RadioCommand::SetPacketDataRate(rate) => radio.set_packet_data_rate(rate).await?,
        RadioCommand::SetBeaconMode(mode) => radio.set_beacon_mode(mode).await?,
        RadioCommand::SetGpsSettings(settings) => radio.set_gps_settings(settings).await?,
        RadioCommand::SetStepSize { band, step } => radio.set_step_size(band, step).await?,
        RadioCommand::SetPower { band, level } => radio.set_power_level(band, level).await?,
        RadioCommand::SetUrcall { callsign, suffix } => radio.set_urcall(callsign, suffix).await?,
        RadioCommand::PrepareReflectorLink { name, module } => {
            radio.prepare_reflector_link(name, module).await?;
        }
        RadioCommand::PrepareReflectorUnlink => radio.prepare_reflector_unlink().await?,
        RadioCommand::SetCQ => radio.set_cq().await?,
        RadioCommand::ReadMemory
        | RadioCommand::ApplySetting(_)
        | RadioCommand::EnterAprs { .. }
        | RadioCommand::ExitAprs
        | RadioCommand::SendAprsMessage { .. } => {
            unreachable!("session commands are dispatched before CAT execution")
        }
    }
    Ok(())
}

/// Identify a replacement using only CAT reads before any state mutation.
async fn qualify_replacement<T: Transport>(
    radio: &mut Radio<T>,
    expected: &SerialNumber,
) -> Result<bool, Error> {
    let _model = radio.identify().await?;
    let identity = radio.get_serial_information().await?;
    Ok(identity.serial_number() == expected)
}

async fn reconnect(
    endpoint: &Endpoint,
    serial: &SerialNumber,
    tx: &mpsc::UnboundedSender<Message>,
    stop: &mut watch::Receiver<bool>,
) -> Result<Option<Radio<EitherTransport>>, Box<WorkerCompletion>> {
    loop {
        if *stop.borrow() || tx.is_closed() {
            return Ok(None);
        }
        let _send = tx.send(Message::Status(
            "Searching for the same CAT serial identity".to_owned(),
        ));
        let endpoint = endpoint.clone();
        let opened = tokio::task::spawn_blocking(move || open_replacements(endpoint)).await;
        let candidates = match opened {
            Ok(Ok(candidates)) => candidates,
            Ok(Err(error)) => {
                let _send = tx.send(Message::ConnectionError(Arc::new(error)));
                Vec::new()
            }
            Err(error) => {
                return Err(Box::new(WorkerCompletion {
                    task: Some(error),
                    ..WorkerCompletion::default()
                }));
            }
        };
        // Each candidate is opened only after the previous candidate is closed.
        for candidate in candidates {
            if *stop.borrow() || tx.is_closed() {
                return Ok(None);
            }
            let opened = tokio::task::spawn_blocking(move || open_endpoint(candidate)).await;
            let transport = match opened {
                Ok(Ok(transport)) => transport,
                Ok(Err(error)) => {
                    if error.prevents_replacement() {
                        return Err(Box::new(WorkerCompletion {
                            open: Some(error),
                            ..WorkerCompletion::default()
                        }));
                    }
                    let _send = tx.send(Message::ConnectionError(Arc::new(error)));
                    continue;
                }
                Err(error) => {
                    return Err(Box::new(WorkerCompletion {
                        task: Some(error),
                        ..WorkerCompletion::default()
                    }));
                }
            };
            let mut radio = Radio::new(transport);
            let qualification = if *stop.borrow() {
                Ok(false)
            } else {
                qualify_replacement(&mut radio, serial).await
            };
            match qualification {
                Ok(true) if !*stop.borrow() => {
                    if let Err(error) = radio.set_auto_info(true).await {
                        let release = radio.disconnect().await.err();
                        if let Some(release) = release {
                            return Err(Box::new(WorkerCompletion {
                                operations: vec![error],
                                release: Some(release),
                                ..WorkerCompletion::default()
                            }));
                        }
                        let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error))));
                        continue;
                    }
                    return Ok(Some(radio));
                }
                Ok(_) => {
                    let _send = tx.send(Message::Status(
                        "Replacement CAT serial identity did not match".to_owned(),
                    ));
                }
                Err(error) => {
                    let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error))));
                }
            }
            if let Err(error) = radio.disconnect().await {
                return Err(Box::new(WorkerCompletion {
                    release: Some(error),
                    ..WorkerCompletion::default()
                }));
            }
            if *stop.borrow() {
                return Ok(None);
            }
        }
        tokio::select! { () = stopped(stop) => return Ok(None), () = tokio::time::sleep(RECONNECT_INTERVAL) => {} }
    }
}
fn open_replacements(endpoint: Endpoint) -> Result<Vec<Endpoint>, OpenFailure> {
    match endpoint {
        Endpoint::Serial { path, baud } => {
            let mut paths = vec![path];
            for port in SerialTransport::discover_usb().map_err(OpenFailure::Transport)? {
                if !paths.contains(&port.port_name) {
                    paths.push(port.port_name);
                }
            }
            Ok(paths
                .into_iter()
                .map(|path| Endpoint::Serial { path, baud })
                .collect())
        }
        #[cfg(target_os = "macos")]
        Endpoint::Bluetooth(address) => Ok(vec![Endpoint::Bluetooth(address)]),
    }
}
fn open_endpoint(endpoint: Endpoint) -> Result<EitherTransport, OpenFailure> {
    match endpoint {
        Endpoint::Serial { path, baud } => open_serial(&path, baud).map(|(_, transport)| transport),
        #[cfg(target_os = "macos")]
        Endpoint::Bluetooth(address) => {
            kenwood_thd75::BluetoothTransport::open(Some(address.as_str()))
                .map(EitherTransport::Bluetooth)
                .map_err(OpenFailure::Transport)
        }
    }
}

fn reconnect_required_after_mcp<T: Transport>(radio: &Radio<T>) -> bool {
    radio.cat_recovery_required() || *radio.link_state().borrow() == LinkState::Down
}

/// Preserve read failures as unavailable; a lost or ambiguous stream stops the pass.
macro_rules! observation {
    ($radio:expr, $call:expr) => {
        match $call.await {
            Ok(value) => Some(value),
            Err(error) => {
                if $radio.cat_recovery_required()
                    || error.is_link_lost()
                    || matches!(error, Error::Timeout(_))
                {
                    return Err(error);
                }
                None
            }
        }
    };
}

async fn poll_once<T: Transport>(
    radio: &mut Radio<T>,
    profile: Option<FirmwareProfile>,
    monitor: &StateMonitor,
) -> Result<RadioState, Error> {
    let mut state = RadioState {
        band_a: poll_band(radio, Band::A).await?,
        band_b: poll_band(radio, Band::B).await?,
        battery_level: observation!(radio, radio.get_battery_level()),
        band_mode: observation!(radio, radio.get_band_mode()),
        bluetooth: observation!(radio, radio.get_bluetooth()),
        vox: observation!(radio, radio.get_vox()),
        vox_gain: observation!(radio, radio.get_vox_gain()),
        vox_delay: observation!(radio, radio.get_vox_delay()),
        af_gain: observation!(radio, radio.get_af_gain()),
        gps_settings: observation!(radio, radio.get_gps_settings()),
        gps_sentences: observation!(radio, radio.get_gps_sentences()),
        beacon_mode: observation!(radio, radio.get_beacon_mode()),
        fine_step: observation!(radio, radio.get_fine_step()),
        filter_width_ssb: observation!(
            radio,
            radio.get_filter_width(kenwood_thd75::types::FilterMode::Ssb)
        ),
        filter_width_cw: observation!(
            radio,
            radio.get_filter_width(kenwood_thd75::types::FilterMode::Cw)
        ),
        filter_width_am: observation!(
            radio,
            radio.get_filter_width(kenwood_thd75::types::FilterMode::Am)
        ),
        aprs_callsign: observation!(radio, radio.get_aprs_callsign()).flatten(),
        dstar_urcall: observation!(radio, radio.get_urcall()),
        dstar_rpt1: observation!(radio, radio.get_rpt1()),
        dstar_rpt2: observation!(radio, radio.get_rpt2()),
        dstar_slot: observation!(radio, radio.get_dstar_slot()),
        ..RadioState::default()
    };
    (state.gps_mode, state.dstar_gateway_mode) =
        poll_collision_sensitive_state(radio, profile).await?;
    state.band_a.s_meter = monitor.band(Band::A).s_meter;
    state.band_a.busy = monitor.band(Band::A).busy;
    state.band_b.s_meter = monitor.band(Band::B).s_meter;
    state.band_b.busy = monitor.band(Band::B).busy;
    Ok(state)
}
async fn poll_band<T: Transport>(radio: &mut Radio<T>, band: Band) -> Result<BandState, Error> {
    Ok(BandState {
        frequency: observation!(radio, radio.get_frequency(band)),
        squelch: observation!(radio, radio.get_squelch(band)),
        mode: observation!(radio, radio.get_operating_mode(band)),
        power_level: observation!(radio, radio.get_power_level(band)),
        attenuator: observation!(radio, radio.get_attenuator(band)),
        step_size: observation!(radio, radio.get_step_size(band)),
        ..BandState::default()
    })
}
async fn read_firmware_profile<T: Transport>(
    radio: &mut Radio<T>,
) -> (String, Option<FirmwareProfile>) {
    radio.get_firmware_version().await.map_or_else(
        |_error| (String::new(), None),
        |identity| {
            (
                identity.to_string(),
                Some(FirmwareProfile::from_identity(&identity)),
            )
        },
    )
}
async fn poll_collision_sensitive_state<T: Transport>(
    radio: &mut Radio<T>,
    profile: Option<FirmwareProfile>,
) -> Result<(Option<GpsRadioMode>, Option<DvGatewayMode>), Error> {
    let gps = if profile.is_some_and(FirmwareProfile::supports_bare_gps_mode) {
        observation!(radio, radio.read_gps_mode())
    } else {
        None
    };
    let gateway = if profile.is_some_and(FirmwareProfile::supports_bare_gateway) {
        observation!(radio, radio.read_gateway())
    } else {
        None
    };
    Ok((gps, gateway))
}

async fn enter_aprs_session<T: Transport>(
    radio: Radio<T>,
    config: kenwood_thd75::AprsClientConfig,
    tx: &mpsc::UnboundedSender<Message>,
    commands: &mut mpsc::Receiver<RadioCommand>,
    stop: &mut watch::Receiver<bool>,
) -> (Radio<T>, Vec<Error>) {
    let mut client = match kenwood_thd75::AprsClient::start(radio, config).await {
        Ok(client) => client,
        Err(failure) => {
            let (radio, error) = *failure;
            let _send = tx.send(Message::AprsRecoveryFailed(error.to_string()));
            return (radio, vec![error]);
        }
    };
    let _send = tx.send(Message::AprsStarted);
    let mut errors = Vec::new();
    if let Err(error) = run_aprs_loop(&mut client, tx, commands, stop).await {
        errors.push(error);
    }
    let desynced = match client.stop().await {
        Ok(desynced) => desynced,
        Err((client, error)) => {
            errors.push(error);
            let _send = tx.send(Message::AprsRecoveryFailed(
                "KISS exit failed; connection must close".to_owned(),
            ));
            return ((*client).into_radio_recovery_required(), errors);
        }
    };
    match desynced.restore().await {
        Ok(radio) => {
            let _send = tx.send(Message::AprsStopped);
            (radio, errors)
        }
        Err(failure) => {
            let (desynced, error) = *failure;
            errors.push(error);
            let _send = tx.send(Message::AprsRecoveryFailed(
                "CAT restoration failed; connection must close".to_owned(),
            ));
            (desynced.into_radio_unproven(), errors)
        }
    }
}
async fn run_aprs_loop<T: Transport>(
    client: &mut kenwood_thd75::AprsClient<T>,
    tx: &mpsc::UnboundedSender<Message>,
    commands: &mut mpsc::Receiver<RadioCommand>,
    stop: &mut watch::Receiver<bool>,
) -> Result<(), Error> {
    loop {
        if *stop.borrow() || tx.is_closed() {
            return Ok(());
        }
        tokio::select! {
            biased;
            () = stopped(stop) => return Ok(()),
            command = commands.recv() => match command {
                None | Some(RadioCommand::ExitAprs) => return Ok(()),
                Some(RadioCommand::SendAprsMessage {addressee,text}) => match client.queue_message(&addressee,&text) {
                    Ok(message_id) => { let _send = tx.send(Message::AprsMessageQueued {addressee,text,message_id}); }
                    Err(error) => { let _send = tx.send(Message::RadioError(RadioFailure(Arc::new(error)))); }
                },
                Some(command) => { let _send = tx.send(Message::CommandRejected {command,reason:CommandRejection::WrongMode}); }
            },
            event = client.next_event() => match event {
                Ok(Some(event)) => { if tx.send(Message::AprsEvent(Box::new(event))).is_err() { return Ok(()); } }
                Ok(None) | Err(Error::Timeout(_)) => {},
                Err(error) => return Err(error),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_transport::MockTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CloseProbe {
        closes: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
        fail: bool,
    }
    impl Transport for CloseProbe {
        fn write(&mut self, _bytes: &[u8]) -> impl Future<Output = Result<(), TransportError>> {
            std::future::poll_fn(move |_| {
                let _previous = self.writes.fetch_add(1, Ordering::AcqRel);
                std::task::Poll::Ready(Err(TransportError::Write(std::io::Error::other(
                    "unexpected fixture write",
                ))))
            })
        }
        fn read(
            &mut self,
            _bytes: &mut [u8],
        ) -> impl Future<Output = Result<usize, TransportError>> {
            std::future::poll_fn(|_| {
                std::task::Poll::Ready(Err(TransportError::Read(std::io::Error::other(
                    "unexpected fixture read",
                ))))
            })
        }
        fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> {
            std::future::poll_fn(move |_| {
                let _previous = self.closes.fetch_add(1, Ordering::AcqRel);
                std::task::Poll::Ready(if self.fail {
                    Err(TransportError::Disconnected(std::io::Error::other(
                        "fixture close failure",
                    )))
                } else {
                    Ok(())
                })
            })
        }
    }

    #[tokio::test]
    async fn stopped_worker_closes_once_and_preserves_release_failure()
    -> Result<(), Box<dyn std::error::Error>> {
        for fail in [false, true] {
            let closes = Arc::new(AtomicUsize::new(0));
            let writes = Arc::new(AtomicUsize::new(0));
            let radio = Radio::new(CloseProbe {
                closes: Arc::clone(&closes),
                writes: Arc::clone(&writes),
                fail,
            });
            let (_stop, stopped_rx) = watch::channel(true);
            let (messages, _receiver) = mpsc::unbounded_channel();
            let (commands, receiver) = mpsc::channel(1);
            commands.try_send(RadioCommand::FreqUp(Band::A))?;
            let inputs = WorkerInputs {
                endpoint: Endpoint::Serial {
                    path: "fixture".to_owned(),
                    baud: NonZeroU32::new(9600).ok_or("nonzero baud")?,
                },
                serial: SerialNumber::new("C3C10368")?,
                tx: messages,
                commands: receiver,
                stop: stopped_rx,
            };
            let completion = run_radio(
                radio,
                inputs,
                |_endpoint, _serial, _messages, _stop| async {
                    Err(Box::new(WorkerCompletion {
                        operations: vec![Error::CatRecoveryRequired],
                        ..WorkerCompletion::default()
                    }))
                },
            )
            .await;
            assert_eq!(
                closes.load(Ordering::Acquire),
                1,
                "worker did not complete exactly one close"
            );
            assert_eq!(
                writes.load(Ordering::Acquire),
                0,
                "stopped worker started another CAT operation"
            );
            assert_eq!(
                completion.release.is_some(),
                fail,
                "release failure was discarded"
            );
            assert!(
                completion.operations.is_empty(),
                "stopped worker attempted reconnect"
            );
        }
        Ok(())
    }

    #[test]
    fn guided_retry_uses_the_retained_endpoint_without_discovery()
    -> Result<(), Box<dyn std::error::Error>> {
        let baud = NonZeroU32::new(9600).ok_or("nonzero fixture baud")?;
        let request = ConnectionRequest::Retained(Endpoint::Serial {
            path: "selected-radio".to_owned(),
            baud,
        });
        let (path, transport) = request.open_with(
            |_port, _baud| Err(OpenFailure::AmbiguousPorts(vec!["wrong-radio".to_owned()])),
            |endpoint| match endpoint {
                Endpoint::Serial { path, baud: actual } => {
                    assert_eq!(actual, baud);
                    Ok((path, EitherTransport::Mock(MockTransport::new())))
                }
                #[cfg(target_os = "macos")]
                Endpoint::Bluetooth(_) => Err(OpenFailure::AmbiguousPorts(Vec::new())),
            },
        )?;
        assert_eq!(
            path, "selected-radio",
            "guidance retry rediscovered another radio"
        );
        let EitherTransport::Mock(mock) = transport else {
            return Err("fixture opened a real transport".into());
        };
        mock.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_or_closed_command_stream_starts_no_cat_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let (messages, _receiver) = mpsc::unbounded_channel();
        let (stop, mut stopped_rx) = watch::channel(true);
        let (_commands, mut receiver) = mpsc::channel(1);
        let mut radio = Radio::new(MockTransport::new());
        assert!(
            matches!(
                run_cat(&mut radio, &messages, &mut receiver, &mut stopped_rx).await,
                CatExit::Stop
            ),
            "stop ignored before polling"
        );
        let _previous = stop.send_replace(false);
        receiver.close();
        assert!(
            matches!(
                run_cat(&mut radio, &messages, &mut receiver, &mut stopped_rx).await,
                CatExit::Stop
            ),
            "closed command receiver kept polling"
        );
        radio.disconnect().await?;
        Ok(())
    }

    #[tokio::test]
    async fn replacement_serial_is_proved_before_any_mutating_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let expected = SerialNumber::new("C3C10368")?;
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"AE\r", b"AE C5310165,K01\r");
        let mut radio = Radio::new(mock);
        assert!(
            !qualify_replacement(&mut radio, &expected).await?,
            "another physical radio passed replacement identity"
        );
        radio.disconnect().await?;
        Ok(())
    }

    #[test]
    fn ambiguous_serial_discovery_requires_selection() -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            unique_port(vec!["one".to_owned(), "two".to_owned()]).is_err(),
            "ambiguous ports were selected"
        );
        assert_eq!(
            unique_port(vec!["one".to_owned(), "one".to_owned()])?,
            Some("one".to_owned())
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_shutdown_retains_join_until_the_connection_is_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        let (stop, mut stopped_rx) = watch::channel(false);
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            stopped(&mut stopped_rx).await;
            let _closed = closed_rx.await;
            WorkerCompletion::default()
        });
        let mut worker = RadioWorker {
            stop,
            task: Some(task),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), worker.shutdown())
                .await
                .is_err(),
            "shutdown did not wait for release"
        );
        assert!(
            worker.task.is_some(),
            "cancelled shutdown abandoned its task"
        );
        closed_tx
            .send(())
            .map_err(|()| "release task disappeared")?;
        assert!(worker.shutdown().await?.is_clean());
        assert!(worker.task.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_and_automation_firmware_skip_collision_sensitive_reads()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut mock = MockTransport::new();
        mock.expect(b"FV\r", b"FV 1.03.AZM\r");
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);
        let (_, profile) = read_firmware_profile(&mut radio).await;
        assert_eq!(profile, Some(FirmwareProfile::AzimuthAutomation));
        assert_eq!(
            poll_collision_sensitive_state(&mut radio, profile).await?,
            (None, None)
        );
        assert_eq!(
            poll_collision_sensitive_state(&mut radio, None).await?,
            (None, None)
        );
        assert_eq!(
            radio.identify().await?.model,
            kenwood_thd75::RadioModel::ThD75
        );
        Ok(())
    }

    #[tokio::test]
    async fn unavailable_band_observations_are_not_false_or_zero()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut mock = MockTransport::new();
        for command in [
            b"FQ 0\r".as_slice(),
            b"SQ 0\r",
            b"MD 0\r",
            b"PC 0\r",
            b"RA 0\r",
            b"SF 0\r",
        ] {
            mock.expect(command, b"N\r");
        }
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let mut radio = Radio::new(mock);
        let band = poll_band(&mut radio, Band::A).await?;
        assert!(
            band.frequency.is_none()
                && band.squelch.is_none()
                && band.mode.is_none()
                && band.power_level.is_none()
                && band.attenuator.is_none()
                && band.step_size.is_none(),
            "failed reads invented observations"
        );
        assert!(
            band.busy.is_none() && band.s_meter.is_none(),
            "AI state was fabricated"
        );
        assert_eq!(
            radio.identify().await?.model,
            kenwood_thd75::RadioModel::ThD75
        );
        Ok(())
    }
}
