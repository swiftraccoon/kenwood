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
use dstar_gateway::tokio_shell::{AsyncSession, ShellError};
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Configured, DExtra, DPlus, Dcs, Event, Session, VoiceEndReason,
};
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
use dstar_gateway_core::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;

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
        frame: VoiceFrame,
    },
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

    fn from_event<P: dstar_gateway_core::session::client::Protocol + std::fmt::Debug>(
        ev: Event<P>,
    ) -> Self {
        match ev {
            Event::VoiceStart {
                stream_id, header, ..
            } => Self::VoiceStart {
                stream_id,
                my_call: header.my_call.to_string(),
            },
            Event::VoiceEnd { stream_id, reason } => Self::VoiceEnd { stream_id, reason },
            Event::VoiceFrame { frame, .. } => Self::VoiceFrame { frame },
            other => Self::Other(format!("{other:?}")),
        }
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
    let mut session: Option<RuntimeSession> = None;
    // Counts frames on the currently-observed INCOMING stream; reset
    // when the stream ends.
    let mut rx_frame_count: u32 = 0;
    // Counts frames on the currently-transmitting OUTGOING stream;
    // reset between streams.
    let mut tx_frame_count: u32 = 0;
    // Active outgoing TX stream — `Some` between `StartTx` and `EndTx`.
    let mut tx_stream: Option<TxStream> = None;

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
                            Ok(rs) => {
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
                                session = Some(rs);
                            }
                            Err(e) => {
                                let _unused = evt_tx.send(SessionEvent::Error(format!("connect failed: {e}"))).await;
                                let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                            }
                        }
                    }
                    SessionCommand::Disconnect => {
                        if let Some(mut rs) = session.take() {
                            let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnecting)).await;
                            if let Err(e) = rs.disconnect().await {
                                let _unused = evt_tx.send(SessionEvent::Log(format!("disconnect: {e}"))).await;
                            }
                            let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                        }
                    }
                    SessionCommand::TxSilence { seconds } => {
                        let Some(rs) = session.as_mut() else {
                            let _unused = evt_tx.send(SessionEvent::Log("TX: not connected".into())).await;
                            continue;
                        };
                        if let Err(e) = tx_silence(rs, seconds, &evt_tx).await {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TX error: {e}"))).await;
                        }
                    }
                    SessionCommand::StartTx { my_call } => {
                        let Some(rs) = session.as_mut() else {
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: not connected".into())).await;
                            continue;
                        };
                        if tx_stream.is_some() {
                            let _unused = evt_tx.send(SessionEvent::Log("StartTx: already transmitting — ignoring".into())).await;
                            continue;
                        }
                        match start_tx(rs, &my_call).await {
                            Ok(ts) => {
                                let _unused = evt_tx.send(SessionEvent::Log(format!(
                                    "TX started sid=0x{:04X} my_call={my_call}",
                                    ts.sid.get()
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
                        let Some(rs) = session.as_mut() else { continue };
                        let Some(ts) = tx_stream.as_mut() else { continue };
                        let seq = ts.seq;
                        tx_frame_count = tx_frame_count.saturating_add(1);
                        tracing::trace!(
                            sid = format_args!("{:#06X}", ts.sid.get()),
                            seq,
                            frame_num = tx_frame_count,
                            "TX voice frame"
                        );
                        if let Err(e) = rs.send_voice(ts.sid, seq, frame).await {
                            let _unused = evt_tx.send(SessionEvent::Error(format!("TxFrame: {e}"))).await;
                        }
                        ts.seq = (ts.seq + 1) % SUPERFRAME_LEN;
                    }
                    SessionCommand::EndTx => {
                        if let (Some(rs), Some(ts)) = (session.as_mut(), tx_stream.take()) {
                            let seq = ts.seq;
                            tracing::info!(
                                sid = format_args!("{:#06X}", ts.sid.get()),
                                eot_seq = seq,
                                frames = tx_frame_count,
                                "TX ending — sending EOT"
                            );
                            if let Err(e) = rs.send_eot(ts.sid, seq).await {
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
                match event {
                    Some(RuntimeEvent::VoiceStart { stream_id, my_call }) => {
                        rx_frame_count = 0;
                        tracing::info!(
                            sid = format_args!("{:#06X}", stream_id.get()),
                            from = %my_call,
                            "RX VoiceStart"
                        );
                        // Reset the decoder directly on the audio
                        // worker — don't route through the GUI.
                        audio.send(AudioCommand::RxStart);
                        let _unused = evt_tx.send(SessionEvent::VoiceStart {
                            stream_id: stream_id.get(),
                            from: my_call,
                        }).await;
                    }
                    Some(RuntimeEvent::VoiceFrame { frame }) => {
                        rx_frame_count = rx_frame_count.saturating_add(1);
                        tracing::trace!(frame_num = rx_frame_count, "RX voice frame");
                        // Route the raw AMBE directly to the audio
                        // worker (no GUI hop) so decode and playback
                        // aren't gated on the egui repaint cadence.
                        audio.send(AudioCommand::RxFrame(frame));
                    }
                    Some(RuntimeEvent::VoiceEnd { stream_id, reason }) => {
                        tracing::info!(
                            sid = format_args!("{:#06X}", stream_id.get()),
                            frames = rx_frame_count,
                            ?reason,
                            "RX VoiceEnd"
                        );
                        let _unused = evt_tx.send(SessionEvent::VoiceEnd {
                            stream_id: stream_id.get(),
                            frames: rx_frame_count,
                            reason: format!("{reason:?}"),
                        }).await;
                        rx_frame_count = 0;
                    }
                    Some(RuntimeEvent::Other(s)) => {
                        let _unused = evt_tx.send(SessionEvent::Log(format!("event: {s}"))).await;
                    }
                    None => {
                        // Session ended / channel closed.
                        let _unused = evt_tx.send(SessionEvent::Log("session event channel closed".into())).await;
                        session = None;
                        let _unused = evt_tx.send(SessionEvent::Status(ConnStatus::Disconnected)).await;
                    }
                }
            }
        }
    }
}

/// Returns `next_event()` on the active session, or `pending` forever
/// when no session is active (disables the select branch cleanly).
async fn next_event_opt(session: Option<&mut RuntimeSession>) -> Option<RuntimeEvent> {
    match session {
        Some(s) => s.next_event().await,
        None => std::future::pending().await,
    }
}

/// Establish a `DExtra` / `DPlus` / DCS session per `cfg`.
async fn connect(cfg: &ConnectConfig) -> Result<RuntimeSession, String> {
    match cfg.protocol {
        ProtocolKind::DExtra => Ok(RuntimeSession::DExtra(connect_dextra(cfg).await?)),
        ProtocolKind::DPlus => Ok(RuntimeSession::DPlus(connect_dplus(cfg).await?)),
        ProtocolKind::Dcs => Ok(RuntimeSession::Dcs(connect_dcs(cfg).await?)),
        other => Err(format!("unsupported protocol: {other:?}")),
    }
}

async fn connect_dextra(cfg: &ConnectConfig) -> Result<AsyncSession<DExtra>, String> {
    let sock = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("bind UDP: {e}"))?,
    );
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
                    break;
                }
            }
            Ok(Err(e)) => return Err(format!("recv handshake: {e}")),
            Err(_) => return Err("handshake timeout".into()),
        }
    }

    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("handshake did not reach Connected".into());
    }

    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}

