//! D-STAR reflector protocol packet builders and parsers.
//!
//! Implements the three standard D-STAR reflector protocols: `DExtra` (XRF),
//! DCS, and `DPlus` (REF). These are pure functions that build and parse UDP
//! payloads --- no networking code is included. The caller is responsible
//! for sending/receiving these payloads via their own UDP socket.
//!
//! Protocol packet formats are based on the open-source xlxd and `MMDVMHost`
//! implementations (both GPL-2.0-or-later).

use super::dstar::DStarHeader;

/// AMBE silence frame (9 bytes) --- used in EOT packets.
pub const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

/// D-STAR sync bytes (3 bytes) --- slow data filler for EOT.
pub const DSTAR_SYNC_BYTES: [u8; 3] = [0x55, 0x55, 0x55];

/// DSVT magic header bytes.
const DSVT_MAGIC: &[u8; 4] = b"DSVT";

/// Pad a callsign to exactly 8 characters (space-padded on the right).
fn pad_callsign(callsign: &str) -> [u8; 8] {
    let mut buf = [b' '; 8];
    let bytes = callsign.as_bytes();
    let len = bytes.len().min(8);
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// Build a DSVT header packet (shared by `DExtra` and `DPlus`).
///
/// Returns 56 bytes: `"DSVT" + 0x10 + config/padding + stream_id + 0x80 + header[41]`.
fn build_dsvt_header(header: &DStarHeader, stream_id: u16) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(56);
    pkt.extend_from_slice(DSVT_MAGIC);
    pkt.push(0x10); // header flag
    // 3 bytes reserved/flags.
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]);
    // 2 bytes band/protocol config.
    pkt.extend_from_slice(&[0x20, 0x00]);
    // 2 bytes reserved.
    pkt.extend_from_slice(&[0x01, 0x00]);
    // Stream ID (big-endian).
    pkt.extend_from_slice(&stream_id.to_be_bytes());
    // Counter byte with header indicator.
    pkt.push(0x80);
    // D-STAR header (41 bytes with CRC).
    pkt.extend_from_slice(&header.encode());
    debug_assert_eq!(pkt.len(), 56);
    pkt
}

/// Build a DSVT voice data packet (shared by `DExtra` and `DPlus`).
///
/// Returns 27 bytes: `"DSVT" + 0x20 + config/padding + stream_id + seq + ambe[9] + slow[3]`.
fn build_dsvt_voice(stream_id: u16, seq: u8, ambe: [u8; 9], slow_data: [u8; 3]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(27);
    pkt.extend_from_slice(DSVT_MAGIC);
    pkt.push(0x20); // voice flag
    // 3 bytes reserved/flags.
    pkt.extend_from_slice(&[0x00, 0x00, 0x00]);
    // 2 bytes band/protocol config.
    pkt.extend_from_slice(&[0x20, 0x00]);
    // 2 bytes reserved.
    pkt.extend_from_slice(&[0x01, 0x00]);
    // Stream ID (big-endian).
    pkt.extend_from_slice(&stream_id.to_be_bytes());
    // Sequence byte.
    pkt.push(seq);
    // AMBE voice data (9 bytes).
    pkt.extend_from_slice(&ambe);
    // Slow data (3 bytes).
    pkt.extend_from_slice(&slow_data);
    debug_assert_eq!(pkt.len(), 27);
    pkt
}

/// Build a DSVT EOT packet (shared by `DExtra` and `DPlus`).
///
/// Returns 27 bytes. Same as voice but seq has bit 6 set (0x40), AMBE is
/// silence, and slow data is sync bytes.
fn build_dsvt_eot(stream_id: u16, seq: u8) -> Vec<u8> {
    build_dsvt_voice(stream_id, seq | 0x40, AMBE_SILENCE, DSTAR_SYNC_BYTES)
}

/// Parse a DSVT packet, returning `(is_header, stream_id, payload_after_stream_id)`.
///
/// Returns `None` if the data doesn't start with "DSVT" or is too short.
fn parse_dsvt(data: &[u8]) -> Option<(bool, u16, &[u8])> {
    if data.len() < 17 || &data[0..4] != DSVT_MAGIC {
        return None;
    }
    let is_header = data[4] == 0x10;
    let stream_id = u16::from_be_bytes([data[12], data[13]]);
    Some((is_header, stream_id, &data[14..]))
}

/// `DExtra` protocol packet builders and parsers (XRF reflectors, port 30001).
pub mod dextra {
    use super::{
        DStarHeader, build_dsvt_eot, build_dsvt_header, build_dsvt_voice, pad_callsign, parse_dsvt,
    };

    /// Poll/keepalive interval in milliseconds.
    pub const POLL_INTERVAL_MS: u32 = 3000;

