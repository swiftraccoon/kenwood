// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Tokio event loop driving a sans-io MMDVM codec over any
//! [`AsyncRead`](tokio::io::AsyncRead)+[`AsyncWrite`](tokio::io::AsyncWrite)
//! transport.
//!
//! Lifecycle:
//! 1. Send `GetVersion` + `GetStatus` immediately to learn the
//!    protocol version and initial FIFO depths.
//! 2. Enter the main `tokio::select!` (biased, in priority order):
//!    - receive from [`Request`] channel (handle → loop)
//!    - 250 ms periodic `GetStatus` poll (matches `MMDVMHost`'s
//!      `m_statusTimer(1000, 0, 250)` at `Modem.cpp:245`)
//!    - 10 ms playout tick to drain the [`TxQueue`] into the wire
//!      when modem reports slot space (`Modem.cpp:247`)
//!    - read inbound bytes from the transport (last, so a saturated
//!      RX stream cannot starve the ticks)
//! 3. Loop exits on consumer drop, `Shutdown` command, or a fatal
//!    transport error.

use std::time::Duration;

use mmdvm_core::{
    Command, MmdvmFrame, ModemMode, ModemStatus, NakReason, ProtocolVersion, VersionResponse,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;

use crate::error::ShellError;
use crate::transport::Transport;

use super::{DstarConfiguration, Event, Request, TaskCompletion, TxQueue};

/// Period between automatic `GetStatus` polls.
///
/// Mirrors `m_statusTimer(1000, 0, 250)` in `MMDVMHost/Modem.cpp:245`.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Period between TX-queue playout drains.
///
/// Mirrors `m_playoutTimer(1000, 0, 10)` in `MMDVMHost/Modem.cpp:247`.
const PLAYOUT_INTERVAL: Duration = Duration::from_millis(10);

/// Bytes requested per transport read, enough for a maximum 510-byte frame.
const RX_READ_CHUNK: usize = 512;

/// Deadline for a single transport write (frame bytes + flush).
///
/// On expiry the loop emits [`Event::Fatal`] and retires the stream because a
/// partially written frame cannot safely be followed by another command.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a graceful shutdown keeps trying to flush queued TX
/// frames before force-exiting.
///
/// Checked between loop operations after shutdown is accepted. A write and
/// flush may additionally take [`WRITE_TIMEOUT`]. Undelivered frames are
/// reported through [`Event::TxDropped`].
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum retained RX buffer capacity, guarding against a malformed
/// stream endlessly appending without producing frames. If the buffer
/// exceeds this size with no decode progress the contents are dropped
/// and the loop resyncs.
const RX_BUFFER_HARD_CAP: usize = 8 * 1024;

/// A configuration waiting for a version, or already written awaiting its reply.
enum PendingConfiguration {
    WaitingVersion {
        configuration: DstarConfiguration,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), ShellError>>,
    },
    Written(oneshot::Sender<Result<(), ShellError>>),
}

/// Main tokio task driving a single MMDVM modem.
pub(crate) struct ModemLoop<T: Transport> {
    transport: T,
    rx_buffer: Vec<u8>,
    command_rx: mpsc::Receiver<Request>,
    event_tx: broadcast::Sender<Event>,
    tx_queue: TxQueue,
    dstar_space: u8,
    protocol_version: Option<ProtocolVersion>,
    shutting_down: bool,
    /// Deadline for the shutdown TX flush, set when the `Shutdown`
    /// command arrives; the loop force-exits once it passes.
    flush_deadline: Option<Instant>,
    /// Reply channel of an in-flight `SetMode` awaiting the modem's
    /// ACK/NAK. A dropped or timed-out receiver does not remove this entry;
    /// another mode change is refused until the previous reply arrives.
    pending_set_mode: Option<oneshot::Sender<Result<(), ShellError>>>,
    pending_configuration: Option<PendingConfiguration>,
}

