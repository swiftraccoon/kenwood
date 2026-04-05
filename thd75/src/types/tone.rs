//! Tone, DCS (Digital-Coded Squelch), and related signaling types for the
//! TH-D75 transceiver.
//!
//! Contains CTCSS (Continuous Tone-Coded Squelch System) frequency and DCS
//! code lookup tables, along with validated newtype wrappers and signaling
//! mode enums.
//!
//! Per User Manual Chapter 10:
//!
//! - CTCSS, Tone, and DCS cannot be active simultaneously on a channel.
//! - Pressing `[TONE]` cycles: Tone -> CTCSS (CT) -> DCS -> Cross Tone -> Off.
//!   When APRS Voice Alert is configured, Voice Alert ON is added to the cycle.
//! - CTCSS/DCS settings can be applied independently per VFO, Memory Channel,
//!   and Call mode. Changes in Memory/Call mode are temporary unless stored.
//! - Both CTCSS and DCS support frequency/code scanning (`[F]` + hold `[TONE]`)
//!   to identify an incoming signal's tone or code.
//!
//! See User Manual Chapters 7 and 10 for full CTCSS/DCS/Cross Tone details.

use crate::error::ValidationError;

/// CTCSS (Continuous Tone-Coded Squelch System) frequency table.
///
/// 50 sub-audible tone frequencies in Hz, used for selective calling.
/// Indexed by [`ToneCode`]. Table is at firmware address `0xC003C694`.
/// The D75 supports indices 0-49 (50 tones), extending the D74's 35-tone
/// table with 15 additional tones including interleaved entries in the
/// 159-200 Hz range (159.8, 165.5, 171.3, 177.3, 183.5, 189.9, 196.6,
/// 199.5) and high-frequency tones (210.7-254.1 Hz).
///
/// This table corresponds to **KI4LAX TABLE A** in the CAT command
/// reference, which maps hex indices 0x00-0x31 to CTCSS tone frequencies.
pub const CTCSS_FREQUENCIES: [f64; 50] = [
    67.0, 69.3, 71.9, 74.4, 77.0, 79.7, 82.5, 85.4, 88.5, 91.5, // 0-9
    94.8, 97.4, 100.0, 103.5, 107.2, 110.9, 114.8, 118.8, 123.0, 127.3, // 10-19
    131.8, 136.5, 141.3, 146.2, 151.4, 156.7, 159.8, 162.2, 165.5, 167.9, // 20-29
    171.3, 173.8, 177.3, 179.9, 183.5, 186.2, 189.9, 192.8, 196.6, 199.5, // 30-39
    203.5, 206.5, 210.7, 218.1, 225.7, 229.1, 233.6, 241.8, 250.3, 254.1, // 40-49
];

/// DCS (Digital-Coded Squelch) code table.
///
/// 104 digital squelch codes used for selective calling. Indexed by
/// [`DcsCode`]. Table is at firmware address `0xC0086FC4`.
///
/// This table corresponds to **KI4LAX TABLE B** in the CAT command
/// reference, which maps hex indices 0x00-0x67 to DCS code numbers.
pub const DCS_CODES: [u16; 104] = [
    23, 25, 26, 31, 32, 36, 43, 47, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125, 131,
    132, 134, 143, 145, 152, 155, 156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 243, 244, 245,
    246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315, 325, 331, 332, 343, 346, 351,
    356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455, 462, 464, 465, 466,
    503, 506, 516, 523, 526, 532, 546, 565, 606, 612, 624, 627, 631, 632, 654, 662, 664, 703, 712,
    723, 731, 732, 734, 743, 754,
];

/// Validated CTCSS tone code (index into [`CTCSS_FREQUENCIES`]).
///
/// Wraps a `u8` index in the range 0..=49. The D75 supports 50 CTCSS tones
/// (indices 0-49), as confirmed by the firmware tone table at `0xC003C694`.
/// Use [`ToneCode::frequency_hz`] to look up the corresponding CTCSS
/// frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ToneCode(u8);

