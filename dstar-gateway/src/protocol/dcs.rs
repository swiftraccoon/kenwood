//! `DCS` protocol (`DCS` reflectors, UDP port 30051).
//!
//! `DCS` uses 100-byte voice packets that embed the full D-STAR header
//! in every frame (unlike DExtra/DPlus which only send the header once).
//! This makes the protocol resilient to packet loss — any single voice
//! packet carries enough routing information to reconstruct the stream.
//!
//! Connection uses a 519-byte packet containing an HTML client ID.
//! Keepalive interval is 2 seconds.
//!
//! # Packet formats (per `g4klx/ircDDBGateway` and `LX3JL/xlxd`)
//!
//! | Packet       | Size   | Format |
//! |--------------|--------|--------|
//! | Connect      | 519    | callsign\[8\] + module + reflector\_module + 0x00 + reflector\[7\] + HTML |
//! | Disconnect   | 19     | callsign\[8\] + module + 0x20 + 0x00 + reflector\[8\] |
//! | Poll         | 17     | callsign\[8\] + 0x00 + name\[8\] |
//! | ACK          | 14     | callsign\[7\] + 0x20 + module + refl\_module + "ACK" + 0x00 |
//! | NAK          | 14     | callsign\[7\] + 0x20 + module + refl\_module + "NAK" + 0x00 |
//! | Voice        | 100    | "0001" + flags\[3\] + header fields + stream\_id\[2 LE\] + seq + AMBE\[9\] + slow\[3\] + rpt\_seq\[3 LE\] + trailer |
//!
//! # 100-byte voice layout
//!
//! ```text
//! Offset  Length  Field
//! 0       4       "0001" magic
//! 4       3       Flag bytes (control, reserved, reserved)
//! 7       8       RPT2 callsign (space-padded)
//! 15      8       RPT1 callsign (space-padded)
//! 23      8       YOUR callsign (space-padded)
//! 31      8       MY callsign (space-padded)
//! 39      4       MY suffix (space-padded)
//! 43      2       Stream ID (little-endian)
//! 45      1       Sequence number (bit 6 = end-of-transmission)
//! 46      9       AMBE voice data
//! 55      3       Slow data (or sync bytes 0x55 0x55 0x55 for EOT)
//! 58      3       Repeater sequence counter (little-endian, 24-bit)
//! 61      1       0x01
//! 62      1       0x00
//! 63      1       0x21
//! 64      36      Text / padding (zeros)
//! ```
//!
//! # Keepalive
//!
//! 17-byte poll: callsign\[8\] + 0x00 + reflector\[8\], sent every 2 seconds.
//! The reflector echoes a 22-byte response.
//!
//! # Reference implementations
//!
//! Protocol formats verified against `g4klx/ircDDBGateway`
//! `DCSProtocolHandler.cpp`, `ConnectData.cpp`, `AMBEData.cpp`,
//! `HeaderData.cpp`, `PollData.cpp` and `LX3JL/xlxd`
//! `cdcsprotocol.cpp`.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::error::Error;
use crate::header::DStarHeader;
use crate::types::{Callsign, Module, StreamId, Suffix};
use crate::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

use super::{ConnectionState, ReflectorEvent, format_hex_head};

/// Default `DCS` port.
pub const DEFAULT_PORT: u16 = 30051;

/// Keepalive interval.
///
/// DCS reflectors expect polls every 2 seconds — more frequent than
/// `DExtra` (3s) or `DPlus` (5s).
pub const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Connection timeout (no inbound traffic received).
///
/// Mirrors `ircDDBGateway`'s 30 s DPlus/DExtra poll inactivity timers.
/// Reset on every successfully parsed inbound packet (poll echo or
/// voice frame) in [`DcsClient::poll`], not just `PollEcho`. See the
/// matching comment on `dplus::POLL_TIMEOUT` for full rationale.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time to wait for a disconnect ACK before giving up and
/// forcing the client to the `Disconnected` state.
///
/// See `DExtra`'s `DISCONNECT_TIMEOUT` for rationale.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// "0001" magic bytes at the start of every voice packet.
const VOICE_MAGIC: &[u8; 4] = b"0001";

/// "EEEE" magic at the start of status data packets (ignored).
const STATUS_MAGIC: &[u8; 4] = b"EEEE";

/// HTML template for the 519-byte connect packet's client ID.
///
/// Per `ircDDBGateway` `ConnectData.cpp`. The reflector displays
/// this on its dashboard.
const CLIENT_HTML: &str = "<table border=\"0\" width=\"95%\"><tr>\
<td width=\"4%\"><img border=\"0\" src=dongle.jpg></td>\
<td width=\"96%\"><font size=\"2\"><b>DONGLE</b> dstar-gateway 0.1</font></td>\
</tr></table>";

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a `DCS` connect packet (519 bytes).
///
/// Per `ircDDBGateway` `ConnectData::getDCSData` `CT_LINK1`:
///
/// ```text
/// [0..8]    repeater callsign (space-padded, 7 chars + module at [8])
/// [8]       repeater module (last char of callsign)
/// [9]       reflector module
/// [10]      0x00
/// [11..19]  reflector callsign (7 chars, space-padded)
/// [19..519] HTML client ID (zero-padded to 500 bytes)
/// ```
///
/// # Panics
///
/// Cannot panic — `callsign` and `reflector` are validated 8-byte
/// types, the HTML payload is clamped to at most 500 bytes before
/// copying, and all slice writes use statically-known offsets within
/// the 519-byte buffer.
#[must_use]
pub fn build_connect(
    callsign: &Callsign,
    module: Module,
    reflector: &Callsign,
    refl_module: Module,
) -> Vec<u8> {
    let mut pkt = vec![b' '; 519];

    // Repeater callsign [0..8]: first 7 bytes of typed callsign,
    // module letter at [8].
    pkt[..7].copy_from_slice(&callsign.as_bytes()[..7]);
    pkt[8] = module.as_byte();
    // Reflector module at [9].
    pkt[9] = refl_module.as_byte();
    // Separator.
    pkt[10] = 0x00;
    // Reflector callsign [11..19]: first 7 bytes of typed callsign at
    // [11..18], the trailing byte at 18 is space filler (matches
    // ircDDBGateway which uses a 7-char reflector name + space).
    pkt[11..18].copy_from_slice(&reflector.as_bytes()[..7]);
    // HTML payload [19..519] (zero-padded).
    pkt[19..519].fill(0x00);
    let html = CLIENT_HTML.as_bytes();
    let html_len = html.len().min(500);
    pkt[19..19 + html_len].copy_from_slice(&html[..html_len]);

    pkt
}

