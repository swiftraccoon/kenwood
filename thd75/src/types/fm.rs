//! FM broadcast radio types.
//!
//! The TH-D75 has a built-in wideband FM broadcast receiver. The user manual
//! gives its inclusive tuning range as 76.0-108.0 MHz, while the operating
//! tips describe broadcast station centers through 107.9 MHz. This type uses
//! the user manual's inclusive storage domain.
//!
//! # Reception methods (per Operating Tips §5.10.7)
//!
//! There are two ways to receive FM broadcast:
//!
//! 1. **Band B frequency selection**: Tune Band B to the FM broadcast
//!    band and select WFM mode. This uses the normal Band B receiver.
//! 2. **FM Radio mode** (Menu No. 700): A dedicated FM radio mode that
//!    runs concurrently with APRS and D-STAR operations. When a signal
//!    is received on the amateur bands, FM Radio audio is muted; it
//!    returns automatically after a configurable timeout (Menu No. 701).
//!
//! # Operation (per User Manual Chapter 21)
//!
//! - Frequency range: 76.0-108.0 MHz (WFM)
//! - 10 memory channels (FM0-FM9) with assignable names
//! - Direct frequency input supported via `[ENT]` and number keys
//! - `[MODE]` toggles between FM Radio mode (VFO tuning) and FM Radio
//!   Memory mode (FM0-FM9 recall). Cannot switch to memory mode if no
//!   stations are registered.
//! - `[A/B]` starts seek scanning; "\<\<Tuned\>\>" is displayed when a
//!   station is found
//! - FM Radio cannot be enabled when Band B is set to LF/MF, HF, 50,
//!   or FMBC bands, or when Priority Scan, WX Alert, or IF/Detect
//!   output mode is active.
//! - When FM Radio mode is on, Menu No. 105, 134, 200, 203, 204, 210,
//!   and 220 cannot be accessed.
//!
//! The FM radio state is readable through the FR CAT command. Retained
//! hardware evidence rejects FR writes, so changes use Menu 700's exact MCP
//! cell through `Radio::set_fm_radio_via_mcp`. FM memory channels are managed
//! through the radio's menu system or MCP software; no CAT command programs
//! an individual FM memory channel.
//!
//! When FM radio mode is active, the display shows "WFM" (Wide FM) and
//! the radio uses the wideband FM demodulator. The LED control setting
//! has a separate "FM Radio" option for controlling LED behavior during
//! FM broadcast reception.
//!
//! See TH-D75 User Manual, Chapter 21: FM Radio.

use std::fmt;

use crate::error::ValidationError;

use super::Frequency;

/// FM broadcast radio frequency range lower bound (76.0 MHz).
pub const FM_RADIO_MIN: Frequency = Frequency::new(76_000_000);

/// FM broadcast radio frequency range upper bound (108.0 MHz).
pub const FM_RADIO_MAX: Frequency = Frequency::new(108_000_000);

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
    number: u8,
    /// Station frequency in Hz (76,000,000 - 108,000,000).
    /// The radio tunes in 50/100 kHz steps in the FM broadcast band.
    frequency: Frequency,
    /// Station name (up to 8 UTF-8 encoded bytes).
    name: String,
}

impl FmRadioChannel {
    /// Create a new FM radio channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FmRadioChannelOutOfRange`] outside FM0-FM9,
    /// [`ValidationError::FmRadioFrequencyOutOfRange`] outside 76-108 MHz
    /// inclusive, or [`ValidationError::FmRadioNameTooLong`] when `name`
    /// exceeds eight UTF-8 encoded bytes. Fields are private, so validation
    /// cannot be bypassed after construction.
    pub fn new(number: u8, frequency: Frequency, name: String) -> Result<Self, ValidationError> {
        if number >= FM_RADIO_CHANNEL_COUNT {
            return Err(ValidationError::FmRadioChannelOutOfRange { channel: number });
        }
        if !(FM_RADIO_MIN..=FM_RADIO_MAX).contains(&frequency) {
            return Err(ValidationError::FmRadioFrequencyOutOfRange {
                frequency_hz: frequency.as_hz(),
            });
        }
        if name.len() > 8 {
            return Err(ValidationError::FmRadioNameTooLong { len: name.len() });
        }
        Ok(Self {
            number,
            frequency,
            name,
        })
    }

