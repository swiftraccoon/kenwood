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
//! connect. A failed authentication is retained while the UDP handshake is
//! attempted with an empty host list; a combined failure reports both details.

use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;
use std::sync::Arc;

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::codec::dplus::HostList;
use dstar_gateway_core::dprs::{DprsReport, Latitude, Longitude};
use dstar_gateway_core::header::{DstarHeader, ENCODED_LEN as HEADER_ENCODED_LEN};
use dstar_gateway_core::hosts::HostAddress;
use dstar_gateway_core::session::client::{
    Connected, Connecting, DExtra, DPlus, Dcs, DisconnectReason, Event, Session, VoiceEndReason,
};
use dstar_gateway_core::slowdata::{SlowDataTextCollector, descramble};
use dstar_gateway_core::voice::VoiceFrame;
use dstar_gateway_core::{
    Callsign, DcsReflector, FrameSequence, Module, RelayRouting, RoutingCallsign, StreamId,
};
use thiserror::Error;
use tokio::net::UdpSocket;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::debug;

use crate::reflector::{Reflector, ReflectorProtocol};
use crate::wire::{VoiceSequence, VoiceStreamId};

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

/// Handshake timeout: how long the session waits for the reflector to
/// ACK the LINK before giving up.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const SETUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(35);

/// Editable foreign connection inputs; construction of the executable
/// [`ReflectorConnectionConfig`] validates the complete candidate without I/O.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReflectorConnectionCandidate {
    /// Local station prefix, occupying at most seven routing bytes.
    pub callsign: String,
    /// Directory or manually entered endpoint candidate.
    pub reflector: Reflector,
    /// Local A-Z module letter.
    pub local_module: String,
    /// Remote A-Z module letter.
    pub reflector_module: String,
}

/// Immutable checked identity, endpoint and protocol used by a connection.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Object)]
pub struct ReflectorConnectionConfig {
    station: RoutingCallsign,
    reflector: RoutingCallsign,
    address: HostAddress,
    port: NonZeroU16,
    local: Module,
    remote: Module,
    protocol: ReflectorProtocol,
    dcs: Option<DcsReflector>,
}

#[uniffi::export]
impl ReflectorConnectionConfig {
    /// Validate all fields before DNS, authentication or socket creation.
    ///
    /// # Errors
    ///
    /// Returns [`ReflectorError::InvalidCallsign`],
    /// [`ReflectorError::InvalidModule`] or [`ReflectorError::InvalidEndpoint`]
    /// for malformed routing identities, module letters, address or zero port.
    #[uniffi::constructor]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "UniFFI constructors receive owned foreign records"
    )]
    pub fn new(candidate: ReflectorConnectionCandidate) -> Result<Arc<Self>, ReflectorError> {
        let station = RoutingCallsign::new(&candidate.callsign)
            .map_err(|error| ReflectorError::InvalidCallsign(error.to_string()))?;
        let reflector = RoutingCallsign::new(&candidate.reflector.name)
            .map_err(|error| ReflectorError::InvalidCallsign(error.to_string()))?;
        let address = HostAddress::new(&candidate.reflector.host)
            .map_err(|error| ReflectorError::InvalidEndpoint(error.to_string()))?;
        let port = NonZeroU16::new(candidate.reflector.port).ok_or_else(|| {
            ReflectorError::InvalidEndpoint("UDP port must be nonzero".to_owned())
        })?;
        let local = parse_module(&candidate.local_module)?;
        let remote = parse_module(&candidate.reflector_module)?;
        let dcs = if candidate.reflector.protocol == ReflectorProtocol::Dcs {
            Some(
                DcsReflector::try_from(reflector)
                    .map_err(|error| ReflectorError::InvalidCallsign(error.to_string()))?,
            )
        } else {
            None
        };
        Ok(Arc::new(Self {
            station,
            reflector,
            address,
            port,
            local,
            remote,
            protocol: candidate.reflector.protocol,
            dcs,
        }))
    }
}

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
    station_callsign: RoutingCallsign,
    local_module_typed: Module,
    reflector_callsign: RoutingCallsign,
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
    async fn sender(&self) -> Result<tokio::sync::mpsc::Sender<TxCommand>, ReflectorError> {
        self.backend
            .lock()
            .await
            .as_ref()
            .filter(|backend| backend.shutdown.is_some() && !backend.join.is_finished())
            .map(|backend| backend.tx.clone())
            .ok_or(ReflectorError::AlreadyDisconnected)
    }

    /// Rewrite a radio-emitted D-STAR header for reflector ingestion.
    ///
    /// - `rpt1[0..7]` = operator callsign (7 bytes, space-padded).
    /// - `rpt1[7]`    = local module letter.
    /// - `rpt2[0..7]` = reflector callsign (e.g. `"REF030 "`).
    /// - `rpt2[7]`    = reflector module letter.
    /// - `ur_call`    = `"CQCQCQ"` (hotspot-to-reflector convention).
    ///
    /// `my_call` / `my_suffix` / flags pass through unchanged; they
    /// identify the operator and their tail (`/D75`, `/M`, etc.).
    fn build_reflector_header(&self, radio: &DstarHeader) -> DstarHeader {
        // `for_relay` zeroes the flags (correct for a TX-from-scratch
        // client); chain the radio's source flags back on for this relay.
        DstarHeader::for_relay(
            RelayRouting {
                operator: self.station_callsign,
                local_module: self.local_module_typed,
                reflector: self.reflector_callsign,
                reflector_module: self.reflector_module_typed,
            },
            radio.my_call,
            radio.my_suffix,
        )
        .with_flags(radio.flags)
    }
}

/// Background task handle for a live reflector session.
struct Backend {
    shutdown: Option<oneshot::Sender<()>>,
    join: JoinHandle<Result<(), String>>,
    tx: tokio::sync::mpsc::Sender<TxCommand>,
}

