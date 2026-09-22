//! Tone, code and D-STAR squelch fields of `FO` and `ME` channel records.
//!
//! Code tables follow the User Manual (Tone Frequency, DCS Code). Wire fields
//! were read from firmware 1.02 channel records; the correspondence between a
//! wire index and a table entry has not been checked against the panel.

use std::fmt;

use crate::error::ValidationError;

/// The 50 selectable tone and CTCSS frequencies in tenths of a hertz,
/// in the User Manual's table order (`01` 67.0 Hz through `50` 254.1 Hz).
pub const TONE_FREQUENCIES_DECIHERTZ: [u16; 50] = [
    670, 693, 719, 744, 770, 797, 825, 854, 885, 915, 948, 974, 1000, 1035, 1072, 1109, 1148, 1188,
    1230, 1273, 1318, 1365, 1413, 1462, 1514, 1567, 1598, 1622, 1655, 1679, 1713, 1738, 1773, 1799,
    1835, 1862, 1899, 1928, 1966, 1995, 2035, 2065, 2107, 2181, 2257, 2291, 2336, 2418, 2503, 2541,
];

/// The 104 DCS codes as printed (octal digits), in the User Manual's order.
pub const DCS_CODES: [u16; 104] = [
    23, 25, 26, 31, 32, 36, 43, 47, 51, 53, 54, 65, 71, 72, 73, 74, 114, 115, 116, 122, 125, 131,
    132, 134, 143, 145, 152, 155, 156, 162, 165, 172, 174, 205, 212, 223, 225, 226, 243, 244, 245,
    246, 251, 252, 255, 261, 263, 265, 266, 271, 274, 306, 311, 315, 325, 331, 332, 343, 346, 351,
    356, 364, 365, 371, 411, 412, 413, 423, 431, 432, 445, 446, 452, 454, 455, 462, 464, 465, 466,
    503, 506, 516, 523, 526, 532, 546, 565, 606, 612, 624, 627, 631, 632, 654, 662, 664, 703, 712,
    723, 731, 732, 734, 743, 754,
];

/// Index into [`TONE_FREQUENCIES_DECIHERTZ`] carried by the transmit tone and
/// receive CTCSS fields; two wire digits, `00` through `49`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToneCode {
    index: u8,
    decihertz: u16,
}

impl ToneCode {
    /// Highest index.
    pub const MAX: u8 = 49;

    /// Validate a table index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub fn new(index: u8) -> Result<Self, ValidationError> {
        TONE_FREQUENCIES_DECIHERTZ
            .get(usize::from(index))
            .map(|&decihertz| Self { index, decihertz })
            .ok_or_else(|| ValidationError::SettingOutOfRange {
                setting: "tone code",
                value: u64::from(index),
                max: u64::from(Self::MAX),
            })
    }

    /// The table index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.index
    }

    /// The table entry in tenths of a hertz.
    #[must_use]
    pub const fn as_decihertz(self) -> u16 {
        self.decihertz
    }
}

impl fmt::Display for ToneCode {
    /// The frequency as the manual prints it, for example `88.5 Hz`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let decihertz = self.as_decihertz();
        write!(formatter, "{}.{} Hz", decihertz / 10, decihertz % 10)
    }
}

/// Index into [`DCS_CODES`]; three wire digits, `000` through `103`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DcsCode {
    index: u8,
    code: u16,
}

impl DcsCode {
    /// Highest index.
    pub const MAX: u8 = 103;

    /// Validate a table index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub fn new(index: u8) -> Result<Self, ValidationError> {
        DCS_CODES
            .get(usize::from(index))
            .map(|&code| Self { index, code })
            .ok_or_else(|| ValidationError::SettingOutOfRange {
                setting: "DCS code",
                value: u64::from(index),
                max: u64::from(Self::MAX),
            })
    }

    /// The table index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.index
    }

    /// The printed code, for example `23` for `D023`.
    #[must_use]
    pub const fn code(self) -> u16 {
        self.code
    }
}

impl fmt::Display for DcsCode {
    /// The code as the manual prints it, for example `D023`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "D{:03}", self.code())
    }
}

/// The single active tone-signaling function of a channel.
///
/// The record carries four enable digits (tone, CTCSS, DCS, cross tone); the
/// panel cycles them as one selection, so at most one is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneMode {
    /// No tone signaling.
    Off,
    /// Transmit tone only.
    Tone,
    /// Receive CTCSS (and transmit the same tone).
    Ctcss,
    /// DCS.
    Dcs,
    /// Cross tone: different transmit and receive signaling.
    CrossTone,
}

