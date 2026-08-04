//! Strongly-typed primitives for APRS wire-format data.
//!
//! These newtypes are used by the APRS parsers and builders. Every type
//! validates at construction and rejects out-of-range values, making
//! illegal APRS packets unrepresentable.

use core::{fmt, num::NonZeroU16};

use ax25_codec::Callsign;

use crate::error::AprsError;

// ---------------------------------------------------------------------------
// Shared DDMM.hh formatter
// ---------------------------------------------------------------------------

/// Format the magnitude of a coordinate as the APRS uncompressed
/// `DDMM.hh` core (degrees + whole minutes + hundredths of a minute),
/// zero-padding the degree field to `deg_width` columns.
///
/// Per APRS 1.0.1 §6 p.23-24, the minutes field is **whole minutes
/// `00..=59` plus hundredths `00..=99`**, never `60.00`. A naive
/// `format!("{minutes:05.2}")` on a value like `59.9999` rounds the
/// printed minutes up to `60.00` with no carry into the degree field,
/// emitting a malformed coordinate that decodes a full degree off (or
/// is rejected outright).
///
/// This helper instead computes integer degrees, integer whole-minutes,
/// and integer hundredths-of-a-minute, **rounding the hundredths and
/// carrying any overflow upward**: hundredths `100` rolls into `+1`
/// minute, and minute `60` rolls into `+1` degree. The result always
/// satisfies `minutes < 60` and `hundredths < 100`.
///
/// `value_abs` must already be the non-negative magnitude of a validated,
/// in-range coordinate.
pub(crate) fn format_ddmm_hundredths(value_abs: f64, deg_width: usize) -> String {
    // Total hundredths-of-a-minute across the whole value, rounded to
    // the nearest integer. For a clamped latitude (<=90) this is at
    // most 90 * 60 * 100 = 540_000; for longitude (<=180) at most
    // 1_080_000. Both are far inside u32.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "value_abs is a non-negative, validated coordinate magnitude, so \
                  value_abs * 6000 is in 0..=1_080_000 and the rounded cast to u32 cannot \
                  truncate or sign-flip"
    )]
    let total_hundredths = (value_abs * 6000.0).round() as u32;

    // Decompose, carrying overflow upward so the printed minute field is
    // always whole-minutes 00..=59 plus hundredths 00..=99.
    let hundredths = total_hundredths % 100;
    let total_minutes = total_hundredths / 100;
    let minutes = total_minutes % 60;
    let degrees = total_minutes / 60;

    // `deg_width` is the zero-padded degree-field width (2 for latitude,
    // 3 for longitude).
    format!("{degrees:0deg_width$}{minutes:02}.{hundredths:02}")
}

// ---------------------------------------------------------------------------
// Latitude / Longitude
// ---------------------------------------------------------------------------

/// Geographic latitude in decimal degrees, validated to `[-90.0, 90.0]`.
///
/// Positive = North, negative = South. Rejects NaN and out-of-range
/// values.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Latitude(f64);

impl Latitude {
    /// Minimum valid latitude (South Pole).
    pub const MIN: f64 = -90.0;
    /// Maximum valid latitude (North Pole).
    pub const MAX: f64 = 90.0;
    /// The equator (`0°`).
    pub const EQUATOR: Self = Self(0.0);

