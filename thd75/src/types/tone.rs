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
/// 51 entries: 50 sub-audible CTCSS tone frequencies (indices 0-49) plus
/// the 1750 Hz tone burst at index 50. Indexed by [`ToneCode`].
///
/// The D75 supports indices 0-49, including interleaved entries in the
/// 159-200 Hz range and high-frequency tones through 254.1 Hz.
///
/// Index 50 (1750.0 Hz) is the European repeater access tone burst,
/// not a CTCSS tone. It is a short audio-frequency burst used to open
/// European repeaters.
pub const CTCSS_FREQUENCIES: [f64; 51] = [
    67.0, 69.3, 71.9, 74.4, 77.0, 79.7, 82.5, 85.4, 88.5, 91.5, // 0-9
    94.8, 97.4, 100.0, 103.5, 107.2, 110.9, 114.8, 118.8, 123.0, 127.3, // 10-19
    131.8, 136.5, 141.3, 146.2, 151.4, 156.7, 159.8, 162.2, 165.5, 167.9, // 20-29
    171.3, 173.8, 177.3, 179.9, 183.5, 186.2, 189.9, 192.8, 196.6, 199.5, // 30-39
    203.5, 206.5, 210.7, 218.1, 225.7, 229.1, 233.6, 241.8, 250.3, 254.1,  // 40-49
    1750.0, // 50: 1750 Hz tone burst (European repeater access, NOT a CTCSS tone)
];

/// DCS (Digital-Coded Squelch) code table.
///
/// 104 digital squelch codes used for selective calling. Indexed by
/// [`DcsCode`].
pub const DCS_CODES: [u16; 104] = [
    23, 25, 26, 31, 32, 36, 43, 47, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125, 131,
    132, 134, 143, 145, 152, 155, 156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 243, 244, 245,
    246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315, 325, 331, 332, 343, 346, 351,
    356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455, 462, 464, 465, 466,
    503, 506, 516, 523, 526, 532, 546, 565, 606, 612, 624, 627, 631, 632, 654, 662, 664, 703, 712,
    723, 731, 732, 734, 743, 754,
];

/// Validated tone code (index into [`CTCSS_FREQUENCIES`]).
///
/// Wraps a `u8` index in the range 0..=50. Indices 0-49 are standard
/// CTCSS sub-audible tones. Index 50 is the 1750 Hz tone burst used for
/// European repeater access; it is NOT a CTCSS tone but a short
/// audio-frequency burst.
///
/// Use [`ToneCode::as_hz`] to look up the corresponding frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToneCode(u8);

impl ToneCode {
    /// Maximum valid tone code index (inclusive).
    pub const MAX_INDEX: u8 = 50;

    /// 100.0 Hz CTCSS tone (index 12 in the TH-D75 codebook), the documented
    /// Menu No. 593 factory choice for APRS voice alert.
    pub const TONE_100HZ: Self = Self(12);

    /// Creates a new `ToneCode` from a raw index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ToneCodeOutOfRange`] if `index > 50`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index <= 50 {
            Ok(Self(index))
        } else {
            Err(ValidationError::ToneCodeOutOfRange(index))
        }
    }

    /// Returns the raw index into the CTCSS frequency table.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Returns the CTCSS frequency in Hz for this tone code.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "`ToneCode::new` validates `self.0 <= MAX_INDEX == 50` and \
                  CTCSS_FREQUENCIES has 51 entries, so this const-fn index is always \
                  in-bounds. Kept as indexed access because `slice::get` is not \
                  const-callable, so a `const fn` has no non-indexing accessor."
    )]
    pub const fn as_hz(self) -> f64 {
        CTCSS_FREQUENCIES[self.0 as usize]
    }
}

impl std::fmt::Display for ToneCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({} Hz)", self.0, self.as_hz())
    }
}

/// Validated CTCSS decoder-frequency index (0-49).
///
/// Channel records have separate transmit-tone and receive-CTCSS fields. The
/// transmit field also accepts the 1750 Hz burst at index 50, while a CTCSS
/// decoder does not. This narrower type prevents that invalid state from
/// reaching CAT or stored-channel APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CtcssCode(u8);

impl CtcssCode {
    /// Number of valid CTCSS decoder indices (`0` through `49`).
    pub const COUNT: u8 = 50;

    /// Largest valid CTCSS decoder index.
    pub const MAX_INDEX: u8 = 49;

