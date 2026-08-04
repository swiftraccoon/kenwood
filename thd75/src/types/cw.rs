//! CW (Continuous Wave / Morse Code) settings types.
//!
//! The TH-D75 supports CW mode on SSB-capable bands with configurable
//! break-in timing, sidetone pitch frequency, and CW-on-FM operation.
//! Break-in allows the receiver to activate between transmitted elements;
//! full break-in (QSK) provides instantaneous receive between every
//! dit and dah.
//!
//! # CW reverse (per Operating Tips §5.10.2)
//!
//! Menu No. 171 controls CW sideband selection:
//! - Normal: USB (Upper Side Band)
//! - Reverse: LSB (Lower Side Band)
//!
//! These types model CW settings from the TH-D75's menu system.

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// CW settings
// ---------------------------------------------------------------------------

/// CW (Morse code) operating settings.
///
/// Controls break-in timing, sidetone pitch, and the CW-on-FM feature
/// that allows sending CW tones over an FM carrier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CwSettings {
    /// Enable break-in (receive between transmitted CW elements).
    pub break_in: bool,
    /// Break-in delay time (time to hold TX after last element).
    pub delay_time: CwDelay,
    /// CW sidetone pitch frequency.
    pub pitch_frequency: CwPitch,
    /// Enable CW tone generation on FM mode.
    pub cw_on_fm: bool,
}

impl CwSettings {
    /// Returns the documented TH-D75 factory CW settings.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self {
            break_in: false,
            delay_time: CwDelay::Ms300,
            pitch_frequency: CwPitch::factory_default(),
            cw_on_fm: false,
        }
    }
}

// ---------------------------------------------------------------------------
// CW delay time
// ---------------------------------------------------------------------------

/// CW break-in delay time.
///
/// Controls how long the transmitter stays keyed after the last CW
/// element before switching back to receive. `Full` provides QSK
/// (full break-in) with instantaneous TX/RX switching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CwDelay {
    /// Full break-in (QSK) -- instantaneous TX/RX switching.
    Full,
    /// 50 ms delay.
    Ms50,
    /// 100 ms delay.
    Ms100,
    /// 150 ms delay.
    Ms150,
    /// 200 ms delay.
    Ms200,
    /// 250 ms delay.
    Ms250,
    /// 300 ms delay.
    Ms300,
}

// ---------------------------------------------------------------------------
// CW pitch frequency
// ---------------------------------------------------------------------------

/// CW sidetone pitch frequency (400-1000 Hz in 100 Hz steps).
///
/// The sidetone is the locally generated audio tone heard while
/// transmitting CW. The pitch can be adjusted to the operator's
/// preference: 400 / 500 / 600 / 700 / 800 / 900 / 1000 Hz.
/// Default: 800 Hz.
///
/// Per User Manual Chapter 12: this also sets the center frequency
/// of the CW bandwidth filter (Menu No. 121). The CW filter is
/// centered on the pitch frequency.
///
/// Source: Operating Tips §5.10.2, Menu No. 170.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CwPitch(u16);

impl CwPitch {
    /// Minimum pitch frequency in Hz.
    pub const MIN_HZ: u16 = 400;

    /// Maximum pitch frequency in Hz.
    pub const MAX_HZ: u16 = 1000;

    /// Step size in Hz (100 Hz per Operating Tips §5.10.2).
    pub const STEP_HZ: u16 = 100;

    /// Returns the documented 800 Hz TH-D75 factory pitch.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(800)
    }

    /// Creates a new CW pitch frequency.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CwPitchOutOfRange`] if the frequency is
    /// outside 400-1000 Hz inclusive or is not a multiple of 100 Hz.
    pub const fn new(hz: u16) -> Result<Self, ValidationError> {
        if hz >= Self::MIN_HZ && hz <= Self::MAX_HZ && hz.is_multiple_of(Self::STEP_HZ) {
            Ok(Self(hz))
        } else {
            Err(ValidationError::CwPitchOutOfRange { hz })
        }
    }

    /// Returns the pitch frequency in Hz.
    #[must_use]
    pub const fn as_hz(self) -> u16 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cw_settings_factory_default() {
        let settings = CwSettings::factory_default();
        assert!(!settings.break_in);
        assert_eq!(settings.delay_time, CwDelay::Ms300);
        assert_eq!(settings.pitch_frequency.as_hz(), 800);
        assert!(!settings.cw_on_fm);
    }

    #[test]
    fn cw_pitch_valid_range() {
        let mut count = 0;
        let mut hz = CwPitch::MIN_HZ;
        while hz <= CwPitch::MAX_HZ {
            assert!(CwPitch::new(hz).is_ok(), "valid pitch {hz} rejected");
            count += 1;
            hz += CwPitch::STEP_HZ;
        }
        // 400, 500, 600, 700, 800, 900, 1000 = 7 valid values.
        assert_eq!(count, 7);
    }

    #[test]
    fn cw_pitch_invalid_below_min() {
        assert!(matches!(
            CwPitch::new(350),
            Err(ValidationError::CwPitchOutOfRange { hz: 350 })
        ));
    }

    #[test]
    fn cw_pitch_invalid_above_max() {
        assert!(matches!(
            CwPitch::new(1050),
            Err(ValidationError::CwPitchOutOfRange { hz: 1050 })
        ));
    }

    #[test]
    fn cw_pitch_invalid_not_step() {
        assert!(matches!(
            CwPitch::new(425),
            Err(ValidationError::CwPitchOutOfRange { hz: 425 })
        ));
        assert!(matches!(
            CwPitch::new(801),
            Err(ValidationError::CwPitchOutOfRange { hz: 801 })
        ));
    }

    #[test]
    fn cw_pitch_boundary_values() {
        assert!(CwPitch::new(400).is_ok());
        assert!(CwPitch::new(1000).is_ok());
    }

    #[test]
    fn cw_pitch_default() {
        let pitch = CwPitch::factory_default();
        assert_eq!(pitch.as_hz(), 800);
    }
}
