//! Frequencies and repeater offsets as `FQ`, `FO` and `ME` carry them.
//!
//! Every value is ten ASCII decimal digits of hertz on the wire. The receiver
//! limits are the TM-D750A/E specification values from the User Manual; the
//! radio applies its narrower per-band limits itself.

use std::fmt;

use crate::error::ValidationError;

/// Number of ASCII digits in a wire frequency or offset field.
pub const WIRE_DIGITS: usize = 10;

/// A frequency in hertz inside the receiver range.
///
/// Band A receives 108-174, 216-260 and 410-470 MHz and Band B 108-524 MHz;
/// this type bounds their union. A write outside the selected band's range is
/// answered `N` by the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frequency(u32);

impl Frequency {
    /// Lowest receivable frequency, 108 MHz.
    pub const MIN_HZ: u32 = 108_000_000;
    /// Highest receivable frequency, 524 MHz.
    pub const MAX_HZ: u32 = 524_000_000;

    /// Validate a frequency in hertz.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::FrequencyOutOfRange`] below
    /// [`Self::MIN_HZ`] or above [`Self::MAX_HZ`].
    pub const fn new(hz: u32) -> Result<Self, ValidationError> {
        if hz < Self::MIN_HZ || hz > Self::MAX_HZ {
            Err(ValidationError::FrequencyOutOfRange { hz })
        } else {
            Ok(Self(hz))
        }
    }

    /// The frequency in hertz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        self.0
    }

    /// The ten-digit wire field, for example `0145190000`.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        format!("{:0width$}", self.0, width = WIRE_DIGITS)
    }

    /// Parse a ten-digit wire field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidWireDigits`] unless the text is
    /// exactly ten ASCII digits, then the error of [`Self::new`].
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        Self::new(parse_wire_hz(text)?)
    }

    /// Parse an unsigned decimal megahertz value with at most six fractional
    /// digits, such as `145.190` or `446`.
    ///
    /// The conversion is exact integer arithmetic. No sign, unit, exponent or
    /// digit grouping is accepted.
    ///
    /// # Examples
    ///
    /// ```
    /// use kenwood_tmd750::types::Frequency;
    ///
    /// assert_eq!(Frequency::from_mhz_str("145.190")?.as_hz(), 145_190_000);
    /// assert!(Frequency::from_mhz_str("145.190 MHz").is_err());
    /// # Ok::<(), kenwood_tmd750::ValidationError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidMegahertzText`] for any other shape
    /// and the error of [`Self::new`] for an out-of-range value.
    pub fn from_mhz_str(text: &str) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::InvalidMegahertzText {
            text: text.to_owned(),
        };
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || (text.contains('.') && fraction.is_empty())
            || fraction.len() > 6
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        let mhz: u32 = whole.parse().map_err(|_| invalid())?;
        let mut fraction_hz: u32 = 0;
        for byte in fraction
            .bytes()
            .chain(std::iter::repeat_n(b'0', 6 - fraction.len()))
        {
            fraction_hz = fraction_hz * 10 + u32::from(byte - b'0');
        }
        let hz = mhz
            .checked_mul(1_000_000)
            .and_then(|whole_hz| whole_hz.checked_add(fraction_hz))
            .ok_or_else(invalid)?;
        Self::new(hz)
    }

    /// Whether the frequency is a whole multiple of `step`.
    #[must_use]
    pub const fn is_aligned_to(self, step: StepSize) -> bool {
        self.0.is_multiple_of(step.as_hz())
    }

    /// The frequency `offset_hz` higher, when inside the receiver range.
    #[must_use]
    pub const fn checked_add_hz(self, offset_hz: u32) -> Option<Self> {
        match self.0.checked_add(offset_hz) {
            Some(hz) if hz <= Self::MAX_HZ => Some(Self(hz)),
            _ => None,
        }
    }

    /// The frequency `offset_hz` lower, when inside the receiver range.
    #[must_use]
    pub const fn checked_sub_hz(self, offset_hz: u32) -> Option<Self> {
        match self.0.checked_sub(offset_hz) {
            Some(hz) if hz >= Self::MIN_HZ => Some(Self(hz)),
            _ => None,
        }
    }
}

