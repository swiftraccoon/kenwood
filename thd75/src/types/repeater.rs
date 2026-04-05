//! FM repeater configuration types.
//!
//! The TH-D75 supports FM repeater operation with configurable offset
//! frequency, automatic offset based on the operating band, a 1750 Hz
//! tone burst for repeater access (common in Europe and Japan), and
//! reverse function for listening on the repeater input frequency.
//!
//! # Offset frequency (per User Manual Chapter 7)
//!
//! Menu No. 140 sets the offset frequency. Range: 0.00-29.95 MHz in
//! 50 kHz steps. Defaults: 600 kHz on 144 MHz, 5 MHz on 430/440 MHz.
//!
//! # Automatic Repeater Offset (per User Manual Chapter 7)
//!
//! Menu No. 141 enables automatic offset. When active, the radio
//! selects offset direction and activates the Tone function based on
//! the operating frequency and band plan.
//!
//! ## TH-D75A auto-offset directions
//!
//! | Frequency Range | Offset |
//! |----------------|--------|
//! | 145.100-145.499 MHz | -600 kHz |
//! | 146.000-146.399 MHz | +600 kHz |
//! | 146.600-146.999 MHz | -600 kHz |
//! | 147.000-147.399 MHz | +600 kHz |
//! | 147.600-147.999 MHz | -600 kHz |
//! | 223.920-224.999 MHz | -1.6 MHz |
//! | 442.000-444.999 MHz | +5 MHz |
//! | 447.000-449.999 MHz | -5 MHz |
//! | All other frequencies | Simplex |
//!
//! ## TH-D75E auto-offset directions
//!
//! | Frequency Range | Offset |
//! |----------------|--------|
//! | 145.600-145.799 MHz | -600 kHz |
//! | All other frequencies | Simplex |
//!
//! # Offset direction (per User Manual Chapter 7)
//!
//! `[F]`, `[REV]` cycles: Simplex -> + -> - -> Simplex.
//! TH-D75E on 430 MHz adds: = (-7.6 MHz).
//! If the offset TX frequency falls outside the band, TX is inhibited.
//!
//! # Reverse function (per User Manual Chapter 7)
//!
//! `[REV]` exchanges TX and RX frequencies so you can check signal
//! strength from the other station directly. The R icon appears when
//! active. Reverse is inhibited if the resulting TX or RX frequency
//! would be out of range. Auto Repeater Offset does not function when
//! Reverse is on.
//!
//! These types model repeater settings from Chapter 7 of the TH-D75
//! user manual. Derived from the capability gap analysis features 198-199
//! and feature 137 (1750 Hz tone).

// ---------------------------------------------------------------------------
// Repeater configuration
// ---------------------------------------------------------------------------

/// FM repeater operating configuration.
///
/// Controls the offset frequency, automatic offset selection, and
/// 1750 Hz tone burst behavior for accessing FM repeaters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepeaterConfig {
    /// Repeater offset frequency in Hz.
    ///
    /// The offset is the difference between the repeater's transmit and
    /// receive frequencies. Common values are 600 kHz (2 m band) and
    /// 5 MHz (70 cm band).
    pub offset_frequency: u32,
    /// Enable automatic offset selection based on the operating frequency.
    ///
    /// When enabled, the radio automatically selects the correct offset
    /// direction (positive or negative) and frequency based on the
    /// band plan.
    pub auto_offset: bool,
    /// 1750 Hz tone burst hold mode.
    ///
    /// The 1750 Hz tone burst is used to access repeaters that require
    /// a burst tone instead of or in addition to CTCSS/DCS.
    pub tone_burst_1750hz: ToneBurstHold,
}

impl Default for RepeaterConfig {
    fn default() -> Self {
        Self {
            offset_frequency: 600_000,
            auto_offset: true,
            tone_burst_1750hz: ToneBurstHold::Off,
        }
    }
}

// ---------------------------------------------------------------------------
// 1750 Hz tone burst
// ---------------------------------------------------------------------------

/// 1750 Hz tone burst hold mode (Menu No. 143).
///
/// Controls how the 1750 Hz tone burst is generated when pressing
/// the designated key:
/// - `Off`: Tone burst is disabled.
/// - `Hold`: Tone is transmitted for as long as the key is held.
///
/// Per User Manual Chapter 7: when Hold is enabled and the `[CALL]`
/// key (or whichever key is mapped to 1750 Hz via Menu No. 142) is
/// released, the transmitter stays keyed for 2 additional seconds
/// without continuously sending the 1750 Hz tone.
///
/// On the TH-D75E, `[CALL]` defaults to 1750 Hz tone burst. On the
/// TH-D75A, `[CALL]` defaults to the Call channel function. Menu No.
/// 142 allows switching between these assignments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneBurstHold {
    /// 1750 Hz tone burst disabled.
    Off,
    /// Tone burst transmitted while key is held.
    Hold,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeater_config_default() {
        let cfg = RepeaterConfig::default();
        assert_eq!(cfg.offset_frequency, 600_000);
        assert!(cfg.auto_offset);
        assert_eq!(cfg.tone_burst_1750hz, ToneBurstHold::Off);
    }

    #[test]
    fn repeater_config_uhf_offset() {
        let cfg = RepeaterConfig {
            offset_frequency: 5_000_000,
            auto_offset: false,
            tone_burst_1750hz: ToneBurstHold::Hold,
        };
        assert_eq!(cfg.offset_frequency, 5_000_000);
        assert!(!cfg.auto_offset);
        assert_eq!(cfg.tone_burst_1750hz, ToneBurstHold::Hold);
    }
}