impl<T: Transport> ModemLoop<T> {
    /// Build a new loop.
    pub(crate) fn new(
        transport: T,
        command_rx: mpsc::Receiver<Request>,
        event_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            transport,
            rx_buffer: Vec::with_capacity(RX_READ_CHUNK),
            command_rx,
            event_tx,
            tx_queue: TxQueue::new(),
            dstar_space: 0,
            protocol_version: None,
            shutting_down: false,
            flush_deadline: None,
            pending_set_mode: None,
            pending_configuration: None,
        }
    }

    /// Run the loop until it exits. Returns the owned transport so
    /// callers can recover it after a clean shutdown.
    ///
    /// On error the transport is dropped along with the loop state,
    /// since a failed transport is not useful to recover.
    pub(crate) async fn run(mut self) -> TaskCompletion<T> {
        let result = self.run_inner().await;

        // Every send_dstar_* call for these frames already reported
        // success ("queued"), so discarding them on exit must be
        // observable or the far end hears a silently truncated
        // transmission.
        let undelivered = self.tx_queue.len();
        if undelivered > 0 {
            tracing::warn!(
                target: "mmdvm::tokio_shell",
                frames = undelivered,
                "exiting with undelivered TX frames"
            );
            self.emit_event(Event::TxDropped {
                frames: undelivered,
            });
        }

        match &result {
            Ok(()) => tracing::debug!(
                target: "mmdvm::tokio_shell",
                "modem loop exited cleanly"
            ),
            Err(e) => {
                tracing::warn!(
                    target: "mmdvm::tokio_shell",
                    error = %e,
                    "modem loop exited with error"
                );
                // Consumers watching next_event() cannot see the
                // JoinHandle result, so surface the cause as a
                // terminal event so a dead link is distinguishable
                // from a clean close.
                self.emit_event(Event::Fatal {
                    message: e.to_string(),
                });
            }
        }
        TaskCompletion {
            transport: result.map(|()| self.transport),
            dropped_frames: undelivered,
        }
    }

    async fn run_inner(&mut self) -> Result<(), ShellError> {
        // Initial handshake: send GetVersion, then GetStatus, so the
        // consumer's first couple of events describe the hardware
        // and its current state.
        self.write_frame(&MmdvmFrame::new(Command::GET_VERSION))
            .await?;
        self.write_frame(&MmdvmFrame::new(Command::GET_STATUS))
            .await?;

        let mut read_chunk = [0u8; RX_READ_CHUNK];

        let status_tick_start = Instant::now() + STATUS_POLL_INTERVAL;
        let playout_tick_start = Instant::now() + PLAYOUT_INTERVAL;
        let mut status_tick = tokio::time::interval_at(status_tick_start, STATUS_POLL_INTERVAL);
        let mut playout_tick = tokio::time::interval_at(playout_tick_start, PLAYOUT_INTERVAL);
        // Prefer "skip if we fall behind" over burst-catchup: if the
        // runtime is slow we don't want a flood of back-to-back status
        // polls.
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        playout_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            self.dispatch_configuration().await?;
            if self.shutting_down {
                if self.tx_queue.is_empty() {
                    tracing::debug!(
                        target: "mmdvm::tokio_shell",
                        "shutdown complete; exiting loop"
                    );
                    return Ok(());
                }
                if self
                    .flush_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    // The modem never granted enough space to flush.
                    // Force-exit rather than hang the shutdown() caller
                    // forever; run() reports the loss as TxDropped.
                    tracing::warn!(
                        target: "mmdvm::tokio_shell",
                        "shutdown flush deadline expired with frames still queued"
                    );
                    return Ok(());
                }
            }

            tokio::select! {
                biased;

                maybe_cmd = self.command_rx.recv() => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: command_rx fired");
                    let Some(cmd) = maybe_cmd else {
                        tracing::debug!(
                            target: "mmdvm::tokio_shell",
                            "command channel closed; exiting loop"
                        );
                        return Ok(());
                    };
                    self.apply_command(cmd).await?;
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: command handled");
                }

                // The tick branches sit ABOVE the transport read on
                // purpose: with `biased` polling, a perpetually
                // readable transport (misbehaving firmware streaming
                // back-to-back bytes, or a garbage flood) would
                // otherwise win every iteration and starve status
                // polling and TX playout entirely. Ticks are ready at
                // most once per interval, so RX still dominates in
                // healthy operation. This mirrors the reference,
                // where playout runs unconditionally on every
                // `clock()` call regardless of RX pressure.
                _ = status_tick.tick() => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: status_tick fired");
                    // Keep polling during a shutdown flush: a status
                    // response is the only way to learn that FIFO
                    // space freed up, which is exactly what the flush
                    // is waiting for.
                    self.write_frame(&MmdvmFrame::new(Command::GET_STATUS)).await?;
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: status_tick handled");
                }

                _ = playout_tick.tick() => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: playout_tick fired");
                    self.drain_tx_queue().await?;
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: playout_tick handled");
                }

                read = self.transport.read(&mut read_chunk) => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: transport.read fired");
                    match read {
                        Ok(0) => {
                            tracing::debug!(
                                target: "mmdvm::tokio_shell",
                                "transport EOF; exiting loop"
                            );
                            self.emit_event(Event::TransportClosed);
                            return Ok(());
                        }
                        Ok(n) => {
                            if let Some(slice) = read_chunk.get(..n) {
                                self.rx_buffer.extend_from_slice(slice);
                            }
                            self.drain_rx();
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "mmdvm::tokio_shell",
                                error = %e,
                                "transport read failed"
                            );
                            return Err(ShellError::Io(e));
                        }
                    }
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: transport.read handled");
                }
            }
        }
    }

    /// Apply a command from the handle.
    async fn apply_command(&mut self, cmd: Request) -> Result<(), ShellError> {
        match cmd {
            Request::ConfigureDstar {
                configuration,
                deadline,
                reply,
            } => {
                if self.pending_configuration.is_some() {
                    let _send_result = reply.send(Err(ShellError::ConfigurationPending));
                } else {
                    self.pending_configuration = Some(PendingConfiguration::WaitingVersion {
                        configuration,
                        deadline,
                        reply,
                    });
                }
            }
            Request::GetVersion { reply } => {
                let reply = self
                    .write_for_request(&MmdvmFrame::new(Command::GET_VERSION), reply)
                    .await?;
                let _send_result = reply.send(Ok(()));
            }
            Request::GetStatus { reply } => {
                let reply = self
                    .write_for_request(&MmdvmFrame::new(Command::GET_STATUS), reply)
                    .await?;
                let _send_result = reply.send(Ok(()));
            }
            Request::SetMode {
                mode,
                deadline,
                reply,
            } => {
                if self.pending_set_mode.is_some() {
                    let _send_result = reply.send(Err(ShellError::ModeChangePending));
                } else if reply.is_closed() || Instant::now() >= deadline {
                    let _send_result = reply.send(Err(ShellError::ResponseTimeout));
                } else {
                    let frame = MmdvmFrame::with_payload(Command::SET_MODE, vec![mode.as_byte()])?;
                    self.pending_set_mode = Some(self.write_for_request(&frame, reply).await?);
                }
            }
            Request::SendDStarHeader { bytes, reply } => {
                let result =
                    self.tx_queue
                        .push_dstar_header(bytes)
                        .map_err(|_| ShellError::BufferFull {
                            mode: ModemMode::Dstar,
                        });
                let _send_result = reply.send(result);
            }
            Request::SendDStarData { bytes, reply } => {
                let result =
                    self.tx_queue
                        .push_dstar_data(bytes)
                        .map_err(|_| ShellError::BufferFull {
                            mode: ModemMode::Dstar,
                        });
                let _send_result = reply.send(result);
            }
            Request::SendDStarEot { reply } => {
                let result = self
                    .tx_queue
                    .push_dstar_eot()
                    .map_err(|_| ShellError::BufferFull {
                        mode: ModemMode::Dstar,
                    });
                let _send_result = reply.send(result);
            }
            Request::SendRaw { frame, reply } => {
                let reply = self.write_for_request(&frame, reply).await?;
                let _send_result = reply.send(Ok(()));
            }
            Request::Shutdown { reply } => {
                self.shutting_down = true;
                self.flush_deadline = Some(Instant::now() + SHUTDOWN_FLUSH_TIMEOUT);
                let _send_result = reply.send(());
            }
        }
        Ok(())
    }

    /// Send a waiting configuration only after a matching version is confirmed.
    async fn dispatch_configuration(&mut self) -> Result<(), ShellError> {
        let Some(pending) = self.pending_configuration.take() else {
            return Ok(());
        };
        let PendingConfiguration::WaitingVersion {
            configuration,
            deadline,
            reply,
        } = pending
        else {
            self.pending_configuration = Some(pending);
            return Ok(());
        };
        if reply.is_closed() || Instant::now() >= deadline {
            let _send_result = reply.send(Err(ShellError::ResponseTimeout));
        } else if let Some(got) = self.protocol_version {
            let expected = configuration.protocol();
            if expected == got {
                let frame = MmdvmFrame::with_payload(
                    Command::SET_CONFIG,
                    configuration.payload().to_vec(),
                )?;
                let reply = self.write_for_request(&frame, reply).await?;
                self.pending_configuration = Some(PendingConfiguration::Written(reply));
            } else {
                let _send_result = reply.send(Err(ShellError::ConfigurationProtocolMismatch {
                    expected,
                    got,
                }));
            }
        } else {
            self.pending_configuration = Some(PendingConfiguration::WaitingVersion {
                configuration,
                deadline,
                reply,
            });
        }
        Ok(())
    }

    /// Resolve a reply only for a configuration that reached the wire.
    fn complete_configuration(&mut self, result: Result<(), ShellError>) {
        match self.pending_configuration.take() {
            Some(PendingConfiguration::Written(reply)) => {
                let _send_result = reply.send(result);
            }
            pending => self.pending_configuration = pending,
        }
    }

    /// Walk the RX buffer, decoding every complete frame currently
    /// available.
    fn drain_rx(&mut self) {
        loop {
            match MmdvmFrame::from_bytes(&self.rx_buffer) {
                Ok(Some((frame, consumed))) => {
                    // Drop the consumed prefix. `drain` returns an
                    // iterator that clears the range when dropped.
                    drop(self.rx_buffer.drain(..consumed));
                    self.dispatch_frame(frame);
                }
                Ok(None) => {
                    // Need more bytes.
                    if self.rx_buffer.len() > RX_BUFFER_HARD_CAP {
                        tracing::warn!(
                            target: "mmdvm::tokio_shell",
                            len = self.rx_buffer.len(),
                            "RX buffer exceeded hard cap without decoding a frame; resyncing"
                        );
                        self.rx_buffer.clear();
                    }
                    return;
                }
                Err(e) => {
                    // Silent-death prevention: decode errors are
                    // dropped as diagnostics, not propagated;
                    // propagating would kill the whole session loop
                    // on a single malformed byte. Resync to the next
                    // frame-start candidate so we don't loop forever
                    // on the same junk.
                    tracing::debug!(
                        target: "mmdvm::tokio_shell",
                        error = %e,
                        "decoder rejected RX bytes; resyncing"
                    );
                    self.resync_rx_buffer();
                }
            }
        }
    }

    /// Discard the malformed prefix of the RX buffer up to (but not
    /// including) the next `0xE0` frame-start candidate after index
    /// 0, or everything if none exists. Equivalent to the reference's
    /// byte-at-a-time scan for `MMDVM_FRAME_START`, but linear
    /// instead of quadratic on garbage bursts.
    fn resync_rx_buffer(&mut self) {
        let next_start = self
            .rx_buffer
            .iter()
            .skip(1)
            .position(|&b| b == mmdvm_core::MMDVM_FRAME_START);
        match next_start {
            Some(offset) => {
                // `offset` is relative to index 1.
                drop(self.rx_buffer.drain(..=offset));
            }
            None => self.rx_buffer.clear(),
        }
    }

    /// Dispatch a decoded frame to the appropriate event variant.
    fn dispatch_frame(&mut self, frame: MmdvmFrame) {
        match frame.command() {
            Command::GET_VERSION => self.handle_version(frame.payload()),
            Command::GET_STATUS => self.handle_status(frame.payload()),
            Command::ACK => {
                if let [cmd] = frame.payload() {
                    let command = Command::from(*cmd);
                    if command == Command::SET_MODE
                        && let Some(reply) = self.pending_set_mode.take()
                    {
                        let _send_result = reply.send(Ok(()));
                    }
                    if command == Command::SET_CONFIG {
                        self.complete_configuration(Ok(()));
                    }
                    self.emit_event(Event::Ack { command });
                } else {
                    self.protocol_violation(Command::ACK, "ACK requires one command byte");
                }
            }
            Command::NAK => {
                if let [cmd, reason] = frame.payload() {
                    let command = Command::from(*cmd);
                    let reason = NakReason::from(*reason);
                    if command == Command::SET_MODE
                        && let Some(reply) = self.pending_set_mode.take()
                    {
                        let _send_result = reply.send(Err(ShellError::Nak { command, reason }));
                    }
                    if command == Command::SET_CONFIG {
                        self.complete_configuration(Err(ShellError::Nak { command, reason }));
                    }
                    self.emit_event(Event::Nak { command, reason });
                } else {
                    self.protocol_violation(Command::NAK, "NAK requires command and reason bytes");
                }
            }
            Command::DSTAR_HEADER => self.emit_dstar_header(frame.payload()),
            Command::DSTAR_DATA => self.emit_dstar_data(frame.payload()),
            Command::DSTAR_LOST => {
                self.emit_event(Event::DstarLost);
            }
            Command::DSTAR_EOT => {
                self.emit_event(Event::DstarEot);
            }
            Command::DEBUG1
            | Command::DEBUG2
            | Command::DEBUG3
            | Command::DEBUG4
            | Command::DEBUG5
            | Command::DEBUG_DUMP => {
                self.emit_event(Event::Debug {
                    command: frame.command(),
                    payload: frame.into_payload(),
                });
            }
            Command::SERIAL_DATA => {
                self.emit_event(Event::SerialData(frame.into_payload()));
            }
            Command::TRANSPARENT => {
                self.emit_event(Event::TransparentData(frame.into_payload()));
            }
            other => {
                self.emit_event(Event::UnhandledResponse {
                    command: other,
                    payload: frame.into_payload(),
                });
            }
        }
    }

    /// Handle a `GET_VERSION` response payload.
    fn handle_version(&mut self, payload: &[u8]) {
        match VersionResponse::parse(payload) {
            Ok(v) => {
                self.protocol_version = Some(v.protocol());
                self.dstar_space = 0;
                self.emit_event(Event::Version(v));
            }
            Err(e) => self.protocol_violation(Command::GET_VERSION, &e.to_string()),
        }
    }

    /// Handle a `GET_STATUS` response payload.
    fn handle_status(&mut self, payload: &[u8]) {
        let Some(protocol) = self.protocol_version else {
            self.protocol_violation(
                Command::GET_STATUS,
                "status arrived before a supported version response",
            );
            return;
        };
        let parsed = match protocol {
            ProtocolVersion::V1 => ModemStatus::parse_v1(payload),
            ProtocolVersion::V2 => ModemStatus::parse_v2(payload),
            _ => {
                self.protocol_violation(
                    Command::GET_STATUS,
                    "confirmed version has no supported status layout",
                );
                return;
            }
        };
        match parsed {
            Ok(s) => {
                self.dstar_space = s.dstar_space;
                self.emit_event(Event::Status(s));
            }
            // A status that never parses freezes dstar_space and
            // stalls TX forever with every send reporting success, so
            // the consumer must be able to see that happening.
            Err(e) => self.protocol_violation(Command::GET_STATUS, &e.to_string()),
        }
    }

    /// Emit [`Event::ProtocolViolation`] for a frame whose payload
    /// doesn't match its command's wire layout, with a matching
    /// `warn` log.
    fn protocol_violation(&self, command: Command, detail: &str) {
        tracing::warn!(
            target: "mmdvm::tokio_shell",
            command = format!("0x{command:02X}"),
            detail,
            "protocol violation in modem response"
        );
        self.emit_event(Event::ProtocolViolation {
            command,
            detail: detail.to_owned(),
        });
    }

    /// Deliver an event to the bounded consumer ring without ever
    /// blocking the loop.
    ///
    /// Blocking on a full channel would
    /// deadlock a single-task consumer that is awaiting a command
    /// reply while the channel is full (the consumer waits on the
    /// loop, the loop waits on the consumer). A broadcast channel is a
    /// fixed-size ring: it keeps the loop nonblocking and its receiver
    /// reports the exact overwritten count as [`Event::EventsDropped`].
    fn emit_event(&self, event: Event) {
        if self.event_tx.send(event).is_err() {
            tracing::debug!(
                target: "mmdvm::tokio_shell",
                "event consumer dropped; suppressing further events"
            );
        }
    }

    /// Parse a D-STAR header payload and emit the corresponding
    /// event.
    fn emit_dstar_header(&self, payload: &[u8]) {
        if let Ok(bytes) = <[u8; 41]>::try_from(payload) {
            self.emit_event(Event::DstarHeaderRx { bytes });
        } else {
            self.protocol_violation(
                Command::DSTAR_HEADER,
                &format!(
                    "D-STAR header payload is {} bytes, expected 41",
                    payload.len()
                ),
            );
        }
    }

    /// Parse a D-STAR voice data payload and emit the corresponding
    /// event.
    fn emit_dstar_data(&self, payload: &[u8]) {
        if let Ok(bytes) = <[u8; 12]>::try_from(payload) {
            self.emit_event(Event::DstarDataRx { bytes });
        } else {
            self.protocol_violation(
                Command::DSTAR_DATA,
                &format!(
                    "D-STAR data payload is {} bytes, expected 12",
                    payload.len()
                ),
            );
        }
    }

    /// Release at most ONE queued D-STAR frame per playout tick,
    /// mirroring the reference's pacing (`MMDVMHost/Modem.cpp:1049-1084`
    /// writes a single frame, then restarts the playout timer). The
    /// successful write decrements the local `dstar_space` estimate;
    /// the real number is recalibrated on every status response.
    /// One-frame pacing bounds how far a stale status estimate can
    /// overshoot the modem's real FIFO.
    async fn drain_tx_queue(&mut self) -> Result<(), ShellError> {
        if let Some(frame) = self.tx_queue.pop_if_space_allows(self.dstar_space) {
            let wire = MmdvmFrame::with_payload(frame.command, frame.payload)?;
            tracing::trace!(
                target: "mmdvm::tokio_shell",
                command = format!("0x{:02X}", frame.command),
                mode = ?frame.mode,
                slots = frame.slots_required,
                dstar_space_before = self.dstar_space,
                "draining TX queue"
            );
            self.write_frame(&wire).await?;
            self.dstar_space = self.dstar_space.saturating_sub(frame.slots_required);
        }
        Ok(())
    }

    /// Encode `frame` and push the bytes to the transport, bounded
    /// by [`WRITE_TIMEOUT`] so a wedged write side cannot freeze the
    /// loop indefinitely.
    async fn write_frame(&mut self, frame: &MmdvmFrame) -> Result<(), std::io::Error> {
        let bytes = frame.to_bytes();
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            len = bytes.len(),
            cmd = format!("0x{:02X}", frame.command()),
            "write_frame: awaiting transport.write_all"
        );
        let write = async {
            self.transport.write_all(&bytes).await?;
            self.transport.flush().await
        };
        match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("transport write timed out after {WRITE_TIMEOUT:?}"),
                ));
            }
        }
        tracing::trace!(target: "mmdvm::hang_hunt", "write_frame: flushed");
        Ok(())
    }

    /// Preserve the caller's write error and retire the partially written stream.
    async fn write_for_request(
        &mut self,
        frame: &MmdvmFrame,
        reply: oneshot::Sender<Result<(), ShellError>>,
    ) -> Result<oneshot::Sender<Result<(), ShellError>>, ShellError> {
        if let Err(error) = self.write_frame(frame).await {
            let fatal = std::io::Error::new(error.kind(), error.to_string());
            let _send_result = reply.send(Err(ShellError::Io(error)));
            return Err(ShellError::Io(fatal));
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ConfigurationError, ConfigurationProfile, SignalLevel, SignalLevels, TxDelay,
    };
    use super::*;

    fn configuration(
        profile: ConfigurationProfile,
    ) -> Result<DstarConfiguration, ConfigurationError> {
        DstarConfiguration::new(
            profile,
            TxDelay::try_from(10)?,
            SignalLevels {
                receive: SignalLevel::new(128),
                transmit: SignalLevel::new(128),
            },
        )
    }

    #[tokio::test(start_paused = true)]
    async fn configuration_requires_matching_version_before_any_configuration_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let (transport, mut peer) = tokio::io::duplex(128);
        let (_commands, receiver) = mpsc::channel(1);
        let (sender, _events) = broadcast::channel(8);
        let mut modem = ModemLoop::new(transport, receiver, sender);
        let (reply, response) = oneshot::channel();
        modem
            .apply_command(Request::ConfigureDstar {
                configuration: configuration(ConfigurationProfile::MmdvmV2)?,
                deadline: Instant::now() + Duration::from_secs(2),
                reply,
            })
            .await?;
        modem.dispatch_configuration().await?;
        assert!(
            matches!(
                modem.pending_configuration,
                Some(PendingConfiguration::WaitingVersion { .. })
            ),
            "an unconfirmed layout cannot reach the transport"
        );
        modem.handle_version(b"\x01TEST");
        modem.dispatch_configuration().await?;
        let response = tokio::time::timeout(Duration::from_millis(1), response).await;
        assert!(
            matches!(
                response,
                Ok(Ok(Err(ShellError::ConfigurationProtocolMismatch {
                    expected: ProtocolVersion::V2,
                    got: ProtocolVersion::V1,
                })))
            ),
            "a different confirmed version rejects the selected configuration: {response:?}"
        );
        let mut byte = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(1), peer.read(&mut byte))
                .await
                .is_err(),
            "neither unconfirmed nor mismatched configuration may write even a prefix"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_configuration_retains_reservation_until_its_own_reply()
    -> Result<(), Box<dyn std::error::Error>> {
        let (transport, mut peer) = tokio::io::duplex(128);
        let (_commands, receiver) = mpsc::channel(1);
        let (sender, _events) = broadcast::channel(8);
        let mut modem = ModemLoop::new(transport, receiver, sender);
        modem.handle_version(b"\x01TEST");
        let configuration = configuration(ConfigurationProfile::MmdvmV1)?;
        let (reply, response) = oneshot::channel();
        modem
            .apply_command(Request::ConfigureDstar {
                configuration,
                deadline: Instant::now() + Duration::from_secs(2),
                reply,
            })
            .await?;
        modem.dispatch_configuration().await?;
        drop(response);
        modem.dispatch_configuration().await?;
        modem.dispatch_frame(MmdvmFrame::with_payload(
            Command::ACK,
            vec![Command::SET_MODE.get()],
        )?);
        let (reply, response) = oneshot::channel();
        modem
            .apply_command(Request::ConfigureDstar {
                configuration,
                deadline: Instant::now() + Duration::from_secs(2),
                reply,
            })
            .await?;
        let response = tokio::time::timeout(Duration::from_millis(1), response).await;
        assert!(
            matches!(response, Ok(Ok(Err(ShellError::ConfigurationPending)))),
            "cancellation and an unrelated ACK cannot release a written configuration: {response:?}"
        );
        modem.dispatch_frame(MmdvmFrame::with_payload(
            Command::ACK,
            vec![Command::SET_CONFIG.get()],
        )?);
        let (reply, response) = oneshot::channel();
        modem
            .apply_command(Request::ConfigureDstar {
                configuration,
                deadline: Instant::now() + Duration::from_secs(2),
                reply,
            })
            .await?;
        modem.dispatch_configuration().await?;
        modem.dispatch_frame(MmdvmFrame::with_payload(
            Command::NAK,
            vec![Command::SET_CONFIG.get(), 4],
        )?);
        assert!(
            matches!(
                response.await?,
                Err(ShellError::Nak {
                    command: Command::SET_CONFIG,
                    reason: NakReason::DataIncorrect,
                })
            ),
            "the new configuration receives its own rejection after the late ACK retires the first"
        );
        let mut bytes = [0_u8; 52];
        let _read = peer.read_exact(&mut bytes).await?;
        let (first, consumed) = MmdvmFrame::from_bytes(&bytes)?.ok_or("first configuration")?;
        let (_, second) = bytes.split_at(consumed);
        let (second, consumed) = MmdvmFrame::from_bytes(second)?.ok_or("second configuration")?;
        assert_eq!(
            consumed, 26,
            "only two complete v1 configurations were written"
        );
        assert_eq!(
            first, second,
            "the refused overlap must write no third configuration"
        );
        Ok(())
    }

    #[test]
    fn status_cannot_grant_capacity_before_a_supported_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let (transport, _peer) = tokio::io::duplex(64);
        let (_commands, receiver) = mpsc::channel(1);
        let (sender, mut events) = broadcast::channel(8);
        let mut modem = ModemLoop::new(transport, receiver, sender);
        let status = [1, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0];

        modem.handle_status(&status);
        assert_eq!(
            modem.dstar_space, 0,
            "unsolicited status cannot grant FIFO capacity"
        );
        assert!(
            matches!(
                events.try_recv()?,
                Event::ProtocolViolation {
                    command: Command::GET_STATUS,
                    ..
                }
            ),
            "status before a version is an explicit protocol violation"
        );
        modem.handle_version(&[3]);
        assert!(
            matches!(
                events.try_recv()?,
                Event::ProtocolViolation {
                    command: Command::GET_VERSION,
                    ..
                }
            ),
            "an unsupported version cannot establish the status layout"
        );
        modem.handle_status(&status);
        assert_eq!(
            modem.dstar_space, 0,
            "a rejected version leaves transmission blocked"
        );
        assert!(
            matches!(
                events.try_recv()?,
                Event::ProtocolViolation {
                    command: Command::GET_STATUS,
                    ..
                }
            ),
            "status remains rejected after a malformed version"
        );

        modem.handle_version(b"\x01TEST");
        assert!(
            matches!(events.try_recv()?, Event::Version(_)),
            "a supported version establishes the layout"
        );
        modem.handle_status(&[0, 1, 0, 17, 0, 0, 0]);
        assert_eq!(
            modem.dstar_space, 17,
            "only confirmed-layout status grants capacity"
        );
        assert!(
            matches!(events.try_recv()?, Event::Status(_)),
            "confirmed status is observable"
        );
        Ok(())
    }
}