    /// Create a latitude, rejecting NaN or out-of-range values.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidLatitude`] if `degrees` is not finite
    /// or not in `[-90.0, 90.0]`.
    pub fn new(degrees: f64) -> Result<Self, AprsError> {
        if !degrees.is_finite() || !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(AprsError::InvalidLatitude(
                "must be finite and in [-90.0, 90.0]",
            ));
        }
        Ok(Self(degrees))
    }

    /// Return the latitude as decimal degrees.
    #[must_use]
    pub const fn as_degrees(self) -> f64 {
        self.0
    }

    /// Format this latitude as the standard APRS uncompressed 8-byte
    /// field `DDMM.HHN` (or `…S` for southern hemisphere).
    ///
    /// The `DDMM.hh` core is produced by `format_ddmm_hundredths`,
    /// which carries minute/degree overflow correctly so the minutes
    /// field is always whole minutes `00..=59` plus hundredths
    /// `00..=99` per APRS 1.0.1 §6 p.23 (never the malformed `60.00`).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'N' } else { 'S' };
        let core = format_ddmm_hundredths(self.0.abs(), 2);
        format!("{core}{hemisphere}")
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
    /// The prime meridian (`0°`).
    pub const PRIME_MERIDIAN: Self = Self(0.0);

    /// Create a longitude, rejecting NaN or out-of-range values.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidLongitude`] if `degrees` is not finite
    /// or not in `[-180.0, 180.0]`.
    pub fn new(degrees: f64) -> Result<Self, AprsError> {
        if !degrees.is_finite() || !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(AprsError::InvalidLongitude(
                "must be finite and in [-180.0, 180.0]",
            ));
        }
        Ok(Self(degrees))
    }

    /// Return the longitude as decimal degrees.
    #[must_use]
    pub const fn as_degrees(self) -> f64 {
        self.0
    }

    /// Format this longitude as the standard APRS uncompressed 9-byte
    /// field `DDDMM.HHE` (or `…W` for western hemisphere).
    ///
    /// The `DDDMM.hh` core is produced by `format_ddmm_hundredths`,
    /// which carries minute/degree overflow correctly so the minutes
    /// field is always whole minutes `00..=59` plus hundredths
    /// `00..=99` per APRS 1.0.1 §6 p.24 (never the malformed `60.00`).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'E' } else { 'W' };
        let core = format_ddmm_hundredths(self.0.abs(), 3);
        format!("{core}{hemisphere}")
    }
}

// ---------------------------------------------------------------------------
// Speed
// ---------------------------------------------------------------------------

/// A validated ground-speed measurement.
///
/// The canonical representation is kilometers per hour. Named constructors
/// make the caller's input unit explicit and reject negative, non-finite, or
/// conversion-overflowing values. Accessors return decimal values and never
/// silently round to an integer wire field.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Speed(f64);

impl Speed {
    /// Conversion factor: 1 knot = `1.852` `km/h`.
    pub const KNOTS_TO_KMH: f64 = 1.852;
    /// Conversion factor: 1 mph = `1.609_344` `km/h`.
    pub const MPH_TO_KMH: f64 = 1.609_344;

    /// Create a speed measured in kilometers per hour.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] unless `kmh` is finite and
    /// non-negative.
    pub fn from_kmh(kmh: f64) -> Result<Self, AprsError> {
        Self::from_converted_kmh(kmh)
    }

    /// Create a speed measured in knots.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] unless `knots` and its converted
    /// value are finite and non-negative.
    pub fn from_knots(knots: f64) -> Result<Self, AprsError> {
        if !knots.is_finite() || knots < 0.0 {
            return Err(AprsError::InvalidSpeed(
                "knots must be finite and non-negative",
            ));
        }
        Self::from_converted_kmh(knots * Self::KNOTS_TO_KMH)
    }

    /// Create a speed measured in statute miles per hour.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] unless `mph` and its converted
    /// value are finite and non-negative.
    pub fn from_mph(mph: f64) -> Result<Self, AprsError> {
        if !mph.is_finite() || mph < 0.0 {
            return Err(AprsError::InvalidSpeed(
                "miles per hour must be finite and non-negative",
            ));
        }
        Self::from_converted_kmh(mph * Self::MPH_TO_KMH)
    }

    fn from_converted_kmh(kmh: f64) -> Result<Self, AprsError> {
        if !kmh.is_finite() || kmh < 0.0 {
            return Err(AprsError::InvalidSpeed(
                "speed must be finite and non-negative",
            ));
        }
        Ok(Self(kmh))
    }

    /// Return the speed in kilometers per hour.
    #[must_use]
    pub const fn as_kmh(self) -> f64 {
        self.0
    }

    /// Return the speed in knots.
    #[must_use]
    pub fn as_knots(self) -> f64 {
        self.0 / Self::KNOTS_TO_KMH
    }

    /// Return the speed in statute miles per hour.
    #[must_use]
    pub fn as_mph(self) -> f64 {
        self.0 / Self::MPH_TO_KMH
    }
}

/// A Mic-E wire speed in whole knots (`0..=799`).
///
/// Mic-E allocates three decimal digits across its speed/course bytes. This
/// type prevents the encoder from silently clamping a larger speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MiceSpeed(u16);

