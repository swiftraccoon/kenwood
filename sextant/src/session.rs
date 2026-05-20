// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Async session task — owns the `AsyncSession<P>` handles and
//! brokers commands from / events to the GUI.
//!
//! The GUI never touches `dstar-gateway` types directly. It sends
//! [`SessionCommand`]s to this task (Connect, Disconnect, transmit)
//! and receives [`SessionEvent`]s back, which it renders into the
//! status indicator + event log.
//!
//! TX audio (iteration 2) will flow through a separate `mpsc<VoiceFrame>`
//! owned by the session task: the audio worker pushes encoded frames
//! into that channel while PTT is held, and this task calls
//! `send_voice` until the channel empties + the operator releases PTT.
//! For now the only TX available is `TxSilence { seconds }`, which
//! transmits the AMBE silence pattern for diagnostic purposes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::audio::{AudioCommand, AudioHandle};
use crate::geo::{GpsPosition, parse_gps_sentence};
use dstar_gateway::tokio_shell::{AsyncSession, ShellError};
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, Connecting, DExtra, DPlus, Dcs, Event, Protocol, Session,
    VoiceEndReason,
};
use dstar_gateway_core::slowdata::SlowDataTextCollector;
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use dstar_gateway_core::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// Pair an active runtime session with the configuration it was
/// established with. The cfg fields (operator callsign, modules,
/// reflector callsign) are needed to build correctly-routed `rpt1` /
/// `rpt2` headers at TX time — see [`DStarHeader::for_relay`] for the
/// `rpt[7]` module-byte invariant that strict reflectors enforce.
struct ActiveSession {
    runtime: RuntimeSession,
    cfg: ConnectConfig,
}

/// Configuration captured by the GUI and handed to the session task
/// when the user clicks Connect.
#[derive(Debug, Clone)]
pub(crate) struct ConnectConfig {
    /// Protocol family.
    pub(crate) protocol: ProtocolKind,
    /// Operator callsign (max 8 ASCII, uppercase).
    pub(crate) callsign: Callsign,
    /// Module letter we claim locally.
    pub(crate) local_module: Module,
    /// Reflector callsign (embedded in DCS wire packets).
    pub(crate) reflector_callsign: Callsign,
    /// Module letter on the reflector we're linking into.
    pub(crate) reflector_module: Module,
    /// Reflector UDP peer address.
    pub(crate) peer: SocketAddr,
    /// Auto-reconnect after a reflector-driven disconnect.
    pub(crate) reconnect_on_drop: bool,
}

/// Command emitted by the GUI (or audio worker), consumed by the
/// session task.
#[derive(Debug)]
pub(crate) enum SessionCommand {
    /// Establish a session with the given reflector.
    Connect(ConnectConfig),
    /// Gracefully tear down the current session.
    Disconnect,
    /// Send N seconds of AMBE silence for pipeline sanity checks.
    /// Useful before wiring real mic capture — proves header + voice
    /// + EOT reach the reflector.
    TxSilence { seconds: f32 },
    /// Begin a TX stream — audio worker sends this when the operator
    /// keys PTT. The session task generates a fresh stream-id, sends
    /// the header, and starts accepting `TxFrame`s.
    StartTx {
        /// Callsign to embed in the D-STAR header `my_call`.
        my_call: String,
    },
    /// One encoded voice frame from the audio worker. Ignored if no
    /// TX stream is active.
    TxFrame(VoiceFrame),
    /// End the active TX stream — emits EOT and clears state.
    EndTx,
    /// The audio worker hit a fatal init error (no devices, format
    /// unsupported, etc.). Forwarded to the GUI as
    /// [`SessionEvent::Error`] so the user sees "audio is dead"
    /// instead of pretending PTT works while no frames flow.
    AudioInitError(String),
}

/// Current lifecycle state of the session, summarised for the GUI.
#[derive(Debug, Clone)]
pub(crate) enum ConnStatus {
    /// Idle, not connected.
    Disconnected,
    /// Handshake in progress.
    Connecting { peer: SocketAddr },
    /// Connected — showing the remote reflector + module.
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
        stream_id: u16,
        /// Source callsign (if known).
        from: String,
    },
    /// An incoming voice stream ended.
    VoiceEnd {
        /// Stream identifier.
        stream_id: u16,
        /// Number of voice frames observed.
        frames: u32,
        /// Reason reported by the core state machine.
        reason: String,
    },
    /// Hard error — session task is returning to Disconnected.
    Error(String),
    /// A complete 20-character D-STAR slow-data text message has been
    /// assembled from incoming voice frames.
    SlowDataMessage {
        /// Source stream-id (so the GUI can correlate with `VoiceStart`).
        stream_id: u16,
        /// 20-character text message (UTF-8 lossy, trailing spaces trimmed).
        text: String,
    },
    /// A GPS position decoded from incoming slow data.
    GpsPosition {
        /// Source stream-id (correlates with `VoiceStart`).
        stream_id: u16,
        /// Latitude, decimal degrees.
        latitude: f64,
        /// Longitude, decimal degrees.
        longitude: f64,
    },
    /// Reflector hosts learned from the `DPlus` auth server, forwarded
    /// so the GUI can merge them into the reflector directory.
    ReflectorHosts(Vec<(String, std::net::IpAddr)>),
}

