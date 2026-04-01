//! FM repeater configuration types.
//!
//! The TH-D75 supports FM repeater operation with configurable offset
//! frequency, automatic offset based on the operating band, a 1750 Hz
//! tone burst for repeater access (common in Europe and Japan), and
//! reverse function for listening on the repeater input frequency.
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

/// 1750 Hz tone burst hold mode.
///
/// Controls how the 1750 Hz tone burst is generated when pressing
/// the designated key:
/// - `Off`: Tone burst is disabled.
/// - `Hold`: Tone is transmitted for as long as the key is held.
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