/// TX commands the foreground session sends to its background task.
#[derive(Debug)]
enum TxCommand {
    Header {
        header: Box<DstarHeader>,
        stream_id: StreamId,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Voice {
        stream_id: StreamId,
        seq: FrameSequence,
        frame: VoiceFrame,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Eot {
        stream_id: StreamId,
        seq: FrameSequence,
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
/// a local TX is relayed to the reflector. Reflectors typically do not
/// echo the sender's packets back, so a recently-heard entry is
/// synthesised for the relayed stream instead.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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
///
/// Every 41-byte value yields field observations; callsign formatting retains
/// malformed bytes as diagnostic escapes. CRC checking is left to the caller.
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
    let h = DstarHeader::decode(&arr);
    Some(DecodedRadioHeader {
        mycall: h.my_call.to_string(),
        suffix: h.my_suffix.to_string(),
        urcall: h.ur_call.to_string(),
        rpt1: h.rpt1.to_string(),
        rpt2: h.rpt2.to_string(),
    })
}

/// Parsed DPRS position report carried by slow-data GPS blocks.
///
/// All coordinates are decimal degrees; negative values are south/west.
/// `callsign`, `symbol`, and `comment` are populated when the DPRS
/// sentence decodes cleanly; `symbol` is a single APRS symbol char
/// rendered as a one-character string for FFI simplicity.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct GpsPosition {
    /// Reporting station callsign, trimmed.
    callsign: String,
    /// Latitude in decimal degrees (north positive).
    latitude: f64,
    /// Longitude in decimal degrees (east positive).
    longitude: f64,
    /// APRS symbol character (e.g. `"["` for a jogger).
    symbol: String,
    /// Free-form comment appended to the DPRS sentence, if any.
    comment: Option<String>,
}

impl GpsPosition {
    /// Reporting station text, empty for NMEA without an APRS source.
    #[must_use]
    pub fn callsign(&self) -> &str {
        &self.callsign
    }
    /// Latitude in decimal degrees within `-90..=90`, north positive.
    #[must_use]
    pub const fn latitude(&self) -> f64 {
        self.latitude
    }
    /// Longitude in decimal degrees within `-180..=180`, east positive.
    #[must_use]
    pub const fn longitude(&self) -> f64 {
        self.longitude
    }
    /// APRS symbol code, empty for NMEA-only positions.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    /// Optional position comment.
    #[must_use]
    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }
}

/// Why the reflector link ended, typed so Swift can pattern-match
/// instead of string-matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DisconnectCause {
    /// Reflector rejected the LINK request.
    Rejected,
    /// Reflector acknowledged the unlink (expected after `disconnect`).
    UnlinkAcked,
    /// No keepalive traffic inside the timeout window.
    KeepaliveTimeout,
    /// Reflector did not acknowledge the disconnect in time.
    DisconnectTimeout,
    /// A cause added upstream that this build doesn't know yet.
    Unknown,
}

/// Why a voice stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum VoiceEndCause {
    /// Real end-of-transmission frame arrived.
    Eot,
    /// The stream went silent past the inactivity window.
    Inactivity,
    /// A cause added upstream that this build doesn't know yet.
    Unknown,
}

/// Translated reflector event surfaced to the Swift observer.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum ReflectorEvent {
    /// The reflector acknowledged the LINK/CONNECT and the session is live.
    Connected,
    /// Connection ended. `reason` is a typed [`DisconnectCause`]
    /// (rejected, unlink-acked, keepalive-timeout, disconnect-timeout,
    /// or unknown) so Swift can pattern-match instead of string-matching.
    Disconnected {
        /// Typed disconnect reason.
        reason: DisconnectCause,
    },
    /// `DPlus` keepalive bounce, useful as a "still alive" signal.
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
        /// MMDVM D-STAR header frame (command 0x10) and send to the radio.
        header_bytes: Vec<u8>,
    },
    /// A voice frame arrived mid-stream.
    VoiceFrame {
        /// Stream identifier this frame belongs to.
        stream_id: u16,
        /// Sequence number within the stream.
        seq: u8,
        /// Raw 12-byte voice frame (9 bytes AMBE + 3 bytes slow-data).
        /// Ready to wrap as an MMDVM D-STAR data frame (command 0x11)
        /// and send to the radio.
        voice_bytes: Vec<u8>,
    },
    /// Slow-data metadata for the current stream updated. Fires any
    /// time the assembled text or parsed GPS position changes; the
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
        /// Typed reason the stream ended (EOT, inactivity, or unknown).
        reason: VoiceEndCause,
        /// Slow-data text assembled over the stream, if a complete
        /// 20-byte message arrived. This is the "TX message" operators
        /// set on their radios (Kenwood's stored comment field) and
        /// the commonest reason to show "recently heard" entries with
        /// context beyond the callsign.
        text: Option<String>,
        /// Final parsed DPRS position seen on the stream, if any.
        position: Option<GpsPosition>,
    },
    /// The background task ended: session is finished, no further events.
    Ended,
    /// An event variant this build doesn't recognise, surfaced for
    /// diagnostics instead of being conflated with session teardown.
    Unknown {
        /// Debug rendering of the unrecognised event.
        detail: String,
    },
}

/// Foreign-implemented trait that receives translated reflector events.
///
/// Swift implements this protocol and passes an instance when calling
/// [`ReflectorSession::connect`]. The background task holds the observer via
/// `Arc<dyn ReflectorObserver>` and releases it when the session ends.
#[uniffi::export(with_foreign)]
pub trait ReflectorObserver: Send + Sync + std::fmt::Debug {
    /// Called once per reflector event. Must not block for long: the
    /// session task awaits completion before pumping the next event.
    fn on_event(&self, event: ReflectorEvent);
}

/// Error variants surfaced across the FFI boundary.
#[derive(Debug, Clone, PartialEq, Eq, Error, uniffi::Error)]
#[non_exhaustive]
pub enum ReflectorError {
    /// Caller-supplied callsign is not a valid D-STAR callsign.
    #[error("invalid callsign: {0}")]
    InvalidCallsign(String),
    /// Module letter was missing or not A-Z.
    #[error("invalid module letter: {0}")]
    InvalidModule(String),
    /// Host syntax or UDP port was invalid before any I/O.
    #[error("invalid reflector endpoint: {0}")]
    InvalidEndpoint(String),
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
    /// The complete DNS, authentication and UDP connection budget expired.
    #[error("reflector connection exceeded the 35-second setup deadline")]
    ConnectionTimeout,
    /// Session is already disconnected.
    #[error("already disconnected")]
    AlreadyDisconnected,
    /// Underlying `dstar-gateway` shell returned an error.
    #[error("session shell error: {0}")]
    Shell(String),
    /// Background task died without a clean shutdown signal.
    #[error("session task panicked: {0}")]
    TaskPanic(String),
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
}

#[uniffi::export(async_runtime = "tokio")]
impl ReflectorSession {
    /// The operator callsign this session identifies as.
    #[must_use]
    pub fn callsign(self: Arc<Self>) -> String {
        self.callsign.clone()
    }

