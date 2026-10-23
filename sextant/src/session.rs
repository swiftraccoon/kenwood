// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Async session task: owns the `AsyncSession<P>` handles and
//! brokers commands from / events to the GUI.
//!
//! The GUI never touches `dstar-gateway` types directly. It sends
//! [`SessionCommand`]s to this task (Connect, Disconnect, transmit)
//! and receives [`SessionEvent`]s back, which it renders into the
//! status indicator + event log.
//!
//! TX audio flows over the same [`SessionCommand`] channel. When the
//! operator keys PTT the audio worker sends `StartTx`, then a
//! `TxFrame(VoiceFrame)` per encoded frame while PTT is held, then
//! `EndTx` on release; this task turns those into the header,
//! `send_voice` calls, and EOT on the wire. `TxSilence` sends 100
//! silence frames at 20 ms intervals, followed by EOT.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::{AudioCommand, AudioHandle};
use crate::geo::{GpsPosition, MAX_GPS_SENTENCE_BYTES, parse_gps_sentence};
use crate::hosts::DirectoryUpdate;
use dstar_gateway::tokio_shell::{
    AnyAsyncSession, AnyEvent, AsyncSession, ConnectError, ShellError, drive_connecting,
    fresh_stream_id,
};
use dstar_gateway_core::codec::dplus::DPlusHost;
use dstar_gateway_core::header::{DstarHeader, RelayRouting};
use dstar_gateway_core::session::client::{
    Configured, DExtra, DPlus, Dcs, Session, VoiceEndReason,
};
use dstar_gateway_core::slowdata::{SlowDataTextCollector, SlowDataTextMessage};
use dstar_gateway_core::types::{DcsReflector, FrameSequence, RoutingCallsign, StreamId, Suffix};
use dstar_gateway_core::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::sync::watch;

pub(crate) use self::config::{ConnectConfig, ConnectionIdentity, Endpoint};
use self::config::{ConnectionProtocol, local_bind_address};
pub(crate) use self::control::{ControlState, SessionEvents, SessionHandle};

mod config;
mod control;

/// Pair an active runtime session with the configuration it was
/// established with. The cfg fields (operator callsign, modules,
/// reflector callsign) are needed to build correctly-routed `rpt1` /
/// `rpt2` headers at TX time; see [`DstarHeader::for_relay`] for the
/// `rpt[7]` module-byte invariant that strict reflectors enforce.
struct ActiveSession {
    runtime: RuntimeSession,
    cfg: ConnectConfig,
}

/// Connection failure retained through resolution, socket and protocol layers.
#[derive(Debug)]
enum ConnectFailure {
    Resolve(std::io::Error),
    Bind(std::io::Error),
    NoAddress,
    Deadline,
    Core(dstar_gateway_core::error::Error),
    Handshake(ConnectError),
}

impl std::fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(error) => write!(f, "resolve reflector: {error}"),
            Self::Bind(error) => write!(f, "bind reflector socket: {error}"),
            Self::NoAddress => f.write_str("reflector hostname returned no address"),
            Self::Deadline => f.write_str("connection exceeded its total deadline"),
            Self::Core(error) => error.fmt(f),
            Self::Handshake(error) => f.write_str(&describe_dplus_handshake_error(error)),
        }
    }
}

impl std::error::Error for ConnectFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Resolve(error) | Self::Bind(error) => Some(error),
            Self::Core(error) => Some(error),
            Self::Handshake(error) => Some(error),
            Self::NoAddress | Self::Deadline => None,
        }
    }
}

/// Command emitted by the GUI (or audio worker), consumed by the
/// session task.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    /// Establish a session with the given reflector.
    Connect(ConnectConfig),
    /// Refresh the directory with one bounded, cancellable authentication.
    RefreshDirectory(RoutingCallsign),
    /// Send 100 AMBE silence frames at 20 ms intervals, then EOT.
    TxSilence,
    /// Begin a TX stream. The audio worker sends this when the
    /// operator keys PTT. The session task generates a fresh stream-id, sends
    /// the header, and starts accepting `TxFrame`s.
    StartTx,
    /// One encoded voice frame from the audio worker. Ignored if no
    /// TX stream is active.
    TxFrame(VoiceFrame),
    /// End the active TX stream: emits EOT and clears state.
    EndTx,
}

/// Routing fields from an RX stream's D-STAR header, display-trimmed.
#[derive(Debug, Clone, Default)]
pub(crate) struct RxRoute {
    /// Origin suffix (often the radio model, e.g. `D75`).
    pub(crate) suffix: String,
    /// Destination callsign (usually `CQCQCQ`).
    pub(crate) to: String,
    /// Access repeater (RPT1).
    pub(crate) rpt1: String,
    /// Gateway repeater (RPT2).
    pub(crate) rpt2: String,
}

impl RxRoute {
    /// Extract the display-trimmed routing fields from a header.
    fn from_header(header: &DstarHeader) -> Self {
        Self {
            suffix: header.my_suffix.to_string().trim().to_owned(),
            to: header.ur_call.to_string().trim().to_owned(),
            rpt1: header.rpt1.to_string().trim().to_owned(),
            rpt2: header.rpt2.to_string().trim().to_owned(),
        }
    }
}

/// Current lifecycle state of the session, summarised for the GUI.
#[derive(Debug, Clone)]
pub(crate) enum ConnStatus {
    /// Idle, not connected.
    Disconnected,
    /// Handshake in progress.
    Connecting { peer: Endpoint },
    /// Connected, showing the remote reflector + module.
    Connected {
        /// Reflector callsign (display-form).
        reflector: String,
        /// Reflector module letter.
        module: char,
    },
    /// Teardown in progress.
    Disconnecting,
}

/// Event emitted by the session task, consumed by the GUI.
#[derive(Debug)]
pub(crate) enum SessionEvent {
    /// Connection state change.
    Status(ConnStatus),
    /// Informational log line to append.
    Log(String),
    /// An incoming voice stream started.
    VoiceStart {
        /// Stream identifier.
        stream_id: StreamId,
        /// Source callsign (if known).
        from: String,
        /// Routing fields from the stream's D-STAR header.
        route: RxRoute,
    },
    /// An incoming voice stream ended.
    VoiceEnd {
        /// Stream identifier.
        stream_id: StreamId,
        /// Number of voice frames observed.
        frames: u32,
        /// Reason reported by the core state machine.
        reason: VoiceEndReason,
    },
    /// Hard error: the session task is returning to Disconnected.
    Error(String),
    /// A complete 20-character D-STAR slow-data text message has been
    /// assembled from incoming voice frames.
    SlowDataMessage {
        /// Source stream-id (so the GUI can correlate with `VoiceStart`).
        stream_id: StreamId,
        /// Exact 20-byte wire message with a separately validated text view.
        message: SlowDataTextMessage,
    },
    /// A GPS position decoded from incoming slow data.
    GpsPosition {
        /// Source stream-id (correlates with `VoiceStart`).
        stream_id: StreamId,
        /// Checked geographic position.
        position: GpsPosition,
    },
    /// Reflector hosts learned from the `DPlus` auth server, forwarded
    /// so the GUI can merge them into the reflector directory.
    ReflectorHosts(Vec<DPlusHost>),
    /// Result of the single in-progress directory request.
    DirectoryUpdate(DirectoryUpdate),
    /// Per-stream RX link-quality counters. Emitted when a stream
    /// ends (final values) and once per second while one is active.
    RxStats {
        /// Voice frames received and played.
        received: u32,
        /// Frames lost to sequence gaps.
        lost: u32,
        /// Frames dropped for arriving late.
        late: u32,
    },
    /// Periodic (1 Hz) link-health sample while connected.
    LinkHealth {
        /// Seconds since the last datagram arrived from the
        /// reflector (keepalives included).
        last_heard_secs: f32,
    },
}

/// Protocol-erased reflector session; the GUI-facing event mapping is
/// [`RuntimeEvent::from`] over the erased [`AnyEvent`].
type RuntimeSession = AnyAsyncSession;

/// One owned directory operation, polled alongside session control.
type DirectoryFetch = std::pin::Pin<Box<dyn Future<Output = DirectoryUpdate> + Send>>;

/// Lightweight state kept while an outgoing voice stream is live.
///
/// `seq` is the NEXT wire seq to use, always in `0..21`.  D-STAR
/// encodes seq in the low 6 bits of a wire byte with bit 6 (`0x40`)
/// reserved as the EOT flag.  Any value `>= 0x40` would set that bit
/// mid-stream and the reflector would treat it as EOT, silently
/// ending the stream.  Wrapping at the superframe length (21) is
/// both spec-correct and keeps the stream well clear of the EOT bit.
#[derive(Debug)]
struct TxStream {
    sid: StreamId,
    seq: FrameSequence,
}

/// D-STAR superframe length: seq wraps mod this value.
const SUPERFRAME_LEN: u8 = 21;

/// Longest sequence gap concealed frame-by-frame: ten frames or 200 ms.
/// Larger gaps resynchronize without generating concealment frames.
const MAX_CONCEAL: u8 = 10;