impl MiceSpeed {
    /// Maximum representable Mic-E speed in knots.
    pub const MAX_KNOTS: u16 = 799;

    /// Create a Mic-E speed from whole knots.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] when `knots > 799`.
    pub const fn new(knots: u16) -> Result<Self, AprsError> {
        if knots > Self::MAX_KNOTS {
            return Err(AprsError::InvalidSpeed(
                "Mic-E speed must be 0-799 whole knots",
            ));
        }
        Ok(Self(knots))
    }

    /// Return the whole-knot Mic-E wire value.
    #[must_use]
    pub const fn as_knots(self) -> u16 {
        self.0
    }
}

/// A validated true heading in decimal degrees (`0..=360`).
///
/// Unlike [`Course`], this type retains fractional sensor precision and does
/// not assign a special "unknown" meaning to zero. Use it for navigation and
/// `SmartBeaconing` calculations.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Heading(f64);

impl Heading {
    /// Create a true heading.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidCourse`] unless `degrees` is finite and
    /// inside `0..=360`.
    pub fn new(degrees: f64) -> Result<Self, AprsError> {
        if !degrees.is_finite() || !(0.0..=360.0).contains(&degrees) {
            return Err(AprsError::InvalidCourse(
                "heading must be finite and in 0-360 degrees",
            ));
        }
        Ok(Self(degrees))
    }

