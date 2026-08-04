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
//! These types model weather alert settings from the TH-D75 menu and MCP
//! storage domains.

// ---------------------------------------------------------------------------
// Weather settings
// ---------------------------------------------------------------------------

/// Weather alert receiver settings (TH-D75A only).
///
/// Controls the weather alert monitoring and automatic weather channel
/// scanning features. These features are only available on the Americas
/// model (TH-D75A); they are not present on the European model (TH-D75E).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WeatherSettings {
    /// Enable weather alert monitoring.
    ///
    /// When enabled, the radio periodically checks NOAA Weather Radio
    /// frequencies for 1050 Hz weather alert tones and sounds an alarm
    /// when detected.
    pub alert: bool,
    /// Automatic weather-channel scan interval.
    pub auto_scan: WeatherAutoScan,
}

/// Automatic weather-channel scanning interval stored by Menu No. 136.
///
/// The MCP menu field uses the non-contiguous raw values `0`, `1`, `2`, and
/// `4` for Off, 15, 30, and 60 minutes respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WeatherAutoScan {
    /// Automatic scanning disabled (raw `0`).
    #[default]
    Off,
    /// Scan every 15 minutes (raw `1`).
    Minutes15,
    /// Scan every 30 minutes (raw `2`).
    Minutes30,
    /// Scan every 60 minutes (raw `4`).
    Minutes60,
}

impl TryFrom<u8> for WeatherAutoScan {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Minutes15),
            2 => Ok(Self::Minutes30),
            4 => Ok(Self::Minutes60),
            _ => Err(crate::error::ValidationError::SettingOutOfRange {
                name: "automatic weather scan interval",
                value,
                detail: "must be raw 0, 1, 2, or 4 (Off, 15, 30, or 60 minutes)",
            }),
        }
    }
}

impl From<WeatherAutoScan> for u8 {
    fn from(interval: WeatherAutoScan) -> Self {
        match interval {
            WeatherAutoScan::Off => 0,
            WeatherAutoScan::Minutes15 => 1,
            WeatherAutoScan::Minutes30 => 2,
            WeatherAutoScan::Minutes60 => 4,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_settings_default() {
        let settings = WeatherSettings::default();
        assert!(!settings.alert);
        assert_eq!(settings.auto_scan, WeatherAutoScan::Off);
    }

    #[test]
    fn weather_settings_enabled() {
        let settings = WeatherSettings {
            alert: true,
            auto_scan: WeatherAutoScan::Minutes30,
        };
        assert!(settings.alert);
        assert_eq!(settings.auto_scan, WeatherAutoScan::Minutes30);
    }

    #[test]
    fn weather_auto_scan_preserves_sparse_storage_domain() -> Result<(), Box<dyn std::error::Error>>
    {
        for (raw, interval) in [
            (0, WeatherAutoScan::Off),
            (1, WeatherAutoScan::Minutes15),
            (2, WeatherAutoScan::Minutes30),
            (4, WeatherAutoScan::Minutes60),
        ] {
            assert_eq!(WeatherAutoScan::try_from(raw)?, interval);
            assert_eq!(u8::from(interval), raw);
        }
        assert!(WeatherAutoScan::try_from(3).is_err());
        Ok(())
    }
}
