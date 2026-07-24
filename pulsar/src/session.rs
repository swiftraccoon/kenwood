// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Open Terminal connection supervision and call recording.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use dmr_rewind::{Client, ClientConfig, Event, SessionType as WireSessionType, Subscription};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinError, JoinSet};
use tokio::time::Instant;

use crate::capture::{
    AudioPacket, CaptureError, CaptureManager, CaptureRecordLimit, CompletedRecording, DmrIdentity,
    EmbeddedLcRecord, EndReason, SessionType, StreamOrigin, SuperHeader, TerminatorRecord,
    VoiceHeaderRecord,
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
pub async fn record(config: Config) -> Result<(), Error> {
    let call_hang = config.call_hang();
    let server_timeout = config.server_timeout();
    let capture_record_limit = config.max_capture_records_per_call;
    let writer = Arc::new(Writer::new(config.recordings_dir));
    let mut prepared = Vec::with_capacity(config.masters.len());
    for master in config.masters {
        let password = master
            .load_password()
            .map_err(|source| Error::Password {
                master: master.name.clone(),
                source,
            })?
            .expose_secret()
            .to_owned();
        prepared.push((master, password));
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (recording_tx, recording_rx) = mpsc::channel(WRITE_QUEUE_CAPACITY);
    let mut writer_task = tokio::spawn(writer_loop(writer, recording_rx));
    let mut workers = JoinSet::new();

    for (master, password) in prepared {
        let worker_recordings = recording_tx.clone();
        let worker_shutdown = shutdown_rx.clone();
        let _abort_handle = workers.spawn(async move {
            let name = master.name.clone();
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
    drop(shutdown_rx);
    drop(recording_tx);

    let (failure, writer_finished) = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(Error::Signal)?;
            (None, false)
        }
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
    while let Some(worker) = workers.join_next().await {
        let (_master, result) = worker.map_err(Error::WorkerTask)?;
        result?;
    }
    Ok(())
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
    let mut backoff = Backoff::new(seed ^ u64::from(master.port));

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
                    master = %master.name,
                    host = %master.host,
                    port = master.port,
                    error,
                    "Open Terminal connection failed"
                );
            }
            Ok(client) => {
                let peer = client.server_addr();
                tracing::info!(
                    master = %master.name,
                    %peer,
                    subscriptions =
                        master.talkgroups.len().saturating_add(master.private_ids.len()),
                    "Open Terminal connected"
                );
                let connected_at = Instant::now();
                let origin = StreamOrigin {
                    master: master.name.clone(),
                    host: master.host.clone(),
                    port: master.port,
                    peer,
                    dmr_id: master.dmr_id,
                };
                let manager = CaptureManager::new(origin, capture_record_limit);
                match pump_session(
                    client,
                    manager,
                    recordings.clone(),
                    call_hang,
                    shutdown.clone(),
                    &master.name,
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
            master = %master.name,
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
    let mut addresses = tokio::net::lookup_host((master.host.as_str(), master.port))
        .await
        .map_err(|error| {
            format!(
                "resolve {}:{} for {} bind: {error}",
                master.host, master.port, master.bind
            )
        })?;
    let peer = addresses
        .find(|address| address.is_ipv4() == master.bind.is_ipv4())
        .ok_or_else(|| {
            format!(
                "resolve {}:{}: no address matches {} bind",
                master.host,
                master.port,
                if master.bind.is_ipv4() {
                    "IPv4"
                } else {
                    "IPv6"
                }
            )
        })?;

    let mut subscriptions = Vec::with_capacity(
        master
            .talkgroups
            .len()
            .saturating_add(master.private_ids.len()),
    );
    subscriptions.extend(master.talkgroups.iter().copied().map(Subscription::group));
    subscriptions.extend(
        master
            .private_ids
            .iter()
            .copied()
            .map(Subscription::private),
    );

    let client_config = ClientConfig::new(peer, master.dmr_id, password.to_owned())
        .with_bind(master.bind)
        .with_description(format!("pulsar/{} receive-only", env!("CARGO_PKG_VERSION")))
        .with_subscriptions(subscriptions)
        .with_timeout(server_timeout);
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
    loop {
        if *shutdown.borrow_and_update() {
            finish_and_enqueue(&mut manager, EndReason::Shutdown, Utc::now(), &recordings).await?;
            if let Err(error) = client.close().await {
                tracing::debug!(master = label, error = %error, "Open Terminal close failed");
            }
            return Ok(PumpOutcome::Shutdown);
        }

        match client.next_event_for(HOUSEKEEPING_INTERVAL).await {
            Ok(Some(event)) => {
                if handle_event(event, &mut manager, &recordings, label).await?
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
                finish_and_enqueue(&mut manager, EndReason::Disconnect, Utc::now(), &recordings)
                    .await?;
                return Ok(PumpOutcome::Dropped);
            }
        }
        enqueue_if_complete(
            &recordings,
            manager.finish_if_inactive(Utc::now(), call_hang),
        )
        .await?;
    }
}

async fn handle_event(
    event: Event,
    manager: &mut CaptureManager,
    recordings: &mpsc::Sender<CompletedRecording>,
    label: &str,
) -> Result<EventDisposition, Error> {
    match event {
        Event::VoiceHeader(event) => {
            handle_voice_header(event, manager, recordings, label).await?;
        }
        Event::Terminator(event) => {
            handle_terminator(event, manager, recordings).await?;
        }
        Event::Audio(event) => {
            let now = received_at(event.metadata.received_at);
            manager
                .on_audio(
                    AudioPacket {
                        transport_seq: event.metadata.sequence,
                        flags: event.metadata.flags.bits(),
                        subtype: event.subtype,
                        ambe: event.data,
                    },
                    now,
                )
                .map_err(|source| Error::Capture {
                    master: label.to_owned(),
                    source,
                })?;
        }
        Event::EmbeddedData(event) => {
            let now = received_at(event.metadata.received_at);
            manager
                .on_embedded_lc(
                    EmbeddedLcRecord {
                        transport_seq: event.metadata.sequence,
                        flags: event.metadata.flags.bits(),
                        subtype: event.subtype,
                        data: event.data,
                    },
                    now,
                )
                .map_err(|source| Error::Capture {
                    master: label.to_owned(),
                    source,
                })?;
        }
        Event::SuperHeader(event) => {
            handle_super_header(event, manager, recordings, label).await?;
        }
        Event::Close(_metadata) => {
            tracing::warn!(master = label, "master closed Open Terminal session");
            finish_and_enqueue(manager, EndReason::Disconnect, Utc::now(), recordings).await?;
            return Ok(EventDisposition::Dropped);
        }
        Event::Report(event) => {
            tracing::info!(
                master = label,
                report = ?String::from_utf8_lossy(&event.data).trim(),
                "master report"
            );
        }
        Event::Failure(event) => {
            tracing::warn!(
                master = label,
                message = ?String::from_utf8_lossy(&event.data).trim(),
                "master application failure"
            );
        }
        Event::BusyNotice(event) => {
            tracing::warn!(
                master = label,
                payload_bytes = event.data.len(),
                "master busy notice"
            );
        }
        Event::Other(event) => {
            tracing::debug!(
                master = label,
                packet_type = ?event.packet_type,
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
) -> Result<(), Error> {
    let now = received_at(event.metadata.received_at);
    if let Some(header) = header_from_link_control(
        event.link_control.flco,
        event.link_control.source_id,
        event.link_control.destination_id,
    ) {
        enqueue_if_complete(recordings, manager.on_super_header(header, now)).await?;
    }
    manager
        .on_voice_header(
            VoiceHeaderRecord {
                transport_seq: event.metadata.sequence,
                flags: event.metadata.flags.bits(),
                subtype: event.subtype,
                data: event.data,
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
) -> Result<(), Error> {
    let now = received_at(event.metadata.received_at);
    if manager.is_active()
        && let Some(link_control) = event.link_control
        && let Some(header) = header_from_link_control(
            link_control.flco,
            link_control.source_id,
            link_control.destination_id,
        )
    {
        manager.enrich_from_terminator(header);
    }
    let completed = manager.on_terminator(
        TerminatorRecord {
            transport_seq: event.metadata.sequence,
            flags: event.metadata.flags.bits(),
            subtype: event.subtype,
            data: event.data,
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
) -> Result<(), Error> {
    let now = received_at(event.metadata.received_at);
    let wire = event.data;
    let session_type = match wire.session_type {
        WireSessionType::GroupVoice => SessionType::Group,
        WireSessionType::PrivateVoice => SessionType::Private,
        WireSessionType::Unknown(value) => {
            tracing::warn!(
                master = label,
                session_type = value,
                "ignored superheader with unknown session type"
            );
            return Ok(());
        }
    };
    let header = SuperHeader {
        session_type,
        source: DmrIdentity {
            id: wire.source_id,
            callsign: wire.source_call.trimmed_lossy(),
        },
        destination: DmrIdentity {
            id: wire.target_id,
            callsign: wire.target_call.trimmed_lossy(),
        },
    };
    enqueue_if_complete(recordings, manager.on_super_header(header, now)).await
}

fn received_at(value: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(value)
}

const fn header_from_link_control(
    flco: u8,
    source_id: u32,
    destination_id: u32,
) -> Option<SuperHeader> {
    let session_type = match flco & 0x3f {
        0 => SessionType::Group,
        3 => SessionType::Private,
        _ => return None,
    };
    Some(SuperHeader {
        session_type,
        source: DmrIdentity {
            id: source_id,
            callsign: String::new(),
        },
        destination: DmrIdentity {
            id: destination_id,
            callsign: String::new(),
        },
    })
}

async fn finish_and_enqueue(
    manager: &mut CaptureManager,
    reason: EndReason,
    now: DateTime<Utc>,
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
            if header.source.callsign.trim().is_empty() {
                format!("ID{}", header.source.id)
            } else {
                header.source.callsign.clone()
            }
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
    use std::error::Error as StdError;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use tokio::net::UdpSocket;

    type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

    fn test_record_limit() -> Result<CaptureRecordLimit, crate::capture::CaptureRecordLimitError> {
        CaptureRecordLimit::try_from_count(100)
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
        let group = header_from_link_control(0x80, 3_101_234, 91);
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
        let private = header_from_link_control(0x43, 3_101_234, 3_109_999);
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
        assert!(header_from_link_control(4, 1, 2).is_none());
    }

    #[tokio::test]
    async fn capture_overflow_is_fatal_and_never_enqueues_partial_audio() -> TestResult {
        let origin = StreamOrigin {
            master: "BM-TEST".to_owned(),
            host: "127.0.0.1".to_owned(),
            port: 54_006,
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: 3_101_234,
        };
        let limit = CaptureRecordLimit::try_from_count(1)?;
        let mut manager = CaptureManager::new(origin, limit);
        let (recording_tx, mut recording_rx) = mpsc::channel(1);
        let audio_event = |sequence| {
            Event::Audio(dmr_rewind::AudioEvent {
                metadata: dmr_rewind::EventMetadata {
                    sequence,
                    flags: dmr_rewind::PacketFlags::NONE,
                    payload_len: 27,
                    raw_datagram: Vec::new(),
                    received_at: SystemTime::UNIX_EPOCH,
                },
                subtype: 0,
                data: [0xA5; 27],
            })
        };

        assert_eq!(
            handle_event(audio_event(1), &mut manager, &recording_tx, "BM-TEST").await?,
            EventDisposition::Continue
        );
        let overflow = handle_event(audio_event(2), &mut manager, &recording_tx, "BM-TEST").await;

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
            port: 54_006,
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: 3_101_234,
        };
        let mut manager = CaptureManager::new(origin, test_record_limit()?);
        let (recording_tx, mut recording_rx) = mpsc::channel(1);
        let data = [
            0x00, 0x00, 0x04, 0x00, 0x00, 0x5B, 0x2F, 0x52, 0x32, 0x00, 0x00, 0x00,
        ];
        let event = dmr_rewind::TerminatorEvent {
            metadata: dmr_rewind::EventMetadata {
                sequence: 12,
                flags: dmr_rewind::PacketFlags::NONE,
                payload_len: 12,
                raw_datagram: Vec::new(),
                received_at: SystemTime::UNIX_EPOCH,
            },
            subtype: 2,
            data: Some(data),
            link_control: Some(dmr_rewind::FullLinkControl::from_bytes(data)),
        };

        handle_terminator(event, &mut manager, &recording_tx).await?;

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
            port: 54_006,
            peer: "127.0.0.1:54006".parse()?,
            dmr_id: 3_101_234,
        };
        let mut manager = CaptureManager::new(origin, test_record_limit()?);
        let original_header = SuperHeader {
            session_type: SessionType::Group,
            source: DmrIdentity {
                id: 3_101_234,
                callsign: "N0CALL".to_owned(),
            },
            destination: DmrIdentity {
                id: 91,
                callsign: "TG91".to_owned(),
            },
        };
        let now = received_at(SystemTime::UNIX_EPOCH);
        assert!(
            manager
                .on_super_header(original_header.clone(), now)
                .is_none(),
            "the first superheader must open a call without completing one"
        );
        let audio = AudioPacket {
            transport_seq: 11,
            flags: 0x8001,
            subtype: 4,
            ambe: [0xA5; 27],
        };
        assert_eq!(manager.on_audio(audio, now), Ok(()));

        let link_control = dmr_rewind::FullLinkControl {
            flco: 0,
            feature_id: 0,
            service_options: 4,
            destination_id: 3_100,
            source_id: 3_109_999,
            tail: [0; 3],
        };
        let data = link_control.to_bytes()?;
        let event = dmr_rewind::TerminatorEvent {
            metadata: dmr_rewind::EventMetadata {
                sequence: 12,
                flags: dmr_rewind::PacketFlags::REAL_TIME_1,
                payload_len: 12,
                raw_datagram: Vec::new(),
                received_at: SystemTime::UNIX_EPOCH,
            },
            subtype: 2,
            data: Some(data),
            link_control: Some(link_control),
        };
        let (recording_tx, mut recording_rx) = mpsc::channel(2);

        handle_terminator(event, &mut manager, &recording_tx).await?;

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
        let master = MasterConfig {
            name: "BM-TEST".to_owned(),
            host: server_address.ip().to_string(),
            port: server_address.port(),
            dmr_id: 3_101_234,
            password_env: "UNUSED_IN_TEST".to_owned(),
            bind: "127.0.0.1:0".parse()?,
            talkgroups: vec![91],
            private_ids: Vec::new(),
        };
        let client = connect_master(&master, "test-secret", Duration::from_secs(6)).await?;
        let origin = StreamOrigin {
            master: master.name.clone(),
            host: master.host.clone(),
            port: master.port,
            peer: server_address,
            dmr_id: master.dmr_id,
        };
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
            &master.name,
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
