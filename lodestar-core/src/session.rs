// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Reflector session wrapper for the Rust-to-Swift FFI boundary.
//!
//! Each connect spawns a tokio task that owns an
//! `dstar_gateway::tokio_shell::AsyncSession<P>` for one of the three
//! D-STAR reflector protocols (`DPlus` / `DExtra` / `Dcs`). The task
//! pumps `AsyncSession::next_event()` in a loop and dispatches
//! translated events to a Swift-implemented [`ReflectorObserver`]
//! callback. A oneshot channel carries the graceful-disconnect signal
//! from the foreground `ReflectorSession` handle to the task.
//!
//! `DPlus` also runs the [`AuthClient`] TCP handshake before the UDP
//! connect; failure falls back to an empty host list so a stale but
//! valid auth on the reflector can still land the UDP LINK.

use std::sync::Arc;

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::header::{DStarHeader, ENCODED_LEN as HEADER_ENCODED_LEN};
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Connected, Connecting, DExtra, DPlus, Dcs, DisconnectReason, Event, Session,
    VoiceEndReason,
};
use dstar_gateway_core::slowdata::{SlowDataTextCollector, descramble};
use dstar_gateway_core::voice::VoiceFrame;
use dstar_gateway_core::{Callsign, Module, StreamId, parse_dprs};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::reflector::{Reflector, ReflectorProtocol};

/// Handshake timeout — how long we wait for the reflector to ACK the LINK
/// before giving up.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Opaque [`UniFFI`](https://mozilla.github.io/uniffi-rs/) handle to a
/// live (or previously-live) reflector session.
#[derive(uniffi::Object)]
pub struct ReflectorSession {
    backend: Mutex<Option<Backend>>,
    callsign: String,
    reflector_name: String,
    local_module: String,
    reflector_module: String,
    protocol: ReflectorProtocol,
    // Typed copies of the identity strings, validated at connect time
    // and reused on every outbound header rewrite. Built here so the
    // `send_header` hot path never has to re-parse.
    station_callsign: Callsign,
    local_module_typed: Module,
    reflector_callsign: Callsign,
    reflector_module_typed: Module,
}

impl std::fmt::Debug for ReflectorSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReflectorSession")
            .field("callsign", &self.callsign)
            .field("reflector_name", &self.reflector_name)
            .field("local_module", &self.local_module)
            .field("reflector_module", &self.reflector_module)
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl ReflectorSession {
    /// Rewrite a radio-emitted D-STAR header for reflector ingestion.
    ///
    /// - `rpt1[0..7]` = operator callsign (7 bytes, space-padded).
    /// - `rpt1[7]`    = local module letter.
    /// - `rpt2[0..7]` = reflector callsign (e.g. `"REF030 "`).
    /// - `rpt2[7]`    = reflector module letter.
    /// - `ur_call`    = `"CQCQCQ"` (hotspot-to-reflector convention).
    ///
    /// `my_call` / `my_suffix` / flags pass through unchanged — they
    /// identify the operator and their tail (`/D75`, `/M`, etc.).
    fn build_reflector_header(&self, radio: &DStarHeader) -> DStarHeader {
        let mut rpt1_buf = [b' '; 8];
        let cs_bytes = self.station_callsign.as_bytes();
        rpt1_buf[..7].copy_from_slice(&cs_bytes[..7]);
        rpt1_buf[7] = self.local_module_typed.as_byte();
        let rpt1 = Callsign::from_wire_bytes(rpt1_buf);

        let mut rpt2_buf = [b' '; 8];
        let refl_bytes = self.reflector_callsign.as_bytes();
        rpt2_buf[..7].copy_from_slice(&refl_bytes[..7]);
        rpt2_buf[7] = self.reflector_module_typed.as_byte();
        let rpt2 = Callsign::from_wire_bytes(rpt2_buf);

        // `try_from_str("CQCQCQ")` is infallible for a valid static
        // string, but const construction needs a wire-byte literal.
        let ur_call = Callsign::from_wire_bytes(*b"CQCQCQ  ");

        DStarHeader {
            flag1: radio.flag1,
            flag2: radio.flag2,
            flag3: radio.flag3,
            rpt2,
            rpt1,
            ur_call,
            my_call: radio.my_call,
            my_suffix: radio.my_suffix,
        }
    }
}

/// Background task handle for a live reflector session.
struct Backend {
    shutdown: oneshot::Sender<()>,
    join: JoinHandle<Result<(), String>>,
    tx: tokio::sync::mpsc::Sender<TxCommand>,
}

/// TX commands the foreground session sends to its background task.
#[derive(Debug)]
enum TxCommand {
    Header {
        header: Box<DStarHeader>,
        stream_id: StreamId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Voice {
        stream_id: StreamId,
        seq: u8,
        frame: VoiceFrame,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Eot {
        stream_id: StreamId,
        seq: u8,
        /// Reply carries the assembled 20-char TX text (if the radio
        /// sent a complete 4-block message during the stream) alongside
        /// the shell result so Swift can include it in the local
        /// recently-heard entry. `None` if no complete text was seen.
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
}

/// Fields decoded from a 41-byte on-wire D-STAR header.
///
/// Used by the Swift side to surface "who am I transmitting as" when
/// a local TX is relayed to the reflector. Reflectors typically don't
/// echo our own packets back, so we synthesise a recently-heard entry
/// for our own relayed stream instead.
#[derive(Debug, Clone, uniffi::Record)]
pub struct DecodedRadioHeader {
    /// Trimmed operator callsign (`MY`).
    pub mycall: String,
    /// Trimmed 4-char operator suffix.
    pub suffix: String,
    /// Trimmed destination / routing callsign (`URCALL`).
    pub urcall: String,
    /// Trimmed access-repeater callsign (`RPT1`).
    pub rpt1: String,
    /// Trimmed gateway-repeater callsign (`RPT2`).
    pub rpt2: String,
}

/// Decode a 41-byte on-wire D-STAR header emitted by the TH-D75 in
/// Reflector Terminal Mode. Returns `None` for any wrong-length input.
#[uniffi::export]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary requires owned Vec<u8>"
)]
#[must_use]
pub fn decode_radio_header(bytes: Vec<u8>) -> Option<DecodedRadioHeader> {
    if bytes.len() != HEADER_ENCODED_LEN {
        return None;
    }
    let mut arr = [0u8; HEADER_ENCODED_LEN];
    arr.copy_from_slice(&bytes);
    let h = DStarHeader::decode(&arr);
    Some(DecodedRadioHeader {
        mycall: h.my_call.as_str().trim().to_owned(),
        suffix: h.my_suffix.as_str().trim().to_owned(),
        urcall: h.ur_call.as_str().trim().to_owned(),
        rpt1: h.rpt1.as_str().trim().to_owned(),
        rpt2: h.rpt2.as_str().trim().to_owned(),
    })
}

/// Parsed DPRS position report carried by slow-data GPS blocks.
///
/// All coordinates are decimal degrees; negative values are south/west.
/// `callsign`, `symbol`, and `comment` are populated when the DPRS
/// sentence decodes cleanly — `symbol` is a single APRS symbol char
/// rendered as a one-character string for FFI simplicity.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct GpsPosition {
    /// Reporting station callsign, trimmed.
    pub callsign: String,
    /// Latitude in decimal degrees (north positive).
    pub latitude: f64,
    /// Longitude in decimal degrees (east positive).
    pub longitude: f64,
    /// APRS symbol character (e.g. `"["` for a jogger).
    pub symbol: String,
    /// Free-form comment appended to the DPRS sentence, if any.
    pub comment: Option<String>,
}

