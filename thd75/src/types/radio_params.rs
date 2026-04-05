//! Validated parameter types for radio CAT command methods.
//!
//! These newtypes and enums enforce valid ranges at construction time
//! for parameters that the radio methods previously accepted as raw `u8`.

use std::fmt;

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// SquelchLevel (0-6)
// ---------------------------------------------------------------------------

/// Squelch threshold level (0-6).
///
/// 0 = open (no squelch), 6 = maximum squelch. Used by the `SQ` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SquelchLevel(u8);

impl SquelchLevel {
    /// Open squelch (level 0).
    pub const OPEN: Self = Self(0);
    /// Maximum squelch (level 6).
    pub const MAX: Self = Self(6);

    /// Creates a new `SquelchLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 6`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 6 {
            Err(ValidationError::SettingOutOfRange {
                name: "squelch level",
                value,
                detail: "must be 0-6",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for SquelchLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SquelchLevel> for u8 {
    fn from(level: SquelchLevel) -> Self {
        level.0
    }
}

impl fmt::Display for SquelchLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQ{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// AfGainLevel (0-99)
// ---------------------------------------------------------------------------

/// Audio frequency gain level (0-99).
///
/// Controls the volume output level. Used by the `AG` CAT command.
/// The wire format is a bare 3-digit zero-padded decimal (`AG 015\r`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AfGainLevel(u8);

impl AfGainLevel {
    /// Creates a new `AfGainLevel` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 99`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 99 {
            Err(ValidationError::SettingOutOfRange {
                name: "AF gain level",
                value,
                detail: "must be 0-99",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for AfGainLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AfGainLevel> for u8 {
    fn from(level: AfGainLevel) -> Self {
        level.0
    }
}

impl fmt::Display for AfGainLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// SMeterReading (0-5)
// ---------------------------------------------------------------------------

/// S-meter reading (0-5).
///
/// The radio returns 0-5 via the `SM` command, mapping to signal strengths
/// S0, S1, S3, S5, S7, S9 respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SMeterReading(u8);

impl SMeterReading {
    /// Creates a new `SMeterReading` from a raw value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] if `value > 5`.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > 5 {
            Err(ValidationError::SettingOutOfRange {
                name: "S-meter reading",
                value,
                detail: "must be 0-5",
            })
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the raw `u8` value.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Returns the approximate S-unit string.
    #[must_use]
    pub const fn s_unit(&self) -> &'static str {
        match self.0 {
            0 => "S0",
            1 => "S1",
            2 => "S3",
            3 => "S5",
            4 => "S7",
            5 => "S9",
            _ => "S?",
        }
    }
}

impl TryFrom<u8> for SMeterReading {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SMeterReading> for u8 {
    fn from(reading: SMeterReading) -> Self {
        reading.0
    }
}

impl fmt::Display for SMeterReading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.s_unit())
    }
}

// ---------------------------------------------------------------------------
// VfoMemoryMode
// ---------------------------------------------------------------------------

/// VFO/Memory/Call/Weather operating mode.
///
/// Controls which channel selection mode the band is in.
/// Used by the `VM` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VfoMemoryMode {
    /// VFO mode — frequency entered directly (index 0).
    Vfo = 0,
    /// Memory channel mode — recalls stored channels (index 1).
    Memory = 1,
    /// Call channel mode — quick-access channel (index 2).
    Call = 2,
    /// Weather channel mode — NOAA weather frequencies (index 3).
    Weather = 3,
}

impl fmt::Display for VfoMemoryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vfo => f.write_str("VFO"),
            Self::Memory => f.write_str("Memory"),
            Self::Call => f.write_str("Call"),
            Self::Weather => f.write_str("Weather"),
        }
    }
}

impl TryFrom<u8> for VfoMemoryMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Vfo),
            1 => Ok(Self::Memory),
            2 => Ok(Self::Call),
            3 => Ok(Self::Weather),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "VFO/memory mode",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<VfoMemoryMode> for u8 {
    fn from(mode: VfoMemoryMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// FilterMode
// ---------------------------------------------------------------------------

/// Receiver filter mode selection.
///
/// Selects which demodulator's filter width to read or set.
/// Used by the `SF` CAT command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FilterMode {
    /// SSB (LSB/USB) filter (index 0).
    Ssb = 0,
    /// CW filter (index 1).
    Cw = 1,
    /// AM filter (index 2).
    Am = 2,
}

impl fmt::Display for FilterMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ssb => f.write_str("SSB"),
            Self::Cw => f.write_str("CW"),
            Self::Am => f.write_str("AM"),
        }
    }
}

impl TryFrom<u8> for FilterMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ssb),
            1 => Ok(Self::Cw),
            2 => Ok(Self::Am),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "filter mode",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<FilterMode> for u8 {
    fn from(mode: FilterMode) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squelch_level_valid() {
        for v in 0..=6 {
            assert!(SquelchLevel::new(v).is_ok());
        }
        assert!(SquelchLevel::new(7).is_err());
    }

    #[test]
    fn squelch_level_round_trip() {
        let sq = SquelchLevel::new(4).unwrap();
        assert_eq!(u8::from(sq), 4);
        assert_eq!(sq.as_u8(), 4);
    }

    #[test]
    fn af_gain_valid() {
        assert!(AfGainLevel::new(0).is_ok());
        assert!(AfGainLevel::new(99).is_ok());
        assert!(AfGainLevel::new(100).is_err());
    }

    #[test]
    fn smeter_s_units() {
        assert_eq!(SMeterReading::new(0).unwrap().s_unit(), "S0");
        assert_eq!(SMeterReading::new(5).unwrap().s_unit(), "S9");
        assert!(SMeterReading::new(6).is_err());
    }

    #[test]
    fn vfo_memory_mode_round_trip() {
        for v in 0..=3 {
            let mode = VfoMemoryMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(VfoMemoryMode::try_from(4).is_err());
    }

    #[test]
    fn filter_mode_round_trip() {
        for v in 0..=2 {
            let mode = FilterMode::try_from(v).unwrap();
            assert_eq!(u8::from(mode), v);
        }
        assert!(FilterMode::try_from(3).is_err());
    }
}