/// Protocol-generic wrapper over `AsyncSession<P>`. Borrowed verbatim
/// from the pattern in `thd75-repl/src/main.rs` — same runtime-state
/// dispatch so the event-pump code can be protocol-agnostic.
enum RuntimeSession {
    DPlus(AsyncSession<DPlus>),
    DExtra(AsyncSession<DExtra>),
    Dcs(AsyncSession<Dcs>),
}

impl RuntimeSession {
    async fn next_event(&mut self) -> Option<RuntimeEvent> {
        match self {
            Self::DPlus(s) => s.next_event().await.map(RuntimeEvent::from_dplus),
            Self::DExtra(s) => s.next_event().await.map(RuntimeEvent::from_dextra),
            Self::Dcs(s) => s.next_event().await.map(RuntimeEvent::from_dcs),
        }
    }

    async fn send_header(&mut self, header: DStarHeader, sid: StreamId) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_header(header, sid).await,
            Self::DExtra(s) => s.send_header(header, sid).await,
            Self::Dcs(s) => s.send_header(header, sid).await,
        }
    }

    async fn send_voice(
        &mut self,
        sid: StreamId,
        seq: u8,
        frame: VoiceFrame,
    ) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_voice(sid, seq, frame).await,
            Self::DExtra(s) => s.send_voice(sid, seq, frame).await,
            Self::Dcs(s) => s.send_voice(sid, seq, frame).await,
        }
    }

    async fn send_eot(&mut self, sid: StreamId, seq: u8) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.send_eot(sid, seq).await,
            Self::DExtra(s) => s.send_eot(sid, seq).await,
            Self::Dcs(s) => s.send_eot(sid, seq).await,
        }
    }

    async fn disconnect(&mut self) -> Result<(), ShellError> {
        match self {
            Self::DPlus(s) => s.disconnect().await,
            Self::DExtra(s) => s.disconnect().await,
            Self::Dcs(s) => s.disconnect().await,
        }
    }
}

/// Lightweight state kept while an outgoing voice stream is live.
///
/// `seq` is the NEXT wire seq to use, always in `0..21`.  D-STAR
/// encodes seq in the low 6 bits of a wire byte with bit 6 (`0x40`)
/// reserved as the EOT flag.  Any value `>= 0x40` would set that bit
/// mid-stream and the reflector would treat it as EOT — silently
/// ending the stream.  Wrapping at the superframe length (21) is
/// both spec-correct and keeps us well clear of the EOT bit.
#[derive(Debug)]
struct TxStream {
    sid: StreamId,
    seq: u8,
}

/// D-STAR superframe length — seq wraps mod this value.
const SUPERFRAME_LEN: u8 = 21;

#[derive(Debug)]
enum RuntimeEvent {
    VoiceStart {
        stream_id: StreamId,
        my_call: String,
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
    /// Reflector-side disconnect — keepalive timeout, link rejection,
    /// or an unlink ACK after our own disconnect. Surfaced explicitly
    /// (not folded into `Other`) so the run loop can clear `session`
    /// and tell the GUI the link is dead, instead of leaving a stale
    /// `Connected` indicator while every TX silently fails.
    Disconnected { reason: String },
    /// Anything else — logged as a debug line, not surfaced to the GUI
    /// explicitly.
    Other(String),
}

impl RuntimeEvent {
    fn from_dplus(ev: Event<DPlus>) -> Self {
        Self::from_event(ev)
    }
    fn from_dextra(ev: Event<DExtra>) -> Self {
        Self::from_event(ev)
    }
    fn from_dcs(ev: Event<Dcs>) -> Self {
        Self::from_event(ev)
    }

