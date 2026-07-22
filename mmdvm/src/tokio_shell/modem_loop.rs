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
//!    - receive from [`Command`] channel (handle → loop)
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
    MMDVM_ACK, MMDVM_DEBUG_DUMP, MMDVM_DEBUG1, MMDVM_DEBUG2, MMDVM_DEBUG3, MMDVM_DEBUG4,
    MMDVM_DEBUG5, MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, MMDVM_DSTAR_LOST,
    MMDVM_GET_STATUS, MMDVM_GET_VERSION, MMDVM_NAK, MMDVM_SERIAL_DATA, MMDVM_SET_MODE,
    MMDVM_TRANSPARENT, MmdvmFrame, ModemMode, ModemStatus, NakReason, VersionResponse,
    decode_frame, encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::error::ShellError;
use crate::transport::Transport;

use super::{Command, Event, TxQueue};

/// Period between automatic `GetStatus` polls.
///
/// Mirrors `m_statusTimer(1000, 0, 250)` in `MMDVMHost/Modem.cpp:245`.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Period between TX-queue playout drains.
///
/// Mirrors `m_playoutTimer(1000, 0, 10)` in `MMDVMHost/Modem.cpp:247`.
const PLAYOUT_INTERVAL: Duration = Duration::from_millis(10);

/// RX buffer grow-as-needed chunk size, aligned with maximum MMDVM
/// frame length (255).
const RX_READ_CHUNK: usize = 512;

/// Deadline for a single transport write (frame bytes + flush).
///
/// A wedged write side (kernel TX buffer full under asserted flow
/// control, hung USB endpoint) would otherwise freeze the entire
/// loop inside a handler `.await`: no reads, no commands, no
/// shutdown. 5 s is far beyond any healthy serial or Bluetooth SPP
/// latency; on expiry the loop exits with [`Event::Fatal`].
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a graceful shutdown keeps trying to flush queued TX
/// frames before force-exiting.
///
/// The queue holds at most 64 frames draining at one per 10 ms
/// playout tick (~640 ms), plus status-poll latency to learn about
/// freed FIFO space; 2 s covers the worst healthy case. A modem
/// that grants no space within that window is treated as wedged;
/// undelivered frames surface as [`Event::TxDropped`].
const SHUTDOWN_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Maximum retained RX buffer capacity, guarding against a malformed
/// stream endlessly appending without producing frames. If the buffer
/// exceeds this size with no decode progress we drop the contents and
/// resync.
const RX_BUFFER_HARD_CAP: usize = 8 * 1024;

/// Main tokio task driving a single MMDVM modem.
pub(crate) struct ModemLoop<T: Transport> {
    transport: T,
    rx_buffer: Vec<u8>,
    command_rx: mpsc::Receiver<Command>,
    event_tx: mpsc::Sender<Event>,
    tx_queue: TxQueue,
    dstar_space: u8,
    protocol_version: u8,
    shutting_down: bool,
    /// Deadline for the shutdown TX flush, set when the `Shutdown`
    /// command arrives; the loop force-exits once it passes.
    flush_deadline: Option<Instant>,
    /// Reply channel of an in-flight `SetMode` awaiting the modem's
    /// ACK/NAK. The firmware acknowledges every `SetMode`
    /// (`MMDVM/SerialPort.cpp` `processMessage`), so the caller's
    /// result reflects whether the mode change actually happened.
    pending_set_mode: Option<oneshot::Sender<Result<(), ShellError>>>,
    /// Events dropped since the event channel was last writable.
    /// See [`ModemLoop::emit_event`].
    dropped_events: u64,
}

impl<T: Transport> ModemLoop<T> {
    /// Build a new loop.
    pub(crate) fn new(
        transport: T,
        command_rx: mpsc::Receiver<Command>,
        event_tx: mpsc::Sender<Event>,
    ) -> Self {
        Self {
            transport,
            rx_buffer: Vec::with_capacity(RX_READ_CHUNK),
            command_rx,
            event_tx,
            tx_queue: TxQueue::new(),
            dstar_space: 0,
            // TH-D75 and newer MMDVMHost firmwares speak v2, so assume
            // that until the first `VersionResponse` corrects us.
            protocol_version: 2,
            shutting_down: false,
            flush_deadline: None,
            pending_set_mode: None,
            dropped_events: 0,
        }
    }

    /// Run the loop until it exits. Returns the owned transport so
    /// callers can recover it after a clean shutdown.
    ///
    /// On error the transport is dropped along with the loop state,
    /// since a failed transport is not useful to recover.
    pub(crate) async fn run(mut self) -> Result<T, ShellError> {
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
        result.map(|()| self.transport)
    }

