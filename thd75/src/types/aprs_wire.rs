//! Strongly-typed primitives for APRS wire-format data.
//!
//! These newtypes are used by the [`kiss`](crate::kiss) module for parsing
//! and building KISS / AX.25 / APRS frames. They sit alongside, but are
//! distinct from, the configuration-oriented types in
//! [`types::aprs`](crate::types::aprs) which model the radio's own MCP
//! settings for its on-board APRS subsystem.
//!
//! **Separation of concerns:**
//!
//! | Module | Purpose | Example |
//! |---|---|---|
//! | [`types::aprs`](crate::types::aprs) | MCP settings stored in the radio's flash memory | `AprsCallsign` (up to 9 chars incl. SSID) |
//! | [`types::aprs_wire`](self) | Runtime wire-format parsing and construction | `Callsign` (1-6 chars, no SSID) + `Ssid` |
//!
//! Every type validates at construction and rejects out-of-range values,
//! making illegal APRS packets unrepresentable.

use std::fmt;

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Callsign
// ---------------------------------------------------------------------------

/// An AX.25 base callsign (without SSID).
///
/// Per AX.25 v2.2 §3.2, a callsign is **1 to 6 ASCII characters**, each of
/// which is an uppercase letter (`A`-`Z`) or a digit (`0`-`9`). The wire
/// format left-shifts each byte by one bit; this type stores the plain
/// ASCII form and does the shifting at encode time.
///
/// Use [`Ssid`] alongside this type to represent the full 7-byte AX.25
/// address. For APRS-IS login strings or display, format with `-` between
/// callsign and SSID (e.g. `"N0CALL-7"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// Returns [`ValidationError::AprsWireOutOfRange`] if the input fails
    /// any of the above rules.
    pub fn new(s: &str) -> Result<Self, ValidationError> {
        if s.is_empty() {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "Callsign",
                detail: "must not be empty",
            });
        }
        if s.len() > Self::MAX_LEN {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "Callsign",
                detail: "length exceeds 6 characters",
            });
        }
        for &b in s.as_bytes() {
            let valid = b.is_ascii_uppercase() || b.is_ascii_digit();
            if !valid {
                return Err(ValidationError::AprsWireOutOfRange {
                    field: "Callsign byte",
                    detail: "must be uppercase A-Z or digit 0-9",
                });
            }
        }
        Ok(Self(s.to_owned()))
    }

    /// Like [`Self::new`] but uppercases lowercase ASCII first.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new`] after case conversion.
    pub fn new_case_insensitive(s: &str) -> Result<Self, ValidationError> {
        let upper = s.to_ascii_uppercase();
        Self::new(&upper)
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

    /// Always `false` — a valid [`Callsign`] is never empty. Provided for
    /// slice-convention parity with `len`.
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

impl std::str::FromStr for Callsign {
    type Err = ValidationError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
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

impl std::ops::Deref for Callsign {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
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
/// SSIDs distinguish multiple stations running under the same callsign.
/// Conventional meanings on APRS:
/// - `0` — home station (fixed)
/// - `1` — generic digipeater
/// - `5` — other networks (Dstar, iGate)
/// - `7` — handheld radio
/// - `9` — mobile (car)
/// - `15` — generic / other
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ssid(u8);

impl Ssid {
    /// The `-0` SSID (home station / no suffix on display).
    pub const ZERO: Self = Self(0);

    /// Maximum legal SSID value.
    pub const MAX: u8 = 15;

    /// Create an SSID from a raw `u8`, validating `0..=15`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if `n > 15`.
    pub const fn new(n: u8) -> Result<Self, ValidationError> {
        if n <= Self::MAX {
            Ok(Self(n))
        } else {
            Err(ValidationError::AprsWireOutOfRange {
                field: "Ssid",
                detail: "must be 0-15",
            })
        }
    }

    /// Return the raw SSID value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Ssid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
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
    fn partial_cmp(&self, other: &u8) -> Option<std::cmp::Ordering> {
        self.0.partial_cmp(other)
    }
}

// ---------------------------------------------------------------------------
// Latitude / Longitude
// ---------------------------------------------------------------------------

/// Geographic latitude in decimal degrees, validated to `[-90.0, 90.0]`.
///
/// Positive = North, negative = South. Rejects NaN and out-of-range
/// values. Use [`Self::new`] for fallible construction and
/// [`Self::new_clamped`] when you prefer silent clamping.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Latitude(f64);

impl Latitude {
    /// Minimum valid latitude (South Pole).
    pub const MIN: f64 = -90.0;
    /// Maximum valid latitude (North Pole).
    pub const MAX: f64 = 90.0;

    /// Create a latitude, rejecting NaN or out-of-range values.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if `degrees` is
    /// not finite or not in `[-90.0, 90.0]`.
    pub fn new(degrees: f64) -> Result<Self, ValidationError> {
        if !degrees.is_finite() || !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "Latitude",
                detail: "must be finite and in [-90.0, 90.0]",
            });
        }
        Ok(Self(degrees))
    }

    /// Create a latitude, clamping any input to `[-90.0, 90.0]`. NaN
    /// becomes `0.0`.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // f64::clamp is not const stable
    pub fn new_clamped(degrees: f64) -> Self {
        if degrees.is_nan() {
            return Self(0.0);
        }
        Self(degrees.clamp(Self::MIN, Self::MAX))
    }

    /// Return the latitude as decimal degrees.
    #[must_use]
    pub const fn as_degrees(self) -> f64 {
        self.0
    }
}

