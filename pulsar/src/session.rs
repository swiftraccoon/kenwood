// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Open Terminal connection supervision and call recording.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use dmr_rewind::{Client, ClientConfig, Event, EventKind, Subscription};
use dmr_rewind_core::{Callsign, ControlOctet, SessionType as WireSessionType, WireDmrId};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::capture::{
    AudioPacket, CaptureError, CaptureManager, CaptureRecordLimit, CaptureTime, CompletedRecording,
    DmrIdentity, EmbeddedLcRecord, FinishReason, SessionType, StreamOrigin, SuperHeader,
    TerminatorRecord, VoiceHeaderRecord,
};
use crate::config::{Config, MasterConfig, PasswordError};
use crate::writer::{WriteError, Writer};

const STABLE_AFTER: Duration = Duration::from_secs(60);
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(250);
const WRITE_QUEUE_CAPACITY: usize = 32;

type WorkerOutput = (String, Result<(), Error>);
type JoinedWorker = Option<Result<WorkerOutput, JoinError>>;

/// Recorder supervision or persistence failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Recorder construction was attempted outside a Tokio runtime.
    #[error("start recorder in a Tokio runtime: {0}")]
    Runtime(#[source] tokio::runtime::TryCurrentError),
    /// The recorder's result was already consumed.
    #[error("recorder completion was already collected")]
    AlreadyFinished,
    /// The coordinator task panicked or the runtime cancelled it.
    #[error("recorder coordinator failed: {0}")]
    CoordinatorTask(#[source] JoinError),
    /// A configured Hotspot Security password could not be loaded.
    #[error("load credential for master {master:?}: {source}")]
    Password {
        /// Operator-facing master name.
        master: String,
        /// Environment lookup failure.
        #[source]
        source: PasswordError,
    },
    /// The process could not install or wait for its interrupt handler.
    #[error("wait for Ctrl-C: {0}")]
    Signal(#[source] std::io::Error),
    /// A recorder worker stopped without a shutdown request.
    #[error("master worker {master:?} stopped unexpectedly")]
    WorkerStopped {
        /// Operator-facing master name.
        master: String,
    },
    /// A recorder worker task panicked or was cancelled.
    #[error("master worker task failed: {0}")]
    WorkerTask(#[source] JoinError),
    /// The dedicated recording-writer task stopped unexpectedly.
    #[error("recording writer stopped unexpectedly")]
    WriterStopped,
    /// The dedicated recording-writer task panicked or was cancelled.
    #[error("recording writer task failed: {0}")]
    WriterWorkerTask(#[source] JoinError),
    /// The recording writer stopped accepting completed calls.
    #[error("recording writer is unavailable for master {master:?}")]
    WriterQueueClosed {
        /// Operator-facing master name.
        master: String,
    },
    /// A blocking persistence task panicked or was cancelled.
    #[error("recording write task for master {master:?} failed: {source}")]
    WriteTask {
        /// Operator-facing master name.
        master: String,
        /// Blocking-task failure.
        #[source]
        source: JoinError,
    },
    /// A completed recording could not be committed.
    #[error("write recording for master {master:?}: {source}")]
    Write {
        /// Operator-facing master name.
        master: String,
        /// Atomic writer failure.
        #[source]
        source: WriteError,
    },
    /// A call exceeded the operator's configured retention limit.
    #[error("capture call for master {master:?}: {source}")]
    Capture {
        /// Operator-facing master name.
        master: String,
        /// Exact capture failure, including the configured boundary.
        #[source]
        source: CaptureError,
    },
}

/// An owned running coordinator that drains all workers and persistence tasks.
///
/// Dropping this handle requests shutdown. The coordinator keeps ownership of
/// its workers and writer until draining finishes; the runtime must remain alive.
/// Use [`Self::shutdown`] or [`Self::wait`] to observe completion/errors. Dropping
/// a wait future leaves the task attached to this handle for a later wait.
#[derive(Debug)]
pub struct Recorder {
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<Result<(), Error>>>,
}

impl Recorder {
    /// Load every credential, then start independent master workers.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Runtime`] without a Tokio runtime or [`Error::Password`]
    /// before spawning any task when a credential cannot be loaded.
    pub fn start(config: Config) -> Result<Self, Error> {
        let runtime = tokio::runtime::Handle::try_current().map_err(Error::Runtime)?;
        let call_hang = config.call_hang();
        let server_timeout = config.server_timeout();
        let capture_record_limit = config.max_capture_records_per_call();
        let writer = Arc::new(Writer::new(config.recordings_dir().to_path_buf()));
        let mut prepared = Vec::with_capacity(config.masters().len());
        for master in config.into_masters() {
            let password = master
                .load_password()
                .map_err(|source| Error::Password {
                    master: master.name().to_owned(),
                    source,
                })?
                .expose_secret()
                .to_owned();
            prepared.push((master, password));
        }
        let (shutdown, receiver) = watch::channel(false);
        let control = shutdown.clone();
        let task = runtime.spawn(run_prepared(
            prepared,
            writer,
            call_hang,
            server_timeout,
            capture_record_limit,
            control,
            receiver,
        ));
        Ok(Self {
            shutdown,
            task: Some(task),
        })
    }

    /// Request graceful shutdown without waiting for filesystem I/O.
    pub fn request_shutdown(&self) {
        let _sent = self.shutdown.send(true);
    }

    /// Wait for all workers and accepted writes, including blocking sync calls.
    ///
    /// Each Open Terminal close has its configured send deadline. Queue draining
    /// and filesystem operations have no overall timeout. A full queue pauses
    /// UDP reception and may cause packet loss. No task is aborted on cancellation.
    ///
    /// # Errors
    ///
    /// Returns worker/persistence errors after draining remaining tasks, or
    /// [`Error::AlreadyFinished`] if completion was previously collected.
    pub async fn wait(&mut self) -> Result<(), Error> {
        let task = self.task.as_mut().ok_or(Error::AlreadyFinished)?;
        let result = task.await.map_err(Error::CoordinatorTask);
        self.task = None;
        result?
    }

    /// Request shutdown and await the same completion boundary as [`Self::wait`].
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::wait`].
    pub async fn shutdown(&mut self) -> Result<(), Error> {
        self.request_shutdown();
        self.wait().await
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

#[derive(Debug)]
struct Backoff {
    attempt: u32,
    state: u64,
}

impl Backoff {
    const fn new(seed: u64) -> Self {
        Self {
            attempt: 0,
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_delay(&mut self) -> Duration {
        let exponent = self.attempt.min(6);
        let cap_ms = (1_000_u64 << exponent).min(60_000);
        self.attempt = self.attempt.saturating_add(1);
        let mut value = self.state;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.state = value;
        Duration::from_millis(value % cap_ms)
    }

    const fn reset(&mut self) {
        self.attempt = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PumpOutcome {
    Dropped,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventDisposition {
    Continue,
    Dropped,
}

/// Run every configured recorder until Ctrl-C.
///
/// Each master is supervised independently. Transient DNS, UDP, handshake,
/// authentication, and session failures reconnect with jittered exponential
/// backoff. A persistence failure is fatal because continuing would silently
/// discard completed calls.
///
/// # Errors
///
/// Returns [`Error`] when a credential cannot be loaded, signal handling
/// fails, a worker task dies, or a recording cannot be committed.
///
/// Signal errors request shutdown and drain tasks before returning. Dropping
/// this future requests shutdown and leaves the coordinator draining on the
/// runtime. Callers that need to await cancellation use [`Recorder`] directly.
/// Queue draining and blocking filesystem sync have no overall deadline.
pub async fn record(config: Config) -> Result<(), Error> {
    let mut recorder = Recorder::start(config)?;
    record_until(&mut recorder, tokio::signal::ctrl_c()).await
}

async fn record_until(
    recorder: &mut Recorder,
    signal: impl Future<Output = std::io::Result<()>>,
) -> Result<(), Error> {
    let signal_result = tokio::select! {
        result = recorder.wait() => return result,
        signal = signal => signal,
    };
    let drained = recorder.shutdown().await;
    if let Err(error) = signal_result {
        if let Err(error) = drained {
            tracing::error!(%error, "recorder failed while draining after signal error");
        }
        return Err(Error::Signal(error));
    }
    drained
}

async fn run_prepared(
    prepared: Vec<(MasterConfig, String)>,
    writer: Arc<Writer>,
    call_hang: Duration,
    server_timeout: Duration,
    capture_record_limit: CaptureRecordLimit,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), Error> {
    let (recording_tx, recording_rx) = mpsc::channel(WRITE_QUEUE_CAPACITY);
    let writer_task = tokio::spawn(writer_loop(writer, recording_rx));
    let mut workers = JoinSet::new();

    for (master, password) in prepared {
        let worker_recordings = recording_tx.clone();
        let worker_shutdown = shutdown_rx.clone();
        let _abort_handle = workers.spawn(async move {
            let name = master.name().to_owned();
            let result = supervise(
                master,
                password,
                call_hang,
                server_timeout,
                capture_record_limit,
                worker_recordings,
                worker_shutdown,
            )
            .await;
            (name, result)
        });
    }
    drop(recording_tx);

    run_tasks(shutdown_tx, shutdown_rx, workers, writer_task).await
}

async fn run_tasks(
    shutdown_tx: watch::Sender<bool>,
    mut shutdown_rx: watch::Receiver<bool>,
    mut workers: JoinSet<WorkerOutput>,
    mut writer_task: JoinHandle<Result<(), Error>>,
) -> Result<(), Error> {
    let (failure, writer_finished) = tokio::select! {
        () = wait_for_shutdown(&mut shutdown_rx) => (None, false),
        worker = workers.join_next() => (Some(worker_failure(worker)), false),
        result = &mut writer_task => (Some(writer_failure(result)), true),
    };

    if failure.is_none() {
        tracing::info!("shutdown requested");
    }
    let _send_result = shutdown_tx.send(true);
    let workers_result = drain_workers(&mut workers).await;
    let writer_result = if writer_finished {
        Ok(())
    } else {
        match writer_task.await {
            Ok(result) => result,
            Err(source) => Err(Error::WriterWorkerTask(source)),
        }
    };

    if let Some(failure) = failure {
        if let Err(error) = workers_result {
            tracing::error!(error = %error, "another recorder worker failed during shutdown");
        }
        if let Err(error) = writer_result {
            tracing::error!(error = %error, "recording writer failed during shutdown");
        }
        Err(failure)
    } else {
        workers_result?;
        writer_result
    }
}

fn worker_failure(worker: JoinedWorker) -> Error {
    match worker {
        Some(Ok((master, Ok(())))) => Error::WorkerStopped { master },
        Some(Ok((_master, Err(error)))) => error,
        Some(Err(source)) => Error::WorkerTask(source),
        None => Error::WorkerStopped {
            master: "no configured worker".to_owned(),
        },
    }
}

fn writer_failure(result: Result<Result<(), Error>, JoinError>) -> Error {
    match result {
        Ok(Ok(())) => Error::WriterStopped,
        Ok(Err(error)) => error,
        Err(source) => Error::WriterWorkerTask(source),
    }
}

async fn drain_workers(workers: &mut JoinSet<WorkerOutput>) -> Result<(), Error> {
    let mut failure = None;
    while let Some(worker) = workers.join_next().await {
        let result = match worker {
            Ok((_master, result)) => result,
            Err(source) => Err(Error::WorkerTask(source)),
        };
        if failure.is_none() {
            failure = result.err();
        }
    }
    failure.map_or(Ok(()), Err)
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    while !*shutdown.borrow_and_update() {
        if shutdown.changed().await.is_err() {
            break;
        }
    }
}

async fn supervise(
    master: MasterConfig,
    password: String,
    call_hang: Duration,
    server_timeout: Duration,
    capture_record_limit: CaptureRecordLimit,
    recordings: mpsc::Sender<CompletedRecording>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), Error> {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(1, |duration| {
            duration.as_secs() ^ u64::from(duration.subsec_nanos()) | 1
        });
    let mut backoff = Backoff::new(seed ^ u64::from(master.port().get()));

    loop {
        if *shutdown.borrow_and_update() {
            return Ok(());
        }

        let connection = tokio::select! {
            result = connect_master(&master, &password, server_timeout) => result,
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
                continue;
            }
        };

        match connection {
            Err(error) => {
                tracing::warn!(
                    master = master.name(),
                    host = master.host(),
                    port = master.port().get(),
                    error,
                    "Open Terminal connection failed"
                );
            }
            Ok(client) => {
                let peer = client.server_addr();
                tracing::info!(
                    master = master.name(),
                    %peer,
                    subscriptions =
                        master.talkgroups().len().saturating_add(master.private_ids().len()),
                    "Open Terminal connected"
                );
                let connected_at = Instant::now();
                let origin = StreamOrigin {
                    master: master.name().to_owned(),
                    host: master.host().to_owned(),
                    port: master.port(),
                    peer,
                    dmr_id: master.dmr_id(),
                };
                let manager = CaptureManager::new(origin, capture_record_limit);
                match pump_session(
                    client,
                    manager,
                    recordings.clone(),
                    call_hang,
                    shutdown.clone(),
                    master.name(),
                )
                .await?
                {
                    PumpOutcome::Shutdown => return Ok(()),
                    PumpOutcome::Dropped => {}
                }
                if connected_at.elapsed() >= STABLE_AFTER {
                    backoff.reset();
                }
            }
        }

        let delay = backoff.next_delay();
        tracing::debug!(
            master = master.name(),
            delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
            "reconnect backoff"
        );
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
}

async fn connect_master(
    master: &MasterConfig,
    password: &str,
    server_timeout: Duration,
) -> Result<Client, String> {
    let mut addresses = tokio::net::lookup_host((master.host(), master.port().get()))
        .await
        .map_err(|error| {
            format!(
                "resolve {}:{} for {} bind: {error}",
                master.host(),
                master.port(),
                master.bind()
            )
        })?;
    let peer = addresses
        .find(|address| address.is_ipv4() == master.bind().is_ipv4())
        .ok_or_else(|| {
            format!(
                "resolve {}:{}: no address matches {} bind",
                master.host(),
                master.port(),
                if master.bind().is_ipv4() {
                    "IPv4"
                } else {
                    "IPv6"
                }
            )
        })?;

    let mut subscriptions = Vec::with_capacity(
        master
            .talkgroups()
            .len()
            .saturating_add(master.private_ids().len()),
    );
    subscriptions.extend(master.talkgroups().iter().copied().map(Subscription::group));
    subscriptions.extend(
        master
            .private_ids()
            .iter()
            .copied()
            .map(Subscription::private),
    );

    let configured = (|| {
        ClientConfig::new(peer, master.dmr_id(), password.to_owned())?
            .with_bind(master.bind())?
            .with_description(format!("pulsar/{} receive-only", env!("CARGO_PKG_VERSION")))?
            .with_subscriptions(subscriptions)?
            .with_timeout(server_timeout)
    })();
    let client_config = configured.map_err(|error| error.to_string())?;
    Client::connect(client_config)
        .await
        .map_err(|error| error.to_string())
}

async fn pump_session(
    mut client: Client,
    mut manager: CaptureManager,
    recordings: mpsc::Sender<CompletedRecording>,
    call_hang: Duration,
    mut shutdown: watch::Receiver<bool>,
    label: &str,
) -> Result<PumpOutcome, Error> {
    let clock = Instant::now();
    loop {
        if *shutdown.borrow_and_update() {
            finish_and_enqueue(
                &mut manager,
                FinishReason::Shutdown,
                CaptureTime::new(Utc::now(), clock.elapsed()),
                &recordings,
            )
            .await?;
            if let Err(error) = client.close().await {
                tracing::debug!(master = label, error = %error, "Open Terminal close failed");
            }
            return Ok(PumpOutcome::Shutdown);
        }

        match client.next_event_for(HOUSEKEEPING_INTERVAL).await {
            Ok(Some(event)) => {
                if handle_event(event, &mut manager, &recordings, label, clock).await?
                    == EventDisposition::Dropped
                {
                    return Ok(PumpOutcome::Dropped);
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    master = label,
                    error = %error,
                    "Open Terminal session dropped"
                );
                finish_and_enqueue(
                    &mut manager,
                    FinishReason::Disconnect,
                    CaptureTime::new(Utc::now(), clock.elapsed()),
                    &recordings,
                )
                .await?;
                return Ok(PumpOutcome::Dropped);
            }
        }
        enqueue_if_complete(
            &recordings,
            manager.finish_if_inactive(CaptureTime::new(Utc::now(), clock.elapsed()), call_hang),
        )
        .await?;
    }
}

async fn handle_event(
    event: Event,
    manager: &mut CaptureManager,
    recordings: &mpsc::Sender<CompletedRecording>,
    label: &str,
    clock: Instant,
) -> Result<EventDisposition, Error> {
    match event.into_kind() {
        EventKind::VoiceHeader(event) => {
            handle_voice_header(event, manager, recordings, label, clock).await?;
        }
        EventKind::Terminator(event) => {
            handle_terminator(event, manager, recordings, clock).await?;
        }
        EventKind::Audio(event) => {
            let now = received_at(event.metadata().received_at(), clock);
            manager
                .on_audio(
                    AudioPacket {
                        transport_seq: event.metadata().sequence(),
                        flags: event.metadata().flags(),
                        subtype: event.subtype(),
                        ambe: event.data(),
                    },
                    now,
                )
                .map_err(|source| Error::Capture {
                    master: label.to_owned(),
                    source,
                })?;
        }
        EventKind::EmbeddedData(event) => {
            let now = received_at(event.metadata().received_at(), clock);
            let record = EmbeddedLcRecord::from_event(event);
            manager
                .on_embedded_lc(record, now)
                .map_err(|source| Error::Capture {
                    master: label.to_owned(),
                    source,
                })?;
        }
        EventKind::SuperHeader(event) => {
            handle_super_header(event, manager, recordings, label, clock).await?;
        }
        EventKind::Close(_metadata) => {
            tracing::warn!(master = label, "master closed Open Terminal session");
            finish_and_enqueue(
                manager,
                FinishReason::Disconnect,
                CaptureTime::new(Utc::now(), clock.elapsed()),
                recordings,
            )
            .await?;
            return Ok(EventDisposition::Dropped);
        }
        EventKind::Report(event) => {
            tracing::info!(
                master = label,
                report = ?String::from_utf8_lossy(event.data()).trim(),
                "master report"
            );
        }
        EventKind::Failure(event) => {
            tracing::warn!(
                master = label,
                message = ?String::from_utf8_lossy(event.data()).trim(),
                "master application failure"
            );
        }
        EventKind::BusyNotice(event) => {
            tracing::warn!(
                master = label,
                payload_bytes = event.data().len(),
                "master busy notice"
            );
        }
        EventKind::Other(event) => {
            tracing::debug!(
                master = label,
                packet_type = ?event.packet_type(),
                "ignored Open Terminal packet"
            );
        }
        _ => {}
    }
    Ok(EventDisposition::Continue)
}

async fn handle_voice_header(
    event: dmr_rewind::VoiceHeaderEvent,
    manager: &mut CaptureManager,
    recordings: &mpsc::Sender<CompletedRecording>,
    label: &str,
    clock: Instant,
) -> Result<(), Error> {
    let now = received_at(event.metadata().received_at(), clock);
    if let Some(header) = header_from_link_control(
        event.link_control().flco,
        WireDmrId::new(event.link_control().source_id.get()),
        WireDmrId::new(event.link_control().destination_id.get()),
    ) {
        enqueue_if_complete(recordings, manager.on_super_header(header, now)).await?;
    }
    manager
        .on_voice_header(
            VoiceHeaderRecord {
                transport_seq: event.metadata().sequence(),
                flags: event.metadata().flags(),
                data: event.data(),
            },
            now,
        )
        .map_err(|source| Error::Capture {
            master: label.to_owned(),
            source,
        })?;
    Ok(())
}

async fn handle_terminator(
    event: dmr_rewind::TerminatorEvent,
    manager: &mut CaptureManager,
    recordings: &mpsc::Sender<CompletedRecording>,
    clock: Instant,
) -> Result<(), Error> {
    let now = received_at(event.metadata().received_at(), clock);
    if manager.is_active()
        && let Some(link_control) = event.link_control()
        && let Some(header) = header_from_link_control(
            link_control.flco,
            WireDmrId::new(link_control.source_id.get()),
            WireDmrId::new(link_control.destination_id.get()),
        )
    {
        manager.enrich_from_terminator(header);
    }
    let completed = manager.on_terminator(
        TerminatorRecord {
            transport_seq: event.metadata().sequence(),
            flags: event.metadata().flags(),
            data: event.data(),
        },
        now,
    );
    enqueue_if_complete(recordings, completed).await
}

async fn handle_super_header(
    event: dmr_rewind::SuperHeaderEvent,
    manager: &mut CaptureManager,
    recordings: &mpsc::Sender<CompletedRecording>,
    label: &str,
    clock: Instant,
) -> Result<(), Error> {
    let now = received_at(event.metadata().received_at(), clock);
    let wire = event.data();
    let session_type = match wire.session_type {
        WireSessionType::GroupVoice => SessionType::Group,
        WireSessionType::PrivateVoice => SessionType::Private,
        WireSessionType::Unknown(value) => {
            tracing::warn!(
                master = label,
                session_type = value.get(),
                "ignored superheader with unknown session type"
            );
            return Ok(());
        }
    };
    let header = SuperHeader {
        session_type,
        source: DmrIdentity {
            id: wire.source_id,
            callsign: Some(wire.source_call),
        },
        destination: DmrIdentity {
            id: wire.target_id,
            callsign: Some(wire.target_call),
        },
    };
    enqueue_if_complete(recordings, manager.on_super_header(header, now)).await
}

fn received_at(value: SystemTime, clock: Instant) -> CaptureTime {
    CaptureTime::new(DateTime::<Utc>::from(value), clock.elapsed())
}

const fn header_from_link_control(
    flco: ControlOctet,
    source_id: WireDmrId,
    destination_id: WireDmrId,
) -> Option<SuperHeader> {
    let session_type = match flco.get() & 0x3f {
        0 => SessionType::Group,
        3 => SessionType::Private,
        _ => return None,
    };
    Some(SuperHeader {
        session_type,
        source: DmrIdentity {
            id: source_id,
            callsign: None,
        },
        destination: DmrIdentity {
            id: destination_id,
            callsign: None,
        },
    })
}

async fn finish_and_enqueue(
    manager: &mut CaptureManager,
    reason: FinishReason,
    now: CaptureTime,
    recordings: &mpsc::Sender<CompletedRecording>,
) -> Result<(), Error> {
    enqueue_if_complete(recordings, manager.finish(reason, now)).await
}

async fn enqueue_if_complete(
    recordings: &mpsc::Sender<CompletedRecording>,
    recording: Option<CompletedRecording>,
) -> Result<(), Error> {
    let Some(recording) = recording else {
        return Ok(());
    };
    let master = recording.origin.master.clone();
    recordings
        .send(recording)
        .await
        .map_err(|_error| Error::WriterQueueClosed { master })
}

async fn writer_loop(
    writer: Arc<Writer>,
    mut recordings: mpsc::Receiver<CompletedRecording>,
) -> Result<(), Error> {
    while let Some(recording) = recordings.recv().await {
        write_recording(&writer, recording).await?;
    }
    Ok(())
}

async fn write_recording(writer: &Arc<Writer>, recording: CompletedRecording) -> Result<(), Error> {
    let master = recording.origin.master.clone();
    let source = recording.header.as_ref().map_or_else(
        || "UNKNOWN".to_owned(),
        |header| {
            header
                .source
                .callsign
                .map(Callsign::to_trimmed_lossy)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("ID{}", header.source.id))
        },
    );
    let destination = recording.header.as_ref().map_or_else(
        || "UNKNOWN".to_owned(),
        |header| match header.session_type {
            SessionType::Group => format!("TG{}", header.destination.id),
            SessionType::Private => format!("ID{}", header.destination.id),
        },
    );
    let packets = recording.packet_count();
    let duration_s = recording.codec_duration_s();
    let blocking_writer = Arc::clone(writer);
    let write_master = master.clone();
    let path = tokio::task::spawn_blocking(move || blocking_writer.write(&recording))
        .await
        .map_err(|source| Error::WriteTask {
            master: write_master,
            source,
        })?
        .map_err(|source| Error::Write {
            master: master.clone(),
            source,
        })?;
    tracing::info!(
        master,
        source,
        destination,
        packets,
        duration_s,
        path = %path.display(),
        "DMR recording finalized"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EndReason;
    use crate::config::{MasterHost, MasterName, PasswordVariable};
    use std::error::Error as StdError;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use tokio::net::UdpSocket;

    type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

    fn test_record_limit() -> Result<CaptureRecordLimit, crate::capture::CaptureRecordLimitError> {
        CaptureRecordLimit::new(100)
    }

    #[test]
    fn backoff_respects_exponential_caps_and_reset() {
        let mut backoff = Backoff::new(42);
        for cap_ms in [1_000_u64, 2_000, 4_000, 8_000, 16_000, 32_000, 60_000] {
            assert!(backoff.next_delay().as_millis() < u128::from(cap_ms));
        }
        backoff.reset();
        assert!(backoff.next_delay().as_millis() < 1_000);
    }

    #[test]
    fn backoff_is_deterministic_for_a_seed() {
        let mut left = Backoff::new(99);
        let mut right = Backoff::new(99);
        for _ in 0..10 {
            assert_eq!(left.next_delay(), right.next_delay());
        }
    }

    #[test]
    fn link_control_masks_protection_bits() {
        let group = header_from_link_control(
            ControlOctet::new(0x80),
            WireDmrId::new(3_101_234),
            WireDmrId::new(91),
        );
        assert!(
            matches!(
                group,
                Some(SuperHeader {
                    session_type: SessionType::Group,
                    ..
                })
            ),
            "got {group:?}"
        );
        let private = header_from_link_control(
            ControlOctet::new(0x43),
            WireDmrId::new(3_101_234),
            WireDmrId::new(3_109_999),
        );
        assert!(
            matches!(
                private,
                Some(SuperHeader {
                    session_type: SessionType::Private,
                    ..
                })
            ),
            "got {private:?}"
        );
        assert!(
            header_from_link_control(ControlOctet::new(4), WireDmrId::new(1), WireDmrId::new(2))
                .is_none()
        );
    }

    #[tokio::test]
    async fn capture_overflow_is_fatal_and_never_enqueues_partial_audio() -> TestResult {
        let origin = StreamOrigin {
            master: "BM-TEST".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: const { std::num::NonZeroU16::new(54_006).unwrap() },
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: dmr_rewind::SubscriberId::new(3_101_234)?,
        };
        let limit = CaptureRecordLimit::new(1)?;
        let mut manager = CaptureManager::new(origin, limit);
        let (recording_tx, mut recording_rx) = mpsc::channel(1);
        let audio_event = |sequence| {
            Event::from_datagram(
                rewind_datagram(0x0920, sequence, &[0xA5; 27]),
                SystemTime::UNIX_EPOCH,
            )
        };
        let clock = Instant::now();

        assert_eq!(
            handle_event(
                audio_event(1)?,
                &mut manager,
                &recording_tx,
                "BM-TEST",
                clock
            )
            .await?,
            EventDisposition::Continue
        );
        let overflow = handle_event(
            audio_event(2)?,
            &mut manager,
            &recording_tx,
            "BM-TEST",
            clock,
        )
        .await;

        assert!(
            matches!(
                overflow,
                Err(Error::Capture {
                    ref master,
                    source: CaptureError::RecordLimitExceeded {
                        limit: 1,
                        retained_records: 1,
                    },
                }) if master == "BM-TEST"
            ),
            "got {overflow:?}"
        );
        assert!(
            !manager.is_active(),
            "fatal overflow must discard the partial call"
        );
        assert!(
            matches!(
                recording_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "a partial recording must not reach the writer queue"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stray_full_link_control_terminator_does_not_open_call() -> TestResult {
        let origin = StreamOrigin {
            master: "BM-TEST".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: const { std::num::NonZeroU16::new(54_006).unwrap() },
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: dmr_rewind::SubscriberId::new(3_101_234)?,
        };
        let mut manager = CaptureManager::new(origin, test_record_limit()?);
        let (recording_tx, mut recording_rx) = mpsc::channel(1);
        let data = [
            0x00, 0x00, 0x04, 0x00, 0x00, 0x5B, 0x2F, 0x52, 0x32, 0x00, 0x00, 0x00,
        ];
        let EventKind::Terminator(event) =
            Event::from_datagram(rewind_datagram(0x0912, 12, &data), SystemTime::UNIX_EPOCH)?
                .into_kind()
        else {
            return Err("wrong event kind".into());
        };

        handle_terminator(event, &mut manager, &recording_tx, Instant::now()).await?;

        assert!(!manager.is_active());
        assert!(matches!(
            recording_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mismatched_terminator_does_not_replace_the_active_call() -> TestResult {
        let origin = StreamOrigin {
            master: "BM-TEST".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: const { std::num::NonZeroU16::new(54_006).unwrap() },
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: dmr_rewind::SubscriberId::new(3_101_234)?,
        };
        let mut manager = CaptureManager::new(origin, test_record_limit()?);
        let original_header = SuperHeader {
            session_type: SessionType::Group,
            source: DmrIdentity {
                id: WireDmrId::new(3_101_234),
                callsign: Some(Callsign::from_bytes(*b"N0CALL    ")),
            },
            destination: DmrIdentity {
                id: WireDmrId::new(91),
                callsign: Some(Callsign::from_bytes(*b"TG91      ")),
            },
        };
        let clock = Instant::now();
        let now = received_at(SystemTime::UNIX_EPOCH, clock);
        assert!(
            manager
                .on_super_header(original_header.clone(), now)
                .is_none(),
            "the first superheader must open a call without completing one"
        );
        let audio = AudioPacket {
            transport_seq: 11.into(),
            flags: dmr_rewind_core::PacketFlags::from_bits(0x8001),
            subtype: dmr_rewind_core::DmrAudioSubtype::new(4)?,
            ambe: [0xA5; 27],
        };
        assert_eq!(manager.on_audio(audio, now), Ok(()));

        let link_control = dmr_rewind::FullLinkControl {
            flco: 0.into(),
            feature_id: 0.into(),
            service_options: 4.into(),
            destination_id: dmr_rewind::DmrId::new(3_100)?,
            source_id: dmr_rewind::DmrId::new(3_109_999)?,
            tail: [0; 3],
        };
        let data = link_control.to_bytes();
        let EventKind::Terminator(event) =
            Event::from_datagram(rewind_datagram(0x0912, 12, &data), SystemTime::UNIX_EPOCH)?
                .into_kind()
        else {
            return Err("wrong event kind".into());
        };
        let (recording_tx, mut recording_rx) = mpsc::channel(2);

        handle_terminator(event, &mut manager, &recording_tx, clock).await?;

        let recording = recording_rx.try_recv()?;
        assert_eq!(
            recording.end_reason,
            EndReason::Terminator,
            "the terminator must finalize rather than replace the call"
        );
        assert_eq!(
            recording.header.as_ref(),
            Some(&original_header),
            "conflicting terminator identity must not overwrite the active header"
        );
        assert_eq!(
            recording.audio_packets,
            [audio],
            "audio received before the terminator must remain on the recording"
        );
        assert_eq!(
            recording.terminator.as_ref().and_then(|item| item.data),
            Some(data),
            "the exact conflicting terminator must still be retained as wire evidence"
        );
        assert!(
            !manager.is_active(),
            "the terminator must close the active call"
        );
        assert!(
            matches!(
                recording_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "one terminator must enqueue exactly one recording"
        );
        Ok(())
    }

    #[tokio::test]
    async fn fake_open_terminal_call_is_archived_end_to_end() -> TestResult {
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let server_address = socket.local_addr()?;
        let server = tokio::spawn(run_fake_open_terminal(socket));
        let temporary = tempfile::tempdir()?;
        let master = MasterConfig::new(
            MasterName::new("BM-TEST".to_owned())?,
            MasterHost::new(server_address.ip().to_string())?,
            std::num::NonZeroU16::new(server_address.port()).ok_or("zero bound port")?,
            dmr_rewind::SubscriberId::new(3_101_234)?,
            PasswordVariable::new("UNUSED_IN_TEST".to_owned())?,
            vec![dmr_rewind::DestinationId::new(91)?],
        )?
        .with_bind("127.0.0.1:0".parse()?)?;
        let client = connect_master(&master, "test-secret", Duration::from_secs(6)).await?;
        let origin = StreamOrigin::new(&master, server_address);
        let manager = CaptureManager::new(origin, test_record_limit()?);
        let writer = Arc::new(Writer::new(temporary.path().to_path_buf()));
        let (recording_tx, recording_rx) = mpsc::channel(WRITE_QUEUE_CAPACITY);
        let writer_task = tokio::spawn(writer_loop(writer, recording_rx));
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);

        let outcome = pump_session(
            client,
            manager,
            recording_tx,
            Duration::from_secs(1),
            shutdown_rx,
            master.name(),
        )
        .await?;
        assert_eq!(outcome, PumpOutcome::Dropped);
        server.await??;
        writer_task.await??;

        let json_path = find_recording_json(temporary.path())?;
        let document: serde_json::Value = serde_json::from_slice(&fs::read(&json_path)?)?;
        assert_eq!(
            document
                .pointer("/call/source/id")
                .and_then(serde_json::Value::as_u64),
            Some(3_101_234)
        );
        assert_eq!(
            document
                .pointer("/call/destination/id")
                .and_then(serde_json::Value::as_u64),
            Some(91)
        );
        assert_eq!(
            document
                .pointer("/audio/packets")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            document
                .pointer("/end_reason")
                .and_then(serde_json::Value::as_str),
            Some("terminator")
        );
        assert!(
            document.pointer("/terminator").is_some(),
            "empty wire terminator must still be represented"
        );
        assert!(
            document.pointer("/terminator/data_hex").is_none(),
            "empty wire terminator must not invent Full Link Control"
        );

        let raw_filename = document
            .pointer("/raw/filename")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| io::Error::other("metadata has no raw filename"))?;
        let raw_path = json_path
            .parent()
            .ok_or_else(|| io::Error::other("JSON path has no parent"))?
            .join(raw_filename);
        let raw = fs::read(raw_path)?;
        assert_eq!(raw.len(), 50);
        assert_eq!(raw.get(23..50), Some(&[0xA5; 27][..]));
        Ok(())
    }

    #[tokio::test]
    async fn signal_failure_requests_shutdown_and_waits_for_draining() -> TestResult {
        let (shutdown, mut receiver) = watch::channel(false);
        let (release, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            wait_for_shutdown(&mut receiver).await;
            let _released = wait.await;
            Ok(())
        });
        let mut recorder = Recorder {
            shutdown,
            task: Some(task),
        };
        let mut operation = Box::pin(record_until(
            &mut recorder,
            std::future::ready(Err(io::Error::other("fixture signal failure"))),
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut operation)
                .await
                .is_err(),
            "signal failure must wait for the coordinator"
        );
        release.send(()).map_err(|()| "drain receiver dropped")?;
        let result = operation.await;
        assert!(matches!(result, Err(Error::Signal(_))), "{result:?}");
        assert!(
            recorder.task.is_none(),
            "completion must be collected before signal failure returns"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancelled_wait_keeps_completion_and_drop_requests_shutdown() -> TestResult {
        let (shutdown, mut receiver) = watch::channel(false);
        let (drained, completed) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            wait_for_shutdown(&mut receiver).await;
            let _sent = drained.send(());
            Ok(())
        });
        let mut recorder = Recorder {
            shutdown,
            task: Some(task),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), recorder.wait())
                .await
                .is_err()
        );
        assert!(
            recorder.task.is_some(),
            "cancelling a wait must retain the coordinator handle"
        );
        drop(recorder);
        tokio::time::timeout(Duration::from_secs(1), completed).await??;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_drains_full_queue_and_pending_blocking_persistence() -> TestResult {
        let (shutdown, receiver) = watch::channel(false);
        let mut worker_shutdown = receiver.clone();
        let (queue, mut recordings) = mpsc::channel(1);
        let (started, waiting) = tokio::sync::oneshot::channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writer_barrier = Arc::clone(&barrier);
        let committed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer_committed = Arc::clone(&committed);
        let writer = tokio::spawn(async move {
            let mut started = Some(started);
            while let Some(_recording) = recordings.recv().await {
                let barrier = Arc::clone(&writer_barrier);
                let first = started.take();
                tokio::task::spawn_blocking(move || {
                    if let Some(started) = first {
                        let _sent = started.send(());
                        let _released = barrier.wait();
                    }
                })
                .await
                .map_err(Error::WriterWorkerTask)?;
                let _prior = writer_committed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        });
        let mut workers = JoinSet::new();
        let _worker = workers.spawn(async move {
            for token in 0..3_u8 {
                if queue.send(token).await.is_err() {
                    return ("fixture".to_owned(), Err(Error::WriterStopped));
                }
            }
            wait_for_shutdown(&mut worker_shutdown).await;
            ("fixture".to_owned(), Ok(()))
        });
        let task = tokio::spawn(run_tasks(shutdown.clone(), receiver, workers, writer));
        let mut recorder = Recorder {
            shutdown,
            task: Some(task),
        };
        waiting.await?;
        recorder.request_shutdown();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), recorder.wait())
                .await
                .is_err(),
            "a pending blocking write must keep shutdown incomplete"
        );
        assert_eq!(committed.load(std::sync::atomic::Ordering::SeqCst), 0);
        let _released = barrier.wait();
        recorder.wait().await?;
        assert_eq!(
            committed.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "accepted and backpressured writes must drain"
        );
        Ok(())
    }

    #[tokio::test]
    async fn worker_failure_drains_other_workers_before_returning() -> TestResult {
        let mut workers = JoinSet::new();
        let _failed = workers.spawn(async { ("failed".to_owned(), Err(Error::WriterStopped)) });
        let (release, wait) = tokio::sync::oneshot::channel();
        let _pending = workers.spawn(async {
            let _released = wait.await;
            ("pending".to_owned(), Ok(()))
        });
        let mut draining = Box::pin(drain_workers(&mut workers));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut draining)
                .await
                .is_err(),
            "one failure must not abandon other workers"
        );
        release.send(()).map_err(|()| "worker was aborted")?;
        assert!(matches!(draining.await, Err(Error::WriterStopped)));
        Ok(())
    }

    async fn run_fake_open_terminal(socket: UdpSocket) -> io::Result<()> {
        let mut buffer = [0_u8; 512];
        let (length, client) = socket.recv_from(&mut buffer).await?;
        let keepalive = received_datagram(&buffer, length)?;
        assert_eq!(packet_type(keepalive)?, 0x0000);
        assert_eq!(keepalive.get(22).copied(), Some(0x21));

        let _sent = socket
            .send_to(&rewind_datagram(0x0002, 1, &[1, 2, 3, 4]), client)
            .await?;
        let (length, source) = socket.recv_from(&mut buffer).await?;
        assert_eq!(source, client);
        let authentication = received_datagram(&buffer, length)?;
        assert_eq!(packet_type(authentication)?, 0x0003);
        assert_eq!(authentication.len(), 50);

        let _sent = socket
            .send_to(&rewind_datagram(0x0000, 2, &[]), client)
            .await?;
        let (length, source) = socket.recv_from(&mut buffer).await?;
        assert_eq!(source, client);
        let subscription = received_datagram(&buffer, length)?;
        assert_eq!(packet_type(subscription)?, 0x0901);
        assert_eq!(
            subscription.get(18..26),
            Some(&[7, 0, 0, 0, 91, 0, 0, 0][..])
        );

        let _sent = socket
            .send_to(&rewind_datagram(0x0901, 3, &[]), client)
            .await?;
        let full_link_control = [
            0x00, 0x00, 0x04, 0x00, 0x00, 0x5B, 0x2F, 0x52, 0x32, 0x00, 0x00, 0x00,
        ];
        let _sent = socket
            .send_to(&rewind_datagram(0x0911, 10, &full_link_control), client)
            .await?;
        let _sent = socket
            .send_to(&rewind_datagram(0x0920, 11, &[0xA5; 27]), client)
            .await?;
        let _sent = socket
            .send_to(&rewind_datagram(0x0912, 12, &[]), client)
            .await?;
        let _sent = socket
            .send_to(&rewind_datagram(0x0001, 13, &[]), client)
            .await?;
        Ok(())
    }

    fn rewind_datagram(packet_type: u16, sequence: u32, payload: &[u8]) -> Vec<u8> {
        let payload_len = u16::try_from(payload.len()).unwrap_or(u16::MAX);
        let mut datagram = Vec::with_capacity(18_usize.saturating_add(payload.len()));
        datagram.extend_from_slice(b"REWIND01");
        datagram.extend_from_slice(&packet_type.to_le_bytes());
        datagram.extend_from_slice(&0_u16.to_le_bytes());
        datagram.extend_from_slice(&sequence.to_le_bytes());
        datagram.extend_from_slice(&payload_len.to_le_bytes());
        datagram.extend_from_slice(payload);
        datagram
    }

    fn packet_type(datagram: &[u8]) -> io::Result<u16> {
        let bytes = datagram
            .get(8..10)
            .ok_or_else(|| io::Error::other("datagram has no packet type"))?;
        let bytes: [u8; 2] = bytes
            .try_into()
            .map_err(|_| io::Error::other("packet type is not two bytes"))?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn received_datagram(buffer: &[u8], length: usize) -> io::Result<&[u8]> {
        buffer
            .get(..length)
            .ok_or_else(|| io::Error::other("received length exceeds buffer"))
    }

    fn find_recording_json(base: &Path) -> io::Result<PathBuf> {
        let master = base.join("BM-TEST").join("GROUP-91");
        for date in fs::read_dir(master)? {
            let date = date?;
            if !date.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(date.path())? {
                let path = entry?.path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    return Ok(path);
                }
            }
        }
        Err(io::Error::other("no recording JSON found"))
    }
}