    /// Construct a CTCSS decoder-frequency index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CtcssCodeOutOfRange`] for index 50 or above.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index <= Self::MAX_INDEX {
            Ok(Self(index))
        } else {
            Err(ValidationError::CtcssCodeOutOfRange(index))
        }
    }

    /// Return the zero-based CTCSS table index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Return the selected CTCSS frequency in hertz.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "`CtcssCode::new` proves the index is 0-49 and the table has 51 entries; indexed \
                  access keeps this method const because `slice::get` is not const-callable"
    )]
    pub const fn as_hz(self) -> f64 {
        CTCSS_FREQUENCIES[self.0 as usize]
    }
}

impl TryFrom<u8> for CtcssCode {
    type Error = ValidationError;

    fn try_from(index: u8) -> Result<Self, Self::Error> {
        Self::new(index)
    }
}

impl From<CtcssCode> for u8 {
    fn from(code: CtcssCode) -> Self {
        code.as_raw()
    }
}

impl From<CtcssCode> for ToneCode {
    fn from(code: CtcssCode) -> Self {
        Self(code.0)
    }
}

impl TryFrom<ToneCode> for CtcssCode {
    type Error = ValidationError;

    fn try_from(code: ToneCode) -> Result<Self, Self::Error> {
        Self::new(code.as_raw())
    }
}

impl std::fmt::Display for CtcssCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} ({} Hz)", self.0, self.as_hz())
    }
}

/// Validated DCS code (index into [`DCS_CODES`]).
///
/// Wraps a `u8` index in the range 0..=103. Use [`DcsCode::code_value`]
/// to look up the corresponding DCS code number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DcsCode(u8);

impl DcsCode {
    /// Number of valid DCS code indices (0-103).
    pub const COUNT: u8 = 104;

    /// Maximum valid DCS code index (inclusive).
    pub const MAX_INDEX: u8 = 103;

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
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// Returns the DCS code value for this index.
    #[must_use]
    #[expect(
        clippy::indexing_slicing,
        reason = "`DcsCode::new` validates `self.0 < COUNT == 104` and DCS_CODES has 104 \
                  entries, so this const-fn index is always in-bounds. Kept as indexed \
                  access because `slice::get` is not const-callable, so a `const fn` \
                  has no non-indexing accessor."
    )]
    pub const fn code_value(self) -> u16 {
        DCS_CODES[self.0 as usize]
    }
}

impl std::fmt::Display for DcsCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "D{:03}", self.code_value())
    }
}

/// Mutually exclusive tone signaling mode for a channel.
///
/// FO/ME represents this as four booleans. Flash stores the same state as a
/// one-hot nibble in byte `0x0A`: Tone=`8`, CTCSS=`4`, DCS=`2`, Cross=`1`.
/// Values with more than one bit set are invalid because the radio exposes
/// these modes as one selection cycle.
///
/// Per User Manual Chapter 10: CTCSS does not make conversations
/// private -- it only relieves you from hearing unwanted conversations.
/// When CTCSS or DCS is active during scan, scan stops on any signal
/// but immediately resumes if the signal lacks the matching tone/code.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneMode {
    /// No tone signaling (`0`).
    Off = 0,
    /// Transmit a tone without receive-side decoding (`8`).
    Tone = 8,
    /// Transmit and decode CTCSS (`4`).
    Ctcss = 4,
    /// Transmit and decode DCS (`2`).
    Dcs = 2,
    /// Use separate transmit and receive signaling types (`1`).
    CrossTone = 1,
}

impl ToneMode {
    /// Number of semantic tone modes.
    pub const COUNT: u8 = 5;

    /// Every tone mode in the radio's front-panel cycle order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Off,
        Self::Tone,
        Self::Ctcss,
        Self::Dcs,
        Self::CrossTone,
    ];
}

impl std::fmt::Display for ToneMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Tone => f.write_str("Tone"),
            Self::Ctcss => f.write_str("CTCSS"),
            Self::Dcs => f.write_str("DCS"),
            Self::CrossTone => f.write_str("Cross Tone"),
        }
    }
}

impl TryFrom<u8> for ToneMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::CrossTone),
            2 => Ok(Self::Dcs),
            4 => Ok(Self::Ctcss),
            8 => Ok(Self::Tone),
            _ => Err(ValidationError::ToneModeOutOfRange(value)),
        }
    }
}