    /// Events received from a `DExtra` reflector.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DExtraEvent {
        /// Reflector acknowledged the connection.
        ConnectAck,
        /// Reflector rejected the connection.
        ConnectNak,
        /// Echo response to a poll/keepalive.
        PollEcho,
        /// Incoming voice stream header.
        Header {
            /// The D-STAR radio header.
            header: DStarHeader,
            /// Stream identifier.
            stream_id: u16,
        },
        /// Incoming voice data frame.
        Voice {
            /// Stream identifier.
            stream_id: u16,
            /// Sequence number (0--20 cycle).
            seq: u8,
            /// AMBE-encoded voice data.
            ambe: [u8; 9],
            /// Slow data payload.
            slow_data: [u8; 3],
        },
        /// End of transmission marker.
        Eot {
            /// Stream identifier.
            stream_id: u16,
        },
    }

    /// Build a connect packet (11 bytes).
    ///
    /// Format: `callsign[8] + module + module + 0x0B`.
    #[must_use]
    pub fn build_connect(callsign: &str, module: char) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(11);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(module as u8);
        pkt.push(module as u8);
        pkt.push(0x0B);
        pkt
    }

    /// Build a disconnect packet (11 bytes).
    ///
    /// Format: `callsign[8] + module + 0x20 + 0x00`.
    #[must_use]
    pub fn build_disconnect(callsign: &str, module: char) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(11);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(module as u8);
        pkt.push(b' ');
        pkt.push(0x00);
        pkt
    }

    /// Build a poll/keepalive packet (9 bytes).
    ///
    /// Format: `callsign[8] + 0x00`.
    #[must_use]
    pub fn build_poll(callsign: &str) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(9);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(0x00);
        pkt
    }

    /// Build a voice header packet (56 bytes).
    ///
    /// Format: `"DSVT" + 0x10 + padding + config + stream_id + 0x80 + DStarHeader[41]`.
    #[must_use]
    pub fn build_header(header: &DStarHeader, stream_id: u16) -> Vec<u8> {
        build_dsvt_header(header, stream_id)
    }

    /// Build a voice data packet (27 bytes).
    ///
    /// Format: `"DSVT" + 0x20 + padding + config + stream_id + seq + ambe[9] + slow[3]`.
    #[must_use]
    pub fn build_voice(stream_id: u16, seq: u8, ambe: &[u8; 9], slow_data: &[u8; 3]) -> Vec<u8> {
        build_dsvt_voice(stream_id, seq, *ambe, *slow_data)
    }

    /// Build an EOT packet (27 bytes).
    ///
    /// Same as voice but seq has bit 6 set, AMBE is silence, slow data is sync.
    #[must_use]
    pub fn build_eot(stream_id: u16, seq: u8) -> Vec<u8> {
        build_dsvt_eot(stream_id, seq)
    }

    /// Parse an incoming `DExtra` packet.
    ///
    /// Returns `None` if the packet format is not recognized.
    #[must_use]
    pub fn parse_packet(data: &[u8]) -> Option<DExtraEvent> {
        // Connect ACK/NAK: 11 bytes, byte 10 is 0x0B (ack) or 0x00 (nak).
        if data.len() == 11 {
            return match data[10] {
                0x0B => Some(DExtraEvent::ConnectAck),
                0x00 => Some(DExtraEvent::ConnectNak),
                _ => None,
            };
        }

        // Poll echo: 9 bytes ending with 0x00.
        if data.len() == 9 && data[8] == 0x00 {
            return Some(DExtraEvent::PollEcho);
        }

        // DSVT-based packets (header or voice).
        if let Some((is_header, stream_id, payload)) = parse_dsvt(data) {
            if is_header && data.len() == 56 {
                // payload starts after stream_id byte 12; byte 12 is 0x80 counter.
                // Header is bytes 13..54 (41 bytes).
                if payload.len() >= 42 {
                    let header_bytes: &[u8] = &payload[1..42];
                    let mut arr = [0u8; 41];
                    arr.copy_from_slice(header_bytes);
                    if let Ok(header) = DStarHeader::decode(&arr) {
                        return Some(DExtraEvent::Header { header, stream_id });
                    }
                }
            } else if !is_header && data.len() == 27 && payload.len() >= 13 {
                let seq = payload[0];
                let mut ambe = [0u8; 9];
                ambe.copy_from_slice(&payload[1..10]);
                let mut slow_data = [0u8; 3];
                slow_data.copy_from_slice(&payload[10..13]);

                // EOT: bit 6 of seq is set.
                if seq & 0x40 != 0 {
                    return Some(DExtraEvent::Eot { stream_id });
                }
                return Some(DExtraEvent::Voice {
                    stream_id,
                    seq,
                    ambe,
                    slow_data,
                });
            }
        }

        None
    }
}