/// Geographic longitude in decimal degrees, validated to `[-180.0, 180.0]`.
///
/// Positive = East, negative = West. Rejects NaN and out-of-range values.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Longitude(f64);

impl Longitude {
    /// Minimum valid longitude (International Date Line, west side).
    pub const MIN: f64 = -180.0;
    /// Maximum valid longitude (International Date Line, east side).
    pub const MAX: f64 = 180.0;

    /// Create a longitude, rejecting NaN or out-of-range values.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if `degrees` is
    /// not finite or not in `[-180.0, 180.0]`.
    pub fn new(degrees: f64) -> Result<Self, ValidationError> {
        if !degrees.is_finite() || !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "Longitude",
                detail: "must be finite and in [-180.0, 180.0]",
            });
        }
        Ok(Self(degrees))
    }

    /// Create a longitude, clamping to `[-180.0, 180.0]`. NaN → `0.0`.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // f64::clamp is not const stable
    pub fn new_clamped(degrees: f64) -> Self {
        if degrees.is_nan() {
            return Self(0.0);
        }
        Self(degrees.clamp(Self::MIN, Self::MAX))
    }

    /// Return the longitude as decimal degrees.
    #[must_use]
    pub const fn as_degrees(self) -> f64 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Speed
// ---------------------------------------------------------------------------

/// A ground-speed measurement with explicit units.
///
/// APRS uses multiple unit conventions depending on context:
/// - **Knots** — Mic-E and course/speed extension on wire
/// - **`Km/h`** — `SmartBeaconing` parameters
/// - **Mph** — US weather station display convention
///
/// This enum keeps each unit distinct and provides lossless conversions
/// so callers never accidentally mix them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Speed {
    /// Nautical miles per hour.
    Knots(u16),
    /// Kilometres per hour (decimal to allow `SmartBeaconing` thresholds).
    Kmh(f64),
    /// Statute miles per hour.
    Mph(u16),
}

impl Speed {
    /// Conversion factor: 1 knot = `1.852` `km/h`.
    pub const KNOTS_TO_KMH: f64 = 1.852;
    /// Conversion factor: 1 mph = `1.609_344` `km/h`.
    pub const MPH_TO_KMH: f64 = 1.609_344;