/// Frames within this distance BEHIND the expected sequence are
/// treated as late/reordered duplicates and dropped. The wire
/// counter is mod-21, so "slightly behind" and "far ahead" are the
/// same number, and this window is how the two are told apart.
const REORDER_WINDOW: u8 = 3;

/// Where an arriving frame's sequence lands relative to expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GapClass {
    /// Exactly the expected sequence.
    InOrder,
    /// This many frames were lost: conceal them, then play the frame.
    Conceal(u8),
    /// Too many frames lost to conceal: resync and play the frame.
    Dropout(u8),
    /// Behind the expected sequence: drop the frame, never
    /// double-play.
    Late,
}

/// Classify `seq` against the `expected` next sequence on the mod-21
/// superframe ring.
const fn classify_gap(expected: u8, seq: u8) -> GapClass {
    let gap = (seq + SUPERFRAME_LEN - expected) % SUPERFRAME_LEN;
    if gap == 0 {
        GapClass::InOrder
    } else if gap <= MAX_CONCEAL {
        GapClass::Conceal(gap)
    } else if gap >= SUPERFRAME_LEN - REORDER_WINDOW {
        GapClass::Late
    } else {
        GapClass::Dropout(gap)
    }
}

#[derive(Debug)]
enum RuntimeEvent {
    VoiceStart {
        stream_id: StreamId,
        my_call: String,
        route: RxRoute,
    },
    VoiceEnd {
        stream_id: StreamId,
        reason: VoiceEndReason,
    },
    VoiceFrame {
        /// Wire seq (0..21 within a superframe). Needed by the
        /// slow-data text collector to align half-block boundaries
        /// and treat seq=0 as a resync.
        seq: u8,
        frame: VoiceFrame,
    },
    /// Reflector-side disconnect: keepalive timeout, link rejection,
    /// or an unlink ACK after the local disconnect. Surfaced explicitly
    /// (not folded into `Other`) so the run loop can clear `session`
    /// and tell the GUI the link is dead, instead of leaving a stale
    /// `Connected` indicator while every TX silently fails.
    Disconnected { reason: String },
    /// Keepalive echo from the reflector. Liveness is already
    /// surfaced through the activity watch (the status line's
    /// "heard Ns ago"), so these stay out of the event log; at
    /// poll cadence they'd drown every operator-relevant line.
    Keepalive,
    /// Anything else, logged as a debug line and not surfaced to the
    /// GUI explicitly.
    Other(String),
}

impl From<AnyEvent> for RuntimeEvent {
    fn from(ev: AnyEvent) -> Self {
        match ev {
            AnyEvent::VoiceStart {
                stream_id, header, ..
            } => Self::VoiceStart {
                stream_id,
                my_call: header.my_call.to_string(),
                route: RxRoute::from_header(&header),
            },
            AnyEvent::VoiceEnd { stream_id, reason } => Self::VoiceEnd { stream_id, reason },
            AnyEvent::VoiceFrame { seq, frame, .. } => Self::VoiceFrame { seq, frame },
            AnyEvent::Disconnected { reason } => Self::Disconnected {
                reason: format!("{reason:?}"),
            },
            AnyEvent::PollEcho { .. } => Self::Keepalive,
            other => Self::Other(format!("{other:?}")),
        }
    }
}

/// A GPS fragment or sentence rejected before position publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpsAssemblyError {
    SequenceGap,
    BlockLength,
    InvalidByte,
    Overflow,
    InvalidSentence,
}

impl std::fmt::Display for GpsAssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::SequenceGap => "GPS sequence gap; partial sentence discarded",
            Self::BlockLength => "GPS block length is invalid; partial sentence discarded",
            Self::InvalidByte => "GPS byte is invalid; discarding through the line terminator",
            Self::Overflow => {
                "GPS sentence exceeded 256 bytes; discarding through the line terminator"
            }
            Self::InvalidSentence => "GPS sentence was rejected; awaiting another sentence",
        })
    }
}

/// Pairs adjacent odd/even fragments and bounds one ASCII GPS sentence.
///
/// `ircDDBGateway/Common/APRSCollector.cpp::writeData` pairs six-byte blocks;
/// `SlowDataEncoder.cpp::setGPSData` encodes their one-to-five-byte payload width.
/// A zero low nibble retains the collector's fixed-five-byte receive form.
#[derive(Debug, Default)]
struct GpsSlowData {
    /// In-progress 6-byte block (first half in [0..3], second in [3..6]).
    block: [u8; 6],
    first_sequence: Option<u8>,
    last_sequence: Option<u8>,
    /// Accumulated GPS sentence bytes awaiting a terminator.
    buffer: Vec<u8>,
    discarding: bool,
    error: Option<GpsAssemblyError>,
}

impl GpsSlowData {
    /// Drop all partial state; call on stream boundaries.
    fn reset(&mut self) {
        self.block = [0u8; 6];
        self.first_sequence = None;
        self.last_sequence = None;
        self.buffer.clear();
        self.discarding = false;
        self.error = None;
    }

    /// Feed one voice frame's slow-data fragment at superframe `seq`.
    /// Returns a [`GpsPosition`] when a sentence completes.
    fn push(&mut self, fragment: [u8; 3], seq: u8) -> Option<GpsPosition> {
        if seq > 20 {
            self.reject(GpsAssemblyError::SequenceGap, false);
            return None;
        }
        if self
            .last_sequence
            .is_some_and(|last| (last + 1) % 21 != seq)
        {
            if !self.buffer.is_empty()
                || self.discarding
                || (self.first_sequence.is_some() && self.block[0] & 0xF0 == 0x30)
            {
                self.reject(GpsAssemblyError::SequenceGap, false);
            } else {
                self.first_sequence = None;
            }
        }
        self.last_sequence = Some(seq);
        if seq == 0 {
            self.first_sequence = None;
            return None;
        }
        let plain = dstar_gateway_core::slowdata::descramble(fragment);
        if seq.is_multiple_of(2) {
            if self.first_sequence.take() != Some(seq - 1) {
                return None;
            }
            self.block[3..6].copy_from_slice(&plain);
            return self.commit_block();
        }
        self.block[0..3].copy_from_slice(&plain);
        self.first_sequence = Some(seq);
        None
    }

    fn reject(&mut self, error: GpsAssemblyError, discarding: bool) {
        self.buffer.clear();
        self.first_sequence = None;
        self.discarding = discarding;
        self.error = Some(error);
    }

    const fn take_error(&mut self) -> Option<GpsAssemblyError> {
        self.error.take()
    }

    /// A complete 6-byte block assembled: route GPS payloads.
    fn commit_block(&mut self) -> Option<GpsPosition> {
        // High nibble 0x30 == GPS NMEA passthrough.
        if self.block[0] & 0xF0 != 0x30 {
            return None;
        }
        let length = match self.block[0] & 0x0F {
            0 => 5,
            count @ 1..=5 => usize::from(count),
            _ => {
                self.reject(GpsAssemblyError::BlockLength, true);
                return None;
            }
        };
        let mut position = None;
        for index in 1..=length {
            let byte = *self.block.get(index)?;
            if matches!(byte, b'\r' | b'\n') {
                if !self.discarding && !self.buffer.is_empty() {
                    position = std::str::from_utf8(&self.buffer)
                        .ok()
                        .and_then(parse_gps_sentence);
                    if position.is_none() {
                        self.error = Some(GpsAssemblyError::InvalidSentence);
                    }
                }
                self.buffer.clear();
                self.discarding = false;
            } else if !self.discarding {
                if self.buffer.is_empty() && byte != b'$' {
                    continue;
                }
                if !(b' '..=b'~').contains(&byte) {
                    self.reject(GpsAssemblyError::InvalidByte, true);
                } else if self.buffer.len() >= MAX_GPS_SENTENCE_BYTES - 1 {
                    self.reject(GpsAssemblyError::Overflow, true);
                } else {
                    self.buffer.push(byte);
                }
            }
        }
        position
    }
}

/// Mutable state carried across `decide_runtime_event` calls.
///
/// Pulled out of the `run` loop so the decision logic is a pure
/// function that unit tests exercise without a real reflector or
/// audio device.
#[derive(Debug, Default)]
struct EventState {
    /// Frames observed on the currently-active incoming stream.
    /// Reset on `VoiceStart` and `VoiceEnd`.
    rx_frame_count: u32,
    /// Reassembles 20-char D-STAR slow-data text messages from the
    /// 3-byte slow-data half-blocks in each incoming voice frame.
    /// Reset on `VoiceStart` and reflector-driven `Disconnected` so
    /// half-block state from the previous stream can't leak into the
    /// next message.
    slow_data: SlowDataTextCollector,
    /// Reassembles incoming GPS/DPRS positions from slow data.
    gps: GpsSlowData,
    /// Stream-id of the currently-active incoming stream, kept so the
    /// `SlowDataMessage` event can correlate with the originating
    /// `VoiceStart` in the GUI log.
    rx_stream_id: Option<StreamId>,
    /// Next expected wire seq (0..21) on the active RX stream;
    /// `None` until the stream's first frame arrives (and between
    /// streams).
    expected_seq: Option<u8>,
    /// Frames lost to sequence gaps this stream (concealed or
    /// skipped in a dropout resync).
    frames_lost: u32,
    /// Frames dropped for arriving behind the expected sequence.
    frames_late: u32,
}