/// Translated reflector event surfaced to the Swift observer.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum ReflectorEvent {
    /// The reflector acknowledged our LINK/CONNECT and we are live.
    Connected,
    /// Connection ended. `reason` is a human-readable rendering of
    /// [`DisconnectReason`] (rejected, unlink-acked, keepalive-timeout,
    /// disconnect-timeout).
    Disconnected {
        /// Human-readable disconnect reason.
        reason: String,
    },
    /// `DPlus` keepalive bounce — useful as a "still alive" signal.
    PollEcho,
    /// A remote station started transmitting.
    VoiceStart {
        /// Sender-chosen stream identifier (non-zero u16).
        stream_id: u16,
        /// Transmitting station's callsign (MY).
        mycall: String,
        /// Transmitting station's 4-char suffix.
        suffix: String,
        /// Destination / routing callsign (URCALL).
        urcall: String,
        /// Access repeater callsign (RPT1).
        rpt1: String,
        /// Gateway repeater callsign (RPT2).
        rpt2: String,
        /// Raw 41-byte on-wire D-STAR header. Ready to wrap as an
        /// MMDVM `DStarHeader` frame (command 0x10) and send to the radio.
        header_bytes: Vec<u8>,
    },
    /// A voice frame arrived mid-stream.
    VoiceFrame {
        /// Stream identifier this frame belongs to.
        stream_id: u16,
        /// Sequence number within the stream.
        seq: u8,
        /// Raw 12-byte voice frame (9 bytes AMBE + 3 bytes slow-data).
        /// Ready to wrap as an MMDVM `DStarData` frame (command 0x11)
        /// and send to the radio.
        voice_bytes: Vec<u8>,
    },
    /// Slow-data metadata for the current stream updated. Fires any
    /// time the assembled text or parsed GPS position changes — the
    /// UI uses it to show live "now transmitting" details before the
    /// stream ends.
    SlowDataUpdate {
        /// Stream identifier this update belongs to.
        stream_id: u16,
        /// Latest assembled 20-byte TX message, if any. Sticky: once
        /// set, continues to be reported on every subsequent update
        /// for this stream.
        text: Option<String>,
        /// Latest parsed DPRS position, if any. Sticky, same as `text`.
        position: Option<GpsPosition>,
    },
    /// The current voice stream ended.
    VoiceEnd {
        /// Stream identifier that ended.
        stream_id: u16,
        /// Why the stream ended — `"eot"` (real EOT) or `"inactivity"`.
        reason: String,
        /// Slow-data text assembled over the stream, if a complete
        /// 20-byte message arrived. This is the "TX message" operators
        /// set on their radios (Kenwood's stored comment field) and
        /// the commonest reason to show "recently heard" entries with
        /// context beyond the callsign.
        text: Option<String>,
        /// Final parsed DPRS position seen on the stream, if any.
        position: Option<GpsPosition>,
    },
    /// The background task ended — session is finished, no further events.
    Ended,
}

/// Foreign-implemented trait that receives translated reflector events.
///
/// Swift implements this protocol and passes an instance when calling
/// [`connect_reflector`]. The background task holds the observer via
/// `Arc<dyn ReflectorObserver>` and releases it when the session ends.
#[uniffi::export(with_foreign)]
pub trait ReflectorObserver: Send + Sync + std::fmt::Debug {
    /// Called once per reflector event. Must not block for long — the
    /// session task awaits completion before pumping the next event.
    fn on_event(&self, event: ReflectorEvent);
}

/// Error variants surfaced across the FFI boundary.
#[derive(Debug, Error, uniffi::Error)]
pub enum ReflectorError {
    /// Caller-supplied callsign is not a valid D-STAR callsign.
    #[error("invalid callsign: {0}")]
    InvalidCallsign(String),
    /// Module letter was missing or not A-Z.
    #[error("invalid module letter: {0}")]
    InvalidModule(String),
    /// DNS lookup for the reflector host failed.
    #[error("DNS lookup failed: {0}")]
    DnsFailed(String),
    /// UDP socket bind failed.
    #[error("socket bind failed: {0}")]
    SocketBindFailed(String),
    /// Session builder rejected the parameters.
    #[error("session build failed: {0}")]
    BuildFailed(String),
    /// `DPlus` `AuthClient` TCP handshake failed.
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// Handshake did not complete within the timeout.
    #[error("connect failed: {0}")]
    ConnectFailed(String),
    /// Handshake timed out.
    #[error("handshake timed out")]
    HandshakeTimeout,
    /// Session is already disconnected.
    #[error("already disconnected")]
    AlreadyDisconnected,
    /// Underlying `dstar-gateway` shell returned an error.
    #[error("session shell error: {0}")]
    Shell(String),
    /// Background task died without a clean shutdown signal.
    #[error("session task panicked: {0}")]
    TaskPanic(String),
    /// Stream ID was zero (not allowed by `StreamId`).
    #[error("stream ID cannot be zero")]
    ZeroStreamId,
    /// D-STAR header payload was not exactly 41 bytes.
    #[error("D-STAR header must be {expected} bytes, got {got}")]
    BadHeaderLength {
        /// Required length (41).
        expected: u32,
        /// Supplied length.
        got: u32,
    },
    /// Voice-frame payload was not exactly 12 bytes (9 AMBE + 3 slow-data).
    #[error("voice frame must be 12 bytes, got {got}")]
    BadVoiceLength {
        /// Supplied length.
        got: u32,
    },
    /// Voice frame must have 9 AMBE bytes + 3 slow-data bytes = 12 total.
    #[error("cannot send TX frames on a background-owned session outside its task")]
    SessionInBackground,
}