    /// Convert to `km/h`.
    #[must_use]
    pub fn as_kmh(self) -> f64 {
        match self {
            Self::Knots(k) => f64::from(k) * Self::KNOTS_TO_KMH,
            Self::Kmh(k) => k,
            Self::Mph(m) => f64::from(m) * Self::MPH_TO_KMH,
        }
    }

    /// Convert to knots (rounded to nearest integer).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn as_knots(self) -> u16 {
        match self {
            Self::Knots(k) => k,
            Self::Kmh(k) => (k / Self::KNOTS_TO_KMH).round() as u16,
            Self::Mph(m) => (f64::from(m) * Self::MPH_TO_KMH / Self::KNOTS_TO_KMH).round() as u16,
        }
    }
}

// ---------------------------------------------------------------------------
// Course
// ---------------------------------------------------------------------------

/// A course-over-ground value, validated to `0..=360` degrees.
///
/// By APRS convention, `0` means "course not known" (per Mic-E) while any
/// other value is a true-north bearing. To distinguish "not known" from
/// "due north" callers typically use `Option<Course>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Course(u16);

impl Course {
    /// Maximum legal course value.
    pub const MAX: u16 = 360;

    /// Create a course, validating `0..=360`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if `degrees > 360`.
    pub const fn new(degrees: u16) -> Result<Self, ValidationError> {
        if degrees <= Self::MAX {
            Ok(Self(degrees))
        } else {
            Err(ValidationError::AprsWireOutOfRange {
                field: "Course",
                detail: "must be 0-360 degrees",
            })
        }
    }

    /// Return the course in degrees.
    #[must_use]
    pub const fn as_degrees(self) -> u16 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// MessageId
// ---------------------------------------------------------------------------

/// An APRS message identifier: 1 to 5 alphanumeric characters.
///
/// Per APRS 1.0.1 §14, message IDs in the `{NNNNN` trailer and in ack/rej
/// frames are 1-5 characters drawn from `[A-Za-z0-9]`. This type enforces
/// those rules at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(String);

impl MessageId {
    /// Maximum length of a message ID.
    pub const MAX_LEN: usize = 5;

    /// Create a message ID, rejecting empty or non-alphanumeric input.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if the input is
    /// empty, longer than 5 characters, or contains non-alphanumeric
    /// bytes.
    pub fn new(s: &str) -> Result<Self, ValidationError> {
        if s.is_empty() || s.len() > Self::MAX_LEN {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "MessageId",
                detail: "must be 1-5 characters",
            });
        }
        if !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "MessageId",
                detail: "must be ASCII alphanumeric",
            });
        }
        Ok(Self(s.to_owned()))
    }

    /// Return the ID as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// SymbolTable / AprsSymbol
// ---------------------------------------------------------------------------

/// An APRS symbol table selector.
///
/// Per APRS 1.0.1 §5.1, the first character of a position report's symbol
/// pair selects the table:
/// - `/` — Primary table (most common symbols)
/// - `\` — Alternate table
/// - `0-9` or `A-Z` — Overlay character (displays on top of the alternate
///   table's symbol) used for groups and regional indicators
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolTable {
    /// Primary table (`/`).
    Primary,
    /// Alternate table (`\`).
    Alternate,
    /// Overlay character (digit or uppercase letter) on the alternate
    /// table.
    Overlay(u8),
}

impl SymbolTable {
    /// Parse a single byte into a `SymbolTable`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] for anything other
    /// than `/`, `\`, digits, or uppercase ASCII letters.
    pub const fn from_byte(b: u8) -> Result<Self, ValidationError> {
        match b {
            b'/' => Ok(Self::Primary),
            b'\\' => Ok(Self::Alternate),
            b'0'..=b'9' | b'A'..=b'Z' => Ok(Self::Overlay(b)),
            _ => Err(ValidationError::AprsWireOutOfRange {
                field: "SymbolTable",
                detail: "must be '/', '\\\\', 0-9, or A-Z",
            }),
        }
    }