/// One decision the run loop should act on after processing a runtime
/// event. Decoupling decisions from execution keeps the matching
/// logic testable and the run loop's I/O linear.
#[derive(Debug)]
enum EventDecision {
    /// Forward a `SessionEvent` to the GUI.
    EmitSessionEvent(SessionEvent),
    /// Tell the audio worker a new RX stream is starting (decoder reset).
    AudioRxStart,
    /// Hand a decoded voice frame to the audio worker.
    AudioRxFrame(VoiceFrame),
    /// A frame was lost upstream: tell the audio worker to
    /// synthesize one concealment frame.
    AudioRxLost,
    /// The RX stream ended: tell the audio worker to fade out and
    /// flush its held-back tail frame.
    AudioRxEnd,
    /// Clear the active session, because the reflector dropped it or the
    /// underlying transport died. The run loop sets `session = None`.
    ClearSession,
}

#[cfg(test)]
impl PartialEq for EventDecision {
    fn eq(&self, other: &Self) -> bool {
        // SessionEvent doesn't impl PartialEq because it carries
        // free-form Strings. For tests we compare the Debug repr.
        format!("{self:?}") == format!("{other:?}")
    }
}

/// Apply the sequence-gap policy for one arriving frame: queue
/// concealment decisions for small gaps, resync silently on
/// dropouts, and update the loss/late counters.
///
/// Returns `false` when the frame is a late duplicate that the
/// caller must drop (the stream already played past its slot, so
/// playing it now would double-play 20 ms of audio).
fn apply_gap_policy(state: &mut EventState, seq: u8, decisions: &mut Vec<EventDecision>) -> bool {
    match state
        .expected_seq
        .map(|expected| classify_gap(expected, seq))
    {
        Some(GapClass::Late) => {
            state.frames_late = state.frames_late.saturating_add(1);
            return false;
        }
        Some(GapClass::Conceal(n)) => {
            state.frames_lost = state.frames_lost.saturating_add(u32::from(n));
            for _ in 0..n {
                decisions.push(EventDecision::AudioRxLost);
            }
        }
        Some(GapClass::Dropout(n)) => {
            // Too long to conceal without smearing one voice
            // posture across the hole: count it and resync.
            tracing::debug!(lost = n, seq, "RX dropout; resyncing without concealment");
            state.frames_lost = state.frames_lost.saturating_add(u32::from(n));
        }
        Some(GapClass::InOrder) | None => {}
    }
    true
}

/// Translate a runtime event into a list of decisions for the run
/// loop to execute. Pure function; mutates only the supplied
/// `EventState` counter and slow-data collector.
fn reject_frame_sequence(seq: u8, state: &mut EventState) -> Vec<EventDecision> {
    state.frames_late = state.frames_late.saturating_add(1);
    state.gps.reset();
    vec![EventDecision::EmitSessionEvent(SessionEvent::Log(format!(
        "Voice sequence {seq} is outside 0 through 20; frame discarded"
    )))]
}

fn decide_voice_frame(seq: u8, frame: VoiceFrame, state: &mut EventState) -> Vec<EventDecision> {
    if seq > 20 {
        return reject_frame_sequence(seq, state);
    }
    let mut decisions = Vec::new();
    if !apply_gap_policy(state, seq, &mut decisions) {
        return decisions;
    }
    state.expected_seq = Some((seq + 1) % SUPERFRAME_LEN);
    state.rx_frame_count = state.rx_frame_count.saturating_add(1);
    let slow = frame.slow_data;
    state.slow_data.push(slow, seq);
    let gps = state.gps.push(slow, seq);
    if let Some(error) = state.gps.take_error() {
        decisions.push(EventDecision::EmitSessionEvent(SessionEvent::Log(
            error.to_string(),
        )));
    }
    decisions.push(EventDecision::AudioRxFrame(frame));
    if let (Some(message), Some(stream_id)) = (state.slow_data.take_message(), state.rx_stream_id) {
        decisions.push(EventDecision::EmitSessionEvent(
            SessionEvent::SlowDataMessage { stream_id, message },
        ));
    }
    if let (Some(position), Some(stream_id)) = (gps, state.rx_stream_id) {
        decisions.push(EventDecision::EmitSessionEvent(SessionEvent::GpsPosition {
            stream_id,
            position,
        }));
    }
    decisions
}

fn decide_runtime_event(event: RuntimeEvent, state: &mut EventState) -> Vec<EventDecision> {
    match event {
        RuntimeEvent::VoiceStart {
            stream_id,
            my_call,
            route,
        } => {
            state.rx_frame_count = 0;
            state.rx_stream_id = Some(stream_id);
            state.expected_seq = None;
            state.frames_lost = 0;
            state.frames_late = 0;
            // Reset slow-data assembly so half-block state from a
            // previous stream can't bleed into the new message.
            state.slow_data.reset();
            state.gps.reset();
            vec![
                EventDecision::AudioRxStart,
                EventDecision::EmitSessionEvent(SessionEvent::VoiceStart {
                    stream_id,
                    from: my_call,
                    route,
                }),
            ]
        }
        RuntimeEvent::VoiceFrame { seq, frame } => decide_voice_frame(seq, frame, state),
        RuntimeEvent::VoiceEnd { stream_id, reason } => {
            let frames = state.rx_frame_count;
            let lost = state.frames_lost;
            let late = state.frames_late;
            state.rx_stream_id = None;
            state.rx_frame_count = 0;
            state.expected_seq = None;
            state.frames_lost = 0;
            state.frames_late = 0;
            // EOT: drop any partial slow-data half-blocks; the next
            // stream restarts assembly cleanly.
            state.slow_data.reset();
            state.gps.reset();
            vec![
                EventDecision::AudioRxEnd,
                EventDecision::EmitSessionEvent(SessionEvent::RxStats {
                    received: frames,
                    lost,
                    late,
                }),
                EventDecision::EmitSessionEvent(SessionEvent::VoiceEnd {
                    stream_id,
                    frames,
                    reason,
                }),
            ]
        }
        RuntimeEvent::Disconnected { reason } => {
            state.rx_stream_id = None;
            state.slow_data.reset();
            state.gps.reset();
            vec![
                // A link drop mid-stream must still flush the audio
                // worker's held-back tail frame.
                EventDecision::AudioRxEnd,
                EventDecision::EmitSessionEvent(SessionEvent::Log(format!(
                    "session disconnected by reflector: {reason}"
                ))),
                EventDecision::ClearSession,
                EventDecision::EmitSessionEvent(SessionEvent::Status(ConnStatus::Disconnected)),
            ]
        }
        RuntimeEvent::Keepalive => {
            tracing::trace!("reflector keepalive echo");
            Vec::new()
        }
        RuntimeEvent::Other(s) => vec![EventDecision::EmitSessionEvent(SessionEvent::Log(
            format!("event: {s}"),
        ))],
    }
}