    fn from_event<P: Protocol + std::fmt::Debug>(ev: Event<P>) -> Self {
        match ev {
            Event::VoiceStart {
                stream_id, header, ..
            } => Self::VoiceStart {
                stream_id,
                my_call: header.my_call.to_string(),
            },
            Event::VoiceEnd { stream_id, reason } => Self::VoiceEnd { stream_id, reason },
            Event::VoiceFrame { seq, frame, .. } => Self::VoiceFrame { seq, frame },
            Event::Disconnected { reason } => Self::Disconnected {
                reason: format!("{reason:?}"),
            },
            other => Self::Other(format!("{other:?}")),
        }
    }
}

/// Assembles incoming GPS slow-data: pairs 3-byte fragments into
/// 6-byte Kenwood blocks (type byte + 5 payload bytes), accumulates
/// `0x3X` GPS payloads into a sentence buffer, and yields a position
/// when a `$$CRC` / `$GPRMC` / `$GPGGA` sentence completes. Mirrors
/// the proven `lodestar-core` block-assembly layout.
#[derive(Debug, Default)]
struct GpsSlowData {
    /// In-progress 6-byte block (first half in [0..3], second in [3..6]).
    block: [u8; 6],
    /// True once the first 3-byte half of the current block is in.
    have_first_half: bool,
    /// Accumulated GPS sentence bytes awaiting a terminator.
    buffer: String,
}

impl GpsSlowData {
    /// Drop all partial state — call on stream boundaries.
    fn reset(&mut self) {
        self.block = [0u8; 6];
        self.have_first_half = false;
        self.buffer.clear();
    }

    /// Feed one voice frame's slow-data fragment at superframe `seq`.
    /// Returns a [`GpsPosition`] when a sentence completes.
    fn push(&mut self, fragment: [u8; 3], seq: u8) -> Option<GpsPosition> {
        // seq 0 is the superframe sync frame — no slow data, resync.
        if seq == 0 {
            self.have_first_half = false;
            return None;
        }
        let plain = dstar_gateway_core::slowdata::descramble(fragment);
        if self.have_first_half {
            self.block[3..6].copy_from_slice(&plain);
            self.have_first_half = false;
            return self.commit_block();
        }
        self.block[0..3].copy_from_slice(&plain);
        self.have_first_half = true;
        None
    }

