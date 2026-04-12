//! D-STAR radio header (41 bytes on the wire with CRC-CCITT).
//!
//! The header is transmitted at the start of every D-STAR voice
//! stream. It contains routing information (repeater callsigns,
//! destination, origin) and 3 flag bytes for control signaling.
//!
//! # Wire format (per JARL D-STAR specification)
//!
//! ```text
//! Offset  Length  Field
//! 0       1       Flag 1 (control)
//! 1       1       Flag 2 (reserved)
//! 2       1       Flag 3 (reserved)
//! 3       8       RPT2 callsign (space-padded)
//! 11      8       RPT1 callsign (space-padded)
//! 19      8       YOUR callsign (space-padded)
//! 27      8       MY callsign (space-padded)
//! 35      4       MY suffix (space-padded)
//! 39      2       CRC-CCITT (little-endian)
//! ```
//!
//! # CRC-CCITT
//!
//! Reflected polynomial 0x8408, initial value 0xFFFF, final XOR
//! 0xFFFF. Computed over bytes 0-38, stored little-endian at 39-40.
//!
//! See `ircDDBGateway/Common/HeaderData.cpp:637-684` (`getDPlusData`)
//! and `ircDDBGateway/Common/CCITTChecksum.cpp` for the reference
//! implementation this module mirrors.

use crate::types::{Callsign, Suffix};

/// Size of the encoded header on the wire (including CRC).
pub const ENCODED_LEN: usize = 41;

/// D-STAR radio header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DStarHeader {
    /// Control flag byte 1.
    pub flag1: u8,
    /// Reserved flag byte 2.
    pub flag2: u8,
    /// Reserved flag byte 3.
    pub flag3: u8,
    /// Repeater 2 callsign (gateway).
    pub rpt2: Callsign,
    /// Repeater 1 callsign (access).
    pub rpt1: Callsign,
    /// Destination callsign (YOUR).
    pub ur_call: Callsign,
    /// Origin callsign (MY).
    pub my_call: Callsign,
    /// Origin suffix.
    pub my_suffix: Suffix,
}

impl DStarHeader {
    /// Encode the header into 41 bytes with CRC.
    #[must_use]
    pub fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut buf = [0u8; ENCODED_LEN];
        if let Some(b) = buf.get_mut(0) {
            *b = self.flag1;
        }
        if let Some(b) = buf.get_mut(1) {
            *b = self.flag2;
        }
        if let Some(b) = buf.get_mut(2) {
            *b = self.flag3;
        }
        if let Some(s) = buf.get_mut(3..11) {
            s.copy_from_slice(self.rpt2.as_bytes());
        }
        if let Some(s) = buf.get_mut(11..19) {
            s.copy_from_slice(self.rpt1.as_bytes());
        }
        if let Some(s) = buf.get_mut(19..27) {
            s.copy_from_slice(self.ur_call.as_bytes());
        }
        if let Some(s) = buf.get_mut(27..35) {
            s.copy_from_slice(self.my_call.as_bytes());
        }
        if let Some(s) = buf.get_mut(35..39) {
            s.copy_from_slice(self.my_suffix.as_bytes());
        }

        let crc = crc_ccitt(buf.get(..39).unwrap_or(&[]));
        if let Some(b) = buf.get_mut(39) {
            *b = (crc & 0xFF) as u8;
        }
        if let Some(b) = buf.get_mut(40) {
            *b = (crc >> 8) as u8;
        }
        buf
    }

    /// Encode the header for embedding in a DSVT voice header packet.
    ///
    /// Identical to [`Self::encode`] except the three flag bytes are
    /// forced to zero BEFORE CRC computation. Matches
    /// `ircDDBGateway/Common/HeaderData.cpp:665-667` (`getDPlusData`).
    ///
    /// DCS voice packets carry real flag bytes — use [`Self::encode`]
    /// for those.
    #[must_use]
    pub fn encode_for_dsvt(&self) -> [u8; ENCODED_LEN] {
        let mut h = *self;
        h.flag1 = 0;
        h.flag2 = 0;
        h.flag3 = 0;
        h.encode()
    }

    /// Decode a 41-byte header.
    ///
    /// **Infallible.** Mirrors `ircDDBGateway`'s `setDPlusData` /
    /// `setDExtraData` / `setDCSData` reference implementations,
    /// which do raw `memcpy` of the callsign fields with zero
    /// validation and skip the CRC check. Real reflectors emit
    /// headers with bad CRCs and non-printable callsign bytes; a
    /// strict decoder would silently drop real-world traffic.
    #[must_use]
    pub fn decode(data: &[u8; ENCODED_LEN]) -> Self {
        let mut rpt2_bytes = [0u8; 8];
        if let Some(s) = data.get(3..11) {
            rpt2_bytes.copy_from_slice(s);
        }
        let mut rpt1_bytes = [0u8; 8];
        if let Some(s) = data.get(11..19) {
            rpt1_bytes.copy_from_slice(s);
        }
        let mut ur_bytes = [0u8; 8];
        if let Some(s) = data.get(19..27) {
            ur_bytes.copy_from_slice(s);
        }
        let mut my_bytes = [0u8; 8];
        if let Some(s) = data.get(27..35) {
            my_bytes.copy_from_slice(s);
        }
        let mut suffix_bytes = [0u8; 4];
        if let Some(s) = data.get(35..39) {
            suffix_bytes.copy_from_slice(s);
        }

        Self {
            flag1: *data.first().unwrap_or(&0),
            flag2: *data.get(1).unwrap_or(&0),
            flag3: *data.get(2).unwrap_or(&0),
            rpt2: Callsign::from_wire_bytes(rpt2_bytes),
            rpt1: Callsign::from_wire_bytes(rpt1_bytes),
            ur_call: Callsign::from_wire_bytes(ur_bytes),
            my_call: Callsign::from_wire_bytes(my_bytes),
            my_suffix: Suffix::from_wire_bytes(suffix_bytes),
        }
    }
}