#[uniffi::export(async_runtime = "tokio")]
impl ReflectorSession {
    /// The operator callsign this session identifies as.
    #[must_use]
    pub fn callsign(self: Arc<Self>) -> String {
        self.callsign.clone()
    }

    /// Name of the reflector we're connected to.
    #[must_use]
    pub fn reflector_name(self: Arc<Self>) -> String {
        self.reflector_name.clone()
    }

    /// Local module letter.
    #[must_use]
    pub fn local_module(self: Arc<Self>) -> String {
        self.local_module.clone()
    }

    /// Reflector module letter.
    #[must_use]
    pub fn reflector_module(self: Arc<Self>) -> String {
        self.reflector_module.clone()
    }

    /// Which D-STAR protocol this session uses.
    #[must_use]
    pub fn protocol(self: Arc<Self>) -> ReflectorProtocol {
        self.protocol
    }

    /// `true` while the background task is still running.
    pub async fn is_connected(self: Arc<Self>) -> bool {
        self.backend.lock().await.is_some()
    }

    /// Send a D-STAR header opening a new outbound voice stream.
    ///
    /// `header_bytes` must be exactly 41 bytes (the on-wire D-STAR
    /// header format used by MMDVM). `stream_id` must be non-zero.
    ///
    /// # Errors
    ///
    /// - [`ReflectorError::BadHeaderLength`] / [`ReflectorError::ZeroStreamId`]
    ///   for invalid inputs.
    /// - [`ReflectorError::AlreadyDisconnected`] if the session has been torn down.
    /// - [`ReflectorError::Shell`] for failures inside `dstar-gateway`.
    pub async fn send_header(
        self: Arc<Self>,
        header_bytes: Vec<u8>,
        stream_id: u16,
    ) -> Result<(), ReflectorError> {
        if header_bytes.len() != HEADER_ENCODED_LEN {
            return Err(ReflectorError::BadHeaderLength {
                expected: u32::try_from(HEADER_ENCODED_LEN).unwrap_or(u32::MAX),
                got: u32::try_from(header_bytes.len()).unwrap_or(u32::MAX),
            });
        }
        let sid = StreamId::new(stream_id).ok_or(ReflectorError::ZeroStreamId)?;
        let mut arr = [0u8; HEADER_ENCODED_LEN];
        arr.copy_from_slice(&header_bytes);
        let radio_header = DStarHeader::decode(&arr);

        // The TH-D75 in Reflector Terminal Mode emits its TX header
        // with rpt1/rpt2 both set to the literal `"DIRECT  "` (the
        // radio doesn't know the gateway's callsign). xlxd and
        // ircDDBGateway read rpt1[7]/rpt2[7] as the module letter and
        // silently drop any packet where it isn't a valid module —
        // `"DIRECT  "` has 'T' at byte 7, so the reflector eats the
        // stream without a NAK or log line. We rewrite rpt1/rpt2 /
        // ur_call to the hotspot-to-reflector convention here.
        //
        // See `thd75-repl::build_reflector_header` (main.rs) for the
        // verified-working reference implementation.
        let ref_header = self.build_reflector_header(&radio_header);

        let tx = self
            .backend
            .lock()
            .await
            .as_ref()
            .ok_or(ReflectorError::AlreadyDisconnected)?
            .tx
            .clone();
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(TxCommand::Header {
            header: Box::new(ref_header),
            stream_id: sid,
            reply: reply_tx,
        })
        .await
        .map_err(|_| ReflectorError::AlreadyDisconnected)?;
        reply_rx
            .await
            .map_err(|_| ReflectorError::AlreadyDisconnected)?
            .map_err(ReflectorError::Shell)
    }

    /// Send a single 12-byte voice frame (9 AMBE + 3 slow-data).
    ///
    /// # Errors
    ///
    /// See [`ReflectorSession::send_header`].
    pub async fn send_voice(
        self: Arc<Self>,
        stream_id: u16,
        seq: u8,
        voice_bytes: Vec<u8>,
    ) -> Result<(), ReflectorError> {
        if voice_bytes.len() != 12 {
            return Err(ReflectorError::BadVoiceLength {
                got: u32::try_from(voice_bytes.len()).unwrap_or(u32::MAX),
            });
        }
        let sid = StreamId::new(stream_id).ok_or(ReflectorError::ZeroStreamId)?;
        let mut ambe = [0u8; 9];
        let mut slow = [0u8; 3];
        // `.get(range)` returns Option instead of panicking; the length
        // check above guarantees both slices are present.
        let ambe_src = voice_bytes
            .get(..9)
            .ok_or(ReflectorError::BadVoiceLength { got: 0 })?;
        let slow_src = voice_bytes
            .get(9..12)
            .ok_or(ReflectorError::BadVoiceLength { got: 0 })?;
        ambe.copy_from_slice(ambe_src);
        slow.copy_from_slice(slow_src);
        let frame = VoiceFrame {
            ambe,
            slow_data: slow,
        };
        let tx = self
            .backend
            .lock()
            .await
            .as_ref()
            .ok_or(ReflectorError::AlreadyDisconnected)?
            .tx
            .clone();
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(TxCommand::Voice {
            stream_id: sid,
            seq,
            frame,
            reply: reply_tx,
        })
        .await
        .map_err(|_| ReflectorError::AlreadyDisconnected)?;
        reply_rx
            .await
            .map_err(|_| ReflectorError::AlreadyDisconnected)?
            .map_err(ReflectorError::Shell)
    }

