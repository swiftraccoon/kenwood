//! AX.25 address type (callsign + SSID) and its components.
//!
//! [`Ax25Address`] combines a validated [`Callsign`] and [`Ssid`].
//! [`RouteEntry`] adds a repeater's H-bit; command/response classification
//! belongs to [`crate::Ax25Header`].

use alloc::string::{String, ToString};
use core::fmt;

use crate::error::Ax25Error;

// ---------------------------------------------------------------------------
// Callsign
// ---------------------------------------------------------------------------

/// An AX.25 base callsign (without SSID).
///
/// Per AX.25 v2.2 §3.12, a callsign is **1 to 6 ASCII characters**, each of
/// which is an uppercase letter (`A`-`Z`) or a digit (`0`-`9`). The wire
/// format left-shifts each byte by one bit; this type stores the plain
/// ASCII form and does the shifting at encode time.
///
/// Use [`Ssid`] alongside this type to represent the full 7-byte AX.25
/// address. For APRS-IS login strings or display, format with `-` between
/// callsign and SSID (e.g. `"N0CALL-7"`).
/// See the [address and path examples](crate#addresses-and-paths).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[doc(alias = "call-sign")]
#[doc(alias = "operator")]
pub struct Callsign(String);

impl Callsign {
    /// Maximum length of a callsign (bytes).
    pub const MAX_LEN: usize = 6;

    /// Create a new callsign from a string. Rejects any input that is
    /// empty, longer than 6 bytes, or contains anything other than
    /// uppercase ASCII letters and digits.
    ///
    /// The input is matched case-sensitively; lowercase letters are
    /// rejected. Use [`Self::new_case_insensitive`] to accept mixed case
    /// and uppercase on the fly.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidCallsign`] if the input fails any of
    /// the above rules.
    pub fn new(s: &str) -> Result<Self, Ax25Error> {
        Self::validate(s)?;
        Ok(Self(s.to_string()))
    }

