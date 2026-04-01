//! CW (Continuous Wave / Morse Code) configuration types.
//!
//! The TH-D75 supports CW mode on SSB-capable bands with configurable
//! break-in timing, sidetone pitch frequency, and CW-on-FM operation.
//! Break-in allows the receiver to activate between transmitted elements;
//! full break-in (QSK) provides instantaneous receive between every
//! dit and dah.
//!
//! These types model CW settings from the TH-D75's menu system.
//! Derived from the capability gap analysis features 133-136.

// ---------------------------------------------------------------------------
// CW configuration
// ---------------------------------------------------------------------------

/// CW (Morse code) operating configuration.
///
/// Controls break-in timing, sidetone pitch, and the CW-on-FM feature
/// that allows sending CW tones over an FM carrier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CwConfig {
    /// Enable break-in (receive between transmitted CW elements).
    pub break_in: bool,
    /// Break-in delay time (time to hold TX after last element).
    pub delay_time: CwDelay,
    /// CW sidetone pitch frequency.
    pub pitch_frequency: CwPitch,
    /// Enable CW tone generation on FM mode.
    pub cw_on_fm: bool,
}

impl Default for CwConfig {
    fn default() -> Self {
        Self {
            break_in: false,
            delay_time: CwDelay::Ms300,
            pitch_frequency: CwPitch::default(),
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

/// CW sidetone pitch frequency (400-1000 Hz in 50 Hz steps).
///
/// The sidetone is the locally generated audio tone heard while
/// transmitting CW. The pitch can be adjusted to the operator's
/// preference between 400 Hz and 1000 Hz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CwPitch(u16);

impl CwPitch {
    /// Minimum pitch frequency in Hz.
    pub const MIN_HZ: u16 = 400;

    /// Maximum pitch frequency in Hz.
    pub const MAX_HZ: u16 = 1000;

    /// Step size in Hz.
    pub const STEP_HZ: u16 = 50;

    /// Creates a new CW pitch frequency.
    ///
    /// # Errors
    ///
    /// Returns `None` if the frequency is outside the 400-1000 Hz range
    /// or is not a multiple of 50 Hz.
    #[must_use]
    pub const fn new(hz: u16) -> Option<Self> {
        if hz >= Self::MIN_HZ && hz <= Self::MAX_HZ && hz % Self::STEP_HZ == 0 {
            Some(Self(hz))
        } else {
            None
        }
    }

    /// Returns the pitch frequency in Hz.
    #[must_use]
    pub const fn hz(self) -> u16 {
        self.0
    }
}

impl Default for CwPitch {
    fn default() -> Self {
        // 800 Hz is a typical default CW pitch.
        Self(800)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cw_config_default() {
        let cfg = CwConfig::default();
        assert!(!cfg.break_in);
        assert_eq!(cfg.delay_time, CwDelay::Ms300);
        assert_eq!(cfg.pitch_frequency.hz(), 800);
        assert!(!cfg.cw_on_fm);
    }

    #[test]
    fn cw_pitch_valid_range() {
        let mut count = 0;
        let mut hz = CwPitch::MIN_HZ;
        while hz <= CwPitch::MAX_HZ {
            assert!(CwPitch::new(hz).is_some(), "valid pitch {hz} rejected");
            count += 1;
            hz += CwPitch::STEP_HZ;
        }
        // 400, 450, 500, ..., 1000 = 13 valid values.
        assert_eq!(count, 13);
    }

    #[test]
    fn cw_pitch_invalid_below_min() {
        assert!(CwPitch::new(350).is_none());
    }

    #[test]
    fn cw_pitch_invalid_above_max() {
        assert!(CwPitch::new(1050).is_none());
    }

    #[test]
    fn cw_pitch_invalid_not_step() {
        assert!(CwPitch::new(425).is_none());
        assert!(CwPitch::new(801).is_none());
    }

    #[test]
    fn cw_pitch_boundary_values() {
        assert!(CwPitch::new(400).is_some());
        assert!(CwPitch::new(1000).is_some());
    }

    #[test]
    fn cw_pitch_default() {
        let pitch = CwPitch::default();
        assert_eq!(pitch.hz(), 800);
    }
}