    /// Send an EOT packet ending the current outbound stream.
    ///
    /// Returns the assembled 20-char "TX message" the radio sent in
    /// slow data during the stream (the Kenwood 4-block text), or
    /// `None` if no complete message was received. Swift uses this
    /// for the local recently-heard entry.
    ///
    /// # Errors
    ///
    /// See [`ReflectorSession::send_header`].
    pub async fn send_eot(
        self: Arc<Self>,
        stream_id: u16,
        seq: u8,
    ) -> Result<Option<String>, ReflectorError> {
        let sid = StreamId::new(stream_id).ok_or(ReflectorError::ZeroStreamId)?;
        let tx = self
            .backend
            .lock()
            .await
            .as_ref()
            .ok_or(ReflectorError::AlreadyDisconnected)?
            .tx
            .clone();
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(TxCommand::Eot {
            stream_id: sid,
            seq,
            reply: reply_tx,
        })
        .await
        .map_err(|_| ReflectorError::AlreadyDisconnected)?;
        reply_rx
            .await
            .map_err(|_| ReflectorError::AlreadyDisconnected)?
            .map_err(ReflectorError::Shell)
    }

    /// Graceful disconnect. Signals the background task to send the
    /// goodbye packet and then awaits the task's completion.
    ///
    /// Idempotent: a second call returns
    /// [`ReflectorError::AlreadyDisconnected`].
    ///
    /// # Errors
    ///
    /// - [`ReflectorError::AlreadyDisconnected`] if the session has
    ///   already been torn down.
    /// - [`ReflectorError::Shell`] if the background task's disconnect
    ///   attempt returned an error from `dstar-gateway`.
    /// - [`ReflectorError::TaskPanic`] if the task panicked.
    pub async fn disconnect(self: Arc<Self>) -> Result<(), ReflectorError> {
        let backend = {
            let mut guard = self.backend.lock().await;
            guard.take().ok_or(ReflectorError::AlreadyDisconnected)?
        };
        // Best-effort shutdown signal — if the receiver already dropped,
        // the task is exiting on its own and we just await its result.
        let _ = backend.shutdown.send(());
        match backend.join.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(msg)) => Err(ReflectorError::Shell(msg)),
            Err(join_err) => Err(ReflectorError::TaskPanic(format!("{join_err}"))),
        }
    }
}

