//! `DExtra` protocol (XRF reflectors, UDP port 30001).
//!
//! The simplest D-STAR reflector protocol. Uses DSVT framing for voice
//! packets (shared with `DPlus`). Connection management uses 11-byte
//! link/unlink packets and 9-byte keepalives.
//!
//! # Packet formats (per `g4klx/ircDDBGateway` and `LX3JL/xlxd`)
//!
//! | Packet       | Size | Format |
//! |--------------|------|--------|
//! | Connect      | 11   | callsign\[8\] + module + module + 0x0B |
//! | Disconnect   | 11   | callsign\[8\] + module + 0x20 + 0x00 |
//! | Poll         | 9    | callsign\[8\] + 0x00 |
//! | Voice header | 56   | DSVT header (see below) |
//! | Voice data   | 27   | DSVT voice (see below) |
//!
//! # DSVT framing
//!
//! ```text
//! Header (56 bytes):
//!   "DSVT" + 0x10 + 3 reserved + 0x20 0x00 0x01 0x00
//!   + stream_id[2 LE] + 0x80 + D-STAR header[41]
//!
//! Voice (27 bytes):
//!   "DSVT" + 0x20 + 3 reserved + 0x20 0x00 0x01 0x00
//!   + stream_id[2 LE] + seq + AMBE[9] + slow_data[3]
//! ```
//!
//! EOT is a voice packet with seq bit 6 set (0x40), AMBE silence,
//! and sync slow data bytes.
//!
//! # Keepalive
//!
//! Poll packets sent every 3 seconds. The reflector echoes them back.
//! If no echo is received within 30 seconds, the connection is
//! considered lost.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::error::Error;
use crate::header::{self, DStarHeader};
use crate::types::{Callsign, Module, StreamId};
use crate::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

use super::{ConnectionState, ReflectorEvent, format_hex_head};

/// Default `DExtra` port.
pub const DEFAULT_PORT: u16 = 30001;

/// Keepalive interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Connection timeout (no inbound traffic received).
///
/// Matches `ircDDBGateway/Common/DExtraHandler.cpp`'s 30 s
/// `m_pollInactivityTimer`, which is reset on every inbound packet
/// (poll echo at :274, AMBE voice at :710). Our [`DExtraClient::poll`]
/// mirrors that by resetting its internal `last_poll_received`
/// instant whenever a packet is successfully parsed.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time to wait for a disconnect ACK before giving up and
/// forcing the client to the `Disconnected` state.
///
/// Per T32: reflectors may never echo the unlink packet (network loss,
/// reflector crashed, UDP NAT expired). The client must not hang in
/// `Disconnecting` forever. Two seconds is chosen to comfortably cover
/// a round-trip on slow WAN links without making CLI tear-down feel
/// sluggish.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of times `DExtraClient::connect` retransmits the connect packet.
///
/// Per `ircDDBGateway/Common/DExtraProtocolHandler.cpp:106-110`.
const CONNECT_RETX: usize = 2;

/// Number of times `DExtraClient::send_header` retransmits the DSVT header.
///
/// Per `ircDDBGateway/Common/DExtraProtocolHandler.cpp:64-68`.
const HEADER_RETX: usize = 5;

/// Inter-copy delay for retransmission bursts.
const RETX_DELAY: Duration = Duration::from_millis(50);

/// DSVT magic bytes.
const DSVT: &[u8; 4] = b"DSVT";

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a `DExtra` connect/link packet (11 bytes).
///
/// Layout per `ircDDBGateway/Common/ConnectData.cpp:287-295`:
/// `[callsign[8], local_module, reflector_module, 0x00]`.
///
/// # Parameters
///
/// - `callsign`: originating station callsign (8-byte typed value)
/// - `local_module`: the module letter on the originating station
/// - `refl_module`: the module letter on the target reflector
///
/// # Panics
///
/// Cannot panic — inputs are all validated types and the slice copies
/// use fixed, statically-known lengths.
#[must_use]
pub fn build_connect(callsign: &Callsign, local_module: Module, refl_module: Module) -> [u8; 11] {
    let mut pkt = [0u8; 11];
    pkt[..8].copy_from_slice(callsign.as_bytes());
    pkt[8] = local_module.as_byte();
    pkt[9] = refl_module.as_byte();
    pkt[10] = 0x00;
    pkt
}

/// Build a disconnect/unlink packet (11 bytes).
///
/// # Panics
///
/// Cannot panic — inputs are all validated types and the slice copy
/// uses a fixed, statically-known length.
#[must_use]
pub fn build_disconnect(callsign: &Callsign, module: Module) -> [u8; 11] {
    let mut pkt = [0u8; 11];
    pkt[..8].copy_from_slice(callsign.as_bytes());
    pkt[8] = module.as_byte();
    pkt[9] = b' ';
    pkt[10] = 0x00;
    pkt
}

/// Build a poll/keepalive packet (9 bytes).
///
/// # Panics
///
/// Cannot panic — the callsign argument is a validated 8-byte type and
/// the slice copy uses a fixed, statically-known length.
#[must_use]
pub fn build_poll(callsign: &Callsign) -> [u8; 9] {
    let mut pkt = [0u8; 9];
    pkt[..8].copy_from_slice(callsign.as_bytes());
    pkt[8] = 0x00;
    pkt
}

/// Build a `DSVT` voice header packet (56 bytes).
///
/// # Panics
///
/// Cannot panic — `header.encode_for_dsvt()` is a fixed-length block,
/// `stream_id` is a validated non-zero u16, and all other writes are
/// straight appends with known sizes. The `debug_assert_eq!` on the
/// final length only fires in debug builds to catch future layout bugs.
#[must_use]
pub fn build_header(header: &DStarHeader, stream_id: StreamId) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(56);
    pkt.extend_from_slice(DSVT);
    pkt.push(0x10); // header flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.extend_from_slice(&[0x20, 0x00, 0x01, 0x02]); // config — band3 = 0x02
    pkt.extend_from_slice(&stream_id.get().to_le_bytes());
    pkt.push(0x80); // header indicator
    pkt.extend_from_slice(&header.encode_for_dsvt());
    debug_assert_eq!(pkt.len(), 56);
    pkt
}