/// Top-level session task entry point. Runs until `cmd_rx` closes.
#[expect(
    clippy::too_many_lines,
    reason = "main event loop; splitting the per-command arms into separate helpers would obscure the select! structure"
)]
pub(crate) async fn run(
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    mut terminal: watch::Receiver<ControlState>,
    evt_tx: SessionEvents,
    audio: AudioHandle,
) {
    let mut session: Option<ActiveSession> = None;
    // Per-event-decision state (RX frame counter). Lives outside the
    // inline match so `decide_runtime_event` (the testable seam) owns
    // the mutation.
    let mut rx_state = EventState::default();
    // Counts frames on the currently-transmitting OUTGOING stream;
    // reset between streams.
    let mut tx_frame_count: u32 = 0;
    // Active outgoing TX stream: `Some` between `StartTx` and `EndTx`.
    let mut tx_stream: Option<TxStream> = None;
    // Last config used to connect, replayed for auto-reconnect after
    // a reflector-driven drop. Cleared by an explicit `Disconnect`.
    let mut last_cfg: Option<ConnectConfig> = None;
    // Pending reconnect: (when to fire, attempt index). `None` when no
    // reconnect is scheduled.
    let mut reconnect_at: Option<(tokio::time::Instant, u8)> = None;
    // 1 Hz link-health cadence. Skipped ticks (e.g. during a slow
    // connect) don't burst-catch-up.
    let mut health_tick = tokio::time::interval(Duration::from_secs(1));
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut terminal_generation = 0;
    let mut directory_fetch: Option<DirectoryFetch> = None;

    loop {
        let request = *terminal.borrow_and_update();
        if request.generation != terminal_generation || request.shutdown {
            terminal_generation = request.generation;
            last_cfg = None;
            reconnect_at = None;
            directory_fetch = None;
            while cmd_rx.try_recv().is_ok() {}
            retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
            if request.shutdown {
                break;
            }
        }
        tokio::select! {
            biased;
            changed = terminal.changed() => {
                if changed.is_err() { break; }
            }
            update = next_directory_update(&mut directory_fetch), if directory_fetch.is_some() => {
                directory_fetch = None;
                let _sent = evt_tx.send(SessionEvent::DirectoryUpdate(update)).await;
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    SessionCommand::RefreshDirectory(callsign) => {
                        if directory_fetch.is_none() {
                            directory_fetch = Some(Box::pin(crate::hosts::fetch_auth_directory(callsign)));
                        } else {
                            let _sent = evt_tx.send(SessionEvent::Log("directory request already in progress".to_owned())).await;
                        }
                    }
                    SessionCommand::Connect(cfg) => {
                        if session.is_some() {
                            let _unused = evt_tx.send(SessionEvent::Log("already connected; ignoring Connect".into())).await;
                            continue;
                        }
                        let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Connecting { peer: cfg.endpoint.clone() })).await;
                        let result = tokio::select! {
                            biased;
                            _ = terminal.changed() => continue,
                            result = connect(&cfg) => result,
                        };
                        match result {
                            Ok((rs, auth, hosts)) => {
                                let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Connected {
                                    reflector: cfg.reflector_callsign.to_string(),
                                    module: cfg.reflector_module.as_char(),
                                })).await;
                                let _unused = evt_tx.send(SessionEvent::Log(format!(
                                    "connected to {} module {} via {:?}",
                                    cfg.reflector_callsign,
                                    cfg.reflector_module.as_char(),
                                    cfg.protocol,
                                ))).await;
                                let auth_note = match auth {
                                    AuthOutcome::NotApplicable => None,
                                    AuthOutcome::Authenticated => {
                                        Some("DPlus auth: ok".to_owned())
                                    }
                                    AuthOutcome::FellBack => Some(
                                        "DPlus auth: failed; connected with empty host list"
                                            .to_owned(),
                                    ),
                                };
                                if let Some(note) = auth_note {
                                    let _unused = evt_tx.send(SessionEvent::Log(note)).await;
                                }
                                if !hosts.is_empty() {
                                    let _unused = evt_tx
                                        .send(SessionEvent::ReflectorHosts(hosts))
                                        .await;
                                }
                                // Remember the config so a reflector-driven
                                // drop can replay it (if reconnect is on).
                                last_cfg = Some(cfg.clone());
                                reconnect_at = None;
                                audio.set_connected(Some(cfg.callsign));
                                session = Some(ActiveSession { runtime: rs, cfg });
                            }
                            Err(e) => {
                                let _unused = evt_tx.send(SessionEvent::Error(format!("connect failed: {e}"))).await;
                                let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                            }
                        }
                    }
                    SessionCommand::TxSilence => {
                        let Some(active) = session.as_mut() else {
                            let _unused = evt_tx.send(SessionEvent::Log("TX: not connected".into())).await;
                            continue;
                        };
                        if tx_stream.is_some() {
                            // Refuse silence test mid-PTT: interleaving two
                            // stream-ids on one wire confuses the reflector.
                            let _unused = evt_tx.send(SessionEvent::Log(
                                "TxSilence: PTT active; ignoring".into(),
                            )).await;
                            continue;
                        }
                        let result = tokio::select! {
                            biased;
                            _ = terminal.changed() => continue,
                            result = tx_silence(active, &mut tx_stream, &evt_tx) => result,
                        };
                        if let Err(e) = result {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TX error: {e}"))).await;
                            retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
                        }
                    }
                    SessionCommand::StartTx => {
                        let Some(active) = session.as_mut() else {
                            audio.set_connected(None);
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: not connected".into())).await;
                            continue;
                        };
                        if tx_stream.is_some() {
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: already transmitting; ignoring".into())).await;
                            continue;
                        }
                        match start_tx(active, &mut tx_stream).await {
                            Ok(sid) => {
                                let _unused = evt_tx.send(SessionEvent::Log(format!(
                                    "TX started sid=0x{:04X} my_call={}",
                                    sid.get(),
                                    active.cfg.callsign,
                                ))).await;
                                // Reset frame counter on stream start.
                                // `EndTx` only resets when both `session` and
                                // `tx_stream` are Some; if the session dropped
                                // mid-transmission the counter would otherwise
                                // leak into the next stream's logs. Mirrors the
                                // `rx_frame_count = 0` reset on VoiceStart.
                                tx_frame_count = 0;
                            }
                            Err(e) => {
                                let _unused = evt_tx.send(SessionEvent::Error(format!("StartTx: {e}"))).await;
                                retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
                            }
                        }
                    }
                    SessionCommand::TxFrame(frame) => {
                        let Some(active) = session.as_mut() else { continue };
                        let Some(ts) = tx_stream.as_mut() else { continue };
                        let seq = ts.seq;
                        tx_frame_count = tx_frame_count.saturating_add(1);
                        tracing::trace!(
                            sid = format_args!("{:#06X}", ts.sid.get()),
                            seq = seq.get(),
                            frame_num = tx_frame_count,
                            "TX voice frame"
                        );
                        if let Err(e) = active.runtime.send_voice(ts.sid, seq, frame).await {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TxFrame: {e}"))).await;
                            retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
                            continue;
                        }
                        ts.seq = ts.seq.next();
                    }
                    SessionCommand::EndTx => {
                        if let (Some(active), Some(ts)) = (session.as_mut(), tx_stream.take()) {
                            let seq = ts.seq;
                            tracing::info!(
                                sid = format_args!("{:#06X}", ts.sid.get()),
                                eot_seq = seq.get(),
                                frames = tx_frame_count,
                                "TX ending; sending EOT"
                            );
                            if let Err(e) = active.runtime.send_eot(ts.sid, seq).await {
                                let _unused = evt_tx.send(SessionEvent::Error(format!("EndTx: {e}"))).await;
                            } else {
                                let _unused = evt_tx.send(SessionEvent::Log(format!(
                                    "TX ended sid=0x{:04X} frames={tx_frame_count}",
                                    ts.sid.get()
                                ))).await;
                            }
                            tx_frame_count = 0;
                        }
                    }
                }
            }
            event = next_event_opt(session.as_mut()) => {
                let Some(ev) = event else {
                    // Session ended / channel closed.
                    let _unused = evt_tx.send(SessionEvent::Log("session event channel closed".into())).await;
                    retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
                    let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                    continue;
                };
                tracing::trace!(event = ?ev, "runtime event");
                for decision in decide_runtime_event(ev, &mut rx_state) {
                    match decision {
                        EventDecision::EmitSessionEvent(se) => {
                            let _unused = evt_tx.send(se).await;
                        }
                        EventDecision::AudioRxStart => {
                            audio.send(AudioCommand::RxStart);
                        }
                        EventDecision::AudioRxFrame(frame) => {
                            audio.send(AudioCommand::RxFrame(frame));
                        }
                        EventDecision::AudioRxLost => {
                            audio.send(AudioCommand::RxLost);
                        }
                        EventDecision::AudioRxEnd => {
                            audio.send(AudioCommand::RxEnd);
                        }
                        EventDecision::ClearSession => {
                            retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
                            // Schedule an auto-reconnect if the link was
                            // configured for it. First attempt fires
                            // immediately; later attempts back off.
                            if let Some(cfg) = last_cfg.clone()
                                && cfg.reconnect_on_drop
                            {
                                let delay = backoff_delay(0);
                                reconnect_at =
                                    Some((tokio::time::Instant::now() + delay, 1));
                                let _unused = evt_tx
                                    .send(SessionEvent::Log(format!(
                                        "reconnecting in {}s",
                                        delay.as_secs()
                                    )))
                                    .await;
                            }
                        }
                    }
                }
            }
            _ = health_tick.tick(), if session.is_some() => {
                if let Some(active) = session.as_ref() {
                    let age = active.runtime.activity().borrow().elapsed();
                    let _unused = evt_tx
                        .send(SessionEvent::LinkHealth {
                            last_heard_secs: age.as_secs_f32(),
                        })
                        .await;
                    // Live per-stream counters while a stream is up;
                    // the final values still arrive via VoiceEnd.
                    if rx_state.expected_seq.is_some() {
                        let _unused = evt_tx
                            .send(SessionEvent::RxStats {
                                received: rx_state.rx_frame_count,
                                lost: rx_state.frames_lost,
                                late: rx_state.frames_late,
                            })
                            .await;
                    }
                }
            }
            () = sleep_until_reconnect(reconnect_at), if reconnect_at.is_some() => {
                if let (Some((_, attempt)), Some(cfg)) =
                    (reconnect_at.take(), last_cfg.clone())
                {
                    let _unused = evt_tx
                        .send(SessionEvent::Status(ConnStatus::Connecting {
                            peer: cfg.endpoint.clone(),
                        }))
                        .await;
                    let result = tokio::select! {
                        biased;
                        _ = terminal.changed() => continue,
                        result = connect(&cfg) => result,
                    };
                    match result {
                        Ok((rs, _auth, _hosts)) => {
                            let _unused = evt_tx
                                .send(SessionEvent::Status(ConnStatus::Connected {
                                    reflector: cfg.reflector_callsign.to_string(),
                                    module: cfg.reflector_module.as_char(),
                                }))
                                .await;
                            audio.set_connected(Some(cfg.callsign));
                            session = Some(ActiveSession { runtime: rs, cfg });
                        }
                        Err(e) => {
                            let _unused = evt_tx
                                .send(SessionEvent::Log(format!(
                                    "reconnect attempt {attempt} failed: {e}"
                                )))
                                .await;
                            let delay = backoff_delay(attempt);
                            reconnect_at = Some((
                                tokio::time::Instant::now() + delay,
                                attempt.saturating_add(1),
                            ));
                        }
                    }
                }
            }
        }
    }
    retire_session(&mut session, &mut tx_stream, &audio, &evt_tx).await;
}