/// Connect to a reflector and return an opaque session handle.
///
/// Drives the full per-protocol handshake synchronously, then spawns a
/// tokio task that owns the session and pumps events into `observer`.
/// The returned `ReflectorSession` is already in the Connected state.
///
/// # Errors
///
/// Returns [`ReflectorError`] variants describing whichever step of
/// the connect flow failed — invalid input, DNS, socket bind, session
/// builder rejection, reflector handshake failure, or timeout.
#[uniffi::export(async_runtime = "tokio")]
pub async fn connect_reflector(
    callsign: String,
    reflector: Reflector,
    local_module: String,
    reflector_module: String,
    observer: Arc<dyn ReflectorObserver>,
) -> Result<Arc<ReflectorSession>, ReflectorError> {
    debug!(
        target: "lodestar_core::session",
        callsign = %callsign,
        reflector = %reflector.name,
        protocol = ?reflector.protocol,
        "connect_reflector invoked"
    );

    let station = Callsign::try_from_str(&callsign)
        .map_err(|e| ReflectorError::InvalidCallsign(format!("{callsign}: {e}")))?;
    let ref_cs = Callsign::try_from_str(&reflector.name)
        .map_err(|e| ReflectorError::InvalidCallsign(format!("{}: {e}", reflector.name)))?;
    let local = parse_module(&local_module)?;
    let remote = parse_module(&reflector_module)?;

    let peer = resolve_peer(&reflector).await?;
    let socket = bind_local_socket().await?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (tx_cmd, tx_rx) = tokio::sync::mpsc::channel::<TxCommand>(64);

    let join = match reflector.protocol {
        ReflectorProtocol::DExtra => {
            let session =
                connect_dextra(station, peer, local, remote, ref_cs, socket.clone()).await?;
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
        ReflectorProtocol::Dcs => {
            let session = connect_dcs(station, peer, local, remote, ref_cs, socket.clone()).await?;
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
        ReflectorProtocol::DPlus => {
            let session =
                connect_dplus(station, peer, local, remote, ref_cs, socket.clone()).await?;
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
    };

    Ok(Arc::new(ReflectorSession {
        backend: Mutex::new(Some(Backend {
            shutdown: shutdown_tx,
            join,
            tx: tx_cmd,
        })),
        callsign,
        reflector_name: reflector.name,
        local_module,
        reflector_module,
        protocol: reflector.protocol,
        station_callsign: station,
        local_module_typed: local,
        reflector_callsign: ref_cs,
        reflector_module_typed: remote,
    }))
}

// ---------------------------------------------------------------------------
// Background task
// ---------------------------------------------------------------------------

/// TX-side slow-data text tracker. Assembles the outgoing radio's
/// Kenwood 4-block text (the 20-char "TX message") so it can be
/// reported back to Swift when the stream ends — used for local
/// recently-heard entries.
///
/// Outbound `slow_data` bytes are SCRAMBLED (wire format) because the
/// radio emits them that way in MMDVM `DStarData` frames. The Kenwood
/// D-STAR sync frame carries `[0x55, 0x55, 0x55]` plain, which lines
/// up with the collector's `frame_index == 0` resync trigger — we
/// detect sync frames by matching the wire bytes directly rather
/// than relying on our ad-hoc outbound seq counter (which is monotonic
/// and doesn't wrap on superframe boundaries).
#[derive(Default)]
struct TxTextState {
    collector: SlowDataTextCollector,
    latest: Option<String>,
    frame_index: u8,
}

impl TxTextState {
    fn ingest(&mut self, slow_data: [u8; 3]) {
        // Detect D-STAR superframe sync by the raw wire bytes; the
        // radio always emits 0x55,0x55,0x55 as slow-data filler on
        // sync frames. When we see it, feed the collector with
        // frame_index==0 so it resets its half-block phase.
        let index = if slow_data == [0x55, 0x55, 0x55] {
            self.frame_index = 0;
            0
        } else {
            self.frame_index = self.frame_index.wrapping_add(1).max(1);
            self.frame_index
        };
        self.collector.push(slow_data, index);
        if let Some(msg) = self.collector.take_message()
            && let Some(cleaned) = clean_slow_data_text(&msg)
        {
            self.latest = Some(cleaned);
        }
    }

    fn take_text(&mut self) -> Option<String> {
        self.latest.take()
    }
}

/// Per-stream slow-data dispatcher (Kenwood / ircDDBGateway protocol).
///
/// Both text and GPS use the **same 6-byte fixed block** structure in
/// the D-STAR slow-data stream: one type byte followed by 5 payload
/// bytes, transmitted as two consecutive 3-byte halves. The high
/// nibble of the type byte identifies the kind:
///
/// - `0x4X` — text block (Kenwood 4-block protocol, handled by
///   [`SlowDataTextCollector`] which uses the low nibble as a block
///   index 0..=3 and composes a 20-char message across 4 blocks).
/// - `0x3X` — GPS block: 5 payload bytes are appended to a running
///   sentence buffer which is scanned for DPRS (`$$CRC...\r`) and
///   NMEA (`$GPRMC...\n`, `$GPGGA...\n`) sentences.
///
/// Reference: `ircDDBGateway/Common/APRSCollector.cpp` (`SS_FIRST` /
/// `SS_SECOND`, `SLOW_DATA_TYPE_MASK`, `addGPSData`).
///
/// Reset on each `VoiceStart`; flushed into the matching `VoiceEnd`.
struct StreamSlowDataState {
    stream_id: StreamId,

    /// Assembled 6-byte block (first half in `[0..3]`, second half in
    /// `[3..6]`). Populated across two `push` calls.
    block_buffer: [u8; 6],
    /// Which half the next frame completes.
    block_phase: BlockPhase,

    /// Kenwood 4-block text accumulator. We still feed every frame
    /// into it so it can commit complete 20-char messages; it
    /// self-filters non-text type bytes internally.
    text_collector: SlowDataTextCollector,

    /// Growing buffer of GPS-block payloads. Sentences are delimited
    /// by `\r` (`$$CRC...` DPRS) or `\n` (`$GPRMC` / `$GPGGA` NMEA).
    gps_buffer: String,

    latest_text: Option<String>,
    latest_position: Option<GpsPosition>,
}

/// Which half of the 6-byte block the next frame completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockPhase {
    First,
    Second,
}

/// Cap on the running GPS sentence buffer. A normal DPRS sentence is
/// ~60 bytes; NMEA sentences are similar. Keeps a malformed /
/// unterminated stream from growing without bound.
const GPS_BUFFER_CAP: usize = 1024;

/// High nibble of the slow-data type byte (mask).
const SLOW_DATA_TYPE_MASK: u8 = 0xF0;
/// High nibble marking a GPS block.
const SLOW_DATA_TYPE_GPS: u8 = 0x30;
/// High nibble marking a text block.
const SLOW_DATA_TYPE_TEXT: u8 = 0x40;

/// Result of pushing one voice frame's slow-data through the dispatcher.
#[derive(Default, Debug)]
struct SlowDataDelta {
    text_changed: bool,
    position_changed: bool,
}

impl SlowDataDelta {
    const fn any(&self) -> bool {
        self.text_changed || self.position_changed
    }
}

impl StreamSlowDataState {
    fn new(stream_id: StreamId) -> Self {
        Self {
            stream_id,
            block_buffer: [0u8; 6],
            block_phase: BlockPhase::First,
            text_collector: SlowDataTextCollector::new(),
            gps_buffer: String::new(),
            latest_text: None,
            latest_position: None,
        }
    }

    /// Ingest one voice-frame slow-data fragment. `seq` is the reflector
    /// frame index (0 every 21 frames → D-STAR superframe sync).
    fn push(&mut self, slow_data: [u8; 3], seq: u8) -> SlowDataDelta {
        // D-STAR superframe sync: slow_data is filler (0x55 0x55 0x55).
        // Any partial block is discarded protocol-wide.
        if seq == 0 {
            self.text_collector.push(slow_data, 0);
            self.block_phase = BlockPhase::First;
            return SlowDataDelta::default();
        }

        // Text collector tracks its own half-phase internally and
        // self-filters non-text type bytes, so feed it every frame.
        self.text_collector.push(slow_data, seq);

        // Assemble the 6-byte block locally so we can inspect its
        // type byte and route GPS payloads — even when the text
        // collector decides the block isn't for it.
        let plain = descramble(slow_data);
        match self.block_phase {
            BlockPhase::First => {
                if let Some(dst) = self.block_buffer.get_mut(0..3) {
                    dst.copy_from_slice(&plain);
                }
                self.block_phase = BlockPhase::Second;
                SlowDataDelta::default()
            }
            BlockPhase::Second => {
                if let Some(dst) = self.block_buffer.get_mut(3..6) {
                    dst.copy_from_slice(&plain);
                }
                self.block_phase = BlockPhase::First;
                self.commit_block()
            }
        }
    }

    /// A complete 6-byte block has assembled. Dispatch by type nibble.
    fn commit_block(&mut self) -> SlowDataDelta {
        let type_byte = self.block_buffer.first().copied().unwrap_or(0);
        let type_high = type_byte & SLOW_DATA_TYPE_MASK;

        let mut delta = SlowDataDelta::default();
        match type_high {
            SLOW_DATA_TYPE_TEXT => {
                // text_collector already saw both halves via its own
                // push path; just check whether a 20-char message is
                // now complete.
                delta.text_changed = self.commit_text();
            }
            SLOW_DATA_TYPE_GPS => {
                // 5 bytes of GPS sentence payload at positions 1..=5.
                // Copy out to a short String so we're not holding a
                // borrow on `self` while `ingest_gps_chunk` mutates it.
                let payload: [u8; 5] = self
                    .block_buffer
                    .get(1..6)
                    .and_then(|s| <[u8; 5]>::try_from(s).ok())
                    .unwrap_or([0u8; 5]);
                let chunk = String::from_utf8_lossy(&payload).into_owned();
                delta.position_changed = ingest_gps_chunk(self, &chunk);
            }
            _ => {
                // Header retx, fast data, squelch, unknown — ignore.
            }
        }
        delta
    }

    /// Commit the text-collector's current message (if complete) into
    /// `latest_text`. Returns `true` if the stored value changed.
    fn commit_text(&mut self) -> bool {
        let Some(msg) = self.text_collector.take_message() else {
            return false;
        };
        let Some(cleaned) = clean_slow_data_text(&msg) else {
            return false;
        };
        if Some(&cleaned) == self.latest_text.as_ref() {
            return false;
        }
        self.latest_text = Some(cleaned);
        true
    }
}

/// Pump events from an `AsyncSession<P>` into an observer until either
/// the shutdown signal fires or the session ends on its own. Also
/// services TX commands from the foreground `ReflectorSession` handle.
///
/// Maintains a per-stream `SlowDataTextCollector` so the 20-byte TX
/// text message (the comment Kenwood operators set on their radios)
/// can be included in the final `VoiceEnd` event.
async fn run_session_task<P>(
    mut session: AsyncSession<P>,
    observer: Arc<dyn ReflectorObserver>,
    mut shutdown: oneshot::Receiver<()>,
    mut tx_rx: tokio::sync::mpsc::Receiver<TxCommand>,
) -> Result<(), String>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    let mut slow_state: Option<StreamSlowDataState> = None;

    // TX-side text collector. Reset on every outbound Header; updated
    // per Voice frame; consumed on Eot so the reply can carry the
    // assembled TX message back to Swift for the local recently-heard.
    let mut tx_text_state = TxTextState::default();

    let result = loop {
        tokio::select! {
            // Graceful shutdown: disconnect the session cleanly and exit.
            _ = &mut shutdown => {
                let disconnect_result = session
                    .disconnect()
                    .await
                    .map_err(|e| format!("{e}"));
                break disconnect_result;
            }
            // TX path: forward radio → reflector frames.
            Some(cmd) = tx_rx.recv() => {
                match cmd {
                    TxCommand::Header { header, stream_id, reply } => {
                        tx_text_state = TxTextState::default();
                        let r = session
                            .send_header(*header, stream_id)
                            .await
                            .map_err(|e| format!("{e}"));
                        drop(reply.send(r));
                    }
                    TxCommand::Voice { stream_id, seq, frame, reply } => {
                        tx_text_state.ingest(frame.slow_data);
                        let r = session
                            .send_voice(stream_id, seq, frame)
                            .await
                            .map_err(|e| format!("{e}"));
                        drop(reply.send(r));
                    }
                    TxCommand::Eot { stream_id, seq, reply } => {
                        let text = tx_text_state.take_text();
                        tx_text_state = TxTextState::default();
                        let r = session
                            .send_eot(stream_id, seq)
                            .await
                            .map(|()| text)
                            .map_err(|e| format!("{e}"));
                        drop(reply.send(r));
                    }
                }
            }
            // Normal path: pump the next event and deliver it.
            maybe_event = session.next_event() => {
                match maybe_event {
                    Some(event) => {
                        handle_event(&event, &observer, &mut slow_state);
                    }
                    None => {
                        break Ok(());
                    }
                }
            }
        }
    };
    observer.on_event(ReflectorEvent::Ended);
    result
}

/// Dispatch a single event from the underlying session. Updates the
/// per-stream slow-data state (text + GPS) and synthesises:
///   * `SlowDataUpdate` whenever a new text/position is assembled mid-stream,
///   * enriched `VoiceEnd` carrying the final text + position when the stream ends.
fn handle_event<P>(
    event: &Event<P>,
    observer: &Arc<dyn ReflectorObserver>,
    slow_state: &mut Option<StreamSlowDataState>,
) where
    P: dstar_gateway_core::session::client::Protocol,
{
    match event {
        Event::VoiceStart { stream_id, .. } => {
            *slow_state = Some(StreamSlowDataState::new(*stream_id));
            observer.on_event(translate_event(event));
        }
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
            ..
        } => {
            // Emit the raw voice event first — the relay layer consumes
            // it for radio↔reflector pass-through without waiting on the
            // slow-data decoders.
            observer.on_event(translate_event(event));

            if let Some(state) = slow_state.as_mut()
                && state.stream_id == *stream_id
            {
                let delta = state.push(frame.slow_data, *seq);
                if delta.any() {
                    observer.on_event(ReflectorEvent::SlowDataUpdate {
                        stream_id: stream_id.get(),
                        text: state.latest_text.clone(),
                        position: state.latest_position.clone(),
                    });
                }
            }
        }
        Event::VoiceEnd { stream_id, reason } => {
            let (text, position) = slow_state
                .take()
                .filter(|s| s.stream_id == *stream_id)
                .map_or((None, None), |s| (s.latest_text, s.latest_position));
            observer.on_event(ReflectorEvent::VoiceEnd {
                stream_id: stream_id.get(),
                reason: render_voice_end_reason(*reason),
                text,
                position,
            });
        }
        _ => {
            observer.on_event(translate_event(event));
        }
    }
}

