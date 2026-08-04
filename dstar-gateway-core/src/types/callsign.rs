//! D-STAR callsign (8 bytes, ASCII, space-padded).
//!
//! Every wire-format callsign field in D-STAR is 8 bytes, ASCII, and
//! space-padded on the right. This type validates at construction
//! time on the **TX** path, and stores wire bytes verbatim on the
//! **RX** path (matching `ircDDBGateway`'s lenient `memcpy` pattern,
//! which the audit found is required to avoid silently dropping
//! real-world traffic).
//!
//! See `ircDDBGateway/Common/HeaderData.cpp:619-623` for the
//! reference receive-path pattern this type mirrors.

use super::type_error::TypeError;
use super::wire_text::{WireTextError, diagnostic_wire_bytes, trimmed_printable_ascii};

/// D-STAR callsign (8 bytes, ASCII, space-padded on the right).
///
/// # Invariants
///
/// - Constructed via [`Self::try_from_str`]: printable ASCII, 1..=8 bytes after
///   removing trailing ASCII-space padding, then padded to exactly 8 bytes.
/// - Constructed via [`Self::from_wire_bytes`]: any 8 bytes,
///   verbatim. Used on the receive path where real reflectors emit
///   non-printable bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[doc(alias = "call-sign")]
#[doc(alias = "operator")]
pub struct Callsign([u8; 8]);

impl Callsign {
    /// Try to build a `Callsign` from a string slice.
    ///
    /// - Must be printable ASCII
    /// - Must be 1..=8 bytes (trailing ASCII spaces are removed before the
    ///   length check, then restored as fixed-width padding)
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidCallsign`] if `s` is empty, longer
    /// than 8 bytes after padding removal, or contains a byte outside
    /// printable ASCII.
    ///
    /// # Example
    ///
    /// ```
    /// use dstar_gateway_core::Callsign;
    /// let cs = Callsign::try_from_str("W1AW")?;
    /// assert_eq!(cs.as_bytes(), b"W1AW    "); // space-padded to 8 bytes
    /// # Ok::<(), dstar_gateway_core::TypeError>(())
    /// ```
    pub fn try_from_str(s: &str) -> Result<Self, TypeError> {
        let trimmed = s.trim_end_matches(' ');
        if trimmed.is_empty() {
            return Err(TypeError::InvalidCallsign { reason: "empty" });
        }
        if trimmed.len() > 8 {
            return Err(TypeError::InvalidCallsign {
                reason: "longer than 8 bytes",
            });
        }
        if !trimmed.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            return Err(TypeError::InvalidCallsign {
                reason: "byte outside printable ASCII",
            });
        }
        let mut buf = [b' '; 8];
        // trimmed.len() <= 8 checked above, so this slice is in-bounds.
        // Use .get_mut() to satisfy clippy::indexing_slicing.
        let src = trimmed.as_bytes();
        if let Some(dst) = buf.get_mut(..trimmed.len())
            && let Some(s) = src.get(..trimmed.len())
        {
            dst.copy_from_slice(s);
        }
        Ok(Self(buf))
    }

    /// Build a `Callsign` directly from 8 wire bytes, storing them
    /// verbatim without any validation.
    ///
    /// Used on the **receive** path. Mirrors `ircDDBGateway`'s
    /// `memcpy(m_myCall1, data + 44U, LONG_CALLSIGN_LENGTH)` from
    /// `Common/HeaderData.cpp:622`. Real reflectors emit non-printable
    /// bytes in callsign fields and a strict ASCII filter would
    /// silently drop those headers.
    #[must_use]
    pub const fn from_wire_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Return the 8-byte padded representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    /// Return validated protocol text without trailing space padding.
    ///
    /// The receive constructor intentionally accepts arbitrary bytes. This
    /// method therefore validates the stored field instead of replacing
    /// malformed bytes. [`Self::as_bytes`] always remains available for exact
    /// inspection when validation fails.
    ///
    /// # Errors
    ///
    /// Returns [`WireTextError`] at the first byte outside printable ASCII.
    pub fn text(&self) -> Result<&str, WireTextError> {
        trimmed_printable_ascii(&self.0)
    }

    /// Return text for a valid callsign or an exact escaped diagnostic.
    ///
    /// This compatibility view never inserts Unicode replacement characters.
    /// Use [`Self::text`] for semantic decisions, because it distinguishes
    /// validated callsign text from malformed receive bytes.
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        self.text().map_or_else(
            |_| std::borrow::Cow::Owned(diagnostic_wire_bytes(&self.0)),
            std::borrow::Cow::Borrowed,
        )
    }
}