impl ToneMode {
    /// Combine the four enable digits, in wire order: tone, CTCSS, DCS,
    /// cross tone.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::MultipleToneModes`] when more than one
    /// digit is set.
    pub const fn from_flags(
        [tone, ctcss, dcs, cross_tone]: [bool; 4],
    ) -> Result<Self, ValidationError> {
        match (tone, ctcss, dcs, cross_tone) {
            (false, false, false, false) => Ok(Self::Off),
            (true, false, false, false) => Ok(Self::Tone),
            (false, true, false, false) => Ok(Self::Ctcss),
            (false, false, true, false) => Ok(Self::Dcs),
            (false, false, false, true) => Ok(Self::CrossTone),
            _ => Err(ValidationError::MultipleToneModes {
                tone,
                ctcss,
                dcs,
                cross_tone,
            }),
        }
    }

    /// The four enable digits in wire order: tone, CTCSS, DCS, cross tone.
    #[must_use]
    pub const fn to_flags(self) -> [bool; 4] {
        match self {
            Self::Off => [false, false, false, false],
            Self::Tone => [true, false, false, false],
            Self::Ctcss => [false, true, false, false],
            Self::Dcs => [false, false, true, false],
            Self::CrossTone => [false, false, false, true],
        }
    }
}

impl fmt::Display for ToneMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Tone => "Tone",
            Self::Ctcss => "CTCSS",
            Self::Dcs => "DCS",
            Self::CrossTone => "Cross Tone",
        })
    }
}

/// The one-digit hexadecimal cross-tone field of a channel record.
///
/// Records read from firmware 1.02 carried `0` on FM settings and `8` on a
/// DR repeater entry; the digit is retained exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CrossToneField(u8);

impl CrossToneField {
    /// Highest wire value, `F`.
    pub const MAX: u8 = 15;

    /// Validate a nibble.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "cross tone field",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The nibble.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }

    /// The uppercase hexadecimal wire digit.
    #[must_use]
    pub const fn wire_char(self) -> char {
        match self.0 {
            raw @ 0..=9 => (b'0' + raw) as char,
            raw => (b'A' + raw - 10) as char,
        }
    }

    /// Parse the one-character wire field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidHexDigit`] for anything other than
    /// one uppercase hexadecimal digit.
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::InvalidHexDigit {
            text: text.to_owned(),
        };
        let mut chars = text.chars();
        let (Some(digit), None) = (chars.next(), chars.next()) else {
            return Err(invalid());
        };
        let raw = match digit {
            '0'..='9' => digit as u8 - b'0',
            'A'..='F' => digit as u8 - b'A' + 10,
            _ => return Err(invalid()),
        };
        Self::new(raw)
    }
}

impl fmt::Display for CrossToneField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.wire_char())
    }
}

/// D-STAR digital squelch type (User Manual, Menu 620).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigitalSquelch {
    /// Off (`0`).
    Off = 0,
    /// Code squelch (`1`).
    Code = 1,
    /// Callsign squelch (`2`).
    Callsign = 2,
}

impl DigitalSquelch {
    /// The wire value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for DigitalSquelch {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Code),
            2 => Ok(Self::Callsign),
            _ => Err(ValidationError::SettingOutOfRange {
                setting: "digital squelch",
                value: u64::from(value),
                max: 2,
            }),
        }
    }
}

impl fmt::Display for DigitalSquelch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "off",
            Self::Code => "code squelch",
            Self::Callsign => "callsign squelch",
        })
    }
}

/// D-STAR digital code, `00` through `99` (User Manual, Menu 621).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DigitalCode(u8);

impl DigitalCode {
    /// Highest code.
    pub const MAX: u8 = 99;

    /// Validate a code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value > Self::MAX {
            Err(ValidationError::SettingOutOfRange {
                setting: "digital code",
                value: value as u64,
                max: Self::MAX as u64,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// The code.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl fmt::Display for DigitalCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:02}", self.0)
    }
}

/// D-STAR destination callsign (URCALL) of a channel record.
///
/// Up to eight bytes of uppercase letters, digits, spaces and `/`, kept
/// exactly as the radio sends them; the VFO default is `CQCQCQ` and an
/// unset field is empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UrCallsign(String);

impl UrCallsign {
    /// Maximum byte length.
    pub const MAX_LEN: usize = 8;