/// Build a `DSVT` voice data packet (27 bytes).
///
/// # Panics
///
/// Cannot panic — `stream_id` is a validated non-zero u16, `frame.ambe`
/// and `frame.slow_data` are fixed-size arrays, and all other writes
/// are straight appends with known sizes.
#[must_use]
pub fn build_voice(stream_id: StreamId, seq: u8, frame: &VoiceFrame) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(27);
    pkt.extend_from_slice(DSVT);
    pkt.push(0x20); // voice flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.extend_from_slice(&[0x20, 0x00, 0x01, 0x02]); // config — band3 = 0x02
    pkt.extend_from_slice(&stream_id.get().to_le_bytes());
    pkt.push(seq);
    pkt.extend_from_slice(&frame.ambe);
    pkt.extend_from_slice(&frame.slow_data);
    debug_assert_eq!(pkt.len(), 27);
    pkt
}

/// Build a `DSVT` end-of-transmission packet (27 bytes).
///
/// # Panics
///
/// Cannot panic — delegates to [`build_voice`] which itself cannot
/// panic for validated inputs.
#[must_use]
pub fn build_eot(stream_id: StreamId, seq: u8) -> Vec<u8> {
    build_voice(
        stream_id,
        seq | 0x40,
        &VoiceFrame {
            ambe: AMBE_SILENCE,
            slow_data: DSTAR_SYNC_BYTES,
        },
    )
}

// ---------------------------------------------------------------------------
// Packet parser
// ---------------------------------------------------------------------------

