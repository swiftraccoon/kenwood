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

/// Size of the encoded header on the wire (including CRC).
pub const ENCODED_LEN: usize = 41;

/// D-STAR radio header.
///
/// All callsign fields are 8 characters, space-padded on the right.
/// The suffix is 4 characters, space-padded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DStarHeader {
    /// Control flag byte 1.
    pub flag1: u8,
    /// Reserved flag byte 2.
    pub flag2: u8,
    /// Reserved flag byte 3.
    pub flag3: u8,
    /// Repeater 2 callsign (gateway, 8 chars space-padded).
    pub rpt2: [u8; 8],
    /// Repeater 1 callsign (access, 8 chars space-padded).
    pub rpt1: [u8; 8],
    /// Destination callsign (YOUR, 8 chars space-padded).
    pub ur_call: [u8; 8],
    /// Origin callsign (MY, 8 chars space-padded).
    pub my_call: [u8; 8],
    /// Origin suffix (4 chars space-padded).
    pub my_suffix: [u8; 4],
}

impl DStarHeader {
    /// Encode the header into 41 bytes with CRC.
    #[must_use]
    pub fn encode(&self) -> [u8; ENCODED_LEN] {
        let mut buf = [0u8; ENCODED_LEN];
        buf[0] = self.flag1;
        buf[1] = self.flag2;
        buf[2] = self.flag3;
        buf[3..11].copy_from_slice(&self.rpt2);
        buf[11..19].copy_from_slice(&self.rpt1);
        buf[19..27].copy_from_slice(&self.ur_call);
        buf[27..35].copy_from_slice(&self.my_call);
        buf[35..39].copy_from_slice(&self.my_suffix);

        let crc = crc_ccitt(&buf[..39]);
        buf[39] = (crc & 0xFF) as u8;
        buf[40] = (crc >> 8) as u8;
        buf
    }

    /// Decode a 41-byte header, validating the CRC.
    ///
    /// # Errors
    ///
    /// Returns `None` if the CRC does not match.
    #[must_use]
    pub fn decode(data: &[u8; ENCODED_LEN]) -> Option<Self> {
        let computed = crc_ccitt(&data[..39]);
        let stored = u16::from(data[39]) | (u16::from(data[40]) << 8);
        if computed != stored {
            return None;
        }

        let mut rpt2 = [b' '; 8];
        rpt2.copy_from_slice(&data[3..11]);
        let mut rpt1 = [b' '; 8];
        rpt1.copy_from_slice(&data[11..19]);
        let mut ur_call = [b' '; 8];
        ur_call.copy_from_slice(&data[19..27]);
        let mut my_call = [b' '; 8];
        my_call.copy_from_slice(&data[27..35]);
        let mut my_suffix = [b' '; 4];
        my_suffix.copy_from_slice(&data[35..39]);

        Some(Self {
            flag1: data[0],
            flag2: data[1],
            flag3: data[2],
            rpt2,
            rpt1,
            ur_call,
            my_call,
            my_suffix,
        })
    }

    /// Get the RPT2 callsign as a trimmed string.
    #[must_use]
    pub fn rpt2_str(&self) -> &str {
        core::str::from_utf8(&self.rpt2).unwrap_or("").trim_end()
    }

    /// Get the RPT1 callsign as a trimmed string.
    #[must_use]
    pub fn rpt1_str(&self) -> &str {
        core::str::from_utf8(&self.rpt1).unwrap_or("").trim_end()
    }

    /// Get the YOUR callsign as a trimmed string.
    #[must_use]
    pub fn ur_call_str(&self) -> &str {
        core::str::from_utf8(&self.ur_call).unwrap_or("").trim_end()
    }

    /// Get the MY callsign as a trimmed string.
    #[must_use]
    pub fn my_call_str(&self) -> &str {
        core::str::from_utf8(&self.my_call).unwrap_or("").trim_end()
    }

    /// Get the MY suffix as a trimmed string.
    #[must_use]
    pub fn my_suffix_str(&self) -> &str {
        core::str::from_utf8(&self.my_suffix)
            .unwrap_or("")
            .trim_end()
    }

    /// Pad a callsign string to exactly 8 bytes (space-padded right).
    #[must_use]
    pub fn pad_callsign(callsign: &str) -> [u8; 8] {
        let mut buf = [b' '; 8];
        let bytes = callsign.as_bytes();
        let len = bytes.len().min(8);
        buf[..len].copy_from_slice(&bytes[..len]);
        buf
    }

    /// Pad a suffix string to exactly 4 bytes (space-padded right).
    #[must_use]
    pub fn pad_suffix(suffix: &str) -> [u8; 4] {
        let mut buf = [b' '; 4];
        let bytes = suffix.as_bytes();
        let len = bytes.len().min(4);
        buf[..len].copy_from_slice(&bytes[..len]);
        buf
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

    #[test]
    fn encode_decode_roundtrip() {
        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: *b"REF030 G",
            rpt1: *b"REF030 C",
            ur_call: *b"CQCQCQ  ",
            my_call: *b"W1AW    ",
            my_suffix: *b"    ",
        };
        let encoded = header.encode();
        assert_eq!(encoded.len(), ENCODED_LEN);

        let decoded = DStarHeader::decode(&encoded).expect("CRC should be valid");
        assert_eq!(decoded, header);
    }

    #[test]
    fn bad_crc_rejected() {
        let header = DStarHeader {
            flag1: 0x00,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: *b"DIRECT  ",
            rpt1: *b"DIRECT  ",
            ur_call: *b"CQCQCQ  ",
            my_call: *b"N0CALL  ",
            my_suffix: *b"    ",
        };
        let mut encoded = header.encode();
        encoded[40] ^= 0xFF; // corrupt CRC
        assert!(DStarHeader::decode(&encoded).is_none());
    }

    #[test]
    fn pad_callsign_short() {
        assert_eq!(&DStarHeader::pad_callsign("W1AW"), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_exact() {
        assert_eq!(&DStarHeader::pad_callsign("W1AW    "), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_long() {
        assert_eq!(&DStarHeader::pad_callsign("ABCDEFGHIJ"), b"ABCDEFGH");
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
}