    /// Convert back to the wire byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Primary => b'/',
            Self::Alternate => b'\\',
            Self::Overlay(b) => b,
        }
    }
}

/// A full APRS symbol (table selector + 1-byte code).
///
/// Example: `AprsSymbol { table: SymbolTable::Primary, code: b'>' }` is
/// the car icon (`/>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsSymbol {
    /// Symbol table selector.
    pub table: SymbolTable,
    /// Symbol code character (1 byte, spec range is `!` through `~`).
    pub code: u8,
}

impl AprsSymbol {
    /// Car symbol on the primary table (`/>`).
    pub const CAR: Self = Self {
        table: SymbolTable::Primary,
        code: b'>',
    };
    /// House QTH symbol on the primary table (`/-`).
    pub const HOUSE: Self = Self {
        table: SymbolTable::Primary,
        code: b'-',
    };
    /// Weather station symbol (`/_`).
    pub const WEATHER: Self = Self {
        table: SymbolTable::Primary,
        code: b'_',
    };
}

// ---------------------------------------------------------------------------
// Temperature (APRS weather)
// ---------------------------------------------------------------------------

/// Temperature in degrees Fahrenheit as used by APRS weather reports.
///
/// Per APRS 1.0.1 §12.4, weather `t` fields are 3 digits optionally with
/// a leading minus, giving the range `-99` to `999`. This newtype enforces
/// that range and rejects out-of-spec values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fahrenheit(i16);

impl Fahrenheit {
    /// Minimum valid value per APRS 1.0.1 §12.4.
    pub const MIN: i16 = -99;
    /// Maximum valid value per APRS 1.0.1 §12.4.
    pub const MAX: i16 = 999;

    /// Create a temperature, rejecting out-of-range input.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] if `f` is not in
    /// `-99..=999`.
    pub const fn new(f: i16) -> Result<Self, ValidationError> {
        if f < Self::MIN || f > Self::MAX {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "Fahrenheit",
                detail: "must be -99..=999",
            });
        }
        Ok(Self(f))
    }

    /// Return the raw Fahrenheit value.
    #[must_use]
    pub const fn get(self) -> i16 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Tocall
// ---------------------------------------------------------------------------

/// An APRS "tocall" — the destination callsign used to identify the
/// originating software or device.
///
/// APRS tocalls follow the form `APxxxx` where the `xxxx` is registered
/// with the APRS tocall registry. For the Kenwood TH-D75 the assigned
/// tocall is `APK005`. This newtype bundles the validation (1-6 ASCII
/// uppercase alphanumerics, just like [`Callsign`]) with well-known
/// constants for common devices.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tocall(String);

impl Tocall {
    /// Maximum length of a tocall.
    pub const MAX_LEN: usize = 6;

    /// The tocall assigned to the Kenwood TH-D75 / TH-D74 family
    /// (registered as `APK005` in the APRS tocall registry).
    pub const TH_D75: &'static str = "APK005";

    /// Create a tocall from a string, enforcing the same rules as
    /// [`Callsign::new`] (1-6 uppercase ASCII alphanumerics).
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] on invalid input.
    pub fn new(s: &str) -> Result<Self, ValidationError> {
        // Reuse Callsign's validation rules — tocalls are structurally
        // identical to callsigns, they're just a different namespace.
        let _ = Callsign::new(s)?;
        Ok(Self(s.to_owned()))
    }

    /// Build the TH-D75 tocall constant without going through validation.
    #[must_use]
    pub fn th_d75() -> Self {
        Self(Self::TH_D75.to_owned())
    }

    /// Return the tocall as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Tocall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Latitude/Longitude APRS formatting
// ---------------------------------------------------------------------------

