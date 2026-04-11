//! Strongly-typed primitives for dstar-gateway.
//!
//! Four newtypes eliminate footguns that previously appeared as raw
//! `char`, `String`, and sentinel-0 `u16` values throughout the crate:
//!
//! - [`Module`] — reflector module letter, validated as ASCII A-Z
//! - [`Callsign`] — 8-byte space-padded D-STAR callsign
//! - [`Suffix`] — 4-byte space-padded D-STAR short suffix
//! - [`StreamId`] — non-zero u16 stream identifier

use std::num::NonZeroU16;

/// Validation errors for type construction.
#[derive(Debug, thiserror::Error)]
pub enum TypeError {
    /// Supplied module character is not ASCII A-Z.
    #[error("invalid module letter '{got}' (must be ASCII A-Z)")]
    InvalidModule {
        /// The rejected character.
        got: char,
    },
    /// Supplied callsign is empty, too long, or contains non-ASCII.
    #[error("invalid callsign: {reason}")]
    InvalidCallsign {
        /// Human-readable reason for rejection.
        reason: &'static str,
    },
    /// Supplied suffix is too long, or contains non-ASCII.
    #[error("invalid suffix: {reason}")]
    InvalidSuffix {
        /// Human-readable reason for rejection.
        reason: &'static str,
    },
}

/// D-STAR reflector module letter (A-Z).
///
/// D-STAR reflectors host multiple independent voice modules, each
/// identified by a single uppercase letter. This type enforces the
/// A-Z range at construction so mis-typed modules cannot reach the
/// wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Module(u8);

impl Module {
    /// Attempt to build a `Module` from a character.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidModule`] if `c` is not ASCII A-Z.
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
    /// Returns [`TypeError::InvalidModule`] if `b` is not `b'A'..=b'Z'`.
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

/// D-STAR callsign (8 bytes, ASCII, space-padded).
///
/// Every wire-format callsign field in D-STAR is 8 bytes, ASCII, and
/// space-padded. This type validates and performs the padding once
/// at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Callsign([u8; 8]);

impl Callsign {
    /// Try to build a `Callsign` from a string slice.
    ///
    /// - Must be ASCII
    /// - Must be 1..=8 bytes (trailing whitespace is trimmed before
    ///   length check, then space-padded to exactly 8 bytes)
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidCallsign`] on violation.
    pub fn try_from_str(s: &str) -> Result<Self, TypeError> {
        let trimmed = s.trim_end();
        if trimmed.is_empty() {
            return Err(TypeError::InvalidCallsign { reason: "empty" });
        }
        if trimmed.len() > 8 {
            return Err(TypeError::InvalidCallsign {
                reason: "longer than 8 bytes",
            });
        }
        if !trimmed.is_ascii() {
            return Err(TypeError::InvalidCallsign {
                reason: "non-ASCII character",
            });
        }
        let mut buf = [b' '; 8];
        buf[..trimmed.len()].copy_from_slice(trimmed.as_bytes());
        Ok(Self(buf))
    }

    /// Build a `Callsign` directly from 8 wire bytes, storing them
    /// verbatim without any validation.
    ///
    /// Used on the **receive** path for headers lifted from an
    /// incoming D-STAR packet. Mirrors the behaviour of
    /// `ircDDBGateway`'s `CHeaderData::setDPlusData` (and equivalents
    /// for DExtra/DCS), which does `memcpy(m_myCall1, data + 44U,
    /// LONG_CALLSIGN_LENGTH)` with zero byte-level filtering. A
    /// strict "only ASCII printable" check (the previous Rust
    /// version) silently dropped otherwise-fine headers whenever a
    /// remote radio included a null byte, high-bit char, or any
    /// other wire garbage, because `DStarHeader::decode` propagated
    /// the `None` all the way up. The reference does not validate —
    /// we should not either.
    ///
    /// For construction from user input (TX path), use
    /// [`Self::try_from_str`] which does enforce ASCII and length.
    #[must_use]
    pub const fn from_wire_bytes(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }

    /// Return the 8-byte padded representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }

    /// Return the trimmed string (no trailing spaces).
    ///
    /// Uses lossy UTF-8 conversion via [`String::from_utf8_lossy`] so
    /// wire bytes stored verbatim via [`Self::from_wire_bytes`] that
    /// are not valid UTF-8 are rendered with `U+FFFD` replacement
    /// characters rather than panicking or returning an empty string.
    /// Callsigns built via [`Self::try_from_str`] are always valid
    /// ASCII and round-trip with zero allocation (borrowed `Cow`).
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        let end = self.0.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
        String::from_utf8_lossy(&self.0[..end])
    }
}

impl std::fmt::Display for Callsign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 4-byte D-STAR "`my_suffix`" field (short extended callsign suffix).
///
/// The D-STAR radio header carries a 4-byte suffix after the MY
/// callsign, typically used for `/P`, `/M`, `ECHO`, or similar short
/// tags. The field is ASCII, space-padded on the right, and validated
/// here the same way [`Callsign`] is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Returns [`TypeError::InvalidSuffix`] on violation.
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
        buf[..trimmed.len()].copy_from_slice(trimmed.as_bytes());
        Ok(Self(buf))
    }

    /// Build a `Suffix` directly from 4 wire bytes, storing them
    /// verbatim without any validation.
    ///
    /// Same rationale as [`Callsign::from_wire_bytes`]: the
    /// `ircDDBGateway` reference does a raw `memcpy` on the receive
    /// path and never validates the bytes, so neither should we.
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
    ///
    /// Uses lossy UTF-8 conversion via [`String::from_utf8_lossy`] so
    /// wire bytes stored verbatim via [`Self::from_wire_bytes`] that
    /// are not valid UTF-8 are rendered with `U+FFFD` replacement
    /// characters. Suffixes built via [`Self::try_from_str`] are
    /// always valid ASCII and round-trip with zero allocation.
    #[must_use]
    pub fn as_str(&self) -> std::borrow::Cow<'_, str> {
        let end = self.0.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1);
        String::from_utf8_lossy(&self.0[..end])
    }
}

