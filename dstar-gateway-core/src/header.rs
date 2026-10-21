//! D-STAR radio header: 39 field bytes followed by a two-byte CRC.
//!
//! Field order is flags (3), RPT2 (8), RPT1 (8), YOUR (8), MY (8),
//! suffix (4), and little-endian CRC (2). The receive representation
//! preserves every field byte; encoding recomputes the CRC. See
//! `ircDDBGateway/Common/HeaderData.cpp::{setDPlusData,getDPlusData}`.

pub use flags::{HeaderFlag, HeaderFlags, RepeaterControl};

use crate::types::{Callsign, Module, RoutingCallsign, Suffix};

mod flags;

/// Encoded header length, including its two-byte CRC.
pub const ENCODED_LEN: usize = 41;

/// Named repeater prefixes and module selections for a reflector relay.
///
/// Each checked prefix occupies seven bytes; its module occupies byte eight.
/// Module syntax is A-Z. The remote reflector determines which modules exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelayRouting {
    /// Local gateway's callsign prefix for RPT1.
    pub operator: RoutingCallsign,
    /// Local module carried in RPT1.
    pub local_module: Module,
    /// Target reflector's callsign prefix for RPT2.
    pub reflector: RoutingCallsign,
    /// Target module carried in RPT2.
    pub reflector_module: Module,
}

/// Lossless fields of one D-STAR radio header.
///
/// Received callsigns, suffix and flags may contain unknown bytes. Their
/// checked text views distinguish printable protocol text from diagnostic
/// formatting. [`Self::for_relay`] constructs checked repeater routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DstarHeader {
    /// Independent flags, repeater-control code and reserved bytes.
    pub flags: HeaderFlags,
    /// Repeater 2 callsign and module (gateway).
    pub rpt2: Callsign,
    /// Repeater 1 callsign and module (access).
    pub rpt1: Callsign,
    /// Destination callsign (YOUR).
    pub ur_call: Callsign,
    /// Origin callsign (MY).
    pub my_call: Callsign,
    /// Origin suffix.
    pub my_suffix: Suffix,
}

