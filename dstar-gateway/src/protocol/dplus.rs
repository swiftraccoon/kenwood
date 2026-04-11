//! `DPlus` protocol (REF reflectors, UDP port 20001).
//!
//! The most common D-STAR reflector protocol. Requires TCP
//! authentication before linking. Uses DSVT voice framing
//! (shared with `DExtra`) wrapped in a 2-byte `DPlus` length prefix.
//!
//! # Link sequence (UDP, per `g4klx/ircDDBGateway`)
//!
//! 1. Send `LINK1` (5 bytes) — initial connect
//! 2. Receive `LINK1` ACK (5 bytes)
//! 3. Send `LINK2` (28 bytes) — login with callsign
//! 4. Receive `OKRW`/`BUSY` ACK (8 bytes)
//! 5. Connected — keepalives every 5 seconds (3-byte poll)
//!
//! # Packet layout table
//!
//! Every UDP packet size the parser ([`parse_packet`]) and builders
//! in this module handle:
//!
//! | Size | Packet              | Direction | Notes                                                |
//! |------|---------------------|-----------|------------------------------------------------------|
//! | 3    | Poll / poll echo    | both      | `0x03 0x60 0x00`, 5 s keepalive                      |
//! | 5    | `LINK1` / unlink    | both      | Initial connect + disconnect, connect ACK            |
//! | 5    | Disconnect          | client→   | `0x05 0x00 0x18 0x00 0x00`                           |
//! | 8    | `OKRW` / `BUSY`     | ←server   | LINK2 step ACK after login                           |
//! | 28   | `LINK2`             | both      | Login with callsign; also the LINK2 ACK length       |
//! | 29   | DSVT voice data     | both      | 2-byte `DPlus` prefix + 27-byte DSVT voice             |
//! | 32   | DSVT voice EOT      | client→   | Voice-data framing with seq bit 6 set (0x40)         |
//! | 58   | DSVT voice header   | both      | 2-byte `DPlus` prefix + 56-byte DSVT header (41-byte D-STAR header) |
//!
//! TCP auth response records from the `DPlus` auth server are parsed
//! separately by [`parse_auth_response`] (framed chunks containing
//! 26-byte host records, variable total length).
//!
//! # Keepalive
//!
//! 3-byte poll: `0x03 0x60 0x00`, sent every 5 seconds.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::error::Error;
use crate::header::{self, DStarHeader};
use crate::types::{Callsign, Module, StreamId};
use crate::voice::{AMBE_SILENCE, VoiceFrame};

use super::{ConnectionState, ReflectorEvent, format_hex_head};

/// Default `DPlus` port.
pub const DEFAULT_PORT: u16 = 20001;

/// Keepalive interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Connection timeout.
///
/// Matches `ircDDBGateway/Common/DPlusHandler.cpp:58`
/// (`m_pollInactivityTimer(1000U, 30U)` — 30 s outgoing poll inactivity
/// timer). The reference resets this timer on every inbound packet
/// (poll echo, header, or AMBE voice frame) — see
/// `ircDDBGateway/Common/DPlusHandler.cpp:272` (poll),
/// `:603,:633` (header), and `:661` (AMBE). Our [`DPlusClient::poll`]
/// mirrors that by resetting `last_poll_received` on any successfully
/// parsed packet, not just `PollEcho`.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum time to wait for a disconnect ACK before giving up and
/// forcing the client to the `Disconnected` state.
///
/// See `DExtra`'s `DISCONNECT_TIMEOUT` for rationale.
pub const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Number of times [`DPlusClient::connect`] retransmits the initial connect packet.
///
/// Per `ircDDBGateway/Common/DPlusProtocolHandler.cpp:64-68`.
const CONNECT_RETX: usize = 2;

/// Number of times [`DPlusClient::send_header`] retransmits the DSVT header packet.
///
/// Per `ircDDBGateway/Common/DPlusProtocolHandler.cpp:64-68`.
const HEADER_RETX: usize = 5;

/// Number of times [`DPlusClient::disconnect`] retransmits the unlink packet.
const DISCONNECT_RETX: usize = 3;

/// Inter-copy delay for retransmission bursts.
const RETX_DELAY: Duration = Duration::from_millis(50);

/// DSVT magic.
const DSVT: &[u8; 4] = b"DSVT";

// ---------------------------------------------------------------------------
// Auth response host list
// ---------------------------------------------------------------------------

/// A single entry from the `DPlus` auth server's host list response.
///
/// Per `ircDDBGateway/Common/DPlusAuthenticator.cpp:169-189`, the
/// auth server returns a list of 26-byte records describing all
/// REF reflectors currently registered in the `DPlus` network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DPlusHost {
    /// Reflector callsign (e.g. `"REF030"`), trimmed of trailing spaces.
    pub callsign: String,
    /// Reflector IPv4 address.
    pub address: std::net::IpAddr,
}

/// Parsed host list from the `DPlus` auth TCP response.
///
/// Returned by [`DPlusClient::auth_hosts`] after
/// [`DPlusClient::authenticate`] completes. Empty until authentication
/// has been performed.
#[derive(Debug, Clone, Default)]
pub struct HostList {
    hosts: Vec<DPlusHost>,
}

impl HostList {
    /// Create an empty host list.
    #[must_use]
    pub const fn new() -> Self {
        Self { hosts: Vec::new() }
    }

    /// Return a slice of all parsed hosts.
    #[must_use]
    pub fn hosts(&self) -> &[DPlusHost] {
        &self.hosts
    }

    /// Number of hosts in the list.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.hosts.len()
    }

    /// True if the list is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Look up a host by callsign (case-insensitive).
    ///
    /// Returns the first match or `None` if the callsign isn't in
    /// the list.
    #[must_use]
    pub fn find(&self, callsign: &str) -> Option<&DPlusHost> {
        self.hosts
            .iter()
            .find(|h| h.callsign.eq_ignore_ascii_case(callsign))
    }
}