async fn next_directory_update(request: &mut Option<DirectoryFetch>) -> DirectoryUpdate {
    match request {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// Retain the active handle until EOT, unlink and task completion are observed.
async fn retire_session(
    session: &mut Option<ActiveSession>,
    tx_stream: &mut Option<TxStream>,
    audio: &AudioHandle,
    events: &SessionEvents,
) {
    audio.set_connected(None);
    if let Some(mut active) = session.take() {
        let _sent = events
            .send(SessionEvent::Status(ConnStatus::Disconnecting))
            .await;
        if let Some(stream) = tx_stream.take()
            && let Err(error) = active.runtime.send_eot(stream.sid, stream.seq).await
        {
            let _sent = events
                .send(SessionEvent::Error(format!("end transmission: {error}")))
                .await;
        }
        if let Err(error) = active.runtime.disconnect().await {
            let _sent = events
                .send(SessionEvent::Error(format!("disconnect: {error}")))
                .await;
        }
        if let Err(error) = active.runtime.join().await {
            let _sent = events
                .send(SessionEvent::Error(format!("session completion: {error}")))
                .await;
        }
    }
    *tx_stream = None;
    let _sent = events
        .send(SessionEvent::Status(ConnStatus::Disconnected))
        .await;
}

/// Reconnect backoff: 0s, 3s, 10s, then 20s for every later attempt.
const fn backoff_delay(attempt: u8) -> Duration {
    match attempt {
        0 => Duration::from_secs(0),
        1 => Duration::from_secs(3),
        2 => Duration::from_secs(10),
        _ => Duration::from_secs(20),
    }
}

/// Sleeps until the scheduled reconnect instant, or forever when no
/// reconnect is pending (the `select!` arm is gated by `is_some()`).
async fn sleep_until_reconnect(at: Option<(tokio::time::Instant, u8)>) {
    match at {
        Some((when, _)) => tokio::time::sleep_until(when).await,
        None => std::future::pending().await,
    }
}

/// Returns `next_event()` on the active session, or `pending` forever
/// when no session is active (disables the select branch cleanly).
async fn next_event_opt(session: Option<&mut ActiveSession>) -> Option<RuntimeEvent> {
    match session {
        Some(s) => s.runtime.next_event().await.map(RuntimeEvent::from),
        None => std::future::pending().await,
    }
}

/// Establish a `DExtra` / `DPlus` / DCS session per `cfg`.
/// Outcome of the optional `DPlus` authentication step.
#[derive(Debug, Clone, Copy)]
enum AuthOutcome {
    /// Not a `DPlus` session, so no auth attempted.
    NotApplicable,
    /// Auth succeeded against the `DPlus` auth server.
    Authenticated,
    /// Auth failed; connecting with an empty host list.
    FellBack,
}

async fn connect(
    cfg: &ConnectConfig,
) -> Result<(RuntimeSession, AuthOutcome, Vec<DPlusHost>), ConnectFailure> {
    connect_with_budget(cfg, HANDSHAKE_DEADLINE).await
}

/// One total budget includes resolution, optional authentication and handshake.
async fn connect_with_budget(
    cfg: &ConnectConfig,
    budget: Duration,
) -> Result<(RuntimeSession, AuthOutcome, Vec<DPlusHost>), ConnectFailure> {
    tokio::time::timeout(budget, async {
        let peer = cfg.endpoint.resolve().await?;
        connect_resolved(cfg, peer).await
    })
    .await
    .map_err(|_| ConnectFailure::Deadline)?
}

async fn connect_resolved(
    cfg: &ConnectConfig,
    peer: SocketAddr,
) -> Result<(RuntimeSession, AuthOutcome, Vec<DPlusHost>), ConnectFailure> {
    match cfg.protocol {
        ConnectionProtocol::DExtra => Ok((
            RuntimeSession::DExtra(connect_dextra(cfg, peer).await?),
            AuthOutcome::NotApplicable,
            Vec::new(),
        )),
        ConnectionProtocol::DPlus => {
            let (session, auth, host_list) = connect_dplus(cfg, peer).await?;
            let hosts = host_list.hosts().to_vec();
            Ok((RuntimeSession::DPlus(session), auth, hosts))
        }
        ConnectionProtocol::Dcs(reflector) => Ok((
            RuntimeSession::Dcs(connect_dcs(cfg, peer, reflector).await?),
            AuthOutcome::NotApplicable,
            Vec::new(),
        )),
    }
}

/// Bind a fresh ephemeral UDP socket for the session.
async fn bind_session_socket(peer: SocketAddr) -> Result<Arc<UdpSocket>, ConnectFailure> {
    let sock = UdpSocket::bind(local_bind_address(peer))
        .await
        .map_err(ConnectFailure::Bind)?;
    Ok(Arc::new(sock))
}

/// Overall deadline for the UDP connect handshake (the shared pump in
/// `dstar-gateway` polls inside this window).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

async fn connect_dextra(
    cfg: &ConnectConfig,
    peer: SocketAddr,
) -> Result<AsyncSession<DExtra>, ConnectFailure> {
    let sock = bind_session_socket(peer).await?;
    let configured = Session::<DExtra, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .peer(peer)
        .build();
    let connecting = configured
        .connect(Instant::now())
        .map_err(|f| ConnectFailure::Core(f.error))?;
    let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
        .await
        .map_err(ConnectFailure::Handshake)?;
    Ok(AsyncSession::spawn(connected, sock))
}

async fn connect_dplus(
    cfg: &ConnectConfig,
    peer: SocketAddr,
) -> Result<
    (
        AsyncSession<DPlus>,
        AuthOutcome,
        dstar_gateway_core::codec::dplus::HostList,
    ),
    ConnectFailure,
> {
    // Local test setups rarely have the DPlus TCP auth server so we
    // treat auth as best-effort: try it first, and if it fails use an
    // empty `HostList` to satisfy the typestate. The UDP handshake is
    // identical to DExtra after that.
    let (host_list, auth) = match dstar_gateway::auth::AuthClient::new()
        .authenticate(cfg.callsign.callsign())
        .await
    {
        Ok(h) => (h, AuthOutcome::Authenticated),
        Err(e) => {
            tracing::debug!(?e, "DPlus auth failed; falling back to empty host list");
            (
                dstar_gateway_core::codec::dplus::HostList::new(),
                AuthOutcome::FellBack,
            )
        }
    };
    let sock = bind_session_socket(peer).await?;
    let configured = Session::<DPlus, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .peer(peer)
        .build();
    let authed = configured
        .authenticate(host_list.clone())
        .map_err(|f| ConnectFailure::Core(f.error))?;
    let connecting = authed
        .connect(Instant::now())
        .map_err(|f| ConnectFailure::Core(f.error))?;
    let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
        .await
        .map_err(ConnectFailure::Handshake)?;
    Ok((AsyncSession::spawn(connected, sock), auth, host_list))
}

/// A login NAK (`DPlus` BUSY) closes the session and surfaces as
/// [`ConnectError::Rejected`]; the refusal alone identifies no cause.
fn describe_dplus_handshake_error(e: &ConnectError) -> String {
    match e {
        ConnectError::Rejected => {
            "reflector refused the link (BUSY); verify the target and authorization".to_string()
        }
        other => format!("handshake: {other}"),
    }
}

async fn connect_dcs(
    cfg: &ConnectConfig,
    peer: SocketAddr,
    reflector: DcsReflector,
) -> Result<AsyncSession<Dcs>, ConnectFailure> {
    let sock = bind_session_socket(peer).await?;
    let configured = Session::<Dcs, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .dcs_reflector(reflector)
        .peer(peer)
        .build();
    let connecting = configured
        .connect(Instant::now())
        .map_err(|f| ConnectFailure::Core(f.error))?;
    let connected = drive_connecting(connecting, &sock, HANDSHAKE_DEADLINE)
        .await
        .map_err(ConnectFailure::Handshake)?;
    Ok(AsyncSession::spawn(connected, sock))
}

/// Begin a voice TX: allocate a stream-id, build and send the header,
/// return the tracking state. The header is built via
/// [`DstarHeader::for_relay`] so `rpt1[7]` / `rpt2[7]` carry the
/// validated module letters strict reflectors (xlxd-derived) demand.
async fn start_tx(
    active: &mut ActiveSession,
    stream: &mut Option<TxStream>,
) -> Result<StreamId, ShellError> {
    let sid = fresh_stream_id();
    let header = DstarHeader::for_relay(
        RelayRouting {
            operator: active.cfg.callsign,
            local_module: active.cfg.local_module,
            reflector: active.cfg.reflector_callsign,
            reflector_module: active.cfg.reflector_module,
        },
        active.cfg.callsign.callsign(),
        Suffix::EMPTY,
    );
    tracing::info!(
        sid = format_args!("{:#06X}", sid.get()),
        my_call = %active.cfg.callsign,
        rpt1 = %active.cfg.callsign,
        rpt1_module = %active.cfg.local_module,
        rpt2 = %active.cfg.reflector_callsign,
        rpt2_module = %active.cfg.reflector_module,
        "TX starting; sending header"
    );
    *stream = Some(TxStream {
        sid,
        seq: FrameSequence::ZERO,
    });
    active.runtime.send_header(header, sid).await?;
    Ok(sid)
}

/// Send 100 AMBE silence frames using the established routing identity.
/// Cancellation retains the active stream for explicit EOT during retirement.
async fn tx_silence(
    active: &mut ActiveSession,
    tx_stream: &mut Option<TxStream>,
    evt_tx: &SessionEvents,
) -> Result<(), ShellError> {
    let sid = start_tx(active, tx_stream).await?;
    let total_frames = 100_u32;
    let _unused = evt_tx
        .send(SessionEvent::Log(format!(
            "TX: sent header, sending {total_frames} silence frames"
        )))
        .await;

    let frame = VoiceFrame {
        ambe: AMBE_SILENCE,
        slow_data: DSTAR_SYNC_BYTES,
    };
    for _ in 0..total_frames {
        let Some(stream) = tx_stream.as_mut() else {
            return Ok(());
        };
        let sequence = stream.seq;
        active.runtime.send_voice(sid, sequence, frame).await?;
        stream.seq = sequence.next();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    if let Some(stream) = tx_stream.as_ref() {
        active.runtime.send_eot(sid, stream.seq).await?;
    }
    *tx_stream = None;
    let _unused = evt_tx
        .send(SessionEvent::Log(format!(
            "TX: sent EOT ({total_frames} frames)"
        )))
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ConnStatus, EventDecision, EventState, GapClass, RuntimeEvent, SessionEvent, classify_gap,
        decide_runtime_event,
    };
    use dstar_gateway_core::session::client::VoiceEndReason;
    use dstar_gateway_core::slowdata::SlowDataTextMessage;
    use dstar_gateway_core::types::StreamId;
    use dstar_gateway_core::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Compile-time-checked [`StreamId`] constructor for test fixtures.
    /// `Option::unwrap` is const since 1.83, so a zero literal becomes
    /// a compile error, matching the workspace convention of infallible
    /// test-fixture construction.
    const fn sid(n: u16) -> StreamId {
        match StreamId::new(n) {
            Some(s) => s,
            None => unreachable!(),
        }
    }

    const fn sample_frame() -> VoiceFrame {
        VoiceFrame {
            ambe: AMBE_SILENCE,
            slow_data: DSTAR_SYNC_BYTES,
        }
    }

    /// **Regression guard for the silent-disconnect bug.** Before the
    /// fix, `Event::Disconnected` from the reflector was folded into
    /// `RuntimeEvent::Other`, the run loop logged it as a plain text
    /// line, and `ConnStatus` stayed `Connected` while every TX
    /// silently failed. This test pins the new behaviour: every
    /// reflector-driven disconnect must emit a status flip + clear
    /// the session, in that order.
    #[test]
    fn disconnected_clears_session_and_emits_status() -> TestResult {
        let mut state = EventState {
            rx_frame_count: 5,
            ..EventState::default()
        };
        let decisions = decide_runtime_event(
            RuntimeEvent::Disconnected {
                reason: "KeepaliveInactivity".into(),
            },
            &mut state,
        );
        assert_eq!(
            decisions.len(),
            4,
            "expected 4 decisions, got {decisions:?}"
        );
        let first = decisions.first().ok_or("no decisions emitted")?;
        assert!(
            matches!(first, EventDecision::AudioRxEnd),
            "first decision must flush the audio holdback, got {first:?}",
        );
        let second = decisions.get(1).ok_or("missing second decision")?;
        assert!(
            matches!(
                second,
                EventDecision::EmitSessionEvent(SessionEvent::Log(_))
            ),
            "second decision must be a log line, got {second:?}",
        );
        let third = decisions.get(2).ok_or("missing third decision")?;
        assert!(
            matches!(third, EventDecision::ClearSession),
            "third decision must clear the session so subsequent commands fail loudly, \
             got {third:?}",
        );
        let fourth = decisions.get(3).ok_or("missing fourth decision")?;
        assert!(
            matches!(
                fourth,
                EventDecision::EmitSessionEvent(SessionEvent::Status(ConnStatus::Disconnected))
            ),
            "fourth decision must flip ConnStatus to Disconnected, got {fourth:?}",
        );
        Ok(())
    }

    #[test]
    fn voice_start_resets_frame_counter_and_emits_audio_reset() -> TestResult {
        let mut state = EventState {
            rx_frame_count: 99,
            ..EventState::default()
        };
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceStart {
                stream_id: sid(0x1234),
                my_call: "W1AW".into(),
                route: super::RxRoute::default(),
            },
            &mut state,
        );
        assert_eq!(
            state.rx_frame_count, 0,
            "VoiceStart resets the frame counter"
        );
        let first = decisions.first().ok_or("no decisions emitted")?;
        assert!(matches!(first, EventDecision::AudioRxStart));
        let second = decisions.get(1).ok_or("missing second decision")?;
        assert!(matches!(
            second,
            EventDecision::EmitSessionEvent(SessionEvent::VoiceStart {
                stream_id,
                ..
            }) if stream_id.get() == 0x1234
        ));
        Ok(())
    }

    #[test]
    fn voice_frame_increments_counter_and_routes_to_audio() -> TestResult {
        let mut state = EventState {
            rx_frame_count: 7,
            ..EventState::default()
        };
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 1,
                frame: sample_frame(),
            },
            &mut state,
        );
        assert_eq!(state.rx_frame_count, 8);
        assert_eq!(decisions.len(), 1);
        let first = decisions.first().ok_or("no decisions emitted")?;
        assert!(matches!(first, EventDecision::AudioRxFrame(_)));
        Ok(())
    }

    /// Feeding the wire-format slow-data fragments for a complete
    /// 20-char text message must produce a `SlowDataMessage` decision
    /// once the fourth block lands.
    #[test]
    fn slow_data_assembles_complete_message() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;

        let mut state = EventState {
            rx_stream_id: Some(sid(0xABCD)),
            ..EventState::default()
        };
        // 8 half-blocks (4 blocks × 2 halves) at non-zero seqs;
        // cribbed from the text_collector test fixture.
        let halves: [[u8; 3]; 8] = [
            [0x40, b'C', b'Q'],
            *b" wo",
            [0x41, b'r', b'k'],
            *b"ing",
            [0x42, b' ', b' '],
            *b"   ",
            [0x43, b' ', b' '],
            *b"   ",
        ];
        let mut emitted_message: Option<SlowDataTextMessage> = None;
        for (seq, half) in (1u8..).zip(halves.iter()) {
            let frame = VoiceFrame {
                ambe: AMBE_SILENCE,
                slow_data: scramble(*half),
            };
            let decisions =
                decide_runtime_event(RuntimeEvent::VoiceFrame { seq, frame }, &mut state);
            for d in decisions {
                if let EventDecision::EmitSessionEvent(SessionEvent::SlowDataMessage {
                    stream_id,
                    message,
                }) = d
                {
                    assert_eq!(
                        stream_id.get(),
                        0xABCD,
                        "slow-data message tagged with active stream"
                    );
                    emitted_message = Some(message);
                }
            }
        }
        let message = emitted_message.ok_or("no slow-data message emitted after 8 halves")?;
        assert_eq!(message.text()?, "CQ working", "trailing spaces trimmed");
        Ok(())
    }

    #[test]
    fn slow_data_preserves_invalid_text_bytes_without_replacement() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;

        let mut state = EventState {
            rx_stream_id: Some(sid(0xBEEF)),
            ..EventState::default()
        };
        let halves: [[u8; 3]; 8] = [
            [0x40, b'A', 0xFF],
            *b"   ",
            [0x41, b' ', b' '],
            *b"   ",
            [0x42, b' ', b' '],
            *b"   ",
            [0x43, b' ', b' '],
            *b"   ",
        ];
        let mut completed = None;
        for (seq, half) in (1u8..).zip(halves) {
            let decisions = decide_runtime_event(
                RuntimeEvent::VoiceFrame {
                    seq,
                    frame: VoiceFrame {
                        ambe: AMBE_SILENCE,
                        slow_data: scramble(half),
                    },
                },
                &mut state,
            );
            for decision in decisions {
                if let EventDecision::EmitSessionEvent(SessionEvent::SlowDataMessage {
                    message,
                    ..
                }) = decision
                {
                    completed = Some(message);
                }
            }
        }

        let message = completed.ok_or("no slow-data message emitted")?;
        assert_eq!(message.as_bytes().get(..2), Some([b'A', 0xFF].as_slice()));
        assert_eq!(
            message.text().map_err(|error| (error.index, error.byte)),
            Err((1, 0xFF))
        );
        Ok(())
    }

    #[test]
    fn voice_start_resets_slow_data_collector() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;
        let mut state = EventState::default();
        // Push half a message, then start a new stream; the collector
        // must drop the partial state.
        for (seq, half) in (1u8..).zip([[0x40_u8, b'X', b'X'], *b"XXX"].iter()) {
            let _unused = decide_runtime_event(
                RuntimeEvent::VoiceFrame {
                    seq,
                    frame: VoiceFrame {
                        ambe: AMBE_SILENCE,
                        slow_data: scramble(*half),
                    },
                },
                &mut state,
            );
        }
        // VoiceStart on a new stream resets internal state.
        let _vs = decide_runtime_event(
            RuntimeEvent::VoiceStart {
                stream_id: sid(0x0001),
                my_call: "W1AW".into(),
                route: super::RxRoute::default(),
            },
            &mut state,
        );
        // Now feed a fresh 4-block message and verify the previously
        // partially-collected slot doesn't bleed in.
        let halves: [[u8; 3]; 8] = [
            [0x40, b'N', b'E'],
            *b"W  ",
            [0x41, b' ', b' '],
            *b"   ",
            [0x42, b' ', b' '],
            *b"   ",
            [0x43, b' ', b' '],
            *b"   ",
        ];
        let mut message: Option<SlowDataTextMessage> = None;
        for (seq, half) in (1u8..).zip(halves.iter()) {
            let decisions = decide_runtime_event(
                RuntimeEvent::VoiceFrame {
                    seq,
                    frame: VoiceFrame {
                        ambe: AMBE_SILENCE,
                        slow_data: scramble(*half),
                    },
                },
                &mut state,
            );
            for d in decisions {
                if let EventDecision::EmitSessionEvent(SessionEvent::SlowDataMessage {
                    message: completed,
                    ..
                }) = d
                {
                    message = Some(completed);
                }
            }
        }
        let final_message = message.ok_or("no slow-data message emitted after fresh stream")?;
        assert_eq!(
            final_message.text()?,
            "NEW",
            "fresh stream's message must not include slot from prior partial"
        );
        Ok(())
    }

    #[test]
    fn voice_end_reports_frame_count_and_resets() -> TestResult {
        let mut state = EventState {
            rx_frame_count: 42,
            ..EventState::default()
        };
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceEnd {
                stream_id: sid(0x9ABC),
                reason: VoiceEndReason::Eot,
            },
            &mut state,
        );
        assert_eq!(state.rx_frame_count, 0, "VoiceEnd resets the counter");
        // Index 2: AudioRxEnd and the per-stream RxStats decision
        // precede VoiceEnd.
        let third = decisions.get(2).ok_or("missing VoiceEnd decision")?;
        let EventDecision::EmitSessionEvent(SessionEvent::VoiceEnd {
            stream_id, frames, ..
        }) = third
        else {
            return Err(format!("expected VoiceEnd decision, got {third:?}").into());
        };
        assert_eq!(stream_id.get(), 0x9ABC);
        assert_eq!(
            *frames, 42,
            "frame count must be the value at the moment of EOT"
        );
        Ok(())
    }

    #[test]
    fn other_events_emit_log_line() -> TestResult {
        let mut state = EventState::default();
        let decisions = decide_runtime_event(
            RuntimeEvent::Other("HostList { hosts: [] }".into()),
            &mut state,
        );
        assert_eq!(decisions.len(), 1);
        let first = decisions.first().ok_or("no decisions emitted")?;
        assert!(matches!(
            first,
            EventDecision::EmitSessionEvent(SessionEvent::Log(_))
        ));
        Ok(())
    }

    /// Keepalive echoes arrive at poll cadence; they must stay out
    /// of the event log entirely (liveness is shown by the status
    /// line's last-heard age instead).
    #[test]
    fn keepalive_emits_no_decisions() {
        let mut state = EventState::default();
        let decisions = decide_runtime_event(RuntimeEvent::Keepalive, &mut state);
        assert!(
            decisions.is_empty(),
            "keepalive must be silent, got {decisions:?}"
        );
    }

    /// A complete CRC-checked sentence spans a sync frame without losing bytes.
    #[test]
    fn slow_data_assembles_gps_position() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;

        let sentence: &[u8] = b"$$CRC8FF2,W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>Asheville test\r";
        let mut state = EventState {
            rx_stream_id: Some(sid(0x5678)),
            ..EventState::default()
        };
        let mut emitted: Option<(f64, f64)> = None;
        let mut seq: u8 = 1;
        for chunk in sentence.chunks(5) {
            // Pack the chunk into a 6-byte GPS block: [0x30, b0..b4].
            let mut block = [0u8; 6];
            block[0] = 0x30;
            for (slot, &b) in block.iter_mut().skip(1).zip(chunk) {
                *slot = b;
            }
            for half in block.chunks(3) {
                let mut frag = [0u8; 3];
                frag.copy_from_slice(half);
                let frame = VoiceFrame {
                    ambe: AMBE_SILENCE,
                    slow_data: scramble(frag),
                };
                let decisions =
                    decide_runtime_event(RuntimeEvent::VoiceFrame { seq, frame }, &mut state);
                for d in decisions {
                    if let EventDecision::EmitSessionEvent(SessionEvent::GpsPosition {
                        position,
                        ..
                    }) = d
                    {
                        emitted = Some((position.latitude(), position.longitude()));
                    }
                }
                if seq == 20 {
                    let _sync = decide_runtime_event(
                        RuntimeEvent::VoiceFrame {
                            seq: 0,
                            frame: VoiceFrame {
                                ambe: AMBE_SILENCE,
                                slow_data: DSTAR_SYNC_BYTES,
                            },
                        },
                        &mut state,
                    );
                    seq = 1;
                } else {
                    seq += 1;
                }
            }
        }
        let (lat, lon) = emitted.ok_or("no GPS position decoded from slow data")?;
        assert!(
            lat > 34.0 && lat < 37.0,
            "Asheville latitude {lat} out of expected band"
        );
        assert!(
            lon > -84.0 && lon < -81.0,
            "Asheville longitude {lon} out of expected band"
        );
        Ok(())
    }

    #[test]
    fn gps_overflow_and_invalid_framing_are_observable() {
        use super::{GpsAssemblyError, GpsSlowData, MAX_GPS_SENTENCE_BYTES};
        use dstar_gateway_core::slowdata::scramble;
        let mut gps = GpsSlowData {
            block: *b"\x35$AAAA",
            ..GpsSlowData::default()
        };
        let _position = gps.commit_block();
        gps.block = *b"\x35AAAAA";
        for _ in 0..MAX_GPS_SENTENCE_BYTES {
            let _position = gps.commit_block();
        }
        assert!(gps.buffer.len() < MAX_GPS_SENTENCE_BYTES);
        assert!(gps.discarding);
        assert_eq!(gps.take_error(), Some(GpsAssemblyError::Overflow));
        gps.block = *b"\x35\rxxxx";
        let _position = gps.commit_block();
        assert!(!gps.discarding);
        let _position = gps.push(scramble(*b"\x35$G"), 1);
        let _position = gps.push(scramble(*b"PRM"), 3);
        assert_eq!(gps.take_error(), Some(GpsAssemblyError::SequenceGap));
        gps.block = *b"\x36$AAAA";
        let _position = gps.commit_block();
        assert_eq!(gps.take_error(), Some(GpsAssemblyError::BlockLength));
        gps.reset();
        gps.block = [0x35, b'$', 0xff, b'A', b'A', b'A'];
        let _position = gps.commit_block();
        assert_eq!(gps.take_error(), Some(GpsAssemblyError::InvalidByte));
    }

    #[test]
    fn unknown_receive_sequence_cannot_overflow_gap_arithmetic() {
        let mut state = EventState::default();
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: u8::MAX,
                frame: VoiceFrame {
                    ambe: AMBE_SILENCE,
                    slow_data: DSTAR_SYNC_BYTES,
                },
            },
            &mut state,
        );
        assert_eq!(state.frames_late, 1);
        assert!(matches!(
            decisions.as_slice(),
            [EventDecision::EmitSessionEvent(SessionEvent::Log(_))]
        ));
    }

    /// The mod-21 ring is split into zones: exact match, concealable
    /// gap (1..=10 ahead), dropout (11..=17 ahead), and late
    /// (within 3 behind).
    #[test]
    fn classify_gap_zones() {
        assert_eq!(classify_gap(5, 5), GapClass::InOrder);
        assert_eq!(classify_gap(5, 6), GapClass::Conceal(1));
        assert_eq!(classify_gap(5, 15), GapClass::Conceal(10));
        assert_eq!(classify_gap(5, 16), GapClass::Dropout(11));
        assert_eq!(classify_gap(5, 1), GapClass::Dropout(17));
        assert_eq!(classify_gap(5, 2), GapClass::Late);
        assert_eq!(classify_gap(5, 4), GapClass::Late);
        // Wrap-around cases.
        assert_eq!(classify_gap(20, 0), GapClass::Conceal(1));
        assert_eq!(classify_gap(0, 20), GapClass::Late);
        assert_eq!(classify_gap(0, 0), GapClass::InOrder);
    }

    /// A two-frame sequence gap must emit two concealment decisions
    /// ahead of the real frame and count the loss.
    #[test]
    fn sequence_gap_emits_concealment_decisions() {
        let mut state = EventState::default();
        // First frame of the stream initializes the expectation.
        let _first = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 0,
                frame: sample_frame(),
            },
            &mut state,
        );
        // seq 1 and 2 were lost; seq 3 arrives.
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 3,
                frame: sample_frame(),
            },
            &mut state,
        );
        assert_eq!(state.frames_lost, 2);
        assert_eq!(
            decisions.len(),
            3,
            "two conceal + one frame, got {decisions:?}"
        );
        assert!(matches!(
            decisions.first(),
            Some(EventDecision::AudioRxLost)
        ));
        assert!(matches!(decisions.get(1), Some(EventDecision::AudioRxLost)));
        assert!(matches!(
            decisions.get(2),
            Some(EventDecision::AudioRxFrame(_))
        ));
    }

    /// Sequence wrap 19 → 20 → 0 is in-order: no concealment, no
    /// loss counted.
    #[test]
    fn seq_wrap_without_gap_is_clean() {
        let mut state = EventState::default();
        for seq in [19_u8, 20, 0] {
            let decisions = decide_runtime_event(
                RuntimeEvent::VoiceFrame {
                    seq,
                    frame: sample_frame(),
                },
                &mut state,
            );
            assert_eq!(decisions.len(), 1, "seq {seq}: got {decisions:?}");
        }
        assert_eq!(state.frames_lost, 0);
    }

    /// A frame arriving behind the expected sequence was already
    /// concealed past, so playing it now would double-play 20 ms.
    #[test]
    fn late_frame_is_dropped_not_double_played() {
        let mut state = EventState::default();
        let _first = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 5,
                frame: sample_frame(),
            },
            &mut state,
        );
        let late = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 4,
                frame: sample_frame(),
            },
            &mut state,
        );
        assert!(
            late.is_empty(),
            "late frame must produce no decisions, got {late:?}"
        );
        assert_eq!(state.frames_late, 1);
        // The stream recovers: the genuinely-next frame is in order.
        let next = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 6,
                frame: sample_frame(),
            },
            &mut state,
        );
        assert_eq!(next.len(), 1, "got {next:?}");
    }

    /// Gaps beyond the conceal cap resync without concealment so a
    /// long outage doesn't smear one voice posture across seconds.
    #[test]
    fn long_gap_resyncs_without_concealment() {
        let mut state = EventState::default();
        let _first = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 0,
                frame: sample_frame(),
            },
            &mut state,
        );
        // seq 1..=14 lost: beyond MAX_CONCEAL.
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 15,
                frame: sample_frame(),
            },
            &mut state,
        );
        assert_eq!(
            decisions.len(),
            1,
            "no concealment on a dropout, got {decisions:?}"
        );
        assert!(matches!(
            decisions.first(),
            Some(EventDecision::AudioRxFrame(_))
        ));
        assert_eq!(state.frames_lost, 14);
    }

    /// `VoiceEnd` must report the stream's loss counters ahead of the
    /// `VoiceEnd` event itself.
    #[test]
    fn voice_end_reports_stream_stats() -> TestResult {
        let mut state = EventState::default();
        let _f0 = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 0,
                frame: sample_frame(),
            },
            &mut state,
        );
        let _f3 = decide_runtime_event(
            RuntimeEvent::VoiceFrame {
                seq: 3,
                frame: sample_frame(),
            },
            &mut state,
        );
        let decisions = decide_runtime_event(
            RuntimeEvent::VoiceEnd {
                stream_id: sid(0x1234),
                reason: VoiceEndReason::Eot,
            },
            &mut state,
        );
        let first = decisions.first().ok_or("no decisions")?;
        assert!(
            matches!(first, EventDecision::AudioRxEnd),
            "expected the audio holdback flush first, got {first:?}"
        );
        let second = decisions.get(1).ok_or("missing RxStats decision")?;
        assert!(
            matches!(
                second,
                EventDecision::EmitSessionEvent(SessionEvent::RxStats {
                    received: 2,
                    lost: 2,
                    late: 0,
                })
            ),
            "expected RxStats second, got {second:?}"
        );
        assert_eq!(state.frames_lost, 0, "counters reset after VoiceEnd");
        Ok(())
    }

    /// **Regression guard for the masked-rejection bug.** A `DPlus`
    /// reflector that answers BUSY has refused the link, so the
    /// handshake must fail immediately with a "refused" error, not
    /// idle out as a fake 5-second "handshake timeout" (which is what
    /// a real dstargateway REF refusal looked like before the fix).
    #[tokio::test]
    async fn dplus_busy_rejection_fails_fast_with_refusal() -> TestResult {
        use std::time::{Duration, Instant};

        use dstar_gateway::tokio_shell::{ConnectError, drive_connecting};
        use dstar_gateway_core::codec::dplus::HostList;
        use dstar_gateway_core::session::client::{Configured, DPlus, Session};
        use dstar_gateway_core::types::{Module, RoutingCallsign};

        // Fake reflector: reply BUSY to every datagram, like a REF
        // refusing an unauthorized IP.
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let peer = server.local_addr()?;
        let _reflector = tokio::spawn(async move {
            let mut buf = [0_u8; 64];
            while let Ok((_n, src)) = server.recv_from(&mut buf).await {
                let reply = [0x08, 0xC0, 0x04, 0x00, b'B', b'U', b'S', b'Y'];
                if server.send_to(&reply, src).await.is_err() {
                    break;
                }
            }
        });

        let sock = super::bind_session_socket(peer).await?;
        let configured = Session::<DPlus, Configured>::builder()
            .callsign(RoutingCallsign::new("W1AW")?)
            .local_module(Module::C)
            .reflector_module(Module::C)
            .peer(peer)
            .build();
        let authed = configured
            .authenticate(HostList::new())
            .map_err(|f| f.error.to_string())?;
        let connecting = authed
            .connect(Instant::now())
            .map_err(|f| f.error.to_string())?;
        let error = match drive_connecting(connecting, &sock, Duration::from_secs(5)).await {
            Ok(_connected) => return Err("BUSY handshake unexpectedly connected".into()),
            Err(e) => e,
        };
        assert!(
            matches!(error, ConnectError::Rejected),
            "BUSY must surface as a rejection, not a timeout: {error:?}"
        );
        let message = super::describe_dplus_handshake_error(&error);
        assert!(
            message.contains("refused"),
            "operator message names the refusal: {message}"
        );
        Ok(())
    }
}
#[cfg(test)]
mod lifecycle_tests {
    fn loopback_config(
        peer: std::net::SocketAddr,
    ) -> Result<super::ConnectConfig, Box<dyn std::error::Error>> {
        use dstar_gateway_core::types::{Module, ProtocolKind, RoutingCallsign};

        let identity = super::ConnectionIdentity {
            operator: RoutingCallsign::new("W1AW")?,
            local_module: Module::A,
            reflector: RoutingCallsign::new("POLARIS")?,
            reflector_module: Module::B,
        };
        Ok(super::ConnectConfig::new(
            ProtocolKind::DExtra,
            identity,
            super::Endpoint::new(
                peer.ip().into(),
                std::num::NonZeroU16::new(peer.port()).ok_or("loopback port is nonzero")?,
            ),
            false,
        )?)
    }