impl DstarHeader {
    /// Encode all fields into 41 bytes, recomputing CRC over bytes 0 through 38.
    #[must_use]
    pub fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut bytes = [0_u8; ENCODED_LEN];
        if let Some(destination) = bytes.get_mut(..3) {
            destination.copy_from_slice(self.flags.as_bytes());
        }
        for (range, callsign) in [
            (3..11, self.rpt2),
            (11..19, self.rpt1),
            (19..27, self.ur_call),
            (27..35, self.my_call),
        ] {
            if let Some(destination) = bytes.get_mut(range) {
                destination.copy_from_slice(callsign.as_bytes());
            }
        }
        if let Some(destination) = bytes.get_mut(35..39) {
            destination.copy_from_slice(self.my_suffix.as_bytes());
        }
        let checksum = crc_ccitt(bytes.get(..39).unwrap_or(&[])).to_le_bytes();
        if let Some(destination) = bytes.get_mut(39..) {
            destination.copy_from_slice(&checksum);
        }
        bytes
    }

    /// Encode DSVT header fields with zero flags and a matching recomputed CRC.
    ///
    /// Matches `ircDDBGateway/Common/HeaderData.cpp::getDPlusData`.
    /// DCS embedding preserves [`Self::flags`] instead.
    #[must_use]
    pub fn encode_for_dsvt(&self) -> [u8; ENCODED_LEN] {
        self.with_flags(HeaderFlags::NONE).encode()
    }

    /// Construct CQ reflector routing and preserve the supplied speaker identity.
    ///
    /// RPT1 combines the operator prefix and local module; RPT2 combines the
    /// reflector prefix and target module. YOUR is `CQCQCQ` and flags are zero.
    /// This is the relay convention in `ircDDBGateway/Common/DPlusHandler.cpp`
    /// and `xlxd/src/cdplusprotocol.cpp`. Use [`Self::with_flags`] when relaying
    /// source flag bytes.
    #[must_use]
    pub const fn for_relay(routing: RelayRouting, my_call: Callsign, my_suffix: Suffix) -> Self {
        Self {
            flags: HeaderFlags::NONE,
            rpt2: rpt_field(routing.reflector, routing.reflector_module),
            rpt1: rpt_field(routing.operator, routing.local_module),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call,
            my_suffix,
        }
    }

    /// Replace the complete flag value, preserving every other field.
    #[must_use]
    pub const fn with_flags(mut self, flags: HeaderFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Construct a local radio relay header using the repeater convention.
    ///
    /// RPT1 and RPT2 both contain the station plus local module, YOUR becomes
    /// `CQCQCQ`, and the repeater bit is set. Other flag bits and the speaker
    /// identity are preserved. Reference: `MMDVMHost/DStarControl.cpp`.
    #[must_use]
    pub const fn for_radio_relay(
        station: RoutingCallsign,
        local_module: Module,
        incoming: &Self,
    ) -> Self {
        let repeater = rpt_field(station, local_module);
        Self {
            flags: incoming.flags.with(HeaderFlag::Repeater, true),
            rpt2: repeater,
            rpt1: repeater,
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: incoming.my_call,
            my_suffix: incoming.my_suffix,
        }
    }

    /// Decode exactly 41 bytes, preserving flag, callsign and suffix bytes.
    ///
    /// CRC verification belongs to the enclosing codec's diagnostic policy.
    /// This follows `ircDDBGateway`'s field-copying receive path.
    #[must_use]
    pub fn decode(data: &[u8; ENCODED_LEN]) -> Self {
        let mut flags = [0_u8; 3];
        if let Some(source) = data.get(..3) {
            flags.copy_from_slice(source);
        }
        let mut fields = [[0_u8; 8]; 4];
        for (field, range) in fields.iter_mut().zip([3..11, 11..19, 19..27, 27..35]) {
            if let Some(source) = data.get(range) {
                field.copy_from_slice(source);
            }
        }
        let [rpt2, rpt1, destination, speaker] = fields;
        let mut suffix = [0_u8; 4];
        if let Some(source) = data.get(35..39) {
            suffix.copy_from_slice(source);
        }
        Self {
            flags: HeaderFlags::from_bytes(flags),
            rpt2: Callsign::from_wire_bytes(rpt2),
            rpt1: Callsign::from_wire_bytes(rpt1),
            ur_call: Callsign::from_wire_bytes(destination),
            my_call: Callsign::from_wire_bytes(speaker),
            my_suffix: Suffix::from_wire_bytes(suffix),
        }
    }
}

/// Place the module after the complete checked prefix without truncation.
const fn rpt_field(callsign: RoutingCallsign, module: Module) -> Callsign {
    let [byte0, byte1, byte2, byte3, byte4, byte5, byte6] = *callsign.as_bytes();
    Callsign::from_wire_bytes([
        byte0,
        byte1,
        byte2,
        byte3,
        byte4,
        byte5,
        byte6,
        module.as_byte(),
    ])
}

