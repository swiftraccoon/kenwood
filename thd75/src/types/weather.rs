//! Weather alert types (TH-D75A only -- not available on TH-D75E).
//!
//! The TH-D75A (Americas model) includes a weather alert receiver that
//! monitors NOAA Weather Radio frequencies for a 1050 Hz alert tone.
//! When the tone is received, the weather alert tone sounds.
//!
//! Per User Manual Chapter 24:
//!
//! # Weather channels
//!
//! The radio has 10 weather memory channels (A1-A10):
//!
//! | Channel | Frequency | Name | Location |
//! |---------|-----------|------|----------|
//! | A1 | 162.550 MHz | WX 1 | NOAA / Canada |
//! | A2 | 162.400 MHz | WX 2 | NOAA / Canada |
//! | A3 | 162.475 MHz | WX 3 | NOAA / Canada |
//! | A4 | 162.425 MHz | WX 4 | NOAA |
//! | A5 | 162.450 MHz | WX 5 | NOAA |
//! | A6 | 162.500 MHz | WX 6 | NOAA |
//! | A7 | 162.525 MHz | WX 7 | NOAA |
//! | A8 | 161.650 MHz | WX 8 | Canada |
//! | A9 | 161.775 MHz | WX 9 | Canada |
//! | A10 | 163.275 MHz | WX 10 | -- |
//!
//! # Weather alert (Menu No. 105)
//!
//! When activated, the weather alert icon appears on the display and
//! blinks when a signal is being received. Cannot be enabled when
//! priority scan or FM radio mode is active.
//!
//! # Weather channel scan (Menu No. 136)
//!
//! Auto scanning options: Off / 15 / 30 / 60 minutes. When a time is
//! set, scanning starts automatically after the interval. Scanning
//! stops when the channel with the highest signal level is found or
//! when no signal is received on any channel.
//!
//! These types model weather alert settings from the TH-D75 user manual.
//! Derived from the capability gap analysis features 158 and 196.

// ---------------------------------------------------------------------------
// Weather configuration
// ---------------------------------------------------------------------------

/// Weather alert receiver configuration (TH-D75A only).
///
/// Controls the weather alert monitoring and automatic weather channel
/// scanning features. These features are only available on the Americas
/// model (TH-D75A); they are not present on the European model (TH-D75E).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WeatherConfig {
    /// Enable weather alert monitoring.
    ///
    /// When enabled, the radio periodically checks NOAA Weather Radio
    /// frequencies for 1050 Hz weather alert tones and sounds an alarm
    /// when detected.
    pub alert: bool,
    /// Enable automatic weather channel scanning.
    ///
    /// When enabled, the radio scans all 10 NOAA Weather Radio channels
    /// to find the strongest signal for the current location.
    pub auto_scan: bool,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_config_default() {
        let cfg = WeatherConfig::default();
        assert!(!cfg.alert);
        assert!(!cfg.auto_scan);
    }

    #[test]
    fn weather_config_enabled() {
        let cfg = WeatherConfig {
            alert: true,
            auto_scan: true,
        };
        assert!(cfg.alert);
        assert!(cfg.auto_scan);
    }
}