    fn validate(s: &str) -> Result<(), Ax25Error> {
        if s.is_empty() {
            return Err(Ax25Error::InvalidCallsign("must not be empty".to_string()));
        }
        if s.len() > Self::MAX_LEN {
            return Err(Ax25Error::InvalidCallsign(
                "length exceeds 6 characters".to_string(),
            ));
        }
        for &b in s.as_bytes() {
            let valid = b.is_ascii_uppercase() || b.is_ascii_digit();
            if !valid {
                return Err(Ax25Error::InvalidCallsign(
                    "must be uppercase A-Z or digit 0-9".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// Like [`Self::new`] but uppercases lowercase ASCII first.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new`] after case conversion.
    pub fn new_case_insensitive(s: &str) -> Result<Self, Ax25Error> {
        Self::try_from(s.to_ascii_uppercase())
    }

    /// Return the callsign as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the callsign as a byte slice.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Report whether the callsign contains zero bytes.
    ///
    /// Valid callsigns contain at least one byte, so this returns `false`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }
}

impl fmt::Display for Callsign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl core::str::FromStr for Callsign {
    type Err = Ax25Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for Callsign {
    type Error = Ax25Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Callsign {
    type Error = Ax25Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<Callsign> for String {
    fn from(c: Callsign) -> Self {
        c.0
    }
}

impl From<&Callsign> for String {
    fn from(c: &Callsign) -> Self {
        c.0.clone()
    }
}

impl AsRef<str> for Callsign {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Callsign {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Callsign {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for Callsign {
    fn eq(&self, other: &String) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Callsign> for str {
    fn eq(&self, other: &Callsign) -> bool {
        self == other.0
    }
}

impl PartialEq<Callsign> for &str {
    fn eq(&self, other: &Callsign) -> bool {
        *self == other.0
    }
}

impl PartialEq<Callsign> for String {
    fn eq(&self, other: &Callsign) -> bool {
        *self == other.0
    }
}

// ---------------------------------------------------------------------------
// Ssid
// ---------------------------------------------------------------------------

/// An AX.25 Secondary Station Identifier (0-15).
///
/// SSIDs distinguish stations using the same callsign. Construction accepts
/// `0..=15`; conversion to `u8` preserves the numeric value. See the
/// [address and path examples](crate#addresses-and-paths).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[doc(alias = "SSID")]
#[doc(alias = "callsign-ssid")]
pub struct Ssid(u8);

impl Ssid {
    /// SSID zero, displayed without a suffix in an [`Ax25Address`].
    pub const ZERO: Self = Self(0);

    /// Maximum legal SSID value.
    pub const MAX: u8 = 15;

    /// Create an SSID from a raw `u8`, validating `0..=15`.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidSsid`] if `n > 15`.
    pub const fn new(n: u8) -> Result<Self, Ax25Error> {
        if n <= Self::MAX {
            Ok(Self(n))
        } else {
            Err(Ax25Error::InvalidSsid(n))
        }
    }

    /// Return the raw SSID value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Return this SSID minus one, keeping zero at zero.
    ///
    /// This is useful for APRS New-N digipeater hop counters. Because the
    /// operation starts from a validated [`Ssid`], its result is also a
    /// valid [`Ssid`] and needs no fallback or repeated range check.
    #[must_use]
    pub const fn saturating_decrement(self) -> Self {
        Self(self.0.saturating_sub(1))
    }
}

impl fmt::Display for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl TryFrom<u8> for Ssid {
    type Error = Ax25Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Ssid> for u8 {
    fn from(value: Ssid) -> Self {
        value.get()
    }
}

impl PartialEq<u8> for Ssid {
    fn eq(&self, other: &u8) -> bool {
        self.0 == *other
    }
}

impl PartialEq<Ssid> for u8 {
    fn eq(&self, other: &Ssid) -> bool {
        *self == other.0
    }
}

impl PartialOrd<u8> for Ssid {
    fn partial_cmp(&self, other: &u8) -> Option<core::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

// ---------------------------------------------------------------------------
// Ax25Address
// ---------------------------------------------------------------------------

/// An AX.25 address: validated callsign plus SSID.
///
/// Used for source and destination address slots in [`crate::Ax25Packet`].
/// Digipeater slots use [`RouteEntry`], which wraps an `Ax25Address` with
/// a `has_repeated` flag for the AX.25 v2.2 §3.12.1 H-bit. The single
/// physical wire bit at SSID-byte bit 7 is reconstructed at encode/decode
/// time from frame-level context; it is *not* a field on this struct.
/// See the [address and path examples](crate#addresses-and-paths).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ax25Address {
    /// Station callsign (1-6 uppercase ASCII alphanumerics).
    pub callsign: Callsign,
    /// Secondary Station Identifier (0-15).
    pub ssid: Ssid,
}

impl Ax25Address {
    /// Create a new address with validation.
    ///
    /// Rejects empty or malformed callsigns (must be 1-6 uppercase ASCII
    /// alphanumeric characters) and out-of-range SSIDs (must be 0-15).
    /// Accepts mixed-case input and uppercases internally.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidCallsign`] or [`Ax25Error::InvalidSsid`]
    /// if either field fails its validation rules.
    pub fn new(callsign: &str, ssid: u8) -> Result<Self, Ax25Error> {
        Ok(Self {
            callsign: Callsign::new_case_insensitive(callsign)?,
            ssid: Ssid::new(ssid)?,
        })
    }

    /// Infallible constructor from already-validated parts.
    #[must_use]
    pub const fn from_parts(callsign: Callsign, ssid: Ssid) -> Self {
        Self { callsign, ssid }
    }

    /// Parse a canonical display-form address.
    ///
    /// Accepted values are an uppercase AX.25 callsign by itself, such as
    /// `"W1AW"`, or a callsign followed by an SSID from 1 through 15, such
    /// as `"N0CALL-7"`. Because SSID zero is displayed without a suffix,
    /// `-0` and zero-padded suffixes are rejected rather than silently
    /// normalized.
    ///
    /// # Errors
    ///
    /// Returns [`Ax25Error::InvalidCallsign`], [`Ax25Error::InvalidSsid`],
    /// or [`Ax25Error::InvalidAddress`] when `value` is not canonical.
    pub fn from_canonical_str(value: &str) -> Result<Self, Ax25Error> {
        let (callsign, ssid) = match value.split_once('-') {
            None => (Callsign::new(value)?, Ssid::ZERO),
            Some((callsign, suffix)) => {
                if suffix.is_empty() {
                    return Err(Ax25Error::InvalidAddress(
                        "SSID suffix must not be empty".to_string(),
                    ));
                }
                if suffix.starts_with('0') {
                    return Err(Ax25Error::InvalidAddress(
                        "SSID zero must be omitted and nonzero SSIDs must not be zero-padded"
                            .to_string(),
                    ));
                }
                if !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(Ax25Error::InvalidAddress(
                        "SSID suffix must contain only decimal digits".to_string(),
                    ));
                }
                let raw_ssid = suffix.parse::<u8>().map_err(|_| {
                    Ax25Error::InvalidAddress("SSID suffix must be in the range 1-15".to_string())
                })?;
                (Callsign::new(callsign)?, Ssid::new(raw_ssid)?)
            }
        };

        Ok(Self::from_parts(callsign, ssid))
    }
}

impl fmt::Display for Ax25Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ssid.get() == 0 {
            write!(f, "{}", self.callsign)
        } else {
            write!(f, "{}-{}", self.callsign, self.ssid)
        }
    }
}

impl core::str::FromStr for Ax25Address {
    type Err = Ax25Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::from_canonical_str(value)
    }
}

// ---------------------------------------------------------------------------
// RouteEntry
// ---------------------------------------------------------------------------

/// A single hop in an AX.25 frame's digipeater path.
///
/// AX.25 v2.2 §3.12.1 places the "has been repeated" (H) bit on each
/// repeater address's SSID byte. The bit indicates that the named
/// repeater has already retransmitted the frame. Each digipeater slot
/// in an [`crate::Ax25Packet`] is a `RouteEntry`, distinguishing it
/// structurally from endpoint addresses (which use [`Ax25Address`]
/// directly and carry no H-bit at all).
/// See the [address and path examples](crate#addresses-and-paths).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteEntry {
    /// Repeater callsign + SSID.
    pub address: Ax25Address,
    /// AX.25 v2.2 §3.12.1 H-bit ("has been repeated"). Set by the
    /// repeater station when it retransmits the frame.
    pub has_repeated: bool,
}

impl RouteEntry {
    /// Build a fresh (unrepeated) route entry from a raw callsign / SSID.
    ///
    /// # Errors
    ///
    /// Forwards [`Ax25Error::InvalidCallsign`] / [`Ax25Error::InvalidSsid`]
    /// from [`Ax25Address::new`].
    pub fn new(callsign: &str, ssid: u8) -> Result<Self, Ax25Error> {
        Ok(Self {
            address: Ax25Address::new(callsign, ssid)?,
            has_repeated: false,
        })
    }

    /// Infallible constructor from an already-validated address.
    #[must_use]
    pub const fn from_address(address: Ax25Address) -> Self {
        Self {
            address,
            has_repeated: false,
        }
    }

    /// Consume this entry and set its has-been-repeated H-bit.
    ///
    /// The callsign allocation is retained. Clone before calling when the
    /// original entry must remain available.
    #[must_use]
    pub const fn marked_used(mut self) -> Self {
        self.has_repeated = true;
        self
    }
}

impl From<Ax25Address> for RouteEntry {
    fn from(address: Ax25Address) -> Self {
        Self::from_address(address)
    }
}

impl fmt::Display for RouteEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.address)?;
        if self.has_repeated {
            f.write_str("*")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alloc::boxed::Box;

    use super::*;

    type TestResult = Result<(), Box<dyn core::error::Error>>;

    #[test]
    fn callsign_accepts_uppercase_and_digits() {
        assert!(Callsign::new("N0CALL").is_ok());
        assert!(Callsign::new("W1AW").is_ok());
        assert!(Callsign::new("KQ4NIT").is_ok());
        assert!(Callsign::new("K").is_ok()); // shortest
    }

    #[test]
    fn callsign_rejects_lowercase() {
        let r = Callsign::new("n0call");
        assert!(
            matches!(r, Err(Ax25Error::InvalidCallsign(_))),
            "expected InvalidCallsign, got {r:?}"
        );
    }

    #[test]
    fn callsign_rejects_empty() {
        let r = Callsign::new("");
        assert!(
            matches!(r, Err(Ax25Error::InvalidCallsign(_))),
            "expected InvalidCallsign, got {r:?}"
        );
    }

    #[test]
    fn callsign_rejects_too_long() {
        let r = Callsign::new("TOOLONG");
        assert!(
            matches!(r, Err(Ax25Error::InvalidCallsign(_))),
            "expected InvalidCallsign, got {r:?}"
        );
    }

    #[test]
    fn callsign_rejects_punctuation() {
        let r = Callsign::new("N0-CAL");
        assert!(
            matches!(r, Err(Ax25Error::InvalidCallsign(_))),
            "expected InvalidCallsign, got {r:?}"
        );
    }

    #[test]
    fn callsign_case_insensitive_ctor() -> TestResult {
        let a = Callsign::new_case_insensitive("n0call")?;
        let b = Callsign::new("N0CALL")?;
        assert_eq!(a, b);
        Ok(())
    }

    #[test]
    fn owned_callsign_conversion_preserves_the_allocation() -> TestResult {
        let input = String::from("N0CALL");
        let allocation = input.as_ptr();
        let callsign = Callsign::try_from(input)?;
        assert_eq!(callsign.as_str(), "N0CALL");
        assert_eq!(callsign.as_str().as_ptr(), allocation);
        let recovered = String::from(callsign);
        assert_eq!(recovered.as_ptr(), allocation);
        Ok(())
    }

    #[test]
    fn ssid_conversions_preserve_values_and_reject_out_of_range() -> TestResult {
        for value in 0..=u8::MAX {
            let converted = Ssid::try_from(value);
            assert_eq!(converted, Ssid::new(value), "SSID {value}");
            if value <= Ssid::MAX {
                assert_eq!(u8::from(converted?), value);
            }
        }
        Ok(())
    }

    #[test]
    fn ssid_valid_range() -> TestResult {
        assert_eq!(Ssid::new(0)?.get(), 0);
        assert_eq!(Ssid::new(15)?.get(), 15);
        Ok(())
    }

    #[test]
    fn ssid_rejects_too_large() {
        let r = Ssid::new(16);
        assert!(
            matches!(r, Err(Ax25Error::InvalidSsid(16))),
            "expected InvalidSsid(16), got {r:?}"
        );
    }

    #[test]
    fn ssid_saturating_decrement_preserves_the_valid_range() -> TestResult {
        assert_eq!(Ssid::ZERO.saturating_decrement(), Ssid::ZERO);
        assert_eq!(Ssid::new(1)?.saturating_decrement(), Ssid::ZERO);
        assert_eq!(Ssid::new(Ssid::MAX)?.saturating_decrement().get(), 14);
        Ok(())
    }

    #[test]
    fn canonical_address_parser_preserves_display_form() -> TestResult {
        let without_ssid = Ax25Address::from_canonical_str("W1AW")?;
        let with_ssid = Ax25Address::from_canonical_str("N0CALL-15")?;

        assert_eq!(without_ssid.to_string(), "W1AW");
        assert_eq!(without_ssid.ssid, Ssid::ZERO);
        assert_eq!(with_ssid.to_string(), "N0CALL-15");
        assert_eq!(with_ssid.ssid, Ssid::new(15)?);
        Ok(())
    }

    #[test]
    fn canonical_address_parser_rejects_normalizing_inputs() {
        for invalid in [
            "n0call-7",
            "N0CALL-0",
            "N0CALL-00",
            "N0CALL-07",
            "N0CALL-",
            "N0CALL-16",
            "N0CALL-A",
            "N0CALL-7-1",
        ] {
            assert!(
                Ax25Address::from_canonical_str(invalid).is_err(),
                "accepted noncanonical address {invalid:?}"
            );
        }
    }
}
