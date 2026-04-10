//! `DPlus` protocol (REF reflectors, UDP port 20001).
//!
//! The most common D-STAR reflector protocol. Requires TCP
//! authentication before linking. Uses DSVT voice framing
//! (shared with `DExtra`).
//!
//! # Link sequence (UDP, per `g4klx/ircDDBGateway`)
//!
//! 1. Send connect (28 bytes)
//! 2. Receive connect ACK (28 bytes)
//! 3. Send link (11 bytes)
//! 4. Receive link ACK (11 bytes)
//! 5. Connected — keepalives every 5 seconds
//!
//! # Keepalive
//!
//! 3-byte poll: `0x03 0x60 0x00`, sent every 5 seconds.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

use crate::header::{self, DStarHeader};
use crate::voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

use super::{ConnectionState, ReflectorEvent};

/// Default `DPlus` port.
pub const DEFAULT_PORT: u16 = 20001;

/// Keepalive interval.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Connection timeout.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// DSVT magic.
const DSVT: &[u8; 4] = b"DSVT";

// ---------------------------------------------------------------------------
// Packet builders
// ---------------------------------------------------------------------------

/// Build a `DPlus` initial connect packet (5 bytes, step 1).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_LINK1`.
#[must_use]
pub fn build_connect_step1() -> Vec<u8> {
    vec![0x05, 0x00, 0x18, 0x00, 0x01]
}

/// Build a `DPlus` login packet (28 bytes, step 2).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_LINK2`:
/// bytes 4-11 = callsign (trimmed, zero-padded to 16),
/// bytes 20-27 = `"DV019999"`.
#[must_use]
pub fn build_connect_step2(callsign: &str) -> Vec<u8> {
    let mut pkt = vec![0u8; 28];
    pkt[0] = 0x1C;
    pkt[1] = 0xC0;
    pkt[2] = 0x04;
    pkt[3] = 0x00;
    // Callsign (trimmed, not space-padded — zero-fill rest).
    let cs = callsign.trim().as_bytes();
    let len = cs.len().min(8);
    pkt[4..4 + len].copy_from_slice(&cs[..len]);
    // DV client identifier at offset 20.
    pkt[20..28].copy_from_slice(b"DV019999");
    pkt
}

/// Build a `DPlus` link packet (11 bytes).
#[must_use]
pub fn build_link(callsign: &str, module: char) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(11);
    pkt.extend_from_slice(&DStarHeader::pad_callsign(callsign));
    pkt.push(module as u8);
    pkt.push(module as u8);
    pkt.push(0x0B);
    pkt
}

/// Build a `DPlus` disconnect packet (5 bytes).
///
/// Per `ircDDBGateway` `ConnectData::getDPlusData` `CT_UNLINK`.
#[must_use]
pub fn build_disconnect(_callsign: &str, _module: char) -> Vec<u8> {
    vec![0x05, 0x00, 0x18, 0x00, 0x00]
}

/// Build a `DPlus` keepalive packet (3 bytes).
#[must_use]
pub fn build_poll() -> Vec<u8> {
    vec![0x03, 0x60, 0x00]
}

/// Build a `DPlus` voice header (58 bytes: 2-byte prefix + DSVT).
///
/// Per `ircDDBGateway` `HeaderData::getDPlusData`:
/// `[0x3A, 0x80, "DSVT", 0x10, 0x00, 0x00, 0x00, 0x20,
///   band1, band2, band3, id_lo, id_hi, 0x80, flags[3], calls...]`
#[must_use]
pub fn build_header(header: &DStarHeader, stream_id: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(58);
    pkt.push(0x3A); // length = 58
    pkt.push(0x80); // type
    pkt.extend_from_slice(DSVT);
    pkt.push(0x10); // header flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.push(0x20); // config
    pkt.extend_from_slice(&[0x00, 0x01, 0x02]); // band1=0, band2=1, band3=2
    pkt.extend_from_slice(&stream_id.to_le_bytes()); // LE per reference
    pkt.push(0x80); // header indicator
    pkt.extend_from_slice(&header.encode());
    debug_assert_eq!(pkt.len(), 58);
    pkt
}

/// Build a `DPlus` voice data packet (29 bytes: 2-byte prefix + DSVT).
///
/// Per `ircDDBGateway` `AMBEData::getDPlusData`:
/// Normal: `[0x1D, 0x80, ...]`, EOT: `[0x20, 0x80, ...]`
#[must_use]
pub fn build_voice(stream_id: u16, seq: u8, frame: &VoiceFrame) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(29);
    pkt.push(0x1D); // length (normal voice)
    pkt.push(0x80); // type
    pkt.extend_from_slice(DSVT);
    pkt.push(0x20); // voice flag
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]); // reserved
    pkt.push(0x20); // config
    pkt.extend_from_slice(&[0x00, 0x01, 0x02]); // band1/2/3
    pkt.extend_from_slice(&stream_id.to_le_bytes()); // LE per reference
    pkt.push(seq);
    pkt.extend_from_slice(&frame.ambe);
    pkt.extend_from_slice(&frame.slow_data);
    debug_assert_eq!(pkt.len(), 29);
    pkt
}

