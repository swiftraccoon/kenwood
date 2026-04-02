//! Band selection for the TH-D75 transceiver.

use std::fmt;

use crate::error::ValidationError;

/// Radio band index (0-13).
///
/// The TH-D75 uses a numeric band index in the `FO` and `ME` commands.
/// Variants `A` and `B` correspond to the two main VFO bands; the
/// remaining `Band2`..`Band13` map to additional sub-band selections.
///
/// # Band architecture (per Kenwood Operating Tips §1.1, §5.9)
///
/// - **Band A** (upper display): Amateur-only TX/RX at 144 MHz, 220 MHz
///   (TH-D75A only), and 430 MHz. Supports FM and DV modes.
/// - **Band B** (lower display): Wideband RX from 0.1–524 MHz. Supports
///   FM, DV, AM, LSB, USB, CW, NFM, WFM, and DR modes. Band B has an
///   independent receiver chain (separate VCO/PLL/IF per the service
///   manual), so both bands receive simultaneously.
///
/// Band A is the CTRL/PTT band by default. Band B supports all
/// demodulation modes including SSB/CW with DSP and an IF receive filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    /// Band A — amateur TX/RX (144/220/430 MHz). Index 0.
    A = 0,
    /// Band B — wideband RX (0.1–524 MHz, all modes). Index 1.
    B = 1,
    /// Band 2 (index 2).
    Band2 = 2,
    /// Band 3 (index 3).
    Band3 = 3,
    /// Band 4 (index 4).
    Band4 = 4,
    /// Band 5 (index 5).
    Band5 = 5,
    /// Band 6 (index 6).
    Band6 = 6,
    /// Band 7 (index 7).
    Band7 = 7,
    /// Band 8 (index 8).
    Band8 = 8,
    /// Band 9 (index 9).
    Band9 = 9,
    /// Band 10 (index 10).
    Band10 = 10,
    /// Band 11 (index 11).
    Band11 = 11,
    /// Band 12 (index 12).
    Band12 = 12,
    /// Band 13 (index 13).
    Band13 = 13,
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => f.write_str("A"),
            Self::B => f.write_str("B"),
            other => write!(f, "Band {}", u8::from(*other)),
        }
    }
}

impl TryFrom<u8> for Band {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
            2 => Ok(Self::Band2),
            3 => Ok(Self::Band3),
            4 => Ok(Self::Band4),
            5 => Ok(Self::Band5),
            6 => Ok(Self::Band6),
            7 => Ok(Self::Band7),
            8 => Ok(Self::Band8),
            9 => Ok(Self::Band9),
            10 => Ok(Self::Band10),
            11 => Ok(Self::Band11),
            12 => Ok(Self::Band12),
            13 => Ok(Self::Band13),
            _ => Err(ValidationError::BandOutOfRange(value)),
        }
    }
}

impl From<Band> for u8 {
    fn from(band: Band) -> Self {
        band as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ValidationError;

    #[test]
    fn band_valid_range() {
        for i in 0u8..14 {
            assert!(Band::try_from(i).is_ok(), "Band({i}) should be valid");
        }
    }

    #[test]
    fn band_invalid() {
        assert!(Band::try_from(14).is_err());
        assert!(Band::try_from(255).is_err());
    }

    #[test]
    fn band_round_trip() {
        for i in 0u8..14 {
            let val = Band::try_from(i).unwrap();
            assert_eq!(u8::from(val), i);
        }
    }

    #[test]
    fn band_error_variant() {
        let err = Band::try_from(14).unwrap_err();
        assert!(matches!(err, ValidationError::BandOutOfRange(14)));
    }

    #[test]
    fn band_display() {
        assert_eq!(Band::A.to_string(), "A");
        assert_eq!(Band::B.to_string(), "B");
        assert_eq!(Band::Band5.to_string(), "Band 5");
        assert_eq!(Band::Band13.to_string(), "Band 13");
    }
}
