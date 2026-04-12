//! D-STAR reflector module letter (A-Z).
//!
//! D-STAR reflectors host multiple independent voice modules, each
//! identified by a single uppercase letter. This type enforces the
//! A-Z range at construction so mis-typed modules cannot reach the
//! wire.
//!
//! See `ircDDBGateway/Common/DStarDefines.h` for the underlying
//! `LONG_CALLSIGN_LENGTH` discipline this constant pairs with.

use super::type_error::TypeError;

/// D-STAR reflector module letter (A-Z).
///
/// # Invariants
///
/// The wrapped byte is always in `b'A'..=b'Z'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Module(u8);

impl Module {
    /// Module A.
    pub const A: Self = Self(b'A');
    /// Module B.
    pub const B: Self = Self(b'B');
    /// Module C.
    pub const C: Self = Self(b'C');
    /// Module D.
    pub const D: Self = Self(b'D');
    /// Module E.
    pub const E: Self = Self(b'E');
    /// Module F.
    pub const F: Self = Self(b'F');
    /// Module G.
    pub const G: Self = Self(b'G');
    /// Module H.
    pub const H: Self = Self(b'H');
    /// Module I.
    pub const I: Self = Self(b'I');
    /// Module J.
    pub const J: Self = Self(b'J');
    /// Module K.
    pub const K: Self = Self(b'K');
    /// Module L.
    pub const L: Self = Self(b'L');
    /// Module M.
    pub const M: Self = Self(b'M');
    /// Module N.
    pub const N: Self = Self(b'N');
    /// Module O.
    pub const O: Self = Self(b'O');
    /// Module P.
    pub const P: Self = Self(b'P');
    /// Module Q.
    pub const Q: Self = Self(b'Q');
    /// Module R.
    pub const R: Self = Self(b'R');
    /// Module S.
    pub const S: Self = Self(b'S');
    /// Module T.
    pub const T: Self = Self(b'T');
    /// Module U.
    pub const U: Self = Self(b'U');
    /// Module V.
    pub const V: Self = Self(b'V');
    /// Module W.
    pub const W: Self = Self(b'W');
    /// Module X.
    pub const X: Self = Self(b'X');
    /// Module Y.
    pub const Y: Self = Self(b'Y');
    /// Module Z.
    pub const Z: Self = Self(b'Z');

    /// Attempt to build a `Module` from a character.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidModule`] if `c` is not ASCII A-Z.
    ///
    /// # Example
    ///
    /// ```
    /// use dstar_gateway_core::Module;
    /// let m = Module::try_from_char('C')?;
    /// assert_eq!(m.as_char(), 'C');
    /// # Ok::<(), dstar_gateway_core::TypeError>(())
    /// ```
    pub const fn try_from_char(c: char) -> Result<Self, TypeError> {
        if c.is_ascii_uppercase() {
            Ok(Self(c as u8))
        } else {
            Err(TypeError::InvalidModule { got: c })
        }
    }

    /// Attempt to build a `Module` from a raw byte.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidModule`] if `b` is not in `b'A'..=b'Z'`.
    pub const fn try_from_byte(b: u8) -> Result<Self, TypeError> {
        if b.is_ascii_uppercase() {
            Ok(Self(b))
        } else {
            Err(TypeError::InvalidModule { got: b as char })
        }
    }

    /// Return the module as a character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0 as char
    }

    /// Return the module as a raw byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_char())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn module_accepts_a_through_z() -> TestResult {
        for c in 'A'..='Z' {
            let m = Module::try_from_char(c)?;
            assert_eq!(m.as_char(), c, "round-trip char must match");
        }
        Ok(())
    }

    #[test]
    fn module_rejects_lowercase() {
        let Err(err) = Module::try_from_char('a') else {
            unreachable!("lowercase must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidModule { got: 'a' }));
    }

    #[test]
    fn module_rejects_digit() {
        let Err(err) = Module::try_from_char('1') else {
            unreachable!("digit must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidModule { got: '1' }));
    }

    #[test]
    fn module_rejects_unicode() {
        let Err(err) = Module::try_from_char('Ä') else {
            unreachable!("non-ASCII must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidModule { got: 'Ä' }));
    }

    #[test]
    fn module_rejects_byte_outside_range() {
        let Err(err) = Module::try_from_byte(0x40) else {
            unreachable!("byte 0x40 (@) must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidModule { .. }));
    }

    #[test]
    fn module_display_single_char() {
        let m = Module::C;
        assert_eq!(format!("{m}"), "C");
    }
}