    /// Validate a callsign field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] for more than eight
    /// bytes or a byte outside `A`-`Z`, `0`-`9`, space and `/`.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        let valid_byte = |byte: u8| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ' || byte == b'/'
        };
        if text.len() > Self::MAX_LEN || !text.bytes().all(valid_byte) {
            return Err(ValidationError::InvalidCallsignText {
                text: text.to_owned(),
            });
        }
        Ok(Self(text.to_owned()))
    }

    /// The field exactly as carried.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UrCallsign {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn tone_and_dcs_tables_are_bounded_and_ordered() -> TestResult {
        assert_eq!(ToneCode::new(0)?.as_decihertz(), 670);
        assert_eq!(ToneCode::new(8)?.to_string(), "88.5 Hz");
        assert_eq!(ToneCode::new(49)?.to_string(), "254.1 Hz");
        for (number, decihertz) in [
            (1, 670),
            (13, 1000),
            (15, 1072),
            (16, 1109),
            (17, 1148),
            (26, 1567),
            (34, 1799),
            (44, 2181),
            (50, 2541),
        ] {
            assert_eq!(
                ToneCode::new(number - 1)?.as_decihertz(),
                decihertz,
                "manual table number {number}"
            );
        }
        assert!(ToneCode::new(50).is_err());
        assert!(
            TONE_FREQUENCIES_DECIHERTZ
                .windows(2)
                .all(|pair| matches!(pair, [low, high] if low < high)),
            "tone table must ascend"
        );
        assert_eq!(DcsCode::new(0)?.to_string(), "D023");
        assert_eq!(DcsCode::new(103)?.code(), 754);
        assert!(DcsCode::new(104).is_err());
        assert!(
            DCS_CODES
                .windows(2)
                .all(|pair| matches!(pair, [low, high] if low < high)),
            "DCS table must ascend"
        );
        assert!(
            DCS_CODES.iter().all(|code| {
                let text = format!("{code:03}");
                text.bytes().all(|byte| (b'0'..=b'7').contains(&byte))
            }),
            "DCS codes are octal digits"
        );
        Ok(())
    }

    #[test]
    fn tone_mode_is_one_hot() -> TestResult {
        assert_eq!(ToneMode::from_flags([false; 4])?, ToneMode::Off);
        assert_eq!(
            ToneMode::from_flags([false, true, false, false])?,
            ToneMode::Ctcss
        );
        for mode in [
            ToneMode::Off,
            ToneMode::Tone,
            ToneMode::Ctcss,
            ToneMode::Dcs,
            ToneMode::CrossTone,
        ] {
            assert_eq!(ToneMode::from_flags(mode.to_flags())?, mode);
        }
        let conflict = ToneMode::from_flags([true, true, false, false]);
        assert!(
            matches!(conflict, Err(ValidationError::MultipleToneModes { .. })),
            "{conflict:?}"
        );
        Ok(())
    }

    #[test]
    fn cross_tone_field_is_one_hex_digit() -> TestResult {
        assert_eq!(CrossToneField::from_wire_str("8")?.as_raw(), 8);
        assert_eq!(CrossToneField::from_wire_str("F")?.wire_char(), 'F');
        for text in ["", "10", "a", "G"] {
            assert!(CrossToneField::from_wire_str(text).is_err(), "{text:?}");
        }
        assert!(CrossToneField::new(16).is_err());
        Ok(())
    }

    #[test]
    fn digital_squelch_code_and_callsign_domains() -> TestResult {
        assert_eq!(DigitalSquelch::try_from(2)?, DigitalSquelch::Callsign);
        assert!(DigitalSquelch::try_from(3).is_err());
        assert_eq!(DigitalCode::new(99)?.to_string(), "99");
        assert!(DigitalCode::new(100).is_err());
        assert_eq!(UrCallsign::new("CQCQCQ")?.as_str(), "CQCQCQ");
        assert_eq!(UrCallsign::new("")?.as_str(), "");
        assert_eq!(UrCallsign::new("W1AW  /A")?.to_string(), "W1AW  /A");
        for text in ["W1AW     ", "w1aw", "W1AW,", "W1AW\r"] {
            assert!(UrCallsign::new(text).is_err(), "{text:?}");
        }
        Ok(())
    }
}
