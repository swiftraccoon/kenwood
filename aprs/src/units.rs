//! Strongly-typed primitives for APRS wire-format data.
//!
//! These newtypes are used by the APRS parsers and builders. Every type
//! validates at construction and rejects out-of-range values, making
//! illegal APRS packets unrepresentable.

use core::{fmt, num::NonZeroU16};

use ax25_codec::Callsign;
use thiserror::Error;

use crate::error::AprsError;

/// Why a decimal geographic coordinate was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CoordinateError {
    /// The coordinate was infinite or NaN.
    #[error("coordinate must be finite")]
    NonFinite,
    /// The coordinate exceeded the latitude or longitude domain.
    #[error("coordinate exceeds its geographic degree range")]
    OutOfRange,
}

/// Why a physical or Mic-E wire speed was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SpeedError {
    /// The supplied measurement was infinite or NaN.
    #[error("speed must be finite")]
    NonFinite,
    /// The supplied measurement was negative.
    #[error("speed must be non-negative")]
    Negative,
    /// Converting a finite measurement to kilometers per hour overflowed.
    #[error("speed conversion exceeds the finite kilometer-per-hour range")]
    ConversionOverflow,
    /// A whole-knot value exceeded Mic-E's three-digit representation.
    #[error("Mic-E speed must be 0..=799 knots (got {knots})")]
    MiceOutOfRange {
        /// Rejected whole-knot measurement.
        knots: u16,
    },
}

/// Why a decimal heading or integer APRS course was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CourseError {
    /// The decimal heading was infinite or NaN.
    #[error("heading must be finite")]
    NonFinite,
    /// The heading or course exceeded `0..=360` degrees.
    #[error("heading or course must be 0..=360 degrees")]
    OutOfRange,
}

/// Why an APRS message identifier was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MessageIdError {
    /// The identifier was empty or exceeded five bytes.
    #[error("message identifier must contain 1..=5 bytes (got {bytes})")]
    Length {
        /// Rejected byte count.
        bytes: usize,
    },
    /// An identifier byte was not an ASCII letter or digit.
    #[error("message identifier byte {byte:#04X} at index {index} is not ASCII alphanumeric")]
    InvalidByte {
        /// Zero-based byte index.
        index: usize,
        /// Rejected byte.
        byte: u8,
    },
}

/// Why an APRS weather temperature was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TemperatureError {
    /// The Fahrenheit measurement exceeded the signed three-byte field.
    #[error("temperature must be -99..=999 degrees Fahrenheit (got {value})")]
    OutOfRange {
        /// Rejected Fahrenheit measurement.
        value: i16,
    },
}

/// Why a standard or compressed APRS symbol-table selector was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SymbolTableError {
    /// The byte was not `/`, `\\`, a digit, or an uppercase ASCII letter.
    #[error("invalid symbol-table selector {byte:#04X}")]
    InvalidSelector {
        /// Rejected byte.
        byte: u8,
    },
    /// An overlay was not a digit or uppercase ASCII letter.
    #[error("invalid symbol overlay {byte:#04X}")]
    InvalidOverlay {
        /// Rejected byte.
        byte: u8,
    },
    /// A compressed numeric overlay used its uncompressed digit.
    #[error("compressed numeric overlay {byte:#04X} must encode as a..=j")]
    CompressedNumericOverlay {
        /// Rejected digit byte.
        byte: u8,
    },
    /// A selector character could not fit in one ASCII wire byte.
    #[error("symbol-table selector {character:?} is not ASCII")]
    NonAscii {
        /// Rejected character.
        character: char,
    },
}

/// Why an APRS symbol code was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SymbolError {
    /// The byte was outside printable non-space ASCII.
    #[error("symbol code must be 0x21..=0x7E (got {byte:#04X})")]
    InvalidCode {
        /// Rejected byte.
        byte: u8,
    },
    /// A character could not fit in one ASCII wire byte.
    #[error("symbol code {character:?} is not ASCII")]
    NonAscii {
        /// Rejected character.
        character: char,
    },
}

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
#[derive(Debug, Clone, Copy, PartialEq)]
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
        if !degrees.is_finite() {
            return Err(AprsError::InvalidLatitude(CoordinateError::NonFinite));
        }
        if !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(AprsError::InvalidLatitude(CoordinateError::OutOfRange));
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
    pub fn to_aprs_uncompressed(self) -> String {
        let hemisphere = if self.0 >= 0.0 { 'N' } else { 'S' };
        let core = format_ddmm_hundredths(self.0.abs(), 2);
        format!("{core}{hemisphere}")
    }
}