/// Build a `DCS` disconnect packet (19 bytes).
///
/// Per `ircDDBGateway` `ConnectData::getDCSData` `CT_UNLINK`:
///
/// ```text
/// [0..8]    repeater callsign (space-padded)
/// [8]       repeater module
/// [9]       0x20 (space)
/// [10]      0x00
/// [11..19]  reflector callsign (space-padded)
/// ```
///
/// # Panics
///
/// Cannot panic — `callsign` and `reflector` are validated 8-byte
/// types and all slice writes use statically-known offsets within the
/// 19-byte buffer.
#[must_use]
pub fn build_disconnect(callsign: &Callsign, module: Module, reflector: &Callsign) -> [u8; 19] {
    let mut pkt = [b' '; 19];
    pkt[..7].copy_from_slice(&callsign.as_bytes()[..7]);
    pkt[8] = module.as_byte();
    pkt[9] = 0x20; // space = disconnect marker
    pkt[10] = 0x00;
    pkt[11..18].copy_from_slice(&reflector.as_bytes()[..7]);
    pkt
}

/// Build a `DCS` poll/keepalive packet (17 bytes).
///
/// Per `ircDDBGateway` `PollData::getDCSData` `DIR_OUTGOING`:
///
/// ```text
/// [0..8]   repeater callsign (space-padded)
/// [8]      0x00 (separator)
/// [9..17]  reflector callsign (space-padded)
/// ```
///
/// # Panics
///
/// Cannot panic — `callsign` and `reflector` are validated 8-byte
/// types and all slice writes use statically-known offsets within the
/// 17-byte buffer.
#[must_use]
pub fn build_poll(callsign: &Callsign, reflector: &Callsign) -> [u8; 17] {
    let mut pkt = [b' '; 17];
    pkt[..8].copy_from_slice(callsign.as_bytes());
    pkt[8] = 0x00;
    pkt[9..17].copy_from_slice(reflector.as_bytes());
    pkt
}

/// Build a `DCS` 100-byte voice packet.
///
/// Unlike DExtra/DPlus, DCS embeds the full D-STAR header in every
/// voice frame. This makes each packet self-describing at the cost
/// of higher bandwidth.
///
/// Per `ircDDBGateway` `AMBEData::getDCSData` + `HeaderData::getDCSData`
/// and `xlxd` `CDcsProtocol::EncodeDvPacket`.
///
/// # Panics
///
/// Cannot panic — all `header` callsign and suffix fields are
/// validated fixed-size byte arrays, `stream_id` is a validated
/// non-zero u16, `frame.ambe` and `frame.slow_data` are fixed-size
/// arrays, and every slice write uses statically-known offsets within
/// the 100-byte buffer.
#[must_use]
pub fn build_voice(
    header: &DStarHeader,
    stream_id: StreamId,
    seq: u8,
    rpt_seq: u32,
    frame: &VoiceFrame,
) -> Vec<u8> {
    let mut pkt = vec![0u8; 100];

    // [0..4] "0001" magic.
    pkt[..4].copy_from_slice(VOICE_MAGIC);

    // [4..7] flags.
    pkt[4] = header.flag1;
    pkt[5] = header.flag2;
    pkt[6] = header.flag3;

    // [7..15] RPT2.
    pkt[7..15].copy_from_slice(header.rpt2.as_bytes());
    // [15..23] RPT1.
    pkt[15..23].copy_from_slice(header.rpt1.as_bytes());
    // [23..31] YOUR.
    pkt[23..31].copy_from_slice(header.ur_call.as_bytes());
    // [31..39] MY.
    pkt[31..39].copy_from_slice(header.my_call.as_bytes());
    // [39..43] suffix.
    pkt[39..43].copy_from_slice(header.my_suffix.as_bytes());

    // [43..45] stream ID (little-endian).
    let sid = stream_id.get();
    pkt[43] = (sid & 0xFF) as u8;
    pkt[44] = (sid >> 8) as u8;

    // [45] sequence.
    pkt[45] = seq;

    // [46..55] AMBE voice data.
    pkt[46..55].copy_from_slice(&frame.ambe);
    // [55..58] slow data.
    pkt[55..58].copy_from_slice(&frame.slow_data);

    // [58..61] repeater sequence counter (24-bit LE).
    pkt[58] = (rpt_seq & 0xFF) as u8;
    pkt[59] = ((rpt_seq >> 8) & 0xFF) as u8;
    pkt[60] = ((rpt_seq >> 16) & 0xFF) as u8;

    // [61..63] fixed trailer.
    pkt[61] = 0x01;
    pkt[62] = 0x00;

    // [63] fixed.
    pkt[63] = 0x21;

    // [64..100] text/padding — already zeroed.
    pkt
}

/// Build a `DCS` end-of-transmission packet (100 bytes).
///
/// Per reference: EOT has seq bit 6 set (0x40), AMBE silence bytes,
/// and sync slow data bytes (`[0x55, 0x55, 0x55]`).
///
/// # Panics
///
/// Cannot panic — delegates to [`build_voice`] which itself cannot
/// panic for validated inputs.
#[must_use]
pub fn build_eot(header: &DStarHeader, stream_id: StreamId, seq: u8, rpt_seq: u32) -> Vec<u8> {
    let frame = VoiceFrame {
        ambe: AMBE_SILENCE,
        slow_data: DSTAR_SYNC_BYTES,
    };
    build_voice(header, stream_id, seq | 0x40, rpt_seq, &frame)
}

// ---------------------------------------------------------------------------
// Packet parser
// ---------------------------------------------------------------------------