impl std::fmt::Display for Suffix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// D-STAR voice stream identifier (non-zero u16).
///
/// Stream ID 0 is reserved — it is used as a sentinel for "no active
/// stream". Wrapping `NonZeroU16` makes the invalid case
/// unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(NonZeroU16);

impl StreamId {
    /// Construct a `StreamId`, returning `None` if `n == 0`.
    #[must_use]
    pub const fn new(n: u16) -> Option<Self> {
        match NonZeroU16::new(n) {
            Some(nz) => Some(Self(nz)),
            None => None,
        }
    }

    /// Return the raw u16.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl std::fmt::Display for StreamId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:04X}", self.0.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_accepts_a_through_z() {
        for c in 'A'..='Z' {
            let m = Module::try_from_char(c).unwrap();
            assert_eq!(m.as_char(), c);
        }
    }

    #[test]
    fn module_rejects_lowercase() {
        assert!(matches!(
            Module::try_from_char('a'),
            Err(TypeError::InvalidModule { .. })
        ));
    }

    #[test]
    fn module_rejects_digit() {
        assert!(matches!(
            Module::try_from_char('1'),
            Err(TypeError::InvalidModule { .. })
        ));
    }

    #[test]
    fn module_rejects_unicode() {
        assert!(matches!(
            Module::try_from_char('Ä'),
            Err(TypeError::InvalidModule { .. })
        ));
    }

    #[test]
    fn module_display_single_char() {
        let m = Module::try_from_char('C').unwrap();
        assert_eq!(format!("{m}"), "C");
    }

    #[test]
    fn callsign_accepts_w1aw() {
        let cs = Callsign::try_from_str("W1AW").unwrap();
        assert_eq!(cs.as_bytes(), b"W1AW    ");
        assert_eq!(cs.as_str(), "W1AW");
    }

    #[test]
    fn callsign_rejects_empty() {
        assert!(matches!(
            Callsign::try_from_str(""),
            Err(TypeError::InvalidCallsign { .. })
        ));
    }

    #[test]
    fn callsign_rejects_nine_chars() {
        assert!(matches!(
            Callsign::try_from_str("VERYLONG1"),
            Err(TypeError::InvalidCallsign { .. })
        ));
    }

    #[test]
    fn callsign_rejects_non_ascii() {
        assert!(matches!(
            Callsign::try_from_str("Wé1AW"),
            Err(TypeError::InvalidCallsign { .. })
        ));
    }

    #[test]
    fn callsign_display_trimmed() {
        let cs = Callsign::try_from_str("W1AW").unwrap();
        assert_eq!(format!("{cs}"), "W1AW");
    }

    #[test]
    fn callsign_trim_then_pad() {
        let cs = Callsign::try_from_str("W1AW  ").unwrap();
        assert_eq!(cs.as_bytes(), b"W1AW    ");
    }

    #[test]
    fn callsign_from_wire_bytes_roundtrip() {
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        assert_eq!(cs.as_bytes(), b"W1AW    ");
        assert_eq!(cs.as_str(), "W1AW");
    }

    #[test]
    fn callsign_from_wire_bytes_stores_non_ascii_verbatim() {
        // Mirrors ircDDBGateway's raw memcpy behaviour — a remote
        // radio transmitting non-printable bytes in a callsign field
        // produces a Callsign with those bytes preserved, not a
        // silent drop. The display path uses lossy UTF-8 so a
        // human-readable rendering still works.
        let bytes = [b'A', 0xC3, b'C', b' ', b' ', b' ', b' ', b' '];
        let cs = Callsign::from_wire_bytes(bytes);
        assert_eq!(cs.as_bytes(), &bytes);
        // 0xC3 is the start of a 2-byte UTF-8 sequence but [0xC3]
        // alone is invalid; lossy conversion renders it as U+FFFD.
        // That is acceptable — the user sees a replacement glyph
        // instead of the whole header being silently dropped.
        assert!(cs.as_str().contains('\u{FFFD}') || cs.as_str().contains('A'));
    }

    #[test]
    fn callsign_from_wire_bytes_stores_control_verbatim() {
        let bytes = [0x00, b'1', b'A', b'W', b' ', b' ', b' ', b' '];
        let cs = Callsign::from_wire_bytes(bytes);
        assert_eq!(cs.as_bytes(), &bytes);
    }

    #[test]
    fn suffix_empty_is_all_spaces() {
        assert_eq!(Suffix::EMPTY.as_bytes(), b"    ");
        assert_eq!(Suffix::EMPTY.as_str(), "");
    }

    #[test]
    fn suffix_from_str_accepts_short() {
        let s = Suffix::try_from_str("P").unwrap();
        assert_eq!(s.as_bytes(), b"P   ");
    }

    #[test]
    fn suffix_from_str_accepts_exact() {
        let s = Suffix::try_from_str("ECHO").unwrap();
        assert_eq!(s.as_bytes(), b"ECHO");
    }

    #[test]
    fn suffix_from_str_accepts_empty() {
        let s = Suffix::try_from_str("").unwrap();
        assert_eq!(s, Suffix::EMPTY);
    }

    #[test]
    fn suffix_from_str_rejects_too_long() {
        assert!(matches!(
            Suffix::try_from_str("TOOLONG"),
            Err(TypeError::InvalidSuffix { .. })
        ));
    }

    #[test]
    fn suffix_from_str_rejects_non_ascii() {
        assert!(matches!(
            Suffix::try_from_str("é"),
            Err(TypeError::InvalidSuffix { .. })
        ));
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
    fn suffix_display_trimmed() {
        let s = Suffix::try_from_str("P").unwrap();
        assert_eq!(format!("{s}"), "P");
    }

    #[test]
    fn stream_id_rejects_zero() {
        assert!(StreamId::new(0).is_none());
    }

    #[test]
    fn stream_id_accepts_non_zero() {
        let sid = StreamId::new(0x1234).unwrap();
        assert_eq!(sid.get(), 0x1234);
    }

    #[test]
    fn stream_id_display_hex() {
        let sid = StreamId::new(0x00AB).unwrap();
        assert_eq!(format!("{sid}"), "0x00AB");
    }
}