/// Append a 5-byte GPS block payload to the running buffer, then
/// scan for complete sentences and parse each. Returns `true` if the
/// latest position changed.
///
/// Three sentence formats are recognised (per ircDDBGateway APRSCollector):
/// - `$$CRC` (DPRS), terminated by `\r` (0x0D).
/// - `$GPRMC` (NMEA), terminated by `\n` (0x0A).
/// - `$GPGGA` (NMEA), terminated by `\n` (0x0A).
fn ingest_gps_chunk(state: &mut StreamSlowDataState, chunk: &str) -> bool {
    tracing::debug!(
        target: "lodestar_core::session::gps",
        chunk = %chunk,
        chunk_bytes = ?chunk.as_bytes(),
        "slow-data GPS block"
    );
    state.gps_buffer.push_str(chunk);

    let mut changed = false;
    while let Some(end) = state.gps_buffer.find(['\r', '\n']) {
        let sentence: String = state.gps_buffer.drain(..=end).collect();
        let trimmed = sentence.trim_matches(['\r', '\n', '\0']).trim();
        if trimmed.is_empty() {
            continue;
        }
        tracing::debug!(
            target: "lodestar_core::session::gps",
            sentence = %trimmed,
            "parsing GPS sentence"
        );
        if let Some(position) = parse_gps_sentence(trimmed)
            && state.latest_position.as_ref() != Some(&position)
        {
            state.latest_position = Some(position);
            changed = true;
        }
    }

    // Prevent pathological growth on malformed / unterminated sentences.
    if state.gps_buffer.len() > GPS_BUFFER_CAP {
        let keep_from = state.gps_buffer.len().saturating_sub(GPS_BUFFER_CAP / 2);
        drop(state.gps_buffer.drain(..keep_from));
    }
    changed
}