impl fmt::Display for Frequency {
    /// Megahertz with six fractional digits, for example `145.190000 MHz`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{:06} MHz",
            self.0 / 1_000_000,
            self.0 % 1_000_000
        )
    }
}

/// A repeater offset in hertz, 0 through 29.95 MHz (User Manual, Menu 140).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OffsetFrequency(u32);

impl OffsetFrequency {
    /// Largest configurable offset, 29.95 MHz.
    pub const MAX_HZ: u32 = 29_950_000;

    /// Validate an offset in hertz.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::OffsetOutOfRange`] above [`Self::MAX_HZ`].
    pub const fn new(hz: u32) -> Result<Self, ValidationError> {
        if hz > Self::MAX_HZ {
            Err(ValidationError::OffsetOutOfRange { hz })
        } else {
            Ok(Self(hz))
        }
    }

    /// The offset in hertz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        self.0
    }

    /// The ten-digit wire field, for example `0000600000`.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        format!("{:0width$}", self.0, width = WIRE_DIGITS)
    }
}

impl fmt::Display for OffsetFrequency {
    /// Megahertz with six fractional digits, for example `0.600000 MHz`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}.{:06} MHz",
            self.0 / 1_000_000,
            self.0 % 1_000_000
        )
    }
}

/// The transmit field of a channel record: an offset from the receive
/// frequency, or an independent transmit frequency on a split channel.
///
/// The two ranges do not overlap (offsets end at 29.95 MHz, frequencies
/// start at 108 MHz), so the wire value alone selects the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransmitSetting {
    /// Offset applied in the direction of the record's shift field.
    Offset(OffsetFrequency),
    /// Independent transmit frequency of a split channel.
    Split(Frequency),
}

impl TransmitSetting {
    /// Parse a ten-digit wire field into whichever range contains it.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidWireDigits`] for a malformed field
    /// and [`ValidationError::TransmitFieldOutOfRange`] for a value in
    /// neither range.
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        let hz = parse_wire_hz(text)?;
        if let Ok(offset) = OffsetFrequency::new(hz) {
            return Ok(Self::Offset(offset));
        }
        Frequency::new(hz)
            .map(Self::Split)
            .map_err(|_| ValidationError::TransmitFieldOutOfRange { hz })
    }

    /// The ten-digit wire field.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        match self {
            Self::Offset(offset) => offset.to_wire_string(),
            Self::Split(frequency) => frequency.to_wire_string(),
        }
    }
}

impl fmt::Display for TransmitSetting {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Offset(offset) => write!(formatter, "offset {offset}"),
            Self::Split(frequency) => write!(formatter, "split {frequency}"),
        }
    }
}

/// Tuning step selected by `SF` and stored in channel records.
///
/// The wire value is one uppercase hexadecimal digit. On firmware 1.02 each
/// value was selected with `SF`, read back, and its size measured as the
/// frequency change of one `UP` and one `DW`; `4` (8.33 kHz) is answered `N`
/// outside the 118 MHz band (User Manual, Frequency Step Size). Selecting a
/// step moves the VFO down to a multiple of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepSize {
    /// 1 kHz (`0`).
    Hz1000 = 0,
    /// 2.5 kHz (`1`).
    Hz2500 = 1,
    /// 5 kHz (`2`).
    Hz5000 = 2,
    /// 6.25 kHz (`3`).
    Hz6250 = 3,
    /// 8.33 kHz (`4`), 118 MHz band only.
    Hz8330 = 4,
    /// 10 kHz (`5`).
    Hz10000 = 5,
    /// 12.5 kHz (`6`).
    Hz12500 = 6,
    /// 15 kHz (`7`).
    Hz15000 = 7,
    /// 20 kHz (`8`).
    Hz20000 = 8,
    /// 25 kHz (`9`).
    Hz25000 = 9,
    /// 30 kHz (`A`).
    Hz30000 = 10,
    /// 50 kHz (`B`).
    Hz50000 = 11,
    /// 100 kHz (`C`).
    Hz100000 = 12,
}

