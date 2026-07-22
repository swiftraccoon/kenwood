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

use crate::types::{Callsign, Module, Suffix};

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
    /// DCS voice packets carry real flag bytes; use [`Self::encode`]
    /// for those.
    #[must_use]
    pub fn encode_for_dsvt(&self) -> [u8; ENCODED_LEN] {
        let mut h = *self;
        h.flag1 = 0;
        h.flag2 = 0;
        h.flag3 = 0;
        h.encode()
    }

    /// Build a header for the reflector-relay path.
    ///
    /// This is the canonical builder for any client (sextant,
    /// thd75-repl, future hotspot crates) sending voice into a
    /// `DPlus` / `DExtra` / `DCS` reflector. Per the convention
    /// validated against `ircDDBGateway/Common/DPlusHandler.cpp:77-79`
    /// and xlxd's `cdplusprotocol.cpp:209`:
    ///
    /// - `rpt1[0..7]` = `operator` callsign (first 7 bytes,
    ///   space-padded)
    /// - `rpt1[7]`   = `local_module` letter (A-Z, NEVER `'G'`)
    /// - `rpt2[0..7]` = `reflector` callsign (first 7 bytes,
    ///   space-padded)
    /// - `rpt2[7]`   = `reflector_module` letter (A-Z, NEVER `'G'`)
    /// - `ur_call`    = `CQCQCQ`
    /// - `flag1`/`flag2`/`flag3` = 0
    ///
    /// Both `rpt1[7]` and `rpt2[7]` are real module letters. xlxd's
    /// `IsValidModule` rejects `'G'` and silently drops the packet
    /// (no NAK, no log line, no retry), so any header with `rpt1[7]`
    /// outside `b'A'..=b'Z'` will be invisible to other clients.
    /// The `Module` type's invariant (`b'A'..=b'Z'`) is what makes
    /// this safe to express infallibly.
    ///
    /// `my_call` and `my_suffix` carry the operator's identity into
    /// the stream, surfaced to other clients as the speaker.
    ///
    /// Callers that need to preserve the original flag bytes (e.g.
    /// `thd75-repl` relaying a radio's TX header) can mutate
    /// `flag1`/`flag2`/`flag3` after construction.
    #[must_use]
    pub fn for_relay(
        operator: Callsign,
        local_module: Module,
        reflector: Callsign,
        reflector_module: Module,
        my_call: Callsign,
        my_suffix: Suffix,
    ) -> Self {
        Self {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: rpt_field(reflector, reflector_module),
            rpt1: rpt_field(operator, local_module),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call,
            my_suffix,
        }
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

/// Build an `rpt1`/`rpt2` field: 7-byte callsign + 1-byte module
/// letter at index 7. The `Module` type's `b'A'..=b'Z'` invariant is
/// what guarantees byte 7 is a valid module letter, so no runtime check
/// needed.
fn rpt_field(callsign: Callsign, module: Module) -> Callsign {
    let cs = callsign.as_bytes();
    let mut buf = [b' '; 8];
    buf[..7].copy_from_slice(&cs[..7]);
    buf[7] = module.as_byte();
    Callsign::from_wire_bytes(buf)
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
        // CRC checks. We mirror that: decode is infallible.
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
        // in callsign fields. Lenient receive: bytes preserved.
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
    fn for_relay_rpt1_carries_local_module_byte() {
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Callsign::from_wire_bytes(*b"REF030  "),
            Module::B,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::EMPTY,
        );
        assert_eq!(
            header.rpt1.as_bytes()[7],
            b'C',
            "rpt1[7] must be the local module letter; xlxd silently drops if invalid"
        );
        assert_eq!(
            &header.rpt1.as_bytes()[..7],
            b"W1AW   ",
            "rpt1[0..7] must be the operator callsign space-padded to 7 bytes"
        );
    }

    #[test]
    fn for_relay_rpt2_carries_reflector_module_byte() {
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Callsign::from_wire_bytes(*b"REF030  "),
            Module::B,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::EMPTY,
        );
        assert_eq!(
            header.rpt2.as_bytes()[7],
            b'B',
            "rpt2[7] must be the reflector module letter"
        );
        assert_eq!(
            &header.rpt2.as_bytes()[..7],
            b"REF030 ",
            "rpt2[0..7] must be the reflector callsign space-padded to 7 bytes"
        );
    }

    #[test]
    fn for_relay_ur_call_is_cqcqcq() {
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Callsign::from_wire_bytes(*b"REF030  "),
            Module::B,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::EMPTY,
        );
        assert_eq!(
            header.ur_call.as_bytes(),
            b"CQCQCQ  ",
            "ur_call must be CQCQCQ space-padded for relay headers"
        );
    }

    #[test]
    fn for_relay_zeroes_flag_bytes() {
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Callsign::from_wire_bytes(*b"REF030  "),
            Module::B,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::EMPTY,
        );
        assert_eq!(header.flag1, 0);
        assert_eq!(header.flag2, 0);
        assert_eq!(header.flag3, 0);
    }

    #[test]
    fn for_relay_passes_through_my_call_and_suffix() {
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"GATEWAY "),
            Module::C,
            Callsign::from_wire_bytes(*b"REF030  "),
            Module::B,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Suffix::from_wire_bytes(*b"ECHO"),
        );
        assert_eq!(
            header.my_call.as_bytes(),
            b"W1AW    ",
            "my_call distinct from operator (relay scenario)"
        );
        assert_eq!(header.my_suffix.as_bytes(), b"ECHO");
    }

    #[test]
    fn for_relay_truncates_callsign_to_7_bytes_for_rpt() {
        // Even with a fully-populated 8-byte callsign, only the first
        // 7 bytes feed rpt1; byte 7 is reserved for the module letter.
        // (Real callsigns are ≤6 chars so this only matters for
        // adversarial inputs, but the invariant must hold.)
        let header = DStarHeader::for_relay(
            Callsign::from_wire_bytes(*b"OPERATOR"),
            Module::A,
            Callsign::from_wire_bytes(*b"REFCALLR"),
            Module::E,
            Callsign::from_wire_bytes(*b"OPERATOR"),
            Suffix::EMPTY,
        );
        assert_eq!(&header.rpt1.as_bytes()[..7], b"OPERATO");
        assert_eq!(header.rpt1.as_bytes()[7], b'A');
        assert_eq!(&header.rpt2.as_bytes()[..7], b"REFCALL");
        assert_eq!(header.rpt2.as_bytes()[7], b'E');
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