/// Parse a `DPlus` auth TCP response into a [`HostList`].
///
/// The auth server response is a sequence of framed chunks. Each
/// chunk has a 3-byte header followed by a body; chunks continue
/// until the TCP stream is closed. Host records live inside the
/// chunk body starting at offset 8.
///
/// Per `ircDDBGateway/Common/DPlusAuthenticator.cpp:151-192`:
///
/// ```text
/// Chunk layout:
///   [0]       low byte of chunk length
///   [1]       high nibble = flags (top two bits must be 0b11);
///             low nibble = high byte of chunk length
///   [2]       packet type, must be 0x01
///   [3..8]    5 bytes of chunk-header filler (discarded)
///   [8..len]  repeated 26-byte host records
///
///   chunk_len = (buf[1] & 0x0F) * 256 + buf[0]
///
/// Host record layout (26 bytes):
///   [ 0..16]  IPv4 address as ASCII text, space-padded
///   [16..24]  reflector callsign, ASCII, space-padded (LONG_CALLSIGN_LENGTH = 8)
///   [24]      module/id byte (ignored)
///   [25]      bit 0x80 = active flag (inactive records skipped)
/// ```
///
/// Records whose IP or callsign field is empty after trimming are
/// skipped, as are records with the active bit clear or with a
/// callsign beginning with `"XRF"` (the reference implementation
/// filters these out before caching).
///
/// # Errors
///
/// Returns [`Error::AuthResponseInvalid`] if:
/// * a chunk header is truncated (fewer than 3 bytes remain),
/// * a chunk body is truncated (fewer than `len` bytes remain),
/// * a chunk claims a length smaller than its own 8-byte header,
/// * the flag bits or packet type fail validation, or
/// * a record contains a non-UTF-8 or unparseable IP address.
#[allow(clippy::too_many_lines)]
pub fn parse_auth_response(data: &[u8]) -> Result<HostList, Error> {
    const CHUNK_HEADER_SIZE: usize = 8;
    const RECORD_SIZE: usize = 26;
    const IP_FIELD_SIZE: usize = 16;
    const CALLSIGN_FIELD_SIZE: usize = 8;

    let mut hosts = Vec::new();
    let mut cursor = 0usize;

    while cursor < data.len() {
        // Need at least 3 bytes to read the length + type header.
        let remaining = data.len() - cursor;
        if remaining < 3 {
            // The reference implementation attempts a 2-byte read
            // after each chunk and simply bails out on short read
            // without erroring; we treat a stray 1-2 trailing bytes
            // the same way.
            if remaining == 0 {
                break;
            }
            return Err(Error::AuthResponseInvalid(
                "truncated chunk header in auth response",
            ));
        }

        let b0 = data[cursor];
        let b1 = data[cursor + 1];
        let b2 = data[cursor + 2];

        let chunk_len = (usize::from(b1 & 0x0F) * 256) + usize::from(b0);

        if (b1 & 0xC0) != 0xC0 || b2 != 0x01 {
            return Err(Error::AuthResponseInvalid(
                "invalid DPlus auth chunk flags or type",
            ));
        }

        if chunk_len < CHUNK_HEADER_SIZE {
            return Err(Error::AuthResponseInvalid(
                "DPlus auth chunk length shorter than header",
            ));
        }

        if cursor + chunk_len > data.len() {
            return Err(Error::AuthResponseInvalid(
                "truncated DPlus auth chunk body",
            ));
        }

        let chunk = &data[cursor..cursor + chunk_len];
        cursor += chunk_len;

        // Records start at offset 8; the reference loop is
        // `for (i = 8; (i + 25) < len; i += 26)`. Note that this is
        // strict `<`, so trailing bytes that don't form a full
        // 26-byte record are ignored silently.
        let mut i = CHUNK_HEADER_SIZE;
        while i + RECORD_SIZE <= chunk.len() {
            let record = &chunk[i..i + RECORD_SIZE];
            i += RECORD_SIZE;

            // IP address field: 16 bytes of ASCII, space-padded on
            // the right. Trim trailing spaces AND nulls (real
            // servers pad with spaces, but synthesised test data
            // may use nulls).
            let ip_bytes = &record[..IP_FIELD_SIZE];
            let ip_str = std::str::from_utf8(ip_bytes)
                .map_err(|_| Error::AuthResponseInvalid("non-UTF-8 in IP address field"))?
                .trim_matches([' ', '\0']);

            // Callsign field: 8 bytes of ASCII, space-padded.
            let callsign_bytes = &record[IP_FIELD_SIZE..IP_FIELD_SIZE + CALLSIGN_FIELD_SIZE];
            let callsign = std::str::from_utf8(callsign_bytes)
                .map_err(|_| Error::AuthResponseInvalid("non-UTF-8 in callsign field"))?
                .trim_matches([' ', '\0']);

            // Active flag lives in the top bit of the record's
            // last byte (index 25).
            let active = (record[25] & 0x80) == 0x80;

            // Skip empty, inactive, or XRF-prefixed records (the
            // reference implementation filters these out before
            // caching, and our callers expect the same).
            if ip_str.is_empty() || callsign.is_empty() || !active || callsign.starts_with("XRF") {
                continue;
            }

            let address: std::net::IpAddr = ip_str
                .parse()
                .map_err(|_| Error::AuthResponseInvalid("malformed IP address"))?;

            hosts.push(DPlusHost {
                callsign: callsign.to_owned(),
                address,
            });
        }
    }

    Ok(HostList { hosts })
}

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a `DPlus` initial connect packet (5 bytes, step 1).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_LINK1`.
///
/// # Panics
///
/// Cannot panic — returns a fixed byte literal.
#[must_use]
pub const fn build_connect_step1() -> [u8; 5] {
    [0x05, 0x00, 0x18, 0x00, 0x01]
}

/// Build a `DPlus` login packet (28 bytes, step 2).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_LINK2`:
/// bytes 4-11 = callsign (trimmed, zero-padded to 16),
/// bytes 20-27 = `"DV019999"`.
///
/// # Panics
///
/// Cannot panic — `callsign` is a validated 8-byte type so the trimmed
/// length is at most 8, and the destination slice `pkt[4..4+trimmed_len]`
/// fits within the 28-byte buffer.
#[must_use]
pub fn build_connect_step2(callsign: &Callsign) -> [u8; 28] {
    let mut pkt = [0u8; 28];
    pkt[0] = 0x1C;
    pkt[1] = 0xC0;
    pkt[2] = 0x04;
    pkt[3] = 0x00;
    // Callsign trimmed of trailing spaces, then zero-padded in place.
    let bytes = callsign.as_bytes();
    let trimmed_len = bytes.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
    pkt[4..4 + trimmed_len].copy_from_slice(&bytes[..trimmed_len]);
    // DV client identifier at offset 20.
    pkt[20..28].copy_from_slice(b"DV019999");
    pkt
}

/// Build a `DPlus` disconnect packet (5 bytes).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_UNLINK`.
///
/// The `DPlus` disconnect is a fixed 5-byte sequence and does not
/// encode the callsign or module. The typed parameters are still
/// accepted so the signature lines up with the other `build_*`
/// helpers and documents what the call site was intending.
///
/// # Panics
///
/// Cannot panic — returns a fixed byte literal.
#[must_use]
pub const fn build_disconnect(_callsign: &Callsign, _module: Module) -> [u8; 5] {
    [0x05, 0x00, 0x18, 0x00, 0x00]
}

/// Build a `DPlus` keepalive packet (3 bytes).
///
/// # Panics
///
/// Cannot panic — returns a fixed byte literal.
#[must_use]
pub const fn build_poll() -> [u8; 3] {
    [0x03, 0x60, 0x00]
}

/// Build a `DPlus` voice header (58 bytes: 2-byte prefix + DSVT).
///
/// Per `ircDDBGateway` `HeaderData::getDPlusData`:
/// `[0x3A, 0x80, "DSVT", 0x10, 0x00, 0x00, 0x00, 0x20,
///   band1, band2, band3, id_lo, id_hi, 0x80, flags[3], calls...]`
///
/// # Panics
///
/// Cannot panic — `header.encode_for_dsvt()` is a fixed-length block,
/// `stream_id` is a validated non-zero u16, and all other writes are
/// straight appends with known sizes.
#[must_use]
pub fn build_header(header: &DStarHeader, stream_id: StreamId) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(58);
    pkt.push(0x3A); // length = 58
    pkt.push(0x80); // type
    pkt.extend_from_slice(DSVT);
    pkt.push(0x10); // header flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.push(0x20); // config
    pkt.extend_from_slice(&[0x00, 0x01, 0x02]); // band1=0, band2=1, band3=2
    pkt.extend_from_slice(&stream_id.get().to_le_bytes()); // LE per reference
    pkt.push(0x80); // header indicator
    // DSVT encoding zeroes flag1/flag2/flag3 before the header CRC is
    // recomputed, per `ircDDBGateway/Common/HeaderData.cpp:615-617`.
    pkt.extend_from_slice(&header.encode_for_dsvt());
    debug_assert_eq!(pkt.len(), 58);
    pkt
}

/// Build a `DPlus` voice data packet (29 bytes: 2-byte prefix + DSVT).
///
/// Per `ircDDBGateway` `AMBEData::getDPlusData`:
/// Normal: `[0x1D, 0x80, ...]`, EOT: `[0x20, 0x80, ...]`
///
/// # Panics
///
/// Cannot panic — `stream_id` is a validated non-zero u16, `frame.ambe`
/// and `frame.slow_data` are fixed-size arrays, and all writes are
/// straight appends with statically-known sizes.
#[must_use]
pub fn build_voice(stream_id: StreamId, seq: u8, frame: &VoiceFrame) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(29);
    pkt.push(0x1D); // length (normal voice)
    pkt.push(0x80); // type
    pkt.extend_from_slice(DSVT);
    pkt.push(0x20); // voice flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.push(0x20); // config
    pkt.extend_from_slice(&[0x00, 0x01, 0x02]); // band1/2/3
    pkt.extend_from_slice(&stream_id.get().to_le_bytes()); // LE per reference
    pkt.push(seq);
    pkt.extend_from_slice(&frame.ambe);
    pkt.extend_from_slice(&frame.slow_data);
    debug_assert_eq!(pkt.len(), 29);
    pkt
}