impl StepSize {
    /// Every step in wire order.
    pub const ALL: [Self; 13] = [
        Self::Hz1000,
        Self::Hz2500,
        Self::Hz5000,
        Self::Hz6250,
        Self::Hz8330,
        Self::Hz10000,
        Self::Hz12500,
        Self::Hz15000,
        Self::Hz20000,
        Self::Hz25000,
        Self::Hz30000,
        Self::Hz50000,
        Self::Hz100000,
    ];

    /// The step in hertz; 8.33 kHz is 8330 Hz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        match self {
            Self::Hz1000 => 1_000,
            Self::Hz2500 => 2_500,
            Self::Hz5000 => 5_000,
            Self::Hz6250 => 6_250,
            Self::Hz8330 => 8_330,
            Self::Hz10000 => 10_000,
            Self::Hz12500 => 12_500,
            Self::Hz15000 => 15_000,
            Self::Hz20000 => 20_000,
            Self::Hz25000 => 25_000,
            Self::Hz30000 => 30_000,
            Self::Hz50000 => 50_000,
            Self::Hz100000 => 100_000,
        }
    }

    /// The wire index, `0` through `12`.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self as u8
    }

    /// The uppercase hexadecimal wire digit, `0` through `C`.
    #[must_use]
    pub const fn wire_char(self) -> char {
        match self.as_raw() {
            raw @ 0..=9 => (b'0' + raw) as char,
            raw => (b'A' + raw - 10) as char,
        }
    }

    /// Parse the one-character wire field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidStepSize`] for anything other than
    /// one uppercase hexadecimal digit from `0` to `C`.
    pub fn from_wire_str(text: &str) -> Result<Self, ValidationError> {
        let invalid = || ValidationError::InvalidStepSize {
            value: text.to_owned(),
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
        Self::try_from(raw).map_err(|_| invalid())
    }
}

impl TryFrom<u8> for StepSize {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|step| step.as_raw() == value)
            .ok_or_else(|| ValidationError::InvalidStepSize {
                value: value.to_string(),
            })
    }
}

impl fmt::Display for StepSize {
    /// Kilohertz as the manual prints them, for example `6.25 kHz`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let hz = self.as_hz();
        if hz.is_multiple_of(1000) {
            write!(formatter, "{} kHz", hz / 1000)
        } else {
            let text = format!("{}.{:03}", hz / 1000, hz % 1000);
            write!(formatter, "{} kHz", text.trim_end_matches('0'))
        }
    }
}

