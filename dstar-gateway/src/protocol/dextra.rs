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
//!   + stream_id[2 BE] + 0x80 + D-STAR header[41]
//!
//! Voice (27 bytes):
//!   "DSVT" + 0x20 + 3 reserved + 0x20 0x00 0x01 0x00
//!   + stream_id[2 BE] + seq + AMBE[9] + slow_data[3]
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

use crate::header::{self, DStarHeader};
use crate::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

use super::{ConnectionState, ReflectorEvent};

/// Default `DExtra` port.
pub const DEFAULT_PORT: u16 = 30001;

/// Keepalive interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Connection timeout (no poll echo received).
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// DSVT magic bytes.
const DSVT: &[u8; 4] = b"DSVT";

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a connect/link packet (11 bytes).
#[must_use]
pub fn build_connect(callsign: &str, module: char) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(11);
    pkt.extend_from_slice(&DStarHeader::pad_callsign(callsign));
    pkt.push(module as u8);
    pkt.push(module as u8);
    pkt.push(0x0B);
    pkt
}

/// Build a disconnect/unlink packet (11 bytes).
#[must_use]
pub fn build_disconnect(callsign: &str, module: char) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(11);
    pkt.extend_from_slice(&DStarHeader::pad_callsign(callsign));
    pkt.push(module as u8);
    pkt.push(b' ');
    pkt.push(0x00);
    pkt
}

/// Build a poll/keepalive packet (9 bytes).
#[must_use]
pub fn build_poll(callsign: &str) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(9);
    pkt.extend_from_slice(&DStarHeader::pad_callsign(callsign));
    pkt.push(0x00);
    pkt
}

/// Build a `DSVT` voice header packet (56 bytes).
#[must_use]
pub fn build_header(header: &DStarHeader, stream_id: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(56);
    pkt.extend_from_slice(DSVT);
    pkt.push(0x10); // header flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.extend_from_slice(&[0x20, 0x00, 0x01, 0x00]); // config
    pkt.extend_from_slice(&stream_id.to_be_bytes());
    pkt.push(0x80); // header indicator
    pkt.extend_from_slice(&header.encode());
    debug_assert_eq!(pkt.len(), 56);
    pkt
}

/// Build a `DSVT` voice data packet (27 bytes).
#[must_use]
pub fn build_voice(stream_id: u16, seq: u8, frame: &VoiceFrame) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(27);
    pkt.extend_from_slice(DSVT);
    pkt.push(0x20); // voice flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.extend_from_slice(&[0x20, 0x00, 0x01, 0x00]); // config
    pkt.extend_from_slice(&stream_id.to_be_bytes());
    pkt.push(seq);
    pkt.extend_from_slice(&frame.ambe);
    pkt.extend_from_slice(&frame.slow_data);
    debug_assert_eq!(pkt.len(), 27);
    pkt
}