/// Reflected CRC-CCITT: polynomial `0x8408`, initial and final XOR `0xFFFF`.
///
/// Reference: `ircDDBGateway/Common/CCITTChecksum.cpp`.
#[must_use]
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x8408
            };
        }
    }
    crc ^ 0xFFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn relay() -> Result<DstarHeader, Box<dyn std::error::Error>> {
        Ok(DstarHeader::for_relay(
            RelayRouting {
                operator: RoutingCallsign::new("W1AW")?,
                local_module: Module::C,
                reflector: RoutingCallsign::new("REF030")?,
                reflector_module: Module::B,
            },
            Callsign::try_from_str("KQ4NIT")?,
            Suffix::try_from_str("D75")?,
        ))
    }

    #[test]
    fn relay_routes_preserve_both_named_modules_and_speaker() -> TestResult {
        let header = relay()?;
        assert_eq!(header.rpt1.as_bytes(), b"W1AW   C");
        assert_eq!(header.rpt2.as_bytes(), b"REF030 B");
        assert_eq!(header.ur_call.as_bytes(), b"CQCQCQ  ");
        assert_eq!(header.my_call.text(), Ok("KQ4NIT"));
        assert_eq!(header.my_suffix.text(), Ok("D75"));
        assert_eq!(header.flags, HeaderFlags::NONE);
        assert_eq!(DstarHeader::decode(&header.encode()), header);
        Ok(())
    }

    #[test]
    fn routing_constructor_rejects_truncation_and_invalid_receive_text() -> TestResult {
        let longest = RoutingCallsign::new("ABCDEFG")?;
        assert_eq!(longest.callsign().as_bytes(), b"ABCDEFG ");
        for text in ["", "OPERATOR", "A\tB", "é"] {
            assert!(
                RoutingCallsign::new(text).is_err(),
                "invalid prefix {text:?}"
            );
        }
        assert!(
            RoutingCallsign::try_from(Callsign::from_wire_bytes(*b"OPERATOR")).is_err(),
            "the eighth byte cannot be discarded"
        );
        assert!(
            RoutingCallsign::try_from(Callsign::from_wire_bytes(*b"W1AW\0   ")).is_err(),
            "lossless receive bytes require validation before routing"
        );
        Ok(())
    }

    #[test]
    fn flag_operations_preserve_reserved_bytes_and_separate_control_code() -> TestResult {
        let header = relay()?.with_flags(HeaderFlags::from_bytes([0xA5, 0xBB, 0xCC]));
        assert_eq!(header.flags.control(), RepeaterControl::Unassigned);
        let flags = header
            .flags
            .with(HeaderFlag::Repeater, true)
            .with(HeaderFlag::Interrupted, false)
            .with_control(RepeaterControl::Acknowledge);
        assert_eq!(flags.as_bytes(), &[0xC3, 0xBB, 0xCC]);
        assert!(flags.contains(HeaderFlag::Data));
        assert!(flags.contains(HeaderFlag::Repeater));
        assert!(!flags.contains(HeaderFlag::Interrupted));
        assert_eq!(DstarHeader::decode(&header.encode()).flags, header.flags);
        let radio = DstarHeader::for_radio_relay(RoutingCallsign::new("W1AW")?, Module::G, &header);
        assert_eq!(radio.flags.as_bytes(), &[0xE5, 0xBB, 0xCC]);
        assert_eq!(radio.rpt1.as_bytes(), b"W1AW   G");
        assert_eq!(radio.rpt2, radio.rpt1);
        assert_eq!(radio.my_call, header.my_call);
        assert_eq!(radio.my_suffix, header.my_suffix);
        Ok(())
    }

    #[test]
    fn dsvt_zeroes_flags_before_computing_its_crc() -> TestResult {
        let header = relay()?.with_flags(HeaderFlags::from_bytes([0xAA, 0xBB, 0xCC]));
        let encoded = header.encode_for_dsvt();
        let expected = header.with_flags(HeaderFlags::NONE).encode();
        assert_eq!(
            encoded, expected,
            "flags and their covered checksum must both change"
        );
        assert_ne!(encoded.get(39..), header.encode().get(39..));
        Ok(())
    }

    #[test]
    fn receive_preserves_nonprintable_fields_and_ignores_crc_in_field_decode() -> TestResult {
        let mut bytes = relay()?.encode();
        *bytes.get_mut(27).ok_or("MY offset")? = 0xC3;
        *bytes.get_mut(40).ok_or("CRC offset")? ^= 0xFF;
        let decoded = DstarHeader::decode(&bytes);
        assert_eq!(decoded.my_call.as_bytes().first(), Some(&0xC3));
        assert!(decoded.my_call.text().is_err());
        assert_eq!(decoded.my_suffix.text(), Ok("D75"));
        Ok(())
    }

    #[test]
    fn crc_ccitt_known_vector_w1aw_header() -> TestResult {
        let header = DstarHeader::for_relay(
            RelayRouting {
                operator: RoutingCallsign::new("REF030")?,
                local_module: Module::C,
                reflector: RoutingCallsign::new("REF030")?,
                reflector_module: Module::G,
            },
            Callsign::try_from_str("W1AW")?,
            Suffix::EMPTY,
        );
        let bytes = header.encode();
        assert_eq!(
            bytes.get(39..),
            Some([0x73, 0x10].as_slice()),
            "independent header checksum vector"
        );
        Ok(())
    }
}