impl std::fmt::Display for Callsign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.text() {
            Ok(text) => f.write_str(text),
            Err(_) => f.write_str(&diagnostic_wire_bytes(&self.0)),
        }
    }
}

impl std::fmt::Debug for Callsign {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Callsign")
            .field("bytes", &self.0)
            .field("text", &self.text())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn callsign_accepts_w1aw() -> TestResult {
        let cs = Callsign::try_from_str("W1AW")?;
        assert_eq!(cs.as_bytes(), b"W1AW    ");
        assert_eq!(cs.text(), Ok("W1AW"));
        Ok(())
    }

    #[test]
    fn callsign_rejects_empty() {
        let Err(err) = Callsign::try_from_str("") else {
            unreachable!("empty must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidCallsign { .. }));
    }

    #[test]
    fn callsign_rejects_nine_chars() {
        let Err(err) = Callsign::try_from_str("VERYLONG1") else {
            unreachable!("9 chars must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidCallsign { .. }));
    }

    #[test]
    fn callsign_rejects_non_ascii() {
        let Err(err) = Callsign::try_from_str("Wé1AW") else {
            unreachable!("non-ASCII must be rejected");
        };
        assert!(matches!(err, TypeError::InvalidCallsign { .. }));
    }

    #[test]
    fn callsign_rejects_control_instead_of_trimming_it() {
        let Err(err) = Callsign::try_from_str("W1AW\n") else {
            unreachable!("a line ending is data, not callsign padding");
        };
        assert!(matches!(err, TypeError::InvalidCallsign { .. }));
    }

    #[test]
    fn callsign_display_trimmed() -> TestResult {
        let cs = Callsign::try_from_str("W1AW")?;
        assert_eq!(format!("{cs}"), "W1AW");
        assert!(
            format!("{cs:?}").contains("W1AW"),
            "debug output should include validated callsign text"
        );
        Ok(())
    }

    #[test]
    fn callsign_trim_then_pad() -> TestResult {
        let cs = Callsign::try_from_str("W1AW  ")?;
        assert_eq!(cs.as_bytes(), b"W1AW    ");
        Ok(())
    }

    #[test]
    fn callsign_from_wire_bytes_roundtrip() {
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        assert_eq!(cs.as_bytes(), b"W1AW    ");
        assert_eq!(cs.text(), Ok("W1AW"));
    }

    #[test]
    fn callsign_from_wire_bytes_stores_non_ascii_verbatim() {
        // Mirrors ircDDBGateway's raw memcpy behaviour: a remote
        // radio transmitting non-printable bytes in a callsign field
        // produces a Callsign with those bytes preserved, not a
        // silent drop.
        let bytes = [b'A', 0xC3, b'C', b' ', b' ', b' ', b' ', b' '];
        let cs = Callsign::from_wire_bytes(bytes);
        assert_eq!(cs.as_bytes(), &bytes);
        assert_eq!(
            cs.text(),
            Err(WireTextError {
                index: 1,
                byte: 0xC3,
            })
        );
        assert_eq!(cs.as_str(), "<invalid wire bytes: 41 C3 43 20 20 20 20 20>");
    }

    #[test]
    fn callsign_from_wire_bytes_stores_control_verbatim() {
        let bytes = [0x00, b'1', b'A', b'W', b' ', b' ', b' ', b' '];
        let cs = Callsign::from_wire_bytes(bytes);
        assert_eq!(cs.as_bytes(), &bytes);
        assert_eq!(
            cs.text(),
            Err(WireTextError {
                index: 0,
                byte: 0x00,
            })
        );
    }
}