/// Geographic longitude in decimal degrees, validated to `[-180.0, 180.0]`.
///
/// Positive = East, negative = West. Rejects NaN and out-of-range values.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        if !degrees.is_finite() {
            return Err(AprsError::InvalidLongitude(CoordinateError::NonFinite));
        }
        if !(Self::MIN..=Self::MAX).contains(&degrees) {
            return Err(AprsError::InvalidLongitude(CoordinateError::OutOfRange));
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
    pub fn to_aprs_uncompressed(self) -> String {
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
/// [`fmt::Display`] writes the canonical kilometer-per-hour value.
#[derive(Debug, Clone, Copy, PartialEq)]
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
        Self::validate_measurement(kmh)?;
        Ok(Self(kmh))
    }

    /// Create a speed measured in knots.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] unless `knots` and its converted
    /// value are finite and non-negative.
    pub fn from_knots(knots: f64) -> Result<Self, AprsError> {
        Self::validate_measurement(knots)?;
        Self::from_converted_kmh(knots * Self::KNOTS_TO_KMH)
    }

    /// Create a speed measured in statute miles per hour.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSpeed`] unless `mph` and its converted
    /// value are finite and non-negative.
    pub fn from_mph(mph: f64) -> Result<Self, AprsError> {
        Self::validate_measurement(mph)?;
        Self::from_converted_kmh(mph * Self::MPH_TO_KMH)
    }

    const fn from_converted_kmh(kmh: f64) -> Result<Self, AprsError> {
        if !kmh.is_finite() {
            return Err(AprsError::InvalidSpeed(SpeedError::ConversionOverflow));
        }
        Ok(Self(kmh))
    }

    fn validate_measurement(value: f64) -> Result<(), AprsError> {
        if !value.is_finite() {
            return Err(AprsError::InvalidSpeed(SpeedError::NonFinite));
        }
        if value < 0.0 {
            return Err(AprsError::InvalidSpeed(SpeedError::Negative));
        }
        Ok(())
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
            return Err(AprsError::InvalidSpeed(SpeedError::MiceOutOfRange {
                knots,
            }));
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
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Heading(f64);

impl Heading {
    /// Create a true heading.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidCourse`] unless `degrees` is finite and
    /// inside `0..=360`.
    pub fn new(degrees: f64) -> Result<Self, AprsError> {
        if !degrees.is_finite() {
            return Err(AprsError::InvalidCourse(CourseError::NonFinite));
        }
        if !(0.0..=360.0).contains(&degrees) {
            return Err(AprsError::InvalidCourse(CourseError::OutOfRange));
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
            Err(AprsError::InvalidCourse(CourseError::OutOfRange))
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// [`TryFrom<String>`] transfers an owned allocation after validation.
    pub fn new(value: &str) -> Result<Self, AprsError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    fn validate(text: &str) -> Result<(), AprsError> {
        if text.is_empty() || text.len() > Self::MAX_LEN {
            return Err(AprsError::InvalidMessageId(MessageIdError::Length {
                bytes: text.len(),
            }));
        }
        if let Some((index, byte)) = text
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_alphanumeric())
        {
            return Err(AprsError::InvalidMessageId(MessageIdError::InvalidByte {
                index,
                byte,
            }));
        }
        Ok(())
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
        fmt::Display::fmt(&self.0, f)
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
    Overlay(SymbolOverlay),
}

/// A numeric or uppercase ASCII overlay on the APRS alternate symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolOverlay(u8);

impl SymbolOverlay {
    /// Validate an overlay byte from `0` through `9` or `A` through `Z`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSymbolTable`] for any other byte.
    pub const fn new(byte: u8) -> Result<Self, AprsError> {
        match byte {
            b'0'..=b'9' | b'A'..=b'Z' => Ok(Self(byte)),
            _ => Err(AprsError::InvalidSymbolTable(
                SymbolTableError::InvalidOverlay { byte },
            )),
        }
    }

    /// Standard overlay byte, independent of the position encoding format.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for SymbolOverlay {
    type Error = AprsError;
    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::new(byte)
    }
}

impl From<SymbolOverlay> for u8 {
    fn from(overlay: SymbolOverlay) -> Self {
        overlay.as_byte()
    }
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
            b'0'..=b'9' | b'A'..=b'Z' => Ok(Self::Overlay(SymbolOverlay(b))),
            _ => Err(AprsError::InvalidSymbolTable(
                SymbolTableError::InvalidSelector { byte: b },
            )),
        }
    }

    /// Convert back to the wire byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Primary => b'/',
            Self::Alternate => b'\\',
            Self::Overlay(overlay) => overlay.as_byte(),
        }
    }

    /// Table selector for a compressed position; numeric overlays encode as `a`-`j`.
    #[must_use]
    pub const fn compressed_byte(self) -> u8 {
        match self {
            Self::Overlay(SymbolOverlay(byte @ b'0'..=b'9')) => b'a' + byte - b'0',
            _ => self.as_byte(),
        }
    }

    /// Parse a compressed table selector, normalizing `a`-`j` to numeric overlays.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidSymbolTable`] for an invalid compressed selector.
    pub const fn from_compressed_byte(byte: u8) -> Result<Self, AprsError> {
        match byte {
            b'a'..=b'j' => Ok(Self::Overlay(SymbolOverlay(b'0' + byte - b'a'))),
            b'0'..=b'9' => Err(AprsError::InvalidSymbolTable(
                SymbolTableError::CompressedNumericOverlay { byte },
            )),
            _ => Self::from_byte(byte),
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
            return Err(AprsError::InvalidSymbol(SymbolError::InvalidCode {
                byte: code,
            }));
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
            return Err(AprsError::InvalidSymbolTable(SymbolTableError::NonAscii {
                character: table,
            }));
        }
        if !code.is_ascii() {
            return Err(AprsError::InvalidSymbol(SymbolError::NonAscii {
                character: code,
            }));
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
            return Err(AprsError::InvalidTemperature(
                TemperatureError::OutOfRange { value: f },
            ));
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
pub struct Tocall(Callsign);

impl Tocall {
    /// Maximum length of a tocall.
    pub const MAX_LEN: usize = 6;

    /// The tocall assigned to the Kenwood TH-D75 in the APRS tocall registry.
    pub const TH_D75: &'static str = "APK005";

    /// Create a tocall from a string, enforcing the same rules as
    /// [`Callsign::new`] (1-6 uppercase ASCII alphanumerics).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTocall`] on invalid input.
    pub fn new(value: &str) -> Result<Self, AprsError> {
        Callsign::new(value)
            .map(Self)
            .map_err(AprsError::InvalidTocall)
    }

    /// Construct the registered TH-D75 software destination `APK005`.
    #[must_use]
    pub fn th_d75() -> Self {
        Self::new(Self::TH_D75)
            .unwrap_or_else(|_| unreachable!("the registered TH-D75 tocall is valid"))
    }

    /// Return the tocall as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Tocall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

macro_rules! finite_value_traits {
    ($type:ty) => {
        impl Eq for $type {}
        impl Ord for $type {
            fn cmp(&self, other: &Self) -> core::cmp::Ordering {
                if self.0 == 0.0 && other.0 == 0.0 {
                    core::cmp::Ordering::Equal
                } else {
                    self.0.total_cmp(&other.0)
                }
            }
        }
        impl PartialOrd for $type {
            fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl core::hash::Hash for $type {
            fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
                let canonical = if self.0 == 0.0 { 0.0 } else { self.0 };
                core::hash::Hash::hash(&canonical.to_bits(), state);
            }
        }
        impl fmt::LowerExp for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::LowerExp::fmt(&self.0, formatter)
            }
        }
        impl fmt::UpperExp for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::UpperExp::fmt(&self.0, formatter)
            }
        }
    };
}

