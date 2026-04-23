//! Strongly-typed primitives for APRS wire-format data.
//!
//! These newtypes are used by the APRS parsers and builders. Every type
//! validates at construction and rejects out-of-range values, making
//! illegal APRS packets unrepresentable.

use core::fmt;

use ax25_codec::Callsign;

use crate::error::AprsError;

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

    /// Create a latitude, clamping any input to `[-90.0, 90.0]`. NaN
    /// becomes `0.0`.
    #[must_use]
    #[expect(
        clippy::missing_const_for_fn,
        reason = "clippy suggests `const fn` based on structural shape, but `f64::clamp` \
                  is not const-stable yet (tracked at rust-lang/rust#93396). Cannot be made \
                  `const` until that stabilizes."
    )]
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

    /// Format this latitude as the standard APRS uncompressed 8-byte
    /// field `DDMM.HHN` (or `…S` for southern hemisphere).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'N' } else { 'S' };
        let lat_abs = self.0.abs();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "`lat_abs` is in [0.0, 90.0] by the Latitude invariant (validated at \
                      construction via `Latitude::new` / `Latitude::new_clamped`), so the \
                      `as u32` cast is always lossless. `cast_possible_truncation` fires \
                      because clippy can't prove the f64 range from the surrounding code; \
                      `cast_sign_loss` fires because f64→u32 drops the sign bit even though \
                      `.abs()` two lines above guarantees non-negative input."
        )]
        let degrees = lat_abs as u32;
        let minutes = (lat_abs - f64::from(degrees)) * 60.0;
        format!("{degrees:02}{minutes:05.2}{hemisphere}")
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

    /// Create a longitude, clamping to `[-180.0, 180.0]`. NaN → `0.0`.
    #[must_use]
    #[expect(
        clippy::missing_const_for_fn,
        reason = "clippy suggests `const fn` based on structural shape, but `f64::clamp` \
                  is not const-stable yet (tracked at rust-lang/rust#93396). Cannot be made \
                  `const` until that stabilizes."
    )]
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

    /// Format this longitude as the standard APRS uncompressed 9-byte
    /// field `DDDMM.HHE` (or `…W` for western hemisphere).
    #[must_use]
    pub fn as_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'E' } else { 'W' };
        let lon_abs = self.0.abs();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "`lon_abs` is in [0.0, 180.0] by the Longitude invariant (validated at \
                      construction via `Longitude::new` / `Longitude::new_clamped`), so the \
                      `as u32` cast is always lossless. `cast_possible_truncation` fires \
                      because clippy can't prove the f64 range from the surrounding code; \
                      `cast_sign_loss` fires because f64→u32 drops the sign bit even though \
                      `.abs()` two lines above guarantees non-negative input."
        )]
        let degrees = lon_abs as u32;
        let minutes = (lon_abs - f64::from(degrees)) * 60.0;
        format!("{degrees:03}{minutes:05.2}{hemisphere}")
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
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "APRS speeds are physical quantities the caller is responsible for keeping \
                  sane — u16 covers 0..65535 knots which exceeds every terrestrial APRS use \
                  case (satellites up to ~14,000 knots, aircraft to ~2000 knots). \
                  `cast_possible_truncation` fires on `.round() as u16` because clippy can't \
                  prove the f64 is bounded; `cast_sign_loss` fires because the `Kmh` and \
                  `Mph` variants internally store non-negative floats but the types don't \
                  enforce it. A fix-the-code version of this method would use \
                  `.round().clamp(0.0, f64::from(u16::MAX)) as u16` to make the saturation \
                  explicit — left as `#[expect]` pending that refactor."
    )]
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
    /// Returns [`AprsError::InvalidTocall`] on invalid input.
    pub fn new(s: &str) -> Result<Self, AprsError> {
        // Reuse Callsign's validation rules — tocalls are structurally
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
    fn latitude_clamped() {
        assert!((Latitude::new_clamped(200.0).as_degrees() - 90.0).abs() < f64::EPSILON);
        assert!((Latitude::new_clamped(-200.0).as_degrees() - (-90.0)).abs() < f64::EPSILON);
        assert!((Latitude::new_clamped(f64::NAN).as_degrees() - 0.0).abs() < f64::EPSILON);
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
}