/// Parse an incoming `DCS` packet into a [`ReflectorEvent`].
///
/// Handles all packet types from the `DCS` protocol:
/// - 100 bytes starting with "0001": voice/EOT
/// - 14 bytes with "ACK"/"NAK": connect response
/// - 519 bytes: connect request (echoed back as ACK context)
/// - 19 bytes: disconnect
/// - 17/22 bytes: poll echo
/// - "EEEE" prefix: status data (ignored)
///
/// Returns `None` for unrecognized or irrelevant packets.
#[must_use]
pub fn parse_packet(data: &[u8]) -> Option<ReflectorEvent> {
    // Voice/EOT: 100 bytes starting with "0001".
    // Per ircDDBGateway DCSProtocolHandler::readPackets() and
    // xlxd CDcsProtocol::IsValidDvPacket().
    if data.len() >= 100 && &data[..4] == VOICE_MAGIC {
        // `parse_voice` returns `None` for packets with `stream_id == 0`,
        // which are malformed per the D-STAR spec.
        return parse_voice(data);
    }

    // Status data ("EEEE" prefix) — ignored per ircDDBGateway.
    if data.len() >= 4 && &data[..4] == STATUS_MAGIC {
        return None;
    }

    // 11-byte shortform disconnect (DExtra-style) per
    // xlxd/src/cdcsprotocol.cpp:392-398:
    //   [callsign(8), module, 0x20, 0x00]
    if data.len() == 11 && data[9] == 0x20 && data[10] == 0x00 {
        return Some(ReflectorEvent::Disconnected);
    }

    // 15-byte packets are explicitly ignored per
    // xlxd/src/cdcsprotocol.cpp:408-417 IsIgnorePacket. These are
    // reflector status pings with all-zero payloads that carry no
    // actionable information.
    if data.len() == 15 {
        return None;
    }

    // Non-voice packets — dispatch by length.
    // Per ircDDBGateway DCSProtocolHandler::readPackets().
    match data.len() {
        // ACK/NAK: 14 bytes.
        // Per ircDDBGateway ConnectData::setDCSData case 14U and
        // xlxd CDcsProtocol::EncodeConnectAckPacket.
        //
        // Layout: callsign[7] + 0x20 + module + refl_module + "ACK\0" or "NAK\0".
        // Offset 10 = 'A' for ACK, 'N' for NAK.
        14 => {
            if data.len() >= 13 && data[10] == b'A' && data[11] == b'C' && data[12] == b'K' {
                Some(ReflectorEvent::Connected)
            } else if data.len() >= 13 && data[10] == b'N' && data[11] == b'A' && data[12] == b'K' {
                Some(ReflectorEvent::Rejected)
            } else {
                None
            }
        }
        // Disconnect: 19 bytes.
        // Per xlxd CDcsProtocol::IsValidDisconnectPacket (19 bytes, [9]=0x20, [10]=0x00).
        19 => Some(ReflectorEvent::Disconnected),
        // Connect (echoed): 519 bytes.
        // Per ircDDBGateway DCSProtocolHandler::readPackets() case 519U → DC_CONNECT.
        519 => Some(ReflectorEvent::Connected),
        // Poll echo: 17 or 22 bytes.
        // Per ircDDBGateway DCSProtocolHandler::readPackets() and
        // xlxd CDcsProtocol::IsValidKeepAlivePacket.
        17 | 22 => Some(ReflectorEvent::PollEcho),
        // 35 bytes = status data per ircDDBGateway (ignored).
        // All other lengths are unrecognized.
        _ => None,
    }
}

/// Parse a 100-byte DCS voice packet into a [`ReflectorEvent`].
///
/// Returns `VoiceEnd` for EOT frames (seq bit 6 set) and `VoiceData`
/// for all other frames. New-stream detection (`VoiceStart`) is
/// performed statefully by [`DcsClient::poll`] — `parse_voice` is a
/// stateless pure function that does not look at prior frames.
///
/// Returns `None` when the packet carries `stream_id == 0`, which is
/// reserved per the D-STAR spec and therefore malformed.
fn parse_voice(data: &[u8]) -> Option<ReflectorEvent> {
    // Stream ID at [43..45] (little-endian). Stream ID 0 is reserved
    // per the D-STAR spec — drop via `?` on `StreamId::new`.
    let stream_id = StreamId::new(u16::from(data[43]) | (u16::from(data[44]) << 8))?;

    // Sequence at [45].
    let seq = data[45];

    // EOT: bit 6 set.
    if seq & 0x40 != 0 {
        return Some(ReflectorEvent::VoiceEnd { stream_id });
    }

    // AMBE data at [46..55].
    let mut ambe = [0u8; 9];
    ambe.copy_from_slice(&data[46..55]);
    // Slow data at [55..58].
    let mut slow_data = [0u8; 3];
    slow_data.copy_from_slice(&data[55..58]);

    Some(ReflectorEvent::VoiceData {
        stream_id,
        seq,
        frame: VoiceFrame { ambe, slow_data },
    })
}

/// Extract the embedded [`DStarHeader`] from a 100-byte DCS voice packet.
///
/// DCS voice packets do not carry a CRC-protected 41-byte header block;
/// instead the individual header fields are scattered across offsets
/// 4..43 of the 100-byte frame. This helper reconstructs a
/// [`DStarHeader`] from those fields for the stream-start event.
///
/// Returns `None` if the packet is too short or does not start with
/// the `"0001"` magic.
fn extract_dcs_header(data: &[u8]) -> Option<DStarHeader> {
    if data.len() < 100 || &data[..4] != VOICE_MAGIC {
        return None;
    }
    let mut rpt2_bytes = [0u8; 8];
    rpt2_bytes.copy_from_slice(&data[7..15]);
    let mut rpt1_bytes = [0u8; 8];
    rpt1_bytes.copy_from_slice(&data[15..23]);
    let mut ur_bytes = [0u8; 8];
    ur_bytes.copy_from_slice(&data[23..31]);
    let mut my_bytes = [0u8; 8];
    my_bytes.copy_from_slice(&data[31..39]);
    let mut suffix_bytes = [0u8; 4];
    suffix_bytes.copy_from_slice(&data[39..43]);

    // Lenient wire-byte storage — no ASCII validation. Mirrors
    // `CHeaderData::setDPlusData` in ircDDBGateway which does raw
    // memcpy from the socket buffer into its callsign fields.
    Some(DStarHeader {
        flag1: data[4],
        flag2: data[5],
        flag3: data[6],
        rpt2: Callsign::from_wire_bytes(rpt2_bytes),
        rpt1: Callsign::from_wire_bytes(rpt1_bytes),
        ur_call: Callsign::from_wire_bytes(ur_bytes),
        my_call: Callsign::from_wire_bytes(my_bytes),
        my_suffix: Suffix::from_wire_bytes(suffix_bytes),
    })
}

// ---------------------------------------------------------------------------
// Async client
// ---------------------------------------------------------------------------