/// Build a `DSVT` end-of-transmission packet (27 bytes).
#[must_use]
pub fn build_eot(stream_id: u16, seq: u8) -> Vec<u8> {
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
#[must_use]
pub fn parse_packet(data: &[u8]) -> Option<ReflectorEvent> {
    // Connect ACK: 11 bytes with 0x0B at byte 10 (classic DExtra),
    // or 14 bytes ending with "ACK\0" (XLX revision).
    if data.len() == 11 && data[10] == 0x0B {
        return Some(ReflectorEvent::Connected);
    }
    if data.len() == 14 && &data[10..14] == b"ACK\0" {
        return Some(ReflectorEvent::Connected);
    }

    // Connect NAK: 11 bytes with 0x00 at byte 10 (classic),
    // or 14 bytes ending with "NAK\0" (XLX).
    if data.len() == 11 && data[10] == 0x00 {
        return Some(ReflectorEvent::Rejected);
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
        let stream_id = u16::from_be_bytes([data[12], data[13]]);

        if is_header && data.len() == 56 {
            let mut arr = [0u8; header::ENCODED_LEN];
            arr.copy_from_slice(&data[15..56]);
            if let Some(header) = DStarHeader::decode(&arr) {
                return Some(ReflectorEvent::VoiceStart { header, stream_id });
            }
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
#[derive(Debug)]
pub struct DExtraClient {
    socket: UdpSocket,
    remote: SocketAddr,
    callsign: String,
    module: char,
    state: ConnectionState,
    last_poll_sent: Instant,
    last_poll_received: Instant,
}

impl DExtraClient {
    /// Create a new client and bind a local UDP socket.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the socket cannot be bound.
    pub async fn new(
        callsign: &str,
        module: char,
        remote: SocketAddr,
    ) -> Result<Self, std::io::Error> {
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let now = Instant::now();
        Ok(Self {
            socket,
            remote,
            callsign: callsign.to_owned(),
            module,
            state: ConnectionState::Disconnected,
            last_poll_sent: now,
            last_poll_received: now,
        })
    }

    /// Send the connect request to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn connect(&mut self) -> Result<(), std::io::Error> {
        let pkt = build_connect(&self.callsign, self.module);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Connecting;
        tracing::info!(
            reflector = %self.remote,
            module = %self.module,
            "DExtra connect sent"
        );
        Ok(())
    }

    /// Send the disconnect request to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn disconnect(&mut self) -> Result<(), std::io::Error> {
        let pkt = build_disconnect(&self.callsign, self.module);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Disconnecting;
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
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, std::io::Error> {
        // Send keepalive if needed.
        if self.state == ConnectionState::Connected
            && self.last_poll_sent.elapsed() >= POLL_INTERVAL
        {
            let pkt = build_poll(&self.callsign);
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            self.last_poll_sent = Instant::now();
        }

        // Check for connection timeout.
        if self.state == ConnectionState::Connected
            && self.last_poll_received.elapsed() >= POLL_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Receive with timeout.
        let mut buf = [0u8; 2048];
        let recv =
            tokio::time::timeout(Duration::from_millis(100), self.socket.recv_from(&mut buf)).await;

        let (len, _addr) = match recv {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(None), // timeout, no data
        };

        let Some(event) = parse_packet(&buf[..len]) else {
            return Ok(None);
        };

        // Update state based on event.
        match &event {
            ReflectorEvent::Connected => {
                self.state = ConnectionState::Connected;
                self.last_poll_received = Instant::now();
            }
            ReflectorEvent::Rejected | ReflectorEvent::Disconnected => {
                self.state = ConnectionState::Disconnected;
            }
            ReflectorEvent::PollEcho => {
                self.last_poll_received = Instant::now();
            }
            _ => {}
        }

        Ok(Some(event))
    }

    /// Send a voice header to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_header(
        &self,
        header: &DStarHeader,
        stream_id: u16,
    ) -> Result<(), std::io::Error> {
        let pkt = build_header(header, stream_id);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        Ok(())
    }

    /// Send a voice data frame to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_voice(
        &self,
        stream_id: u16,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), std::io::Error> {
        let pkt = build_voice(stream_id, seq, frame);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        Ok(())
    }

    /// Send an end-of-transmission to the reflector.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    pub async fn send_eot(&self, stream_id: u16, seq: u8) -> Result<(), std::io::Error> {
        let pkt = build_eot(stream_id, seq);
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

    #[test]
    fn connect_packet_format() {
        let pkt = build_connect("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b'A');
        assert_eq!(pkt[10], 0x0B);
    }

    #[test]
    fn disconnect_packet_format() {
        let pkt = build_disconnect("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b' ');
        assert_eq!(pkt[10], 0x00);
    }

    #[test]
    fn poll_packet_format() {
        let pkt = build_poll("W1AW");
        assert_eq!(pkt.len(), 9);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
    }

    #[test]
    fn header_packet_size() {
        let hdr = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: *b"REF030 G",
            rpt1: *b"REF030 C",
            ur_call: *b"CQCQCQ  ",
            my_call: *b"W1AW    ",
            my_suffix: *b"    ",
        };
        let pkt = build_header(&hdr, 0x1234);
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
        let pkt = build_voice(0x1234, 5, &frame);
        assert_eq!(pkt.len(), 27);
        assert_eq!(&pkt[0..4], b"DSVT");
        assert_eq!(pkt[4], 0x20);
    }

    #[test]
    fn eot_has_flag_set() {
        let pkt = build_eot(0x1234, 3);
        assert_eq!(pkt[14] & 0x40, 0x40);
        assert_eq!(&pkt[15..24], &AMBE_SILENCE);
        assert_eq!(&pkt[24..27], &DSTAR_SYNC_BYTES);
    }

    #[test]
    fn parse_connect_ack() {
        let pkt = build_connect("W1AW", 'A');
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_disconnect() {
        let pkt = build_disconnect("W1AW", 'A');
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Rejected));
    }

    #[test]
    fn parse_poll_echo() {
        let pkt = build_poll("W1AW");
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::PollEcho));
    }

    #[test]
    fn header_roundtrip() {
        let hdr = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: *b"REF030 G",
            rpt1: *b"REF030 C",
            ur_call: *b"CQCQCQ  ",
            my_call: *b"W1AW    ",
            my_suffix: *b"    ",
        };
        let pkt = build_header(&hdr, 0xABCD);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceStart { header, stream_id } => {
                assert_eq!(stream_id, 0xABCD);
                assert_eq!(header.my_call, *b"W1AW    ");
                assert_eq!(header.rpt1, *b"REF030 C");
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
        let pkt = build_voice(0x5678, 7, &frame);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceData {
                stream_id,
                seq,
                frame: f,
            } => {
                assert_eq!(stream_id, 0x5678);
                assert_eq!(seq, 7);
                assert_eq!(f, frame);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn eot_roundtrip() {
        let pkt = build_eot(0x5678, 3);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceEnd { stream_id } => {
                assert_eq!(stream_id, 0x5678);
            }
            other => panic!("expected VoiceEnd, got {other:?}"),
        }
    }

    #[test]
    fn garbage_returns_none() {
        assert!(parse_packet(&[]).is_none());
        assert!(parse_packet(&[0xFF; 5]).is_none());
    }
}
