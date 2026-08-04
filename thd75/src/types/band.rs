//! Band selection for the TH-D75 transceiver.

use std::fmt;

use crate::error::ValidationError;

/// Selectable receiver band in the TH-D75 CAT protocol.
///
/// CAT commands identify the upper and lower receivers as `0` and `1`.
/// The radio's frequency-range selections within those receivers are not
/// additional CAT bands.
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
    /// Band A: amateur TX/RX (144/220/430 MHz). Index 0.
    A = 0,
    /// Band B: wideband RX (0.1–524 MHz, all modes). Index 1.
    B = 1,
}

impl Band {
    /// Number of selectable CAT bands.
    pub const COUNT: u8 = 2;

    /// Every selectable CAT band, in wire-value order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::A, Self::B];
}

impl fmt::Display for Band {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => f.write_str("A"),
            Self::B => f.write_str("B"),
        }
    }
}

impl TryFrom<u8> for Band {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
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
        for (wire_value, band) in (0_u8..).zip(Band::ALL) {
            assert_eq!(u8::from(band), wire_value);
            assert!(
                Band::try_from(wire_value).is_ok(),
                "Band({wire_value}) should be valid"
            );
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
            matches!(err, ValidationError::BandOutOfRange(2)),
            "expected BandOutOfRange(2), got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn band_display() {
        assert_eq!(Band::A.to_string(), "A");
        assert_eq!(Band::B.to_string(), "B");
    }
}
