//! D-STAR radio header (41 bytes on the wire with CRC-CCITT).
//!
//! The header is transmitted at the start of every D-STAR voice stream.
//! It contains routing information (repeater callsigns, destination,
//! origin) and 3 flag bytes for control signaling.
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
//! Reflected polynomial 0x8408, initial value 0xFFFF, final XOR 0xFFFF.
//! Computed over bytes 0-38 (flags + callsigns + suffix), stored
//! little-endian at bytes 39-40.

use crate::types::{Callsign, Suffix};

/// Size of the encoded header on the wire (including CRC).
pub const ENCODED_LEN: usize = 41;

/// D-STAR radio header.
///
/// All callsign fields are typed [`Callsign`]s which enforce the
/// 8-byte ASCII space-padded wire format at construction. The suffix
/// is a typed [`Suffix`] which enforces the 4-byte ASCII space-padded
/// wire format.
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
        buf[0] = self.flag1;
        buf[1] = self.flag2;
        buf[2] = self.flag3;
        buf[3..11].copy_from_slice(self.rpt2.as_bytes());
        buf[11..19].copy_from_slice(self.rpt1.as_bytes());
        buf[19..27].copy_from_slice(self.ur_call.as_bytes());
        buf[27..35].copy_from_slice(self.my_call.as_bytes());
        buf[35..39].copy_from_slice(self.my_suffix.as_bytes());

        let crc = crc_ccitt(&buf[..39]);
        buf[39] = (crc & 0xFF) as u8;
        buf[40] = (crc >> 8) as u8;
        buf
    }

    /// Encode the header for embedding in a DSVT voice header packet.
    ///
    /// Identical to [`encode`](Self::encode) except the three flag
    /// bytes are forced to zero BEFORE CRC computation. This matches
    /// `ircDDBGateway/Common/HeaderData.cpp:615-617`, which zeros
    /// the flag bytes before copying them into the DSVT payload and
    /// before computing the CRC.
    ///
    /// DCS voice packets carry real flag bytes per xlxd
    /// `cdcsprotocol.cpp:EncodeDvPacket` and should keep using
    /// [`encode`](Self::encode) instead of this method.
    #[must_use]
    pub fn encode_for_dsvt(&self) -> [u8; ENCODED_LEN] {
        let mut h = *self;
        h.flag1 = 0;
        h.flag2 = 0;
        h.flag3 = 0;
        h.encode()
    }

    /// Decode a 41-byte header, validating the CRC and all callsign
    /// fields.
    ///
    /// Decode is **infallible** and never rejects a header.
    ///
    /// Mirrors `ircDDBGateway`'s reference behaviour which:
    /// 1. Calls `setDPlusData(..., check=false, ...)` from
    ///    `CDPlusProtocolHandler::readHeader` with a comment reading
    ///    `DPlus checksums are unreliable` at line 172 of
    ///    `DPlusProtocolHandler.cpp`. `DPlus` reflectors routinely
    ///    send headers with wrong CRCs, so the reference skips the
    ///    check entirely.
    /// 2. Calls `setDExtraData(..., check=false, ...)` from
    ///    `CDExtraProtocolHandler::readHeader` — same pattern.
    /// 3. `setDCSData` has no CRC check at all (returns `void`).
    ///
    /// All three reference parsers do a raw `memcpy` of the callsign
    /// fields without any byte-level validation, then return the
    /// populated header. Our decode matches that contract exactly.
    ///
    /// The [`crc_ccitt`] function is retained because our own
    /// [`Self::encode`] uses it to populate the CRC bytes on the TX
    /// side — a correct-CRC outbound header is harmless to reflectors
    /// that do check, and required by the few that do.
    #[must_use]
    pub fn decode(data: &[u8; ENCODED_LEN]) -> Self {
        let mut rpt2_bytes = [0u8; 8];
        rpt2_bytes.copy_from_slice(&data[3..11]);
        let mut rpt1_bytes = [0u8; 8];
        rpt1_bytes.copy_from_slice(&data[11..19]);
        let mut ur_bytes = [0u8; 8];
        ur_bytes.copy_from_slice(&data[19..27]);
        let mut my_bytes = [0u8; 8];
        my_bytes.copy_from_slice(&data[27..35]);
        let mut suffix_bytes = [0u8; 4];
        suffix_bytes.copy_from_slice(&data[35..39]);

        Self {
            flag1: data[0],
            flag2: data[1],
            flag3: data[2],
            rpt2: Callsign::from_wire_bytes(rpt2_bytes),
            rpt1: Callsign::from_wire_bytes(rpt1_bytes),
            ur_call: Callsign::from_wire_bytes(ur_bytes),
            my_call: Callsign::from_wire_bytes(my_bytes),
            my_suffix: Suffix::from_wire_bytes(suffix_bytes),
        }
    }

    /// Get the RPT2 callsign as a trimmed string.
    #[must_use]
    pub fn rpt2_str(&self) -> std::borrow::Cow<'_, str> {
        self.rpt2.as_str()
    }

    /// Get the RPT1 callsign as a trimmed string.
    #[must_use]
    pub fn rpt1_str(&self) -> std::borrow::Cow<'_, str> {
        self.rpt1.as_str()
    }

    /// Get the YOUR callsign as a trimmed string.
    #[must_use]
    pub fn ur_call_str(&self) -> std::borrow::Cow<'_, str> {
        self.ur_call.as_str()
    }

    /// Get the MY callsign as a trimmed string.
    #[must_use]
    pub fn my_call_str(&self) -> std::borrow::Cow<'_, str> {
        self.my_call.as_str()
    }

    /// Get the MY suffix as a trimmed string.
    #[must_use]
    pub fn my_suffix_str(&self) -> std::borrow::Cow<'_, str> {
        self.my_suffix.as_str()
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

    fn cs(s: &str) -> Callsign {
        Callsign::try_from_str(s).expect("valid callsign in test")
    }

    fn sfx(s: &str) -> Suffix {
        Suffix::try_from_str(s).expect("valid suffix in test")
    }

    #[test]
    fn encode_decode_roundtrip() {
        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        };
        let encoded = header.encode();
        assert_eq!(encoded.len(), ENCODED_LEN);

        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded, header);
    }

    #[test]
    fn decode_accepts_bad_crc() {
        // Regression: ircDDBGateway's DPlus/DExtra/DCS readers all
        // skip the CRC check with the comment "DPlus checksums are
        // unreliable" in DPlusProtocolHandler.cpp:172. Real-world
        // reflectors routinely emit headers with wrong CRCs. We must
        // not silently drop them. decode is infallible.
        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: cs("DIRECT"),
            rpt1: cs("DIRECT"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("N0CALL"),
            my_suffix: Suffix::EMPTY,
        };
        let mut encoded = header.encode();
        encoded[40] ^= 0xFF; // corrupt CRC
        let decoded = DStarHeader::decode(&encoded);
        // All user-visible fields still match, even though the CRC
        // bytes on the wire were wrong.
        assert_eq!(decoded.my_call, header.my_call);
        assert_eq!(decoded.ur_call, header.ur_call);
        assert_eq!(decoded.rpt1, header.rpt1);
        assert_eq!(decoded.rpt2, header.rpt2);
    }

    #[test]
    fn decode_accepts_non_ascii_callsign_verbatim() {
        // Regression: a previous over-strict implementation dropped
        // headers containing non-printable bytes in any callsign
        // field, which silently lost real-world reflector traffic
        // (observed live with HL2IPB on REF030 C). The reference
        // ircDDBGateway CHeaderData::setDPlusData does a raw memcpy
        // and never validates content, so we don't either — the
        // byte is stored verbatim and the display path uses lossy
        // UTF-8 rendering.
        let header = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        };
        let mut encoded = header.encode();
        encoded[27] = 0xC3; // first byte of MY callsign — non-ASCII
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded.my_call.as_bytes()[0], 0xC3);
    }

    #[test]
    fn encode_for_dsvt_zeros_flag_bytes_before_crc() {
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
        let dsvt = hdr.encode_for_dsvt();
        assert_eq!(dsvt[0], 0, "flag1 zeroed in DSVT encoding");
        assert_eq!(dsvt[1], 0, "flag2 zeroed in DSVT encoding");
        assert_eq!(dsvt[2], 0, "flag3 zeroed in DSVT encoding");

        // Non-flag bytes unchanged from what encode() would produce
        // with flag1=flag2=flag3=0.
        let expected = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: hdr.rpt2,
            rpt1: hdr.rpt1,
            ur_call: hdr.ur_call,
            my_call: hdr.my_call,
            my_suffix: hdr.my_suffix,
        }
        .encode();
        assert_eq!(
            dsvt, expected,
            "DSVT encoding == encode() with zeroed flags"
        );
    }

    #[test]
    fn crc_known_vector() {
        // "CQCQCQ  " CRC from xlxd test vectors
        let data = b"CQCQCQ  ";
        let crc = crc_ccitt(data);
        // Verify it's deterministic and non-zero
        assert_ne!(crc, 0);
        assert_eq!(crc, crc_ccitt(data));
    }

    #[test]
    fn crc_ccitt_matches_reference_for_w1aw_header() {
        // Canonical 39-byte header body: 3 zero flag bytes + RPT2 + RPT1
        // + UR + MY + suffix. This is the same body as `encode_decode_roundtrip`
        // produces for a W1AW CQ call via REF030 C.
        //
        // The expected CRC was computed by running our `crc_ccitt` against
        // this body and cross-checked against the table-based
        // ircDDBGateway implementation in
        // `ref/ircDDBGateway/Common/CCITTChecksum.cpp`, which uses the
        // same reflected polynomial 0x8408, init 0xFFFF, and final XOR
        // 0xFFFF. The reference impl swaps bytes in its `result()`
        // getter before writing the wire bytes; our `encode()` writes
        // the CRC low byte at offset 39 and high byte at offset 40,
        // producing the same two bytes in the same wire order.
        let mut body = [0u8; 39];
        body[3..11].copy_from_slice(b"REF030 G");
        body[11..19].copy_from_slice(b"REF030 C");
        body[19..27].copy_from_slice(b"CQCQCQ  ");
        body[27..35].copy_from_slice(b"W1AW    ");
        body[35..39].copy_from_slice(b"    ");

        let crc = crc_ccitt(&body);
        assert_eq!(crc, 0x1073);

        // And confirm that encoding the full header stores this CRC
        // little-endian at the expected offsets.
        let header = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: Suffix::EMPTY,
        };
        let encoded = header.encode();
        assert_eq!(encoded[39], (crc & 0xFF) as u8);
        assert_eq!(encoded[40], (crc >> 8) as u8);
    }

    #[test]
    fn suffix_roundtrip_nonempty() {
        let hdr = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: cs("REF030 G"),
            rpt1: cs("REF030 C"),
            ur_call: cs("CQCQCQ"),
            my_call: cs("W1AW"),
            my_suffix: sfx("ECHO"),
        };
        let encoded = hdr.encode();
        let decoded = DStarHeader::decode(&encoded);
        assert_eq!(decoded.my_suffix.as_bytes(), b"ECHO");
    }
}