    /// A complete 6-byte block assembled — route GPS payloads.
    fn commit_block(&mut self) -> Option<GpsPosition> {
        // High nibble 0x30 == GPS NMEA passthrough.
        if self.block[0] & 0xF0 != 0x30 {
            return None;
        }
        // 5 payload bytes follow the type byte.
        let payload = String::from_utf8_lossy(&self.block[1..6]);
        self.buffer.push_str(&payload);
        // GPS sentences terminate with CR (DPRS) or LF (NMEA). Scan
        // for a complete sentence each time the buffer grows.
        let end = self.buffer.find(['\r', '\n'])?;
        let sentence: String = self.buffer.drain(..=end).collect();
        parse_gps_sentence(&sentence)
    }
}

/// Mutable state carried across `decide_runtime_event` calls.
///
/// Pulled out of the `run` loop so the decision logic is a pure
/// function we can unit-test without spinning up a real reflector or
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
    rx_stream_id: u16,
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
    /// Clear the active session — the reflector booted us or the
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

/// Translate a runtime event into a list of decisions for the run
/// loop to execute. Pure function; mutates only the supplied
/// `EventState` counter and slow-data collector.
fn decide_runtime_event(event: RuntimeEvent, state: &mut EventState) -> Vec<EventDecision> {
    match event {
        RuntimeEvent::VoiceStart { stream_id, my_call } => {
            state.rx_frame_count = 0;
            state.rx_stream_id = stream_id.get();
            // Reset slow-data assembly so half-block state from a
            // previous stream can't bleed into the new message.
            state.slow_data.reset();
            state.gps.reset();
            vec![
                EventDecision::AudioRxStart,
                EventDecision::EmitSessionEvent(SessionEvent::VoiceStart {
                    stream_id: stream_id.get(),
                    from: my_call,
                }),
            ]
        }
        RuntimeEvent::VoiceFrame { seq, frame } => {
            state.rx_frame_count = state.rx_frame_count.saturating_add(1);
            // `slow_data` is `[u8; 3]` (`Copy`) — read it out before
            // `frame` is moved into the `AudioRxFrame` decision so both
            // the text collector and the GPS assembler can be fed.
            let slow = frame.slow_data;
            state.slow_data.push(slow, seq);
            let gps = state.gps.push(slow, seq);
            let mut decisions = vec![EventDecision::AudioRxFrame(frame)];
            if let Some(msg_bytes) = state.slow_data.take_message() {
                let text = String::from_utf8_lossy(&msg_bytes).trim_end().to_string();
                if !text.is_empty() {
                    decisions.push(EventDecision::EmitSessionEvent(
                        SessionEvent::SlowDataMessage {
                            stream_id: state.rx_stream_id,
                            text,
                        },
                    ));
                }
            }
            if let Some(pos) = gps {
                decisions.push(EventDecision::EmitSessionEvent(SessionEvent::GpsPosition {
                    stream_id: state.rx_stream_id,
                    latitude: pos.latitude,
                    longitude: pos.longitude,
                }));
            }
            decisions
        }
        RuntimeEvent::VoiceEnd { stream_id, reason } => {
            let frames = state.rx_frame_count;
            state.rx_frame_count = 0;
            // EOT — drop any partial slow-data half-blocks; the next
            // stream restarts assembly cleanly.
            state.slow_data.reset();
            state.gps.reset();
            vec![EventDecision::EmitSessionEvent(SessionEvent::VoiceEnd {
                stream_id: stream_id.get(),
                frames,
                reason: format!("{reason:?}"),
            })]
        }
        RuntimeEvent::Disconnected { reason } => {
            state.slow_data.reset();
            state.gps.reset();
            vec![
                EventDecision::EmitSessionEvent(SessionEvent::Log(format!(
                    "session disconnected by reflector: {reason}"
                ))),
                EventDecision::ClearSession,
                EventDecision::EmitSessionEvent(SessionEvent::Status(ConnStatus::Disconnected)),
            ]
        }
        RuntimeEvent::Other(s) => vec![EventDecision::EmitSessionEvent(SessionEvent::Log(
            format!("event: {s}"),
        ))],
    }
}

/// Top-level session task entry point. Runs until `cmd_rx` closes.
#[expect(
    clippy::too_many_lines,
    reason = "main event loop — splitting the per-command arms into separate helpers would obscure the select! structure"
)]
pub(crate) async fn run(
    mut cmd_rx: mpsc::Receiver<SessionCommand>,
    evt_tx: mpsc::Sender<SessionEvent>,
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
    // Active outgoing TX stream — `Some` between `StartTx` and `EndTx`.
    let mut tx_stream: Option<TxStream> = None;
    // Last config used to connect — replayed for auto-reconnect after
    // a reflector-driven drop. Cleared by an explicit `Disconnect`.
    let mut last_cfg: Option<ConnectConfig> = None;
    // Pending reconnect: (when to fire, attempt index). `None` when no
    // reconnect is scheduled.
    let mut reconnect_at: Option<(tokio::time::Instant, u8)> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    SessionCommand::Connect(cfg) => {
                        if session.is_some() {
                            let _unused = evt_tx.send(SessionEvent::Log("already connected — ignoring Connect".into())).await;
                            continue;
                        }
                        let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Connecting { peer: cfg.peer })).await;
                        match connect(&cfg).await {
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
                                        "DPlus auth: failed — connected with empty host list"
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
                                session = Some(ActiveSession { runtime: rs, cfg });
                            }
                            Err(e) => {
                                let _unused = evt_tx.send(SessionEvent::Error(format!("connect failed: {e}"))).await;
                                let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                            }
                        }
                    }
                    SessionCommand::Disconnect => {
                        // A user-initiated disconnect must never trigger
                        // the auto-reconnect path.
                        last_cfg = None;
                        reconnect_at = None;
                        if let Some(mut active) = session.take() {
                            let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnecting)).await;
                            if let Err(e) = active.runtime.disconnect().await {
                                let _unused = evt_tx.send(SessionEvent::Log(format!("disconnect: {e}"))).await;
                            }
                            let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                        }
                    }
                    SessionCommand::TxSilence { seconds } => {
                        let Some(active) = session.as_mut() else {
                            let _unused = evt_tx.send(SessionEvent::Log("TX: not connected".into())).await;
                            continue;
                        };
                        if tx_stream.is_some() {
                            // Refuse silence test mid-PTT — interleaving two
                            // stream-ids on one wire confuses the reflector.
                            let _unused = evt_tx.send(SessionEvent::Log(
                                "TxSilence: PTT active — ignoring".into(),
                            )).await;
                            continue;
                        }
                        if let Err(e) = tx_silence(active, seconds, &evt_tx).await {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TX error: {e}"))).await;
                        }
                    }
                    SessionCommand::StartTx { my_call } => {
                        let Some(active) = session.as_mut() else {
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: not connected".into())).await;
                            continue;
                        };
                        if tx_stream.is_some() {
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: already transmitting — ignoring".into())).await;
                            continue;
                        }
                        // Audio-worker-supplied `my_call` is a diagnostic
                        // hint; the wire identity comes from the
                        // already-validated `cfg.callsign` so the operator
                        // can't TX with a callsign different from the one
                        // they connected with.
                        if my_call.trim() != active.cfg.callsign.to_string().trim() {
                            tracing::warn!(
                                gui_my_call = %my_call,
                                cfg_callsign = %active.cfg.callsign,
                                "StartTx: GUI callsign drifted from connect-time callsign; using cfg",
                            );
                        }
                        match start_tx(active).await {
                            Ok(ts) => {
                                let _unused = evt_tx.send(SessionEvent::Log(format!(
                                    "TX started sid=0x{:04X} my_call={}",
                                    ts.sid.get(),
                                    active.cfg.callsign,
                                ))).await;
                                tx_stream = Some(ts);
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
                            seq,
                            frame_num = tx_frame_count,
                            "TX voice frame"
                        );
                        if let Err(e) = active.runtime.send_voice(ts.sid, seq, frame).await {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TxFrame: {e}"))).await;
                        }
                        ts.seq = (ts.seq + 1) % SUPERFRAME_LEN;
                    }
                    SessionCommand::AudioInitError(msg) => {
                        // Surface once per init failure — the audio worker
                        // only reports this when it can't open devices.
                        let _unused = evt_tx.send(SessionEvent::Error(format!(
                            "audio init failed: {msg} — TX/RX disabled until restart"
                        ))).await;
                    }
                    SessionCommand::EndTx => {
                        if let (Some(active), Some(ts)) = (session.as_mut(), tx_stream.take()) {
                            let seq = ts.seq;
                            tracing::info!(
                                sid = format_args!("{:#06X}", ts.sid.get()),
                                eot_seq = seq,
                                frames = tx_frame_count,
                                "TX ending — sending EOT"
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
                    session = None;
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
                        EventDecision::ClearSession => {
                            session = None;
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
            () = sleep_until_reconnect(reconnect_at), if reconnect_at.is_some() => {
                if let (Some((_, attempt)), Some(cfg)) =
                    (reconnect_at.take(), last_cfg.clone())
                {
                    let _unused = evt_tx
                        .send(SessionEvent::Status(ConnStatus::Connecting {
                            peer: cfg.peer,
                        }))
                        .await;
                    match connect(&cfg).await {
                        Ok((rs, _auth, _hosts)) => {
                            let _unused = evt_tx
                                .send(SessionEvent::Status(ConnStatus::Connected {
                                    reflector: cfg.reflector_callsign.to_string(),
                                    module: cfg.reflector_module.as_char(),
                                }))
                                .await;
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
        Some(s) => s.runtime.next_event().await,
        None => std::future::pending().await,
    }
}

/// Establish a `DExtra` / `DPlus` / DCS session per `cfg`.
/// Outcome of the optional `DPlus` authentication step.
#[derive(Debug, Clone, Copy)]
enum AuthOutcome {
    /// Not a `DPlus` session — no auth attempted.
    NotApplicable,
    /// Auth succeeded against the `DPlus` auth server.
    Authenticated,
    /// Auth failed; connecting with an empty host list.
    FellBack,
}

async fn connect(
    cfg: &ConnectConfig,
) -> Result<(RuntimeSession, AuthOutcome, Vec<(String, std::net::IpAddr)>), String> {
    match cfg.protocol {
        ProtocolKind::DExtra => Ok((
            RuntimeSession::DExtra(connect_dextra(cfg).await?),
            AuthOutcome::NotApplicable,
            Vec::new(),
        )),
        ProtocolKind::DPlus => {
            let (session, auth, host_list) = connect_dplus(cfg).await?;
            let hosts = host_list
                .hosts()
                .iter()
                .map(|h| (h.callsign.clone(), h.address))
                .collect();
            Ok((RuntimeSession::DPlus(session), auth, hosts))
        }
        ProtocolKind::Dcs => Ok((
            RuntimeSession::Dcs(connect_dcs(cfg).await?),
            AuthOutcome::NotApplicable,
            Vec::new(),
        )),
        other => Err(format!("unsupported protocol: {other:?}")),
    }
}

/// Bind a fresh ephemeral UDP socket for the session.
async fn bind_session_socket() -> Result<Arc<UdpSocket>, String> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("bind UDP: {e}"))?;
    Ok(Arc::new(sock))
}

/// Drive the protocol-generic UDP handshake loop: pump up to 4
/// `poll_transmit` / `recv` pairs, settling when the core reaches
/// `ClientStateKind::Connected`. Per-protocol `connect_*` functions
/// build the typestate path (auth for `DPlus`, none for `DExtra`/`Dcs`)
/// and then defer the actual datagram exchange here.
async fn run_handshake<P>(
    connecting: &mut Session<P, Connecting>,
    sock: &UdpSocket,
) -> Result<(), String>
where
    P: Protocol,
{
    for _ in 0..4_u8 {
        if let Some(tx) = connecting.poll_transmit(Instant::now()) {
            let _bytes = sock
                .send_to(tx.payload, tx.dst)
                .await
                .map_err(|e| format!("send handshake: {e}"))?;
        }
        let mut buf = [0u8; 128];
        match timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await {
            Ok(Ok((n, src))) => {
                let slice = buf.get(..n).unwrap_or(&[]);
                connecting
                    .handle_input(Instant::now(), src, slice)
                    .map_err(|e| format!("handshake input: {e}"))?;
                if connecting.state_kind() == ClientStateKind::Connected {
                    return Ok(());
                }
            }
            Ok(Err(e)) => return Err(format!("recv handshake: {e}")),
            Err(_) => return Err("handshake timeout".into()),
        }
    }
    if connecting.state_kind() == ClientStateKind::Connected {
        Ok(())
    } else {
        Err("handshake did not reach Connected".into())
    }
}

async fn connect_dextra(cfg: &ConnectConfig) -> Result<AsyncSession<DExtra>, String> {
    let sock = bind_session_socket().await?;
    let configured = Session::<DExtra, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .reflector_callsign(cfg.reflector_callsign)
        .peer(cfg.peer)
        .build();
    let mut connecting = configured
        .connect(Instant::now())
        .map_err(|f| format!("connect: {}", f.error))?;
    run_handshake(&mut connecting, &sock).await?;
    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}

async fn connect_dplus(
    cfg: &ConnectConfig,
) -> Result<
    (
        AsyncSession<DPlus>,
        AuthOutcome,
        dstar_gateway_core::codec::dplus::HostList,
    ),
    String,
> {
    // Local test setups rarely have the DPlus TCP auth server so we
    // treat auth as best-effort: try it first, and if it fails use an
    // empty `HostList` to satisfy the typestate. The UDP handshake is
    // identical to DExtra after that.
    let (host_list, auth) = match dstar_gateway::auth::AuthClient::new()
        .authenticate(cfg.callsign)
        .await
    {
        Ok(h) => (h, AuthOutcome::Authenticated),
        Err(e) => {
            tracing::debug!(?e, "DPlus auth failed — falling back to empty host list");
            (
                dstar_gateway_core::codec::dplus::HostList::new(),
                AuthOutcome::FellBack,
            )
        }
    };
    let sock = bind_session_socket().await?;
    let configured = Session::<DPlus, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .reflector_callsign(cfg.reflector_callsign)
        .peer(cfg.peer)
        .build();
    let authed = configured
        .authenticate(host_list.clone())
        .map_err(|f| format!("authenticate: {}", f.error))?;
    let mut connecting = authed
        .connect(Instant::now())
        .map_err(|f| format!("connect: {}", f.error))?;
    run_handshake(&mut connecting, &sock).await?;
    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok((AsyncSession::spawn(connected, sock), auth, host_list))
}

async fn connect_dcs(cfg: &ConnectConfig) -> Result<AsyncSession<Dcs>, String> {
    let sock = bind_session_socket().await?;
    let configured = Session::<Dcs, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .reflector_callsign(cfg.reflector_callsign)
        .peer(cfg.peer)
        .build();
    let mut connecting = configured
        .connect(Instant::now())
        .map_err(|f| format!("connect: {}", f.error))?;
    run_handshake(&mut connecting, &sock).await?;
    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}

/// Begin a voice TX: allocate a stream-id, build and send the header,
/// return the tracking state. The header is built via
/// [`DStarHeader::for_relay`] so `rpt1[7]` / `rpt2[7]` carry the
/// validated module letters strict reflectors (xlxd-derived) demand.
async fn start_tx(active: &mut ActiveSession) -> Result<TxStream, String> {
    let Some(sid) = StreamId::new(rand_stream_id()) else {
        return Err("stream id zero — retry".into());
    };
    let header = DStarHeader::for_relay(
        active.cfg.callsign,
        active.cfg.local_module,
        active.cfg.reflector_callsign,
        active.cfg.reflector_module,
        active.cfg.callsign,
        Suffix::EMPTY,
    );
    tracing::info!(
        sid = format_args!("{:#06X}", sid.get()),
        my_call = %active.cfg.callsign,
        rpt1 = %active.cfg.callsign,
        rpt1_module = %active.cfg.local_module,
        rpt2 = %active.cfg.reflector_callsign,
        rpt2_module = %active.cfg.reflector_module,
        "TX starting — sending header"
    );
    active
        .runtime
        .send_header(header, sid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(TxStream { sid, seq: 0 })
}

/// TX pipeline sanity check — send `seconds` worth of AMBE silence.
/// Proves header + voice + EOT reach the reflector without needing
/// mic capture or the AMBE encoder. Uses the operator's configured
/// callsign and the same [`DStarHeader::for_relay`] convention as
/// real PTT, so the silence test exercises the same wire identity
/// the operator will be transmitting under.
async fn tx_silence(
    active: &mut ActiveSession,
    seconds: f32,
    evt_tx: &mpsc::Sender<SessionEvent>,
) -> Result<(), String> {
    // D-STAR voice frame rate is 50 fps (20 ms). Clamp sane bounds —
    // 0.2 s minimum (10 frames, enough for a header + EOT pair with a
    // brief gap), 10 s maximum to keep an accidental infinite loop
    // from holding the mic open on a shared reflector.
    let frames_f = (seconds.clamp(0.2, 10.0) * 50.0).round();
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "seconds is clamped to 0.2..=10.0 and multiplied by 50, yielding \
                  10.0..=500.0 before rounding; the result is always a small positive \
                  integer that fits in u32 with no sign loss or truncation."
    )]
    let total_frames = frames_f as u32;

    let Some(sid) = StreamId::new(rand_stream_id()) else {
        return Err("stream id zero — retry".into());
    };

    let header = DStarHeader::for_relay(
        active.cfg.callsign,
        active.cfg.local_module,
        active.cfg.reflector_callsign,
        active.cfg.reflector_module,
        active.cfg.callsign,
        Suffix::EMPTY,
    );

    active
        .runtime
        .send_header(header, sid)
        .await
        .map_err(|e| e.to_string())?;
    let _unused = evt_tx
        .send(SessionEvent::Log(format!(
            "TX: sent header, sending {total_frames} silence frames ({seconds:.1}s)"
        )))
        .await;

    let frame = VoiceFrame {
        ambe: AMBE_SILENCE,
        slow_data: DSTAR_SYNC_BYTES,
    };
    let start = Instant::now();
    // D-STAR encodes seq in the low 6 bits of the wire byte with bit 6
    // reserved as the EOT flag.  Wrapping mod 256 (as a prior revision
    // did) sets bit 6 at `i == 64`, which the reflector parses as an
    // EOT and silently closes the stream — 1.28 s into the helper's
    // supposedly-10-s run.  Wrap mod SUPERFRAME_LEN (21) to match the
    // real-mic TxFrame handler above and stay clear of bit 6.
    for i in 0..total_frames {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Modulo SUPERFRAME_LEN (21) keeps the result in 0..=20, which \
                      trivially fits in u8."
        )]
        let seq = (i % u32::from(SUPERFRAME_LEN)) as u8;
        active
            .runtime
            .send_voice(sid, seq, frame)
            .await
            .map_err(|e| e.to_string())?;
        // Natural 20 ms pacing — avoids flooding the reflector. Real
        // mic capture will inherently pace itself at 50 fps.
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            start + Duration::from_millis(20 * u64::from(i + 1)),
        ))
        .await;
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "Modulo SUPERFRAME_LEN (21) keeps the result in 0..=20, which \
                  trivially fits in u8."
    )]
    let eot_seq = (total_frames % u32::from(SUPERFRAME_LEN)) as u8;
    active
        .runtime
        .send_eot(sid, eot_seq)
        .await
        .map_err(|e| e.to_string())?;
    let _unused = evt_tx
        .send(SessionEvent::Log(format!(
            "TX: sent EOT ({total_frames} frames)"
        )))
        .await;
    Ok(())
}

