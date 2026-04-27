//! 4-byte D-STAR `my_suffix` field (short extended callsign suffix).
//!
//! The D-STAR radio header carries a 4-byte suffix after the MY
//! callsign, typically used for `/P`, `/M`, `ECHO`, or similar tags.
//! Same construction discipline as [`super::callsign::Callsign`]:
//! strict on the TX path, lenient on the RX path.

use super::type_error::TypeError;

/// 4-byte D-STAR callsign suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[doc(alias = "callsign-suffix")]
#[doc(alias = "operator-suffix")]
pub struct Suffix([u8; 4]);

impl Suffix {
    /// Empty (all-spaces) suffix.
    pub const EMPTY: Self = Self(*b"    ");

    /// Try to build a `Suffix` from a string slice.
    ///
    /// - Must be ASCII
    /// - Trailing whitespace is trimmed before the length check
    /// - The trimmed length must be 0..=4 bytes
    /// - Result is space-padded to exactly 4 bytes
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidSuffix`] if `s` is longer than 4
    /// bytes (after trimming) or contains non-ASCII characters.
    ///
    /// # Example
    ///
    /// ```
    /// use dstar_gateway_core::Suffix;
    /// let s = Suffix::try_from_str("ECHO")?;
    /// assert_eq!(s.as_bytes(), b"ECHO");
    /// # Ok::<(), dstar_gateway_core::TypeError>(())
    /// ```
    pub fn try_from_str(s: &str) -> Result<Self, TypeError> {
        let trimmed = s.trim_end();
        if trimmed.len() > 4 {
            return Err(TypeError::InvalidSuffix {
                reason: "longer than 4 bytes",
            });
        }
        if !trimmed.is_ascii() {
            return Err(TypeError::InvalidSuffix {
                reason: "non-ASCII character",
            });
        }
        let mut buf = [b' '; 4];
        let src = trimmed.as_bytes();
        if let Some(dst) = buf.get_mut(..trimmed.len())
            && let Some(s) = src.get(..trimmed.len())
        {
            dst.copy_from_slice(s);
        }
        Ok(Self(buf))
    }

    /// Build a `Suffix` directly from 4 wire bytes, storing them
    /// verbatim. Same rationale as [`super::callsign::Callsign::from_wire_bytes`].
    #[must_use]
    pub const fn from_wire_bytes(bytes: [u8; 4]) -> Self {
        Self(bytes)
    }

    /// Return the 4-byte padded representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 4] {
        &self.0
    }

    /// Return the trimmed suffix (no trailing spaces).
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        let end = self.0.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
        let slice = self.0.get(..end).unwrap_or(&[]);
        String::from_utf8_lossy(slice)
    }
}

impl std::fmt::Display for Suffix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn suffix_empty_is_all_spaces() {
        assert_eq!(Suffix::EMPTY.as_bytes(), b"    ");
        assert_eq!(Suffix::EMPTY.as_str(), "");
    }

    #[test]
    fn suffix_from_str_accepts_short() -> TestResult {
        let s = Suffix::try_from_str("P")?;
        assert_eq!(s.as_bytes(), b"P   ");
        Ok(())
    }

    #[test]
    fn suffix_from_str_accepts_exact() -> TestResult {
        let s = Suffix::try_from_str("ECHO")?;
        assert_eq!(s.as_bytes(), b"ECHO");
        Ok(())
    }

    #[test]
    fn suffix_from_str_accepts_empty() -> TestResult {
        let s = Suffix::try_from_str("")?;
        assert_eq!(s, Suffix::EMPTY);
        Ok(())
    }

    #[test]
    fn suffix_from_str_rejects_too_long() {
        let Err(err) = Suffix::try_from_str("TOOLONG") else {
            unreachable!("7 chars must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidSuffix { .. }));
    }

    #[test]
    fn suffix_from_str_rejects_non_ascii() {
        let Err(err) = Suffix::try_from_str("é") else {
            unreachable!("non-ASCII must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidSuffix { .. }));
    }

    #[test]
    fn suffix_from_wire_bytes_roundtrip() {
        let s = Suffix::from_wire_bytes(*b"ECHO");
        assert_eq!(s.as_bytes(), b"ECHO");
        assert_eq!(s.as_str(), "ECHO");
    }

    #[test]
    fn suffix_from_wire_bytes_stores_non_ascii_verbatim() {
        let bytes = [b'A', 0xC3, b'C', b' '];
        let s = Suffix::from_wire_bytes(bytes);
        assert_eq!(s.as_bytes(), &bytes);
    }

    #[test]
    fn suffix_display_trimmed() -> TestResult {
        let s = Suffix::try_from_str("P")?;
        assert_eq!(format!("{s}"), "P");
        Ok(())
    }
}