/// Parse an incoming `DExtra` packet into a [`ReflectorEvent`].
///
/// Returns `None` if the packet format is not recognized.
///
/// # 11-byte arm classification
///
/// The 11-byte `DExtra` packet is a small family with overlapping
/// layouts. We disambiguate by inspecting bytes 9 and 10.
///
/// Transmit-side references (what reflectors/gateways actually emit):
///
/// - xlxd `CDextraProtocol::EncodeConnectAckPacket` at
///   `ref/xlxd/src/cdextraprotocol.cpp:508-529`. For `ProtRev == 2`
///   (XRF/modern) it writes 11 bytes laid out as
///   `[reflector callsign 8B][lm @ byte 8][rm @ byte 9][0x00 @ byte 10]`.
///   Byte 9 is the reflector module letter (uppercase A-Z — xlxd
///   validates with `CProtocol::IsLetter` at
///   `ref/xlxd/src/cprotocol.cpp:268-271`, which is literally
///   `c >= 'A' && c <= 'Z'`). For `ProtRev != 2` the same function
///   emits a 14-byte `…ACK\0` packet instead, which is handled by the
///   14-byte arm below rather than here.
/// - xlxd rev-2 disconnect ACK at `cdextraprotocol.cpp:181-184` echoes
///   the client's own 11-byte disconnect buffer back unchanged. That
///   buffer is produced by `CDextraProtocol::EncodeDisconnectPacket`
///   at `cdextraprotocol.cpp:538-542` as ten ASCII spaces followed by
///   `0x00`, so the echoed ACK has `0x20` at byte 9 and `0x00` at byte
///   10.
/// - ircDDBGateway `CConnectData::getDExtraData` at
///   `ref/ircDDBGateway/Common/ConnectData.cpp:278-321`: `CT_LINK1` /
///   `CT_LINK2` write 11 bytes with the reflector module letter at
///   byte 9 and `0x00` at byte 10 (lines 291-295); `CT_UNLINK` writes
///   11 bytes with space at byte 9 and `0x00` at byte 10
///   (lines 297-300).
///
/// Receive-side references (how the reference reflectors classify the
/// same bytes):
///
/// - xlxd `CDextraProtocol::IsValidConnectPacket` at
///   `cdextraprotocol.cpp:388-413`: 11 bytes with `data[9] != ' '` and
///   `IsLetter(data[9])` is a connect request. Line 396 derives the
///   legacy "revision 1" flag from `data[10] == 11` (that is, `0x0B`),
///   which is a revision-detection heuristic applied to incoming
///   requests from legacy clients.
/// - xlxd `CDextraProtocol::IsValidDisconnectPacket` at
///   `cdextraprotocol.cpp:415-425`: 11 bytes with `data[9] == ' '` is
///   an unlink; byte 10 is ignored.
/// - ircDDBGateway `CConnectData::setDExtraData` at
///   `ConnectData.cpp:114-157`: for `length == 11`, byte 9 == space is
///   `CT_UNLINK`, otherwise `CT_LINK1`. Byte 10 is ignored entirely.
///
/// We therefore disambiguate as follows:
///
/// - `(_, 0x0B)` → `Connected` — legacy "revision 1" marker retained
///   for symmetry with xlxd's rev-detection path
///   (`cdextraprotocol.cpp:396,399`), even though no current xlxd or
///   ircDDBGateway release emits `0x0B` in an ACK.
/// - `(b' ', 0x00)` → `Disconnected` — xlxd rev-2 echo of the client's
///   own disconnect (`cdextraprotocol.cpp:181-184,538-542`) and
///   ircDDBGateway `CT_UNLINK` (`ConnectData.cpp:127-129,297-300`).
/// - `(letter, 0x00)` where `letter` is uppercase A-Z → `Connected` —
///   modern xlxd rev-2 ACK and ircDDBGateway `CT_LINK1`
///   (`cdextraprotocol.cpp:508-521`, `ConnectData.cpp:130-131,291-295`).
///   We intentionally match xlxd's `IsLetter` (`cprotocol.cpp:268-271`,
///   uppercase A-Z only) rather than a broader alphabetic test, because
///   D-STAR module letters are always uppercase per
///   [`Module::try_from_char`].
/// - Any other `(_, 0x00)` combo → `Rejected` — 11-byte packets that
///   terminate in `0x00` but do not have a valid module letter or
///   space at byte 9 are not produced by any known reflector and are
///   treated as unrecognized rejections.
/// - Anything else → `None`.
#[must_use]
pub fn parse_packet(data: &[u8]) -> Option<ReflectorEvent> {
    // 11-byte connect/unlink ACK family.
    if data.len() == 11 {
        match (data[9], data[10]) {
            (_, 0x0B) => return Some(ReflectorEvent::Connected),
            (b' ', 0x00) => return Some(ReflectorEvent::Disconnected),
            (b9, 0x00) if b9.is_ascii_uppercase() => {
                return Some(ReflectorEvent::Connected);
            }
            (_, 0x00) => return Some(ReflectorEvent::Rejected),
            _ => {}
        }
    }
    // 14-byte XLX ACK/NAK form.
    if data.len() == 14 && &data[10..14] == b"ACK\0" {
        return Some(ReflectorEvent::Connected);
    }
    if data.len() == 14 && &data[10..14] == b"NAK\0" {
        return Some(ReflectorEvent::Rejected);
    }

    // Poll echo: 9 bytes ending with 0x00.
    if data.len() == 9 && data[8] == 0x00 {
        return Some(ReflectorEvent::PollEcho);
    }

    // DSVT packets (header or voice).
    if data.len() >= 17 && &data[0..4] == DSVT {
        let is_header = data[4] == 0x10;
        // Stream ID 0 is reserved per the D-STAR spec — a packet with
        // stream_id == 0 is malformed, so drop it via `?` on the
        // `Option` returned by `StreamId::new`.
        let stream_id = StreamId::new(u16::from_le_bytes([data[12], data[13]]))?;

        if is_header && data.len() == 56 {
            let mut arr = [0u8; header::ENCODED_LEN];
            arr.copy_from_slice(&data[15..56]);
            return Some(ReflectorEvent::VoiceStart {
                header: DStarHeader::decode(&arr),
                stream_id,
            });
        } else if !is_header && data.len() == 27 {
            let seq = data[14];
            let mut ambe = [0u8; 9];
            ambe.copy_from_slice(&data[15..24]);
            let mut slow_data = [0u8; 3];
            slow_data.copy_from_slice(&data[24..27]);

            if seq & 0x40 != 0 {
                return Some(ReflectorEvent::VoiceEnd { stream_id });
            }
            return Some(ReflectorEvent::VoiceData {
                stream_id,
                seq,
                frame: VoiceFrame { ambe, slow_data },
            });
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Async client
// ---------------------------------------------------------------------------

/// Async `DExtra` reflector client.
///
/// Manages a UDP connection to a DExtra/XRF reflector with automatic
/// keepalives. Call [`poll`](Self::poll) in a loop to receive events
/// and send keepalives. Use [`send_header`](Self::send_header),
/// [`send_voice`](Self::send_voice), and [`send_eot`](Self::send_eot)
/// to transmit voice streams.
///
/// For most users the unified [`crate::ReflectorClient`] is easier;
/// drop to this type when you need per-protocol control over a
/// DExtra/XRF (or XLX) reflector on UDP port 30001.
///
/// # Example
///
/// ```no_run
/// use dstar_gateway::protocol::dextra::DExtraClient;
/// use dstar_gateway::{Callsign, Module};
/// use std::time::Duration;
///
/// # async fn example() -> Result<(), dstar_gateway::Error> {
/// let mut client = DExtraClient::new(
///     Callsign::try_from_str("W1AW")?,
///     Module::try_from_char('B')?, // local module
///     Module::try_from_char('C')?, // reflector module
///     "1.2.3.4:30001".parse().unwrap(),
/// )
/// .await?;
/// client.connect_and_wait(Duration::from_secs(5)).await?;
/// // ... send voice via send_header + send_voice + send_eot ...
/// client.disconnect().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct DExtraClient {
    socket: UdpSocket,
    remote: SocketAddr,
    callsign: Callsign,
    module: Module,
    refl_module: Module,
    state: ConnectionState,
    last_poll_sent: Instant,
    last_poll_received: Instant,
    poll_interval: Duration,
    /// Timestamp of the most recent `disconnect()` call, used by
    /// [`poll`](Self::poll) to force a transition from `Disconnecting`
    /// to `Disconnected` after [`DISCONNECT_TIMEOUT`] if the reflector
    /// never acknowledges.
    disconnect_sent_at: Option<Instant>,
    /// Stream ID of the most recently active RX stream, used to
    /// deduplicate spurious `VoiceStart` events on mid-stream header
    /// refreshes.
    ///
    /// XRF/XLX reflectors re-transmit the 56-byte DSVT voice header at
    /// every superframe boundary (~420 ms) so late joiners can sync
    /// up. [`parse_packet`] emits `ReflectorEvent::VoiceStart` for each
    /// of those refreshes, which — without this tracker — would
    /// flicker the radio display and re-initialise TX state every
    /// superframe. [`poll`](Self::poll) suppresses the second and
    /// subsequent `VoiceStart` for a given `stream_id`, matching the
    /// stateful stream-ID tracking DCS uses for its C8 fix. Cleared to
    /// `None` on [`ReflectorEvent::VoiceEnd`].
    last_rx_stream_id: Option<StreamId>,

    /// Timestamp of the most recently received voice event, used by
    /// [`poll`](Self::poll) to synthesize a
    /// [`ReflectorEvent::VoiceEnd`] after
    /// [`VOICE_INACTIVITY_TIMEOUT`] of silence. Same mechanism as
    /// `DPlus`; see the docs on
    /// [`crate::protocol::dplus::DPlusClient`] for the full
    /// rationale.
    last_voice_rx: Option<Instant>,
}

/// Maximum time between voice events before the client synthesizes a
/// [`ReflectorEvent::VoiceEnd`] for the active stream. Matches
/// `ircDDBGateway/Common/DStarDefines.h:122` `NETWORK_TIMEOUT = 2`.
pub const VOICE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

impl DExtraClient {
    /// Create a new client and bind a local UDP socket.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the socket cannot be bound.
    pub async fn new(
        callsign: Callsign,
        local_module: Module,
        refl_module: Module,
        remote: SocketAddr,
    ) -> Result<Self, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let now = Instant::now();
        Ok(Self {
            socket,
            remote,
            callsign,
            module: local_module,
            refl_module,
            state: ConnectionState::Disconnected,
            last_poll_sent: now,
            last_poll_received: now,
            poll_interval: POLL_INTERVAL,
            disconnect_sent_at: None,
            last_rx_stream_id: None,
            last_voice_rx: None,
        })
    }

    /// Override the keepalive poll interval.
    ///
    /// Defaults to [`POLL_INTERVAL`] (3 seconds). Decrease this for
    /// links traversing NAT where connection-tracking timers drop idle
    /// flows faster than the default keepalive cadence.
    pub const fn set_poll_interval(&mut self, interval: Duration) {
        self.poll_interval = interval;
    }

    /// Send the connect request to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn connect(&mut self) -> Result<(), Error> {
        let pkt = build_connect(&self.callsign, self.module, self.refl_module);
        for i in 0..CONNECT_RETX {
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            if i + 1 < CONNECT_RETX {
                tokio::time::sleep(RETX_DELAY).await;
            }
        }
        self.state = ConnectionState::Connecting;
        tracing::debug!(
            target: "dstar_gateway::dextra",
            state = "Connecting",
            reflector = %self.remote,
            module = %self.module,
            refl_module = %self.refl_module,
            "DExtra state -> Connecting"
        );
        tracing::info!(
            reflector = %self.remote,
            module = %self.module,
            refl_module = %self.refl_module,
            "DExtra connect sent"
        );
        Ok(())
    }

    /// Station callsign this client was constructed with.
    #[must_use]
    pub const fn callsign(&self) -> &Callsign {
        &self.callsign
    }

    /// Local module letter this client was constructed with.
    #[must_use]
    pub const fn local_module(&self) -> Module {
        self.module
    }

    /// Reflector module letter this client was constructed with.
    #[must_use]
    pub const fn reflector_module(&self) -> Module {
        self.refl_module
    }

    /// Connect to the reflector and wait for the connection ACK or timeout.
    ///
    /// Drives the state machine internally: sends the connect packet
    /// (with retransmission), then polls until state is `Connected` or
    /// the timeout expires. Use this as a more convenient alternative
    /// to calling [`connect`](Self::connect) and [`poll`](Self::poll)
    /// in a loop.
    ///
    /// # Errors
    ///
    /// - [`Error::ConnectTimeout`] if the reflector does not acknowledge
    ///   within `timeout`
    /// - [`Error::Rejected`] if the reflector explicitly sends a NAK
    /// - [`Error::Io`] on underlying socket failure
    pub async fn connect_and_wait(&mut self, timeout: Duration) -> Result<(), Error> {
        let deadline = tokio::time::Instant::now() + timeout;
        self.connect().await?;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(Error::ConnectTimeout(timeout));
            }
            match tokio::time::timeout(remaining, self.poll()).await {
                Ok(Ok(Some(ReflectorEvent::Connected))) => {
                    // state update already done inside poll
                    return Ok(());
                }
                Ok(Ok(Some(ReflectorEvent::Rejected))) => return Err(Error::Rejected),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(Error::ConnectTimeout(timeout)),
            }
        }
    }

    /// Send the disconnect request to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        let pkt = build_disconnect(&self.callsign, self.module);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Disconnecting;
        self.disconnect_sent_at = Some(Instant::now());
        tracing::debug!(
            target: "dstar_gateway::dextra",
            state = "Disconnecting",
            "DExtra state -> Disconnecting"
        );
        tracing::info!("DExtra disconnect sent");
        Ok(())
    }

    /// Poll for the next event from the reflector.
    ///
    /// This method:
    /// 1. Sends a keepalive if the poll interval has elapsed.
    /// 2. Receives and parses the next UDP packet.
    /// 3. Returns the parsed event, or `None` on timeout.
    ///
    /// Call this in a loop to maintain the connection and receive
    /// voice frames.
    ///
    /// # Errors
    ///
    /// Returns an I/O error on socket failures.
    #[allow(clippy::too_many_lines)]
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, Error> {
        // Force Disconnecting -> Disconnected after DISCONNECT_TIMEOUT
        // if the reflector never echoes back the unlink. This keeps
        // callers from hanging forever when the network drops the ACK.
        if self.state == ConnectionState::Disconnecting
            && let Some(sent) = self.disconnect_sent_at
            && sent.elapsed() >= DISCONNECT_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            self.disconnect_sent_at = None;
            tracing::debug!(
                target: "dstar_gateway::dextra",
                state = "Disconnected",
                reason = "disconnect_timeout",
                "DExtra state -> Disconnected (unlink ACK never arrived)"
            );
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Synthesize a VoiceEnd if the reflector stopped mid-stream
        // without sending an EOT packet. See the equivalent comment
        // in DPlusClient::poll for the full rationale; this matches
        // ircDDBGateway/Common/DExtraHandler.cpp:57 which declares
        // `m_inactivityTimer(1000U, NETWORK_TIMEOUT)` with the same
        // 2-second window.
        if let Some(last) = self.last_voice_rx
            && last.elapsed() >= VOICE_INACTIVITY_TIMEOUT
            && let Some(stream_id) = self.last_rx_stream_id
        {
            self.last_rx_stream_id = None;
            self.last_voice_rx = None;
            tracing::debug!(
                target: "dstar_gateway::dextra",
                stream_id = %stream_id,
                "DExtra synthesizing VoiceEnd after voice inactivity timeout"
            );
            return Ok(Some(ReflectorEvent::VoiceEnd { stream_id }));
        }

        // Send keepalive if needed.
        if self.state == ConnectionState::Connected
            && self.last_poll_sent.elapsed() >= self.poll_interval
        {
            let pkt = build_poll(&self.callsign);
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            self.last_poll_sent = Instant::now();
            tracing::trace!(
                target: "dstar_gateway::dextra",
                len = pkt.len(),
                head = %format_hex_head(&pkt),
                "DExtra tx keepalive"
            );
        }
        // Guard: no other send paths in poll() depend on the raw callsign.
        // Keeping this comment so future reviewers remember `pkt` is a
        // stack array now, not a heap Vec.

        // Check for connection timeout.
        if self.state == ConnectionState::Connected
            && self.last_poll_received.elapsed() >= POLL_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            tracing::debug!(
                target: "dstar_gateway::dextra",
                state = "Disconnected",
                reason = "poll_timeout",
                "DExtra state -> Disconnected (keepalive timeout)"
            );
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Receive with timeout.
        let mut buf = [0u8; 2048];
        let recv =
            tokio::time::timeout(Duration::from_millis(100), self.socket.recv_from(&mut buf)).await;

        let (len, _addr) = match recv {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Ok(None), // timeout, no data
        };

        tracing::trace!(
            target: "dstar_gateway::dextra",
            len,
            head = %format_hex_head(&buf[..len]),
            "DExtra rx"
        );

        let Some(event) = parse_packet(&buf[..len]) else {
            return Ok(None);
        };

        // Any successfully parsed packet proves the link is alive, so
        // reset the keepalive clock unconditionally. See the matching
        // fix in `DPlusClient::poll` for the full rationale — the
        // previous per-arm reset only fired on `PollEcho`, which let
        // long voice bursts silently trip the `POLL_TIMEOUT` guard
        // and drop the connection mid-transmission. ircDDBGateway's
        // `DExtraHandler::processInt(CAMBEData)` resets
        // `m_pollInactivityTimer` at
        // `ref/ircDDBGateway/Common/DExtraHandler.cpp:710`.
        self.last_poll_received = Instant::now();

        // Update state based on event.
        match &event {
            ReflectorEvent::Connected => {
                self.state = ConnectionState::Connected;
                tracing::debug!(
                    target: "dstar_gateway::dextra",
                    state = "Connected",
                    "DExtra state -> Connected"
                );
            }
            ReflectorEvent::Rejected | ReflectorEvent::Disconnected => {
                self.state = ConnectionState::Disconnected;
                self.disconnect_sent_at = None;
                tracing::debug!(
                    target: "dstar_gateway::dextra",
                    state = "Disconnected",
                    reason = "reflector_reply",
                    "DExtra state -> Disconnected (reflector rejected or closed)"
                );
            }
            ReflectorEvent::PollEcho => {
                tracing::trace!(
                    target: "dstar_gateway::dextra",
                    "DExtra poll echo"
                );
            }
            ReflectorEvent::VoiceStart { stream_id, .. } => {
                // XRF/XLX reflectors retransmit the DSVT voice header
                // every superframe (~420 ms) so late joiners can sync.
                // The first `VoiceStart` for a given `stream_id` is the
                // real stream start; subsequent ones are keep-alive
                // refreshes and must not propagate to the caller or the
                // REPL will re-announce the stream and re-send the
                // MMDVM header to the radio every superframe. Matches
                // the stateful-tracking approach DCS uses for its C8
                // fix (see `DcsClient::poll`).
                if self.last_rx_stream_id == Some(*stream_id) {
                    self.last_voice_rx = Some(Instant::now());
                    tracing::trace!(
                        target: "dstar_gateway::dextra",
                        stream_id = %stream_id,
                        "DExtra voice header rx (suppressed: mid-stream refresh)"
                    );
                    return Ok(None);
                }
                self.last_rx_stream_id = Some(*stream_id);
                self.last_voice_rx = Some(Instant::now());
                tracing::debug!(
                    target: "dstar_gateway::dextra",
                    stream_id = %stream_id,
                    "DExtra voice header rx"
                );
            }
            ReflectorEvent::VoiceData { stream_id, seq, .. } => {
                // Do NOT touch `last_rx_stream_id` here. Only
                // `VoiceStart` updates the tracker. This lets the
                // next header refresh fire a real `VoiceStart` when
                // we joined mid-stream and the first packet we saw
                // was voice data — e.g. when the bounded drain cap
                // picks up a stream in the middle of a superframe,
                // or when we connect while a transmission is already
                // in progress. The caller (REPL) dedupes the real
                // VoiceStart via its own `rx_stream_id` field, so
                // this cannot cause spurious popups.
                self.last_voice_rx = Some(Instant::now());
                tracing::trace!(
                    target: "dstar_gateway::dextra",
                    stream_id = %stream_id,
                    seq = *seq,
                    "DExtra voice data rx"
                );
            }
            ReflectorEvent::VoiceEnd { stream_id } => {
                if self.last_rx_stream_id == Some(*stream_id) {
                    self.last_rx_stream_id = None;
                }
                self.last_voice_rx = None;
                tracing::debug!(
                    target: "dstar_gateway::dextra",
                    stream_id = %stream_id,
                    "DExtra voice EOT rx"
                );
            }
        }

        Ok(Some(event))
    }

    /// Send a voice header to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_header(
        &mut self,
        header: &DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        let pkt = build_header(header, stream_id);
        tracing::debug!(
            target: "dstar_gateway::dextra",
            stream_id = %stream_id,
            "DExtra voice header tx"
        );
        tracing::trace!(
            target: "dstar_gateway::dextra",
            len = pkt.len(),
            head = %format_hex_head(&pkt),
            "DExtra tx header"
        );
        for i in 0..HEADER_RETX {
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            if i + 1 < HEADER_RETX {
                tokio::time::sleep(RETX_DELAY).await;
            }
        }
        Ok(())
    }

    /// Send a voice data frame to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        let pkt = build_voice(stream_id, seq, frame);
        tracing::trace!(
            target: "dstar_gateway::dextra",
            stream_id = %stream_id,
            seq,
            len = pkt.len(),
            "DExtra tx voice"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        Ok(())
    }

    /// Send an end-of-transmission to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), Error> {
        let pkt = build_eot(stream_id, seq);
        tracing::debug!(
            target: "dstar_gateway::dextra",
            stream_id = %stream_id,
            seq,
            "DExtra voice EOT tx"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        Ok(())
    }

    /// Current connection state.
    #[must_use]
    pub const fn state(&self) -> ConnectionState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Suffix;

    fn cs(s: &str) -> Callsign {
        Callsign::try_from_str(s).expect("valid test callsign")
    }

    fn m(c: char) -> Module {
        Module::try_from_char(c).expect("valid test module")
    }

    fn sid(n: u16) -> StreamId {
        StreamId::new(n).expect("non-zero test stream id")
    }

    #[test]
    fn connect_packet_format() {
        let pkt = build_connect(&cs("W1AW"), m('A'), m('A'));
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b'A');
        assert_eq!(pkt[10], 0x00); // was 0x0B
    }

    #[test]
    fn connect_packet_has_reflector_module_at_byte_9_and_null_terminator() {
        // Per ircDDBGateway/Common/ConnectData.cpp:287-295, the DExtra
        // connect packet is:
        //   [callsign8, local_module, reflector_module, 0x00]
        //
        // Previously byte 9 duplicated the local module and byte 10
        // was 0x0B, making cross-module linking (e.g. B -> XLX307C)
        // impossible.
        let pkt = build_connect(&cs("W1AW"), m('A'), m('C'));
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A', "local module at byte 8");
        assert_eq!(pkt[9], b'C', "reflector module at byte 9");
        assert_eq!(pkt[10], 0x00, "null terminator at byte 10 (not 0x0B)");
    }

    #[test]
    fn disconnect_packet_format() {
        let pkt = build_disconnect(&cs("W1AW"), m('A'));
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b' ');
        assert_eq!(pkt[10], 0x00);
    }

    #[test]
    fn poll_packet_format() {
        let pkt = build_poll(&cs("W1AW"));
        assert_eq!(pkt.len(), 9);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
    }

    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        }
    }

    #[test]
    fn header_packet_size() {
        let pkt = build_header(&test_header(), sid(0x1234));
        assert_eq!(pkt.len(), 56);
        assert_eq!(&pkt[0..4], b"DSVT");
        assert_eq!(pkt[4], 0x10);
    }

    #[test]
    fn voice_packet_size() {
        let frame = VoiceFrame {
            ambe: [0x01; 9],
            slow_data: [0x02; 3],
        };
        let pkt = build_voice(sid(0x1234), 5, &frame);
        assert_eq!(pkt.len(), 27);
        assert_eq!(&pkt[0..4], b"DSVT");
        assert_eq!(pkt[4], 0x20);
    }

    #[test]
    fn eot_has_flag_set() {
        let pkt = build_eot(sid(0x1234), 3);
        assert_eq!(pkt[14] & 0x40, 0x40);
        assert_eq!(&pkt[15..24], &AMBE_SILENCE);
        assert_eq!(&pkt[24..27], &DSTAR_SYNC_BYTES);
    }

    #[test]
    fn parse_connect_ack() {
        let pkt = build_connect(&cs("W1AW"), m('A'), m('A'));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_classic_connect_ack_0x0b() {
        // Classic ircDDBGateway reflectors still send 0x0B as the
        // connect ACK terminator; accept both.
        let mut pkt = build_connect(&cs("W1AW"), m('A'), m('A'));
        pkt[10] = 0x0B;
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_disconnect_ack_is_disconnected() {
        // T32: the parser distinguishes disconnect ACK from connect
        // ACK by inspecting byte 9. The disconnect packet has space
        // (0x20) at byte 9 and 0x00 at byte 10 — xlxd rev 2 echoes
        // this buffer back as the unlink ACK.
        let pkt = build_disconnect(&cs("W1AW"), m('A'));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Disconnected));
    }

    #[test]
    fn parse_nak_like_11_byte_packet_rejected() {
        // 11 bytes terminating in 0x00 but with neither a letter nor
        // a space at byte 9 is unrecognized per xlxd's rules and is
        // mapped to Rejected so the client can drop the connection.
        let mut pkt = [0u8; 11];
        pkt[..8].copy_from_slice(b"W1AW    ");
        pkt[8] = b'A';
        pkt[9] = 0x00; // not a letter, not a space
        pkt[10] = 0x00;
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Rejected));
    }

    #[test]
    fn parse_xlx_nak_still_rejects() {
        // 14-byte XLX "NAK\0" arm is still the rejection path.
        let mut pkt = Vec::with_capacity(14);
        pkt.extend_from_slice(b"W1AW    ");
        pkt.push(b'A');
        pkt.push(b'A');
        pkt.extend_from_slice(b"NAK\0");
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Rejected));
    }

    #[test]
    fn parse_poll_echo() {
        let pkt = build_poll(&cs("W1AW"));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
    }

    #[test]
    fn header_roundtrip() {
        let hdr = test_header();
        let pkt = build_header(&hdr, sid(0xABCD));
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceStart { header, stream_id } => {
                assert_eq!(stream_id.get(), 0xABCD);
                assert_eq!(header.my_call.as_bytes(), b"W1AW    ");
                assert_eq!(header.rpt1.as_bytes(), b"REF030 C");
            }
            other => panic!("expected VoiceStart, got {other:?}"),
        }
    }

    #[test]
    fn voice_roundtrip() {
        let frame = VoiceFrame {
            ambe: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99],
            slow_data: [0xAA, 0xBB, 0xCC],
        };
        let pkt = build_voice(sid(0x5678), 7, &frame);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceData {
                stream_id,
                seq,
                frame: f,
            } => {
                assert_eq!(stream_id.get(), 0x5678);
                assert_eq!(seq, 7);
                assert_eq!(f, frame);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn eot_roundtrip() {
        let pkt = build_eot(sid(0x5678), 3);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceEnd { stream_id } => {
                assert_eq!(stream_id.get(), 0x5678);
            }
            other => panic!("expected VoiceEnd, got {other:?}"),
        }
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_packet(&[]).is_none());
        assert!(parse_packet(&[0xFF; 5]).is_none());
    }

    #[test]
    fn stream_id_is_little_endian_in_voice_header() {
        let pkt = build_header(&test_header(), sid(0x1234));
        // Per xlxd/src/cdextraprotocol.cpp:447,468 and
        // ircDDBGateway/Common/AMBEData.cpp:142, stream ID is written
        // low-byte-first at offsets [12..14].
        assert_eq!(pkt[12], 0x34, "stream ID low byte (LE)");
        assert_eq!(pkt[13], 0x12, "stream ID high byte (LE)");
    }

    #[test]
    fn stream_id_is_little_endian_in_voice_data() {
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let pkt = build_voice(sid(0xABCD), 5, &frame);
        assert_eq!(pkt[12], 0xCD, "stream ID low byte (LE)");
        assert_eq!(pkt[13], 0xAB, "stream ID high byte (LE)");
    }

    #[test]
    fn parse_reads_stream_id_little_endian() {
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let pkt = build_voice(sid(0xABCD), 5, &frame);
        let evt = parse_packet(&pkt).expect("valid voice packet");
        match evt {
            ReflectorEvent::VoiceData { stream_id, .. } => assert_eq!(stream_id.get(), 0xABCD),
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn voice_header_has_band3_byte_0x02() {
        let pkt = build_header(&test_header(), sid(0x1234));
        // Per xlxd/src/cdextraprotocol.cpp:552,567,581, config bytes
        // [8..12] must be [0x20, 0x00, 0x01, 0x02]. Previously we
        // wrote [0x20, 0x00, 0x01, 0x00] which some xlxd-family
        // reflectors silently drop.
        assert_eq!(pkt[8], 0x20);
        assert_eq!(pkt[9], 0x00);
        assert_eq!(pkt[10], 0x01);
        assert_eq!(pkt[11], 0x02, "DSVT band3 byte must be 0x02");
    }

    #[test]
    fn dextra_build_header_zeros_flag_bytes_at_dsvt_offsets() {
        // DSVT voice header layout:
        //   [0..4] "DSVT"
        //   [4]    0x10    (header flag)
        //   [5..8] 3×0x00  (reserved)
        //   [8..12] config [0x20, 0x00, 0x01, 0x02]
        //   [12..14] stream_id LE
        //   [14]   0x80    (header indicator)
        //   [15..56] DStarHeader.encode() (41 bytes)
        //
        // So header flag bytes are at packet offsets 15, 16, 17.
        let hdr = DStarHeader {
            flag1: 0xAA,
            flag2: 0xBB,
            flag3: 0xCC,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        };
        let pkt = build_header(&hdr, sid(0x1234));
        assert_eq!(pkt[15], 0, "flag1 at DSVT offset 15 must be zeroed");
        assert_eq!(pkt[16], 0, "flag2 at DSVT offset 16 must be zeroed");
        assert_eq!(pkt[17], 0, "flag3 at DSVT offset 17 must be zeroed");
    }

    #[test]
    fn voice_data_has_band3_byte_0x02() {
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let pkt = build_voice(sid(0x1234), 0, &frame);
        assert_eq!(pkt[11], 0x02, "DSVT band3 byte must be 0x02");
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(data in proptest::collection::vec(proptest::num::u8::ANY, 0..2048)) {
            let _ = parse_packet(&data);
        }
    }

    #[tokio::test]
    async fn dextra_connect_and_wait_succeeds_on_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a fake ACK responder that echoes a valid connect ACK
        // (11 bytes with 0x00 at byte 10 — the modern DExtra format).
        let _responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            let ack = build_connect(&cs("W1AW"), m('A'), m('A'));
            let _ = listener.send_to(&ack, src).await.unwrap();
        });

        let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_secs(2)).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(client.state(), ConnectionState::Connected);
    }

    #[tokio::test]
    async fn dextra_connect_and_wait_times_out_without_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Do not spawn a responder — client should time out.
        // Keep the listener alive so the port isn't reclaimed.
        let _keep = listener;
        let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(Error::ConnectTimeout(_))),
            "expected ConnectTimeout, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dextra_connect_and_wait_returns_rejected_on_nak() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let _responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            // 14-byte XLX NAK
            let mut nak = Vec::with_capacity(14);
            nak.extend_from_slice(b"W1AW    ");
            nak.push(b'A');
            nak.push(b'A');
            nak.extend_from_slice(b"NAK\0");
            let _ = listener.send_to(&nak, src).await.unwrap();
        });

        let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_secs(2)).await;
        assert!(
            matches!(result, Err(Error::Rejected)),
            "expected Rejected, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dextra_set_poll_interval_stored() {
        // Bind a throwaway socket as the "remote" — we never send.
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = DExtraClient::new(cs("W1AW"), m('A'), m('A'), addr)
            .await
            .unwrap();
        assert_eq!(client.poll_interval, POLL_INTERVAL);
        let new_interval = Duration::from_millis(750);
        client.set_poll_interval(new_interval);
        assert_eq!(client.poll_interval, new_interval);
    }

    /// Spin up a `DExtraClient` wired to a loopback UDP server that
    /// we control. Returns the client (pre-bound to loopback and
    /// flipped to `Connected`) plus the server socket and the
    /// client's local address so the test can inject packets at it.
    async fn loopback_connected_client() -> (DExtraClient, UdpSocket, SocketAddr) {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let mut client = DExtraClient::new(cs("W1AW"), m('B'), m('C'), server_addr)
            .await
            .unwrap();
        // `DExtraClient::new` binds 0.0.0.0 which doesn't route; rebind
        // to loopback for the test so `server.send_to(client_addr)` works.
        client.socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.socket.local_addr().unwrap();
        client.state = ConnectionState::Connected;
        (client, server, client_addr)
    }

    #[tokio::test]
    async fn dextra_suppresses_mid_stream_header_refresh() {
        // Regression for C3: XRF/XLX reflectors retransmit the 56-byte
        // DSVT voice header every superframe (~420 ms). Before this
        // fix, `parse_packet` emitted a fresh `VoiceStart` event for
        // each retransmission, causing the REPL to re-announce the
        // stream and re-send an MMDVM header to the radio every
        // superframe. The `last_rx_stream_id` tracker in
        // `DExtraClient::poll` must suppress every `VoiceStart` after
        // the first one with the same stream_id.
        let (mut client, server, client_addr) = loopback_connected_client().await;
        let pkt = build_header(&test_header(), sid(0xCAFE));

        // First header for stream 0xCAFE -> VoiceStart fires.
        let _n = server.send_to(&pkt, client_addr).await.unwrap();
        let evt1 = client.poll().await.unwrap();
        assert!(
            matches!(
                evt1,
                Some(ReflectorEvent::VoiceStart { stream_id, .. }) if stream_id.get() == 0xCAFE
            ),
            "first header must fire VoiceStart(0xCAFE), got {evt1:?}"
        );

        // Second identical header (mid-stream refresh) -> suppressed.
        let _n = server.send_to(&pkt, client_addr).await.unwrap();
        let evt2 = client.poll().await.unwrap();
        assert!(
            evt2.is_none(),
            "mid-stream header refresh must be suppressed, got {evt2:?}"
        );
    }

    #[tokio::test]
    async fn dextra_emits_fresh_voice_start_on_new_stream_id() {
        // After a real stream change (different stream_id), the
        // tracker must let the new VoiceStart through.
        let (mut client, server, client_addr) = loopback_connected_client().await;

        let pkt_a = build_header(&test_header(), sid(0x1111));
        let _n = server.send_to(&pkt_a, client_addr).await.unwrap();
        let evt_a = client.poll().await.unwrap();
        assert!(
            matches!(
                evt_a,
                Some(ReflectorEvent::VoiceStart { stream_id, .. }) if stream_id.get() == 0x1111
            ),
            "first VoiceStart for 0x1111 must fire, got {evt_a:?}"
        );

        let pkt_b = build_header(&test_header(), sid(0x2222));
        let _n = server.send_to(&pkt_b, client_addr).await.unwrap();
        let evt_b = client.poll().await.unwrap();
        assert!(
            matches!(
                evt_b,
                Some(ReflectorEvent::VoiceStart { stream_id, .. }) if stream_id.get() == 0x2222
            ),
            "new stream_id 0x2222 must fire a fresh VoiceStart, got {evt_b:?}"
        );
    }

    #[tokio::test]
    async fn dextra_voice_end_clears_stream_tracking() {
        // After a VoiceEnd clears `last_rx_stream_id`, any subsequent
        // VoiceStart — even with the same stream_id as the just-ended
        // stream — must fire.
        let (mut client, server, client_addr) = loopback_connected_client().await;

        let hdr_pkt = build_header(&test_header(), sid(0xABCD));
        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt1 = client.poll().await.unwrap();
        assert!(
            matches!(evt1, Some(ReflectorEvent::VoiceStart { .. })),
            "first VoiceStart must fire, got {evt1:?}"
        );

        // Send the canonical 27-byte DExtra EOT (voice frame with seq
        // bit 6 set) for the same stream.
        let eot_pkt = build_eot(sid(0xABCD), 0);
        let _n = server.send_to(&eot_pkt, client_addr).await.unwrap();
        let evt_end = client.poll().await.unwrap();
        assert!(
            matches!(evt_end, Some(ReflectorEvent::VoiceEnd { .. })),
            "VoiceEnd must fire, got {evt_end:?}"
        );

        // A brand-new header reusing the same stream_id must now fire
        // a fresh VoiceStart because the tracker was cleared on
        // VoiceEnd.
        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt2 = client.poll().await.unwrap();
        assert!(
            matches!(
                evt2,
                Some(ReflectorEvent::VoiceStart { stream_id, .. }) if stream_id.get() == 0xABCD
            ),
            "reused stream_id after VoiceEnd must fire fresh VoiceStart, got {evt2:?}"
        );
    }

    #[tokio::test]
    async fn dextra_mid_stream_join_still_fires_voice_start_on_next_header() {
        // Regression: mid-stream join via VoiceData must NOT cache the
        // stream_id in `last_rx_stream_id`. Otherwise the next header
        // refresh (~420 ms later) would be suppressed and the REPL
        // would never get a VoiceStart to announce the stream — which
        // manifested live as "sometimes the D-STAR popup never appears
        // when someone is transmitting." The VoiceData arm must leave
        // the tracker untouched; only the VoiceStart arm updates it.
        let (mut client, server, client_addr) = loopback_connected_client().await;

        let data_pkt = build_voice(sid(0xBEEF), 5, &VoiceFrame::silence());
        let _n = server.send_to(&data_pkt, client_addr).await.unwrap();
        let evt_data = client.poll().await.unwrap();
        assert!(
            matches!(
                evt_data,
                Some(ReflectorEvent::VoiceData { stream_id, .. }) if stream_id.get() == 0xBEEF
            ),
            "mid-stream VoiceData must propagate, got {evt_data:?}"
        );

        let hdr_pkt = build_header(&test_header(), sid(0xBEEF));
        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt_hdr = client.poll().await.unwrap();
        assert!(
            matches!(
                evt_hdr,
                Some(ReflectorEvent::VoiceStart { stream_id, .. }) if stream_id.get() == 0xBEEF
            ),
            "header after mid-stream VoiceData must still fire VoiceStart, got {evt_hdr:?}"
        );

        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt_refresh = client.poll().await.unwrap();
        assert!(
            evt_refresh.is_none(),
            "second header for known stream must be suppressed, got {evt_refresh:?}"
        );
    }
}
