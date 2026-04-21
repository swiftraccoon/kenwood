// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Tokio event loop driving a sans-io MMDVM codec over any
//! [`AsyncRead`]+[`AsyncWrite`] transport.
//!
//! Lifecycle:
//! 1. Send `GetVersion` + `GetStatus` immediately to learn the
//!    protocol version and initial FIFO depths.
//! 2. Enter the main `tokio::select!`:
//!    - receive from [`Command`] channel (handle → loop)
//!    - read inbound bytes from the transport
//!    - 250 ms periodic `GetStatus` poll (matches `MMDVMHost`'s
//!      `m_statusTimer(1000, 0, 250)` at `Modem.cpp:245`)
//!    - 10 ms playout tick to drain the [`TxQueue`] into the wire
//!      when modem reports slot space (`Modem.cpp:247`)
//! 3. Loop exits on consumer drop, `Shutdown` command, or a fatal
//!    transport error.

// `ModemLoop` is `pub(crate)` because the handle in `handle.rs`
// needs to reference it from a sibling submodule, but it must not be
// part of the crate's public API.
#![expect(
    clippy::redundant_pub_crate,
    reason = "ModemLoop is crate-internal infrastructure; pub(crate) documents the intent"
)]

use std::time::Duration;

use mmdvm_core::{
    MMDVM_ACK, MMDVM_DEBUG_DUMP, MMDVM_DEBUG1, MMDVM_DEBUG2, MMDVM_DEBUG3, MMDVM_DEBUG4,
    MMDVM_DEBUG5, MMDVM_DSTAR_DATA, MMDVM_DSTAR_EOT, MMDVM_DSTAR_HEADER, MMDVM_DSTAR_LOST,
    MMDVM_GET_STATUS, MMDVM_GET_VERSION, MMDVM_NAK, MMDVM_SERIAL_DATA, MMDVM_SET_MODE,
    MMDVM_TRANSPARENT, MmdvmFrame, ModemStatus, NakReason, VersionResponse, decode_frame,
    encode_frame,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::error::ShellError;
use crate::transport::Transport;

use super::{Command, Event, TxQueue};

/// Period between automatic `GetStatus` polls.
///
/// Mirrors `m_statusTimer(1000, 0, 250)` in `ref/MMDVMHost/Modem.cpp:245`.
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Period between TX-queue playout drains.
///
/// Mirrors `m_playoutTimer(1000, 0, 10)` in `ref/MMDVMHost/Modem.cpp:247`.
const PLAYOUT_INTERVAL: Duration = Duration::from_millis(10);

/// RX buffer grow-as-needed chunk size, aligned with maximum MMDVM
/// frame length (255).
const RX_READ_CHUNK: usize = 512;

/// Maximum retained RX buffer capacity — guards against a malformed
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
            // TH-D75 and newer MMDVMHost firmwares speak v2 — assume
            // that until the first `VersionResponse` corrects us.
            protocol_version: 2,
            shutting_down: false,
        }
    }

    /// Run the loop until it exits. Returns the owned transport so
    /// callers can recover it after a clean shutdown.
    ///
    /// On error the transport is dropped along with the loop state,
    /// since a failed transport is not useful to recover.
    pub(crate) async fn run(mut self) -> Result<T, ShellError> {
        let result = self.run_inner().await;
        match &result {
            Ok(()) => tracing::debug!(
                target: "mmdvm::tokio_shell",
                "modem loop exited cleanly"
            ),
            Err(e) => tracing::warn!(
                target: "mmdvm::tokio_shell",
                error = %e,
                "modem loop exited with error"
            ),
        }
        result.map(|()| self.transport)
    }

    async fn run_inner(&mut self) -> Result<(), ShellError> {
        // Initial handshake — send GetVersion, then GetStatus, so the
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
        // Prefer "skip if we fall behind" over burst-catchup — if the
        // runtime is slow we don't want a flood of back-to-back status
        // polls.
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        playout_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            if self.shutting_down && self.tx_queue.is_empty() {
                tracing::debug!(
                    target: "mmdvm::tokio_shell",
                    "shutdown complete; exiting loop"
                );
                return Ok(());
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

                read = self.transport.read(&mut read_chunk) => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: transport.read fired");
                    match read {
                        Ok(0) => {
                            tracing::debug!(
                                target: "mmdvm::tokio_shell",
                                "transport EOF; exiting loop"
                            );
                            emit_event(&self.event_tx, Event::TransportClosed).await;
                            return Ok(());
                        }
                        Ok(n) => {
                            if let Some(slice) = read_chunk.get(..n) {
                                self.rx_buffer.extend_from_slice(slice);
                            }
                            self.drain_rx().await?;
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

                _ = status_tick.tick() => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: status_tick fired");
                    if !self.shutting_down {
                        self.write_frame(&MmdvmFrame::new(MMDVM_GET_STATUS)).await?;
                    }
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: status_tick handled");
                }

                _ = playout_tick.tick() => {
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: playout_tick fired");
                    self.drain_tx_queue().await?;
                    tracing::trace!(target: "mmdvm::hang_hunt", "select: playout_tick handled");
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
                let result = self.write_frame(&frame).await;
                let _send_result = reply.send(result);
            }
            Command::SendDStarHeader { bytes, reply } => {
                self.tx_queue.push_dstar_header(bytes);
                let _send_result = reply.send(Ok(()));
            }
            Command::SendDStarData { bytes, reply } => {
                self.tx_queue.push_dstar_data(bytes);
                let _send_result = reply.send(Ok(()));
            }
            Command::SendDStarEot { reply } => {
                self.tx_queue.push_dstar_eot();
                let _send_result = reply.send(Ok(()));
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
                let _send_result = reply.send(());
            }
        }
        Ok(())
    }

    /// Walk the RX buffer, decoding every complete frame currently
    /// available.
    async fn drain_rx(&mut self) -> Result<(), ShellError> {
        loop {
            match decode_frame(&self.rx_buffer) {
                Ok(Some((frame, consumed))) => {
                    // Drop the consumed prefix. `drain` returns an
                    // iterator that clears the range when dropped —
                    // we consume it immediately with `for _ in`.
                    drop(self.rx_buffer.drain(..consumed));
                    self.dispatch_frame(frame).await?;
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
                    return Ok(());
                }
                Err(e) => {
                    // Silent-death prevention: decode errors are
                    // dropped as diagnostics, not propagated —
                    // propagating via `?` would kill the whole
                    // session loop on a single malformed byte. We
                    // also resync by consuming one byte so we don't
                    // loop forever on the same junk.
                    tracing::debug!(
                        target: "mmdvm::tokio_shell",
                        error = %e,
                        "decoder rejected RX bytes; resyncing"
                    );
                    if !self.rx_buffer.is_empty() {
                        let _discarded = self.rx_buffer.remove(0);
                    }
                }
            }
        }
    }

    /// Dispatch a decoded frame to the appropriate event variant.
    async fn dispatch_frame(&mut self, frame: MmdvmFrame) -> Result<(), ShellError> {
        match frame.command {
            MMDVM_GET_VERSION => self.handle_version(&frame.payload).await,
            MMDVM_GET_STATUS => self.handle_status(&frame.payload).await,
            MMDVM_ACK => {
                let cmd = frame.payload.first().copied().unwrap_or(0);
                emit_event(&self.event_tx, Event::Ack { command: cmd }).await;
            }
            MMDVM_NAK => {
                let cmd = frame.payload.first().copied().unwrap_or(0);
                let reason = NakReason::from_byte(frame.payload.get(1).copied().unwrap_or(0));
                emit_event(
                    &self.event_tx,
                    Event::Nak {
                        command: cmd,
                        reason,
                    },
                )
                .await;
            }
            MMDVM_DSTAR_HEADER => emit_dstar_header(&self.event_tx, &frame.payload).await,
            MMDVM_DSTAR_DATA => emit_dstar_data(&self.event_tx, &frame.payload).await,
            MMDVM_DSTAR_LOST => {
                emit_event(&self.event_tx, Event::DStarLost).await;
            }
            MMDVM_DSTAR_EOT => {
                emit_event(&self.event_tx, Event::DStarEot).await;
            }
            MMDVM_DEBUG1 | MMDVM_DEBUG2 | MMDVM_DEBUG3 | MMDVM_DEBUG4 | MMDVM_DEBUG5
            | MMDVM_DEBUG_DUMP => {
                emit_debug(&self.event_tx, frame.command, &frame.payload).await;
            }
            MMDVM_SERIAL_DATA => {
                emit_event(&self.event_tx, Event::SerialData(frame.payload)).await;
            }
            MMDVM_TRANSPARENT => {
                emit_event(&self.event_tx, Event::TransparentData(frame.payload)).await;
            }
            other => {
                emit_event(
                    &self.event_tx,
                    Event::UnhandledResponse {
                        command: other,
                        payload: frame.payload,
                    },
                )
                .await;
            }
        }
        Ok(())
    }

    /// Handle an `MMDVM_GET_VERSION` response payload.
    async fn handle_version(&mut self, payload: &[u8]) {
        match VersionResponse::parse(payload) {
            Ok(v) => {
                self.protocol_version = v.protocol;
                emit_event(&self.event_tx, Event::Version(v)).await;
            }
            Err(e) => tracing::debug!(
                target: "mmdvm::tokio_shell",
                error = %e,
                "malformed GetVersion response; swallowing"
            ),
        }
    }

    /// Handle an `MMDVM_GET_STATUS` response payload.
    async fn handle_status(&mut self, payload: &[u8]) {
        let parsed = if self.protocol_version >= 2 {
            ModemStatus::parse_v2(payload)
        } else {
            ModemStatus::parse_v1(payload)
        };
        match parsed {
            Ok(s) => {
                self.dstar_space = s.dstar_space;
                emit_event(&self.event_tx, Event::Status(s)).await;
            }
            Err(e) => tracing::debug!(
                target: "mmdvm::tokio_shell",
                error = %e,
                "malformed GetStatus response; swallowing"
            ),
        }
    }

    /// Drain as many queued D-STAR frames as the modem's reported
    /// FIFO slot count can absorb. Each successful write decrements
    /// the local `dstar_space` estimate; the real number is
    /// recalibrated on every status response.
    async fn drain_tx_queue(&mut self) -> Result<(), ShellError> {
        while let Some(frame) = self.tx_queue.pop_if_space_allows(self.dstar_space) {
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
        tracing::trace!(target: "mmdvm::hang_hunt", "drain_tx_queue: queue empty");
        Ok(())
    }

    /// Encode `frame` and push the bytes to the transport.
    async fn write_frame(&mut self, frame: &MmdvmFrame) -> Result<(), ShellError> {
        let bytes = encode_frame(frame)?;
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            len = bytes.len(),
            cmd = format!("0x{:02X}", frame.command),
            "write_frame: awaiting transport.write_all"
        );
        self.transport.write_all(&bytes).await?;
        tracing::trace!(target: "mmdvm::hang_hunt", "write_frame: write_all done, awaiting flush");
        self.transport.flush().await?;
        tracing::trace!(target: "mmdvm::hang_hunt", "write_frame: flushed");
        Ok(())
    }
}

