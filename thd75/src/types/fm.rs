//! FM broadcast radio types.
//!
//! The TH-D75 has a built-in wideband FM broadcast receiver covering
//! 76.0-108.0 MHz. It provides 10 FM memory channels (FM0-FM9) for
//! storing favourite broadcast stations.
//!
//! The FM radio is toggled on/off via the FR CAT command (already
//! implemented as `get_fm_radio` / `set_fm_radio`). FM memory channels
//! are managed through the radio's menu system or MCP software, as there
//! is no CAT command for individual FM memory channel programming.
//!
//! When FM radio mode is active, the display shows "WFM" (Wide FM) and
//! the radio uses the wideband FM demodulator. The LED control setting
//! has a separate "FM Radio" option for controlling LED behavior during
//! FM broadcast reception.
//!
//! See TH-D75 User Manual, Chapter 21: FM Radio.

use std::fmt;

/// FM broadcast radio frequency range lower bound (76.0 MHz), in Hz.
pub const FM_RADIO_MIN_HZ: u32 = 76_000_000;

/// FM broadcast radio frequency range upper bound (108.0 MHz), in Hz.
pub const FM_RADIO_MAX_HZ: u32 = 108_000_000;

/// Number of FM radio memory channels available.
pub const FM_RADIO_CHANNEL_COUNT: u8 = 10;

/// An FM broadcast radio memory channel (FM0-FM9).
///
/// The TH-D75 provides 10 memory channels for storing FM broadcast
/// station frequencies. These are separate from the 1000 regular
/// memory channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmRadioChannel {
    /// Channel number (0-9, displayed as FM0-FM9).
    pub number: u8,
    /// Station frequency in Hz (76,000,000 - 108,000,000).
    /// The radio tunes in 50/100 kHz steps in the FM broadcast band.
    pub frequency_hz: u32,
    /// Station name (up to 8 characters).
    pub name: String,
}

impl FmRadioChannel {
    /// Create a new FM radio channel.
    ///
    /// Returns `None` if the channel number or frequency is out of range,
    /// or if the name exceeds 8 characters.
    #[must_use]
    pub fn new(number: u8, frequency_hz: u32, name: String) -> Option<Self> {
        if number >= FM_RADIO_CHANNEL_COUNT {
            return None;
        }
        if !(FM_RADIO_MIN_HZ..=FM_RADIO_MAX_HZ).contains(&frequency_hz) {
            return None;
        }
        if name.len() > 8 {
            return None;
        }
        Some(Self {
            number,
            frequency_hz,
            name,
        })
    }

    /// Returns the frequency in MHz as a floating-point value.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn frequency_mhz(&self) -> f64 {
        f64::from(self.frequency_hz) / 1_000_000.0
    }
}

impl fmt::Display for FmRadioChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.name.is_empty() {
            write!(f, "FM{}: {:.1} MHz", self.number, self.frequency_mhz())
        } else {
            write!(
                f,
                "FM{}: {:.1} MHz ({})",
                self.number,
                self.frequency_mhz(),
                self.name
            )
        }
    }
}

/// FM radio operating mode.
///
/// The TH-D75's FM broadcast receiver can operate in two modes:
/// direct frequency tuning or memory channel recall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FmRadioMode {
    /// Direct frequency tuning — tune to any frequency in the
    /// 76-108 MHz FM broadcast band using the dial or up/down keys.
    Tuning,
    /// Memory channel mode — recall one of the 10 FM memory
    /// channels (FM0-FM9).
    Memory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm_channel_valid() {
        let ch = FmRadioChannel::new(0, 89_100_000, "NPR".to_owned()).unwrap();
        assert_eq!(ch.number, 0);
        assert_eq!(ch.frequency_hz, 89_100_000);
        assert!((ch.frequency_mhz() - 89.1).abs() < 0.001);
        assert_eq!(ch.name, "NPR");
    }

    #[test]
    fn fm_channel_invalid_number() {
        assert!(FmRadioChannel::new(10, 89_100_000, String::new()).is_none());
    }

    #[test]
    fn fm_channel_invalid_frequency_low() {
        assert!(FmRadioChannel::new(0, 75_000_000, String::new()).is_none());
    }

    #[test]
    fn fm_channel_invalid_frequency_high() {
        assert!(FmRadioChannel::new(0, 109_000_000, String::new()).is_none());
    }

    #[test]
    fn fm_channel_name_too_long() {
        assert!(FmRadioChannel::new(0, 89_100_000, "123456789".to_owned()).is_none());
    }

    #[test]
    fn fm_channel_display_with_name() {
        let ch = FmRadioChannel::new(3, 101_100_000, "KFLY".to_owned()).unwrap();
        let s = format!("{ch}");
        assert!(s.contains("FM3"));
        assert!(s.contains("101.1"));
        assert!(s.contains("KFLY"));
    }

    #[test]
    fn fm_channel_display_without_name() {
        let ch = FmRadioChannel::new(0, 88_500_000, String::new()).unwrap();
        let s = format!("{ch}");
        assert!(s.contains("FM0"));
        assert!(s.contains("88.5"));
        assert!(!s.contains('('));
    }

    #[test]
    fn fm_channel_boundary_frequencies() {
        // Lower bound
        let low = FmRadioChannel::new(0, FM_RADIO_MIN_HZ, String::new());
        assert!(low.is_some());
        // Upper bound
        let high = FmRadioChannel::new(0, FM_RADIO_MAX_HZ, String::new());
        assert!(high.is_some());
    }

    #[test]
    fn fm_radio_mode_debug() {
        let _ = format!("{:?}", FmRadioMode::Tuning);
        let _ = format!("{:?}", FmRadioMode::Memory);
    }
}