/// Build a `DPlus` end-of-transmission packet (32 bytes).
///
/// Per `ircDDBGateway/Common/AMBEData.cpp:380-388` (`getDPlusData`
/// with `isEnd()` true), the `DPlus` EOT is a 32-byte packet ending
/// in the 6-byte pattern `[0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]`.
/// This differs from the 29-byte `DPlus` voice data packet.
///
/// `xlxd/src/cdplusprotocol.cpp:507 IsValidDvLastFramePacket`
/// rejects EOTs of any other length — so a 29-byte EOT (produced
/// by naively reusing `build_voice` with the EOT seq bit) is
/// silently dropped and streams hang from the reflector's side.
///
/// # Layout
///
/// ```text
/// [0]      0x20            DPlus prefix byte (EOT variant)
/// [1]      0x80            DPlus type byte
/// [2..6]   "DSVT"
/// [6]      0x20            voice flag
/// [7..10]  0x00 0x00 0x00  reserved
/// [10]     0x20            config
/// [11..14] 0x00 0x01 0x02  band1 / band2 / band3
/// [14..16] stream_id LE
/// [16]     seq | 0x40      EOT seq bit set
/// [17..26] AMBE silence    9 bytes from AMBE_SILENCE const
/// [26..32] 0x55 0x55 0x55 0x55 0xC8 0x7A    end pattern
/// ```
///
/// # Panics
///
/// Cannot panic — `stream_id` is a validated non-zero u16 and all
/// writes are straight appends with statically-known sizes.
#[must_use]
pub fn build_eot(stream_id: StreamId, seq: u8) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(32);
    pkt.push(0x20); // DPlus prefix (EOT variant)
    pkt.push(0x80); // DPlus type
    pkt.extend_from_slice(DSVT);
    pkt.push(0x20); // voice flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.push(0x20); // config
    pkt.extend_from_slice(&[0x00, 0x01, 0x02]); // band1 / band2 / band3
    pkt.extend_from_slice(&stream_id.get().to_le_bytes()); // stream ID LE
    pkt.push(seq | 0x40); // seq with EOT bit
    pkt.extend_from_slice(&AMBE_SILENCE); // 9 silence bytes
    // End pattern: 0x55 0x55 0x55 0x55 0xC8 0x7A (6 bytes)
    pkt.extend_from_slice(&[0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]);
    debug_assert_eq!(pkt.len(), 32);
    pkt
}

// ---------------------------------------------------------------------------
// Packet parser
// ---------------------------------------------------------------------------

/// Return true if an 8-byte `DPlus` login reply is an `OKRW` accept.
///
/// The 8-byte `DPlus` login reply lays out as `[0x08, 0xC0, 0x04, 0x00]`
/// followed by a 4-byte ASCII status field at offsets `[4..8]`:
///
/// - `"OKRW"` — accept (`CT_ACK`)
/// - anything else (typically `"BUSY"`) — reject (`CT_NAK`)
///
/// References:
/// - `ref/ircDDBGateway/Common/ConnectData.cpp:251-259`
///   (`CConnectData::setDPlusData` length-8 arm: `memcmp(data + 4U, "OKRW", 4U)`)
/// - `ref/xlxd/src/cdplusprotocol.cpp:535-544`
///   (`EncodeLoginAckPacket`: `{ 0x08, 0xC0, 0x04, 0x00, 'O', 'K', 'R', 'W' }`;
///   `EncodeLoginNackPacket`: `{ 0x08, 0xC0, 0x04, 0x00, 'B', 'U', 'S', 'Y' }`)
const fn is_plus_ok_rw(data: [u8; 8]) -> bool {
    data[4] == b'O' && data[5] == b'K' && data[6] == b'R' && data[7] == b'W'
}