impl ToneCode {
    /// Creates a new `ToneCode` from a raw index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ToneCodeOutOfRange`] if `index >= 50`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index < 50 {
            Ok(Self(index))
        } else {
            Err(ValidationError::ToneCodeOutOfRange(index))
        }
    }

    /// Returns the raw index into the CTCSS frequency table.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Returns the CTCSS frequency in Hz for this tone code.
    #[must_use]
    pub const fn frequency_hz(self) -> f64 {
        CTCSS_FREQUENCIES[self.0 as usize]
    }
}

impl std::fmt::Display for ToneCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} Hz)", self.0, CTCSS_FREQUENCIES[self.0 as usize])
    }
}

/// Validated DCS code (index into [`DCS_CODES`]).
///
/// Wraps a `u8` index in the range 0..=103. Use [`DcsCode::code_value`]
/// to look up the corresponding DCS code number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DcsCode(u8);

impl DcsCode {
    /// Creates a new `DcsCode` from a raw index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DcsCodeInvalid`] if `index >= 104`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index < 104 {
            Ok(Self(index))
        } else {
            Err(ValidationError::DcsCodeInvalid(index))
        }
    }

    /// Returns the raw index into the DCS code table.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Returns the DCS code value for this index.
    #[must_use]
    pub const fn code_value(self) -> u16 {
        DCS_CODES[self.0 as usize]
    }
}

impl std::fmt::Display for DcsCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D{:03}", DCS_CODES[self.0 as usize])
    }
}

/// Tone signaling mode for a channel.
///
/// Maps to the tone-mode field in the `FO` and `ME` commands.
/// Corresponds to **KI4LAX TABLE F** in the CAT command reference
/// (index 0 = Off, 1 = CTCSS, 2 = DCS).
///
/// Per User Manual Chapter 10: CTCSS does not make conversations
/// private -- it only relieves you from hearing unwanted conversations.
/// When CTCSS or DCS is active during scan, scan stops on any signal
/// but immediately resumes if the signal lacks the matching tone/code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneMode {
    /// No tone signaling (index 0).
    Off = 0,
    /// CTCSS tone (index 1).
    Ctcss = 1,
    /// DCS code (index 2).
    Dcs = 2,
}

impl std::fmt::Display for ToneMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Ctcss => f.write_str("CTCSS"),
            Self::Dcs => f.write_str("DCS"),
        }
    }
}

impl TryFrom<u8> for ToneMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Ctcss),
            2 => Ok(Self::Dcs),
            _ => Err(ValidationError::ToneModeOutOfRange(value)),
        }
    }
}

impl From<ToneMode> for u8 {
    fn from(mode: ToneMode) -> Self {
        mode as Self
    }
}

/// CTCSS encode/decode mode (byte 0x09 bits \[1:0\]).
///
/// Controls whether CTCSS tones are encoded on transmit, decoded on
/// receive, or both. Uses [`ValidationError::ToneModeOutOfRange`] for
/// out-of-range values since it shares the same valid range (0-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CtcssMode {
    /// CTCSS disabled (index 0).
    Off = 0,
    /// Encode and decode CTCSS (index 1).
    On = 1,
    /// Encode CTCSS on transmit only (index 2).
    EncodeOnly = 2,
}

impl TryFrom<u8> for CtcssMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::On),
            2 => Ok(Self::EncodeOnly),
            _ => Err(ValidationError::ToneModeOutOfRange(value)),
        }
    }
}

impl From<CtcssMode> for u8 {
    fn from(mode: CtcssMode) -> Self {
        mode as Self
    }
}

/// Data speed for packet/digital modes.
///
/// Maps to the data-speed field in the `FO` and `ME` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSpeed {
    /// 1200 bps (index 0).
    Bps1200 = 0,
    /// 9600 bps (index 1).
    Bps9600 = 1,
}

impl std::fmt::Display for DataSpeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bps1200 => f.write_str("1200 bps"),
            Self::Bps9600 => f.write_str("9600 bps"),
        }
    }
}

impl TryFrom<u8> for DataSpeed {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bps1200),
            1 => Ok(Self::Bps9600),
            _ => Err(ValidationError::DataSpeedOutOfRange(value)),
        }
    }
}

impl From<DataSpeed> for u8 {
    fn from(speed: DataSpeed) -> Self {
        speed as Self
    }
}