    /// Return the heading in decimal degrees.
    #[must_use]
    pub const fn as_degrees(self) -> f64 {
        self.0
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
    /// Returns [`AprsError::InvalidCourse`] if `degrees > 360`.
    pub const fn new(degrees: u16) -> Result<Self, AprsError> {
        if degrees <= Self::MAX {
            Ok(Self(degrees))
        } else {
            Err(AprsError::InvalidCourse("must be 0-360 degrees"))
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
    /// Returns [`AprsError::InvalidMessageId`] if the input is empty,
    /// longer than 5 characters, or contains non-alphanumeric bytes.
    pub fn new(s: &str) -> Result<Self, AprsError> {
        if s.is_empty() || s.len() > Self::MAX_LEN {
            return Err(AprsError::InvalidMessageId("must be 1-5 characters"));
        }
        if !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(AprsError::InvalidMessageId("must be ASCII alphanumeric"));
        }
        Ok(Self(s.to_owned()))
    }

    /// Create the decimal message ID for a nonzero sequence number.
    ///
    /// Every nonzero `u16` formats as one to five ASCII digits, so this
    /// constructor is infallible while preserving the same invariant as
    /// [`Self::new`]. It is useful for state machines that generate APRS
    /// message IDs from a wrapping counter.
    #[must_use]
    pub fn from_sequence_number(sequence: NonZeroU16) -> Self {
        Self(sequence.to_string())
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
/// - `/`: Primary table (most common symbols)
/// - `\`: Alternate table
/// - `0-9` or `A-Z`: Overlay character (displays on top of the alternate
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
    /// Returns [`AprsError::InvalidSymbolTable`] for anything other than
    /// `/`, `\`, digits, or uppercase ASCII letters.
    pub const fn from_byte(b: u8) -> Result<Self, AprsError> {
        match b {
            b'/' => Ok(Self::Primary),
            b'\\' => Ok(Self::Alternate),
            b'0'..=b'9' | b'A'..=b'Z' => Ok(Self::Overlay(b)),
            _ => Err(AprsError::InvalidSymbolTable(
                "must be '/', '\\\\', 0-9, or A-Z",
            )),
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

/// A validated APRS symbol (table selector + one-byte code).
///
/// Construct symbols with [`Self::new`] or [`Self::from_chars`]. The fields
/// are private so an invalid wire symbol cannot be represented after
/// construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsSymbol {
    table: SymbolTable,
    code: u8,
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

    /// Create a symbol from a validated table and an APRS wire byte.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSymbol`] unless `code` is printable
    /// ASCII (`!` through `~`), as required by APRS 1.0.1 §5.1.
    pub const fn new(table: SymbolTable, code: u8) -> Result<Self, AprsError> {
        if code < b'!' || code > b'~' {
            return Err(AprsError::InvalidSymbol(
                "code must be printable ASCII (0x21-0x7E)",
            ));
        }
        Ok(Self { table, code })
    }

    /// Parse the table selector and symbol code from user-facing characters.
    ///
    /// This checks that each `char` is representable as exactly one ASCII
    /// wire byte before conversion. Unicode characters are rejected instead
    /// of being truncated to an unrelated byte.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSymbolTable`] for an invalid table or
    /// [`AprsError::InvalidSymbol`] for a non-ASCII/non-printable code.
    pub fn from_chars(table: char, code: char) -> Result<Self, AprsError> {
        if !table.is_ascii() {
            return Err(AprsError::InvalidSymbolTable(
                "must be one ASCII byte: '/', '\\\\', 0-9, or A-Z",
            ));
        }
        if !code.is_ascii() {
            return Err(AprsError::InvalidSymbol(
                "code must be one printable ASCII byte (0x21-0x7E)",
            ));
        }
        let table = SymbolTable::from_byte(table as u8)?;
        Self::new(table, code as u8)
    }

    /// Return the validated symbol-table selector.
    #[must_use]
    pub const fn table(self) -> SymbolTable {
        self.table
    }

    /// Return the symbol-table selector as its APRS wire byte.
    #[must_use]
    pub const fn table_byte(self) -> u8 {
        self.table.as_byte()
    }

    /// Return the symbol code as its APRS wire byte.
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }

    /// Return the symbol-table selector as a user-facing character.
    #[must_use]
    pub const fn table_char(self) -> char {
        self.table.as_byte() as char
    }

    /// Return the symbol code as a user-facing character.
    #[must_use]
    pub const fn code_char(self) -> char {
        self.code as char
    }
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
    /// Returns [`AprsError::InvalidTemperature`] if `f` is not in
    /// `-99..=999`.
    pub const fn new(f: i16) -> Result<Self, AprsError> {
        if f < Self::MIN || f > Self::MAX {
            return Err(AprsError::InvalidTemperature("must be -99..=999"));
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

/// An APRS "tocall": the destination callsign used to identify the
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
    /// Returns [`AprsError::InvalidTocall`] on invalid input.
    pub fn new(s: &str) -> Result<Self, AprsError> {
        // Reuse Callsign's validation rules; tocalls are structurally
        // identical to callsigns, they're just a different namespace.
        // `Callsign::new` lives in ax25-codec and returns `Ax25Error`;
        // map to this crate's `AprsError` at the boundary.
        let _validated = Callsign::new(s)
            .map_err(|_| AprsError::InvalidTocall("must be 1-6 uppercase A-Z or 0-9"))?;
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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn latitude_accepts_valid_range() -> TestResult {
        let _lat = Latitude::new(0.0)?;
        let _lat = Latitude::new(90.0)?;
        let _lat = Latitude::new(-90.0)?;
        let _lat = Latitude::new(35.25)?;
        Ok(())
    }

    #[test]
    fn latitude_rejects_out_of_range() {
        assert!(matches!(
            Latitude::new(90.01),
            Err(AprsError::InvalidLatitude(_))
        ));
        assert!(matches!(
            Latitude::new(-90.01),
            Err(AprsError::InvalidLatitude(_))
        ));
        assert!(matches!(
            Latitude::new(f64::NAN),
            Err(AprsError::InvalidLatitude(_))
        ));
        assert!(matches!(
            Latitude::new(f64::INFINITY),
            Err(AprsError::InvalidLatitude(_))
        ));
    }

    #[test]
    fn longitude_accepts_valid_range() -> TestResult {
        let _lon = Longitude::new(180.0)?;
        let _lon = Longitude::new(-180.0)?;
        let _lon = Longitude::new(0.0)?;
        Ok(())
    }

    #[test]
    fn longitude_rejects_out_of_range() {
        assert!(matches!(
            Longitude::new(180.01),
            Err(AprsError::InvalidLongitude(_))
        ));
        assert!(matches!(
            Longitude::new(-180.01),
            Err(AprsError::InvalidLongitude(_))
        ));
        assert!(matches!(
            Longitude::new(f64::NAN),
            Err(AprsError::InvalidLongitude(_))
        ));
        assert!(matches!(
            Longitude::new(f64::INFINITY),
            Err(AprsError::InvalidLongitude(_))
        ));
    }

    #[test]
    fn speed_conversions() -> TestResult {
        let s = Speed::from_knots(10.0)?;
        assert!((s.as_kmh() - 18.52).abs() < 1e-6);
        let s = Speed::from_kmh(100.0)?;
        assert!((s.as_knots() - 53.995_680_345_6).abs() < 1e-9);
        let s = Speed::from_mph(60.0)?;
        assert!((s.as_kmh() - 96.5606).abs() < 1e-3);
        assert!((s.as_mph() - 60.0).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn speed_rejects_negative_non_finite_and_conversion_overflow() {
        assert!(matches!(
            Speed::from_kmh(-0.1),
            Err(AprsError::InvalidSpeed(_))
        ));
        assert!(matches!(
            Speed::from_knots(f64::NAN),
            Err(AprsError::InvalidSpeed(_))
        ));
        assert!(matches!(
            Speed::from_mph(f64::MAX),
            Err(AprsError::InvalidSpeed(_))
        ));
    }

    #[test]
    fn mice_speed_validates_wire_range() -> TestResult {
        assert_eq!(MiceSpeed::new(0)?.as_knots(), 0);
        assert_eq!(MiceSpeed::new(799)?.as_knots(), 799);
        assert!(matches!(
            MiceSpeed::new(800),
            Err(AprsError::InvalidSpeed(_))
        ));
        Ok(())
    }

    #[test]
    fn heading_validates_sensor_range() -> TestResult {
        assert!((Heading::new(0.0)?.as_degrees() - 0.0).abs() < f64::EPSILON);
        assert!((Heading::new(123.45)?.as_degrees() - 123.45).abs() < f64::EPSILON);
        assert!((Heading::new(360.0)?.as_degrees() - 360.0).abs() < f64::EPSILON);
        assert!(matches!(
            Heading::new(-0.1),
            Err(AprsError::InvalidCourse(_))
        ));
        assert!(matches!(
            Heading::new(f64::INFINITY),
            Err(AprsError::InvalidCourse(_))
        ));
        Ok(())
    }

    #[test]
    fn course_valid_range() -> TestResult {
        assert_eq!(Course::new(0)?.as_degrees(), 0);
        assert_eq!(Course::new(360)?.as_degrees(), 360);
        assert_eq!(Course::new(180)?.as_degrees(), 180);
        Ok(())
    }

    #[test]
    fn course_rejects_too_large() {
        assert!(matches!(Course::new(361), Err(AprsError::InvalidCourse(_))));
    }

    #[test]
    fn message_id_valid() -> TestResult {
        assert_eq!(MessageId::new("1")?.as_str(), "1");
        assert_eq!(MessageId::new("12345")?.as_str(), "12345");
        assert_eq!(MessageId::new("ABC")?.as_str(), "ABC");
        Ok(())
    }

    #[test]
    fn message_id_from_sequence_number_covers_u16_boundaries() {
        assert_eq!(
            MessageId::from_sequence_number(NonZeroU16::MIN).as_str(),
            "1"
        );
        assert_eq!(
            MessageId::from_sequence_number(NonZeroU16::MAX).as_str(),
            "65535"
        );
    }

    #[test]
    fn message_id_rejects_empty_or_long() {
        assert!(matches!(
            MessageId::new(""),
            Err(AprsError::InvalidMessageId(_))
        ));
        assert!(matches!(
            MessageId::new("123456"),
            Err(AprsError::InvalidMessageId(_))
        ));
    }

    #[test]
    fn message_id_rejects_non_alnum() {
        assert!(matches!(
            MessageId::new("12-3"),
            Err(AprsError::InvalidMessageId(_))
        ));
        assert!(matches!(
            MessageId::new("ab c"),
            Err(AprsError::InvalidMessageId(_))
        ));
    }

    #[test]
    fn symbol_table_parse() -> TestResult {
        assert_eq!(SymbolTable::from_byte(b'/')?, SymbolTable::Primary);
        assert_eq!(SymbolTable::from_byte(b'\\')?, SymbolTable::Alternate);
        assert_eq!(SymbolTable::from_byte(b'9')?, SymbolTable::Overlay(b'9'));
        assert_eq!(SymbolTable::from_byte(b'Z')?, SymbolTable::Overlay(b'Z'));
        assert!(matches!(
            SymbolTable::from_byte(b'a'),
            Err(AprsError::InvalidSymbolTable(_))
        ));
        assert!(matches!(
            SymbolTable::from_byte(b'!'),
            Err(AprsError::InvalidSymbolTable(_))
        ));
        Ok(())
    }

    #[test]
    fn symbol_table_round_trip() -> TestResult {
        for b in [b'/', b'\\', b'0', b'5', b'A', b'Z'] {
            let table = SymbolTable::from_byte(b)?;
            assert_eq!(table.as_byte(), b);
        }
        Ok(())
    }

    #[test]
    fn aprs_symbol_round_trip() -> TestResult {
        let symbol = AprsSymbol::from_chars('/', '>')?;
        assert_eq!(symbol.table(), SymbolTable::Primary);
        assert_eq!(symbol.table_byte(), b'/');
        assert_eq!(symbol.code(), b'>');
        assert_eq!(symbol.table_char(), '/');
        assert_eq!(symbol.code_char(), '>');
        Ok(())
    }

    #[test]
    fn aprs_symbol_rejects_non_printable_code() {
        assert!(matches!(
            AprsSymbol::new(SymbolTable::Primary, b' '),
            Err(AprsError::InvalidSymbol(_))
        ));
        assert!(matches!(
            AprsSymbol::new(SymbolTable::Primary, 0x7F),
            Err(AprsError::InvalidSymbol(_))
        ));
    }

    #[test]
    fn aprs_symbol_rejects_unicode_instead_of_truncating_it() {
        assert!(matches!(
            AprsSymbol::from_chars('\u{2215}', '>'),
            Err(AprsError::InvalidSymbolTable(_))
        ));
        assert!(matches!(
            AprsSymbol::from_chars('/', '\u{013E}'),
            Err(AprsError::InvalidSymbol(_))
        ));
    }

    #[test]
    fn fahrenheit_valid_range() -> TestResult {
        assert_eq!(Fahrenheit::new(-99)?.get(), -99);
        assert_eq!(Fahrenheit::new(999)?.get(), 999);
        assert_eq!(Fahrenheit::new(72)?.get(), 72);
        Ok(())
    }

    #[test]
    fn fahrenheit_rejects_out_of_range() {
        assert!(matches!(
            Fahrenheit::new(-100),
            Err(AprsError::InvalidTemperature(_))
        ));
        assert!(matches!(
            Fahrenheit::new(1000),
            Err(AprsError::InvalidTemperature(_))
        ));
    }

    #[test]
    fn tocall_th_d75() {
        assert_eq!(Tocall::th_d75().as_str(), "APK005");
        assert_eq!(Tocall::TH_D75, "APK005");
    }

    #[test]
    fn tocall_validates() -> TestResult {
        let _tc = Tocall::new("APK005")?;
        let _tc = Tocall::new("APXXXX")?;
        assert!(matches!(
            Tocall::new("toolongname"),
            Err(AprsError::InvalidTocall(_))
        ));
        assert!(matches!(Tocall::new(""), Err(AprsError::InvalidTocall(_))));
        Ok(())
    }

    #[test]
    fn latitude_aprs_format_north() -> TestResult {
        let lat = Latitude::new(49.058_333)?;
        let s = lat.as_aprs_uncompressed();
        assert_eq!(s.len(), 8);
        assert!(s.ends_with('N'));
        assert!(s.starts_with("49"));
        Ok(())
    }

    #[test]
    fn latitude_aprs_format_south() -> TestResult {
        let lat = Latitude::new(-33.856)?;
        let s = lat.as_aprs_uncompressed();
        assert!(s.ends_with('S'));
        Ok(())
    }

    #[test]
    fn longitude_aprs_format_west() -> TestResult {
        let lon = Longitude::new(-72.029_166)?;
        let s = lon.as_aprs_uncompressed();
        assert_eq!(s.len(), 9);
        assert!(s.ends_with('W'));
        assert!(s.starts_with("072"));
        Ok(())
    }

    #[test]
    fn longitude_aprs_format_east() -> TestResult {
        let lon = Longitude::new(151.209)?;
        let s = lon.as_aprs_uncompressed();
        assert!(s.ends_with('E'));
        assert!(s.starts_with("151"));
        Ok(())
    }

    // ---- format_ddmm_hundredths carry correctness (APRS 1.0.1 §6) ----

    /// Split a `DDMM.hh` core into its (degrees, minutes, hundredths)
    /// integer components for boundary assertions. `deg_width` is the
    /// number of leading degree digits (2 for latitude, 3 for
    /// longitude).
    fn split_ddmm(
        core: &str,
        deg_width: usize,
    ) -> Result<(u32, u32, u32), Box<dyn std::error::Error>> {
        let degrees: u32 = core
            .get(..deg_width)
            .ok_or("degree field missing")?
            .parse()?;
        let minutes: u32 = core
            .get(deg_width..deg_width + 2)
            .ok_or("minute field missing")?
            .parse()?;
        // Skip the '.' separator between minutes and hundredths.
        let hundredths: u32 = core
            .get(deg_width + 3..deg_width + 5)
            .ok_or("hundredths field missing")?
            .parse()?;
        Ok((degrees, minutes, hundredths))
    }

    #[test]
    fn ddmm_normal_value_formats_exactly() {
        // 49.058333° → 49° 03.50' (the spec's worked example).
        let core = format_ddmm_hundredths(49.058_333, 2);
        assert_eq!(core, "4903.50", "expected 4903.50, got {core}");
    }

    #[test]
    fn ddmm_latitude_carry_boundary_33_999999() -> TestResult {
        // 33.999999° used to print "3360.00" (minutes rounded to 60.00
        // with no carry). The carry-correct helper must roll to 34° 00.00'.
        let core = format_ddmm_hundredths(33.999_999, 2);
        let (deg, min, hun) = split_ddmm(&core, 2)?;
        assert!(min < 60, "minutes must stay < 60, got {min} in {core}");
        assert_eq!(
            (deg, min, hun),
            (34, 0, 0),
            "carry into degree wrong: {core}"
        );
        Ok(())
    }

    #[test]
    fn ddmm_latitude_carry_boundary_89_999999() -> TestResult {
        // Just under the North Pole: must carry to 90° 00.00', not 89° 60.00'.
        let core = format_ddmm_hundredths(89.999_999, 2);
        let (deg, min, hun) = split_ddmm(&core, 2)?;
        assert!(min < 60, "minutes must stay < 60, got {min} in {core}");
        assert_eq!(
            (deg, min, hun),
            (90, 0, 0),
            "carry into degree wrong: {core}"
        );
        Ok(())
    }

    #[test]
    fn ddmm_longitude_carry_boundary_179_999999() -> TestResult {
        // Just under the date line (3-digit degree field).
        let core = format_ddmm_hundredths(179.999_999, 3);
        let (deg, min, hun) = split_ddmm(&core, 3)?;
        assert!(min < 60, "minutes must stay < 60, got {min} in {core}");
        assert_eq!(
            (deg, min, hun),
            (180, 0, 0),
            "carry into degree wrong: {core}"
        );
        Ok(())
    }

    #[test]
    fn ddmm_longitude_carry_boundary_97_999983() -> TestResult {
        // 97.999983° used to print "09760.00"; it must carry to 98° 00.00'.
        let core = format_ddmm_hundredths(97.999_983, 3);
        let (deg, min, hun) = split_ddmm(&core, 3)?;
        assert!(min < 60, "minutes must stay < 60, got {min} in {core}");
        assert_eq!(
            (deg, min, hun),
            (98, 0, 0),
            "carry into degree wrong: {core}"
        );
        Ok(())
    }

    #[test]
    fn latitude_newtype_carry_boundary_no_60_minutes() -> TestResult {
        // The Latitude newtype method must also carry correctly.
        let lat = Latitude::new(33.999_999)?;
        let s = lat.as_aprs_uncompressed();
        assert_eq!(s, "3400.00N", "expected carry to 3400.00N, got {s}");
        assert_eq!(s.len(), 8, "latitude field is 8 bytes");
        Ok(())
    }

    #[test]
    fn longitude_newtype_carry_boundary_no_60_minutes() -> TestResult {
        // The Longitude newtype method (western hemisphere) must carry.
        let lon = Longitude::new(-97.999_983)?;
        let s = lon.as_aprs_uncompressed();
        assert_eq!(s, "09800.00W", "expected carry to 09800.00W, got {s}");
        assert_eq!(s.len(), 9, "longitude field is 9 bytes");
        Ok(())
    }
}
