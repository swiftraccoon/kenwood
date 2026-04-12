//! D-STAR band letter (A, B, C, or D).
//!
//! Distinct from [`super::module::Module`] (A-Z) — band letters are
//! the radio's hardware bands (typically two on a TH-D75: A=upper,
//! B=lower, plus C and D for tri/quad-band radios). Module letters
//! identify reflector modules and may be any A-Z.

use super::type_error::TypeError;

/// D-STAR radio band letter (A, B, C, D).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BandLetter {
    /// Band A.
    A,
    /// Band B.
    B,
    /// Band C.
    C,
    /// Band D.
    D,
}

impl BandLetter {
    /// Construct a `BandLetter` from a character.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidBandLetter`] for any character other
    /// than `'A'`, `'B'`, `'C'`, or `'D'`.
    pub const fn try_from_char(c: char) -> Result<Self, TypeError> {
        match c {
            'A' => Ok(Self::A),
            'B' => Ok(Self::B),
            'C' => Ok(Self::C),
            'D' => Ok(Self::D),
            _ => Err(TypeError::InvalidBandLetter { got: c }),
        }
    }

    /// Return the band letter as a character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
            Self::C => 'C',
            Self::D => 'D',
        }
    }
}

impl std::fmt::Display for BandLetter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_char())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn band_letter_accepts_a_b_c_d() -> TestResult {
        assert_eq!(BandLetter::try_from_char('A')?, BandLetter::A);
        assert_eq!(BandLetter::try_from_char('B')?, BandLetter::B);
        assert_eq!(BandLetter::try_from_char('C')?, BandLetter::C);
        assert_eq!(BandLetter::try_from_char('D')?, BandLetter::D);
        Ok(())
    }

    #[test]
    fn band_letter_rejects_e() {
        let Err(err) = BandLetter::try_from_char('E') else {
            unreachable!("E must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidBandLetter { got: 'E' }));
    }

    #[test]
    fn band_letter_rejects_lowercase() {
        let Err(err) = BandLetter::try_from_char('a') else {
            unreachable!("lowercase must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidBandLetter { got: 'a' }));
    }

    #[test]
    fn band_letter_as_char_roundtrip() -> TestResult {
        for c in ['A', 'B', 'C', 'D'] {
            let bl = BandLetter::try_from_char(c)?;
            assert_eq!(bl.as_char(), c);
        }
        Ok(())
    }

    #[test]
    fn band_letter_display() {
        assert_eq!(format!("{}", BandLetter::C), "C");
    }
}