impl From<ToneMode> for u8 {
    fn from(mode: ToneMode) -> Self {
        mode as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn table_entry(table: &[f64], idx: usize) -> Result<f64, BoxErr> {
        table.get(idx).copied().ok_or_else(|| {
            format!(
                "CTCSS_FREQUENCIES[{idx}] out of range (len={})",
                table.len()
            )
            .into()
        })
    }

    #[test]
    fn tone_code_valid_range() -> TestResult {
        for i in 0u8..=ToneCode::MAX_INDEX {
            let val = ToneCode::new(i)?;
            assert_eq!(val.as_raw(), i, "ToneCode round-trip failed at {i}");
        }
        Ok(())
    }

    #[test]
    fn tone_code_invalid() {
        assert!(ToneCode::new(ToneCode::MAX_INDEX + 1).is_err());
        assert!(ToneCode::new(255).is_err());
    }

    #[test]
    fn tone_code_frequency_lookup() -> TestResult {
        let tc = ToneCode::new(0)?;
        assert!((tc.as_hz() - 67.0).abs() < f64::EPSILON);
        let tc = ToneCode::new(42)?;
        assert!((tc.as_hz() - 210.7).abs() < f64::EPSILON);
        let tc = ToneCode::new(49)?;
        assert!((tc.as_hz() - 254.1).abs() < f64::EPSILON);
        // Code 50: 1750 Hz tone burst (European repeater access).
        let tc = ToneCode::new(50)?;
        assert!((tc.as_hz() - 1750.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn ctcss_table_completeness() -> TestResult {
        assert_eq!(CTCSS_FREQUENCIES.len(), 51);
        assert!((table_entry(&CTCSS_FREQUENCIES, 0)? - 67.0).abs() < f64::EPSILON);
        assert!((table_entry(&CTCSS_FREQUENCIES, 42)? - 210.7).abs() < f64::EPSILON);
        assert!((table_entry(&CTCSS_FREQUENCIES, 43)? - 218.1).abs() < f64::EPSILON);
        assert!((table_entry(&CTCSS_FREQUENCIES, 49)? - 254.1).abs() < f64::EPSILON);
        assert!((table_entry(&CTCSS_FREQUENCIES, 50)? - 1750.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn ctcss_code_valid_range_and_conversions() -> TestResult {
        for raw in 0u8..CtcssCode::COUNT {
            let code = CtcssCode::try_from(raw)?;
            assert_eq!(u8::from(code), raw);
            assert_eq!(ToneCode::from(code).as_raw(), raw);
        }
        assert!(CtcssCode::try_from(CtcssCode::COUNT).is_err());
        assert!(CtcssCode::try_from(ToneCode::new(50)?).is_err());
        Ok(())
    }

    #[test]
    fn dcs_code_valid() {
        assert!(DcsCode::new(0).is_ok());
        assert!(DcsCode::new(DcsCode::MAX_INDEX).is_ok());
    }

    #[test]
    fn dcs_code_invalid() {
        assert!(DcsCode::new(DcsCode::COUNT).is_err());
        assert!(DcsCode::new(255).is_err());
    }

    #[test]
    fn dcs_code_table_completeness() -> TestResult {
        assert_eq!(DCS_CODES.len(), 104);
        assert_eq!(*DCS_CODES.first().ok_or("DCS_CODES[0] missing")?, 23);
        assert_eq!(*DCS_CODES.get(103).ok_or("DCS_CODES[103] missing")?, 754);
        Ok(())
    }

    #[test]
    fn dcs_code_value_lookup() -> TestResult {
        let dc = DcsCode::new(0)?;
        assert_eq!(dc.code_value(), 23);
        Ok(())
    }

    #[test]
    fn tone_mode_valid_range() -> TestResult {
        assert_eq!(ToneMode::ALL.map(u8::from), [0, 8, 4, 2, 1]);
        for expected in ToneMode::ALL {
            let raw = u8::from(expected);
            let actual = ToneMode::try_from(raw)?;
            assert_eq!(actual, expected, "ToneMode round-trip failed at {raw}");
        }
        Ok(())
    }

    #[test]
    fn tone_mode_invalid() {
        for raw in [3, 5, 6, 7, 9, 15, 255] {
            assert!(ToneMode::try_from(raw).is_err(), "{raw} is not one-hot");
        }
    }
}