    /// Name of the connected reflector.
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
        self.backend
            .lock()
            .await
            .as_ref()
            .is_some_and(|backend| backend.shutdown.is_some() && !backend.join.is_finished())
    }

    /// Send a D-STAR header opening a new outbound voice stream.
    ///
    /// `header_bytes` must be exactly 41 bytes (the on-wire D-STAR
    /// header format used by MMDVM). `stream_id` must be non-zero.
    ///
    /// # Errors
    ///
    /// - [`ReflectorError::BadHeaderLength`] for an invalid payload.
    ///   Stream identifiers use checked foreign lifting before queueing.
    /// - [`ReflectorError::AlreadyDisconnected`] if the session has been torn down.
    /// - [`ReflectorError::Shell`] for failures inside `dstar-gateway`.
    ///
    /// # Cancellation and completion
    ///
    /// The 64-request queue accepts work when `send` finishes. Cancelling
    /// before acceptance queues nothing; abandoning the response afterwards
    /// permits the datagram to finish. Success observes UDP send completion,
    /// not reflector receipt. Never retry an uncertain send automatically.
    /// [`Self::disconnect`] rejects new requests and waits for task completion.
    pub async fn send_header(
        self: Arc<Self>,
        header_bytes: Vec<u8>,
        stream_id: VoiceStreamId,
    ) -> Result<(), ReflectorError> {
        if header_bytes.len() != HEADER_ENCODED_LEN {
            return Err(ReflectorError::BadHeaderLength {
                expected: u32::try_from(HEADER_ENCODED_LEN).unwrap_or(u32::MAX),
                got: u32::try_from(header_bytes.len()).unwrap_or(u32::MAX),
            });
        }
        let sid = stream_id.core();
        let mut arr = [0u8; HEADER_ENCODED_LEN];
        arr.copy_from_slice(&header_bytes);
        let radio_header = DstarHeader::decode(&arr);

        // Build repeater routes from the session's validated callsigns and
        // modules. The label `DIRECT  ` contains T at byte 5 and spaces at
        // bytes 6 and 7; build_reflector_header sets the module at byte 7.
        // The operator identity, suffix and source flags remain unchanged.
        let ref_header = self.build_reflector_header(&radio_header);

        let tx = self.sender().await?;
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
    /// Returns [`ReflectorError::BadVoiceLength`] unless exactly 12 bytes are
    /// supplied. Connection, queue, cancellation and completion behavior
    /// follows [`Self::send_header`]; `seq` is a checked 0-through-20 index.
    pub async fn send_voice(
        self: Arc<Self>,
        stream_id: VoiceStreamId,
        seq: VoiceSequence,
        voice_bytes: Vec<u8>,
    ) -> Result<(), ReflectorError> {
        if voice_bytes.len() != 12 {
            return Err(ReflectorError::BadVoiceLength {
                got: u32::try_from(voice_bytes.len()).unwrap_or(u32::MAX),
            });
        }
        let sid = stream_id.core();
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
        let tx = self.sender().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(TxCommand::Voice {
            stream_id: sid,
            seq: seq.core(),
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
    /// Connection, queue, cancellation and completion behavior follows
    /// [`Self::send_header`]. The checked sequence excludes EOT flag bits;
    /// the protocol encoder adds termination separately.
    pub async fn send_eot(
        self: Arc<Self>,
        stream_id: VoiceStreamId,
        seq: VoiceSequence,
    ) -> Result<Option<String>, ReflectorError> {
        let sid = stream_id.core();
        let tx = self.sender().await?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(TxCommand::Eot {
            stream_id: sid,
            seq: seq.core(),
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
    /// New sends are refused once shutdown starts. Cancelling this future
    /// retains the task handle and its eventual result; another call can
    /// resume waiting. After a completed call, another call returns
    /// [`ReflectorError::AlreadyDisconnected`]. Dropping the session only
    /// signals shutdown; it neither waits nor observes cleanup failures.
    ///
    /// # Errors
    ///
    /// - [`ReflectorError::AlreadyDisconnected`] if the session has
    ///   already been torn down.
    /// - [`ReflectorError::Shell`] if the background task's disconnect
    ///   attempt returned an error from `dstar-gateway`.
    /// - [`ReflectorError::TaskPanic`] if the task panicked.
    pub async fn disconnect(self: Arc<Self>) -> Result<(), ReflectorError> {
        let mut guard = self.backend.lock().await;
        let backend = guard.as_mut().ok_or(ReflectorError::AlreadyDisconnected)?;
        if let Some(shutdown) = backend.shutdown.take() {
            let _sent = shutdown.send(());
        }
        let result = match (&mut backend.join).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(msg)) => Err(ReflectorError::Shell(msg)),
            Err(join_err) => Err(ReflectorError::TaskPanic(format!("{join_err}"))),
        };
        drop(guard.take());
        result
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl ReflectorSession {
    /// Connect a checked configuration, then start its single observer task.
    ///
    /// The complete DNS/authentication/UDP setup budget is 35 seconds. DNS has
    /// five seconds; at most eight resolved endpoints are attempted with a
    /// matching IPv4 or IPv6 socket and a five-second UDP handshake each.
    /// Cancellation before return drops the in-progress socket and handshake.
    /// The observer runs synchronously on the task and must return promptly.
    ///
    /// # Errors
    ///
    /// Returns typed DNS, bind, authentication or handshake failures, or
    /// [`ReflectorError::ConnectionTimeout`] at the complete setup deadline.
    #[uniffi::constructor(name = "connect")]
    pub async fn connect(
        configuration: Arc<ReflectorConnectionConfig>,
        observer: Arc<dyn ReflectorObserver>,
    ) -> Result<Arc<Self>, ReflectorError> {
        with_setup_deadline(connect_checked(&configuration, observer)).await
    }
}

async fn with_setup_deadline<T>(
    operation: impl Future<Output = Result<T, ReflectorError>>,
) -> Result<T, ReflectorError> {
    tokio::time::timeout(SETUP_BUDGET, operation)
        .await
        .map_err(|_| ReflectorError::ConnectionTimeout)?
}

enum ConnectedReflector {
    DExtra(AsyncSession<DExtra>),
    Dcs(AsyncSession<Dcs>),
    DPlus(AsyncSession<DPlus>),
}

async fn connect_checked(
    config: &ReflectorConnectionConfig,
    observer: Arc<dyn ReflectorObserver>,
) -> Result<Arc<ReflectorSession>, ReflectorError> {
    let peers = resolve_peers(config).await?;
    let mut connected = None;
    let mut last_failure = None;
    for peer in peers {
        let socket = match bind_local_socket(peer).await {
            Ok(socket) => socket,
            Err(error) => {
                last_failure = Some(error);
                continue;
            }
        };
        let result = match config.protocol {
            ReflectorProtocol::DExtra => {
                connect_dextra(config.station, peer, config.local, config.remote, socket)
                    .await
                    .map(ConnectedReflector::DExtra)
            }
            ReflectorProtocol::Dcs => {
                let target = config.dcs.ok_or_else(|| {
                    ReflectorError::BuildFailed(
                        "DCS target missing from checked configuration".to_owned(),
                    )
                })?;
                connect_dcs(
                    config.station,
                    peer,
                    config.local,
                    config.remote,
                    target,
                    socket,
                )
                .await
                .map(ConnectedReflector::Dcs)
            }
            ReflectorProtocol::DPlus => {
                connect_dplus(config.station, peer, config.local, config.remote, socket)
                    .await
                    .map(ConnectedReflector::DPlus)
            }
        };
        match result {
            Ok(session) => {
                connected = Some(session);
                break;
            }
            Err(error) => last_failure = Some(error),
        }
    }
    let connected = connected.ok_or_else(|| {
        last_failure.unwrap_or_else(|| {
            ReflectorError::DnsFailed("resolver returned no usable endpoints".to_owned())
        })
    })?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let (tx_cmd, tx_rx) = tokio::sync::mpsc::channel::<TxCommand>(64);
    let join = match connected {
        ConnectedReflector::DExtra(session) => {
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
        ConnectedReflector::Dcs(session) => {
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
        ConnectedReflector::DPlus(session) => {
            tokio::spawn(run_session_task(session, observer, shutdown_rx, tx_rx))
        }
    };

    Ok(Arc::new(ReflectorSession {
        backend: Mutex::new(Some(Backend {
            shutdown: Some(shutdown_tx),
            join,
            tx: tx_cmd,
        })),
        callsign: config.station.to_string(),
        reflector_name: config.reflector.to_string(),
        local_module: config.local.to_string(),
        reflector_module: config.remote.to_string(),
        protocol: config.protocol,
        station_callsign: config.station,
        local_module_typed: config.local,
        reflector_callsign: config.reflector,
        reflector_module_typed: config.remote,
    }))
}

/// Fetch the authoritative `DPlus` REF directory from the auth server.
/// Requires a registered callsign; returns `AuthFailed` when the
/// server rejects it or the TCP exchange fails.
///
/// # Errors
///
/// - [`ReflectorError::InvalidCallsign`] if `callsign` is not a valid
///   D-STAR callsign.
/// - [`ReflectorError::AuthFailed`] if the auth-server handshake fails.
#[uniffi::export(async_runtime = "tokio")]
pub async fn fetch_dplus_directory(callsign: String) -> Result<Vec<Reflector>, ReflectorError> {
    let cs = Callsign::try_from_str(&callsign)
        .map_err(|e| ReflectorError::InvalidCallsign(format!("{callsign}: {e}")))?;
    let list = AuthClient::new()
        .authenticate(cs)
        .await
        .map_err(|e| ReflectorError::AuthFailed(format!("{e}")))?;
    Ok(list
        .hosts()
        .iter()
        .map(|h| Reflector {
            name: h.callsign.to_string(),
            host: h.address.to_string(),
            port: dstar_gateway_core::codec::dplus::consts::DEFAULT_PORT,
            protocol: ReflectorProtocol::DPlus,
            description: String::new(),
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Background task
// ---------------------------------------------------------------------------

/// TX-side slow-data text tracker. Assembles the outgoing radio's
/// Kenwood 4-block text (the 20-char "TX message") so it can be
/// reported back to Swift when the stream ends; used for local
/// recently-heard entries.
///
/// Outbound slow-data bytes retain MMDVM wire scrambling. The checked
/// transmit index resets half-block assembly at zero and pairs adjacent
/// odd/even indices within each 21-frame superframe.
#[derive(Default)]
struct TxTextState {
    collector: SlowDataTextCollector,
    latest: Option<String>,
}

impl TxTextState {
    fn ingest(&mut self, slow_data: [u8; 3], sequence: FrameSequence) {
        self.collector.push(slow_data, sequence.get());
        if let Some(msg) = self.collector.take_message()
            && let Some(cleaned) = clean_slow_data_text(msg.as_bytes())
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
/// - `0x4X`: text block (Kenwood 4-block protocol, handled by
///   [`SlowDataTextCollector`] which uses the low nibble as a block
///   index 0..=3 and composes a 20-char message across 4 blocks).
/// - `0x3X`: GPS block whose 5 payload bytes are appended to a running
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

    /// Kenwood 4-block text accumulator. Every frame is still fed
    /// into it so it can commit complete 20-char messages; it
    /// self-filters non-text type bytes internally.
    text_collector: SlowDataTextCollector,

    /// Growing buffer of GPS-block payloads. Sentences are delimited
    /// by `\r` (`$$CRC...` DPRS) or `\n` (`$GPRMC` / `$GPGGA` NMEA).
    gps_buffer: Vec<u8>,

    last_sequence: Option<u8>,

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
            gps_buffer: Vec::new(),
            last_sequence: None,
            latest_text: None,
            latest_position: None,
        }
    }

    /// Ingest one voice-frame slow-data fragment. `seq` is the reflector
    /// frame index (0 every 21 frames → D-STAR superframe sync).
    fn push(&mut self, slow_data: [u8; 3], seq: u8) -> SlowDataDelta {
        if seq >= 21 {
            return SlowDataDelta::default();
        }
        if self
            .last_sequence
            .is_some_and(|last| seq != (last + 1) % 21)
        {
            self.block_phase = BlockPhase::First;
        }
        self.last_sequence = Some(seq);
        tracing::trace!(
            target: "lodestar_core::session::slow_data",
            seq,
            bytes = ?slow_data,
            phase = ?self.block_phase,
            "slow-data frame"
        );

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
        // type byte and route GPS payloads, even when the text
        // collector decides the block isn't for it.
        let plain = descramble(slow_data);
        match (self.block_phase, seq.is_multiple_of(2)) {
            (_, false) => {
                if let Some(dst) = self.block_buffer.get_mut(0..3) {
                    dst.copy_from_slice(&plain);
                }
                self.block_phase = BlockPhase::Second;
                SlowDataDelta::default()
            }
            (BlockPhase::Second, true) => {
                if let Some(dst) = self.block_buffer.get_mut(3..6) {
                    dst.copy_from_slice(&plain);
                }
                self.block_phase = BlockPhase::First;
                self.commit_block()
            }
            (BlockPhase::First, true) => SlowDataDelta::default(),
        }
    }

    /// A complete 6-byte block has assembled. Dispatch by type nibble.
    fn commit_block(&mut self) -> SlowDataDelta {
        let type_byte = self.block_buffer.first().copied().unwrap_or(0);
        let type_high = type_byte & SLOW_DATA_TYPE_MASK;

        tracing::debug!(
            target: "lodestar_core::session::slow_data",
            type_byte = format_args!("0x{:02X}", type_byte),
            block = format_args!(
                "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                self.block_buffer[0], self.block_buffer[1], self.block_buffer[2],
                self.block_buffer[3], self.block_buffer[4], self.block_buffer[5]
            ),
            payload_ascii = ?String::from_utf8_lossy(&self.block_buffer[1..6]),
            "6-byte slow-data block assembled"
        );

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
                // Copy the bounded payload before updating sentence state.
                let payload: [u8; 5] = self
                    .block_buffer
                    .get(1..6)
                    .and_then(|s| <[u8; 5]>::try_from(s).ok())
                    .unwrap_or([0u8; 5]);
                let length = usize::from(type_byte & 0x0F);
                if let Some(chunk) = payload.get(..length) {
                    delta.position_changed = ingest_gps_chunk(self, chunk);
                }
            }
            _ => {
                // Header retx, fast data, squelch, unknown: ignore.
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
        let Some(cleaned) = clean_slow_data_text(msg.as_bytes()) else {
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
    P: dstar_gateway_core::session::client::Protocol + std::fmt::Debug,
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
                        tx_text_state.ingest(frame.slow_data, seq);
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
                        handle_event(&event, observer.as_ref(), &mut slow_state);
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
    observer: &dyn ReflectorObserver,
    slow_state: &mut Option<StreamSlowDataState>,
) where
    P: dstar_gateway_core::session::client::Protocol + std::fmt::Debug,
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
            // Emit the raw voice event first: the relay layer consumes
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
                reason: map_voice_end_reason(*reason),
                text,
                position,
            });
        }
        _ => {
            observer.on_event(translate_event(event));
        }
    }
}

/// Every recognised GPS sentence prefix paired with its terminator.
const GPS_SENTENCE_PATTERNS: [&[u8]; 3] = [b"$$CRC", b"$GPRMC", b"$GPGGA"];

/// Append a 5-byte GPS block payload to the running buffer, then
/// scan for complete sentences and parse each. Returns `true` if the
/// latest position changed.
///
/// Three sentence formats are recognised (per ircDDBGateway APRSCollector):
/// - `$$CRC` (DPRS), terminated by `\r` (0x0D).
/// - `$GPRMC` (NMEA), terminated by `\n` (0x0A).
/// - `$GPGGA` (NMEA), terminated by `\n` (0x0A).
///
/// The prefix may appear anywhere in the running buffer (not just
/// byte 0), because GPS payload chunks span many blocks and the first few
/// blocks can contain leading padding or the tail of a prior sentence.
/// Any bytes preceding a recognised prefix are dropped as noise once
/// the sentence lands.
fn ingest_gps_chunk(state: &mut StreamSlowDataState, chunk: &[u8]) -> bool {
    let mut changed = false;
    for &byte in chunk {
        if matches!(byte, b'\r' | b'\n') {
            for start in 0..state.gps_buffer.len() {
                let Some(tail) = state.gps_buffer.get(start..) else {
                    continue;
                };
                if !GPS_SENTENCE_PATTERNS
                    .iter()
                    .any(|prefix| tail.starts_with(prefix))
                {
                    continue;
                }
                let Ok(sentence) = std::str::from_utf8(tail) else {
                    continue;
                };
                if let Some(position) = parse_gps_sentence(sentence) {
                    if state.latest_position.as_ref() != Some(&position) {
                        state.latest_position = Some(position);
                        changed = true;
                    }
                    break;
                }
            }
            state.gps_buffer.clear();
            continue;
        }
        if state.gps_buffer.len() == GPS_BUFFER_CAP {
            drop(state.gps_buffer.drain(..GPS_BUFFER_CAP / 2));
        }
        state.gps_buffer.push(byte);
    }
    changed
}

/// Parse a complete GPS sentence into a [`GpsPosition`]. The prefix
/// has already been identified by the ingest loop, so this dispatches
/// directly.
fn parse_gps_sentence(sentence: &str) -> Option<GpsPosition> {
    if sentence.starts_with("$$CRC") {
        return match sentence.parse::<DprsReport>() {
            Ok(report) => Some(GpsPosition {
                callsign: report.source().as_str().to_owned(),
                latitude: report.latitude().degrees(),
                longitude: report.longitude().degrees(),
                symbol: report.symbol().code().as_char().to_string(),
                comment: (!report.comment().as_str().is_empty())
                    .then(|| report.comment().as_str().to_owned()),
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
        };
    }
    let body = checked_nmea_body(sentence)?;
    match body.split(',').next()? {
        "GPRMC" => parse_nmea_rmc(body),
        "GPGGA" => parse_nmea_gga(body),
        _ => None,
    }
}

/// Verify the complete ASCII XOR envelope, matching
/// `ircDDBGateway/Common/APRSCollector.cpp::checkXOR`.
fn checked_nmea_body(sentence: &str) -> Option<&str> {
    if sentence.len() >= 300 || !sentence.is_ascii() {
        return None;
    }
    let (body, checksum) = sentence.strip_prefix('$')?.split_once('*')?;
    if checksum.len() != 2 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let expected = u8::from_str_radix(checksum, 16).ok()?;
    (body.bytes().fold(0, |sum, byte| sum ^ byte) == expected).then_some(body)
}

/// Decode a comma-delimited NMEA coordinate field (`DDMM.MMMM`) plus
/// a hemisphere char (`N`/`S` or `E`/`W`) into decimal degrees.
fn nmea_coord_to_degrees(raw: &str, hemisphere: &str, deg_width: usize) -> Option<f64> {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.len() != deg_width + 2
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || (raw.contains('.') && fraction.is_empty())
    {
        return None;
    }
    let deg: f64 = raw.get(..deg_width)?.parse().ok()?;
    let minutes: f64 = raw.get(deg_width..)?.parse().ok()?;
    let maximum = if deg_width == 2 { 90.0 } else { 180.0 };
    if !minutes.is_finite() || deg > maximum || (deg >= maximum && minutes > 0.0) {
        return None;
    }
    if !(0.0..60.0).contains(&minutes) {
        return None;
    }
    let mut decimal = deg + minutes / 60.0;
    match (deg_width, hemisphere) {
        (2, "S") | (3, "W") => decimal = -decimal,
        (2, "N") | (3, "E") => {}
        _ => return None,
    }
    match deg_width {
        2 => Latitude::new(decimal).ok().map(Latitude::degrees),
        3 => Longitude::new(decimal).ok().map(Longitude::degrees),
        _ => None,
    }
}

/// Parse a `$GPRMC` sentence. Only lat/lon are extracted; the rest of
/// the NMEA fields (speed, heading, date) are ignored, since the UI only
/// shows position.
fn parse_nmea_rmc(sentence: &str) -> Option<GpsPosition> {
    // $GPRMC,hhmmss.ss,A,DDMM.MMMM,N,DDDMM.MMMM,W,speed,course,ddmmyy,...
    //   0    1         2 3         4 5          6
    let fields: Vec<&str> = sentence.split(',').collect();
    if fields.get(2) != Some(&"A") || fields.get(12) == Some(&"N") {
        return None;
    }
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
    if !matches!(fields.get(6), Some(&("1" | "2" | "3" | "4" | "5" | "6"))) {
        return None;
    }
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
    P: dstar_gateway_core::session::client::Protocol + std::fmt::Debug,
{
    match event {
        Event::Connected { .. } => ReflectorEvent::Connected,
        Event::Disconnected { reason } => ReflectorEvent::Disconnected {
            reason: map_disconnect_reason(*reason),
        },
        Event::PollEcho { .. } => ReflectorEvent::PollEcho,
        Event::VoiceStart {
            stream_id, header, ..
        } => ReflectorEvent::VoiceStart {
            stream_id: stream_id.get(),
            mycall: header.my_call.to_string(),
            suffix: header.my_suffix.to_string(),
            urcall: header.ur_call.to_string(),
            rpt1: header.rpt1.to_string(),
            rpt2: header.rpt2.to_string(),
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
            reason: map_voice_end_reason(*reason),
            // `handle_event` always produces the authoritative VoiceEnd
            // with the assembled text + GPS; this fallback only fires
            // for callers that bypass the state machine.
            text: None,
            position: None,
        },
        // `Event` is `#[non_exhaustive]` and carries a private
        // `__Phantom` variant to thread `P`. Surface anything future as
        // `Unknown` with its debug rendering instead of conflating it
        // with session teardown.
        other => ReflectorEvent::Unknown {
            detail: format!("{other:?}"),
        },
    }
}

/// Map a `dstar-gateway-core` [`DisconnectReason`] to the FFI-visible
/// [`DisconnectCause`]. The catch-all covers `#[non_exhaustive]` causes
/// added upstream that this build doesn't know yet.
const fn map_disconnect_reason(reason: DisconnectReason) -> DisconnectCause {
    match reason {
        DisconnectReason::Rejected => DisconnectCause::Rejected,
        DisconnectReason::UnlinkAcked => DisconnectCause::UnlinkAcked,
        DisconnectReason::KeepaliveInactivity => DisconnectCause::KeepaliveTimeout,
        DisconnectReason::DisconnectTimeout => DisconnectCause::DisconnectTimeout,
        _ => DisconnectCause::Unknown,
    }
}

/// Map a `dstar-gateway-core` [`VoiceEndReason`] to the FFI-visible
/// [`VoiceEndCause`]. The catch-all covers `#[non_exhaustive]` reasons
/// added upstream that this build doesn't know yet.
const fn map_voice_end_reason(reason: VoiceEndReason) -> VoiceEndCause {
    match reason {
        VoiceEndReason::Eot => VoiceEndCause::Eot,
        VoiceEndReason::Inactivity => VoiceEndCause::Inactivity,
        _ => VoiceEndCause::Unknown,
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
async fn resolve_peers(
    config: &ReflectorConnectionConfig,
) -> Result<Vec<SocketAddr>, ReflectorError> {
    if let Some(ip) = config.address.ip() {
        return Ok(vec![SocketAddr::new(ip, config.port.get())]);
    }
    let host = config
        .address
        .dns_name()
        .ok_or_else(|| ReflectorError::DnsFailed("checked DNS address missing".to_owned()))?;
    let addrs = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::lookup_host((host, config.port.get())),
    )
    .await
    .map_err(|_| ReflectorError::DnsFailed("DNS exceeded five seconds".to_owned()))?
    .map_err(|error| ReflectorError::DnsFailed(error.to_string()))?;
    let peers: Vec<_> = addrs.take(8).collect();
    if peers.is_empty() {
        return Err(ReflectorError::DnsFailed(
            "resolver returned no addresses".to_owned(),
        ));
    }
    Ok(peers)
}

/// Bind an ephemeral local UDP socket for a new reflector session.
async fn bind_local_socket(peer: SocketAddr) -> Result<Arc<UdpSocket>, ReflectorError> {
    let ip = if peer.is_ipv6() {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    };
    UdpSocket::bind(SocketAddr::new(ip, 0))
        .await
        .map(Arc::new)
        .map_err(|e| ReflectorError::SocketBindFailed(format!("{e}")))
}

/// Drive a `Session<P, Connecting>` to `Connected` via the shared pump
/// in `dstar_gateway::tokio_shell::drive_connecting`, mapping its typed
/// failures onto [`ReflectorError`].
async fn drive_handshake_to_connected<P>(
    session: Session<P, Connecting>,
    socket: &UdpSocket,
) -> Result<Session<P, Connected>, ReflectorError>
where
    P: dstar_gateway_core::session::client::Protocol,
{
    dstar_gateway::tokio_shell::drive_connecting(session, socket, HANDSHAKE_TIMEOUT)
        .await
        .map_err(|error| match error {
            dstar_gateway::tokio_shell::ConnectError::TimedOut(_) => {
                ReflectorError::HandshakeTimeout
            }
            other => ReflectorError::ConnectFailed(other.to_string()),
        })
}

/// Build, connect, and spawn a `DExtra` session.
async fn connect_dextra(
    station: RoutingCallsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<DExtra>, ReflectorError> {
    let configured = Session::<DExtra, _>::builder()
        .callsign(station)
        .local_module(local_module)
        .reflector_module(reflector_module)
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
    station: RoutingCallsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    reflector_callsign: DcsReflector,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<Dcs>, ReflectorError> {
    let configured = Session::<Dcs, _>::builder()
        .callsign(station)
        .local_module(local_module)
        .reflector_module(reflector_module)
        .dcs_reflector(reflector_callsign)
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
/// Authentication failure is retained while the UDP handshake is attempted
/// with an empty host list. If that also fails, both failures are returned.
async fn connect_dplus(
    station: RoutingCallsign,
    peer: std::net::SocketAddr,
    local_module: Module,
    reflector_module: Module,
    socket: Arc<UdpSocket>,
) -> Result<AsyncSession<DPlus>, ReflectorError> {
    let (hosts, auth_warning) = match AuthClient::new().authenticate(station.callsign()).await {
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
                // Preserve independent authentication and handshake failures.
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

// ---------------------------------------------------------------------------
// Tests: end-to-end slow-data GPS decoder
// ---------------------------------------------------------------------------

#[cfg(test)]
mod slow_data_gps_tests {
    use super::*;
    use dstar_gateway_core::StreamId;
    use dstar_gateway_core::slowdata::scramble;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Re-implement ircDDBGateway's `CSlowDataEncoder::setGPSData` + `getData`
    /// pair to produce the exact wire bytes (scrambled, in 3-byte frames)
    /// a Kenwood radio / hotspot would emit for the given DPRS sentence.
    ///
    /// Layout (from `SlowDataEncoder.cpp:329-357`):
    ///
    /// ```text
    /// for each chunk of up to 5 chars:
    ///     emit byte: (SLOW_DATA_TYPE_GPS=0x30) | dataLen
    ///     emit `dataLen` chars
    /// pad remaining buffer up to the nearest 60-byte multiple; the
    /// padding itself is emitted through the `(0x30 | 0) = 0x30`
    /// short-chunk path, so the tail of the stream is a run of 0x30
    /// bytes.
    /// ```
    fn encode_dprs_to_scrambled_frames(sentence: &str) -> Vec<[u8; 3]> {
        const TYPE_GPS: u8 = 0x30;
        const BLOCK_SIZE: usize = 6;
        const FULL_BLOCK_SIZE: usize = BLOCK_SIZE * 10;

        let bytes = sentence.as_bytes();
        let data_size = 1 + bytes.len().saturating_sub(1) / BLOCK_SIZE + bytes.len();
        let full_size = FULL_BLOCK_SIZE * (1 + (data_size.saturating_sub(1)) / FULL_BLOCK_SIZE);

        let mut buf: Vec<u8> = Vec::with_capacity(full_size);
        for chunk in bytes.chunks(5) {
            // `chunks(5)` yields 1..=5 bytes, always fitting the low nibble.
            buf.push(TYPE_GPS | u8::try_from(chunk.len()).unwrap_or(5));
            buf.extend_from_slice(chunk);
        }
        // Once the sentence is exhausted the reference encoder keeps
        // emitting `(TYPE_GPS | 0)` type bytes with no payload, so the
        // tail of the stream is a run of bare 0x30 bytes.
        while buf.len() < full_size {
            buf.push(TYPE_GPS);
        }
        buf.truncate(full_size);

        let mut frames: Vec<[u8; 3]> = Vec::new();
        for chunk in buf.chunks_exact(3) {
            let mut tri = [0u8; 3];
            tri.copy_from_slice(chunk);
            frames.push(scramble(tri));
        }
        frames
    }

    /// Feed the given scrambled frames through the real
    /// `StreamSlowDataState.push` pipeline and return the extracted
    /// latest position, if any.
    fn decode_frames(frames: &[[u8; 3]]) -> Option<GpsPosition> {
        let sid = StreamId::new(0x1234)?;
        let mut state = StreamSlowDataState::new(sid);

        let mut seq: u8 = 1;
        for &frame in frames {
            let _delta = state.push(frame, seq);
            seq += 1;
            if seq == 21 {
                let _sync = state.push([0x55; 3], 0);
                seq = 1;
            }
        }
        state.latest_position
    }

    #[test]
    fn decodes_dprs_sentence_with_cr_terminator() -> TestResult {
        let sentence = "$$CRC7FE5,W1AW    >APDPRS,DSTAR*:!4321.12N/07123.45W>Test\r";
        let frames = encode_dprs_to_scrambled_frames(sentence);
        assert!(
            frames.len() >= 20,
            "encoder should produce at least one full 60-byte row, got {}",
            frames.len()
        );

        let pos = decode_frames(&frames).ok_or("decoder should recover position")?;
        assert_eq!(pos.callsign.trim(), "W1AW");
        let expected_lat = 43.0 + 21.12 / 60.0;
        let expected_lon = -(71.0 + 23.45 / 60.0);
        assert!(
            (pos.latitude - expected_lat).abs() < 0.01,
            "lat {} not near {}",
            pos.latitude,
            expected_lat
        );
        assert!(
            (pos.longitude - expected_lon).abs() < 0.01,
            "lon {} not near {}",
            pos.longitude,
            expected_lon
        );
        Ok(())
    }

    #[test]
    fn no_terminator_means_no_position() {
        let sentence = "$$CRC1234,W1AW    >APDPRS,DSTAR*:!4321.12N/07123.45W>Test";
        let frames = encode_dprs_to_scrambled_frames(sentence);
        assert!(decode_frames(&frames).is_none());
    }

    #[test]
    fn nmea_rmc_sentence_decodes() -> TestResult {
        let sentence = "$GPRMC,123456.00,A,4321.1200,N,07123.4500,W,0.0,0.0,010125,,*20\n";
        let frames = encode_dprs_to_scrambled_frames(sentence);
        let pos = decode_frames(&frames).ok_or("decoder should recover GPRMC")?;
        let expected_lat = 43.0 + 21.12 / 60.0;
        assert!(
            (pos.latitude - expected_lat).abs() < 0.01,
            "lat {}",
            pos.latitude
        );
        Ok(())
    }

    fn nmea(body: &str) -> String {
        let checksum = body.bytes().fold(0_u8, |sum, byte| sum ^ byte);
        format!("${body}*{checksum:02X}\r\n")
    }

    #[test]
    fn malformed_bytes_are_bounded_and_do_not_hide_a_following_fix() -> TestResult {
        let mut state = StreamSlowDataState::new(StreamId::new(1).ok_or("stream")?);
        for _ in 0..400 {
            let _changed = ingest_gps_chunk(&mut state, &[0xFF; 5]);
            assert!(
                state.gps_buffer.len() <= GPS_BUFFER_CAP,
                "GPS retention exceeded its byte cap"
            );
        }
        let sentence = "$GPRMC,123456.00,A,4321.1200,N,07123.4500,W,0.0,0.0,010125,,*20\n";
        for chunk in sentence.as_bytes().chunks(5) {
            let _changed = ingest_gps_chunk(&mut state, chunk);
        }
        assert!(
            state.latest_position.is_some(),
            "valid following ASCII sentence was discarded"
        );
        assert!(
            state.gps_buffer.is_empty(),
            "complete sentence left retained bytes"
        );
        Ok(())
    }

    #[test]
    fn malformed_utf8_through_the_real_dispatcher_remains_bounded_and_recovers() -> TestResult {
        let stream = StreamId::new(1).ok_or("stream")?;
        let mut state = StreamSlowDataState::new(stream);
        let mut sequence = 1;
        for _ in 0..400 {
            for plain in [[0x35, 0xFF, 0xFF], [0xFF; 3]] {
                let _changed = state.push(scramble(plain), sequence);
                sequence += 1;
                if sequence == 21 {
                    let _sync = state.push([0x55; 3], 0);
                    sequence = 1;
                }
            }
            assert!(
                state.gps_buffer.len() <= GPS_BUFFER_CAP,
                "dispatcher expanded malformed bytes beyond the cap"
            );
        }
        let sentence = "$GPRMC,123456.00,A,4321.1200,N,07123.4500,W,0.0,0.0,010125,,*20\n";
        for frame in encode_dprs_to_scrambled_frames(sentence) {
            let _changed = state.push(frame, sequence);
            sequence += 1;
            if sequence == 21 {
                let _sync = state.push([0x55; 3], 0);
                sequence = 1;
            }
        }
        let position = state
            .latest_position
            .ok_or("valid following fix was lost")?;
        assert!(
            (position.latitude - (43.0 + 21.12 / 60.0)).abs() < 1e-9,
            "following fix has the wrong latitude"
        );
        Ok(())
    }

    #[test]
    fn numeric_integrity_and_fix_status_are_required() {
        for (latitude, north_south, longitude, east_west) in [
            ("9000.0001", "N", "07123.45", "W"),
            ("4360.00", "N", "07123.45", "W"),
            ("43NaN", "N", "07123.45", "W"),
            ("4321.12", "E", "07123.45", "W"),
            ("4321.12", "N", "18000.001", "E"),
            ("4321.12", "N", "07123.45", "S"),
            ("+321.12", "N", "07123.45", "W"),
        ] {
            let wire = nmea(&format!(
                "GPRMC,123456,A,{latitude},{north_south},{longitude},{east_west},0,0,010125,,"
            ));
            assert!(
                parse_gps_sentence(wire.trim_end()).is_none(),
                "invalid coordinate admitted: {wire}"
            );
        }
        for body in [
            "GPRMC,123456,V,4321.12,N,07123.45,W,0,0,010125,,",
            "GPGGA,123456,4321.12,N,07123.45,W,0,8,1,0,M,0,M,,",
        ] {
            assert!(
                parse_gps_sentence(nmea(body).trim_end()).is_none(),
                "invalid fix admitted: {body}"
            );
        }
        assert!(
            parse_gps_sentence("$GPRMC,123456.00,A,4321.1200,N,07123.4500,W,0.0,0.0,010125,,*31")
                .is_none(),
            "bad NMEA XOR was accepted"
        );
        assert!(
            parse_gps_sentence("$$CRC1234,W1AW    >APDPRS,DSTAR*:!4321.12N/07123.45W>Test")
                .is_none(),
            "bad DPRS CRC was accepted"
        );
        let endpoints = nmea("GPRMC,123456,A,9000,N,18000,E,0,0,010125,,");
        assert!(
            parse_gps_sentence(endpoints.trim_end()).is_some(),
            "geographic endpoints were rejected"
        );
        let gga = nmea("GPGGA,123456,4321.12,N,07123.45,W,1,8,1,0,M,0,M,,");
        assert!(
            parse_gps_sentence(gga.trim_end()).is_some(),
            "valid GGA was rejected"
        );
    }

    #[test]
    fn a_position_comment_cannot_override_its_enclosing_valid_report() -> TestResult {
        let embedded = nmea("GPRMC,000000,A,1000,N,02000,E,,,010125");
        assert!(
            parse_gps_sentence(embedded.trim_end()).is_some(),
            "embedded NMEA fixture is invalid"
        );
        let body = format!(
            "W1AW>APDPRS,DSTAR*:!3530.00N/08233.00W>{}",
            embedded.trim_end()
        );
        let crc = dstar_gateway_core::dprs::compute_crc(format!("{body}\r").as_bytes());
        let wire = format!("$$CRC{crc:04X},{body}\r");
        let _verified: DprsReport = wire.parse()?;
        let mut state = StreamSlowDataState::new(StreamId::new(1).ok_or("stream")?);
        let _changed = ingest_gps_chunk(&mut state, wire.as_bytes());
        let position = state
            .latest_position
            .ok_or("enclosing DPRS position missing")?;
        assert!(
            (position.latitude - 35.5).abs() < f64::EPSILON,
            "embedded NMEA text replaced the enclosing report"
        );
        Ok(())
    }

    #[test]
    fn test_scrambler_roundtrip_with_real_sentence() -> TestResult {
        let sentence = "$$CRC0001,XXX\r";
        let frames = encode_dprs_to_scrambled_frames(sentence);
        let plain0 =
            dstar_gateway_core::slowdata::descramble(*frames.first().ok_or("missing frame 0")?);
        let plain1 =
            dstar_gateway_core::slowdata::descramble(*frames.get(1).ok_or("missing frame 1")?);
        let block: Vec<u8> = plain0.iter().chain(plain1.iter()).copied().collect();
        assert_eq!(
            block.first(),
            Some(&0x35),
            "first block type byte should be 0x35 (GPS|5)"
        );
        assert_eq!(block.get(1..6), Some(b"$$CRC".as_slice()));
        Ok(())
    }

    /// Exercise the block-phase alignment: when joining mid-stream
    /// by skipping the first frame, every downstream block read is
    /// shifted by 3 bytes: the type nibble will be whatever byte 4
    /// of the original block was, never 0x3X. No GPS should decode.
    #[test]
    fn misaligned_stream_join_drops_gps() -> TestResult {
        let sentence = "$$CRC1234,W1AW    >APDPRS,DSTAR*:!4321.12N/07123.45W>\r";
        let frames = encode_dprs_to_scrambled_frames(sentence);
        let shifted = frames.get(1..).ok_or("need at least one frame")?;
        assert!(decode_frames(shifted).is_none());
        Ok(())
    }
}

#[cfg(test)]
mod cause_mapping_tests {
    use super::{
        DisconnectCause, DisconnectReason, VoiceEndCause, VoiceEndReason, map_disconnect_reason,
        map_voice_end_reason,
    };

    #[test]
    fn disconnect_reasons_map_to_typed_causes() {
        assert_eq!(
            map_disconnect_reason(DisconnectReason::Rejected),
            DisconnectCause::Rejected
        );
        assert_eq!(
            map_disconnect_reason(DisconnectReason::UnlinkAcked),
            DisconnectCause::UnlinkAcked
        );
        assert_eq!(
            map_disconnect_reason(DisconnectReason::KeepaliveInactivity),
            DisconnectCause::KeepaliveTimeout
        );
        assert_eq!(
            map_disconnect_reason(DisconnectReason::DisconnectTimeout),
            DisconnectCause::DisconnectTimeout
        );
    }

    #[test]
    fn voice_end_reasons_map_to_typed_causes() {
        assert_eq!(
            map_voice_end_reason(VoiceEndReason::Eot),
            VoiceEndCause::Eot
        );
        assert_eq!(
            map_voice_end_reason(VoiceEndReason::Inactivity),
            VoiceEndCause::Inactivity
        );
    }
}
