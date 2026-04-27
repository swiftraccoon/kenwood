//! Band selection for the TH-D75 transceiver.

use std::fmt;

use crate::error::ValidationError;

/// Radio band index (0-13).
///
/// The TH-D75 uses a numeric band index in the `FO` and `ME` commands.
/// Variants `A` and `B` correspond to the two main VFO bands; the
/// remaining `Band2`..`Band13` map to additional sub-band selections.
///
/// # Band architecture (per Kenwood Operating Tips §1.1, §5.9; User Manual Chapter 5)
///
/// - **Band A** (upper display): Amateur-only TX/RX at 144 MHz, 220 MHz
///   (TH-D75A only), and 430 MHz. Supports FM and DV modes.
///   Pressing and holding `[Left]/[Right]` cycles: 144 <-> 220 <-> 430 MHz.
///   Band A uses a double super heterodyne receiver (1st IF 57.15 MHz,
///   2nd IF 450 kHz) with VCO/PLL IC800 and IF IC IC900 (AK2365AU).
/// - **Band B** (lower display): Wideband RX from 0.1-524 MHz. Supports
///   FM, DV, AM, LSB, USB, CW, NFM, WFM (FM Radio mode only), and DR.
///   Band B has an independent receiver chain with its own VCO/PLL IC700,
///   IF IC IC1002 (AK2365AU), and a third IF stage at 10.8 kHz via 3rd
///   mixer IC1001 for AM/SSB/CW demodulation. 1st IF is 58.05 MHz, 2nd
///   IF is 450 kHz. This independent hardware allows both bands to
///   receive simultaneously.
///   Pressing and holding `[Left]/[Right]` cycles: 430 <-> UHF(470-524) <->
///   LF/MF(0.1-1.71) <-> HF(1.71-29.7) <-> 50(29.7-76) <-> FMBC(76-108) <->
///   118(108-136) <-> 144(136-174) <-> VHF(174-216/230) <-> 200/300(216/230-410) <-> 430 MHz.
///
/// Both bands share the MAIN MPU (IC2005, OMAP-L138), CODEC (IC2011),
/// and SUB MPU (IC1103) which controls the VCO/PLLs and IF ICs via SPI.
/// The VCO/PLL reference clocks are TCXO1 57.6 MHz (X600) and TCXO2
/// 55.95 MHz (X601), selected by analog switches IC604/IC605.
///
/// Per service manual §2.1.5, the Band B VCO/PLL (IC700) is also used
/// for transmission on all bands. Band A's VCO/PLL (IC800) handles
/// Band A 1st local oscillation only.
///
/// # Hardware signal path (per service manual §2.1.3, §2.1.4)
///
/// ```text
/// Band A RX: ANT → LNA Q404/Q406 → BPF → 1st MIX Q400 → IF 57.15MHz
///            → MCF XF1 → IF AMP Q900 → IC900 → 2nd IF 450kHz → CODEC IC2011
///
/// Band B RX: ANT → LNA Q404/Q406 → BPF → 1st MIX Q500 → IF 58.05MHz
///            → MCF XF2 → IF AMP Q1000 → IC1002 → 2nd IF 450kHz → CODEC IC2011
///            (AM/SSB/CW: → 3rd MIX IC1001 → 3rd IF 10.8kHz → CODEC)
///
/// TX (all):  CODEC IC2011 → MOD AMP IC2027 → SUB MPU IC1103 → Band B
///            VCO/PLL IC700 → RF AMP Q201 → PRE DRV IC201 → DRV AMP Q212
///            → FINAL AMP Q217/Q218 → ANT
/// ```
///
/// Band A is the CTRL/PTT band by default. Band B supports all
/// demodulation modes including SSB/CW with DSP and an IF receive filter.
///
/// # Dual/Single band display (per User Manual Chapter 5)
///
/// Press `[F]`, `[A/B]` to toggle between dual-band (both A and B visible)
/// and single-band (only the selected band visible) display modes.
///
/// # Two-wave simultaneous reception (per User Manual Chapter 2)
///
/// Supported band combinations: `VxU`, `UxV`, `UxU` (both models), plus
/// `Vx220M`, `220MxV`, `Ux220M` (TH-D75A only). D-STAR 2-wave simultaneous
/// reception is also supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(alias = "band-index")]
#[doc(alias = "rf-band")]
pub enum Band {
    /// Band A — amateur TX/RX (144/220/430 MHz). Index 0.
    A = 0,
    /// Band B — wideband RX (0.1–524 MHz, all modes). Index 1.
    B = 1,
    /// Band 2 (index 2). Extended band index used internally by the firmware
    /// for multi-band selection. Most CAT commands (e.g., `FQ`, `MD`, `SQ`)
    /// only accept Band A (0) or Band B (1); sending an extended index
    /// typically results in a `?` error response.
    Band2 = 2,
    /// Band 3 (index 3). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band3 = 3,
    /// Band 4 (index 4). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band4 = 4,
    /// Band 5 (index 5). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band5 = 5,
    /// Band 6 (index 6). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band6 = 6,
    /// Band 7 (index 7). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band7 = 7,
    /// Band 8 (index 8). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band8 = 8,
    /// Band 9 (index 9). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band9 = 9,
    /// Band 10 (index 10). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band10 = 10,
    /// Band 11 (index 11). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band11 = 11,
    /// Band 12 (index 12). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band12 = 12,
    /// Band 13 (index 13). Extended firmware band index — see [`Band::Band2`]
    /// for details on CAT command restrictions.
    Band13 = 13,
}

impl Band {
    /// Number of valid band values (0-13).
    pub const COUNT: u8 = 14;
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
        for i in 0u8..Band::COUNT {
            assert!(Band::try_from(i).is_ok(), "Band({i}) should be valid");
        }
    }

    #[test]
    fn band_invalid() {
        assert!(Band::try_from(Band::COUNT).is_err());
        assert!(Band::try_from(255).is_err());
    }

    #[test]
    fn band_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        for i in 0u8..Band::COUNT {
            let val = Band::try_from(i)?;
            assert_eq!(u8::from(val), i);
        }
        Ok(())
    }

    #[test]
    fn band_error_variant() -> Result<(), Box<dyn std::error::Error>> {
        let err = Band::try_from(Band::COUNT)
            .err()
            .ok_or("expected BandOutOfRange but got Ok")?;
        assert!(
            matches!(err, ValidationError::BandOutOfRange(14)),
            "expected BandOutOfRange(14), got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn band_display() {
        assert_eq!(Band::A.to_string(), "A");
        assert_eq!(Band::B.to_string(), "B");
        assert_eq!(Band::Band5.to_string(), "Band 5");
        assert_eq!(Band::Band13.to_string(), "Band 13");
    }
}