    #[tokio::test]
    async fn connection_budget_covers_the_entire_silent_handshake()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let config = loopback_config(server.local_addr()?)?;
        let started = std::time::Instant::now();
        let result = super::connect_with_budget(&config, std::time::Duration::from_millis(7)).await;
        assert!(
            matches!(result, Err(super::ConnectFailure::Deadline)),
            "total budget was ignored: {result:?}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "connection waited for the nested handshake deadline"
        );
        Ok(())
    }

    #[tokio::test]
    async fn disconnect_cancels_an_in_progress_handshake_without_audio_devices()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await?;
        let config = loopback_config(server.local_addr()?)?;
        let (handle, commands, terminal) = super::SessionHandle::channel();
        let events = super::SessionEvents::default();
        let (audio, _status) = crate::audio::AudioHandle::test_handle();
        let mut task = tokio::spawn(super::run(commands, terminal, events.clone(), audio));
        handle.try_send(super::SessionCommand::Connect(config))?;
        let mut bytes = [0; 64];
        let _packet = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server.recv_from(&mut bytes),
        )
        .await??;
        handle.disconnect();
        let disconnected = tokio::time::timeout(std::time::Duration::from_millis(200), async {
            loop {
                if matches!(
                    events.try_recv(),
                    Ok(super::SessionEvent::Status(super::ConnStatus::Disconnected))
                ) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await;
        handle.shutdown();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), &mut task).await;
        if completed.is_err() {
            task.abort();
            let _cancelled = task.await;
            return Err("session ignored shutdown during handshake".into());
        }
        completed??;
        assert!(
            disconnected.is_ok(),
            "disconnect waited for connection completion"
        );
        Ok(())
    }
}