async fn connect_dplus(cfg: &ConnectConfig) -> Result<AsyncSession<DPlus>, String> {
    // Local test setups rarely have the DPlus TCP auth server so we
    // treat auth as best-effort: try it first, and if it fails use an
    // empty `HostList` to satisfy the typestate. The UDP handshake is
    // identical to DExtra after that.
    let host_list = match dstar_gateway::auth::AuthClient::new()
        .authenticate(cfg.callsign)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(?e, "DPlus auth failed — falling back to empty host list");
            dstar_gateway_core::codec::dplus::HostList::new()
        }
    };

    let sock = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("bind UDP: {e}"))?,
    );

    let configured = Session::<DPlus, Configured>::builder()
        .callsign(cfg.callsign)
        .local_module(cfg.local_module)
        .reflector_module(cfg.reflector_module)
        .reflector_callsign(cfg.reflector_callsign)
        .peer(cfg.peer)
        .build();
    let authed = configured
        .authenticate(host_list)
        .map_err(|f| format!("authenticate: {}", f.error))?;
    let mut connecting = authed
        .connect(Instant::now())
        .map_err(|f| format!("connect: {}", f.error))?;

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
                    break;
                }
            }
            Ok(Err(e)) => return Err(format!("recv handshake: {e}")),
            Err(_) => return Err("handshake timeout".into()),
        }
    }

    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("DPlus handshake did not reach Connected".into());
    }

    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}

async fn connect_dcs(cfg: &ConnectConfig) -> Result<AsyncSession<Dcs>, String> {
    let sock = Arc::new(
        UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| format!("bind UDP: {e}"))?,
    );
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
                    break;
                }
            }
            Ok(Err(e)) => return Err(format!("recv handshake: {e}")),
            Err(_) => return Err("handshake timeout".into()),
        }
    }
    if connecting.state_kind() != ClientStateKind::Connected {
        return Err("DCS handshake did not reach Connected".into());
    }
    let connected = connecting
        .promote()
        .map_err(|f| format!("promote: {}", f.error))?;
    Ok(AsyncSession::spawn(connected, sock))
}

/// Begin a voice TX: allocate a stream-id, build and send the header,
/// return the tracking state. `my_call` is the operator callsign the
/// reflector will surface to other participants.
async fn start_tx(session: &mut RuntimeSession, my_call: &str) -> Result<TxStream, String> {
    let Some(sid) = StreamId::new(rand_stream_id()) else {
        return Err("stream id zero — retry".into());
    };
    let header = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        rpt1: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        ur_call: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        my_call: Callsign::try_from_str(my_call).map_err(|e| e.to_string())?,
        my_suffix: Suffix::EMPTY,
    };
    tracing::info!(
        sid = format_args!("{:#06X}", sid.get()),
        my_call,
        "TX starting — sending header"
    );
    session
        .send_header(header, sid)
        .await
        .map_err(|e| e.to_string())?;
    Ok(TxStream { sid, seq: 0 })
}

/// TX pipeline sanity check — send `seconds` worth of AMBE silence.
/// Proves header + voice + EOT reach the reflector without needing
/// mic capture or the AMBE encoder. Once the audio worker lands this
/// becomes one of several TX paths.
async fn tx_silence(
    session: &mut RuntimeSession,
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

    // Build a minimal CQCQCQ header. `rpt1`/`rpt2` follow the xlxd /
    // ircDDBGateway convention — operator callsign + local module in
    // rpt1, reflector callsign + reflector module in rpt2. Reflectors
    // silently drop packets whose `rpt1[7]` isn't a valid module letter.
    let header = DStarHeader {
        flag1: 0,
        flag2: 0,
        flag3: 0,
        rpt2: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        rpt1: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        ur_call: Callsign::try_from_str("CQCQCQ").map_err(|e| e.to_string())?,
        my_call: Callsign::try_from_str("SEXTANT").map_err(|e| e.to_string())?,
        my_suffix: Suffix::EMPTY,
    };

    session
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
        session
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
    session
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