/// Async `DCS` reflector client.
///
/// Manages a UDP connection to a DCS reflector with automatic
/// keepalives. Unlike `DExtra` and `DPlus`, `DCS` requires the
/// reflector callsign for polls and disconnect, so it is stored
/// alongside the user's callsign.
///
/// # Voice framing
///
/// DCS embeds the full D-STAR header in every voice packet (100 bytes),
/// making each frame self-describing. The `rpt_seq` counter must be
/// incremented for every frame sent — it is tracked automatically by
/// [`send_voice`](Self::send_voice) and [`send_eot`](Self::send_eot).
///
/// For most users the unified [`crate::ReflectorClient`] is easier;
/// drop to this type when you need per-protocol control over a DCS
/// reflector on UDP port 30051.
///
/// # Example
///
/// ```no_run
/// use dstar_gateway::protocol::dcs::DcsClient;
/// use dstar_gateway::{Callsign, Module};
/// use std::time::Duration;
///
/// # async fn example() -> Result<(), dstar_gateway::Error> {
/// let mut client = DcsClient::new(
///     Callsign::try_from_str("W1AW")?,
///     Module::try_from_char('B')?, // local module
///     Callsign::try_from_str("DCS001")?, // reflector callsign
///     Module::try_from_char('C')?, // reflector module
///     "1.2.3.4:30051".parse().unwrap(),
/// )
/// .await?;
/// client.connect_and_wait(Duration::from_secs(5)).await?;
/// // ... send voice via send_header + send_voice + send_eot ...
/// client.disconnect().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct DcsClient {
    socket: UdpSocket,
    remote: SocketAddr,
    callsign: Callsign,
    reflector: Callsign,
    module: Module,
    refl_module: Module,
    state: ConnectionState,
    last_poll_sent: Instant,
    last_poll_received: Instant,
    poll_interval: Duration,
    /// Monotonically increasing sequence counter for outgoing voice.
    rpt_seq: u32,
    /// Most recently seen incoming stream ID. Used to synthesize a
    /// `VoiceStart` event only on stream-ID change. Cleared when a
    /// `VoiceEnd` arrives. Without this, every seq-0 frame (once per
    /// ~420ms superframe) would be classified as a new stream (C8).
    last_rx_stream_id: Option<StreamId>,
    /// Events stashed by `poll` when a new stream is detected: the
    /// accompanying `VoiceData` is queued here and returned on the
    /// next `poll` call so the caller sees both `VoiceStart` and the
    /// first `VoiceData` frame without losing either.
    ///
    /// In practice the queue holds at most one element (the deferred
    /// `VoiceData` that followed a synthesized `VoiceStart`). Using a
    /// `VecDeque` instead of `Option` makes the shape obviously
    /// correct and guards against future refactors that might want to
    /// queue more than one event without redesigning the slot.
    pending_events: VecDeque<ReflectorEvent>,
    /// Cached TX header set by [`send_header`](Self::send_header).
    ///
    /// DCS embeds the full D-STAR header in every voice frame, so the
    /// client must remember the header across [`send_voice`](Self::send_voice)
    /// and [`send_eot`](Self::send_eot) calls. This slot is populated by
    /// `send_header` and read by the subsequent voice/eot sends.
    tx_header: Option<DStarHeader>,
    /// Timestamp of the most recent `disconnect()` call. See the
    /// corresponding field on `DExtraClient` for rationale.
    disconnect_sent_at: Option<Instant>,
}

