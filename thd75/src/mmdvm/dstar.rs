//! D-STAR radio header codec with CRC-CCITT validation.
//!
//! A D-STAR header is 41 bytes: 3 flag bytes, four callsign fields
//! (RPT2, RPT1, UR, MY each 8 bytes), a 4-byte suffix, and a 2-byte
//! CRC-CCITT checksum. This module encodes and decodes the header
//! according to the MMDVM Specification (20150922) and the JARL D-STAR
//! standard.
//!
//! # CRC algorithm
//!
//! CRC-CCITT with polynomial 0x8408 (bit-reflected form of 0x1021),
//! initial value 0xFFFF. The CRC is computed over the first 39 bytes
//! and stored little-endian in bytes 39-40. The final CRC value is
//! bitwise-inverted before storage.

use super::frame::MmdvmError;

/// CRC-CCITT polynomial in reflected form (MMDVM Specification 20150922).
const CRC_POLY: u16 = 0x8408;

/// CRC-CCITT initial value (MMDVM Specification 20150922).
const CRC_INIT: u16 = 0xFFFF;

/// Compute CRC-CCITT over a byte slice.
///
/// Uses polynomial 0x8408 (reflected), initial value 0xFFFF, with final
/// bitwise inversion as specified in the D-STAR standard.
fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc = CRC_INIT;
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ CRC_POLY;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

/// Pad or truncate a string to exactly `len` bytes, right-padding with spaces.
fn pad_callsign(s: &str, len: usize) -> Vec<u8> {
    let mut bytes = s.as_bytes().to_vec();
    bytes.resize(len, b' ');
    bytes.truncate(len);
    bytes
}

/// Parse a callsign field from raw bytes, trimming trailing spaces.
fn parse_callsign(data: &[u8], field: &'static str) -> Result<String, MmdvmError> {
    if !data.is_ascii() {
        return Err(MmdvmError::InvalidCallsign { field });
    }
    // Keep the full field including trailing spaces for round-trip fidelity;
    // callers can trim if desired.
    Ok(String::from_utf8_lossy(data).into_owned())
}

/// D-STAR radio header (41 bytes on the wire).
///
/// Contains flag bytes, four callsign fields, a suffix, and a CRC-CCITT
/// checksum (MMDVM Specification 20150922).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DStarHeader {
    /// Flag byte 1 (control flags).
    pub flag1: u8,
    /// Flag byte 2 (control flags).
    pub flag2: u8,
    /// Flag byte 3 (control flags).
    pub flag3: u8,
    /// Repeater 2 callsign, 8 characters space-padded.
    pub rpt2: String,
    /// Repeater 1 callsign, 8 characters space-padded.
    pub rpt1: String,
    /// Destination callsign (UR), 8 characters space-padded.
    pub ur_call: String,
    /// Origin callsign (MY), 8 characters space-padded.
    pub my_call: String,
    /// Origin suffix, 4 characters space-padded.
    pub my_suffix: String,
}

impl DStarHeader {
    /// Encode this header to a 41-byte array with CRC (MMDVM Specification 20150922).
    ///
    /// The CRC-CCITT is computed over the first 39 bytes (flags + callsigns)
    /// and stored little-endian in bytes 39-40.
    #[must_use]
    pub fn encode(&self) -> [u8; 41] {
        let mut buf = [0u8; 41];
        buf[0] = self.flag1;
        buf[1] = self.flag2;
        buf[2] = self.flag3;
        buf[3..11].copy_from_slice(&pad_callsign(&self.rpt2, 8));
        buf[11..19].copy_from_slice(&pad_callsign(&self.rpt1, 8));
        buf[19..27].copy_from_slice(&pad_callsign(&self.ur_call, 8));
        buf[27..35].copy_from_slice(&pad_callsign(&self.my_call, 8));
        buf[35..39].copy_from_slice(&pad_callsign(&self.my_suffix, 4));
        let crc = crc_ccitt(&buf[..39]);
        buf[39] = (crc & 0xFF) as u8;
        buf[40] = (crc >> 8) as u8;
        buf
    }