impl Latitude {
    /// Format this latitude as the standard APRS uncompressed 8-byte
    /// field `DDMM.HHN` (or `…S` for southern hemisphere).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'N' } else { 'S' };
        let lat_abs = self.0.abs();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let degrees = lat_abs as u32;
        let minutes = (lat_abs - f64::from(degrees)) * 60.0;
        format!("{degrees:02}{minutes:05.2}{hemisphere}")
    }
}

impl Longitude {
    /// Format this longitude as the standard APRS uncompressed 9-byte
    /// field `DDDMM.HHE` (or `…W` for western hemisphere).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'E' } else { 'W' };
        let lon_abs = self.0.abs();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let degrees = lon_abs as u32;
        let minutes = (lon_abs - f64::from(degrees)) * 60.0;
        format!("{degrees:03}{minutes:05.2}{hemisphere}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsign_accepts_uppercase_and_digits() {
        assert!(Callsign::new("N0CALL").is_ok());
        assert!(Callsign::new("W1AW").is_ok());
        assert!(Callsign::new("KQ4NIT").is_ok());
        assert!(Callsign::new("K").is_ok()); // shortest
    }

    #[test]
    fn callsign_rejects_lowercase() {
        assert!(Callsign::new("n0call").is_err());
    }

    #[test]
    fn callsign_rejects_empty() {
        assert!(Callsign::new("").is_err());
    }

    #[test]
    fn callsign_rejects_too_long() {
        assert!(Callsign::new("TOOLONG").is_err());
    }

    #[test]
    fn callsign_rejects_punctuation() {
        assert!(Callsign::new("N0-CAL").is_err());
    }