/// DCS protocol packet builders and parsers (DCS reflectors, port 30051).
pub mod dcs {
    use super::{AMBE_SILENCE, DSTAR_SYNC_BYTES, DStarHeader, pad_callsign};

    /// Poll/keepalive interval in milliseconds.
    pub const POLL_INTERVAL_MS: u32 = 2000;

    /// Events received from a DCS reflector.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DCSEvent {
        /// Reflector acknowledged the connection.
        ConnectAck,
        /// Reflector rejected the connection.
        ConnectNak,
        /// Echo response to a poll/keepalive.
        PollEcho,
        /// Incoming voice frame (DCS embeds the header in every frame).
        Voice {
            /// The D-STAR radio header embedded in the frame.
            header: DStarHeader,
            /// Sequence number.
            seq: u8,
            /// AMBE-encoded voice data.
            ambe: [u8; 9],
            /// Slow data payload.
            slow_data: [u8; 3],
            /// Transmission sequence counter.
            tx_sequence: u32,
        },
        /// End of transmission marker.
        Eot {
            /// The D-STAR radio header embedded in the frame.
            header: DStarHeader,
            /// Sequence number.
            seq: u8,
            /// Transmission sequence counter.
            tx_sequence: u32,
        },
    }

    /// Build a connect packet (519 bytes).
    ///
    /// Format: `callsign[8] + local_module + remote_module + 0x0B + zeros[508]`.
    #[must_use]
    pub fn build_connect(callsign: &str, local_module: char, remote_module: char) -> Vec<u8> {
        let mut pkt = vec![0u8; 519];
        pkt[..8].copy_from_slice(&pad_callsign(callsign));
        pkt[8] = local_module as u8;
        pkt[9] = remote_module as u8;
        pkt[10] = 0x0B;
        // Remaining 508 bytes are zero-filled.
        pkt
    }

    /// Build a disconnect packet (19 bytes).
    ///
    /// Format: `callsign[8] + local_module + 0x20 + 0x00 + reflector_name[8]`.
    #[must_use]
    pub fn build_disconnect(callsign: &str, local_module: char, reflector_name: &str) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(19);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(local_module as u8);
        pkt.push(b' ');
        pkt.push(0x00);
        pkt.extend_from_slice(&pad_callsign(reflector_name));
        pkt
    }

    /// Build a poll packet (17 bytes).
    ///
    /// Format: `callsign[8] + 0x00 + reflector_name[8]`.
    #[must_use]
    pub fn build_poll(callsign: &str, reflector_name: &str) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(17);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(0x00);
        pkt.extend_from_slice(&pad_callsign(reflector_name));
        pkt
    }

    /// Write the D-STAR header fields into a DCS voice frame at the given buffer.
    fn write_header_fields(pkt: &mut [u8], header: &DStarHeader) {
        // Flags.
        pkt[4] = header.flag1;
        pkt[5] = header.flag2;
        pkt[6] = header.flag3;
        // Callsign fields (each 8 bytes, space-padded).
        pkt[7..15].copy_from_slice(&pad_callsign(&header.rpt2));
        pkt[15..23].copy_from_slice(&pad_callsign(&header.rpt1));
        pkt[23..31].copy_from_slice(&pad_callsign(&header.ur_call));
        pkt[31..39].copy_from_slice(&pad_callsign(&header.my_call));
        // MY suffix (4 bytes).
        let sfx = header.my_suffix.as_bytes();
        let sfx_len = sfx.len().min(4);
        pkt[39..39 + sfx_len].copy_from_slice(&sfx[..sfx_len]);
        for b in &mut pkt[39 + sfx_len..43] {
            *b = b' ';
        }
    }

    /// Build a voice frame (100 bytes).
    ///
    /// DCS embeds the full D-STAR header in every voice frame:
    /// `"0001" + flags[3] + RPT2[8] + RPT1[8] + UR[8] + MY[8] + MY_SFX[4]`
    /// `+ "AMBE" + 0x00 + seq + seq + ambe[9] + slow[3] + tx_seq[3] + 0x01 + zeros`.
    #[must_use]
    pub fn build_voice(
        header: &DStarHeader,
        seq: u8,
        ambe: &[u8; 9],
        slow_data: &[u8; 3],
        tx_sequence: u32,
    ) -> Vec<u8> {
        let mut pkt = vec![0u8; 100];
        // Tag.
        pkt[0..4].copy_from_slice(b"0001");
        // Header fields.
        write_header_fields(&mut pkt, header);
        // "AMBE" marker.
        pkt[43..47].copy_from_slice(b"AMBE");
        pkt[47] = 0x00;
        // Sequence bytes.
        pkt[48] = seq;
        pkt[49] = seq;
        // AMBE voice data.
        pkt[50..59].copy_from_slice(ambe);
        // Slow data.
        pkt[59..62].copy_from_slice(slow_data);
        // TX sequence (3 bytes, little-endian).
        let tx_bytes = tx_sequence.to_le_bytes();
        pkt[62..65].copy_from_slice(&tx_bytes[..3]);
        // Marker byte.
        pkt[65] = 0x01;
        // Remaining bytes stay zero.
        pkt
    }

    /// Build an EOT frame (100 bytes, same as voice with EOT flag).
    ///
    /// Uses AMBE silence and sync bytes for slow data. Sequence byte has
    /// bit 6 set (0x40).
    #[must_use]
    pub fn build_eot(header: &DStarHeader, seq: u8, tx_sequence: u32) -> Vec<u8> {
        build_voice(
            header,
            seq | 0x40,
            &AMBE_SILENCE,
            &DSTAR_SYNC_BYTES,
            tx_sequence,
        )
    }

    /// Parse an incoming DCS packet.
    ///
    /// Returns `None` if the packet format is not recognized.
    #[must_use]
    pub fn parse_packet(data: &[u8]) -> Option<DCSEvent> {
        // Connect ACK/NAK: check for 11+ byte packets with module bytes.
        // DCS connect responses are typically the same size as the connect
        // request (519) or a shorter ACK/NAK.
        if data.len() >= 11 && data.len() <= 519 {
            // If it looks like a connect response (has 0x0B at byte 10 = ack,
            // 0x00 at byte 10 = nak), and starts with a callsign.
            if data[10] == 0x0B && data.iter().take(8).all(u8::is_ascii) {
                return Some(DCSEvent::ConnectAck);
            }
            if data[10] == 0x00
                && data.len() <= 19
                && data.iter().take(8).all(u8::is_ascii)
                && data[9] == b' '
            {
                return Some(DCSEvent::ConnectNak);
            }
        }

        // Poll echo: 17 bytes with null separator at byte 8.
        if data.len() == 17 && data[8] == 0x00 {
            return Some(DCSEvent::PollEcho);
        }

        // Voice/EOT frame: 100 bytes starting with "0001".
        if data.len() == 100 && &data[0..4] == b"0001" {
            let header = DStarHeader {
                flag1: data[4],
                flag2: data[5],
                flag3: data[6],
                rpt2: String::from_utf8_lossy(&data[7..15]).into_owned(),
                rpt1: String::from_utf8_lossy(&data[15..23]).into_owned(),
                ur_call: String::from_utf8_lossy(&data[23..31]).into_owned(),
                my_call: String::from_utf8_lossy(&data[31..39]).into_owned(),
                my_suffix: String::from_utf8_lossy(&data[39..43]).into_owned(),
            };

            let seq = data[48];
            let mut ambe = [0u8; 9];
            ambe.copy_from_slice(&data[50..59]);
            let mut slow_data = [0u8; 3];
            slow_data.copy_from_slice(&data[59..62]);
            let tx_sequence =
                u32::from(data[62]) | (u32::from(data[63]) << 8) | (u32::from(data[64]) << 16);

            if seq & 0x40 != 0 {
                return Some(DCSEvent::Eot {
                    header,
                    seq,
                    tx_sequence,
                });
            }
            return Some(DCSEvent::Voice {
                header,
                seq,
                ambe,
                slow_data,
                tx_sequence,
            });
        }

        None
    }
}