/// Send an event, logging and swallowing failure if the consumer
/// channel has been dropped. Kept as a free function so the
/// auto-`Send` checker doesn't require `&ModemLoop<T>: Sync`.
async fn emit_event(sender: &mpsc::Sender<Event>, event: Event) {
    // Hang-hunt: if the REPL stops consuming mmdvm events, this
    // send will eventually block on a full channel (cap 256) and
    // the entire modem loop freezes. A matched "awaiting" / "sent"
    // pair is healthy; "awaiting" with no "sent" for hundreds of
    // ms points directly at event-channel backpressure.
    let variant = std::mem::discriminant(&event);
    tracing::trace!(
        target: "mmdvm::hang_hunt",
        remaining_cap = sender.capacity(),
        ?variant,
        "emit_event: awaiting event_tx.send"
    );
    if sender.send(event).await.is_err() {
        tracing::debug!(
            target: "mmdvm::tokio_shell",
            "event consumer dropped; suppressing further events"
        );
    } else {
        tracing::trace!(target: "mmdvm::hang_hunt", "emit_event: sent");
    }
}

/// Parse a D-STAR header payload and emit the corresponding event.
///
/// Unexpected payload lengths are swallowed as a debug diagnostic to
/// match the sans-io core's leniency rules.
async fn emit_dstar_header(sender: &mpsc::Sender<Event>, payload: &[u8]) {
    if let Ok(bytes) = <[u8; 41]>::try_from(payload) {
        emit_event(sender, Event::DStarHeaderRx { bytes }).await;
    } else {
        tracing::debug!(
            target: "mmdvm::tokio_shell",
            len = payload.len(),
            "D-STAR header with unexpected payload length; dropping"
        );
    }
}

/// Parse a D-STAR voice data payload and emit the corresponding event.
async fn emit_dstar_data(sender: &mpsc::Sender<Event>, payload: &[u8]) {
    if let Ok(bytes) = <[u8; 12]>::try_from(payload) {
        emit_event(sender, Event::DStarDataRx { bytes }).await;
    } else {
        tracing::debug!(
            target: "mmdvm::tokio_shell",
            len = payload.len(),
            "D-STAR data with unexpected payload length; dropping"
        );
    }
}

/// Decode a debug payload and emit it as [`Event::Debug`].
///
/// `command` selects the level: DEBUG1..DEBUG5 map to 1..5, and
/// `MMDVM_DEBUG_DUMP` uses level 0 as a sentinel for "this is a raw
/// hex dump rather than readable text".
async fn emit_debug(sender: &mpsc::Sender<Event>, command: u8, payload: &[u8]) {
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
    emit_event(sender, Event::Debug { level, text }).await;
}
