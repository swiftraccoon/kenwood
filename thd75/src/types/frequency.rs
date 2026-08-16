//! Radio frequency type for the TH-D75 transceiver.

use std::fmt;

use crate::error::{ProtocolError, ValidationError};
use crate::types::mode::StepSize;

/// Radio frequency in Hz.
///
/// Stored as a `u32`, matching the firmware's internal representation.
/// Range: 0 to 4,294,967,295 Hz (0 to ~4.295 GHz).
///
/// # TH-D75 band frequency ranges
///
/// Per service manual §2.1.2 (Table 1) and User Manual Chapter 28, the
/// radio enforces hardware-specific frequency limits per band. The
/// service manual frequency configuration points (A-E) map to the
/// signal path in the receiver block diagrams (§2.1.3):
///
/// ## TH-D75A (K type)
///
/// | Point | Frequency range | Function |
/// |-------|----------------|----------|
/// | A (TX/RX) | 144.000-147.995, 222.000-224.995, 430.000-449.995 MHz | VCO/PLL output → 1st mixer |
/// | B (RX) | 136.000-173.995, 216.000-259.995, 410.000-469.995 MHz | RF AMP → distribution circuit |
/// | C (RX) | 0.100-75.995, 108.000-523.995 MHz | Band B wideband RX input |
/// | D (1st IF) | 193.150-231.145, 158.850-202.845, 352.850-412.845 MHz | After 1st mixer (Band A) |
/// | E (1st IF) | 58.150-134.045, 166.050-465.945 MHz | After 1st mixer (Band B) |
///
/// ## TH-D75E (E, T types)
///
/// | Point | Frequency range | Function |
/// |-------|----------------|----------|
/// | A (TX/RX) | 144.000-145.995, 430.000-439.995 MHz | VCO/PLL output → 1st mixer |
/// | B (RX) | 136.000-173.995, 410.000-469.995 MHz | RF AMP → distribution circuit |
/// | C (RX) | 0.100-75.995, 108.000-523.995 MHz | Band B wideband RX input |
///
/// Band A uses double super heterodyne (1st IF 57.15 MHz, 2nd IF
/// 450 kHz). Band B uses triple super heterodyne (1st IF 58.05 MHz,
/// 2nd IF 450 kHz, 3rd IF 10.8 kHz for AM/SSB/CW).
///
/// Frequencies outside these ranges will be **rejected by the radio**
/// when sent via CAT commands such as `FQ` or `FO`. The firmware
/// validates the frequency against the target band's allowed range and
/// returns a `?` error response if the value is out of bounds. This
/// library does not pre-validate frequencies to avoid duplicating
/// firmware logic that may vary by region or firmware version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Frequency(u32);

impl Frequency {
    /// Creates a new `Frequency` from a value in Hz.
    ///
    /// No validation is performed; the full `u32` range is accepted
    /// to match firmware behaviour.
    #[must_use]
    pub const fn new(hz: u32) -> Self {
        Self(hz)
    }

    /// Creates a frequency from an integer number of kilohertz.
    ///
    /// Returns `None` when converting to hertz would overflow the underlying
    /// `u32` representation.
    #[must_use]
    pub const fn from_khz(khz: u32) -> Option<Self> {
        match khz.checked_mul(1_000) {
            Some(hz) => Some(Self(hz)),
            None => None,
        }
    }

    /// Add an offset expressed in hertz.
    ///
    /// Returns `None` if the result exceeds the frequency representation.
    #[must_use]
    pub const fn checked_add_hz(self, offset_hz: u32) -> Option<Self> {
        match self.0.checked_add(offset_hz) {
            Some(hz) => Some(Self(hz)),
            None => None,
        }
    }

    /// Subtract an offset expressed in hertz.
    ///
    /// Returns `None` if the result would be negative.
    #[must_use]
    pub const fn checked_sub_hz(self, offset_hz: u32) -> Option<Self> {
        match self.0.checked_sub(offset_hz) {
            Some(hz) => Some(Self(hz)),
            None => None,
        }
    }

    /// Apply a signed offset expressed in hertz.
    ///
    /// Returns `None` if the result falls outside the full `u32` frequency
    /// representation.
    #[must_use]
    pub fn checked_offset_hz(self, offset_hz: i64) -> Option<Self> {
        let adjusted = i64::from(self.0).checked_add(offset_hz)?;
        u32::try_from(adjusted).ok().map(Self)
    }