    /// Channel number (0-9).
    #[must_use]
    pub const fn number(&self) -> u8 {
        self.number
    }

    /// Station frequency in Hz.
    #[must_use]
    pub const fn frequency(&self) -> Frequency {
        self.frequency
    }

    /// Station name (may be empty).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the frequency in MHz as a floating-point value.
    #[must_use]
    pub fn frequency_mhz(&self) -> f64 {
        self.frequency.as_mhz()
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
///
/// Per User Manual Chapter 21: the auto-mute return time (Menu No. 701,
/// 1-10 seconds, default 3) controls how long after an amateur-band
/// signal ends before the FM radio audio resumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FmRadioMode {
    /// Direct frequency tuning: tune to any frequency in the
    /// 76-108 MHz FM broadcast band using the dial or up/down keys.
    Tuning,
    /// Memory channel mode: recall one of the 10 FM memory
    /// channels (FM0-FM9).
    Memory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm_channel_valid() -> Result<(), Box<dyn std::error::Error>> {
        let ch = FmRadioChannel::new(0, Frequency::new(89_100_000), "NPR".to_owned())?;
        assert_eq!(ch.number(), 0);
        assert_eq!(ch.frequency(), Frequency::new(89_100_000));
        assert!((ch.frequency_mhz() - 89.1).abs() < 0.001);
        assert_eq!(ch.name(), "NPR");
        Ok(())
    }

    #[test]
    fn fm_channel_invalid_number() {
        assert!(matches!(
            FmRadioChannel::new(10, Frequency::new(89_100_000), String::new()),
            Err(ValidationError::FmRadioChannelOutOfRange { channel: 10 })
        ));
    }

    #[test]
    fn fm_channel_invalid_frequency_low() {
        assert!(matches!(
            FmRadioChannel::new(0, Frequency::new(75_000_000), String::new()),
            Err(ValidationError::FmRadioFrequencyOutOfRange {
                frequency_hz: 75_000_000
            })
        ));
    }

    #[test]
    fn fm_channel_invalid_frequency_high() {
        assert!(matches!(
            FmRadioChannel::new(0, Frequency::new(109_000_000), String::new()),
            Err(ValidationError::FmRadioFrequencyOutOfRange {
                frequency_hz: 109_000_000
            })
        ));
    }

    #[test]
    fn fm_channel_name_too_long() {
        assert!(matches!(
            FmRadioChannel::new(0, Frequency::new(89_100_000), "123456789".to_owned()),
            Err(ValidationError::FmRadioNameTooLong { len: 9 })
        ));
    }

    #[test]
    fn fm_channel_display_with_name() -> Result<(), Box<dyn std::error::Error>> {
        let ch = FmRadioChannel::new(3, Frequency::new(101_100_000), "KFLY".to_owned())?;
        let s = format!("{ch}");
        assert!(s.contains("FM3"));
        assert!(s.contains("101.1"));
        assert!(s.contains("KFLY"));
        Ok(())
    }

    #[test]
    fn fm_channel_display_without_name() -> Result<(), Box<dyn std::error::Error>> {
        let ch = FmRadioChannel::new(0, Frequency::new(88_500_000), String::new())?;
        let s = format!("{ch}");
        assert!(s.contains("FM0"));
        assert!(s.contains("88.5"));
        assert!(!s.contains('('));
        Ok(())
    }

    #[test]
    fn fm_channel_boundary_frequencies() {
        // Lower bound
        let low = FmRadioChannel::new(0, FM_RADIO_MIN, String::new());
        assert!(low.is_ok());
        // Upper bound
        let high = FmRadioChannel::new(0, FM_RADIO_MAX, String::new());
        assert!(high.is_ok());
    }

    #[test]
    fn fm_radio_mode_debug() {
        assert_eq!(format!("{:?}", FmRadioMode::Tuning), "Tuning");
        assert_eq!(format!("{:?}", FmRadioMode::Memory), "Memory");
    }
}