/// CRC-CCITT (reflected polynomial 0x8408, init 0xFFFF, final XOR 0xFFFF).
///
/// Per `g4klx/MMDVMHost` `DSTARCRC.cpp` and JARL D-STAR specification.
#[must_use]
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0x8408;
            } else {
                crc >>= 1;
            }
        }
    }
    crc ^ 0xFFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn cs(bytes: [u8; 8]) -> Callsign {
        Callsign::from_wire_bytes(bytes)
    }

    fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: cs(*b"REF030 G"),
            rpt1: cs(*b"REF030 C"),
            ur_call: cs(*b"CQCQCQ  "),
            my_call: cs(*b"W1AW    "),
            my_suffix: Suffix::EMPTY,
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let header = test_header();
        let encoded = header.encode();
        assert_eq!(encoded.len(), ENCODED_LEN);
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded, header);
    }

    #[test]
    fn decode_accepts_bad_crc() {
        // Per ircDDBGateway/Common/DPlusProtocolHandler.cpp:172
        // ("DPlus checksums are unreliable") the receive path skips
        // CRC checks. We mirror that — decode is infallible.
        let header = test_header();
        let mut encoded = header.encode();
        if let Some(byte) = encoded.get_mut(40) {
            *byte ^= 0xFF;
        }
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded.my_call, header.my_call);
    }

    #[test]
    fn decode_accepts_non_ascii_callsign_verbatim() {
        // Real-world reflector traffic includes non-printable bytes
        // in callsign fields. Lenient receive — bytes preserved.
        let header = test_header();
        let mut encoded = header.encode();
        if let Some(byte) = encoded.get_mut(27) {
            *byte = 0xC3;
        }
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded.my_call.as_bytes()[0], 0xC3);
    }

    #[test]
    fn encode_for_dsvt_zeros_flag_bytes_before_crc() {
        let hdr = DStarHeader {
            flag1: 0xAA,
            flag2: 0xBB,
            flag3: 0xCC,
            ..test_header()
        };
        let dsvt = hdr.encode_for_dsvt();
        assert_eq!(dsvt[0], 0, "flag1 zeroed in DSVT encoding");
        assert_eq!(dsvt[1], 0, "flag2 zeroed in DSVT encoding");
        assert_eq!(dsvt[2], 0, "flag3 zeroed in DSVT encoding");
    }

    #[test]
    fn crc_ccitt_known_vector_w1aw_header() {
        // Canonical 39-byte header body for the W1AW CQ via REF030 C
        // example. Cross-checked against ircDDBGateway's
        // CCITTChecksum.cpp table-based impl.
        let mut body = [0u8; 39];
        if let Some(s) = body.get_mut(3..11) {
            s.copy_from_slice(b"REF030 G");
        }
        if let Some(s) = body.get_mut(11..19) {
            s.copy_from_slice(b"REF030 C");
        }
        if let Some(s) = body.get_mut(19..27) {
            s.copy_from_slice(b"CQCQCQ  ");
        }
        if let Some(s) = body.get_mut(27..35) {
            s.copy_from_slice(b"W1AW    ");
        }
        if let Some(s) = body.get_mut(35..39) {
            s.copy_from_slice(b"    ");
        }
        let crc = crc_ccitt(&body);
        assert_eq!(crc, 0x1073);
    }

    #[test]
    fn suffix_roundtrip_nonempty() {
        let hdr = DStarHeader {
            my_suffix: Suffix::from_wire_bytes(*b"ECHO"),
            ..test_header()
        };
        let encoded = hdr.encode();
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded.my_suffix.as_bytes(), b"ECHO");
    }
}