finite_value_traits!(Latitude);
finite_value_traits!(Longitude);
finite_value_traits!(Speed);
finite_value_traits!(Heading);

impl fmt::Display for Speed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

macro_rules! integer_traits {
    ($type:ty, $integer:ty, $accessor:ident) => {
        impl TryFrom<$integer> for $type {
            type Error = AprsError;
            fn try_from(value: $integer) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
        impl From<$type> for $integer {
            fn from(value: $type) -> Self {
                value.$accessor()
            }
        }
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
        impl fmt::Binary for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Binary::fmt(&self.0, formatter)
            }
        }
        impl fmt::Octal for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Octal::fmt(&self.0, formatter)
            }
        }
        impl fmt::LowerHex for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::LowerHex::fmt(&self.0, formatter)
            }
        }
        impl fmt::UpperHex for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::UpperHex::fmt(&self.0, formatter)
            }
        }
    };
}

integer_traits!(MiceSpeed, u16, as_knots);
integer_traits!(Course, u16, as_degrees);
integer_traits!(Fahrenheit, i16, get);

macro_rules! degree_traits {
    ($type:ty) => {
        impl TryFrom<f64> for $type {
            type Error = AprsError;
            fn try_from(degrees: f64) -> Result<Self, Self::Error> {
                Self::new(degrees)
            }
        }
        impl From<$type> for f64 {
            fn from(value: $type) -> Self {
                value.as_degrees()
            }
        }
        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, formatter)
            }
        }
    };
}