/// Build a `DPlus` EOT packet (32 bytes: `[0x20, 0x80, ...]`).
///
/// Per reference: EOT uses `0x20` prefix byte (not `0x1D`) and the
/// last voice frame has seq bit 6 set.
#[must_use]
pub fn build_eot(stream_id: u16, seq: u8) -> Vec<u8> {
    let mut pkt = build_voice(
        stream_id,
        seq | 0x40,
        &VoiceFrame {
            ambe: AMBE_SILENCE,
            slow_data: DSTAR_SYNC_BYTES,
        },
    );
    // Override prefix byte from 0x1D to 0x20 for EOT.
    pkt[0] = 0x20;
    pkt
}

// ---------------------------------------------------------------------------
// Packet parser
// ---------------------------------------------------------------------------

/// Parse an incoming `DPlus` packet into a [`ReflectorEvent`].
#[must_use]
pub fn parse_packet(data: &[u8]) -> Option<ReflectorEvent> {
    // Per ircDDBGateway DPlusProtocolHandler::readPackets():
    // Non-DSVT packets of length 5, 8, or 28 are connect responses.
    // Length 3 is a poll echo. Length 11 is a link response.
    if data.len() >= 2 && (data[2] != b'D' || data.len() < 6 || &data[2..6] != b"DSVT") {
        match data.len() {
            5 | 8 | 28 => return Some(ReflectorEvent::Connected),
            11 => {
                return if data[10] == 0x0B {
                    Some(ReflectorEvent::Connected)
                } else {
                    Some(ReflectorEvent::Rejected)
                };
            }
            3 => return Some(ReflectorEvent::PollEcho),
            _ => return None,
        }
    }

    // DPlus DSVT packets have a 2-byte prefix: [len, 0x80] + "DSVT" + ...
    // Header: 58 bytes (0x3A 0x80 + 56-byte DSVT header)
    // Voice:  29 bytes (0x1D 0x80 + 27-byte DSVT voice)
    // Per ircDDBGateway: check data[2..6] == "DSVT"
    if data.len() >= 6 && &data[2..6] == DSVT {
        let dsvt = &data[2..]; // strip 2-byte DPlus prefix
        let is_header = dsvt[4] == 0x10;
        let stream_id = u16::from_le_bytes([dsvt[12], dsvt[13]]);

        if is_header && dsvt.len() >= 56 {
            let mut arr = [0u8; header::ENCODED_LEN];
            arr.copy_from_slice(&dsvt[15..56]);
            if let Some(hdr) = DStarHeader::decode(&arr) {
                return Some(ReflectorEvent::VoiceStart {
                    header: hdr,
                    stream_id,
                });
            }
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
/// single-step connect.
#[derive(Debug)]
pub struct DPlusClient {
    socket: UdpSocket,
    remote: SocketAddr,
    callsign: String,
    module: char,
    state: ConnectionState,
    login_sent: bool,
    link_sent: bool,
    last_poll_sent: Instant,
    last_poll_received: Instant,
}

impl DPlusClient {
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
            login_sent: false,
            link_sent: false,
            last_poll_sent: now,
            last_poll_received: now,
        })
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
    /// Returns an I/O error if the TCP connection or auth fails.
    pub async fn authenticate(&self) -> Result<(), std::io::Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        tracing::info!(callsign = %self.callsign, "DPlus TCP auth starting");

        let mut stream = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect("auth.dstargateway.org:20001"),
        )
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "auth connect timeout"))??;

        // Build 56-byte auth packet (per ircDDBGateway DPlusAuthenticator.cpp).
        let mut pkt = vec![b' '; 56];
        pkt[0] = 0x38;
        pkt[1] = 0xC0;
        pkt[2] = 0x01;
        pkt[3] = 0x00;
        let cs = DStarHeader::pad_callsign(&self.callsign);
        pkt[4..12].copy_from_slice(&cs);
        pkt[12..20].copy_from_slice(b"DV019999");
        pkt[28..33].copy_from_slice(b"W7IB2");
        pkt[40..47].copy_from_slice(b"DHS0257");

        stream.write_all(&pkt).await?;

        // Read and discard the response (host list).
        let mut buf = [0u8; 4096];
        let mut total = 0usize;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf)).await {
                Ok(Ok(0)) | Err(_) => break,
                Ok(Ok(n)) => total += n,
                Ok(Err(e)) => return Err(e),
            }
        }

        tracing::info!(bytes = total, "DPlus TCP auth complete");
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
    pub async fn connect(&mut self) -> Result<(), std::io::Error> {
        let pkt = build_connect_step1();
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Connecting;
        self.link_sent = false;
        self.login_sent = false;
        tracing::info!(
            reflector = %self.remote,
            module = %self.module,
            "DPlus connect sent"
        );
        Ok(())
    }

    /// Send the disconnect request.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the UDP send fails.
    pub async fn disconnect(&mut self) -> Result<(), std::io::Error> {
        let pkt = build_disconnect(&self.callsign, self.module);
        let _ = self.socket.send_to(&pkt, self.remote).await?;
        self.state = ConnectionState::Disconnecting;
        tracing::info!("DPlus disconnect sent");
        Ok(())
    }

    /// Poll for the next event. Handles the two-step connect
    /// (connect ACK → send link → link ACK) automatically.
    ///
    /// # Errors
    ///
    /// Returns an I/O error on socket failures.
    pub async fn poll(&mut self) -> Result<Option<ReflectorEvent>, std::io::Error> {
        // Send keepalive if connected.
        if self.state == ConnectionState::Connected
            && self.last_poll_sent.elapsed() >= POLL_INTERVAL
        {
            let pkt = build_poll();
            let _ = self.socket.send_to(&pkt, self.remote).await?;
            self.last_poll_sent = Instant::now();
        }

        // Check timeout.
        if self.state == ConnectionState::Connected
            && self.last_poll_received.elapsed() >= POLL_TIMEOUT
        {
            self.state = ConnectionState::Disconnected;
            return Ok(Some(ReflectorEvent::Disconnected));
        }

        // Receive.
        let mut buf = [0u8; 2048];
        let recv =
            tokio::time::timeout(Duration::from_millis(100), self.socket.recv_from(&mut buf)).await;

        let (len, _addr) = match recv {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e),
            Err(_) => return Ok(None),
        };

        let Some(event) = parse_packet(&buf[..len]) else {
            return Ok(None);
        };

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
                // Step 2 ACK (OKRW) → fully connected.
                self.state = ConnectionState::Connected;
                self.last_poll_received = Instant::now();
                tracing::info!("DPlus connected");
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
    fn connect_step1_format() {
        let pkt = build_connect_step1();
        assert_eq!(pkt, vec![0x05, 0x00, 0x18, 0x00, 0x01]);
    }

    #[test]
    fn connect_step2_format() {
        let pkt = build_connect_step2("W1AW");
        assert_eq!(pkt.len(), 28);
        assert_eq!(pkt[0], 0x1C);
        assert_eq!(pkt[1], 0xC0);
        assert_eq!(&pkt[4..8], b"W1AW");
        assert_eq!(&pkt[20..28], b"DV019999");
    }

    #[test]
    fn link_packet_format() {
        let pkt = build_link("W1AW", 'C');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'C');
        assert_eq!(pkt[9], b'C');
        assert_eq!(pkt[10], 0x0B);
    }

    #[test]
    fn disconnect_packet_format() {
        let pkt = build_disconnect("W1AW", 'C');
        assert_eq!(pkt, vec![0x05, 0x00, 0x18, 0x00, 0x00]);
    }

    #[test]
    fn poll_packet_format() {
        let pkt = build_poll();
        assert_eq!(pkt, vec![0x03, 0x60, 0x00]);
    }

    #[test]
    fn parse_step1_ack() {
        let pkt = build_connect_step1();
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_step2_ack() {
        let pkt = build_connect_step2("W1AW");
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_link_ack() {
        let pkt = build_link("W1AW", 'C');
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(evt, ReflectorEvent::Connected));
    }

    #[test]
    fn parse_poll_echo() {
        let pkt = build_poll();
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
        assert_eq!(pkt.len(), 58);
        assert_eq!(pkt[0], 0x3A);
        assert_eq!(pkt[1], 0x80);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceStart { header, stream_id } => {
                assert_eq!(stream_id, 0xABCD); // LE roundtrip
                assert_eq!(header.my_call, *b"W1AW    ");
            }
            other => panic!("expected VoiceStart, got {other:?}"),
        }
    }

    #[test]
    fn voice_roundtrip() {
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        let pkt = build_voice(0x5678, 7, &frame);
        assert_eq!(pkt.len(), 29);
        assert_eq!(pkt[0], 0x1D);
        assert_eq!(pkt[1], 0x80);
        let evt = parse_packet(&pkt).unwrap();
        match evt {
            ReflectorEvent::VoiceData { stream_id, seq, .. } => {
                assert_eq!(stream_id, 0x5678);
                assert_eq!(seq, 7);
            }
            other => panic!("expected VoiceData, got {other:?}"),
        }
    }

    #[test]
    fn eot_roundtrip() {
        let pkt = build_eot(0x5678, 3);
        assert_eq!(pkt.len(), 29); // same wire length, just different prefix byte
        assert_eq!(pkt[0], 0x20); // EOT prefix
        assert_eq!(pkt[1], 0x80);
        let evt = parse_packet(&pkt).unwrap();
        assert!(matches!(
            evt,
            ReflectorEvent::VoiceEnd { stream_id: 0x5678 }
        ));
    }
}