/// Parse a complete GPS sentence into a [`GpsPosition`]. Dispatches
/// by prefix: `$$CRC` → DPRS, `$GPRMC` / `$GPGGA` → NMEA.
fn parse_gps_sentence(sentence: &str) -> Option<GpsPosition> {
    if sentence.starts_with("$$CRC") {
        match parse_dprs(sentence) {
            Ok(report) => Some(GpsPosition {
                callsign: report.callsign.as_str().trim().to_owned(),
                latitude: report.latitude.degrees(),
                longitude: report.longitude.degrees(),
                symbol: report.symbol.to_string(),
                comment: report.comment.clone(),
            }),
            Err(e) => {
                tracing::debug!(
                    target: "lodestar_core::session::gps",
                    error = %e,
                    sentence,
                    "DPRS parse failed"
                );
                None
            }
        }
    } else if sentence.starts_with("$GPRMC") {
        parse_nmea_rmc(sentence)
    } else if sentence.starts_with("$GPGGA") {
        parse_nmea_gga(sentence)
    } else {
        None
    }
}

/// Decode a comma-delimited NMEA coordinate field (`DDMM.MMMM`) plus
/// a hemisphere char (`N`/`S` or `E`/`W`) into decimal degrees.
fn nmea_coord_to_degrees(raw: &str, hemisphere: &str, deg_width: usize) -> Option<f64> {
    if raw.len() < deg_width + 2 {
        return None;
    }
    let deg: f64 = raw.get(..deg_width)?.parse().ok()?;
    let minutes: f64 = raw.get(deg_width..)?.parse().ok()?;
    let mut decimal = deg + minutes / 60.0;
    match hemisphere {
        "S" | "W" => decimal = -decimal,
        "N" | "E" => {}
        _ => return None,
    }
    Some(decimal)
}

/// Parse a `$GPRMC` sentence. Only lat/lon are extracted; the rest of
/// the NMEA fields (speed, heading, date) are ignored — the UI only
/// shows position.
fn parse_nmea_rmc(sentence: &str) -> Option<GpsPosition> {
    // $GPRMC,hhmmss.ss,A,DDMM.MMMM,N,DDDMM.MMMM,W,speed,course,ddmmyy,...
    //   0    1         2 3         4 5          6
    let fields: Vec<&str> = sentence.split(',').collect();
    let lat_raw = fields.get(3)?;
    let lat_hem = fields.get(4)?;
    let lon_raw = fields.get(5)?;
    let lon_hem = fields.get(6)?;
    let latitude = nmea_coord_to_degrees(lat_raw, lat_hem, 2)?;
    let longitude = nmea_coord_to_degrees(lon_raw, lon_hem, 3)?;
    Some(GpsPosition {
        callsign: String::new(),
        latitude,
        longitude,
        symbol: String::new(),
        comment: None,
    })
}

/// Parse a `$GPGGA` sentence. Same coordinate format as RMC, different
/// field layout.
fn parse_nmea_gga(sentence: &str) -> Option<GpsPosition> {
    // $GPGGA,hhmmss.ss,DDMM.MMMM,N,DDDMM.MMMM,W,fix,sats,...
    //   0    1         2         3 4          5
    let fields: Vec<&str> = sentence.split(',').collect();
    let lat_raw = fields.get(2)?;
    let lat_hem = fields.get(3)?;
    let lon_raw = fields.get(4)?;
    let lon_hem = fields.get(5)?;
    let latitude = nmea_coord_to_degrees(lat_raw, lat_hem, 2)?;
    let longitude = nmea_coord_to_degrees(lon_raw, lon_hem, 3)?;
    Some(GpsPosition {
        callsign: String::new(),
        latitude,
        longitude,
        symbol: String::new(),
        comment: None,
    })
}