    async fn run_inner(&mut self) -> Result<(), ShellError> {
        // Initial handshake: send GetVersion, then GetStatus, so the
        // consumer's first couple of events describe the hardware
        // and its current state.
        self.write_frame(&MmdvmFrame::new(MMDVM_GET_VERSION))
            .await?;
        self.write_frame(&MmdvmFrame::new(MMDVM_GET_STATUS)).await?;

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
                    self.write_frame(&MmdvmFrame::new(MMDVM_GET_STATUS)).await?;
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
    async fn apply_command(&mut self, cmd: Command) -> Result<(), ShellError> {
        match cmd {
            Command::GetVersion { reply } => {
                let result = self.write_frame(&MmdvmFrame::new(MMDVM_GET_VERSION)).await;
                let _send_result = reply.send(result);
            }
            Command::GetStatus { reply } => {
                let result = self.write_frame(&MmdvmFrame::new(MMDVM_GET_STATUS)).await;
                let _send_result = reply.send(result);
            }
            Command::SetMode { mode, reply } => {
                let frame = MmdvmFrame::with_payload(MMDVM_SET_MODE, vec![mode.as_byte()]);
                match self.write_frame(&frame).await {
                    // Written: the caller's reply now waits for the
                    // modem's ACK/NAK (resolved in dispatch_frame).
                    Ok(()) => self.pending_set_mode = Some(reply),
                    Err(e) => {
                        let _send_result = reply.send(Err(e));
                    }
                }
            }
            Command::SendDStarHeader { bytes, reply } => {
                let result =
                    self.tx_queue
                        .push_dstar_header(bytes)
                        .map_err(|_| ShellError::BufferFull {
                            mode: ModemMode::DStar,
                        });
                let _send_result = reply.send(result);
            }
            Command::SendDStarData { bytes, reply } => {
                let result =
                    self.tx_queue
                        .push_dstar_data(bytes)
                        .map_err(|_| ShellError::BufferFull {
                            mode: ModemMode::DStar,
                        });
                let _send_result = reply.send(result);
            }
            Command::SendDStarEot { reply } => {
                let result = self
                    .tx_queue
                    .push_dstar_eot()
                    .map_err(|_| ShellError::BufferFull {
                        mode: ModemMode::DStar,
                    });
                let _send_result = reply.send(result);
            }
            Command::SendRaw {
                command,
                payload,
                reply,
            } => {
                let frame = MmdvmFrame::with_payload(command, payload);
                let result = self.write_frame(&frame).await;
                let _send_result = reply.send(result);
            }
            Command::Shutdown { reply } => {
                self.shutting_down = true;
                self.flush_deadline = Some(Instant::now() + SHUTDOWN_FLUSH_TIMEOUT);
                let _send_result = reply.send(());
            }
        }
        Ok(())
    }

    /// Walk the RX buffer, decoding every complete frame currently
    /// available.
    fn drain_rx(&mut self) {
        loop {
            match decode_frame(&self.rx_buffer) {
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
        match frame.command {
            MMDVM_GET_VERSION => self.handle_version(&frame.payload),
            MMDVM_GET_STATUS => self.handle_status(&frame.payload),
            MMDVM_ACK => {
                // The reference always sends the ACK'd command byte;
                // defaulting a missing byte to 0 would misattribute
                // the ACK to GetVersion (0x00).
                if let Some(&cmd) = frame.payload.first() {
                    if cmd == MMDVM_SET_MODE
                        && let Some(reply) = self.pending_set_mode.take()
                    {
                        let _send_result = reply.send(Ok(()));
                    }
                    self.emit_event(Event::Ack { command: cmd });
                } else {
                    self.protocol_violation(MMDVM_ACK, "empty ACK payload");
                }
            }
            MMDVM_NAK => {
                if let Some(&cmd) = frame.payload.first() {
                    let reason = NakReason::from_byte(frame.payload.get(1).copied().unwrap_or(0));
                    if cmd == MMDVM_SET_MODE
                        && let Some(reply) = self.pending_set_mode.take()
                    {
                        let _send_result = reply.send(Err(ShellError::Nak {
                            command: cmd,
                            reason,
                        }));
                    }
                    self.emit_event(Event::Nak {
                        command: cmd,
                        reason,
                    });
                } else {
                    self.protocol_violation(MMDVM_NAK, "empty NAK payload");
                }
            }
            MMDVM_DSTAR_HEADER => self.emit_dstar_header(&frame.payload),
            MMDVM_DSTAR_DATA => self.emit_dstar_data(&frame.payload),
            MMDVM_DSTAR_LOST => {
                self.emit_event(Event::DStarLost);
            }
            MMDVM_DSTAR_EOT => {
                self.emit_event(Event::DStarEot);
            }
            MMDVM_DEBUG1 | MMDVM_DEBUG2 | MMDVM_DEBUG3 | MMDVM_DEBUG4 | MMDVM_DEBUG5
            | MMDVM_DEBUG_DUMP => {
                self.emit_debug(frame.command, &frame.payload);
            }
            MMDVM_SERIAL_DATA => {
                self.emit_event(Event::SerialData(frame.payload));
            }
            MMDVM_TRANSPARENT => {
                self.emit_event(Event::TransparentData(frame.payload));
            }
            other => {
                self.emit_event(Event::UnhandledResponse {
                    command: other,
                    payload: frame.payload,
                });
            }
        }
    }

    /// Handle an `MMDVM_GET_VERSION` response payload.
    fn handle_version(&mut self, payload: &[u8]) {
        match VersionResponse::parse(payload) {
            Ok(v) => {
                self.protocol_version = v.protocol;
                self.emit_event(Event::Version(v));
            }
            // A dropped version response means the assumed protocol
            // version was never confirmed; if it's wrong, every
            // status parse reads shifted offsets. Surface it.
            Err(e) => self.protocol_violation(MMDVM_GET_VERSION, &e.to_string()),
        }
    }

    /// Handle an `MMDVM_GET_STATUS` response payload.
    fn handle_status(&mut self, payload: &[u8]) {
        let parsed = if self.protocol_version >= 2 {
            ModemStatus::parse_v2(payload)
        } else {
            ModemStatus::parse_v1(payload)
        };
        match parsed {
            Ok(s) => {
                self.dstar_space = s.dstar_space;
                self.emit_event(Event::Status(s));
            }
            // A status that never parses freezes dstar_space and
            // stalls TX forever with every send reporting success, so
            // the consumer must be able to see that happening.
            Err(e) => self.protocol_violation(MMDVM_GET_STATUS, &e.to_string()),
        }
    }

    /// Emit [`Event::ProtocolViolation`] for a frame whose payload
    /// doesn't match its command's wire layout, with a matching
    /// `warn` log.
    fn protocol_violation(&mut self, command: u8, detail: &str) {
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

    /// Deliver an event to the consumer without ever blocking the
    /// loop.
    ///
    /// The event channel is bounded; blocking on a full channel would
    /// deadlock a single-task consumer that is awaiting a command
    /// reply while the channel is full (the consumer waits on the
    /// loop, the loop waits on the consumer). The reference instead
    /// decouples modem I/O from its consumer with fixed-size ring
    /// buffers that lose data when full. We mirror that: excess
    /// events are dropped and counted.
    fn emit_event(&mut self, event: Event) {
        match self.event_tx.try_send(event) {
            Ok(()) => {
                if self.dropped_events > 0 {
                    tracing::warn!(
                        target: "mmdvm::tokio_shell",
                        dropped = self.dropped_events,
                        "event channel recovered; events were dropped while it was full"
                    );
                    self.dropped_events = 0;
                }
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                if self.dropped_events == 0 {
                    tracing::warn!(
                        target: "mmdvm::tokio_shell",
                        "event channel full; dropping events until the consumer catches up"
                    );
                }
                self.dropped_events += 1;
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::debug!(
                    target: "mmdvm::tokio_shell",
                    "event consumer dropped; suppressing further events"
                );
            }
        }
    }

    /// Parse a D-STAR header payload and emit the corresponding
    /// event.
    fn emit_dstar_header(&mut self, payload: &[u8]) {
        if let Ok(bytes) = <[u8; 41]>::try_from(payload) {
            self.emit_event(Event::DStarHeaderRx { bytes });
        } else {
            self.protocol_violation(
                MMDVM_DSTAR_HEADER,
                &format!(
                    "D-STAR header payload is {} bytes, expected 41",
                    payload.len()
                ),
            );
        }
    }

    /// Parse a D-STAR voice data payload and emit the corresponding
    /// event.
    fn emit_dstar_data(&mut self, payload: &[u8]) {
        if let Ok(bytes) = <[u8; 12]>::try_from(payload) {
            self.emit_event(Event::DStarDataRx { bytes });
        } else {
            self.protocol_violation(
                MMDVM_DSTAR_DATA,
                &format!(
                    "D-STAR data payload is {} bytes, expected 12",
                    payload.len()
                ),
            );
        }
    }

    /// Decode a debug payload and emit it as [`Event::Debug`].
    ///
    /// `command` selects the level: DEBUG1..DEBUG5 map to 1..5, and
    /// `MMDVM_DEBUG_DUMP` uses level 0 as a sentinel for "this is a
    /// raw hex dump rather than readable text".
    fn emit_debug(&mut self, command: u8, payload: &[u8]) {
        let level = match command {
            MMDVM_DEBUG1 => 1,
            MMDVM_DEBUG2 => 2,
            MMDVM_DEBUG3 => 3,
            MMDVM_DEBUG4 => 4,
            MMDVM_DEBUG5 => 5,
            _ => 0,
        };
        let text = String::from_utf8_lossy(payload)
            .trim_end_matches('\0')
            .trim_end()
            .to_owned();
        self.emit_event(Event::Debug { level, text });
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
            let wire = MmdvmFrame::with_payload(frame.command, frame.payload);
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
    async fn write_frame(&mut self, frame: &MmdvmFrame) -> Result<(), ShellError> {
        let bytes = encode_frame(frame)?;
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            len = bytes.len(),
            cmd = format!("0x{:02X}", frame.command),
            "write_frame: awaiting transport.write_all"
        );
        let write = async {
            self.transport.write_all(&bytes).await?;
            self.transport.flush().await
        };
        match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Ok(result) => result?,
            Err(_elapsed) => {
                return Err(ShellError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("transport write timed out after {WRITE_TIMEOUT:?}"),
                )));
            }
        }
        tracing::trace!(target: "mmdvm::hang_hunt", "write_frame: flushed");
        Ok(())
    }
}