degree_traits!(Latitude);
degree_traits!(Longitude);
degree_traits!(Heading);

macro_rules! string_traits {
    ($type:ty) => {
        impl AsRef<str> for $type {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl core::str::FromStr for $type {
            type Err = AprsError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
        impl TryFrom<&str> for $type {
            type Error = AprsError;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

string_traits!(MessageId);
string_traits!(Tocall);

impl TryFrom<String> for MessageId {
    type Error = AprsError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl TryFrom<String> for Tocall {
    type Error = AprsError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Callsign::try_from(value)
            .map(Self)
            .map_err(AprsError::InvalidTocall)
    }
}

impl From<MessageId> for String {
    fn from(value: MessageId) -> Self {
        value.0
    }
}

impl From<Tocall> for String {
    fn from(value: Tocall) -> Self {
        value.0.into()
    }
}

impl From<Tocall> for Callsign {
    fn from(value: Tocall) -> Self {
        value.0
    }
}

impl From<Callsign> for Tocall {
    fn from(value: Callsign) -> Self {
        Self(value)
    }
}

impl TryFrom<u8> for SymbolTable {
    type Error = AprsError;
    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::from_byte(byte)
    }
}

impl From<SymbolTable> for u8 {
    fn from(table: SymbolTable) -> Self {
        table.as_byte()
    }
}

impl TryFrom<char> for SymbolOverlay {
    type Error = AprsError;
    fn try_from(character: char) -> Result<Self, Self::Error> {
        if !character.is_ascii() {
            return Err(AprsError::InvalidSymbolTable(SymbolTableError::NonAscii {
                character,
            }));
        }
        Self::new(character as u8)
    }
}

impl From<SymbolOverlay> for char {
    fn from(overlay: SymbolOverlay) -> Self {
        Self::from(overlay.as_byte())
    }
}

impl fmt::Display for SymbolOverlay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&char::from(*self), formatter)
    }
}

impl TryFrom<[u8; 2]> for AprsSymbol {
    type Error = AprsError;
    fn try_from(bytes: [u8; 2]) -> Result<Self, Self::Error> {
        Self::new(SymbolTable::try_from(bytes[0])?, bytes[1])
    }
}

impl From<AprsSymbol> for [u8; 2] {
    fn from(symbol: AprsSymbol) -> Self {
        [symbol.table_byte(), symbol.code()]
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
    fn owned_identifiers_transfer_the_original_allocation() -> TestResult {
        let input = String::from("A1234");
        let allocation = input.as_ptr();
        let identifier = MessageId::try_from(input)?;
        assert_eq!(identifier.as_ref().as_ptr(), allocation);
        let output = String::from(identifier);
        assert_eq!(output.as_ptr(), allocation);
        assert_eq!(output, "A1234");

        let input = String::from("APK005");
        let allocation = input.as_ptr();
        let tocall = Tocall::try_from(input)?;
        assert_eq!(tocall.as_ref().as_ptr(), allocation);
        let output = String::from(tocall);
        assert_eq!(output.as_ptr(), allocation);
        assert_eq!(output, Tocall::TH_D75);
        Ok(())
    }

    #[test]
    fn rejected_scalar_values_preserve_typed_reasons() {
        assert_eq!(
            Latitude::new(f64::NAN),
            Err(AprsError::InvalidLatitude(CoordinateError::NonFinite))
        );
        assert_eq!(
            Latitude::new(90.000_001),
            Err(AprsError::InvalidLatitude(CoordinateError::OutOfRange))
        );
        assert_eq!(
            Longitude::new(f64::INFINITY),
            Err(AprsError::InvalidLongitude(CoordinateError::NonFinite))
        );
        assert_eq!(
            Longitude::new(-180.000_001),
            Err(AprsError::InvalidLongitude(CoordinateError::OutOfRange))
        );
        assert_eq!(
            Speed::from_kmh(-1.0),
            Err(AprsError::InvalidSpeed(SpeedError::Negative))
        );
        assert_eq!(
            Speed::from_knots(f64::NAN),
            Err(AprsError::InvalidSpeed(SpeedError::NonFinite))
        );
        assert_eq!(
            Speed::from_mph(f64::MAX),
            Err(AprsError::InvalidSpeed(SpeedError::ConversionOverflow))
        );
        assert_eq!(
            MiceSpeed::try_from(800),
            Err(AprsError::InvalidSpeed(SpeedError::MiceOutOfRange {
                knots: 800
            }))
        );
        assert_eq!(
            Heading::try_from(f64::NAN),
            Err(AprsError::InvalidCourse(CourseError::NonFinite))
        );
        assert_eq!(
            Course::try_from(361),
            Err(AprsError::InvalidCourse(CourseError::OutOfRange))
        );
        assert_eq!(
            Fahrenheit::try_from(-100),
            Err(AprsError::InvalidTemperature(
                TemperatureError::OutOfRange { value: -100 }
            ))
        );
    }

    #[test]
    fn rejected_text_and_symbol_values_preserve_typed_reasons() {
        assert_eq!(
            MessageId::new("ABCDEF"),
            Err(AprsError::InvalidMessageId(MessageIdError::Length {
                bytes: 6
            }))
        );
        assert_eq!(
            MessageId::new("AB-1"),
            Err(AprsError::InvalidMessageId(MessageIdError::InvalidByte {
                index: 2,
                byte: b'-'
            }))
        );
        for (text, reason) in [
            ("", MessageIdError::Length { bytes: 0 }),
            ("ABCDEF", MessageIdError::Length { bytes: 6 }),
            (
                "AB-1",
                MessageIdError::InvalidByte {
                    index: 2,
                    byte: b'-',
                },
            ),
        ] {
            assert_eq!(
                MessageId::try_from(text.to_owned()),
                Err(AprsError::InvalidMessageId(reason))
            );
        }
        for text in ["", "TOOLONG", "aptest", "AP_1"] {
            assert!(matches!(
                Tocall::try_from(text.to_owned()),
                Err(AprsError::InvalidTocall(_))
            ));
        }
        assert_eq!(
            SymbolOverlay::new(b'a'),
            Err(AprsError::InvalidSymbolTable(
                SymbolTableError::InvalidOverlay { byte: b'a' }
            ))
        );
        assert_eq!(
            SymbolTable::from_compressed_byte(b'5'),
            Err(AprsError::InvalidSymbolTable(
                SymbolTableError::CompressedNumericOverlay { byte: b'5' }
            ))
        );
        assert_eq!(
            AprsSymbol::from_chars('/', 'é'),
            Err(AprsError::InvalidSymbol(SymbolError::NonAscii {
                character: 'é'
            }))
        );
    }

    #[test]
    fn finite_values_preserve_equality_order_and_hash_for_signed_zero() -> TestResult {
        use std::collections::{BTreeSet, HashSet};

        let north = Latitude::try_from(0.0)?;
        let south = Latitude::try_from(-0.0)?;
        assert_eq!(north, south);
        assert_eq!(north.cmp(&south), core::cmp::Ordering::Equal);
        assert_eq!(BTreeSet::from([north, south]).len(), 1);
        assert_eq!(HashSet::from([north, south]).len(), 1);
        assert_eq!(
            HashSet::from([Longitude::try_from(0.0)?, Longitude::try_from(-0.0)?]).len(),
            1
        );
        assert_eq!(
            HashSet::from([Speed::from_kmh(0.0)?, Speed::from_kmh(-0.0)?]).len(),
            1
        );
        assert_eq!(
            HashSet::from([Heading::try_from(0.0)?, Heading::try_from(-0.0)?]).len(),
            1
        );
        assert!(Latitude::try_from(-90.0)? < north);
        assert!(Longitude::try_from(180.0)? > Longitude::PRIME_MERIDIAN);
        Ok(())
    }

    #[test]
    fn numeric_and_symbol_conversions_preserve_units_and_format_flags() -> TestResult {
        let speed = MiceSpeed::try_from(799)?;
        assert_eq!(u16::from(speed), 799);
        assert_eq!(format!("{speed:#08X}"), format!("{:#08X}", 799_u16));
        let course = Course::try_from(360)?;
        assert_eq!(format!("{course:#014b}"), format!("{:#014b}", 360_u16));
        let temperature = Fahrenheit::try_from(-99)?;
        assert_eq!(format!("{temperature:#08x}"), format!("{:#08x}", -99_i16));
        assert_eq!(format!("{temperature:+06}"), format!("{:+06}", -99_i16));
        assert_eq!(
            f64::from(Latitude::try_from(12.5)?).to_bits(),
            12.5_f64.to_bits()
        );
        let latitude = Latitude::new(12.5)?;
        let longitude = Longitude::new(-45.25)?;
        let speed = Speed::from_kmh(25.5)?;
        let heading = Heading::new(359.5)?;
        assert_eq!(
            format!("{latitude:+012.3e}"),
            format!("{:+012.3e}", 12.5_f64)
        );
        assert_eq!(
            format!("{longitude:>16.2E}"),
            format!("{:>16.2E}", -45.25_f64)
        );
        assert_eq!(format!("{speed:+012.3e}"), format!("{:+012.3e}", 25.5_f64));
        assert_eq!(format!("{heading:>16.2E}"), format!("{:>16.2E}", 359.5_f64));
        let symbol = AprsSymbol::try_from(*b"5>")?;
        assert_eq!(<[u8; 2]>::from(symbol), *b"5>");
        assert_eq!(u8::from(SymbolTable::try_from(b'/')?), b'/');
        assert_eq!(char::from(SymbolOverlay::try_from('A')?), 'A');
        Ok(())
    }

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
        assert_eq!(
            SymbolTable::from_byte(b'9')?,
            SymbolTable::Overlay(SymbolOverlay::new(b'9')?)
        );
        assert_eq!(
            SymbolTable::from_byte(b'Z')?,
            SymbolTable::Overlay(SymbolOverlay::new(b'Z')?)
        );
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
        let s = lat.to_aprs_uncompressed();
        assert_eq!(s.len(), 8);
        assert!(s.ends_with('N'));
        assert!(s.starts_with("49"));
        Ok(())
    }

    #[test]
    fn latitude_aprs_format_south() -> TestResult {
        let lat = Latitude::new(-33.856)?;
        let s = lat.to_aprs_uncompressed();
        assert!(s.ends_with('S'));
        Ok(())
    }

    #[test]
    fn longitude_aprs_format_west() -> TestResult {
        let lon = Longitude::new(-72.029_166)?;
        let s = lon.to_aprs_uncompressed();
        assert_eq!(s.len(), 9);
        assert!(s.ends_with('W'));
        assert!(s.starts_with("072"));
        Ok(())
    }

    #[test]
    fn longitude_aprs_format_east() -> TestResult {
        let lon = Longitude::new(151.209)?;
        let s = lon.to_aprs_uncompressed();
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
        // 33.999999° rounds its minutes to 60.00; the helper must carry
        // to 34° 00.00' instead of printing "3360.00".
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
        // 97.999983° must carry to 98° 00.00', never print "09760.00".
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
        let s = lat.to_aprs_uncompressed();
        assert_eq!(s, "3400.00N", "expected carry to 3400.00N, got {s}");
        assert_eq!(s.len(), 8, "latitude field is 8 bytes");
        Ok(())
    }

    #[test]
    fn longitude_newtype_carry_boundary_no_60_minutes() -> TestResult {
        // The Longitude newtype method (western hemisphere) must carry.
        let lon = Longitude::new(-97.999_983)?;
        let s = lon.to_aprs_uncompressed();
        assert_eq!(s, "09800.00W", "expected carry to 09800.00W, got {s}");
        assert_eq!(s.len(), 9, "longitude field is 9 bytes");
        Ok(())
    }
}