impl DcsClient {
    /// Create a new client and bind a local UDP socket.
    ///
    /// # Arguments
    ///
    /// * `callsign` — Your station callsign (e.g. `"W1AW"`).
    /// * `module` — Your module letter (e.g. `'B'`).
    /// * `reflector` — Reflector callsign (e.g. `"DCS001"`).
    /// * `refl_module` — Reflector module to link to (e.g. `'C'`).
    /// * `remote` — Reflector socket address (IP + port 30051).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the socket cannot be bound.
    pub async fn new(
        callsign: Callsign,
        local_module: Module,
        reflector: Callsign,
        refl_module: Module,
        remote: SocketAddr,
    ) -> Result<Self, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let now = Instant::now();
        Ok(Self {
            socket,
            remote,
            callsign,
            reflector,
            module: local_module,
            refl_module,
            state: ConnectionState::Disconnected,
            last_poll_sent: now,
            last_poll_received: now,
            poll_interval: POLL_INTERVAL,
            rpt_seq: 0,
            last_rx_stream_id: None,
            pending_events: VecDeque::new(),
            tx_header: None,
            disconnect_sent_at: None,
        })
    }

    /// Override the keepalive poll interval.
    ///
    /// Defaults to [`POLL_INTERVAL`] (2 seconds). Decrease this for
    /// links traversing NAT where connection-tracking timers drop idle
    /// flows faster than the default keepalive cadence.
    pub const fn set_poll_interval(&mut self, interval: Duration) {
        self.poll_interval = interval;
    }

    /// Station callsign this client was constructed with.
    #[must_use]
    pub const fn callsign(&self) -> &Callsign {
        &self.callsign
    }

    /// Reflector callsign this client was constructed with.
    #[must_use]
    pub const fn reflector_callsign(&self) -> &Callsign {
        &self.reflector
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

    /// Send the connect request to the reflector.
    ///
    /// DCS uses a 519-byte connect packet containing an HTML client
    /// ID that the reflector displays on its dashboard.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn connect(&mut self) -> Result<(), Error> {
        let pkt = build_connect(
            &self.callsign,
            self.module,
            &self.reflector,
            self.refl_module,
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Connecting;
        tracing::debug!(
            target: "dstar_gateway::dcs",
            state = "Connecting",
            reflector = %self.remote,
            module = %self.refl_module,
            "DCS state -> Connecting"
        );
        tracing::info!(
            reflector = %self.remote,
            module = %self.refl_module,
            "DCS connect sent"
        );
        Ok(())
    }

    /// Connect to the reflector and wait for the connection ACK or timeout.
    ///
    /// Drives the state machine internally: sends the 519-byte connect
    /// packet, then polls until state is `Connected` or the timeout
    /// expires. Use this as a more convenient alternative to calling
    /// [`connect`](Self::connect) and [`poll`](Self::poll) in a loop.
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
        let pkt = build_disconnect(&self.callsign, self.module, &self.reflector);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Disconnecting;
        self.disconnect_sent_at = Some(Instant::now());
        tracing::debug!(
            target: "dstar_gateway::dcs",
            state = "Disconnecting",
            "DCS state -> Disconnecting"
        );
        tracing::info!("DCS disconnect sent");
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
        // Drain any event stashed by the previous poll (the VoiceData
        // that accompanied a synthesized VoiceStart). The invariant is
        // that this queue holds at most one element — the `debug_assert!`
        // guards against future refactors breaking that.
        debug_assert!(
            self.pending_events.len() <= 1,
            "pending_events queue should always hold 0 or 1 events"
        );
        if let Some(ev) = self.pending_events.pop_front() {
            return Ok(Some(ev));
        }

        // Force Disconnecting -> Disconnected after DISCONNECT_TIMEOUT
        // if the reflector never ACKs the unlink.
        if self.state == ConnectionState::Disconnecting
            && let Some(sent) = self.disconnect_sent_at
            && sent.elapsed() >= DISCONNECT_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            self.disconnect_sent_at = None;
            tracing::debug!(
                target: "dstar_gateway::dcs",
                state = "Disconnected",
                reason = "disconnect_timeout",
                "DCS state -> Disconnected (unlink ACK never arrived)"
            );
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Send keepalive if connected and interval elapsed.
        if self.state == ConnectionState::Connected
            && self.last_poll_sent.elapsed() >= self.poll_interval
        {
            let pkt = build_poll(&self.callsign, &self.reflector);
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            self.last_poll_sent = Instant::now();
            tracing::trace!(
                target: "dstar_gateway::dcs",
                len = pkt.len(),
                head = %format_hex_head(&pkt),
                "DCS tx keepalive"
            );
        }

        // Check for connection timeout.
        if self.state == ConnectionState::Connected
            && self.last_poll_received.elapsed() >= POLL_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            tracing::debug!(
                target: "dstar_gateway::dcs",
                state = "Disconnected",
                reason = "poll_timeout",
                "DCS state -> Disconnected (keepalive timeout)"
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
            target: "dstar_gateway::dcs",
            len,
            head = %format_hex_head(&buf[..len]),
            "DCS rx"
        );

        let Some(event) = parse_packet(&buf[..len]) else {
            return Ok(None);
        };

        // Any successfully parsed inbound packet proves the link is
        // alive, so reset the keepalive clock unconditionally. See
        // the matching fix in `DPlusClient::poll` for rationale —
        // the previous per-arm reset only fired on `PollEcho`, which
        // let long voice bursts silently trip the `POLL_TIMEOUT`
        // guard and drop the connection mid-transmission.
        self.last_poll_received = Instant::now();

        // Update state based on event.
        match event {
            ReflectorEvent::Connected => {
                self.state = ConnectionState::Connected;
                tracing::debug!(
                    target: "dstar_gateway::dcs",
                    state = "Connected",
                    "DCS state -> Connected"
                );
                Ok(Some(ReflectorEvent::Connected))
            }
            ReflectorEvent::Rejected => {
                self.state = ConnectionState::Disconnected;
                self.disconnect_sent_at = None;
                tracing::debug!(
                    target: "dstar_gateway::dcs",
                    state = "Disconnected",
                    reason = "rejected",
                    "DCS state -> Disconnected (reflector rejected)"
                );
                Ok(Some(ReflectorEvent::Rejected))
            }
            ReflectorEvent::Disconnected => {
                self.state = ConnectionState::Disconnected;
                self.disconnect_sent_at = None;
                tracing::debug!(
                    target: "dstar_gateway::dcs",
                    state = "Disconnected",
                    reason = "reflector_reply",
                    "DCS state -> Disconnected (reflector closed)"
                );
                Ok(Some(ReflectorEvent::Disconnected))
            }
            ReflectorEvent::PollEcho => {
                tracing::trace!(
                    target: "dstar_gateway::dcs",
                    "DCS poll echo"
                );
                Ok(Some(ReflectorEvent::PollEcho))
            }
            ReflectorEvent::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                // Synthesize a VoiceStart on stream-ID change.
                // The accompanying VoiceData is stashed and returned
                // on the next poll so the caller sees both events.
                let new_stream = self.last_rx_stream_id != Some(stream_id);
                self.last_rx_stream_id = Some(stream_id);
                if new_stream && let Some(header) = extract_dcs_header(&buf[..len]) {
                    tracing::debug!(
                        target: "dstar_gateway::dcs",
                        stream_id = %stream_id,
                        "DCS voice header rx (synthesized)"
                    );
                    self.pending_events.push_back(ReflectorEvent::VoiceData {
                        stream_id,
                        seq,
                        frame,
                    });
                    return Ok(Some(ReflectorEvent::VoiceStart { header, stream_id }));
                }
                tracing::trace!(
                    target: "dstar_gateway::dcs",
                    stream_id = %stream_id,
                    seq,
                    "DCS voice data rx"
                );
                Ok(Some(ReflectorEvent::VoiceData {
                    stream_id,
                    seq,
                    frame,
                }))
            }
            ReflectorEvent::VoiceEnd { stream_id } => {
                if self.last_rx_stream_id == Some(stream_id) {
                    self.last_rx_stream_id = None;
                }
                tracing::debug!(
                    target: "dstar_gateway::dcs",
                    stream_id = %stream_id,
                    "DCS voice EOT rx"
                );
                Ok(Some(ReflectorEvent::VoiceEnd { stream_id }))
            }
            vs @ ReflectorEvent::VoiceStart { .. } => {
                // parse_packet no longer emits VoiceStart; this arm
                // exists only to keep the match exhaustive. Forward
                // unchanged if somehow encountered.
                Ok(Some(vs))
            }
        }
    }

    /// Send a voice header (as the first frame of a stream).
    ///
    /// In DCS, there is no separate header packet — the header is
    /// embedded in every voice frame. This sends a frame with
    /// sequence 0 to start a new stream and caches `header` for the
    /// subsequent [`send_voice`](Self::send_voice) and
    /// [`send_eot`](Self::send_eot) calls.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_header(
        &mut self,
        header: &DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        let frame = VoiceFrame::silence();
        let pkt = build_voice(header, stream_id, 0, self.rpt_seq, &frame);
        tracing::debug!(
            target: "dstar_gateway::dcs",
            stream_id = %stream_id,
            "DCS voice header tx"
        );
        tracing::trace!(
            target: "dstar_gateway::dcs",
            len = pkt.len(),
            head = %format_hex_head(&pkt),
            "DCS tx header"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.rpt_seq = self.rpt_seq.wrapping_add(1);
        self.tx_header = Some(*header);
        Ok(())
    }

    /// Send a voice data frame to the reflector.
    ///
    /// Uses the header cached by the most recent
    /// [`send_header`](Self::send_header) call. The `rpt_seq` counter
    /// is incremented automatically for each frame sent.
    ///
    /// # Errors
    ///
    /// - [`Error::NoTxHeader`] if [`send_header`](Self::send_header) has
    ///   not been called yet on this client.
    /// - [`Error::Io`] if the UDP send fails.
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        let header = self.tx_header.as_ref().ok_or(Error::NoTxHeader)?;
        let pkt = build_voice(header, stream_id, seq, self.rpt_seq, frame);
        tracing::trace!(
            target: "dstar_gateway::dcs",
            stream_id = %stream_id,
            seq,
            len = pkt.len(),
            "DCS tx voice"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.rpt_seq = self.rpt_seq.wrapping_add(1);
        Ok(())
    }

    /// Send an end-of-transmission to the reflector.
    ///
    /// Uses the header cached by the most recent
    /// [`send_header`](Self::send_header) call.
    ///
    /// # Errors
    ///
    /// - [`Error::NoTxHeader`] if [`send_header`](Self::send_header) has
    ///   not been called yet on this client.
    /// - [`Error::Io`] if the UDP send fails.
    pub async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), Error> {
        let header = self.tx_header.as_ref().ok_or(Error::NoTxHeader)?;
        let pkt = build_eot(header, stream_id, seq, self.rpt_seq);
        tracing::debug!(
            target: "dstar_gateway::dcs",
            stream_id = %stream_id,
            seq,
            "DCS voice EOT tx"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.rpt_seq = self.rpt_seq.wrapping_add(1);
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

    fn cs(s: &str) -> Callsign {
        Callsign::try_from_str(s).expect("valid test callsign")
    }

    fn m(c: char) -> Module {
        Module::try_from_char(c).expect("valid test module")
    }

    fn sid(n: u16) -> StreamId {
        StreamId::new(n).expect("non-zero test stream id")
    }

    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("DCS001 G"),
            rpt1: cs("DCS001 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        }
    }

    #[test]
    fn connect_packet_size() {
        let pkt = build_connect(&cs("W1AW"), m('B'), &cs("DCS001"), m('C'));
        assert_eq!(pkt.len(), 519);
        // Repeater callsign.
        assert_eq!(&pkt[..4], b"W1AW");
        // Module at [8].
        assert_eq!(pkt[8], b'B');
        // Reflector module at [9].
        assert_eq!(pkt[9], b'C');
        // Separator.
        assert_eq!(pkt[10], 0x00);
        // Reflector callsign at [11..17].
        assert_eq!(&pkt[11..17], b"DCS001");
    }

    #[test]
    fn connect_packet_contains_html() {
        let pkt = build_connect(&cs("W1AW"), m('B'), &cs("DCS001"), m('C'));
        let html_region = &pkt[19..519];
        // Should contain the HTML template.
        let html_str = std::str::from_utf8(html_region).unwrap();
        assert!(html_str.contains("dstar-gateway"));
        assert!(html_str.contains("DONGLE"));
    }

    #[test]
    fn disconnect_packet_format() {
        let pkt = build_disconnect(&cs("W1AW"), m('B'), &cs("DCS001"));
        assert_eq!(pkt.len(), 19);
        assert_eq!(&pkt[..4], b"W1AW");
        assert_eq!(pkt[8], b'B');
        assert_eq!(pkt[9], 0x20); // space = disconnect
        assert_eq!(pkt[10], 0x00);
        assert_eq!(&pkt[11..17], b"DCS001");
    }

    #[test]
    fn poll_packet_format() {
        let pkt = build_poll(&cs("W1AW"), &cs("DCS001"));
        assert_eq!(pkt.len(), 17);
        assert_eq!(&pkt[..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
        assert_eq!(&pkt[9..17], b"DCS001  ");
    }

    #[test]
    fn voice_packet_layout() {
        let hdr = test_header();
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let pkt = build_voice(&hdr, sid(0xABCD), 5, 42, &frame);
        assert_eq!(pkt.len(), 100);
        // Magic.
        assert_eq!(&pkt[..4], b"0001");
        // Flags.
        assert_eq!(pkt[4], 0);
        assert_eq!(pkt[5], 0);
        assert_eq!(pkt[6], 0);
        // RPT2.
        assert_eq!(&pkt[7..15], b"DCS001 G");
        // RPT1.
        assert_eq!(&pkt[15..23], b"DCS001 C");
        // YOUR.
        assert_eq!(&pkt[23..31], b"CQCQCQ  ");
        // MY.
        assert_eq!(&pkt[31..39], b"W1AW    ");
        // Suffix.
        assert_eq!(&pkt[39..43], b"    ");
        // Stream ID (LE).
        assert_eq!(pkt[43], 0xCD);
        assert_eq!(pkt[44], 0xAB);
        // Sequence.
        assert_eq!(pkt[45], 5);
        // AMBE.
        assert_eq!(&pkt[46..55], &[0x11; 9]);
        // Slow data.
        assert_eq!(&pkt[55..58], &[0x22; 3]);
        // Rpt seq (42 = 0x2A LE).
        assert_eq!(pkt[58], 0x2A);
        assert_eq!(pkt[59], 0x00);
        assert_eq!(pkt[60], 0x00);
        // Fixed trailer.
        assert_eq!(pkt[61], 0x01);
        assert_eq!(pkt[62], 0x00);
        assert_eq!(pkt[63], 0x21);
    }

    #[test]
    fn eot_has_flag_set() {
        let hdr = test_header();
        let pkt = build_eot(&hdr, sid(0x1234), 3, 100);
        assert_eq!(pkt.len(), 100);
        // Seq has bit 6 set.
        assert_eq!(pkt[45] & 0x40, 0x40);
        // AMBE silence.
        assert_eq!(&pkt[46..55], &AMBE_SILENCE);
        // Sync slow data.
        assert_eq!(&pkt[55..58], &DSTAR_SYNC_BYTES);
    }

    #[test]
    fn parse_ack() {
        // Simulate a 14-byte ACK from the reflector.
        // Per xlxd EncodeConnectAckPacket:
        // cs[7] + ' ' + module + refl_module + "ACK" + 0x00
        let mut pkt = vec![b' '; 14];
        pkt[..4].copy_from_slice(b"W1AW");
        pkt[7] = b' ';
        pkt[8] = b'B'; // module
        pkt[9] = b'C'; // refl module
        pkt[10] = b'A';
        pkt[11] = b'C';
        pkt[12] = b'K';
        pkt[13] = 0x00;
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_poll_echo_via_builder() {
        // Independent sanity check that the typed poll builder produces
        // a packet the parser classifies as a PollEcho.
        let pkt = build_poll(&cs("W1AW"), &cs("DCS001"));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
    }

    #[test]
    fn parse_nak() {
        let mut pkt = vec![b' '; 14];
        pkt[10] = b'N';
        pkt[11] = b'A';
        pkt[12] = b'K';
        pkt[13] = 0x00;
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Rejected));
    }

    #[test]
    fn parse_poll_echo_17() {
        let pkt = build_poll(&cs("W1AW"), &cs("DCS001"));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
    }

    #[test]
    fn parse_poll_echo_22() {
        // 22-byte poll response from reflector (per ircDDBGateway).
        let pkt = vec![b' '; 22];
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
    }

    #[test]
    fn parse_disconnect_19() {
        let pkt = build_disconnect(&cs("W1AW"), m('B'), &cs("DCS001"));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Disconnected));
    }

    #[test]
    fn parse_11_byte_shortform_disconnect() {
        // Per xlxd/src/cdcsprotocol.cpp:392-398, DCS accepts an 11-byte
        // DExtra-style disconnect:
        //   [callsign(8), module, 0x20, 0x00]
        let mut data = Vec::with_capacity(11);
        data.extend_from_slice(b"W1AW    "); // 8-byte callsign
        data.push(b'A'); // module
        data.push(0x20); // space sentinel
        data.push(0x00); // null terminator
        assert_eq!(data.len(), 11);

        let evt = parse_packet(&data).expect("should be recognized as disconnect");
        assert!(
            matches!(evt, ReflectorEvent::Disconnected),
            "11-byte shortform disconnect should parse as Disconnected, got {evt:?}"
        );
    }

    #[test]
    fn parse_15_byte_packet_returns_none() {
        // Per xlxd/src/cdcsprotocol.cpp:408-417 IsIgnorePacket, 15-byte
        // DCS packets are reflector status pings that should be
        // ignored (no event).
        let data = [0u8; 15];
        assert!(
            parse_packet(&data).is_none(),
            "15-byte DCS packets are ignored per xlxd IsIgnorePacket"
        );
    }

    #[test]
    fn voice_roundtrip() {
        let hdr = test_header();
        let frame = VoiceFrame {
            ambe: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99],
            slow_data: [0xAA, 0xBB, 0xCC],
        };
        let pkt = build_voice(&hdr, sid(0x5678), 7, 0, &frame);
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
    fn parse_voice_seq_zero_is_voice_data() {
        // After C8 fix: parse_packet is stateless and always emits
        // VoiceData for non-EOT frames. VoiceStart synthesis is the
        // job of DcsClient::poll (tracked via last_rx_stream_id).
        let hdr = test_header();
        let frame = VoiceFrame::silence();
        let pkt = build_voice(&hdr, sid(0xABCD), 0, 0, &frame);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceData { stream_id, seq, .. } => {
                assert_eq!(stream_id.get(), 0xABCD);
                assert_eq!(seq, 0);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn extract_dcs_header_roundtrip() {
        let hdr = DStarHeader {
            flag1: 0x11,
            flag2: 0x22,
            flag3: 0x33,
            rpt2: cs("DCS001 G"),
            rpt1: cs("DCS001 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::try_from_str("TEST").unwrap(),
        };
        let frame = VoiceFrame::silence();
        let pkt = build_voice(&hdr, sid(0x1234), 0, 0, &frame);
        let extracted = extract_dcs_header(&pkt).expect("must extract");
        assert_eq!(extracted, hdr);
        // Rejects short or non-magic buffers.
        assert!(extract_dcs_header(&pkt[..99]).is_none());
        let mut bad = build_voice(&hdr, sid(0x1234), 0, 0, &frame);
        bad[0] = b'X';
        assert!(extract_dcs_header(&bad).is_none());
    }

    #[test]
    fn eot_roundtrip() {
        let hdr = test_header();
        let pkt = build_eot(&hdr, sid(0x5678), 3, 0);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceEnd { stream_id } => {
                assert_eq!(stream_id.get(), 0x5678);
            }
            other => panic!("expected VoiceEnd, got {other:?}"),
        }
    }

    #[test]
    fn status_data_ignored() {
        let mut pkt = vec![0u8; 50];
        pkt[..4].copy_from_slice(b"EEEE");
        assert!(parse_packet(&pkt).is_none());
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_packet(&[]).is_none());
        assert!(parse_packet(&[0xFF; 5]).is_none());
    }

    #[tokio::test]
    async fn send_header_increments_rpt_seq() {
        // Regression test for C7: rpt_seq must advance after send_header
        // so the first send_voice uses a distinct value. Previously both
        // shared the header's rpt_seq, confusing the reflector.
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = listener.local_addr().unwrap();

        let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), remote)
            .await
            .unwrap();
        client.state = ConnectionState::Connected;
        let initial_rpt = client.rpt_seq;

        let hdr = test_header();
        client.send_header(&hdr, sid(0x1234)).await.unwrap();
        assert_eq!(
            client.rpt_seq,
            initial_rpt.wrapping_add(1),
            "send_header must increment rpt_seq by 1"
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // multi-stream flow test is intrinsically long
    async fn dcs_voice_start_fires_once_per_new_stream() {
        // Companion to `dcs_emits_voice_start_only_once_per_stream`.
        //
        // That test proves: same stream_id across multiple seq=0
        // frames → exactly one VoiceStart.
        //
        // This test proves the other half of C8: when the stream_id
        // actually changes, the client DOES synthesize a fresh
        // VoiceStart for the new stream. The stateful
        // `last_rx_stream_id` tracker must update on every frame so
        // the first frame of every new stream is classified as a
        // stream boundary.
        //
        // Regression scenario without the fix: either (a) every seq=0
        // frame is a VoiceStart (spurious events — the bug we fixed in
        // C8), or (b) the tracker over-corrects and swallows legitimate
        // stream transitions. This test guards against (b).
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), server_addr)
            .await
            .unwrap();
        client.socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.socket.local_addr().unwrap();
        client.state = ConnectionState::Connected;

        let hdr = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("DCS001 G"),
            rpt1: cs("DCS001 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("N0CALL"),
            my_suffix: Suffix::EMPTY,
        };
        let frame = VoiceFrame::silence();

        // Stream alpha: 3 frames with the same stream_id. Expect one
        // VoiceStart followed by VoiceData frames.
        let first_stream_seq0 = build_voice(&hdr, sid(0x1234), 0, 0, &frame);
        let first_stream_seq1 = build_voice(&hdr, sid(0x1234), 1, 1, &frame);
        let first_stream_seq2 = build_voice(&hdr, sid(0x1234), 2, 2, &frame);

        let _ = server
            .send_to(&first_stream_seq0, client_addr)
            .await
            .unwrap();
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceStart { stream_id, .. }) => {
                assert_eq!(
                    stream_id.get(),
                    0x1234,
                    "first stream frame 1 must be VoiceStart(0x1234)"
                );
            }
            other => panic!("first stream frame 1 must be VoiceStart(0x1234), got {other:?}"),
        }
        // Drain the stashed VoiceData.
        let evt = client.poll().await.unwrap();
        assert!(matches!(evt, Some(ReflectorEvent::VoiceData { .. })));

        let _ = server
            .send_to(&first_stream_seq1, client_addr)
            .await
            .unwrap();
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceData { stream_id, .. }) => {
                assert_eq!(stream_id.get(), 0x1234);
            }
            other => panic!("first stream frame 2 must stay VoiceData, got {other:?}"),
        }

        let _ = server
            .send_to(&first_stream_seq2, client_addr)
            .await
            .unwrap();
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceData { stream_id, .. }) => {
                assert_eq!(stream_id.get(), 0x1234);
            }
            other => panic!("first stream frame 3 must stay VoiceData, got {other:?}"),
        }

        // Stream beta: different stream_id → must produce a new
        // VoiceStart exactly once.
        let new_stream_seq0 = build_voice(&hdr, sid(0x5678), 0, 0, &frame);
        let new_stream_seq1 = build_voice(&hdr, sid(0x5678), 1, 1, &frame);

        let _ = server.send_to(&new_stream_seq0, client_addr).await.unwrap();
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceStart { stream_id, .. }) => {
                assert_eq!(stream_id.get(), 0x5678);
            }
            other => {
                panic!("new stream frame 1 must fire a fresh VoiceStart(0x5678), got {other:?}")
            }
        }
        // Drain the stashed VoiceData for the new stream.
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceData { stream_id, .. }) => {
                assert_eq!(stream_id.get(), 0x5678);
            }
            other => panic!("expected stashed VoiceData(0x5678), got {other:?}"),
        }

        // And the second frame of the new stream must not re-fire.
        let _ = server.send_to(&new_stream_seq1, client_addr).await.unwrap();
        let evt = client.poll().await.unwrap();
        match &evt {
            Some(ReflectorEvent::VoiceData { stream_id, .. }) => {
                assert_eq!(stream_id.get(), 0x5678);
            }
            other => panic!("new stream frame 2 must stay VoiceData, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dcs_emits_voice_start_only_once_per_stream() {
        // Regression test for C8: parse_packet used to emit VoiceStart
        // for every seq==0 frame. Since DCS cycles seq 0..20 every
        // superframe (~420ms), this produced a spurious VoiceStart
        // every ~420ms during an active stream. The fix moves
        // new-stream detection into DcsClient::poll using a
        // last_rx_stream_id tracker.
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();

        let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), server_addr)
            .await
            .unwrap();
        // `DcsClient::new` binds 0.0.0.0 which doesn't route; rebind
        // to loopback for this test so `server.send_to(client_addr)` works.
        client.socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.socket.local_addr().unwrap();
        client.state = ConnectionState::Connected;

        let hdr = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("DCS001 G"),
            rpt1: cs("DCS001 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("N0CALL"),
            my_suffix: Suffix::EMPTY,
        };
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        // Build a DCS voice packet with stream_id = 0xCAFE, seq = 0.
        let pkt0 = build_voice(&hdr, sid(0xCAFE), 0, 0, &frame);

        // First frame of a new stream_id → VoiceStart.
        let _ = server.send_to(&pkt0, client_addr).await.unwrap();
        let evt1 = client.poll().await.unwrap();
        assert!(
            matches!(evt1, Some(ReflectorEvent::VoiceStart { .. })),
            "first frame of new stream must be VoiceStart, got {evt1:?}"
        );

        // Drain the pending VoiceData queued alongside the VoiceStart.
        let evt1b = client.poll().await.unwrap();
        assert!(
            matches!(evt1b, Some(ReflectorEvent::VoiceData { .. })),
            "pending VoiceData must be drained on next poll, got {evt1b:?}"
        );

        // Second seq=0 frame with the same stream_id (next superframe).
        let _ = server.send_to(&pkt0, client_addr).await.unwrap();
        let evt2 = client.poll().await.unwrap();
        match evt2 {
            Some(ReflectorEvent::VoiceData { .. }) => {}
            Some(ReflectorEvent::VoiceStart { .. }) => {
                panic!("expected VoiceData, got spurious VoiceStart (C8 regression)")
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(data in proptest::collection::vec(proptest::num::u8::ANY, 0..2048)) {
            let _ = parse_packet(&data);
        }
    }

    #[tokio::test]
    async fn dcs_connect_and_wait_times_out_without_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // No responder — client should time out.
        let _keep = listener;
        let mut client = DcsClient::new(cs("W1AW"), m('B'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(Error::ConnectTimeout(_))),
            "expected ConnectTimeout, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dcs_connect_and_wait_succeeds_on_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a fake 14-byte ACK responder.
        // Per xlxd EncodeConnectAckPacket:
        //   cs[7] + ' ' + module + refl_module + "ACK" + 0x00
        let _responder = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            let mut ack = vec![b' '; 14];
            ack[..4].copy_from_slice(b"W1AW");
            ack[7] = b' ';
            ack[8] = b'B';
            ack[9] = b'C';
            ack[10] = b'A';
            ack[11] = b'C';
            ack[12] = b'K';
            ack[13] = 0x00;
            let _ = listener.send_to(&ack, src).await.unwrap();
        });

        let mut client = DcsClient::new(cs("W1AW"), m('B'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_secs(2)).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(client.state(), ConnectionState::Connected);
    }

    #[tokio::test]
    async fn dcs_connect_and_wait_returns_rejected_on_nak() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Spawn a fake 14-byte NAK responder.
        // Per ircDDBGateway ConnectData::setDCSData case 14U and the
        // parser at parse_packet() — offset 10 = 'N' selects the NAK
        // arm and yields ReflectorEvent::Rejected.
        //   cs[7] + ' ' + module + refl_module + "NAK" + 0x00
        let _responder = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            let mut nak = vec![b' '; 14];
            nak[..4].copy_from_slice(b"W1AW");
            nak[7] = b' ';
            nak[8] = b'B';
            nak[9] = b'C';
            nak[10] = b'N';
            nak[11] = b'A';
            nak[12] = b'K';
            nak[13] = 0x00;
            let _ = listener.send_to(&nak, src).await.unwrap();
        });

        let mut client = DcsClient::new(cs("W1AW"), m('B'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        let result = client.connect_and_wait(Duration::from_secs(2)).await;
        assert!(
            matches!(result, Err(Error::Rejected)),
            "expected Rejected, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dcs_set_poll_interval_stored() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = DcsClient::new(cs("W1AW"), m('B'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        assert_eq!(client.poll_interval, POLL_INTERVAL);
        let new_interval = Duration::from_millis(500);
        client.set_poll_interval(new_interval);
        assert_eq!(client.poll_interval, new_interval);
    }

    #[tokio::test]
    async fn dcs_send_voice_returns_no_tx_header_when_unset() {
        // T30 moved the cached-header enforcement from the ReflectorClient
        // enum variant into DcsClient::tx_header. send_voice must return
        // Error::NoTxHeader when send_header has not been called yet.
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        let frame = VoiceFrame {
            ambe: [0; 9],
            slow_data: [0; 3],
        };
        let result = client.send_voice(sid(0x1234), 0, &frame).await;
        assert!(
            matches!(result, Err(Error::NoTxHeader)),
            "expected Err(NoTxHeader), got {result:?}"
        );
    }

    #[tokio::test]
    async fn dcs_send_eot_returns_no_tx_header_when_unset() {
        // T30 moved the cached-header enforcement from the ReflectorClient
        // enum variant into DcsClient::tx_header. send_eot must return
        // Error::NoTxHeader when send_header has not been called yet.
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = DcsClient::new(cs("W1AW"), m('A'), cs("DCS001"), m('C'), addr)
            .await
            .unwrap();
        let result = client.send_eot(sid(0x1234), 0).await;
        assert!(
            matches!(result, Err(Error::NoTxHeader)),
            "expected Err(NoTxHeader), got {result:?}"
        );
    }
}