/// Strip ASCII control bytes and trailing whitespace from a 20-byte
/// slow-data text message. Returns `None` if the result would be empty.
fn clean_slow_data_text(bytes: &[u8; 20]) -> Option<String> {
    let raw = String::from_utf8_lossy(bytes);
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Translate a `dstar-gateway-core` [`Event`] into the FFI-visible
/// [`ReflectorEvent`].
fn translate_event<P>(event: &Event<P>) -> ReflectorEvent
where
    P: dstar_gateway_core::session::client::Protocol,
{
    match event {
        Event::Connected { .. } => ReflectorEvent::Connected,
        Event::Disconnected { reason } => ReflectorEvent::Disconnected {
            reason: render_disconnect_reason(*reason),
        },
        Event::PollEcho { .. } => ReflectorEvent::PollEcho,
        Event::VoiceStart {
            stream_id, header, ..
        } => ReflectorEvent::VoiceStart {
            stream_id: stream_id.get(),
            mycall: header.my_call.as_str().trim().to_owned(),
            suffix: header.my_suffix.as_str().trim().to_owned(),
            urcall: header.ur_call.as_str().trim().to_owned(),
            rpt1: header.rpt1.as_str().trim().to_owned(),
            rpt2: header.rpt2.as_str().trim().to_owned(),
            header_bytes: header.encode().to_vec(),
        },
        Event::VoiceFrame {
            stream_id,
            seq,
            frame,
            ..
        } => {
            let mut voice = Vec::with_capacity(12);
            voice.extend_from_slice(&frame.ambe);
            voice.extend_from_slice(&frame.slow_data);
            ReflectorEvent::VoiceFrame {
                stream_id: stream_id.get(),
                seq: *seq,
                voice_bytes: voice,
            }
        }
        Event::VoiceEnd { stream_id, reason } => ReflectorEvent::VoiceEnd {
            stream_id: stream_id.get(),
            reason: render_voice_end_reason(*reason),
            // `handle_event` always produces the authoritative VoiceEnd
            // with the assembled text + GPS — this fallback only fires
            // for callers that bypass the state machine.
            text: None,
            position: None,
        },
        // `Event` is `#[non_exhaustive]` and carries a private
        // `__Phantom` variant to thread `P`. Map anything future to
        // `Ended` so we don't panic but still signal "something we
        // don't recognise arrived".
        _ => ReflectorEvent::Ended,
    }
}

fn render_disconnect_reason(reason: DisconnectReason) -> String {
    match reason {
        DisconnectReason::Rejected => "rejected".to_owned(),
        DisconnectReason::UnlinkAcked => "unlink-acked".to_owned(),
        DisconnectReason::KeepaliveInactivity => "keepalive-timeout".to_owned(),
        DisconnectReason::DisconnectTimeout => "disconnect-timeout".to_owned(),
        _ => "unknown".to_owned(),
    }
}

fn render_voice_end_reason(reason: VoiceEndReason) -> String {
    match reason {
        VoiceEndReason::Eot => "eot".to_owned(),
        VoiceEndReason::Inactivity => "inactivity".to_owned(),
        _ => "unknown".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Connect helpers (one per protocol)
// ---------------------------------------------------------------------------

/// Validate a single-character module letter supplied as a Swift `String`.
fn parse_module(s: &str) -> Result<Module, ReflectorError> {
    let ch = s
        .chars()
        .next()
        .ok_or_else(|| ReflectorError::InvalidModule("empty".to_owned()))?;
    if s.chars().count() != 1 {
        return Err(ReflectorError::InvalidModule(format!(
            "{s:?} must be a single character"
        )));
    }
    Module::try_from_char(ch).map_err(|e| ReflectorError::InvalidModule(format!("{ch}: {e}")))
}

/// Resolve a reflector's host+port to a concrete [`std::net::SocketAddr`].
async fn resolve_peer(reflector: &Reflector) -> Result<std::net::SocketAddr, ReflectorError> {
    let authority = format!("{}:{}", reflector.host, reflector.port);
    let mut addrs = tokio::net::lookup_host(&authority)
        .await
        .map_err(|e| ReflectorError::DnsFailed(format!("{authority}: {e}")))?;
    addrs
        .next()
        .ok_or_else(|| ReflectorError::DnsFailed(format!("{authority}: no addresses")))
}

/// Bind an ephemeral local UDP socket for a new reflector session.
async fn bind_local_socket() -> Result<Arc<UdpSocket>, ReflectorError> {
    UdpSocket::bind("0.0.0.0:0")
        .await
        .map(Arc::new)
        .map_err(|e| ReflectorError::SocketBindFailed(format!("{e}")))
}

/// Drive a `Session<P, Connecting>` to `Connected` by polling the sans-io
/// core and pumping the UDP socket until the reflector ACKs the LINK.
async fn drive_handshake_to_connected<P>(
    mut session: Session<P, Connecting>,
    socket: &UdpSocket,
) -> Result<Session<P, Connected>, ReflectorError>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    let deadline = std::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let mut buf = [0u8; 2048];

    loop {
        match session.state_kind() {
            ClientStateKind::Connected => break,
            ClientStateKind::Closed => {
                return Err(ReflectorError::ConnectFailed(
                    "reflector rejected the connection".to_owned(),
                ));
            }
            _ => {}
        }

        if std::time::Instant::now() >= deadline {
            return Err(ReflectorError::HandshakeTimeout);
        }

        while let Some(tx) = session.poll_transmit(std::time::Instant::now()) {
            let _bytes_sent = socket.send_to(tx.payload, tx.dst).await.map_err(|e| {
                ReflectorError::ConnectFailed(format!("handshake send failed: {e}"))
            })?;
        }

        match tokio::time::timeout(
            std::time::Duration::from_millis(100),
            socket.recv_from(&mut buf),
        )
        .await
        {
            Ok(Ok((n, src))) => {
                if let Some(bytes) = buf.get(..n) {
                    session
                        .handle_input(std::time::Instant::now(), src, bytes)
                        .map_err(|e| {
                            ReflectorError::ConnectFailed(format!("handshake decode failed: {e}"))
                        })?;
                }
            }
            Ok(Err(e)) => {
                return Err(ReflectorError::ConnectFailed(format!(
                    "handshake recv failed: {e}"
                )));
            }
            Err(_) => {
                session.handle_timeout(std::time::Instant::now());
            }
        }
    }

    session.promote().map_err(|f| {
        ReflectorError::ConnectFailed(format!("promote to Connected failed: {}", f.error))
    })
}

/// Build, connect, and spawn a `DExtra` session.
async fn connect_dextra(
    station: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<DExtra>, ReflectorError> {
    let configured = Session::<DExtra, _>::builder()
        .callsign(station)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let connecting = configured
        .connect(std::time::Instant::now())
        .map_err(|f| ReflectorError::BuildFailed(format!("enqueue LINK failed: {}", f.error)))?;

    let connected = drive_handshake_to_connected(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

/// Build, connect, and spawn a `Dcs` session.
async fn connect_dcs(
    station: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<Dcs>, ReflectorError> {
    let configured = Session::<Dcs, _>::builder()
        .callsign(station)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let connecting = configured
        .connect(std::time::Instant::now())
        .map_err(|f| ReflectorError::BuildFailed(format!("enqueue CONNECT failed: {}", f.error)))?;

    let connected = drive_handshake_to_connected(connecting, &socket).await?;
    Ok(AsyncSession::spawn(connected, socket))
}

/// Build, authenticate, and drive a full `DPlus` (REF) connect handshake.
///
/// If auth fails we still attempt the UDP handshake — matches the
/// repl's best-effort behaviour — but if the handshake ALSO fails,
/// we prefix the error with the auth failure so users hunting the
/// real cause don't have to guess.
async fn connect_dplus(
    station: Callsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: Callsign,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<DPlus>, ReflectorError> {
    let (hosts, auth_warning) = match AuthClient::new().authenticate(station).await {
        Ok(h) => (h, None),
        Err(e) => {
            let msg = format!("{e}");
            debug!(
                target: "lodestar_core::session",
                error = %msg,
                "DPlus auth failed, attempting connect with empty host list (previous auth may still be valid)"
            );
            (HostList::new(), Some(msg))
        }
    };

    let configured = Session::<DPlus, _>::builder()
        .callsign(station)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .reflector_callsign(reflector_callsign)
        .peer(peer)
        .build();

    let authenticated = configured.authenticate(hosts).map_err(|f| {
        ReflectorError::BuildFailed(format!("attach host list failed: {}", f.error))
    })?;

    let connecting = authenticated
        .connect(std::time::Instant::now())
        .map_err(|f| ReflectorError::BuildFailed(format!("enqueue LINK1 failed: {}", f.error)))?;

    let handshake = drive_handshake_to_connected(connecting, &socket).await;
    match handshake {
        Ok(connected) => Ok(AsyncSession::spawn(connected, socket)),
        Err(e) => {
            if let Some(auth_warning) = auth_warning {
                // Auth failed AND LINK1 was rejected. The common cause is
                // an unregistered callsign — tell the user so they don't
                // have to guess.
                Err(ReflectorError::AuthFailed(format!(
                    "{e}. DPlus auth also failed: {auth_warning}. \
                     Register your callsign at dstargateway.org, or try an XRF/DCS reflector \
                     (those don't require auth)."
                )))
            } else {
                Err(e)
            }
        }
    }
}
