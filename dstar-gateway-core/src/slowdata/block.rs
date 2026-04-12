//! Parsed slow data block types.

use crate::header::DStarHeader;

/// Slow data block type, recovered from the high nibble of byte 0
/// after descrambling.
///
/// Reference: `ircDDBGateway/Common/DStarDefines.h:85-92`
/// (`SLOW_DATA_TYPE_GPS = 0x30`, `SLOW_DATA_TYPE_TEXT = 0x40`, etc.).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowDataBlockKind {
    /// `0x3X` — GPS NMEA passthrough.
    Gps,
    /// `0x4X` — 20-character status text.
    Text,
    /// `0x5X` — header retransmission.
    HeaderRetx,
    /// `0x8X` — fast data variant 1.
    FastData1,
    /// `0x9X` — fast data variant 2.
    FastData2,
    /// `0xCX` — squelch / control.
    Squelch,
    /// Any other high nibble.
    Unknown {
        /// The high nibble that didn't match.
        high_nibble: u8,
    },
}

impl SlowDataBlockKind {
    /// Decode the high nibble of the type byte into a kind.
    #[must_use]
    pub const fn from_type_byte(byte: u8) -> Self {
        match byte & 0xF0 {
            0x30 => Self::Gps,
            0x40 => Self::Text,
            0x50 => Self::HeaderRetx,
            0x80 => Self::FastData1,
            0x90 => Self::FastData2,
            0xC0 => Self::Squelch,
            other => Self::Unknown {
                high_nibble: other >> 4,
            },
        }
    }
}

/// 20-character status text frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowDataText {
    /// Trimmed text (UTF-8 lossy).
    pub text: String,
}

/// A complete slow data block extracted from a stream of voice frames.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlowDataBlock {
    /// GPS NMEA sentence.
    Gps(String),
    /// 20-character text frame.
    Text(SlowDataText),
    /// Retransmitted header.
    HeaderRetx(DStarHeader),
    /// Fast data block 1 — opaque payload.
    FastData(Vec<u8>),
    /// Squelch / control marker.
    Squelch {
        /// Squelch code byte.
        code: u8,
    },
    /// Unknown block type — payload preserved verbatim.
    Unknown {
        /// The type byte that didn't match a known kind.
        type_byte: u8,
        /// Block payload.
        payload: Vec<u8>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_from_text_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0x4F),
            SlowDataBlockKind::Text
        );
    }

    #[test]
    fn kind_from_gps_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0x3A),
            SlowDataBlockKind::Gps
        );
    }

    #[test]
    fn kind_from_header_retx_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0x52),
            SlowDataBlockKind::HeaderRetx
        );
    }

    #[test]
    fn kind_from_fast_data1_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0x83),
            SlowDataBlockKind::FastData1
        );
    }

    #[test]
    fn kind_from_fast_data2_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0x92),
            SlowDataBlockKind::FastData2
        );
    }

    #[test]
    fn kind_from_squelch_byte() {
        assert_eq!(
            SlowDataBlockKind::from_type_byte(0xC5),
            SlowDataBlockKind::Squelch
        );
    }

    #[test]
    fn kind_from_unknown_byte() {
        let kind = SlowDataBlockKind::from_type_byte(0xA5);
        assert!(matches!(
            kind,
            SlowDataBlockKind::Unknown { high_nibble: 0xA }
        ));
    }
}
