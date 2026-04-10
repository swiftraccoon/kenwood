//! Radio frequency type for the TH-D75 transceiver.

use std::fmt;

use crate::error::ProtocolError;

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

    /// Returns the frequency in Hz.
    #[must_use]
    pub const fn as_hz(self) -> u32 {
        self.0
    }

    /// Returns the frequency in kHz as a floating-point value.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn as_khz(self) -> f64 {
        f64::from(self.0) / 1_000.0
    }

    /// Returns the frequency in MHz as a floating-point value.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
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
        if s.len() != 10 {
            return Err(ProtocolError::FieldParse {
                command: "FQ".to_owned(),
                field: "frequency".to_owned(),
                detail: format!("expected 10-digit string, got {} chars", s.len()),
            });
        }
        let hz: u32 = s.parse().map_err(|_| ProtocolError::FieldParse {
            command: "FQ".to_owned(),
            field: "frequency".to_owned(),
            detail: format!("non-numeric frequency string: {s:?}"),
        })?;
        Ok(Self(hz))
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
    fn frequency_from_wire() {
        let f = Frequency::from_wire_string("0145000000").unwrap();
        assert_eq!(f.as_hz(), 145_000_000);
    }

    #[test]
    fn frequency_from_wire_invalid() {
        assert!(Frequency::from_wire_string("not_a_number").is_err());
        assert!(Frequency::from_wire_string("12345").is_err()); // wrong length
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
}