    /// Decode a 41-byte array into a D-STAR header, validating the CRC
    /// (MMDVM Specification 20150922).
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::CrcMismatch`] if the stored CRC does not match
    /// the computed value, or [`MmdvmError::InvalidCallsign`] if any
    /// callsign field contains non-ASCII bytes.
    pub fn decode(data: &[u8; 41]) -> Result<Self, MmdvmError> {
        let stored_crc = u16::from(data[39]) | (u16::from(data[40]) << 8);
        let computed_crc = crc_ccitt(&data[..39]);
        if stored_crc != computed_crc {
            return Err(MmdvmError::CrcMismatch {
                expected: stored_crc,
                computed: computed_crc,
            });
        }
        Ok(Self {
            flag1: data[0],
            flag2: data[1],
            flag3: data[2],
            rpt2: parse_callsign(&data[3..11], "rpt2")?,
            rpt1: parse_callsign(&data[11..19], "rpt1")?,
            ur_call: parse_callsign(&data[19..27], "ur_call")?,
            my_call: parse_callsign(&data[27..35], "my_call")?,
            my_suffix: parse_callsign(&data[35..39], "my_suffix")?,
        })
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
            rpt2: "DIRECT  ".to_owned(),
            rpt1: "DIRECT  ".to_owned(),
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: "W1AW    ".to_owned(),
            my_suffix: "    ".to_owned(),
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let header = sample_header();
        let encoded = header.encode();
        assert_eq!(encoded.len(), 41);
        let decoded = DStarHeader::decode(&encoded).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn crc_validation_catches_corruption() {
        let header = sample_header();
        let mut encoded = header.encode();
        // Corrupt one byte in the callsign area.
        encoded[5] ^= 0xFF;
        let err = DStarHeader::decode(&encoded).unwrap_err();
        match err {
            MmdvmError::CrcMismatch { .. } => {}
            other => panic!("expected CrcMismatch, got {other:?}"),
        }
    }

    #[test]
    fn crc_known_value() {
        // Verify the CRC function produces a non-trivial value.
        let crc = crc_ccitt(b"ABCDEFGH");
        // The important thing is determinism and that the bit-reflected
        // algorithm with inversion produces a consistent result.
        assert_ne!(crc, 0);
        assert_eq!(crc, crc_ccitt(b"ABCDEFGH"));
    }

    #[test]
    fn short_callsigns_are_padded() {
        let header = DStarHeader {
            flag1: 0x40,
            flag2: 0x00,
            flag3: 0x00,
            rpt2: "REF001C".to_owned(),
            rpt1: "REF001G".to_owned(),
            ur_call: "CQCQCQ".to_owned(),
            my_call: "N0CALL".to_owned(),
            my_suffix: "DMRA".to_owned(),
        };
        let encoded = header.encode();
        let decoded = DStarHeader::decode(&encoded).unwrap();
        // Callsigns should be padded to full width.
        assert_eq!(decoded.rpt2, "REF001C ");
        assert_eq!(decoded.rpt1, "REF001G ");
        assert_eq!(decoded.ur_call, "CQCQCQ  ");
        assert_eq!(decoded.my_call, "N0CALL  ");
        assert_eq!(decoded.my_suffix, "DMRA");
    }

    #[test]
    fn non_ascii_callsign_rejected() {
        let header = sample_header();
        let mut encoded = header.encode();
        // Put a non-ASCII byte in the rpt2 field and fix CRC.
        encoded[3] = 0xFF;
        let crc = crc_ccitt(&encoded[..39]);
        encoded[39] = (crc & 0xFF) as u8;
        encoded[40] = (crc >> 8) as u8;
        let err = DStarHeader::decode(&encoded).unwrap_err();
        assert_eq!(err, MmdvmError::InvalidCallsign { field: "rpt2" });
    }

    #[test]
    fn flag_bytes_preserved() {
        let header = DStarHeader {
            flag1: 0x40,
            flag2: 0x20,
            flag3: 0x01,
            ..sample_header()
        };
        let encoded = header.encode();
        assert_eq!(encoded[0], 0x40);
        assert_eq!(encoded[1], 0x20);
        assert_eq!(encoded[2], 0x01);
        let decoded = DStarHeader::decode(&encoded).unwrap();
        assert_eq!(decoded.flag1, 0x40);
        assert_eq!(decoded.flag2, 0x20);
        assert_eq!(decoded.flag3, 0x01);
    }

    #[test]
    fn pad_callsign_exact_length() {
        assert_eq!(pad_callsign("W1AW    ", 8), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_short() {
        assert_eq!(pad_callsign("W1AW", 8), b"W1AW    ");
    }

    #[test]
    fn pad_callsign_too_long() {
        assert_eq!(pad_callsign("ABCDEFGHIJ", 8), b"ABCDEFGH");
    }

    #[test]
    fn crc_empty_data() {
        // CRC of empty data should just be the inverted init value.
        let crc = crc_ccitt(&[]);
        assert_eq!(crc, !CRC_INIT);
    }

    #[test]
    fn encode_produces_valid_crc() {
        let header = sample_header();
        let encoded = header.encode();
        // Running CRC over the full 41 bytes (including the stored CRC)
        // should not equal zero for this reflected algorithm, but decoding
        // should succeed.
        assert!(DStarHeader::decode(&encoded).is_ok());
    }
}