/// Simple PRNG for stream IDs — doesn't need to be cryptographic. A
/// time-seeded `u16` is plenty to avoid accidental overlap with the
/// previous stream while PTT bounces.
fn rand_stream_id() -> u16 {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    // Map to 1..=0xFFFF to avoid the zero that `StreamId::new` rejects.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Intentional truncation of u32 nanos to u16 — we want the low 16 bits \
                  as a seed for StreamId. OR with 0x1 then .max(1) guarantees non-zero."
    )]
    let v = (nanos as u16) | 0x1;
    v.max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        ConnStatus, EventDecision, EventState, RuntimeEvent, SessionEvent, decide_runtime_event,
    };
    use dstar_gateway_core::session::client::VoiceEndReason;
    use dstar_gateway_core::types::StreamId;
    use dstar_gateway_core::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Compile-time-checked [`StreamId`] constructor for test fixtures.
    /// `Option::unwrap` is const since 1.83, so a zero literal becomes
    /// a compile error — the workspace convention of infallible
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
            3,
            "expected 3 decisions, got {decisions:?}"
        );
        let first = decisions.first().ok_or("no decisions emitted")?;
        assert!(
            matches!(first, EventDecision::EmitSessionEvent(SessionEvent::Log(_))),
            "first decision must be a log line, got {first:?}",
        );
        let second = decisions.get(1).ok_or("missing second decision")?;
        assert!(
            matches!(second, EventDecision::ClearSession),
            "second decision must clear the session so subsequent commands fail loudly, \
             got {second:?}",
        );
        let third = decisions.get(2).ok_or("missing third decision")?;
        assert!(
            matches!(
                third,
                EventDecision::EmitSessionEvent(SessionEvent::Status(ConnStatus::Disconnected))
            ),
            "third decision must flip ConnStatus to Disconnected, got {third:?}",
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
                stream_id: 0x1234,
                ..
            })
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
            rx_stream_id: 0xABCD,
            ..EventState::default()
        };
        // 8 half-blocks (4 blocks × 2 halves) at non-zero seqs;
        // cribbed from the text_collector test fixture.
        let halves: [[u8; 3]; 8] = [
            [0x40, b'C', b'Q'],
            [b' ', b'w', b'o'],
            [0x41, b'r', b'k'],
            [b'i', b'n', b'g'],
            [0x42, b' ', b' '],
            [b' ', b' ', b' '],
            [0x43, b' ', b' '],
            [b' ', b' ', b' '],
        ];
        let mut emitted_message: Option<String> = None;
        for (i, half) in halves.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Test fixture: i is bounded by halves.len()=8, fits in u8."
            )]
            let seq = (i as u8).wrapping_add(1);
            let frame = VoiceFrame {
                ambe: AMBE_SILENCE,
                slow_data: scramble(*half),
            };
            let decisions =
                decide_runtime_event(RuntimeEvent::VoiceFrame { seq, frame }, &mut state);
            for d in decisions {
                if let EventDecision::EmitSessionEvent(SessionEvent::SlowDataMessage {
                    stream_id,
                    text,
                }) = d
                {
                    assert_eq!(
                        stream_id, 0xABCD,
                        "slow-data message tagged with active stream"
                    );
                    emitted_message = Some(text);
                }
            }
        }
        let text = emitted_message.ok_or("no slow-data message emitted after 8 halves")?;
        assert_eq!(text, "CQ working", "trailing spaces trimmed");
        Ok(())
    }

    #[test]
    fn voice_start_resets_slow_data_collector() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;
        let mut state = EventState::default();
        // Push half a message, then start a new stream — the collector
        // must drop the partial state.
        for (i, half) in [[0x40_u8, b'X', b'X'], [b'X', b'X', b'X']]
            .iter()
            .enumerate()
        {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Test fixture: i bounded by 2, fits in u8."
            )]
            let seq = (i as u8).wrapping_add(1);
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
            },
            &mut state,
        );
        // Now feed a fresh 4-block message and verify the previously
        // partially-collected slot doesn't bleed in.
        let halves: [[u8; 3]; 8] = [
            [0x40, b'N', b'E'],
            [b'W', b' ', b' '],
            [0x41, b' ', b' '],
            [b' ', b' ', b' '],
            [0x42, b' ', b' '],
            [b' ', b' ', b' '],
            [0x43, b' ', b' '],
            [b' ', b' ', b' '],
        ];
        let mut text: Option<String> = None;
        for (i, half) in halves.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Test fixture: i bounded by 8, fits in u8."
            )]
            let seq = (i as u8).wrapping_add(1);
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
                    text: t,
                    ..
                }) = d
                {
                    text = Some(t);
                }
            }
        }
        let final_text = text.ok_or("no slow-data message emitted after fresh stream")?;
        assert_eq!(
            final_text, "NEW",
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
        let first = decisions.first().ok_or("no decisions emitted")?;
        let EventDecision::EmitSessionEvent(SessionEvent::VoiceEnd {
            stream_id, frames, ..
        }) = first
        else {
            return Err(format!("expected VoiceEnd decision, got {first:?}").into());
        };
        assert_eq!(*stream_id, 0x9ABC);
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
            RuntimeEvent::Other("PollEcho { peer: 127.0.0.1:30001 }".into()),
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

    /// Feeding the fixed-6-byte-block GPS slow-data for a complete
    /// DPRS sentence must surface a `GpsPosition` decision once the
    /// sentence terminator (`\r`) lands. The sentence literal is the
    /// Asheville example from the `dprs` parser's own test corpus
    /// (35°30.00'N / 82°33.00'W).
    #[test]
    fn slow_data_assembles_gps_position() -> TestResult {
        use dstar_gateway_core::slowdata::scramble;

        let sentence: &[u8] =
            b"$$CRC0000,W1AW    *>APDPRS,DSTAR*:!3530.00N/08233.00W#/Asheville test\r";
        let mut state = EventState {
            rx_stream_id: 0x5678,
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
                        latitude,
                        longitude,
                        ..
                    }) = d
                    {
                        emitted = Some((latitude, longitude));
                    }
                }
                seq = if seq >= 20 { 1 } else { seq + 1 };
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
}