fn parse_wire_hz(text: &str) -> Result<u32, ValidationError> {
    if text.len() != WIRE_DIGITS || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ValidationError::InvalidWireDigits {
            text: text.to_owned(),
            digits: WIRE_DIGITS,
        });
    }
    text.parse::<u32>()
        .map_err(|_| ValidationError::InvalidWireDigits {
            text: text.to_owned(),
            digits: WIRE_DIGITS,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn frequency_round_trips_the_wire_form() -> TestResult {
        let frequency = Frequency::from_wire_str("0145190000")?;
        assert_eq!(frequency.as_hz(), 145_190_000);
        assert_eq!(frequency.to_wire_string(), "0145190000");
        assert_eq!(frequency.to_string(), "145.190000 MHz");
        assert_eq!(
            Frequency::from_wire_str("0524000000")?.as_hz(),
            Frequency::MAX_HZ
        );
        for text in ["145190000", "01451900000", "0145190O00", "", "-145190000"] {
            let result = Frequency::from_wire_str(text);
            assert!(
                matches!(result, Err(ValidationError::InvalidWireDigits { .. })),
                "{text:?}: {result:?}"
            );
        }
        let low = Frequency::from_wire_str("0107999999");
        assert!(
            matches!(
                low,
                Err(ValidationError::FrequencyOutOfRange { hz: 107_999_999 })
            ),
            "{low:?}"
        );
        assert!(Frequency::new(Frequency::MAX_HZ + 1).is_err());
        Ok(())
    }

    #[test]
    fn megahertz_text_is_exact() -> TestResult {
        assert_eq!(Frequency::from_mhz_str("145.190")?.as_hz(), 145_190_000);
        assert_eq!(Frequency::from_mhz_str("446")?.as_hz(), 446_000_000);
        assert_eq!(Frequency::from_mhz_str("144.390000")?.as_hz(), 144_390_000);
        for text in [
            "145.",
            ".190",
            "145.1900001",
            "145,190",
            "+145",
            "145 MHz",
            "",
        ] {
            let result = Frequency::from_mhz_str(text);
            assert!(
                matches!(result, Err(ValidationError::InvalidMegahertzText { .. })),
                "{text:?}: {result:?}"
            );
        }
        assert!(matches!(
            Frequency::from_mhz_str("600"),
            Err(ValidationError::FrequencyOutOfRange { hz: 600_000_000 })
        ));
        Ok(())
    }

    #[test]
    fn alignment_and_checked_arithmetic_respect_the_range() -> TestResult {
        let frequency = Frequency::new(145_190_000)?;
        assert!(frequency.is_aligned_to(StepSize::Hz5000));
        assert!(!frequency.is_aligned_to(StepSize::Hz6250));
        assert_eq!(
            frequency.checked_add_hz(5_000).map(Frequency::as_hz),
            Some(145_195_000)
        );
        assert_eq!(Frequency::new(Frequency::MAX_HZ)?.checked_add_hz(1), None);
        assert_eq!(Frequency::new(Frequency::MIN_HZ)?.checked_sub_hz(1), None);
        Ok(())
    }

    #[test]
    fn transmit_setting_selects_the_range_from_the_value() -> TestResult {
        assert_eq!(
            TransmitSetting::from_wire_str("0000600000")?,
            TransmitSetting::Offset(OffsetFrequency::new(600_000)?)
        );
        assert_eq!(
            TransmitSetting::from_wire_str("0446000000")?,
            TransmitSetting::Split(Frequency::new(446_000_000)?)
        );
        assert_eq!(
            TransmitSetting::from_wire_str("0029950000")?.to_wire_string(),
            "0029950000"
        );
        let between = TransmitSetting::from_wire_str("0050000000");
        assert!(
            matches!(
                between,
                Err(ValidationError::TransmitFieldOutOfRange { hz: 50_000_000 })
            ),
            "{between:?}"
        );
        assert!(OffsetFrequency::new(OffsetFrequency::MAX_HZ + 1).is_err());
        assert_eq!(OffsetFrequency::new(600_000)?.to_string(), "0.600000 MHz");
        Ok(())
    }

    #[test]
    fn step_sizes_follow_the_wire_digits_and_the_manual_table() -> TestResult {
        let expected = [
            ('0', 1_000, "1 kHz"),
            ('1', 2_500, "2.5 kHz"),
            ('2', 5_000, "5 kHz"),
            ('3', 6_250, "6.25 kHz"),
            ('4', 8_330, "8.33 kHz"),
            ('5', 10_000, "10 kHz"),
            ('6', 12_500, "12.5 kHz"),
            ('7', 15_000, "15 kHz"),
            ('8', 20_000, "20 kHz"),
            ('9', 25_000, "25 kHz"),
            ('A', 30_000, "30 kHz"),
            ('B', 50_000, "50 kHz"),
            ('C', 100_000, "100 kHz"),
        ];
        for (index, (digit, hz, label)) in expected.into_iter().enumerate() {
            let step = StepSize::from_wire_str(&digit.to_string())?;
            assert_eq!(Some(&step), StepSize::ALL.get(index));
            assert_eq!(step.as_hz(), hz);
            assert_eq!(step.wire_char(), digit);
            assert_eq!(step.to_string(), label);
            assert_eq!(StepSize::try_from(step.as_raw())?, step);
        }
        for text in ["D", "F", "10", "a", "", "12"] {
            assert!(StepSize::from_wire_str(text).is_err(), "{text:?}");
        }
        assert!(StepSize::try_from(13).is_err());
        Ok(())
    }
}
