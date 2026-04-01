//! Weather alert types (TH-D75A only -- not available on TH-D75E).
//!
//! The TH-D75A (Americas model) includes a weather alert receiver that
//! monitors NOAA Weather Radio frequencies for Specific Area Message
//! Encoding (SAME) alerts. The radio can automatically scan weather
//! channels and sound an alarm when a weather alert is received.
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