/// `DPlus` protocol packet builders and parsers (REF reflectors, port 20001).
///
/// `DPlus` is similar to `DExtra` but has an authentication handshake step.
/// Voice header/data/EOT packets use the same DSVT format as `DExtra`.
pub mod dplus {
    use super::{
        DStarHeader, build_dsvt_eot, build_dsvt_header, build_dsvt_voice, pad_callsign, parse_dsvt,
    };

    /// Poll/keepalive interval in milliseconds.
    pub const POLL_INTERVAL_MS: u32 = 5000;

    /// Events received from a `DPlus` reflector.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum DPlusEvent {
        /// Reflector acknowledged the initial connection (login).
        ConnectAck,
        /// Reflector rejected the connection.
        ConnectNak,
        /// Reflector acknowledged the link request.
        LinkAck,
        /// Reflector rejected the link request.
        LinkNak,
        /// Echo response to a poll/keepalive.
        PollEcho,
        /// Incoming voice stream header.
        Header {
            /// The D-STAR radio header.
            header: DStarHeader,
            /// Stream identifier.
            stream_id: u16,
        },
        /// Incoming voice data frame.
        Voice {
            /// Stream identifier.
            stream_id: u16,
            /// Sequence number (0--20 cycle).
            seq: u8,
            /// AMBE-encoded voice data.
            ambe: [u8; 9],
            /// Slow data payload.
            slow_data: [u8; 3],
        },
        /// End of transmission marker.
        Eot {
            /// Stream identifier.
            stream_id: u16,
        },
    }

    /// Build a connect/login packet (28 bytes).
    ///
    /// `DPlus` uses a login-style connect: `0x1C + 0xC0 + 0x04 + 0x00 + callsign[8] + zeros[16]`.
    #[must_use]
    pub fn build_connect(callsign: &str, _module: char) -> Vec<u8> {
        let mut pkt = vec![0u8; 28];
        pkt[0] = 0x1C; // length
        pkt[1] = 0xC0; // type: login
        pkt[2] = 0x04; // subtype
        pkt[3] = 0x00;
        pkt[4..12].copy_from_slice(&pad_callsign(callsign));
        // Remaining 16 bytes are zeros (no auth token in basic mode).
        pkt
    }

    /// Build a link packet after connect ACK (11 bytes).
    ///
    /// Format: `callsign[8] + module + module + 0x0B`.
    #[must_use]
    pub fn build_link(callsign: &str, module: char) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(11);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(module as u8);
        pkt.push(module as u8);
        pkt.push(0x0B);
        pkt
    }

    /// Build a disconnect packet (11 bytes).
    ///
    /// Format: `callsign[8] + module + 0x20 + 0x00`.
    #[must_use]
    pub fn build_disconnect(callsign: &str, module: char) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(11);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(module as u8);
        pkt.push(b' ');
        pkt.push(0x00);
        pkt
    }

    /// Build a poll/keepalive packet (9 bytes).
    ///
    /// Format: `callsign[8] + 0x00`.
    #[must_use]
    pub fn build_poll(callsign: &str) -> Vec<u8> {
        let mut pkt = Vec::with_capacity(9);
        pkt.extend_from_slice(&pad_callsign(callsign));
        pkt.push(0x00);
        pkt
    }

    /// Build a voice header packet (56 bytes).
    ///
    /// Uses the same DSVT format as `DExtra`.
    #[must_use]
    pub fn build_header(header: &DStarHeader, stream_id: u16) -> Vec<u8> {
        build_dsvt_header(header, stream_id)
    }

    /// Build a voice data packet (27 bytes).
    ///
    /// Uses the same DSVT format as `DExtra`.
    #[must_use]
    pub fn build_voice(stream_id: u16, seq: u8, ambe: &[u8; 9], slow_data: &[u8; 3]) -> Vec<u8> {
        build_dsvt_voice(stream_id, seq, *ambe, *slow_data)
    }

    /// Build an EOT packet (27 bytes).
    ///
    /// Same as voice but seq has bit 6 set, AMBE is silence, slow data is sync.
    #[must_use]
    pub fn build_eot(stream_id: u16, seq: u8) -> Vec<u8> {
        build_dsvt_eot(stream_id, seq)
    }

    /// Parse an incoming `DPlus` packet.
    ///
    /// Returns `None` if the packet format is not recognized.
    #[must_use]
    pub fn parse_packet(data: &[u8]) -> Option<DPlusEvent> {
        // Connect ACK/NAK: 28 bytes with 0xC0 type byte.
        if data.len() == 28 && data[1] == 0xC0 {
            return match data[2] {
                0x04 => Some(DPlusEvent::ConnectAck),
                _ => Some(DPlusEvent::ConnectNak),
            };
        }

        // Link ACK/NAK: 11 bytes (same format as DExtra connect response).
        if data.len() == 11 {
            return match data[10] {
                0x0B => Some(DPlusEvent::LinkAck),
                0x00 => Some(DPlusEvent::LinkNak),
                _ => None,
            };
        }

        // Poll echo: 9 bytes ending with 0x00.
        if data.len() == 9 && data[8] == 0x00 {
            return Some(DPlusEvent::PollEcho);
        }

        // DSVT-based packets (header or voice).
        if let Some((is_header, stream_id, payload)) = parse_dsvt(data) {
            if is_header && data.len() == 56 && payload.len() >= 42 {
                let mut arr = [0u8; 41];
                arr.copy_from_slice(&payload[1..42]);
                if let Ok(header) = DStarHeader::decode(&arr) {
                    return Some(DPlusEvent::Header { header, stream_id });
                }
            } else if !is_header && data.len() == 27 && payload.len() >= 13 {
                let seq = payload[0];
                let mut ambe = [0u8; 9];
                ambe.copy_from_slice(&payload[1..10]);
                let mut slow_data = [0u8; 3];
                slow_data.copy_from_slice(&payload[10..13]);

                if seq & 0x40 != 0 {
                    return Some(DPlusEvent::Eot { stream_id });
                }
                return Some(DPlusEvent::Voice {
                    stream_id,
                    seq,
                    ambe,
                    slow_data,
                });
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> DStarHeader {
        DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: "REF001 G".to_owned(),
            rpt1: "REF001 C".to_owned(),
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: "W1AW    ".to_owned(),
            my_suffix: "    ".to_owned(),
        }
    }

    // ---- pad_callsign tests ----

    #[test]
    fn pad_callsign_short() {
        assert_eq!(&pad_callsign("W1AW"), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_exact() {
        assert_eq!(&pad_callsign("W1AW    "), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_long() {
        assert_eq!(&pad_callsign("ABCDEFGHIJ"), b"ABCDEFGH");
    }

    #[test]
    fn pad_callsign_empty() {
        assert_eq!(&pad_callsign(""), b"        ");
    }

    // ---- DExtra tests ----

    #[test]
    fn dextra_connect_packet_size_and_format() {
        let pkt = dextra::build_connect("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b'A');
        assert_eq!(pkt[10], 0x0B);
    }

    #[test]
    fn dextra_disconnect_packet_size_and_format() {
        let pkt = dextra::build_disconnect("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b' ');
        assert_eq!(pkt[10], 0x00);
    }

    #[test]
    fn dextra_poll_packet_size_and_format() {
        let pkt = dextra::build_poll("W1AW");
        assert_eq!(pkt.len(), 9);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
    }

    #[test]
    fn dextra_header_packet_size() {
        let pkt = dextra::build_header(&sample_header(), 0x1234);
        assert_eq!(pkt.len(), 56);
        assert_eq!(&pkt[0..4], b"DSVT");
        assert_eq!(pkt[4], 0x10);
    }

    #[test]
    fn dextra_voice_packet_size() {
        let ambe = [0x01; 9];
        let slow = [0x02; 3];
        let pkt = dextra::build_voice(0x1234, 5, &ambe, &slow);
        assert_eq!(pkt.len(), 27);
        assert_eq!(&pkt[0..4], b"DSVT");
        assert_eq!(pkt[4], 0x20);
    }

    #[test]
    fn dextra_eot_packet_has_bit6_set() {
        let pkt = dextra::build_eot(0x1234, 3);
        assert_eq!(pkt.len(), 27);
        // Seq byte is at offset 14.
        assert_eq!(pkt[14] & 0x40, 0x40);
        // AMBE should be silence.
        assert_eq!(&pkt[15..24], &AMBE_SILENCE);
        // Slow data should be sync.
        assert_eq!(&pkt[24..27], &DSTAR_SYNC_BYTES);
    }

    #[test]
    fn dextra_parse_connect_ack() {
        let pkt = dextra::build_connect("W1AW", 'A');
        let evt = dextra::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dextra::DExtraEvent::ConnectAck);
    }

    #[test]
    fn dextra_parse_connect_nak() {
        let pkt = dextra::build_disconnect("W1AW", 'A');
        let evt = dextra::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dextra::DExtraEvent::ConnectNak);
    }

    #[test]
    fn dextra_parse_poll_echo() {
        let pkt = dextra::build_poll("W1AW");
        let evt = dextra::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dextra::DExtraEvent::PollEcho);
    }

    #[test]
    fn dextra_header_roundtrip() {
        let hdr = sample_header();
        let pkt = dextra::build_header(&hdr, 0xABCD);
        let evt = dextra::parse_packet(&pkt).unwrap();
        match evt {
            dextra::DExtraEvent::Header { header, stream_id } => {
                assert_eq!(stream_id, 0xABCD);
                assert_eq!(header.my_call, hdr.my_call);
                assert_eq!(header.rpt1, hdr.rpt1);
                assert_eq!(header.rpt2, hdr.rpt2);
                assert_eq!(header.ur_call, hdr.ur_call);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    #[test]
    fn dextra_voice_roundtrip() {
        let ambe = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        let slow = [0xAA, 0xBB, 0xCC];
        let pkt = dextra::build_voice(0x5678, 7, &ambe, &slow);
        let evt = dextra::parse_packet(&pkt).unwrap();
        match evt {
            dextra::DExtraEvent::Voice {
                stream_id,
                seq,
                ambe: a,
                slow_data: s,
            } => {
                assert_eq!(stream_id, 0x5678);
                assert_eq!(seq, 7);
                assert_eq!(a, ambe);
                assert_eq!(s, slow);
            }
            other => panic!("expected Voice, got {other:?}"),
        }
    }

    #[test]
    fn dextra_eot_roundtrip() {
        let pkt = dextra::build_eot(0x5678, 3);
        let evt = dextra::parse_packet(&pkt).unwrap();
        match evt {
            dextra::DExtraEvent::Eot { stream_id } => {
                assert_eq!(stream_id, 0x5678);
            }
            other => panic!("expected Eot, got {other:?}"),
        }
    }

    // ---- DCS tests ----

    #[test]
    fn dcs_connect_packet_size_and_format() {
        let pkt = dcs::build_connect("W1AW", 'A', 'C');
        assert_eq!(pkt.len(), 519);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b'C');
        assert_eq!(pkt[10], 0x0B);
        // Rest should be zeros.
        assert!(pkt[11..].iter().all(|&b| b == 0));
    }

    #[test]
    fn dcs_disconnect_packet_size_and_format() {
        let pkt = dcs::build_disconnect("W1AW", 'A', "DCS001");
        assert_eq!(pkt.len(), 19);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b' ');
        assert_eq!(pkt[10], 0x00);
        assert_eq!(&pkt[11..19], b"DCS001  ");
    }

    #[test]
    fn dcs_poll_packet_size_and_format() {
        let pkt = dcs::build_poll("W1AW", "DCS001");
        assert_eq!(pkt.len(), 17);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
        assert_eq!(&pkt[9..17], b"DCS001  ");
    }

    #[test]
    fn dcs_voice_packet_size() {
        let ambe = [0x01; 9];
        let slow = [0x02; 3];
        let pkt = dcs::build_voice(&sample_header(), 5, &ambe, &slow, 42);
        assert_eq!(pkt.len(), 100);
        assert_eq!(&pkt[0..4], b"0001");
    }

    #[test]
    fn dcs_voice_roundtrip() {
        let hdr = sample_header();
        let ambe = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        let slow = [0xAA, 0xBB, 0xCC];
        let pkt = dcs::build_voice(&hdr, 7, &ambe, &slow, 100);
        let evt = dcs::parse_packet(&pkt).unwrap();
        match evt {
            dcs::DCSEvent::Voice {
                header,
                seq,
                ambe: a,
                slow_data: s,
                tx_sequence,
            } => {
                assert_eq!(seq, 7);
                assert_eq!(a, ambe);
                assert_eq!(s, slow);
                assert_eq!(tx_sequence, 100);
                assert_eq!(header.my_call, hdr.my_call);
                assert_eq!(header.rpt1, hdr.rpt1);
                assert_eq!(header.rpt2, hdr.rpt2);
            }
            other => panic!("expected Voice, got {other:?}"),
        }
    }

    #[test]
    fn dcs_eot_roundtrip() {
        let hdr = sample_header();
        let pkt = dcs::build_eot(&hdr, 3, 50);
        let evt = dcs::parse_packet(&pkt).unwrap();
        match evt {
            dcs::DCSEvent::Eot {
                header,
                seq,
                tx_sequence,
            } => {
                assert_eq!(seq & 0x3F, 3);
                assert!(seq & 0x40 != 0);
                assert_eq!(tx_sequence, 50);
                assert_eq!(header.my_call, hdr.my_call);
            }
            other => panic!("expected Eot, got {other:?}"),
        }
    }

    #[test]
    fn dcs_parse_poll_echo() {
        let pkt = dcs::build_poll("W1AW", "DCS001");
        let evt = dcs::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dcs::DCSEvent::PollEcho);
    }

    // ---- DPlus tests ----

    #[test]
    fn dplus_connect_packet_size_and_format() {
        let pkt = dplus::build_connect("W1AW", 'A');
        assert_eq!(pkt.len(), 28);
        assert_eq!(pkt[0], 0x1C);
        assert_eq!(pkt[1], 0xC0);
        assert_eq!(pkt[2], 0x04);
        assert_eq!(&pkt[4..12], b"W1AW    ");
    }

    #[test]
    fn dplus_link_packet_size_and_format() {
        let pkt = dplus::build_link("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b'A');
        assert_eq!(pkt[10], 0x0B);
    }

    #[test]
    fn dplus_disconnect_packet_size_and_format() {
        let pkt = dplus::build_disconnect("W1AW", 'A');
        assert_eq!(pkt.len(), 11);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], b'A');
        assert_eq!(pkt[9], b' ');
        assert_eq!(pkt[10], 0x00);
    }

    #[test]
    fn dplus_poll_packet_size_and_format() {
        let pkt = dplus::build_poll("W1AW");
        assert_eq!(pkt.len(), 9);
        assert_eq!(&pkt[0..8], b"W1AW    ");
        assert_eq!(pkt[8], 0x00);
    }

    #[test]
    fn dplus_parse_connect_ack() {
        let pkt = dplus::build_connect("W1AW", 'A');
        let evt = dplus::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dplus::DPlusEvent::ConnectAck);
    }

    #[test]
    fn dplus_parse_link_ack() {
        let pkt = dplus::build_link("W1AW", 'A');
        let evt = dplus::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dplus::DPlusEvent::LinkAck);
    }

    #[test]
    fn dplus_parse_poll_echo() {
        let pkt = dplus::build_poll("W1AW");
        let evt = dplus::parse_packet(&pkt).unwrap();
        assert_eq!(evt, dplus::DPlusEvent::PollEcho);
    }

    #[test]
    fn dplus_header_roundtrip() {
        let hdr = sample_header();
        let pkt = dplus::build_header(&hdr, 0xDEAD);
        let evt = dplus::parse_packet(&pkt).unwrap();
        match evt {
            dplus::DPlusEvent::Header { header, stream_id } => {
                assert_eq!(stream_id, 0xDEAD);
                assert_eq!(header.my_call, hdr.my_call);
                assert_eq!(header.rpt1, hdr.rpt1);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    #[test]
    fn dplus_voice_roundtrip() {
        let ambe = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        let slow = [0xAA, 0xBB, 0xCC];
        let pkt = dplus::build_voice(0xBEEF, 12, &ambe, &slow);
        let evt = dplus::parse_packet(&pkt).unwrap();
        match evt {
            dplus::DPlusEvent::Voice {
                stream_id,
                seq,
                ambe: a,
                slow_data: s,
            } => {
                assert_eq!(stream_id, 0xBEEF);
                assert_eq!(seq, 12);
                assert_eq!(a, ambe);
                assert_eq!(s, slow);
            }
            other => panic!("expected Voice, got {other:?}"),
        }
    }

    #[test]
    fn dplus_eot_roundtrip() {
        let pkt = dplus::build_eot(0xBEEF, 5);
        let evt = dplus::parse_packet(&pkt).unwrap();
        match evt {
            dplus::DPlusEvent::Eot { stream_id } => {
                assert_eq!(stream_id, 0xBEEF);
            }
            other => panic!("expected Eot, got {other:?}"),
        }
    }

    #[test]
    fn dplus_header_and_voice_same_as_dextra() {
        let hdr = sample_header();
        let stream_id = 0x1234;
        assert_eq!(
            dplus::build_header(&hdr, stream_id),
            dextra::build_header(&hdr, stream_id)
        );
        let ambe = [0x01; 9];
        let slow = [0x02; 3];
        assert_eq!(
            dplus::build_voice(stream_id, 5, &ambe, &slow),
            dextra::build_voice(stream_id, 5, &ambe, &slow)
        );
        assert_eq!(
            dplus::build_eot(stream_id, 3),
            dextra::build_eot(stream_id, 3)
        );
    }

    // ---- Shared constant tests ----

    #[test]
    fn ambe_silence_is_9_bytes() {
        assert_eq!(AMBE_SILENCE.len(), 9);
    }

    #[test]
    fn dstar_sync_is_3_bytes() {
        assert_eq!(DSTAR_SYNC_BYTES.len(), 3);
    }

    // ---- Edge case tests ----

    #[test]
    fn parse_garbage_returns_none() {
        assert!(dextra::parse_packet(&[]).is_none());
        assert!(dextra::parse_packet(&[0xFF; 5]).is_none());
        assert!(dcs::parse_packet(&[]).is_none());
        assert!(dcs::parse_packet(&[0xFF; 5]).is_none());
        assert!(dplus::parse_packet(&[]).is_none());
        assert!(dplus::parse_packet(&[0xFF; 5]).is_none());
    }

    #[test]
    fn dcs_voice_embeds_header_fields() {
        let hdr = DStarHeader {
            flag1: 0x40,
            flag2: 0x00,
            flag3: 0x01,
            rpt2: "XRF001 G".to_owned(),
            rpt1: "XRF001 C".to_owned(),
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: "N0CALL  ".to_owned(),
            my_suffix: "ABCD".to_owned(),
        };
        let pkt = dcs::build_voice(&hdr, 0, &[0; 9], &[0; 3], 0);
        // Verify header fields are embedded in the packet.
        assert_eq!(&pkt[4..7], &[0x40, 0x00, 0x01]);
        assert_eq!(&pkt[7..15], b"XRF001 G");
        assert_eq!(&pkt[15..23], b"XRF001 C");
        assert_eq!(&pkt[23..31], b"CQCQCQ  ");
        assert_eq!(&pkt[31..39], b"N0CALL  ");
        assert_eq!(&pkt[39..43], b"ABCD");
        assert_eq!(&pkt[43..47], b"AMBE");
    }

    #[test]
    fn dcs_tx_sequence_le_encoding() {
        let pkt = dcs::build_voice(&sample_header(), 0, &[0; 9], &[0; 3], 0x00AB_CDEF);
        // Only lower 3 bytes stored, little-endian.
        assert_eq!(pkt[62], 0xEF);
        assert_eq!(pkt[63], 0xCD);
        assert_eq!(pkt[64], 0xAB);
    }
}