    /// Returns the frequency in Hz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        self.0
    }

    /// Returns the frequency in kHz as a floating-point value.
    #[must_use]
    pub fn as_khz(self) -> f64 {
        f64::from(self.0) / 1_000.0
    }

    /// Returns the frequency in MHz as a floating-point value.
    #[must_use]
    pub fn as_mhz(self) -> f64 {
        f64::from(self.0) / 1_000_000.0
    }

    /// Formats the frequency as a 10-digit zero-padded decimal string
    /// for CAT protocol wire transmission.
    ///
    /// Example: 145 MHz becomes `"0145000000"`.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        format!("{:010}", self.0)
    }

    /// Parses a 10-digit decimal string from the CAT protocol into a
    /// `Frequency`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FieldParse`] if the string is not
    /// exactly 10 characters or contains non-numeric characters.
    pub fn from_wire_string(s: &str) -> Result<Self, ProtocolError> {
        Self::from_wire_field(s, "FQ", "frequency")
    }

    /// Parses a CAT frequency while retaining the calling command and field
    /// in any diagnostic.
    pub(crate) fn from_wire_field(
        s: &str,
        command: &str,
        field: &str,
    ) -> Result<Self, ProtocolError> {
        if s.len() != 10 || !s.as_bytes().iter().all(u8::is_ascii_digit) {
            return Err(ProtocolError::FieldParse {
                command: command.to_owned(),
                field: field.to_owned(),
                detail: format!("expected exactly 10 ASCII decimal digits, got {s:?}"),
            });
        }
        let hz: u32 = s.parse().map_err(|_| ProtocolError::FieldParse {
            command: command.to_owned(),
            field: field.to_owned(),
            detail: format!("non-numeric frequency string: {s:?}"),
        })?;
        Ok(Self(hz))
    }

    /// Parse a decimal megahertz string into an exact frequency.
    ///
    /// Accepts an unsigned decimal number with an optional fractional part of
    /// at most six digits (hertz resolution), such as `"145.190"`, `"435"`,
    /// or `"0.000001"`. Both parts must be nonempty when a decimal point is
    /// present. No sign, unit suffix, exponent, or digit grouping is accepted.
    /// The conversion is exact integer arithmetic; no floating-point rounding
    /// is involved.
    ///
    /// # Examples
    ///
    /// ```
    /// use kenwood_thd75::types::{Frequency, StepSize};
    ///
    /// let freq = Frequency::from_mhz_str("146.520")?;
    /// assert_eq!(freq.as_hz(), 146_520_000);
    /// assert!(freq.is_aligned_to(StepSize::Hz5000));
    /// assert!(Frequency::from_mhz_str("146.52 MHz").is_err());
    /// # Ok::<(), kenwood_thd75::error::ValidationError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidTextValue`] when the text is not an
    /// exact, in-range decimal megahertz value.
    pub fn from_mhz_str(text: &str) -> Result<Self, ValidationError> {
        const DETAIL: &str = "must be an unsigned decimal megahertz value with at most six \
                              fractional digits, 0-4294.967295 MHz";
        /// Hz multiplier for a fractional part of the indexed length.
        const SCALES: [u64; 7] = [1_000_000, 100_000, 10_000, 1_000, 100, 10, 1];
        let invalid = |reason: &str| ValidationError::InvalidTextValue {
            name: "frequency",
            value: text.to_owned(),
            detail: DETAIL,
            reason: reason.to_owned(),
        };

        let (whole, fraction) = match text.split_once('.') {
            Some((whole, fraction)) => (whole, fraction),
            None => (text, ""),
        };
        if whole.is_empty() {
            return Err(invalid("missing digits before the decimal point"));
        }
        if text.contains('.') && fraction.is_empty() {
            return Err(invalid("missing digits after the decimal point"));
        }
        if !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid("contains a character other than a decimal digit"));
        }
        let scale = SCALES
            .get(fraction.len())
            .copied()
            .ok_or_else(|| invalid("more than six fractional digits is below 1 Hz resolution"))?;
        let megahertz: u64 = whole
            .parse()
            .map_err(|_| invalid("integer part exceeds the representable range"))?;
        let fraction_hz = if fraction.is_empty() {
            0
        } else {
            let digits: u64 = fraction
                .parse()
                .map_err(|_| invalid("fractional part exceeds the representable range"))?;
            digits * scale
        };
        let hz = megahertz
            .checked_mul(1_000_000)
            .and_then(|hz| hz.checked_add(fraction_hz))
            .ok_or_else(|| invalid("exceeds 4294.967295 MHz"))?;
        let hz = u32::try_from(hz).map_err(|_| invalid("exceeds 4294.967295 MHz"))?;
        Ok(Self(hz))
    }

    /// Report whether this frequency sits on the radio's nominal integer
    /// raster for `step`.
    ///
    /// Alignment is computed against [`StepSize::as_hz`], the same integer
    /// hertz value this library uses for the step everywhere else. The
    /// 8.33 kHz airband selection is therefore checked against its nominal
    /// 8,330 Hz raster, matching the step's library-wide integer
    /// representation.
    #[must_use]
    pub const fn is_aligned_to(self, step: StepSize) -> bool {
        self.0.is_multiple_of(step.as_hz())
    }

    /// Returns the frequency as a 4-byte little-endian array.
    #[must_use]
    pub const fn to_le_bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// Creates a `Frequency` from a 4-byte little-endian array.
    #[must_use]
    pub const fn from_le_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_le_bytes(bytes))
    }
}