    #[test]
    fn callsign_case_insensitive_ctor() {
        let a = Callsign::new_case_insensitive("n0call").unwrap();
        let b = Callsign::new("N0CALL").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn ssid_valid_range() {
        assert_eq!(Ssid::new(0).unwrap().get(), 0);
        assert_eq!(Ssid::new(15).unwrap().get(), 15);
    }

    #[test]
    fn ssid_rejects_too_large() {
        assert!(Ssid::new(16).is_err());
    }

    #[test]
    fn latitude_accepts_valid_range() {
        assert!(Latitude::new(0.0).is_ok());
        assert!(Latitude::new(90.0).is_ok());
        assert!(Latitude::new(-90.0).is_ok());
        assert!(Latitude::new(35.25).is_ok());
    }

    #[test]
    fn latitude_rejects_out_of_range() {
        assert!(Latitude::new(90.01).is_err());
        assert!(Latitude::new(-90.01).is_err());
        assert!(Latitude::new(f64::NAN).is_err());
        assert!(Latitude::new(f64::INFINITY).is_err());
    }

    #[test]
    fn latitude_clamped() {
        assert!((Latitude::new_clamped(200.0).as_degrees() - 90.0).abs() < f64::EPSILON);
        assert!((Latitude::new_clamped(-200.0).as_degrees() - (-90.0)).abs() < f64::EPSILON);
        assert!((Latitude::new_clamped(f64::NAN).as_degrees() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn longitude_accepts_valid_range() {
        assert!(Longitude::new(180.0).is_ok());
        assert!(Longitude::new(-180.0).is_ok());
        assert!(Longitude::new(0.0).is_ok());
    }

    #[test]
    fn longitude_rejects_out_of_range() {
        assert!(Longitude::new(180.01).is_err());
        assert!(Longitude::new(-180.01).is_err());
    }

    #[test]
    fn speed_conversions() {
        let s = Speed::Knots(10);
        assert!((s.as_kmh() - 18.52).abs() < 1e-6);
        let s = Speed::Kmh(100.0);
        assert_eq!(s.as_knots(), 54); // 100 / 1.852 ≈ 54.0
        let s = Speed::Mph(60);
        assert!((s.as_kmh() - 96.5606).abs() < 1e-3);
    }

    #[test]
    fn course_valid_range() {
        assert_eq!(Course::new(0).unwrap().as_degrees(), 0);
        assert_eq!(Course::new(360).unwrap().as_degrees(), 360);
        assert_eq!(Course::new(180).unwrap().as_degrees(), 180);
    }

    #[test]
    fn course_rejects_too_large() {
        assert!(Course::new(361).is_err());
    }

    #[test]
    fn message_id_valid() {
        assert_eq!(MessageId::new("1").unwrap().as_str(), "1");
        assert_eq!(MessageId::new("12345").unwrap().as_str(), "12345");
        assert_eq!(MessageId::new("ABC").unwrap().as_str(), "ABC");
    }

    #[test]
    fn message_id_rejects_empty_or_long() {
        assert!(MessageId::new("").is_err());
        assert!(MessageId::new("123456").is_err());
    }

    #[test]
    fn message_id_rejects_non_alnum() {
        assert!(MessageId::new("12-3").is_err());
        assert!(MessageId::new("ab c").is_err());
    }

    #[test]
    fn symbol_table_parse() {
        assert_eq!(SymbolTable::from_byte(b'/').unwrap(), SymbolTable::Primary);
        assert_eq!(
            SymbolTable::from_byte(b'\\').unwrap(),
            SymbolTable::Alternate
        );
        assert_eq!(
            SymbolTable::from_byte(b'9').unwrap(),
            SymbolTable::Overlay(b'9')
        );
        assert_eq!(
            SymbolTable::from_byte(b'Z').unwrap(),
            SymbolTable::Overlay(b'Z')
        );
        assert!(SymbolTable::from_byte(b'a').is_err());
        assert!(SymbolTable::from_byte(b'!').is_err());
    }

    #[test]
    fn symbol_table_round_trip() {
        for b in [b'/', b'\\', b'0', b'5', b'A', b'Z'] {
            let table = SymbolTable::from_byte(b).unwrap();
            assert_eq!(table.as_byte(), b);
        }
    }

    #[test]
    fn fahrenheit_valid_range() {
        assert_eq!(Fahrenheit::new(-99).unwrap().get(), -99);
        assert_eq!(Fahrenheit::new(999).unwrap().get(), 999);
        assert_eq!(Fahrenheit::new(72).unwrap().get(), 72);
    }

    #[test]
    fn fahrenheit_rejects_out_of_range() {
        assert!(Fahrenheit::new(-100).is_err());
        assert!(Fahrenheit::new(1000).is_err());
    }

    #[test]
    fn tocall_th_d75() {
        assert_eq!(Tocall::th_d75().as_str(), "APK005");
        assert_eq!(Tocall::TH_D75, "APK005");
    }

    #[test]
    fn tocall_validates() {
        assert!(Tocall::new("APK005").is_ok());
        assert!(Tocall::new("APXXXX").is_ok());
        assert!(Tocall::new("toolongname").is_err());
        assert!(Tocall::new("").is_err());
    }

    #[test]
    fn latitude_aprs_format_north() {
        let lat = Latitude::new(49.058_333).unwrap();
        let s = lat.as_aprs_uncompressed();
        assert_eq!(s.len(), 8);
        assert!(s.ends_with('N'));
        assert!(s.starts_with("49"));
    }

    #[test]
    fn latitude_aprs_format_south() {
        let lat = Latitude::new(-33.856).unwrap();
        let s = lat.as_aprs_uncompressed();
        assert!(s.ends_with('S'));
    }

    #[test]
    fn longitude_aprs_format_west() {
        let lon = Longitude::new(-72.029_166).unwrap();
        let s = lon.as_aprs_uncompressed();
        assert_eq!(s.len(), 9);
        assert!(s.ends_with('W'));
        assert!(s.starts_with("072"));
    }

    #[test]
    fn longitude_aprs_format_east() {
        let lon = Longitude::new(151.209).unwrap();
        let s = lon.as_aprs_uncompressed();
        assert!(s.ends_with('E'));
        assert!(s.starts_with("151"));
    }
}