/// Channel lockout mode for scan operations.
///
/// Maps to the lockout field in the `ME` command.
///
/// Per User Manual Chapter 9: lockout can be set individually for all
/// 1000 memory channels but cannot be set for program scan memory
/// (L0/U0 through L49/U49). The lockout icon appears to the right of
/// the channel number when a locked-out channel is recalled. Lockout
/// cannot be toggled in VFO or CALL channel mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockoutMode {
    /// Not locked out (index 0).
    Off = 0,
    /// Locked out of scan (index 1).
    On = 1,
    /// Group lockout (index 2).
    Group = 2,
}

impl std::fmt::Display for LockoutMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::On => f.write_str("Locked Out"),
            Self::Group => f.write_str("Group Lockout"),
        }
    }
}

impl TryFrom<u8> for LockoutMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::On),
            2 => Ok(Self::Group),
            _ => Err(ValidationError::LockoutOutOfRange(value)),
        }
    }
}

impl From<LockoutMode> for u8 {
    fn from(mode: LockoutMode) -> Self {
        mode as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tone_code_valid_range() {
        for i in 0u8..50 {
            assert!(ToneCode::new(i).is_ok());
        }
    }

    #[test]
    fn tone_code_invalid() {
        assert!(ToneCode::new(50).is_err());
        assert!(ToneCode::new(255).is_err());
    }

    #[test]
    fn tone_code_frequency_lookup() {
        let tc = ToneCode::new(0).unwrap();
        assert!((tc.frequency_hz() - 67.0).abs() < f64::EPSILON);
        let tc = ToneCode::new(42).unwrap();
        assert!((tc.frequency_hz() - 210.7).abs() < f64::EPSILON);
        let tc = ToneCode::new(49).unwrap();
        assert!((tc.frequency_hz() - 254.1).abs() < f64::EPSILON);
    }

    #[test]
    fn ctcss_table_completeness() {
        assert_eq!(CTCSS_FREQUENCIES.len(), 50);
        assert!((CTCSS_FREQUENCIES[0] - 67.0).abs() < f64::EPSILON);
        assert!((CTCSS_FREQUENCIES[42] - 210.7).abs() < f64::EPSILON);
        assert!((CTCSS_FREQUENCIES[43] - 218.1).abs() < f64::EPSILON);
        assert!((CTCSS_FREQUENCIES[49] - 254.1).abs() < f64::EPSILON);
    }

    #[test]
    fn dcs_code_valid() {
        assert!(DcsCode::new(0).is_ok());
        assert!(DcsCode::new(103).is_ok());
    }

    #[test]
    fn dcs_code_invalid() {
        assert!(DcsCode::new(104).is_err());
        assert!(DcsCode::new(255).is_err());
    }

    #[test]
    fn dcs_code_table_completeness() {
        assert_eq!(DCS_CODES.len(), 104);
        assert_eq!(DCS_CODES[0], 23);
        assert_eq!(DCS_CODES[103], 754);
    }

    #[test]
    fn dcs_code_value_lookup() {
        let dc = DcsCode::new(0).unwrap();
        assert_eq!(dc.code_value(), 23);
    }

    #[test]
    fn tone_mode_valid_range() {
        assert!(ToneMode::try_from(0u8).is_ok());
        assert!(ToneMode::try_from(1u8).is_ok());
        assert!(ToneMode::try_from(2u8).is_ok());
    }

    #[test]
    fn tone_mode_invalid() {
        assert!(ToneMode::try_from(3u8).is_err());
    }

    #[test]
    fn data_speed_valid() {
        assert!(DataSpeed::try_from(0u8).is_ok());
        assert!(DataSpeed::try_from(1u8).is_ok());
        assert!(DataSpeed::try_from(2u8).is_err());
    }

    #[test]
    fn lockout_mode_valid() {
        assert!(LockoutMode::try_from(0u8).is_ok());
        assert!(LockoutMode::try_from(2u8).is_ok());
        assert!(LockoutMode::try_from(3u8).is_err());
    }

    #[test]
    fn ctcss_mode_valid() {
        assert!(CtcssMode::try_from(0u8).is_ok());
        assert!(CtcssMode::try_from(2u8).is_ok());
        assert!(CtcssMode::try_from(3u8).is_err());
    }
}