impl fmt::Display for Frequency {
    /// Formats the frequency in MHz with three decimal places.
    ///
    /// Example: `Frequency::new(145_190_000)` displays as `"145.190 MHz"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mhz_whole = self.0 / 1_000_000;
        let mhz_frac = (self.0 % 1_000_000) / 1_000;
        write!(f, "{mhz_whole}.{mhz_frac:03} MHz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_construction() {
        let f = Frequency::new(145_000_000);
        assert_eq!(f.as_hz(), 145_000_000);
    }

    #[test]
    fn frequency_construction_from_khz_and_checked_offsets() {
        let simplex = Frequency::new(146_520_000);
        assert_eq!(Frequency::from_khz(146_520), Some(simplex));
        assert_eq!(
            simplex.checked_add_hz(600_000),
            Some(Frequency::new(147_120_000))
        );
        assert_eq!(
            simplex.checked_sub_hz(600_000),
            Some(Frequency::new(145_920_000))
        );
        assert_eq!(
            simplex.checked_offset_hz(-600_000),
            Some(Frequency::new(145_920_000))
        );
        assert_eq!(Frequency::new(0).checked_sub_hz(1), None);
        assert_eq!(Frequency::new(u32::MAX).checked_add_hz(1), None);
        assert_eq!(Frequency::from_khz(u32::MAX), None);
    }

    #[test]
    fn frequency_display_mhz() {
        let f = Frequency::new(145_000_000);
        assert!((f.as_mhz() - 145.0).abs() < f64::EPSILON);
    }

    #[test]
    fn frequency_display_khz() {
        let f = Frequency::new(145_500_000);
        assert!((f.as_khz() - 145_500.0).abs() < f64::EPSILON);
    }

    #[test]
    fn frequency_wire_format() {
        let f = Frequency::new(145_000_000);
        assert_eq!(f.to_wire_string(), "0145000000");
    }

    #[test]
    fn frequency_from_wire() -> Result<(), Box<dyn std::error::Error>> {
        let f = Frequency::from_wire_string("0145000000")?;
        assert_eq!(f.as_hz(), 145_000_000);
        Ok(())
    }

    #[test]
    fn frequency_from_wire_invalid() {
        for malformed in [
            "not_a_number",
            "12345",
            "+145000000",
            "-145000000",
            "014500000 ",
        ] {
            assert!(
                Frequency::from_wire_string(malformed).is_err(),
                "non-wire frequency form was accepted: {malformed:?}"
            );
        }
    }

    #[test]
    fn frequency_display_formatted() {
        assert_eq!(Frequency::new(145_190_000).to_string(), "145.190 MHz");
        assert_eq!(Frequency::new(445_000_000).to_string(), "445.000 MHz");
        assert_eq!(Frequency::new(50_125_000).to_string(), "50.125 MHz");
        assert_eq!(Frequency::new(0).to_string(), "0.000 MHz");
    }

    #[test]
    fn frequency_from_bytes_le() {
        let bytes = 145_000_000u32.to_le_bytes();
        let f = Frequency::from_le_bytes(bytes);
        assert_eq!(f.as_hz(), 145_000_000);
    }

    #[test]
    fn frequency_to_bytes_le() {
        let f = Frequency::new(145_000_000);
        assert_eq!(f.to_le_bytes(), 145_000_000u32.to_le_bytes());
    }

    #[test]
    fn frequency_from_mhz_str_parses_exact_decimals() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(Frequency::from_mhz_str("145.190")?.as_hz(), 145_190_000);
        assert_eq!(Frequency::from_mhz_str("145.19")?.as_hz(), 145_190_000);
        assert_eq!(Frequency::from_mhz_str("435")?.as_hz(), 435_000_000);
        assert_eq!(Frequency::from_mhz_str("0.100")?.as_hz(), 100_000);
        assert_eq!(Frequency::from_mhz_str("0.000001")?.as_hz(), 1);
        assert_eq!(Frequency::from_mhz_str("4294.967295")?.as_hz(), u32::MAX);
        Ok(())
    }

    #[test]
    fn frequency_from_mhz_str_rejects_non_representable_input() {
        for rejected in [
            "",
            ".",
            "145.",
            ".190",
            "145.1234567",
            "4294.967296",
            "abc",
            "145.19 MHz",
            "-145.190",
            "+145.190",
            "1e3",
            "145..190",
            "145,190",
        ] {
            let result = Frequency::from_mhz_str(rejected);
            assert!(
                result.is_err(),
                "non-representable MHz text was accepted: {rejected:?} -> {result:?}"
            );
        }
    }

    #[test]
    fn frequency_alignment_uses_integer_raster() {
        use crate::types::StepSize;
        assert!(Frequency::new(146_520_000).is_aligned_to(StepSize::Hz5000));
        assert!(!Frequency::new(146_522_000).is_aligned_to(StepSize::Hz5000));
        assert!(Frequency::new(83_300_000).is_aligned_to(StepSize::Hz8330));
        assert!(!Frequency::new(118_005_000).is_aligned_to(StepSize::Hz8330));
        assert!(Frequency::new(0).is_aligned_to(StepSize::Hz12500));
    }
}