/// Parse an incoming `DPlus` packet into a [`ReflectorEvent`].
#[must_use]
pub fn parse_packet(data: &[u8]) -> Option<ReflectorEvent> {
    // Per ircDDBGateway/Common/DPlusProtocolHandler.cpp:131-145,
    // valid non-DSVT DPlus packet lengths are:
    //   3  — poll echo
    //   5  — LINK1 / UNLINK (connect ACK)
    //   8  — OKRW / BUSY (LINK2 reply)
    //   28 — LINK2 (login echo)
    // Any other length with no "DSVT" magic is unknown traffic.
    //
    // Check length FIRST before inspecting bytes. Packets shorter
    // than 6 bytes cannot carry the "DSVT" magic at offsets [2..6],
    // so they go straight to the non-DSVT classifier. Longer packets
    // without "DSVT" also fall through to the classifier, but by
    // then the slice is known to be safely indexable.
    let is_dsvt = data.len() >= 6 && &data[2..6] == DSVT;
    if !is_dsvt {
        match data.len() {
            // Step-1 ACK (5 bytes) always means "connect acknowledged,
            // send step-2 login" — there is no byte-level accept/reject
            // signal on the LINK1 reply. The 28-byte arm is the LINK2
            // echo some reflectors return as a connect confirmation.
            // Only the 8-byte LINK2 reply carries the OKRW / BUSY
            // distinction.
            5 | 28 => return Some(ReflectorEvent::Connected),
            // Step-2 reply: OKRW at bytes [4..8] is accept, anything
            // else (BUSY, FAIL, banned IP, etc.) is reject. Without
            // this distinction a reflector that explicitly rejects
            // us looks identical to a successful connect and the
            // client hits ConnectTimeout after 30 seconds instead of
            // surfacing Error::Rejected immediately. See
            // ircDDBGateway/Common/ConnectData.cpp:251-259.
            8 => {
                // The match arm guarantees `data.len() == 8`, so the
                // fixed-size copy below can never fail.
                let mut arr = [0u8; 8];
                arr.copy_from_slice(data);
                if is_plus_ok_rw(arr) {
                    return Some(ReflectorEvent::Connected);
                }
                return Some(ReflectorEvent::Rejected);
            }
            3 => return Some(ReflectorEvent::PollEcho),
            _ => return None,
        }
    }

    // DPlus DSVT packets have a 2-byte prefix: [len, 0x80] + "DSVT" + ...
    // Header: 58 bytes (0x3A 0x80 + 56-byte DSVT header)
    // Voice:  29 bytes (0x1D 0x80 + 27-byte DSVT voice)
    // At this point data.len() >= 6 and data[2..6] == "DSVT" (checked above).
    let dsvt = &data[2..]; // strip 2-byte `DPlus` prefix
    // dsvt.len() >= 4 here; need >= 15 to read flag/stream_id/seq below.
    if dsvt.len() < 15 {
        return None;
    }
    let is_header = dsvt[4] == 0x10;
    // Stream ID 0 is reserved per the D-STAR spec — a packet with
    // stream_id == 0 is malformed, so drop it via `?` on the
    // `Option` returned by `StreamId::new`.
    let stream_id = StreamId::new(u16::from_le_bytes([dsvt[12], dsvt[13]]))?;

    if is_header && dsvt.len() >= 56 {
        let mut arr = [0u8; header::ENCODED_LEN];
        arr.copy_from_slice(&dsvt[15..56]);
        return Some(ReflectorEvent::VoiceStart {
            header: DStarHeader::decode(&arr),
            stream_id,
        });
    } else if !is_header && dsvt.len() >= 27 {
        let seq = dsvt[14];
        let mut ambe = [0u8; 9];
        ambe.copy_from_slice(&dsvt[15..24]);
        let mut slow_data = [0u8; 3];
        slow_data.copy_from_slice(&dsvt[24..27]);

        if seq & 0x40 != 0 {
            return Some(ReflectorEvent::VoiceEnd { stream_id });
        }
        return Some(ReflectorEvent::VoiceData {
            stream_id,
            seq,
            frame: VoiceFrame { ambe, slow_data },
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Async client
// ---------------------------------------------------------------------------

/// Async `DPlus` reflector client.
///
/// Manages the UDP connection to a REF reflector. The `DPlus` protocol
/// requires a two-step connect (connect → link), unlike `DExtra`'s
/// single-step connect. A mandatory TCP auth step with
/// `auth.dstargateway.org:20001` must be completed first — REF
/// reflectors silently drop UDP packets from callsigns that have not
/// authenticated.
///
/// For most users the unified [`crate::ReflectorClient`] is easier;
/// drop to this type when you need per-protocol control over a REF
/// reflector on UDP port 20001.
///
/// # Example
///
/// ```no_run
/// use dstar_gateway::protocol::dplus::DPlusClient;
/// use dstar_gateway::{Callsign, Module};
/// use std::time::Duration;
///
/// # async fn example() -> Result<(), dstar_gateway::Error> {
/// let mut client = DPlusClient::new(
///     Callsign::try_from_str("W1AW")?,
///     Module::try_from_char('B')?, // local module
///     "1.2.3.4:20001".parse().unwrap(),
/// )
/// .await?;
/// // Mandatory TCP auth against auth.dstargateway.org:20001 before UDP.
/// client.authenticate().await?;
/// client.connect_and_wait(Duration::from_secs(5)).await?;
/// // ... send voice via send_header + send_voice + send_eot ...
/// client.disconnect().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct DPlusClient {
    socket: UdpSocket,
    remote: SocketAddr,
    callsign: Callsign,
    module: Module,
    state: ConnectionState,
    login_sent: bool,
    last_poll_sent: Instant,
    last_poll_received: Instant,
    auth_hosts: HostList,
    poll_interval: Duration,
    /// Effective poll inactivity timeout.
    ///
    /// Initialised to [`POLL_TIMEOUT`] (30 s) and overridable via
    /// [`Self::set_poll_timeout`]. Isolating the value on the instance
    /// lets integration tests drive the timeout-trip path in real time
    /// without sleeping for 30+ seconds per run.
    poll_timeout: Duration,
    /// Timestamp of the most recent `disconnect()` call. See the
    /// corresponding field on `DExtraClient` for rationale.
    disconnect_sent_at: Option<Instant>,
    /// Optional override for the `DPlus` auth TCP endpoint.
    ///
    /// When `None` (the default), [`authenticate`](Self::authenticate)
    /// resolves `auth.dstargateway.org:20001` via DNS. When `Some`, the
    /// concrete `SocketAddr` is used directly, skipping DNS. Primarily
    /// for integration tests that need to redirect the auth TCP
    /// connection at a local fake server, but also usable by operators
    /// who run a private auth server on their own infrastructure.
    auth_endpoint: Option<SocketAddr>,
    /// Stream ID of the most recently active RX stream, used to
    /// deduplicate spurious `VoiceStart` events on mid-stream header
    /// refreshes.
    ///
    /// REF reflectors re-send the 58-byte DSVT voice header at every
    /// superframe boundary (~420 ms) so late joiners can sync up.
    /// [`parse_packet`] emits `ReflectorEvent::VoiceStart` for each of
    /// those refreshes, which — without this tracker — would flicker
    /// the radio display and re-initialise TX state every superframe.
    /// [`poll`](Self::poll) suppresses the second and subsequent
    /// `VoiceStart` for a given `stream_id`, matching the stateful
    /// stream-ID tracking DCS uses for its C8 fix. Cleared to `None`
    /// on [`ReflectorEvent::VoiceEnd`].
    last_rx_stream_id: Option<StreamId>,

    /// Timestamp of the most recently received voice frame (header,
    /// data, or EOT) from the reflector, used to synthesize a
    /// [`ReflectorEvent::VoiceEnd`] when the remote operator stops
    /// transmitting without sending a proper EOT packet.
    ///
    /// Real-world observation on REF030: some operators key up very
    /// briefly (3-5 voice frames) and the reflector simply stops
    /// forwarding packets without emitting a 32-byte EOT packet.
    /// Without this timeout we'd never clear [`Self::last_rx_stream_id`],
    /// the REPL would never send an EOT to the radio's MMDVM modem,
    /// and the radio would stay in "receive active" state until the
    /// next stream arrived (potentially minutes later).
    ///
    /// [`poll`](Self::poll) synthesizes a `VoiceEnd` event once
    /// [`VOICE_INACTIVITY_TIMEOUT`] has elapsed since this timestamp,
    /// matching `ircDDBGateway/Common/DPlusHandler.cpp:63` which uses
    /// `m_inactivityTimer(1000U, NETWORK_TIMEOUT)` — a 2 second
    /// inactivity window.
    ///
    /// Cleared to `None` on:
    /// - explicit [`ReflectorEvent::VoiceEnd`] (real EOT packet)
    /// - synthesized `VoiceEnd` after the timeout fires
    /// - new stream start (the field is overwritten on the first
    ///   voice event of a fresh `stream_id`, not cleared)
    last_voice_rx: Option<Instant>,
}

/// Maximum time between voice events before the client synthesizes a
/// [`ReflectorEvent::VoiceEnd`] for the active stream.
///
/// Matches `ircDDBGateway/Common/DStarDefines.h:122` which defines
/// `NETWORK_TIMEOUT = 2` seconds for all D-STAR network protocols.
/// Shorter values risk cutting off an operator who pauses briefly
/// mid-over; longer values leave the radio's MMDVM modem in a stuck
/// "receive active" state after a brief keyup.
pub const VOICE_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(2);

impl DPlusClient {
    /// Create a new client and bind a local UDP socket.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the socket cannot be bound.
    pub async fn new(
        callsign: Callsign,
        local_module: Module,
        remote: SocketAddr,
    ) -> Result<Self, Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let now = Instant::now();
        Ok(Self {
            socket,
            remote,
            callsign,
            module: local_module,
            state: ConnectionState::Disconnected,
            login_sent: false,
            last_poll_sent: now,
            last_poll_received: now,
            auth_hosts: HostList::new(),
            poll_interval: POLL_INTERVAL,
            poll_timeout: POLL_TIMEOUT,
            disconnect_sent_at: None,
            auth_endpoint: None,
            last_rx_stream_id: None,
            last_voice_rx: None,
        })
    }

    /// Override the `DPlus` auth TCP endpoint.
    ///
    /// By default, [`authenticate`](Self::authenticate) connects to
    /// `auth.dstargateway.org:20001` (resolved via DNS at call time).
    /// Passing `Some(addr)` here routes the next `authenticate()` call
    /// at the supplied `SocketAddr` instead, skipping DNS entirely.
    /// Passing `None` restores the default.
    ///
    /// Intended primarily for integration tests that point `authenticate`
    /// at a local loopback TCP fake, but also usable by operators who
    /// mirror the `DPlus` auth server on their own infrastructure.
    pub const fn set_auth_endpoint(&mut self, endpoint: Option<SocketAddr>) {
        self.auth_endpoint = endpoint;
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

    /// Override the keepalive poll interval.
    ///
    /// Defaults to [`POLL_INTERVAL`] (5 seconds). Decrease this for
    /// links traversing NAT where connection-tracking timers drop idle
    /// flows faster than the default keepalive cadence.
    pub const fn set_poll_interval(&mut self, interval: Duration) {
        self.poll_interval = interval;
    }

    /// Override the poll inactivity timeout.
    ///
    /// Defaults to [`POLL_TIMEOUT`] (30 s, matching `ircDDBGateway`'s
    /// `m_pollInactivityTimer`). Mainly intended for integration tests
    /// that need to exercise the timeout-trip path without sleeping
    /// for 30 s per run — production callers should leave this alone.
    pub const fn set_poll_timeout(&mut self, timeout: Duration) {
        self.poll_timeout = timeout;
    }

    /// Authenticate with the `DPlus` auth server (TCP).
    ///
    /// This registers your callsign and IP address with the `DPlus`
    /// network. Must be called before [`connect`](Self::connect) or
    /// REF reflectors will silently drop your UDP packets.
    ///
    /// Auth server: `auth.dstargateway.org:20001`
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the TCP connection or auth fails, or
    /// [`Error::AuthResponseInvalid`] if the server's host list
    /// response cannot be parsed.
    pub async fn authenticate(&mut self) -> Result<(), Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        tracing::info!(callsign = %self.callsign, "DPlus TCP auth starting");

        // If an explicit endpoint override is set, connect directly to
        // that `SocketAddr` (skipping DNS). Otherwise fall through to
        // the default `auth.dstargateway.org:20001` hostname, which is
        // resolved by tokio at call time.
        let mut stream = if let Some(endpoint) = self.auth_endpoint {
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::net::TcpStream::connect(endpoint),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "auth connect timeout")
            })??
        } else {
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::net::TcpStream::connect("auth.dstargateway.org:20001"),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "auth connect timeout")
            })??
        };

        // Build 56-byte auth packet (per ircDDBGateway DPlusAuthenticator.cpp).
        let mut pkt = vec![b' '; 56];
        pkt[0] = 0x38;
        pkt[1] = 0xC0;
        pkt[2] = 0x01;
        pkt[3] = 0x00;
        pkt[4..12].copy_from_slice(self.callsign.as_bytes());
        pkt[12..20].copy_from_slice(b"DV019999");
        pkt[28..33].copy_from_slice(b"W7IB2");
        pkt[40..47].copy_from_slice(b"DHS0257");

        stream.write_all(&pkt).await?;

        // Read the full response body (one or more framed chunks,
        // each an 8-byte header + N x 26-byte host records). The
        // auth server closes the TCP connection after transmitting
        // the list, so a bounded accumulate loop with per-read
        // timeout terminates cleanly on EOF.
        let mut response = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => response.extend_from_slice(&buf[..n]),
                Ok(Err(e)) => return Err(e.into()),
            }
        }

        // Parse framed chunks of 26-byte host records.
        self.auth_hosts = parse_auth_response(&response)?;

        tracing::info!(
            bytes = response.len(),
            hosts = self.auth_hosts.len(),
            "DPlus TCP auth complete"
        );
        Ok(())
    }

    /// Send the initial connect request (step 1 of 3).
    ///
    /// The full sequence is: step1 → ACK → step2 (login) → ACK → link → ACK.
    /// Steps 2 and 3 are handled automatically by [`poll`](Self::poll).
    ///
    /// Call [`authenticate`](Self::authenticate) first for REF reflectors.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn connect(&mut self) -> Result<(), Error> {
        let pkt = build_connect_step1();
        // Per ircDDBGateway/Common/DPlusProtocolHandler.cpp:64-68, the
        // initial connect packet is retransmitted CONNECT_RETX times
        // with RETX_DELAY between copies to survive UDP loss on the
        // first hop.
        for i in 0..CONNECT_RETX {
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            if i + 1 < CONNECT_RETX {
                tokio::time::sleep(RETX_DELAY).await;
            }
        }
        self.state = ConnectionState::Connecting;
        self.login_sent = false;
        tracing::debug!(
            target: "dstar_gateway::dplus",
            reflector = %self.remote,
            module = %self.module,
            state = "Connecting",
            "DPlus state -> Connecting"
        );
        tracing::info!(
            reflector = %self.remote,
            module = %self.module,
            "DPlus connect sent"
        );
        Ok(())
    }

    /// Connect to the reflector and wait for the connection ACK or timeout.
    ///
    /// Drives the state machine internally: sends the connect packet
    /// (with retransmission), then polls until state is `Connected` or
    /// the timeout expires. Handles the multi-step login flow
    /// automatically via [`poll`](Self::poll). Use this as a more
    /// convenient alternative to calling [`connect`](Self::connect) and
    /// [`poll`](Self::poll) in a loop.
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
                Ok(Ok(_)) => {
                    // DPlus is two-step: poll() absorbs the step1 ACK
                    // internally (sends step2 login, returns Ok(None))
                    // and only emits ReflectorEvent::Connected after the
                    // step2 ACK flips state. Inspect state instead of
                    // matching the event so the step1-absorbed Ok(None)
                    // does not trip an early return. A future "unify
                    // with DExtra/DCS" refactor that switches to event
                    // matching would silently break the two-step path.
                    if self.state == ConnectionState::Connected {
                        return Ok(());
                    }
                    if self.state == ConnectionState::Disconnected {
                        return Err(Error::Rejected);
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err(Error::ConnectTimeout(timeout)),
            }
        }
    }

    /// Send the disconnect request.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn disconnect(&mut self) -> Result<(), Error> {
        let pkt = build_disconnect(&self.callsign, self.module);
        // Unlink is retransmitted DISCONNECT_RETX times so the reflector
        // has a better chance of releasing the slot on lossy links.
        for i in 0..DISCONNECT_RETX {
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            if i + 1 < DISCONNECT_RETX {
                tokio::time::sleep(RETX_DELAY).await;
            }
        }
        self.state = ConnectionState::Disconnecting;
        self.disconnect_sent_at = Some(Instant::now());
        tracing::debug!(
            target: "dstar_gateway::dplus",
            state = "Disconnecting",
            "DPlus state -> Disconnecting"
        );
        tracing::info!("DPlus disconnect sent");
        Ok(())
    }

    /// Poll for the next event. Handles the two-step connect
    /// (connect ACK → send link → link ACK) automatically.
    ///
    /// # Errors
    ///
    /// Returns an I/O error on socket failures.
    #[allow(clippy::too_many_lines)]
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, Error> {
        // Force Disconnecting -> Disconnected after DISCONNECT_TIMEOUT
        // if the reflector never ACKs the unlink.
        if self.state == ConnectionState::Disconnecting
            && let Some(sent) = self.disconnect_sent_at
            && sent.elapsed() >= DISCONNECT_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            self.disconnect_sent_at = None;
            tracing::debug!(
                target: "dstar_gateway::dplus",
                state = "Disconnected",
                reason = "disconnect_timeout",
                "DPlus state -> Disconnected (unlink ACK never arrived)"
            );
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Synthesize a VoiceEnd if the reflector stopped forwarding
        // voice data mid-stream without sending a proper EOT packet.
        // Observed live on REF030 C with brief M6JBE keyups that the
        // reflector simply stopped without flagging. Matches
        // ircDDBGateway/Common/DPlusHandler.cpp:835-843 which clears
        // stream state after m_inactivityTimer (NETWORK_TIMEOUT =
        // 2 s) expires. Without this, the REPL's rx_stream_id stays
        // stuck on the aborted stream, the radio's MMDVM never gets
        // a proper EOT, and its receive state machine is stranded
        // until the next stream arrives (potentially minutes later).
        if let Some(last) = self.last_voice_rx
            && last.elapsed() >= VOICE_INACTIVITY_TIMEOUT
            && let Some(stream_id) = self.last_rx_stream_id
        {
            self.last_rx_stream_id = None;
            self.last_voice_rx = None;
            tracing::debug!(
                target: "dstar_gateway::dplus",
                stream_id = %stream_id,
                "DPlus synthesizing VoiceEnd after voice inactivity timeout"
            );
            return Ok(Some(ReflectorEvent::VoiceEnd { stream_id }));
        }

        // Send keepalive if connected.
        if self.state == ConnectionState::Connected
            && self.last_poll_sent.elapsed() >= self.poll_interval
        {
            let pkt = build_poll();
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            self.last_poll_sent = Instant::now();
            tracing::trace!(
                target: "dstar_gateway::dplus",
                len = pkt.len(),
                head = %format_hex_head(&pkt),
                "DPlus tx keepalive"
            );
        }

        // Check timeout.
        if self.state == ConnectionState::Connected
            && self.last_poll_received.elapsed() >= self.poll_timeout
        {
            self.state = ConnectionState::Disconnected;
            tracing::debug!(
                target: "dstar_gateway::dplus",
                state = "Disconnected",
                reason = "poll_timeout",
                "DPlus state -> Disconnected (keepalive timeout)"
            );
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Receive.
        let mut buf = [0u8; 2048];
        let recv =
            tokio::time::timeout(Duration::from_millis(100), self.socket.recv_from(&mut buf)).await;

        let (len, _addr) = match recv {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Ok(None),
        };

        tracing::trace!(
            target: "dstar_gateway::dplus",
            len,
            head = %format_hex_head(&buf[..len]),
            "DPlus rx"
        );

        let Some(event) = parse_packet(&buf[..len]) else {
            return Ok(None);
        };

        // Any successfully parsed packet from the reflector proves the
        // link is alive, so reset the keepalive clock here rather than
        // only in the `PollEcho` arm below. This matches
        // `ircDDBGateway/Common/DPlusHandler.cpp` which calls
        // `m_pollInactivityTimer.start()` on incoming poll (:272),
        // header (:603, :633), and AMBE voice (:661). Previously we
        // only reset on `PollEcho`, so a busy reflector streaming
        // voice for ≥30 s without interleaving poll echoes tripped
        // the `POLL_TIMEOUT` guard above and forced the client to
        // `Disconnected` mid-transmission. Observed live on REF030 C
        // during a ~30 s voice burst.
        self.last_poll_received = Instant::now();

        match &event {
            ReflectorEvent::Connected => {
                if !self.login_sent {
                    // Step 1 ACK → send login (step 2).
                    let pkt = build_connect_step2(&self.callsign);
                    let _ = self.socket.send_to(&pkt, self.remote).await?;
                    self.login_sent = true;
                    tracing::info!("DPlus login sent");
                    return Ok(None);
                }
                // Step 2 phase: we've already sent the login packet.
                // `parse_packet` classifies any 5/28-byte non-DSVT
                // reply as Connected, so a duplicate 5-byte LINK1 ACK
                // from the reflector (the CONNECT_RETX retransmission
                // causes the reflector to echo LINK1 twice) MUST NOT
                // count as a login ACK — otherwise a rejecting
                // reflector's BUSY on the 8-byte reply never gets a
                // chance to set `Rejected` because the second 5-byte
                // echo already flipped state to Connected. Only the
                // 8-byte OKRW reply or the 28-byte LINK2 echo should
                // be treated as the login ACK.
                if len == 5 {
                    // Duplicate step-1 ACK from retransmitted LINK1.
                    return Ok(None);
                }
                // Step 2 ACK (OKRW / LINK2 echo) → fully connected.
                self.state = ConnectionState::Connected;
                tracing::debug!(
                    target: "dstar_gateway::dplus",
                    state = "Connected",
                    "DPlus state -> Connected"
                );
                tracing::info!("DPlus connected");
            }
            ReflectorEvent::Rejected | ReflectorEvent::Disconnected => {
                self.state = ConnectionState::Disconnected;
                self.disconnect_sent_at = None;
                tracing::debug!(
                    target: "dstar_gateway::dplus",
                    state = "Disconnected",
                    reason = "reflector_reply",
                    "DPlus state -> Disconnected (reflector rejected or closed)"
                );
            }
            ReflectorEvent::PollEcho => {
                tracing::trace!(
                    target: "dstar_gateway::dplus",
                    "DPlus poll echo"
                );
            }
            ReflectorEvent::VoiceStart { stream_id, .. } => {
                // REF reflectors retransmit the DSVT voice header every
                // superframe (~420 ms) so late joiners can sync. The
                // first `VoiceStart` for a given `stream_id` is the
                // real stream start; all subsequent ones are keep-alive
                // refreshes and must not propagate to the caller or the
                // REPL will re-announce the stream and re-send the
                // MMDVM header to the radio every superframe. This
                // matches the stateful-tracking approach DCS uses for
                // its C8 fix (see `DcsClient::poll`).
                if self.last_rx_stream_id == Some(*stream_id) {
                    self.last_voice_rx = Some(Instant::now());
                    tracing::trace!(
                        target: "dstar_gateway::dplus",
                        stream_id = %stream_id,
                        "DPlus voice header rx (suppressed: mid-stream refresh)"
                    );
                    return Ok(None);
                }
                self.last_rx_stream_id = Some(*stream_id);
                self.last_voice_rx = Some(Instant::now());
                tracing::debug!(
                    target: "dstar_gateway::dplus",
                    stream_id = %stream_id,
                    "DPlus voice header rx"
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
                    target: "dstar_gateway::dplus",
                    stream_id = %stream_id,
                    seq = *seq,
                    "DPlus voice data rx"
                );
            }
            ReflectorEvent::VoiceEnd { stream_id } => {
                if self.last_rx_stream_id == Some(*stream_id) {
                    self.last_rx_stream_id = None;
                }
                self.last_voice_rx = None;
                tracing::debug!(
                    target: "dstar_gateway::dplus",
                    stream_id = %stream_id,
                    "DPlus voice EOT rx"
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
            target: "dstar_gateway::dplus",
            stream_id = %stream_id,
            "DPlus voice header tx"
        );
        tracing::trace!(
            target: "dstar_gateway::dplus",
            len = pkt.len(),
            head = %format_hex_head(&pkt),
            "DPlus tx header"
        );
        // Per ircDDBGateway/Common/DPlusProtocolHandler.cpp:64-68 the
        // voice header is retransmitted HEADER_RETX times so late
        // joiners and lossy paths still see the start of a stream.
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
            target: "dstar_gateway::dplus",
            stream_id = %stream_id,
            seq,
            len = pkt.len(),
            "DPlus tx voice"
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
            target: "dstar_gateway::dplus",
            stream_id = %stream_id,
            seq,
            "DPlus voice EOT tx"
        );
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        Ok(())
    }

    /// Current connection state.
    #[must_use]
    pub const fn state(&self) -> ConnectionState {
        self.state
    }

    /// Host list parsed from the `DPlus` auth response.
    ///
    /// Empty until [`authenticate`](Self::authenticate) has been
    /// called successfully. Provides the current REF reflector →
    /// IP mapping from `auth.dstargateway.org`.
    #[must_use]
    pub const fn auth_hosts(&self) -> &HostList {
        &self.auth_hosts
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
    fn connect_step1_format() {
        let pkt = build_connect_step1();
        assert_eq!(pkt, [0x05, 0x00, 0x18, 0x00, 0x01]);
    }

    #[test]
    fn connect_step2_format() {
        let pkt = build_connect_step2(&cs("W1AW"));
        assert_eq!(pkt.len(), 28);
        assert_eq!(pkt[0], 0x1C);
        assert_eq!(pkt[1], 0xC0);
        assert_eq!(&pkt[4..8], b"W1AW");
        assert_eq!(&pkt[20..28], b"DV019999");
    }

    #[test]
    fn disconnect_packet_format() {
        let pkt = build_disconnect(&cs("W1AW"), m('C'));
        assert_eq!(pkt, [0x05, 0x00, 0x18, 0x00, 0x00]);
    }

    #[test]
    fn poll_packet_format() {
        let pkt = build_poll();
        assert_eq!(pkt, [0x03, 0x60, 0x00]);
    }

    #[test]
    fn parse_step1_ack() {
        let pkt = build_connect_step1();
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_step2_ack() {
        let pkt = build_connect_step2(&cs("W1AW"));
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_login_reply_okrw_is_connected() {
        // xlxd EncodeLoginAckPacket (cdplusprotocol.cpp:535-539):
        //   { 0x08, 0xC0, 0x04, 0x00, 'O', 'K', 'R', 'W' }
        let pkt = [0x08, 0xC0, 0x04, 0x00, b'O', b'K', b'R', b'W'];
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_login_reply_busy_is_rejected() {
        // xlxd EncodeLoginNackPacket (cdplusprotocol.cpp:541-545):
        //   { 0x08, 0xC0, 0x04, 0x00, 'B', 'U', 'S', 'Y' }
        let pkt = [0x08, 0xC0, 0x04, 0x00, b'B', b'U', b'S', b'Y'];
        let evt = parse_packet(&pkt).unwrap();
        assert!(
            matches!(evt, ReflectorEvent::Rejected),
            "BUSY reply should classify as Rejected, got {evt:?}"
        );
    }

    #[test]
    fn parse_login_reply_unknown_eight_byte_is_rejected() {
        // Anything at [4..8] that is not literally "OKRW" — including
        // the stale `b"OKRW    "[..8]` bug the fake reflector used to
        // emit — must be treated as a rejection. This guards against
        // a regression where the parser accepts near-misses.
        let pkt = *b"OKRW    ";
        let evt = parse_packet(&pkt).unwrap();
        assert!(
            matches!(evt, ReflectorEvent::Rejected),
            "non-OKRW 8-byte reply should classify as Rejected, got {evt:?}"
        );
    }

    #[test]
    fn parse_eleven_byte_input_returns_none() {
        // Per ircDDBGateway/Common/DPlusProtocolHandler.cpp:131-145,
        // DPlus does not use 11-byte non-DSVT packets. Verify we
        // correctly classify random 11-byte input as unknown.
        let data = [0x00; 11];
        assert!(parse_packet(&data).is_none());
    }

    #[test]
    fn parse_poll_echo() {
        let pkt = build_poll();
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
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
    fn header_roundtrip() {
        let pkt = build_header(&test_header(), sid(0xABCD));
        assert_eq!(pkt.len(), 58);
        assert_eq!(pkt[0], 0x3A);
        assert_eq!(pkt[1], 0x80);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceStart { header, stream_id } => {
                assert_eq!(stream_id.get(), 0xABCD); // LE roundtrip
                assert_eq!(header.my_call.as_bytes(), b"W1AW    ");
            }
            other => panic!("expected VoiceStart, got {other:?}"),
        }
    }

    #[test]
    fn dplus_build_header_zeros_flag_bytes() {
        // Per ircDDBGateway/Common/HeaderData.cpp:615-617, the DSVT
        // encoding zeroes flag1/flag2/flag3 before CRC computation.
        // The DPlus packet layout is:
        //   [0]      0x3A DPlus prefix (length)
        //   [1]      0x80 DPlus type
        //   [2..6]   "DSVT"
        //   [6]      0x10 header flag
        //   [7..10]  reserved
        //   [10]     0x20 config
        //   [11..14] band1 band2 band3
        //   [14..16] stream_id LE
        //   [16]     0x80 header indicator
        //   [17..58] DStarHeader::encode_for_dsvt() output (41 bytes)
        // Flag bytes land at [17], [18], [19].
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
        assert_eq!(pkt.len(), 58);
        assert_eq!(pkt[17], 0, "flag1 must be zeroed (was {:#04x})", pkt[17]);
        assert_eq!(pkt[18], 0, "flag2 must be zeroed (was {:#04x})", pkt[18]);
        assert_eq!(pkt[19], 0, "flag3 must be zeroed (was {:#04x})", pkt[19]);
    }

    #[test]
    fn voice_roundtrip() {
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let pkt = build_voice(sid(0x5678), 7, &frame);
        assert_eq!(pkt.len(), 29);
        assert_eq!(pkt[0], 0x1D);
        assert_eq!(pkt[1], 0x80);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceData { stream_id, seq, .. } => {
                assert_eq!(stream_id.get(), 0x5678);
                assert_eq!(seq, 7);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn eot_is_32_bytes() {
        let pkt = build_eot(sid(0x1234), 0);
        assert_eq!(
            pkt.len(),
            32,
            "DPlus EOT must be 32 bytes per xlxd cdplusprotocol.cpp:507 IsValidDvLastFramePacket"
        );
    }

    #[test]
    fn eot_has_dplus_prefix_0x20() {
        let pkt = build_eot(sid(0x1234), 0);
        assert_eq!(
            pkt[0], 0x20,
            "DPlus EOT prefix byte must be 0x20 (distinct from voice's 0x1D)"
        );
        assert_eq!(pkt[1], 0x80, "DPlus type byte must be 0x80");
    }

    #[test]
    fn eot_ends_with_reference_end_pattern() {
        let pkt = build_eot(sid(0x1234), 0);
        // Per ircDDBGateway/Common/AMBEData.cpp:380-388, the last 6
        // bytes form the end pattern 0x55 0x55 0x55 0x55 0xC8 0x7A.
        // Offsets [26..32] in the 32-byte packet.
        assert_eq!(&pkt[26..32], &[0x55, 0x55, 0x55, 0x55, 0xC8, 0x7A]);
    }

    #[test]
    fn eot_preserves_stream_id_and_eot_seq_bit() {
        let pkt = build_eot(sid(0xABCD), 5);
        // Stream ID LE at offsets [14..16]
        assert_eq!(pkt[14], 0xCD);
        assert_eq!(pkt[15], 0xAB);
        // Seq byte at offset 16 with EOT bit 0x40 set
        assert_eq!(pkt[16] & 0x40, 0x40, "EOT bit set");
        assert_eq!(pkt[16] & 0x3F, 5, "low bits preserve seq value");
    }

    #[test]
    fn eot_has_ambe_silence_at_offset_17() {
        let pkt = build_eot(sid(0x1234), 0);
        assert_eq!(&pkt[17..26], &AMBE_SILENCE);
    }

    #[test]
    fn eot_matches_reference_golden_vector() {
        use crate::protocol::reference_vectors::DPLUS_EOT_STREAM_1234_SEQ_0;
        let pkt = build_eot(sid(0x1234), 0);
        assert_eq!(pkt.as_slice(), DPLUS_EOT_STREAM_1234_SEQ_0.as_slice());
    }

    #[test]
    fn parse_two_byte_packet_does_not_panic() {
        // Regression test for the bounds bug proptest discovered.
        // parse_packet must not index data[2] without checking
        // data.len() >= 3 first.
        let data = [0x00, 0x00];
        let result = parse_packet(&data);
        assert!(result.is_none());
    }

    #[test]
    fn parse_empty_packet_does_not_panic() {
        let result = parse_packet(&[]);
        assert!(result.is_none());
    }

    #[test]
    fn parse_single_byte_packet_does_not_panic() {
        let result = parse_packet(&[0x00]);
        assert!(result.is_none());
    }

    proptest::proptest! {
        #[test]
        fn parse_never_panics(data in proptest::collection::vec(proptest::num::u8::ANY, 0..2048)) {
            let _ = parse_packet(&data);
        }
    }

    /// Build one `DPlus` auth chunk that wraps the given host records.
    ///
    /// Matches the on-wire framing emitted by the reference
    /// implementation: 2-byte length, flag byte (0xC0 high nibble),
    /// type byte 0x01, 5 bytes of chunk-header filler, then the
    /// packed records.
    fn build_chunk(records: &[[u8; 26]]) -> Vec<u8> {
        let body_len = 8 + records.len() * 26;
        assert!(body_len <= 0x0FFF, "test chunk too large");
        let mut chunk = Vec::with_capacity(body_len);
        #[allow(clippy::cast_possible_truncation)]
        let lo = (body_len & 0xFF) as u8;
        #[allow(clippy::cast_possible_truncation)]
        let hi = ((body_len >> 8) & 0x0F) as u8;
        chunk.push(lo);
        chunk.push(0xC0 | hi);
        chunk.push(0x01);
        // 5 bytes of opaque header filler.
        chunk.extend_from_slice(&[0u8; 5]);
        for r in records {
            chunk.extend_from_slice(r);
        }
        assert_eq!(chunk.len(), body_len);
        chunk
    }

    /// Build a single 26-byte host record: space-padded ASCII IP,
    /// space-padded ASCII callsign, module byte 0, active bit set.
    fn build_record(ip: &str, call: &str) -> [u8; 26] {
        let mut rec = [b' '; 26];
        let ip_bytes = ip.as_bytes();
        assert!(ip_bytes.len() <= 16);
        rec[..ip_bytes.len()].copy_from_slice(ip_bytes);
        let cs_bytes = call.as_bytes();
        assert!(cs_bytes.len() <= 8);
        rec[16..16 + cs_bytes.len()].copy_from_slice(cs_bytes);
        rec[24] = 0; // module/id placeholder
        rec[25] = 0x80; // active flag
        rec
    }

    #[test]
    fn parse_auth_response_empty_is_empty() {
        // Empty input is a valid "no chunks" response.
        let data: Vec<u8> = Vec::new();
        let list = parse_auth_response(&data).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn parse_auth_response_empty_chunk_is_empty() {
        // Chunk with no records is valid and yields zero hosts.
        let data = build_chunk(&[]);
        let list = parse_auth_response(&data).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn parse_auth_response_one_record() {
        let rec = build_record("192.168.1.1", "REF030");
        let data = build_chunk(&[rec]);

        let list = parse_auth_response(&data).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list.hosts()[0].callsign, "REF030");
        assert_eq!(
            list.hosts()[0].address,
            "192.168.1.1".parse::<std::net::IpAddr>().unwrap()
        );
    }

    #[test]
    fn parse_auth_response_three_records_case_insensitive_find() {
        let recs = [
            build_record("10.0.0.1", "REF001"),
            build_record("10.0.0.2", "REF030"),
            build_record("10.0.0.3", "REF100"),
        ];
        let data = build_chunk(&recs);
        let list = parse_auth_response(&data).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list.find("ref030").unwrap().callsign, "REF030");
        assert_eq!(list.find("REF100").unwrap().callsign, "REF100");
        assert!(list.find("XRF999").is_none());
    }

    #[test]
    fn parse_auth_response_multi_chunk() {
        // Real servers split large host lists across multiple
        // framed chunks; we must walk the full stream, not just
        // the first chunk.
        let mut data = build_chunk(&[build_record("10.0.0.1", "REF001")]);
        data.extend_from_slice(&build_chunk(&[
            build_record("10.0.0.2", "REF002"),
            build_record("10.0.0.3", "REF003"),
        ]));

        let list = parse_auth_response(&data).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list.hosts()[0].callsign, "REF001");
        assert_eq!(list.hosts()[2].callsign, "REF003");
    }

    #[test]
    fn parse_auth_response_skips_inactive_and_xrf() {
        // XRF records are filtered out; inactive records (bit 0x80
        // clear in byte 25) are also dropped.
        let mut inactive = build_record("10.0.0.5", "REF005");
        inactive[25] = 0x00;
        let recs = [
            build_record("10.0.0.1", "REF001"),
            build_record("10.0.0.2", "XRF002"),
            inactive,
            build_record("10.0.0.9", "REF009"),
        ];
        let data = build_chunk(&recs);
        let list = parse_auth_response(&data).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list.hosts()[0].callsign, "REF001");
        assert_eq!(list.hosts()[1].callsign, "REF009");
    }

    #[test]
    fn parse_auth_response_invalid_type_byte_rejected() {
        let mut data = build_chunk(&[build_record("10.0.0.1", "REF001")]);
        data[2] = 0x02; // expected 0x01
        let err = parse_auth_response(&data).unwrap_err();
        assert!(matches!(err, Error::AuthResponseInvalid(_)));
    }

    #[test]
    fn parse_auth_response_truncated_chunk_body_rejected() {
        let full = build_chunk(&[build_record("10.0.0.1", "REF001")]);
        // Drop the last record byte — header still claims full length.
        let truncated = &full[..full.len() - 1];
        let err = parse_auth_response(truncated).unwrap_err();
        assert!(matches!(err, Error::AuthResponseInvalid(_)));
    }

    #[test]
    fn parse_auth_response_malformed_ip_is_rejected() {
        let mut rec = [b' '; 26];
        rec[..7].copy_from_slice(b"bogus!!");
        rec[16..22].copy_from_slice(b"REF030");
        rec[25] = 0x80;
        let data = build_chunk(&[rec]);
        let err = parse_auth_response(&data).unwrap_err();
        assert!(matches!(err, Error::AuthResponseInvalid(_)));
    }

    #[tokio::test]
    async fn dplus_connect_and_wait_times_out_without_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // No responder — client should time out.
        let _keep = listener;
        let mut client = DPlusClient::new(cs("W1AW"), m('B'), addr).await.unwrap();
        let result = client.connect_and_wait(Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(Error::ConnectTimeout(_))),
            "expected ConnectTimeout, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dplus_connect_and_wait_succeeds_on_two_step_ack() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Fake two-step ACK responder: reply with step1 ACK (5 bytes),
        // then on receipt of step2 send the step2 ACK (28-byte LINK2
        // form which the parser classifies as Connected).
        let _responder = tokio::spawn(async move {
            let mut buf = [0u8; 128];

            // Wait for step1 connect packet.
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            // Echo the 5-byte step1 ACK.
            let step1_ack = build_connect_step1();
            let _ = listener.send_to(&step1_ack, src).await.unwrap();

            // Wait for step2 login packet.
            let (_, src) = listener.recv_from(&mut buf).await.unwrap();
            // Echo a 28-byte step2 ACK (any 28-byte non-DSVT payload
            // is classified as Connected by the parser).
            let step2_ack = build_connect_step2(&Callsign::try_from_str("W1AW").unwrap());
            let _ = listener.send_to(&step2_ack, src).await.unwrap();
        });

        let mut client = DPlusClient::new(cs("W1AW"), m('B'), addr).await.unwrap();
        let result = client.connect_and_wait(Duration::from_secs(2)).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(client.state(), ConnectionState::Connected);
    }

    /// `DPlus` has two distinct rejection modes: explicit NAK and silence.
    ///
    /// This test covers the *silence* path — a reflector that just
    /// stops responding after receiving the LINK1 (e.g. an overloaded
    /// host, dropped packets, or a firewall black-holing the client).
    /// The observable signal is a `ConnectTimeout` once the configured
    /// timeout elapses, not `Error::Rejected`.
    ///
    /// The explicit-NAK path (BUSY/FAIL on the 8-byte LINK2 reply) is
    /// covered by the
    /// `dplus_connect_and_wait_returns_rejected_on_nak`
    /// integration test in `tests/dplus_integration.rs`, which uses
    /// `FakeReflector::spawn_dplus_rejecting` to drive the
    /// `parse_packet` OKRW-magic check added in F3. That path
    /// surfaces as `Error::Rejected`.
    ///
    /// Reference: `ref/ircDDBGateway/Common/ConnectData.cpp:251-259`
    /// distinguishes `OKRW` (`CT_ACK`) from anything else (`CT_NAK`)
    /// on the 8-byte reply; `ref/xlxd/src/cdplusprotocol.cpp:535-544`
    /// shows xlxd emitting `BUSY` on refused logins. Reflectors that
    /// drop the callsign entirely fall through to this silence path
    /// (`ref/ircDDBGateway/Common/DPlusHandler.cpp:773`
    /// "failed to connect").
    #[tokio::test]
    async fn dplus_connect_and_wait_rejects_via_silence() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Drain the connect packet but never reply — simulating a REF
        // reflector that has decided to ignore us.
        let _drain = tokio::spawn(async move {
            let mut buf = [0u8; 128];
            let _ = listener.recv_from(&mut buf).await;
            // Hold the listener so the OS does not free the port and
            // turn the next send into ICMP unreachable.
            let _keep = listener;
            tokio::time::sleep(Duration::from_secs(5)).await;
        });

        let mut client = DPlusClient::new(cs("W1AW"), m('B'), addr).await.unwrap();
        let result = client.connect_and_wait(Duration::from_millis(300)).await;
        assert!(
            matches!(result, Err(Error::ConnectTimeout(_))),
            "expected ConnectTimeout (silent rejection), got {result:?}"
        );
    }

    #[tokio::test]
    async fn dplus_set_poll_interval_stored() {
        let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = DPlusClient::new(cs("W1AW"), m('B'), addr).await.unwrap();
        assert_eq!(client.poll_interval, POLL_INTERVAL);
        let new_interval = Duration::from_millis(1250);
        client.set_poll_interval(new_interval);
        assert_eq!(client.poll_interval, new_interval);
    }

    /// Spin up a `DPlusClient` wired to a loopback UDP server that we
    /// control. Returns the client (pre-bound to loopback and flipped
    /// to `Connected`) plus the server socket and the client's local
    /// address so the test can inject packets at it.
    async fn loopback_connected_client() -> (DPlusClient, UdpSocket, SocketAddr) {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let mut client = DPlusClient::new(cs("W1AW"), m('B'), server_addr)
            .await
            .unwrap();
        // `DPlusClient::new` binds 0.0.0.0 which doesn't route; rebind
        // to loopback for the test so `server.send_to(client_addr)` works.
        client.socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client.socket.local_addr().unwrap();
        client.state = ConnectionState::Connected;
        (client, server, client_addr)
    }

    #[tokio::test]
    async fn dplus_suppresses_mid_stream_header_refresh() {
        // Regression for C2: DPlus reflectors retransmit the 58-byte
        // DSVT voice header every superframe (~420 ms). Before this
        // fix, `parse_packet` emitted a fresh `VoiceStart` event for
        // each retransmission, causing the REPL to re-announce the
        // stream and re-send an MMDVM header to the radio every
        // superframe. The `last_rx_stream_id` tracker in
        // `DPlusClient::poll` must suppress every `VoiceStart` after
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
        // `poll` should return `Ok(None)` rather than a spurious
        // VoiceStart, and subsequent polls should time out on the
        // empty socket (also Ok(None)).
        let _n = server.send_to(&pkt, client_addr).await.unwrap();
        let evt2 = client.poll().await.unwrap();
        assert!(
            evt2.is_none(),
            "mid-stream header refresh must be suppressed, got {evt2:?}"
        );
    }

    #[tokio::test]
    async fn dplus_emits_fresh_voice_start_on_new_stream_id() {
        // After a real stream change (different stream_id), the
        // tracker must let the new VoiceStart through. Without this
        // path, back-to-back transmissions from two different
        // operators would look like a single endless stream.
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

        // Different stream_id — must fire a fresh VoiceStart even
        // though no VoiceEnd was observed in between (real reflectors
        // sometimes lose the EOT frame or splice streams abruptly).
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
    async fn dplus_voice_end_clears_stream_tracking() {
        // After a VoiceEnd clears `last_rx_stream_id`, any subsequent
        // VoiceStart — even with the same stream_id as the just-ended
        // stream — must fire. This covers the edge case of two
        // consecutive transmissions that happen to reuse the same
        // 16-bit stream_id (birthday-paradox territory on long sessions).
        let (mut client, server, client_addr) = loopback_connected_client().await;

        let hdr_pkt = build_header(&test_header(), sid(0xABCD));
        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt1 = client.poll().await.unwrap();
        assert!(
            matches!(evt1, Some(ReflectorEvent::VoiceStart { .. })),
            "first VoiceStart must fire, got {evt1:?}"
        );

        // Send the canonical 32-byte DPlus EOT for the same stream.
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
    async fn dplus_mid_stream_join_still_fires_voice_start_on_next_header() {
        // Regression for the "sometimes the D-STAR popup never appears"
        // bug observed live on REF030C. If the first packet we see for
        // a new stream happens to be `VoiceData` (e.g. we connect mid
        // transmission, or the bounded poll drain in the REPL picks up
        // the stream in the middle of a superframe), the `VoiceData`
        // arm of `poll` must NOT update `last_rx_stream_id`. Otherwise
        // the next superframe header refresh gets suppressed and the
        // REPL never receives a `VoiceStart` to announce the stream.
        let (mut client, server, client_addr) = loopback_connected_client().await;

        // Build a voice data packet for stream 0xBEEF and send it first
        // (before any header), simulating a mid-stream join.
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

        // Now the header for the SAME stream arrives (next superframe
        // refresh from the reflector). This MUST fire a real
        // VoiceStart — before the fix, the VoiceData arm had already
        // cached the stream_id and the VoiceStart arm suppressed it.
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

        // And a SECOND header (mid-stream refresh after the real one)
        // must still be suppressed — otherwise we'd regress C2.
        let _n = server.send_to(&hdr_pkt, client_addr).await.unwrap();
        let evt_refresh = client.poll().await.unwrap();
        assert!(
            evt_refresh.is_none(),
            "second header for known stream must be suppressed, got {evt_refresh:?}"
        );
    }
}
