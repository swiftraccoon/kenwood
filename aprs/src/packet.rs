//! APRS packet wrapper, data enum, data extensions, and PHG field.
//!
//! This module contains the top-level [`AprsData`] enum (one variant per
//! APRS data type identifier), the [`AprsData::from_bytes`] dispatcher, and a
//! handful of shared primitive types: [`AprsDataExtension`], [`Phg`],
//! [`PhgDirectivity`], [`PositionAmbiguity`],
//! [`AprsReportTimestamp`] and [`TelemetryDefinition`].
//!
//! Shared workflows cover [packet parsing](crate#packet-construction-and-parsing)
//! and [telemetry](crate#weather-and-telemetry).

use core::fmt;

use crate::error::{AprsError, AprsWireTextField};
use crate::item::{AprsItem, AprsObject, AprsQuery};
use crate::message::{AnnouncementLetter, AprsMessage, BulletinGroup, BulletinNumber};
use crate::position::AprsPosition;
use crate::status::AprsStatus;
use crate::telemetry::{
    AprsTelemetry, TelemetryBits, TelemetryEquationCoefficients, TelemetryLabels,
    TelemetryProjectTitle,
};
use crate::text::decode_wire_ascii;
use crate::units::{Course, Speed};
use crate::weather::AprsPositionlessWeatherReport;

// ---------------------------------------------------------------------------
// PositionAmbiguity
// ---------------------------------------------------------------------------

/// APRS position ambiguity level (APRS 1.0.1 §8.1.6).
///
/// Stations can deliberately reduce their reported precision by
/// replacing trailing latitude/longitude digits with spaces. Each level
/// masks one more trailing digit:
///
/// | Level | Example               | Effective precision |
/// |-------|-----------------------|---------------------|
/// | 0     | `4903.50N`            | 0.01 minute         |
/// | 1     | `4903.5 N`            | 0.1 minute          |
/// | 2     | `4903.  N`            | 1 minute            |
/// | 3     | `490 .  N`            | 10 minutes          |
/// | 4     | `49  .  N`            | 1 degree            |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PositionAmbiguity {
    /// No ambiguity: full DDMM.HH precision.
    None,
    /// Last digit of hundredths-of-a-minute masked (0.1' precision).
    OneDigit,
    /// Whole hundredths-of-a-minute masked (1' precision).
    TwoDigits,
    /// Tens of minutes masked (10' precision).
    ThreeDigits,
    /// Whole minutes masked (1° precision).
    FourDigits,
}

impl PositionAmbiguity {
    /// Return how many trailing minute digits the report omits.
    #[must_use]
    pub const fn masked_digits(self) -> u8 {
        match self {
            Self::None => 0,
            Self::OneDigit => 1,
            Self::TwoDigits => 2,
            Self::ThreeDigits => 3,
            Self::FourDigits => 4,
        }
    }
    pub(crate) const fn mask_minute_hundredths(self, value: u16) -> u16 {
        match self {
            Self::None => value,
            Self::OneDigit => value / 10 * 10,
            Self::TwoDigits => value / 100 * 100,
            Self::ThreeDigits => value / 1000 * 1000,
            Self::FourDigits => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// AprsReportTimestamp
// ---------------------------------------------------------------------------

/// Calendar component or wire-shape failure in an APRS timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampError {
    /// A report timestamp is not exactly seven bytes.
    ReportLength,
    /// A positionless-weather timestamp is not exactly eight bytes.
    WeatherLength,
    /// A report timestamp lacks the z, slash or h suffix.
    UnknownSuffix,
    /// Status reports require the day/hour/minute UTC form.
    StatusFormat,
    /// A two-digit component is truncated.
    Truncated,
    /// A component contains a nondecimal byte.
    NonDecimal,
    /// Month is outside 1 through 12.
    Month,
    /// Day is outside 1 through 31.
    Day,
    /// Day does not occur in the supplied month, allowing February 29.
    DayForMonth,
    /// Hour is greater than 23.
    Hour,
    /// Minute is greater than 59.
    Minute,
    /// Second is greater than 59.
    Second,
}

impl fmt::Display for TimestampError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReportLength => "report timestamp must be exactly 7 bytes",
            Self::WeatherLength => "weather timestamp must be exactly 8 bytes",
            Self::UnknownSuffix => "7-byte timestamp must end in 'z', '/', or 'h'",
            Self::StatusFormat => "status timestamp must use DDHHMMz day/hour/minute UTC format",
            Self::Truncated => "timestamp is too short",
            Self::NonDecimal => "timestamp components must be decimal digits",
            Self::Month => "month must be 1-12",
            Self::Day => "day must be 1-31",
            Self::DayForMonth => "day is not valid for month",
            Self::Hour => "hour must be 0-23",
            Self::Minute => "minute must be 0-59",
            Self::Second => "second must be 0-59",
        })
    }
}
impl std::error::Error for TimestampError {}

/// A validated seven-byte APRS timestamp used by position and object reports
/// (APRS 1.0.1 §6.1).
///
/// Three formats are defined on the wire:
///
/// | Suffix | Meaning | Digits |
/// |--------|---------|--------|
/// | `z`    | Day / hour / minute, zulu | DDHHMM |
/// | `/`    | Day / hour / minute, local| DDHHMM |
/// | `h`    | Hour / minute / second, zulu | HHMMSS |
///
/// Its fields are private so an out-of-range calendar component cannot
/// reach an encoder. Use one of the named constructors or [`Self::from_wire`].
/// Status reports accept only the DHM-UTC subset, represented separately by
/// [`crate::AprsStatusTimestamp`].
/// The distinct eight-byte `MMDDHHMM` timestamp used only by positionless
/// weather reports is represented by [`AprsWeatherTimestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsReportTimestamp(AprsReportTimestampValue);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AprsReportTimestampValue {
    DayHourMinuteUtc { day: u8, hour: u8, minute: u8 },
    DayHourMinuteLocal { day: u8, hour: u8, minute: u8 },
    HourMinuteSecondUtc { hour: u8, minute: u8, second: u8 },
}

/// The wire format carried by an [`AprsReportTimestamp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsReportTimestampFormat {
    /// Day, hour, and minute in UTC (`DDHHMMz`).
    DayHourMinuteUtc,
    /// Day, hour, and minute in local time (`DDHHMM/`).
    DayHourMinuteLocal,
    /// Hour, minute, and second in UTC (`HHMMSSh`).
    HourMinuteSecondUtc,
}

impl AprsReportTimestamp {
    /// Create a day/hour/minute UTC timestamp (`DDHHMMz`).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] unless `day` is 1-31,
    /// `hour` is 0-23, and `minute` is 0-59.
    pub fn day_hour_minute_utc(day: u8, hour: u8, minute: u8) -> Result<Self, AprsError> {
        validate_day_hour_minute(day, hour, minute)?;
        Ok(Self(AprsReportTimestampValue::DayHourMinuteUtc {
            day,
            hour,
            minute,
        }))
    }

    /// Create a day/hour/minute local-time timestamp (`DDHHMM/`).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] unless `day` is 1-31,
    /// `hour` is 0-23, and `minute` is 0-59.
    pub fn day_hour_minute_local(day: u8, hour: u8, minute: u8) -> Result<Self, AprsError> {
        validate_day_hour_minute(day, hour, minute)?;
        Ok(Self(AprsReportTimestampValue::DayHourMinuteLocal {
            day,
            hour,
            minute,
        }))
    }

    /// Create an hour/minute/second UTC timestamp (`HHMMSSh`).
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] unless `hour` is 0-23 and
    /// `minute` and `second` are 0-59.
    pub const fn hour_minute_second_utc(
        hour: u8,
        minute: u8,
        second: u8,
    ) -> Result<Self, AprsError> {
        if hour > 23 {
            return Err(AprsError::InvalidTimestamp(TimestampError::Hour));
        }
        if minute > 59 {
            return Err(AprsError::InvalidTimestamp(TimestampError::Minute));
        }
        if second > 59 {
            return Err(AprsError::InvalidTimestamp(TimestampError::Second));
        }
        Ok(Self(AprsReportTimestampValue::HourMinuteSecondUtc {
            hour,
            minute,
            second,
        }))
    }

    /// Return the timestamp's APRS wire format.
    #[must_use]
    pub const fn format(self) -> AprsReportTimestampFormat {
        match self.0 {
            AprsReportTimestampValue::DayHourMinuteUtc { .. } => {
                AprsReportTimestampFormat::DayHourMinuteUtc
            }
            AprsReportTimestampValue::DayHourMinuteLocal { .. } => {
                AprsReportTimestampFormat::DayHourMinuteLocal
            }
            AprsReportTimestampValue::HourMinuteSecondUtc { .. } => {
                AprsReportTimestampFormat::HourMinuteSecondUtc
            }
        }
    }

    /// Format this timestamp as its exact seven-byte APRS wire representation.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        self.to_string()
    }

    /// Parse an APRS timestamp from its wire-format string.
    ///
    /// Accepts the three spec-defined seven-byte report forms (APRS 1.0.1
    /// §6 pp.22-23):
    ///
    /// | Length | Suffix | Variant                  | Spec form  |
    /// |-------:|--------|--------------------------|------------|
    /// | 7      | `z`    | day/hour/minute UTC      | `DDHHMMz`  |
    /// | 7      | `/`    | day/hour/minute local    | `DDHHMM/`  |
    /// | 7      | `h`    | hour/minute/second UTC   | `HHMMSSh`  |
    ///
    /// All non-suffix bytes must be ASCII digits. Range checks per
    /// spec: day 1..=31, hour 0..=23, minute 0..=59, and second 0..=59.
    /// Zero day and out-of-range clock fields are rejected.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] for a malformed shape,
    /// non-decimal component, unknown suffix, or out-of-range component.
    pub fn from_wire(wire: &str) -> Result<Self, AprsError> {
        match wire.len() {
            7 => {
                let p0 = parse_timestamp_pair(wire, 0)?;
                let p1 = parse_timestamp_pair(wire, 2)?;
                let p2 = parse_timestamp_pair(wire, 4)?;
                match wire.as_bytes().get(6).copied() {
                    Some(b'z') => Self::day_hour_minute_utc(p0, p1, p2),
                    Some(b'/') => Self::day_hour_minute_local(p0, p1, p2),
                    Some(b'h') => Self::hour_minute_second_utc(p0, p1, p2),
                    _ => Err(AprsError::InvalidTimestamp(TimestampError::UnknownSuffix)),
                }
            }
            _ => Err(AprsError::InvalidTimestamp(TimestampError::ReportLength)),
        }
    }
}

impl fmt::Display for AprsReportTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            AprsReportTimestampValue::DayHourMinuteUtc { day, hour, minute } => {
                write!(formatter, "{day:02}{hour:02}{minute:02}z")
            }
            AprsReportTimestampValue::DayHourMinuteLocal { day, hour, minute } => {
                write!(formatter, "{day:02}{hour:02}{minute:02}/")
            }
            AprsReportTimestampValue::HourMinuteSecondUtc {
                hour,
                minute,
                second,
            } => write!(formatter, "{hour:02}{minute:02}{second:02}h"),
        }
    }
}

impl core::str::FromStr for AprsReportTimestamp {
    type Err = AprsError;

    fn from_str(wire: &str) -> Result<Self, Self::Err> {
        Self::from_wire(wire)
    }
}

/// A validated positionless-weather timestamp (`MMDDHHMM`).
///
/// APRS reserves this unsuffixed eight-byte UTC form for positionless
/// weather reports. Keeping it separate from [`AprsReportTimestamp`] prevents
/// builders from emitting it in position, object, or status frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsWeatherTimestamp {
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
}

impl AprsWeatherTimestamp {
    /// Create a month/day/hour/minute UTC weather timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] unless `month` is 1-12,
    /// `day` exists in that month (allowing February 29 because the wire
    /// format has no year), `hour` is 0-23, and `minute` is 0-59.
    pub fn month_day_hour_minute_utc(
        month: u8,
        day: u8,
        hour: u8,
        minute: u8,
    ) -> Result<Self, AprsError> {
        if month == 0 || month > 12 {
            return Err(AprsError::InvalidTimestamp(TimestampError::Month));
        }
        validate_day_hour_minute(day, hour, minute)?;
        let maximum_day = match month {
            2 => 29,
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if day > maximum_day {
            return Err(AprsError::InvalidTimestamp(TimestampError::DayForMonth));
        }
        Ok(Self {
            month,
            day,
            hour,
            minute,
        })
    }

    /// Parse an exact eight-byte `MMDDHHMM` timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidTimestamp`] for the wrong length,
    /// non-decimal bytes, or out-of-range components.
    pub fn from_wire(wire: &str) -> Result<Self, AprsError> {
        if wire.len() != 8 {
            return Err(AprsError::InvalidTimestamp(TimestampError::WeatherLength));
        }
        Self::month_day_hour_minute_utc(
            parse_timestamp_pair(wire, 0)?,
            parse_timestamp_pair(wire, 2)?,
            parse_timestamp_pair(wire, 4)?,
            parse_timestamp_pair(wire, 6)?,
        )
    }

    /// Format this timestamp as its exact eight-byte wire representation.
    #[must_use]
    pub fn to_wire_string(self) -> String {
        self.to_string()
    }
}

impl fmt::Display for AprsWeatherTimestamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:02}{:02}{:02}{:02}",
            self.month, self.day, self.hour, self.minute
        )
    }
}

impl core::str::FromStr for AprsWeatherTimestamp {
    type Err = AprsError;

    fn from_str(wire: &str) -> Result<Self, Self::Err> {
        Self::from_wire(wire)
    }
}

fn parse_timestamp_pair(wire: &str, index: usize) -> Result<u8, AprsError> {
    let bytes = wire.as_bytes();
    let high = *bytes
        .get(index)
        .ok_or(AprsError::InvalidTimestamp(TimestampError::Truncated))?;
    let low = *bytes
        .get(index + 1)
        .ok_or(AprsError::InvalidTimestamp(TimestampError::Truncated))?;
    if !high.is_ascii_digit() || !low.is_ascii_digit() {
        return Err(AprsError::InvalidTimestamp(TimestampError::NonDecimal));
    }
    Ok((high - b'0') * 10 + (low - b'0'))
}

const fn validate_day_hour_minute(day: u8, hour: u8, minute: u8) -> Result<(), AprsError> {
    if day == 0 || day > 31 {
        return Err(AprsError::InvalidTimestamp(TimestampError::Day));
    }
    if hour > 23 {
        return Err(AprsError::InvalidTimestamp(TimestampError::Hour));
    }
    if minute > 59 {
        return Err(AprsError::InvalidTimestamp(TimestampError::Minute));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phg and AprsDataExtension
// ---------------------------------------------------------------------------

/// Antenna directivity code from a PHG extension.
///
/// APRS 1.0.1 §7.1 reserves the 4th digit of a `PHGNhgd` field for
/// directivity, with the following mapping (paraphrased from the spec's
/// Table 7-2):
///
/// | Code | Direction                  | Degrees |
/// |------|----------------------------|---------|
/// | `0`  | Omni-directional           | n/a     |
/// | `1`  | North-East                 | 45      |
/// | `2`  | East                       | 90      |
/// | `3`  | South-East                 | 135     |
/// | `4`  | South                      | 180     |
/// | `5`  | South-West                 | 225     |
/// | `6`  | West                       | 270     |
/// | `7`  | North-West                 | 315     |
/// | `8`  | North                      | 360     |
/// | `9`  | Undefined (treat as omni)  | n/a     |
///
/// Use [`Self::from_code`] to construct from the raw `0..=9` ASCII
/// digit value, [`Self::to_degrees`] to recover degrees from cardinal
/// codes, and [`Self::is_omni`] to test for omni semantics
/// (both [`Self::Omni`] and [`Self::Undefined`] are treated as omni
/// in receive-side range-circle code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhgDirectivity {
    /// Omni-directional antenna (code `0`).
    Omni,
    /// Directional antenna, 45° (north-east, code `1`).
    NorthEast,
    /// Directional antenna, 90° (east, code `2`).
    East,
    /// Directional antenna, 135° (south-east, code `3`).
    SouthEast,
    /// Directional antenna, 180° (south, code `4`).
    South,
    /// Directional antenna, 225° (south-west, code `5`).
    SouthWest,
    /// Directional antenna, 270° (west, code `6`).
    West,
    /// Directional antenna, 315° (north-west, code `7`).
    NorthWest,
    /// Directional antenna, 360° (north, code `8`). Equivalent to 0° on
    /// the compass; spec preserves the literal "360" reading from the
    /// wire so receivers can distinguish a station that filled in the
    /// `8` digit from one that filled in `0` (omni).
    North,
    /// Spec-reserved "undefined" directivity (code `9`). Treat as omni
    /// for range-circle calculations; surface for diagnostic logging.
    Undefined,
}

impl PhgDirectivity {
    /// Return the original numeric directivity code, from zero through nine.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Omni => 0,
            Self::NorthEast => 1,
            Self::East => 2,
            Self::SouthEast => 3,
            Self::South => 4,
            Self::SouthWest => 5,
            Self::West => 6,
            Self::NorthWest => 7,
            Self::North => 8,
            Self::Undefined => 9,
        }
    }
    /// Parse a directivity code (`0..=9`) into the typed enum.
    ///
    /// Returns `None` for any code outside that range.
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Omni),
            1 => Some(Self::NorthEast),
            2 => Some(Self::East),
            3 => Some(Self::SouthEast),
            4 => Some(Self::South),
            5 => Some(Self::SouthWest),
            6 => Some(Self::West),
            7 => Some(Self::NorthWest),
            8 => Some(Self::North),
            9 => Some(Self::Undefined),
            _ => None,
        }
    }

    /// Recover the directivity in compass degrees.
    ///
    /// Returns `None` for [`Self::Omni`] and [`Self::Undefined`] (no
    /// single bearing applies). All other variants return the spec-
    /// defined 45° increments.
    #[must_use]
    pub const fn to_degrees(self) -> Option<u16> {
        match self {
            Self::Omni | Self::Undefined => None,
            Self::NorthEast => Some(45),
            Self::East => Some(90),
            Self::SouthEast => Some(135),
            Self::South => Some(180),
            Self::SouthWest => Some(225),
            Self::West => Some(270),
            Self::NorthWest => Some(315),
            Self::North => Some(360),
        }
    }

    /// Whether range-circle calculators should treat this directivity as
    /// omni-directional. Both [`Self::Omni`] (explicit) and
    /// [`Self::Undefined`] (spec-reserved code `9`) return `true`.
    #[must_use]
    pub const fn is_omni(self) -> bool {
        matches!(self, Self::Omni | Self::Undefined)
    }
}

impl TryFrom<u8> for PhgDirectivity {
    type Error = AprsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_code(value).ok_or(AprsError::InvalidFormat)
    }
}
impl From<PhgDirectivity> for u8 {
    fn from(value: PhgDirectivity) -> Self {
        value.code()
    }
}

impl fmt::Display for PhgDirectivity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Omni => "omni",
            Self::NorthEast => "NE",
            Self::East => "E",
            Self::SouthEast => "SE",
            Self::South => "S",
            Self::SouthWest => "SW",
            Self::West => "W",
            Self::NorthWest => "NW",
            Self::North => "N",
            Self::Undefined => "undef",
        };
        f.write_str(s)
    }
}

/// Power-Height-Gain-Directivity data (APRS 1.0.1 Chapter 7).
///
/// PHG provides station RF characteristics for range-circle calculations.
/// The four codes in a `PHGNhgd` extension map to physical
/// quantities via the spec's Table 7-1 / 7-2:
///
/// - `N` (power) → `N² watts` (e.g. `5` → 25 W).
/// - `h` (height) → `10 × 2ʰ feet` above average terrain (e.g. `3` → 80 ft).
/// - `g` (gain) → `g dB` direct (no scaling).
/// - `d` (directivity) → see [`PhgDirectivity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phg {
    power_digit: u8,
    height_exponent: u8,
    gain_db: u8,
    directivity: PhgDirectivity,
}

impl Phg {
    /// Decode the four wire codes following `PHG`, preserving every ASCII height.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] unless power/gain/directivity are
    /// decimal digits and height is an ASCII byte at or above `0`.
    pub fn from_bytes(codes: [u8; 4]) -> Result<Self, AprsError> {
        let [power, height, gain, directivity] = codes;
        if !power.is_ascii_digit()
            || !gain.is_ascii_digit()
            || !directivity.is_ascii_digit()
            || !(b'0'..=127).contains(&height)
        {
            return Err(AprsError::InvalidFormat);
        }
        Ok(Self {
            power_digit: power - b'0',
            height_exponent: height - b'0',
            gain_db: gain - b'0',
            directivity: PhgDirectivity::from_code(directivity - b'0')
                .ok_or(AprsError::InvalidFormat)?,
        })
    }
    /// Return the encoded transmitter power in watts.
    #[must_use]
    pub fn power_watts(self) -> u32 {
        let code = u32::from(self.power_digit);
        code * code
    }
    /// Return the encoded height exponent in `10 × 2^h` feet above average terrain.
    #[must_use]
    pub const fn height_exponent(self) -> u8 {
        self.height_exponent
    }
    /// Return height in feet above average terrain; every ASCII exponent fits f64.
    #[must_use]
    pub fn height_feet(self) -> f64 {
        10.0 * 2.0_f64.powi(i32::from(self.height_exponent))
    }
    /// Return height as u32, or None when its representation exceeds u32.
    ///
    /// Exponents above 28 remain valid PHG data and are available through
    /// [`Self::height_exponent`] and [`Self::height_feet`].
    #[must_use]
    pub const fn height_feet_u32(self) -> Option<u32> {
        if self.height_exponent <= 28 {
            Some(10_u32 << self.height_exponent)
        } else {
            None
        }
    }
    /// Return antenna gain in decibels.
    #[must_use]
    pub const fn gain_db(self) -> u8 {
        self.gain_db
    }
    /// Return the encoded antenna directivity.
    #[must_use]
    pub const fn directivity(self) -> PhgDirectivity {
        self.directivity
    }
}

impl TryFrom<[u8; 4]> for Phg {
    type Error = AprsError;
    fn try_from(value: [u8; 4]) -> Result<Self, Self::Error> {
        Self::from_bytes(value)
    }
}
impl From<Phg> for [u8; 4] {
    fn from(value: Phg) -> Self {
        [
            b'0' + value.power_digit,
            b'0' + value.height_exponent,
            b'0' + value.gain_db,
            b'0' + value.directivity.code(),
        ]
    }
}

/// A course and speed expressed in their checked geographic domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CourseSpeed {
    course: Course,
    speed: Speed,
}

impl CourseSpeed {
    /// Combine a validated course and nonnegative finite speed.
    #[must_use]
    pub const fn new(course: Course, speed: Speed) -> Self {
        Self { course, speed }
    }
    /// Return the course, including the wire value zero when reported.
    #[must_use]
    pub const fn course(self) -> Course {
        self.course
    }
    /// Return speed without rounding away compressed-format precision.
    #[must_use]
    pub const fn speed(self) -> Speed {
        self.speed
    }
}

/// One base-91 datum identifier in a DAO extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DaoDatum(u8);

impl DaoDatum {
    /// Validate a datum byte in ASCII `!` through `{`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] outside the base-91 range.
    pub const fn new(byte: u8) -> Result<Self, AprsError> {
        if byte >= b'!' && byte <= b'{' {
            Ok(Self(byte))
        } else {
            Err(AprsError::InvalidFormat)
        }
    }
    /// Return the transmitted datum byte, preserving case and local identifiers.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DaoDatum {
    type Error = AprsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<DaoDatum> for u8 {
    fn from(value: DaoDatum) -> Self {
        value.0
    }
}

/// Precision interpretation selected by the DAO datum byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaoPrecision {
    /// Both precision characters are spaces: datum identity only.
    None,
    /// Uppercase datum: one decimal thousandth-of-minute digit per coordinate.
    Decimal,
    /// Lowercase datum: one base-91 fraction of a hundredth of a minute.
    Base91,
    /// A local/other datum with preserved base-91 bytes and no defined scale.
    Unspecified,
}

/// A datum-first `!DAO!` precision extension with immutable wire identity.
///
/// See the [primary precision/datum proposal](https://www.aprs.org/datum.txt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaoExtension {
    datum: DaoDatum,
    latitude: u8,
    longitude: u8,
    precision: DaoPrecision,
}

impl DaoExtension {
    /// Decode exactly `!`, datum, latitude precision, longitude precision, `!`.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] for delimiters, invalid datum,
    /// mismatched null characters, or precision characters outside their encoding.
    pub fn from_bytes(bytes: [u8; 5]) -> Result<Self, AprsError> {
        let [start, datum, latitude, longitude, end] = bytes;
        if start != b'!' || end != b'!' {
            return Err(AprsError::InvalidFormat);
        }
        let datum = DaoDatum::new(datum)?;
        let precision = if latitude == b' ' && longitude == b' ' {
            DaoPrecision::None
        } else if datum.0.is_ascii_uppercase()
            && latitude.is_ascii_digit()
            && longitude.is_ascii_digit()
        {
            DaoPrecision::Decimal
        } else if (b'!'..=b'{').contains(&latitude) && (b'!'..=b'{').contains(&longitude) {
            if datum.0.is_ascii_lowercase() {
                DaoPrecision::Base91
            } else if !datum.0.is_ascii_alphabetic() {
                DaoPrecision::Unspecified
            } else {
                return Err(AprsError::InvalidFormat);
            }
        } else {
            return Err(AprsError::InvalidFormat);
        };
        Ok(Self {
            datum,
            latitude,
            longitude,
            precision,
        })
    }
    /// Return the exact datum identifier, including its precision-selecting case.
    #[must_use]
    pub const fn datum(self) -> DaoDatum {
        self.datum
    }
    /// Return the precision encoding.
    #[must_use]
    pub const fn precision(self) -> DaoPrecision {
        self.precision
    }
    /// Return latitude's nonnegative increment in degrees, or None for datum-only/undefined scale.
    #[must_use]
    pub fn latitude_offset_degrees(self) -> Option<f64> {
        self.offset(self.latitude)
    }
    /// Return longitude's nonnegative increment in degrees, or None for datum-only/undefined scale.
    #[must_use]
    pub fn longitude_offset_degrees(self) -> Option<f64> {
        self.offset(self.longitude)
    }
    /// Return all five original bytes, without allocating.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 5] {
        [b'!', self.datum.0, self.latitude, self.longitude, b'!']
    }
    fn offset(self, byte: u8) -> Option<f64> {
        match self.precision {
            DaoPrecision::Decimal => Some(f64::from(byte - b'0') / 60_000.0),
            DaoPrecision::Base91 => Some(f64::from(byte - b'!') / (91.0 * 6_000.0)),
            DaoPrecision::None | DaoPrecision::Unspecified => None,
        }
    }
}

impl TryFrom<[u8; 5]> for DaoExtension {
    type Error = AprsError;
    fn try_from(value: [u8; 5]) -> Result<Self, Self::Error> {
        Self::from_bytes(value)
    }
}
impl From<DaoExtension> for [u8; 5] {
    fn from(value: DaoExtension) -> Self {
        value.to_bytes()
    }
}

/// Parsed APRS data extensions from the position comment field.
///
/// Position reports can carry structured data in the comment string
/// after the coordinates. This struct captures the extensions defined
/// in APRS101 Chapters 6-7.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AprsDataExtension {
    /// Course in degrees (0-360) and speed in knots, from CSE/SPD.
    course_speed: Option<CourseSpeed>,
    /// Power, Height, Gain, Directivity (PHG).
    phg: Option<Phg>,
    /// Altitude in feet (from `/A=NNNNNN` in comment).
    altitude_ft: Option<i32>,
    /// DAO precision extension (`!DAO!` for extra lat/lon digits).
    dao: Option<DaoExtension>,
}

impl AprsDataExtension {
    /// Return the initial course/speed extension when present.
    #[must_use]
    pub const fn course_speed(&self) -> Option<CourseSpeed> {
        self.course_speed
    }
    /// Return decoded power/height/gain/directivity.
    #[must_use]
    pub const fn phg(&self) -> Option<Phg> {
        self.phg
    }
    /// Return altitude in feet from the comment or compressed-position body.
    #[must_use]
    pub const fn altitude_feet(&self) -> Option<i32> {
        self.altitude_ft
    }
    /// Return datum identity and precision encoding without applying a datum transform.
    #[must_use]
    pub const fn dao(&self) -> Option<DaoExtension> {
        self.dao
    }
    pub(crate) const fn with_altitude(mut self, altitude: i32) -> Self {
        self.altitude_ft = Some(altitude);
        self
    }

    /// Parse data extensions from an APRS position comment string.
    ///
    /// Extracts CSE/SPD, PHG, altitude (`/A=NNNNNN`), and DAO (`!DAO!`)
    /// extensions per APRS101 Chapters 6-7.
    ///
    /// # Parameters
    ///
    /// - `comment`: The comment string after the APRS position fields.
    ///
    /// # Returns
    ///
    /// An [`AprsDataExtension`] with each field populated if found.
    #[must_use]
    pub fn from_comment(comment: &str) -> Self {
        let course_speed = parse_cse_spd(comment);
        let phg = parse_phg(comment);
        let altitude_ft = parse_altitude(comment);
        let dao = parse_dao(comment);

        Self {
            course_speed,
            phg,
            altitude_ft,
            dao,
        }
    }
}

/// Parse CSE/SPD from the first 7 characters of the comment.
///
/// Format: `DDD/SSS` where DDD is 3-digit course (000-360) and SSS is
/// 3-digit speed in knots. Per APRS101 Chapter 7, this must be at the
/// start of the comment and use the exact `NNN/NNN` format.
fn parse_cse_spd(comment: &str) -> Option<CourseSpeed> {
    let bytes = comment.as_bytes();
    let header = bytes.get(..7)?;
    if header.get(3) != Some(&b'/') {
        return None;
    }
    let dir_bytes = header.get(..3)?;
    let spd_bytes = header.get(4..7)?;
    if !dir_bytes.iter().all(u8::is_ascii_digit) || !spd_bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let course: u16 = comment.get(0..3)?.parse().ok()?;
    let speed: u16 = comment.get(4..7)?.parse().ok()?;
    if course > 360 {
        return None;
    }
    Some(CourseSpeed::new(
        Course::new(course).ok()?,
        Speed::from_knots(f64::from(speed)).ok()?,
    ))
}

/// Parse a PHG extension from the comment string.
///
/// Format: `PHGNhgd` anywhere in the comment, where:
///
/// - `N` is an ASCII digit `'0'..='9'` (power).
/// - `h` is any ASCII character `'0'` and above (height), spec-allowed
///   to extend beyond `'9'` for balloons, aircraft, and satellites.
///   Its exponent is preserved by [`Phg`].
/// - `g` is an ASCII digit `'0'..='9'` (gain, direct in dB).
/// - `d` is an ASCII digit `'0'..='9'` (directivity); mapped via
///   [`PhgDirectivity::from_code`].
///
/// Returns `None` if `PHG` is absent or any field fails its validation
/// rule. Large height exponents remain available even when a u32 cannot
/// represent the corresponding feet value.
fn parse_phg(comment: &str) -> Option<Phg> {
    let idx = comment.find("PHG")?;
    let rest = comment.get(idx + 3..)?;
    let first_four = rest.get(..4)?.as_bytes();

    // Power, gain, and directivity must be ASCII digits; only the
    // height byte is allowed to extend beyond `'9'` per spec.
    let p_byte = *first_four.first()?;
    let h_byte = *first_four.get(1)?;
    let g_byte = *first_four.get(2)?;
    let d_byte = *first_four.get(3)?;
    if !p_byte.is_ascii_digit() || !g_byte.is_ascii_digit() || !d_byte.is_ascii_digit() {
        return None;
    }

    Phg::from_bytes([p_byte, h_byte, g_byte, d_byte]).ok()
}

/// Parse altitude extension from the comment string.
///
/// Format: `/A=NNNNNN` anywhere in the comment (6-digit altitude in feet,
/// can be negative with a leading minus sign in the 6-digit field).
fn parse_altitude(comment: &str) -> Option<i32> {
    let idx = comment.find("/A=")?;
    let rest = comment.get(idx + 3..)?;
    let val_str = rest.get(..6)?;
    val_str.parse::<i32>().ok()
}

/// Parse a DAO extension from the comment string.
///
/// The first component identifies the datum and precision encoding. Invalid
/// candidates are skipped while searching for a complete later extension.
fn parse_dao(comment: &str) -> Option<DaoExtension> {
    comment.as_bytes().windows(5).find_map(|window| {
        let bytes: [u8; 5] = window.try_into().ok()?;
        DaoExtension::from_bytes(bytes).ok()
    })
}

// ---------------------------------------------------------------------------
// MessageKind
// ---------------------------------------------------------------------------

/// APRS message kind (per APRS 1.0.1 §14 and bulletin sections).
///
/// Distinguishes direct station-to-station messages from the various
/// bulletin and announcement forms based on the addressee prefix. The
/// spec defines five distinct bulletin-family forms at §14 pp.73-74,
/// and a "BLN" addressee that starts with a *letter* (announcement)
/// is structurally different from one that starts with a *digit*
/// (general or group bulletin).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Direct station-to-station message.
    Direct,
    /// Numeric general bulletin (addressee `BLN0`-`BLN9`, i.e. exactly
    /// the `BLN` prefix followed by a single ASCII digit).
    Bulletin {
        /// Bulletin number (0-9).
        number: BulletinNumber,
    },
    /// Group bulletin (addressee `BLN<digit><group>` per APRS 1.0.1
    /// §14 p.74: `BLN` prefix, then a single digit ID, then a 1-5
    /// character group name, e.g. `BLN4WX` for weather group bulletin number 4).
    GroupBulletin {
        /// Bulletin number (0-9).
        number: BulletinNumber,
        /// Group identifier (1-5 visible ASCII characters).
        group: BulletinGroup,
    },
    /// Announcement (addressee `BLNA`-`BLNZ`, i.e. exactly the `BLN`
    /// prefix followed by a single uppercase ASCII letter). Per APRS
    /// 1.0.1 §14 p.73, announcements are transmitted less frequently
    /// than general bulletins and are intended for less time-sensitive
    /// content. Distinct from `GroupBulletin` (which has a multi-char
    /// group identifier) and `Bulletin` (which is numeric).
    Announcement {
        /// Announcement letter (A-Z).
        letter: AnnouncementLetter,
    },
    /// National Weather Service bulletin (addressee `NWS-*`, `SKY-*`,
    /// `CWA-*`, `BOM-*`).
    NwsBulletin,
    /// A complete `ack`/`rej` control body followed by a validated ID,
    /// without a separate message-ID trailer.
    AckRej,
}

// ---------------------------------------------------------------------------
// TelemetryDefinition
// ---------------------------------------------------------------------------

/// Telemetry parameter definitions sent as APRS messages.
///
/// Per APRS 1.0.1 §13.2, a station that emits telemetry frames can send
/// four additional parameter-definition messages to tell receivers how
/// to interpret the analog and digital channels. These messages use the
/// standard APRS message format (`:ADDRESSEE:PARM.…`) with a well-known
/// keyword prefix.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryDefinition {
    /// `PARM.P1,P2,P3,P4,P5,B1,B2,B3,B4,B5,B6,B7,B8`: human-readable
    /// names for 5 analog + 8 digital channels.
    Parameters(TelemetryLabels),
    /// `UNIT.U1,U2,U3,U4,U5,B1,B2,B3,B4,B5,B6,B7,B8`: unit labels.
    Units(TelemetryLabels),
    /// `EQNS.a1,b1,c1,a2,b2,c2,...`: an exact prefix of up to fifteen
    /// calibration coefficients for `y = a*x² + b*x + c`.
    Equations(TelemetryEquationCoefficients),
    /// `BITS.b1b2b3b4b5b6b7b8,project_title`: active-bit mask plus
    /// project title.
    Bits {
        /// Eight digital-channel polarity bits, most significant channel first.
        bits: TelemetryBits,
        /// Free-form project title (up to 23 characters).
        title: TelemetryProjectTitle,
    },
}

impl TelemetryDefinition {
    /// Try to parse a telemetry parameter-definition message from the
    /// text portion of an [`AprsMessage`] (everything after the second
    /// `:` in the wire frame).
    ///
    /// Returns `Ok(None)` when the text doesn't start with a known keyword.
    /// Parsing examines at most 13 labels or 15 coefficients before rejecting
    /// excess fields. Missing trailing coefficients remain omitted, including
    /// a final partial triple; malformed/nonfinite supplied fields are errors.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] for malformed recognized definitions,
    /// excessive fields, invalid positional widths, nonfinite coefficients or
    /// an invalid bit mask/title. PARM/UNIT retain the documented 68-byte maximum
    /// layout adaptation; other definition bodies are at most 67 bytes.
    pub fn from_text(text: &str) -> Result<Option<Self>, AprsError> {
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("PARM.") {
            if trimmed.len() > 68 {
                return Err(AprsError::InvalidFormat);
            }
            return Ok(Some(Self::Parameters(parse_telemetry_labels(rest)?)));
        }
        if let Some(rest) = trimmed.strip_prefix("UNIT.") {
            if trimmed.len() > 68 {
                return Err(AprsError::InvalidFormat);
            }
            return Ok(Some(Self::Units(parse_telemetry_labels(rest)?)));
        }
        if let Some(rest) = trimmed.strip_prefix("EQNS.") {
            if trimmed.len() > 67 {
                return Err(AprsError::InvalidFormat);
            }
            return Ok(Some(Self::Equations(parse_telemetry_equations(rest)?)));
        }
        if let Some(rest) = trimmed.strip_prefix("BITS.") {
            if trimmed.len() > 67 {
                return Err(AprsError::InvalidFormat);
            }
            let (bits, title) = rest.split_once(',').unwrap_or((rest, ""));
            // APRS 1.0.1 §13 p.70: the bit-sense field is exactly 8 binary
            // digits (`b1..b8`). Reject any other length (e.g. an empty
            // mask from a bare "BITS.") or non-binary content rather than
            // surfacing a malformed zero-length active-bit mask.
            if bits.len() != 8 || !bits.bytes().all(|b| b == b'0' || b == b'1') {
                return Err(AprsError::InvalidFormat);
            }
            let mask = bits
                .bytes()
                .fold(0_u8, |mask, bit| (mask << 1) | (bit - b'0'));
            return Ok(Some(Self::Bits {
                bits: TelemetryBits::from_bits(mask),
                title: TelemetryProjectTitle::new(title).map_err(|_| AprsError::InvalidFormat)?,
            }));
        }
        Ok(None)
    }
}

/// Parse a comma-separated label list for `PARM.` / `UNIT.`.
fn parse_telemetry_labels(s: &str) -> Result<TelemetryLabels, AprsError> {
    let mut fields = [""; 13];
    let mut count = 0;
    for field in s.split(',') {
        let slot = fields.get_mut(count).ok_or(AprsError::InvalidFormat)?;
        *slot = field;
        count += 1;
    }
    TelemetryLabels::new(fields.get(..count).ok_or(AprsError::InvalidFormat)?)
        .map_err(|_| AprsError::InvalidFormat)
}

/// Parse the exact nonempty prefix of at most 15 finite coefficients.
///
/// The wire format is 15 comma-separated decimal values per APRS 1.0.1
/// §13 p.70: three coefficients (a, b, c) for each of five analog
/// telemetry channels. The equation is `y = a·v² + b·v + c` where `v`
/// is the raw channel value.
///
/// A malformed supplied field rejects the complete definition; omitted tail
/// fields and a final partial triple remain distinct from supplied zeros.
fn parse_telemetry_equations(s: &str) -> Result<TelemetryEquationCoefficients, AprsError> {
    let mut values = [0.0; 15];
    let mut count = 0;
    for field in s.split(',') {
        let slot = values.get_mut(count).ok_or(AprsError::InvalidFormat)?;
        *slot = field
            .trim()
            .parse::<f64>()
            .map_err(|_| AprsError::InvalidFormat)?;
        count += 1;
    }
    TelemetryEquationCoefficients::new(values.get(..count).ok_or(AprsError::InvalidFormat)?)
        .map_err(|_| AprsError::InvalidFormat)
}

// ---------------------------------------------------------------------------
// AprsData and AprsPacket
// ---------------------------------------------------------------------------

/// A four- or six-character Maidenhead locator from an APRS grid report.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MaidenheadGrid(String);

impl MaidenheadGrid {
    /// Validate field letters A-R, square digits and optional subsquare a-x.
    ///
    /// Letters are checked case-insensitively; the received ASCII case is retained.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidFormat`] for another length or character layout.
    pub fn new(value: &str) -> Result<Self, AprsError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    fn validate(value: &str) -> Result<(), AprsError> {
        let bytes = value.as_bytes();
        let valid = match bytes {
            [field_lon, field_lat, square_lon, square_lat] => {
                (b'A'..=b'R').contains(&field_lon.to_ascii_uppercase())
                    && (b'A'..=b'R').contains(&field_lat.to_ascii_uppercase())
                    && square_lon.is_ascii_digit()
                    && square_lat.is_ascii_digit()
            }
            [
                field_lon,
                field_lat,
                square_lon,
                square_lat,
                subsquare_lon,
                subsquare_lat,
            ] => {
                (b'A'..=b'R').contains(&field_lon.to_ascii_uppercase())
                    && (b'A'..=b'R').contains(&field_lat.to_ascii_uppercase())
                    && square_lon.is_ascii_digit()
                    && square_lat.is_ascii_digit()
                    && (b'a'..=b'x').contains(&subsquare_lon.to_ascii_lowercase())
                    && (b'a'..=b'x').contains(&subsquare_lat.to_ascii_lowercase())
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(AprsError::InvalidFormat)
        }
    }
    /// Borrow the exact validated locator.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::str::FromStr for MaidenheadGrid {
    type Err = AprsError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}
impl TryFrom<&str> for MaidenheadGrid {
    type Error = AprsError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl TryFrom<String> for MaidenheadGrid {
    type Error = AprsError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}
impl AsRef<str> for MaidenheadGrid {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}
impl fmt::Display for MaidenheadGrid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl From<MaidenheadGrid> for String {
    fn from(value: MaidenheadGrid) -> Self {
        value.0
    }
}

/// Legacy raw-weather identifier; its payload remains uninterpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RawWeatherFormat {
    /// The `#` identifier.
    Hash,
    /// The `*` identifier.
    Asterisk,
}

impl RawWeatherFormat {
    /// Return the original data-type identifier.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Hash => b'#',
            Self::Asterisk => b'*',
        }
    }
}
impl TryFrom<u8> for RawWeatherFormat {
    type Error = AprsError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            b'#' => Ok(Self::Hash),
            b'*' => Ok(Self::Asterisk),
            _ => Err(AprsError::InvalidFormat),
        }
    }
}
impl From<RawWeatherFormat> for u8 {
    fn from(value: RawWeatherFormat) -> Self {
        value.as_byte()
    }
}

/// An opaque one-byte user-defined experiment identifier, preserving all octets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExperimentId(u8);
impl ExperimentId {
    /// Preserve one received identifier byte, including unknown extensions.
    #[must_use]
    pub const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }
    /// Return the exact identifier octet.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self.0
    }
}
impl From<u8> for ExperimentId {
    fn from(value: u8) -> Self {
        Self(value)
    }
}
impl From<ExperimentId> for u8 {
    fn from(value: ExperimentId) -> Self {
        value.0
    }
}

/// A parsed APRS data frame, covering all major APRS data types.
///
/// Per APRS101.PDF, the data type is determined by the first byte of the
/// AX.25 information field. Structured variants retain checked values;
/// uninterpreted extension formats retain their original payload bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AprsData {
    /// Position report (uncompressed, compressed, or Mic-E).
    Position(AprsPosition),
    /// APRS message addressed to a specific station.
    Message(AprsMessage),
    /// Status report (free-form text, optionally with Maidenhead grid).
    Status(AprsStatus),
    /// Object report (named, with position and timestamp).
    Object(AprsObject),
    /// Item report (named, with position, no timestamp).
    Item(AprsItem),
    /// Standalone positionless weather report with its mandatory timestamp.
    PositionlessWeather(AprsPositionlessWeatherReport),
    /// Telemetry report (analog values and digital status).
    Telemetry(AprsTelemetry),
    /// Query (position, status, message, or direction finding).
    Query(AprsQuery),
    /// Third-party traffic: a packet originating elsewhere and
    /// forwarded by an intermediate station (APRS 1.0.1 §17). The
    /// `header` carries the original `source>dest,path` and the
    /// `payload` the original info field.
    ThirdParty {
        /// Raw `source>dest,path` header text from the third-party
        /// wrapper.
        header: String,
        /// Original APRS info field as bytes (no further parsing).
        payload: Vec<u8>,
    },
    /// Maidenhead grid locator (data type `[`). The string form is the
    /// 4-6 character grid square, e.g. `"EM13qc"` or `"FM18lv"`.
    Grid(MaidenheadGrid),
    /// Raw GPS sentence / Ultimeter 2000 data (data type `$`).
    ///
    /// APRS 1.0.1 §5.2: anything starting with `$GP`, `$GN`, `$GL`,
    /// `$GA` (GPS/GNSS NMEA) or other `$`-prefixed instrument data.
    /// Holds the full NMEA sentence minus the leading `$`.
    RawGps(String),
    /// Station capabilities report (data type `<`).
    ///
    /// APRS 1.0.1 §15.2: comma-separated `TOKEN=value` tuples
    /// describing what the station supports (`IGATE`, `MSG_CNT`,
    /// `LOC_CNT`, etc.), stored as a list of pairs.
    StationCapabilities(Vec<(String, String)>),
    /// Agrelo `DFjr` (direction-finding) data (data type `%`).
    ///
    /// The library does not interpret the binary format; it preserves
    /// the raw payload bytes for callers that do.
    AgreloDfJr(Vec<u8>),
    /// User-defined APRS data (data type `{`).
    ///
    /// APRS 1.0.1 §18: format is `{<experiment_id><type><data>` where
    /// the experiment ID is one character. It is split out for
    /// convenience; callers that understand the experiment can parse
    /// the rest.
    UserDefined {
        /// One-character experiment identifier (immediately follows `{`).
        experiment: ExperimentId,
        /// Everything after the experiment ID.
        data: Vec<u8>,
    },
    /// Invalid/test frame (data type `,`).
    ///
    /// Used for test beacons and frames that should be ignored by
    /// normal receivers. The payload is preserved for diagnostics.
    InvalidOrTest(Vec<u8>),
    /// Raw weather data from legacy commercial weather stations
    /// (data types `#` Peet Bros U-II, `*` Peet Bros U-II under a
    /// different ID per APRS 1.0.1 §12 p.62 Table).
    ///
    /// Addendum 1.1 deprecates these formats ("not recommended on RF;
    /// please use the Complete Weather Report format instead") but
    /// receivers must tolerate them. The library does not attempt to
    /// parse the proprietary ASCII-hex payload; downstream code can
    /// either decode it via a dedicated Peet Bros parser or log it
    /// for diagnostic purposes.
    RawWeather {
        /// Original data-type identifier byte (`b'#'` or `b'*'`).
        data_type: RawWeatherFormat,
        /// Everything after the type byte, byte-for-byte.
        payload: Vec<u8>,
    },
}

/// A decoded APRS payload without an AX.25 routing envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsPacket {
    /// Decoded APRS data payload.
    pub data: AprsData,
}

impl AprsData {
    /// Parse one complete AX.25 information field, without reading a clock or I/O.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::MicERequiresDestination`] for Mic-E identifiers;
    /// [`Self::from_bytes_with_destination`] accepts their required destination.
    /// Other malformed recognized fields propagate their documented parse error;
    /// an empty or unknown type returns [`AprsError::InvalidFormat`].
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_data(info)
    }
}

// ---------------------------------------------------------------------------
// parse_aprs_data dispatcher
// ---------------------------------------------------------------------------

/// Parse any APRS data frame from an AX.25 information field.
///
/// Dispatches based on the data type identifier (first byte) to the
/// appropriate parser. For Mic-E positions, use
/// [`AprsPosition::from_mice_bytes`] directly since it also requires
/// the destination address.
///
/// **Prefer [`AprsData::from_bytes_with_destination`] when the AX.25
/// destination address is available**, since it handles all data types
/// including Mic-E.
///
/// # Supported data types
///
/// | Byte | Type | Parser |
/// |------|------|--------|
/// | `!`, `=` | Position (no timestamp) | [`AprsPosition::from_bytes`] |
/// | `/`, `@` | Position (with timestamp) | [`AprsPosition::from_bytes`] |
/// | `:` | Message | Inline |
/// | `>` | Status | Inline |
/// | `;` | Object | Inline |
/// | `)` | Item | Inline |
/// | `_` | Positionless weather | Inline |
/// | `` ` ``, `'` | Mic-E | Returns error (use [`AprsPosition::from_mice_bytes`]) |
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
fn parse_aprs_data(info: &[u8]) -> Result<AprsData, AprsError> {
    let first = *info.first().ok_or(AprsError::InvalidFormat)?;

    match first {
        // Position reports (uncompressed and compressed)
        b'!' | b'=' | b'/' | b'@' => AprsPosition::from_bytes(info).map(AprsData::Position),
        // Message
        b':' => AprsMessage::from_bytes(info).map(AprsData::Message),
        // Status
        b'>' => AprsStatus::from_bytes(info).map(AprsData::Status),
        // Object
        b';' => AprsObject::from_bytes(info).map(AprsData::Object),
        // Item
        b')' => AprsItem::from_bytes(info).map(AprsData::Item),
        // Positionless weather
        b'_' => AprsPositionlessWeatherReport::from_bytes(info).map(AprsData::PositionlessWeather),
        // Telemetry
        b'T' => AprsTelemetry::from_bytes(info).map(AprsData::Telemetry),
        // Query
        b'?' => AprsQuery::from_bytes(info).map(AprsData::Query),
        // Third-party traffic (APRS 1.0.1 §17): `}source>dest,path:payload`
        b'}' => parse_aprs_third_party(info),
        // Maidenhead grid locator (APRS 1.0.1 §5.6): `[EM13qc`
        b'[' => parse_aprs_grid(info),
        // Raw GPS / NMEA / Ultimeter (APRS 1.0.1 §5.2): `$GPRMC,...`
        b'$' => parse_aprs_raw_gps(info),
        // Station capabilities (APRS 1.0.1 §15.2): `<IGATE,MSG_CNT=10,LOC_CNT=0`
        b'<' => parse_aprs_capabilities(info),
        // Agrelo DFjr direction-finding data (APRS 1.0.1 §5.5): `%...`
        b'%' => Ok(AprsData::AgreloDfJr(info.get(1..).unwrap_or(&[]).to_vec())),
        // User-defined data (APRS 1.0.1 §18): `{<expid><type><data>`
        b'{' => parse_aprs_user_defined(info),
        // Invalid/test data (APRS 1.0.1 §5.7): `,...`
        b',' => Ok(AprsData::InvalidOrTest(
            info.get(1..).unwrap_or(&[]).to_vec(),
        )),
        // Raw weather formats from legacy commercial stations
        // (APRS 1.0.1 §12 p.62 Table: `#` Peet Bros U-II, `*` Peet
        // Bros U-II again under a different ID, second `$` Ultimeter
        // 2000). Addendum 1.1 line 156-157 deprecates these
        // ("not recommended on RF") but says receivers MUST tolerate
        // them. We surface the raw payload via `RawWeather` so
        // downstream code can choose to parse Peet Bros format
        // separately or just log the bytes.
        b'#' | b'*' => Ok(AprsData::RawWeather {
            data_type: RawWeatherFormat::try_from(first)?,
            payload: info.get(1..).unwrap_or(&[]).to_vec(),
        }),
        // Mic-E identifiers need the destination supplied to from_bytes_with_destination.
        b'`' | b'\'' | 0x1C | 0x1D => Err(AprsError::MicERequiresDestination),
        // All other types are unrecognized.
        _ => Err(AprsError::InvalidFormat),
    }
}

/// Parse an APRS third-party traffic frame (data type `}`).
///
/// Format: `}source>dest,path:payload`. The outer envelope identifies
/// the station that forwarded the packet, and the inner fields carry
/// the original packet exactly as it appeared on its origin transport
/// (typically APRS-IS).
fn parse_aprs_third_party(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'}') {
        return Err(AprsError::InvalidFormat);
    }
    let body = info.get(1..).ok_or(AprsError::InvalidFormat)?;
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return Err(AprsError::InvalidFormat);
    };
    let header_bytes = body.get(..colon).ok_or(AprsError::InvalidFormat)?;
    let payload = body
        .get(colon + 1..)
        .ok_or(AprsError::InvalidFormat)?
        .to_vec();
    let header = decode_wire_ascii(AprsWireTextField::ThirdPartyHeader, header_bytes)?.to_owned();
    Ok(AprsData::ThirdParty { header, payload })
}

/// Parse an APRS Maidenhead grid locator frame (data type `[`).
///
/// Format: `[<4-6 chars>`. The locator is left-padded / right-trimmed.
fn parse_aprs_grid(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'[') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = decode_wire_ascii(AprsWireTextField::MaidenheadGrid, tail)?
        .trim_end_matches(['\r', '\n', ' ']);
    Ok(AprsData::Grid(MaidenheadGrid::new(body)?))
}

/// Parse an APRS raw GPS / NMEA frame (data type `$`).
///
/// Per APRS 1.0.1 §5.2, the frame is a full NMEA sentence including the
/// leading `$`. The body is preserved without the leading `$` (so the
/// caller still sees `GPRMC,...` etc.).
fn parse_aprs_raw_gps(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'$') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = decode_wire_ascii(AprsWireTextField::NmeaSentence, tail)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    Ok(AprsData::RawGps(body))
}

/// Parse an APRS station capabilities frame (data type `<`).
///
/// Per APRS 1.0.1 §15.2, the body is a comma-separated list of tokens,
/// each of the form `KEY` (flag) or `KEY=value`. Whitespace around the
/// delimiters is not permitted in the spec but is trimmed anyway for
/// tolerance.
fn parse_aprs_capabilities(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'<') {
        return Err(AprsError::InvalidFormat);
    }
    let tail = info.get(1..).unwrap_or(&[]);
    let body = decode_wire_ascii(AprsWireTextField::StationCapabilities, tail)?
        .trim_end_matches(['\r', '\n']);
    let mut tokens: Vec<(String, String)> = Vec::new();
    for entry in body.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((k, v)) = entry.split_once('=') {
            tokens.push((k.trim().to_owned(), v.trim().to_owned()));
        } else {
            tokens.push((entry.to_owned(), String::new()));
        }
    }
    Ok(AprsData::StationCapabilities(tokens))
}

/// Parse an APRS user-defined frame (data type `{`).
///
/// Per APRS 1.0.1 §18, the frame is `{<experiment_id>[<type>]<data>`.
/// The experiment ID is the first character after `{`.
fn parse_aprs_user_defined(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.first() != Some(&b'{') {
        return Err(AprsError::InvalidFormat);
    }
    let experiment = ExperimentId::from_byte(*info.get(1).ok_or(AprsError::InvalidFormat)?);
    let data = info.get(2..).unwrap_or(&[]).to_vec();
    Ok(AprsData::UserDefined { experiment, data })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- parse_aprs_data dispatch tests ----

    #[test]
    fn dispatch_position() {
        let info = b"!4903.50N/07201.75W-Test";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Position(_))),
            "expected Position variant",
        );
    }

    #[test]
    fn dispatch_message() {
        let info = b":N0CALL   :Hello{1";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Message(_))),
            "expected Message variant",
        );
    }

    #[test]
    fn dispatch_status() {
        let info = b">Status text";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Status(_))),
            "expected Status variant",
        );
    }

    #[test]
    fn dispatch_object() {
        let info = b";OBJNAME  *092345z4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Object(_))),
            "expected Object variant",
        );
    }

    #[test]
    fn dispatch_item() {
        let info = b")ITEM!4903.50N/07201.75W-";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::Item(_))),
            "expected Item variant",
        );
    }

    #[test]
    fn dispatch_weather() {
        let info = b"_01011234c180s005g...t072";
        assert!(
            matches!(parse_aprs_data(info), Ok(AprsData::PositionlessWeather(_))),
            "expected PositionlessWeather variant",
        );
    }

    #[test]
    fn dispatch_third_party() -> TestResult {
        let info = b"}W1AW>APK005,TCPIP:!4903.50N/07201.75W-from IS";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::ThirdParty { header, payload }
                    if header == "W1AW>APK005,TCPIP"
                        && payload == b"!4903.50N/07201.75W-from IS"
            ),
            "expected ThirdParty, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_third_party_rejects_non_ascii_header_without_replacement() {
        let info = b"}\xFF>APK005:!4903.50N/07201.75W-";
        assert_eq!(
            parse_aprs_data(info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::ThirdPartyHeader,
                index: 0,
                byte: 0xFF,
            }),
        );
    }

    #[test]
    fn dispatch_grid_locator() -> TestResult {
        let info = b"[EM13qc";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g.as_str() == "EM13qc"),
            "expected Grid, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_rejects_non_ascii_without_replacement() {
        assert_eq!(
            parse_aprs_data(b"[EM13\xFFc"),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::MaidenheadGrid,
                index: 4,
                byte: 0xFF,
            }),
        );
    }

    #[test]
    fn dispatch_grid_4char() -> TestResult {
        let info = b"[FM18";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g.as_str() == "FM18"),
            "expected Grid, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_invalid_rejected() {
        assert!(parse_aprs_data(b"[XX12").is_err(), "X > R rejected");
        assert!(parse_aprs_data(b"[AB").is_err(), "too short rejected");
    }

    #[test]
    fn dispatch_grid_five_char_rejected() {
        // Maidenhead locators consist of complete two-character pairs.
        assert!(
            parse_aprs_data(b"[AA00!").is_err(),
            "5-char locator must be rejected",
        );
    }

    #[test]
    fn dispatch_grid_four_char_accepted() -> TestResult {
        let result = parse_aprs_data(b"[AA00")?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g.as_str() == "AA00"),
            "expected Grid(AA00), got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_six_char_accepted() -> TestResult {
        let result = parse_aprs_data(b"[FN20qa")?;
        assert!(
            matches!(&result, AprsData::Grid(g) if g.as_str() == "FN20qa"),
            "expected Grid(FN20qa), got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_grid_six_char_bad_subsquare_rejected() {
        // Right length but the sub-square pair is not a-x letters.
        assert!(
            parse_aprs_data(b"[AA00!!").is_err(),
            "6-char with non-letter sub-square must be rejected",
        );
    }

    #[test]
    fn dispatch_raw_gps() -> TestResult {
        let info = b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::RawGps(s) if s.starts_with("GPRMC,") && s.contains("4807.038")
            ),
            "expected RawGps, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_capabilities_parses_tokens() -> TestResult {
        let info = b"<IGATE,MSG_CNT=10,LOC_CNT=42";
        let result = parse_aprs_data(info)?;
        let AprsData::StationCapabilities(tokens) = result else {
            return Err("expected StationCapabilities".into());
        };
        assert_eq!(tokens.len(), 3);
        assert_eq!(tokens.first(), Some(&("IGATE".to_owned(), String::new())));
        assert_eq!(
            tokens.get(1),
            Some(&("MSG_CNT".to_owned(), "10".to_owned()))
        );
        assert_eq!(
            tokens.get(2),
            Some(&("LOC_CNT".to_owned(), "42".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn dispatch_agrelo_df() -> TestResult {
        let info = b"%\x01\x02\x03\x04";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::AgreloDfJr(bytes) if bytes == &vec![1u8, 2, 3, 4]),
            "expected AgreloDfJr, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_user_defined() -> TestResult {
        let info = b"{Adata payload";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::UserDefined { experiment, data }
                    if experiment.as_byte() == b'A' && data == b"data payload"
            ),
            "expected UserDefined, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_invalid_or_test() -> TestResult {
        let info = b",test frame";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(&result, AprsData::InvalidOrTest(bytes) if bytes == b"test frame"),
            "expected InvalidOrTest, got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_raw_weather_peet_bros_hash() -> TestResult {
        // Peet Bros U-II raw weather format `#...`.
        // Receivers must tolerate per APRS 1.1; the library surfaces
        // it as RawWeather rather than rejecting with InvalidFormat.
        let info = b"#0123*4567";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::RawWeather { data_type, payload }
                    if *data_type == RawWeatherFormat::Hash && payload == b"0123*4567"
            ),
            "expected RawWeather(#), got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_raw_weather_peet_bros_star() -> TestResult {
        // Second Peet Bros identifier (`*`), same tolerance rule.
        let info = b"*0123 4567";
        let result = parse_aprs_data(info)?;
        assert!(
            matches!(
                &result,
                AprsData::RawWeather { data_type, payload }
                    if *data_type == RawWeatherFormat::Asterisk && payload == b"0123 4567"
            ),
            "expected RawWeather(*), got {result:?}",
        );
        Ok(())
    }

    #[test]
    fn dispatch_mice_returns_error() {
        // Mic-E needs destination address, can't parse from info alone
        let info = &[0x60u8, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        assert!(
            matches!(
                parse_aprs_data(info),
                Err(AprsError::MicERequiresDestination)
            ),
            "expected MicERequiresDestination",
        );
    }

    // ---- Timestamp tests ----

    #[test]
    fn aprs_timestamp_dhm_zulu_format() -> TestResult {
        let ts = AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?;
        assert_eq!(ts.to_wire_string(), "092345z");
        Ok(())
    }

    #[test]
    fn aprs_timestamp_hms_format() -> TestResult {
        let ts = AprsReportTimestamp::hour_minute_second_utc(12, 0, 1)?;
        assert_eq!(ts.to_wire_string(), "120001h");
        Ok(())
    }

    // ---- Extensions parser tests ----

    #[test]
    fn parse_extensions_cse_spd() -> TestResult {
        let ext = AprsDataExtension::from_comment("088/036");
        let motion = ext.course_speed().ok_or("course/speed")?;
        assert_eq!(motion.course().as_degrees(), 88);
        assert!((motion.speed().as_knots() - 36.0).abs() < f64::EPSILON);
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
        Ok(())
    }

    #[test]
    fn parse_extensions_cse_spd_with_comment() -> TestResult {
        let ext = AprsDataExtension::from_comment("270/015via Mic-E");
        let motion = ext.course_speed().ok_or("course/speed")?;
        assert_eq!(motion.course().as_degrees(), 270);
        assert!((motion.speed().as_knots() - 15.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn parse_extensions_cse_spd_invalid_course() {
        // Course 999 > 360 is invalid.
        let ext = AprsDataExtension::from_comment("999/050");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_cse_spd_not_at_start() {
        // CSE/SPD must be at position 0.
        let ext = AprsDataExtension::from_comment("xx088/036");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_phg() -> TestResult {
        // PHG5132: power=5²=25 W, height=10×2¹=20 feet above average terrain.
        // gain=3 dB, directivity code 2 → East (90°).
        let ext = AprsDataExtension::from_comment("PHG5132");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts(), 25);
        assert!((phg.height_feet() - 20.0).abs() < f64::EPSILON);
        assert_eq!(phg.gain_db(), 3);
        assert_eq!(phg.directivity(), PhgDirectivity::East);
        assert_eq!(phg.directivity().to_degrees(), Some(90));
        Ok(())
    }

    #[test]
    fn parse_extensions_phg_omni() -> TestResult {
        // PHG2360: power=2²=4 W, height=10×2³=80 ft, gain=6 dB,
        // directivity code 0 → Omni.
        let ext = AprsDataExtension::from_comment("PHG2360");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts(), 4);
        assert!((phg.height_feet() - 80.0).abs() < f64::EPSILON);
        assert_eq!(phg.gain_db(), 6);
        assert_eq!(phg.directivity(), PhgDirectivity::Omni);
        assert_eq!(phg.directivity().to_degrees(), None);
        assert!(phg.directivity().is_omni());
        Ok(())
    }

    #[test]
    fn parse_extensions_phg_in_comment() -> TestResult {
        let ext = AprsDataExtension::from_comment("some text PHG5132 more text");
        let phg = ext.phg.ok_or("phg missing")?;
        assert_eq!(phg.power_watts(), 25);
        Ok(())
    }

    #[test]
    fn phg_directivity_undefined_is_omni() {
        // Spec code 9 maps to Undefined, which displays as "undef"
        // but should be treated as omni for range-circle math.
        let d = PhgDirectivity::Undefined;
        assert_eq!(d.to_degrees(), None);
        assert!(d.is_omni());
    }

    #[test]
    fn phg_directivity_all_codes_round_trip() -> TestResult {
        // Every code in the primary table retains its direction and label.
        let expectations: [(u8, PhgDirectivity, Option<u16>, &str); 10] = [
            (0, PhgDirectivity::Omni, None, "omni"),
            (1, PhgDirectivity::NorthEast, Some(45), "NE"),
            (2, PhgDirectivity::East, Some(90), "E"),
            (3, PhgDirectivity::SouthEast, Some(135), "SE"),
            (4, PhgDirectivity::South, Some(180), "S"),
            (5, PhgDirectivity::SouthWest, Some(225), "SW"),
            (6, PhgDirectivity::West, Some(270), "W"),
            (7, PhgDirectivity::NorthWest, Some(315), "NW"),
            (8, PhgDirectivity::North, Some(360), "N"),
            (9, PhgDirectivity::Undefined, None, "undef"),
        ];
        for (code, expected, degrees, label) in expectations {
            let parsed = PhgDirectivity::from_code(code)
                .ok_or_else(|| format!("code {code} did not parse"))?;
            assert_eq!(parsed, expected, "code {code} parsed to wrong variant");
            assert_eq!(parsed.to_degrees(), degrees, "code {code} wrong degrees");
            assert_eq!(format!("{parsed}"), label, "code {code} wrong display");
        }
        Ok(())
    }

    #[test]
    fn phg_directivity_rejects_out_of_range() {
        assert!(PhgDirectivity::from_code(10).is_none());
        assert!(PhgDirectivity::from_code(99).is_none());
        assert!(PhgDirectivity::from_code(u8::MAX).is_none());
    }

    #[test]
    fn phg_height_extends_beyond_9() -> TestResult {
        // Regression guard (APRS 1.0.1 §7 p.28): the height
        // digit may be any ASCII character `'0'` and above, allowing
        // balloons/satellites to encode heights beyond 5 120 ft (the
        // `'9'` cap). Earlier code rejected `':'` via `is_ascii_digit`.
        let ext = AprsDataExtension::from_comment("PHG5:30");
        let phg = ext.phg.ok_or("phg missing")?;
        assert!(
            (phg.height_feet() - 10_240.0).abs() < f64::EPSILON,
            "':' should decode to 10240 ft"
        );
        Ok(())
    }

    #[test]
    fn phg_height_balloon_range() -> TestResult {
        // `';'` (h=11) is 20 480 feet above average terrain.
        let ext = AprsDataExtension::from_comment("PHG3;30");
        let phg = ext.phg.ok_or("phg missing")?;
        assert!((phg.height_feet() - 20_480.0).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn phg_height_rejects_below_zero_ascii() {
        // Bytes below ASCII `'0'` (e.g. ` ` = 32) are not valid PHG
        // height codes and must produce None.
        let ext = AprsDataExtension::from_comment("PHG5 30");
        assert!(ext.phg.is_none(), "space height should be rejected");
    }

    // ---- AprsReportTimestamp::from_wire round-trip ----

    #[test]
    fn aprs_timestamp_parse_dhm_zulu() -> TestResult {
        let ts = AprsReportTimestamp::from_wire("092345z")?;
        assert_eq!(ts, AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?);
        assert_eq!(ts.format(), AprsReportTimestampFormat::DayHourMinuteUtc);
        assert_eq!(ts.to_wire_string(), "092345z");
        Ok(())
    }

    #[test]
    fn aprs_timestamp_parse_dhm_local() -> TestResult {
        let ts = AprsReportTimestamp::from_wire("110000/")?;
        assert_eq!(ts, AprsReportTimestamp::day_hour_minute_local(11, 0, 0)?);
        assert_eq!(ts.format(), AprsReportTimestampFormat::DayHourMinuteLocal);
        assert_eq!(ts.to_wire_string(), "110000/");
        Ok(())
    }

    #[test]
    fn aprs_timestamp_parse_hms() -> TestResult {
        let ts = AprsReportTimestamp::from_wire("234517h")?;
        assert_eq!(ts, AprsReportTimestamp::hour_minute_second_utc(23, 45, 17)?);
        assert_eq!(ts.format(), AprsReportTimestampFormat::HourMinuteSecondUtc);
        assert_eq!(ts.to_wire_string(), "234517h");
        Ok(())
    }

    #[test]
    fn aprs_weather_timestamp_parse_mdhm() -> TestResult {
        let ts = AprsWeatherTimestamp::from_wire("10092345")?;
        assert_eq!(
            ts,
            AprsWeatherTimestamp::month_day_hour_minute_utc(10, 9, 23, 45)?,
        );
        assert_eq!(ts.to_wire_string(), "10092345");
        Ok(())
    }

    #[test]
    fn aprs_timestamp_parse_rejects_malformed() {
        // Wrong length
        assert!(AprsReportTimestamp::from_wire("123456").is_err());
        assert!(AprsReportTimestamp::from_wire("1234567890").is_err());
        // Unknown suffix
        assert!(AprsReportTimestamp::from_wire("123456X").is_err());
        // Non-digit
        assert!(AprsReportTimestamp::from_wire("1A2345z").is_err());
        // Out-of-range day (0 or > 31)
        assert!(AprsReportTimestamp::from_wire("002345z").is_err());
        assert!(AprsReportTimestamp::from_wire("322345z").is_err());
        // Out-of-range hour
        assert!(AprsReportTimestamp::from_wire("092445z").is_err());
        // Out-of-range minute
        assert!(AprsReportTimestamp::from_wire("092360z").is_err());
        // The eight-byte weather form is not a report timestamp.
        assert!(AprsReportTimestamp::from_wire("00091234").is_err());
        assert!(AprsReportTimestamp::from_wire("13091234").is_err());
    }

    #[test]
    fn aprs_timestamp_round_trip_all_variants() -> TestResult {
        // Build each variant, format to wire, parse back, expect identity.
        let cases = [
            AprsReportTimestamp::day_hour_minute_utc(9, 23, 45)?,
            AprsReportTimestamp::day_hour_minute_local(15, 12, 0)?,
            AprsReportTimestamp::hour_minute_second_utc(23, 45, 17)?,
        ];
        for original in cases {
            let wire = original.to_wire_string();
            let parsed = AprsReportTimestamp::from_wire(&wire)?;
            assert_eq!(parsed, original, "round-trip mismatch for {original:?}");
        }
        Ok(())
    }

    #[test]
    fn aprs_timestamp_constructors_reject_invalid_components() {
        assert!(AprsReportTimestamp::day_hour_minute_utc(0, 0, 0).is_err());
        assert!(AprsReportTimestamp::day_hour_minute_local(32, 0, 0).is_err());
        assert!(AprsReportTimestamp::hour_minute_second_utc(24, 0, 0).is_err());
        assert!(AprsReportTimestamp::hour_minute_second_utc(0, 60, 0).is_err());
        assert!(AprsReportTimestamp::hour_minute_second_utc(0, 0, 60).is_err());
    }

    #[test]
    fn aprs_weather_timestamp_rejects_invalid_components() {
        assert!(AprsWeatherTimestamp::month_day_hour_minute_utc(0, 1, 0, 0).is_err());
        assert!(AprsWeatherTimestamp::month_day_hour_minute_utc(13, 1, 0, 0).is_err());
        assert!(AprsWeatherTimestamp::month_day_hour_minute_utc(2, 30, 0, 0).is_err());
        assert!(AprsWeatherTimestamp::month_day_hour_minute_utc(4, 31, 0, 0).is_err());
        assert!(AprsWeatherTimestamp::from_wire("0101123").is_err());
        assert!(AprsWeatherTimestamp::from_wire("010112345").is_err());
        assert!(AprsWeatherTimestamp::from_wire("01AA1234").is_err());
    }

    #[test]
    fn phg_height_separates_protocol_and_integer_representation() -> TestResult {
        let phg = AprsDataExtension::from_comment("PHG5M30")
            .phg()
            .ok_or("PHG height")?;
        assert_eq!(phg.height_exponent(), 29);
        assert!((phg.height_feet() - 5_368_709_120.0).abs() < f64::EPSILON);
        assert_eq!(phg.height_feet_u32(), None);
        assert!(
            Phg::from_bytes([b'5', 127, b'3', b'0'])?
                .height_feet()
                .is_finite()
        );
        assert!(Phg::from_bytes([b'5', 128, b'3', b'0']).is_err());
        Ok(())
    }

    #[test]
    fn parse_extensions_altitude() {
        let ext = AprsDataExtension::from_comment("some comment /A=001234 more");
        assert_eq!(ext.altitude_ft, Some(1234));
    }

    #[test]
    fn parse_extensions_altitude_negative() {
        let ext = AprsDataExtension::from_comment("/A=-00100");
        assert_eq!(ext.altitude_ft, Some(-100));
    }

    #[test]
    fn parse_extensions_altitude_zeros() {
        let ext = AprsDataExtension::from_comment("/A=000000");
        assert_eq!(ext.altitude_ft, Some(0));
    }

    #[test]
    fn parse_extensions_dao_human_readable() -> TestResult {
        let dao = AprsDataExtension::from_comment("text !W23! more")
            .dao()
            .ok_or("dao missing")?;
        assert_eq!(dao.to_bytes(), *b"!W23!");
        assert_eq!(dao.precision(), DaoPrecision::Decimal);
        assert!(
            (dao.latitude_offset_degrees().ok_or("latitude precision")? - 2.0 / 60_000.0).abs()
                < 1e-15
        );
        assert!(
            (dao.longitude_offset_degrees()
                .ok_or("longitude precision")?
                - 3.0 / 60_000.0)
                .abs()
                < 1e-15
        );
        Ok(())
    }

    #[test]
    fn parse_extensions_dao_base91() -> TestResult {
        let dao = AprsDataExtension::from_comment("!wAb!")
            .dao()
            .ok_or("dao missing")?;
        assert_eq!(dao.to_bytes(), *b"!wAb!");
        assert_eq!(dao.precision(), DaoPrecision::Base91);
        assert!(
            (dao.latitude_offset_degrees().ok_or("latitude precision")? - 32.0 / (91.0 * 6_000.0))
                .abs()
                < 1e-15
        );
        assert!(
            (dao.longitude_offset_degrees()
                .ok_or("longitude precision")?
                - 65.0 / (91.0 * 6_000.0))
                .abs()
                < 1e-15
        );
        Ok(())
    }

    #[test]
    fn parse_extensions_combined() -> TestResult {
        let ext = AprsDataExtension::from_comment("088/036PHG5132/A=001234");
        let motion = ext.course_speed().ok_or("course/speed")?;
        assert_eq!(motion.course().as_degrees(), 88);
        assert!((motion.speed().as_knots() - 36.0).abs() < f64::EPSILON);
        assert!(ext.phg.is_some());
        assert_eq!(ext.altitude_ft, Some(1234));
        Ok(())
    }

    #[test]
    fn parse_extensions_empty() {
        let ext = AprsDataExtension::from_comment("");
        assert!(ext.course_speed.is_none());
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
    }

    // ---- TelemetryDefinition tests ----

    #[test]
    fn telemetry_definition_parm() -> TestResult {
        let def =
            TelemetryDefinition::from_text("PARM.Volts,Temp,Humid,Wind,Rain,Door,Lit,Hot,,,,,")?
                .ok_or("missing")?;
        let TelemetryDefinition::Parameters(p) = def else {
            return Err("expected Parameters".into());
        };
        assert_eq!(p.as_slice().first().map(String::as_str), Some("Volts"));
        assert_eq!(p.as_slice().get(4).map(String::as_str), Some("Rain"));
        assert_eq!(p.as_slice().get(5).map(String::as_str), Some("Door"));
        assert_eq!(p.as_slice().get(7).map(String::as_str), Some("Hot"));
        assert!(
            TelemetryDefinition::from_text("PARM.Volts,Temp,Humid,Wind,Rain,Door,Light,Heat")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn telemetry_definition_unit() -> TestResult {
        let def = TelemetryDefinition::from_text("UNIT.Vdc,C,%,mph,in,open,lit,on,,,,,")?
            .ok_or("missing")?;
        let TelemetryDefinition::Units(p) = def else {
            return Err("expected Units".into());
        };
        assert_eq!(p.as_slice().get(1).map(String::as_str), Some("C"));
        Ok(())
    }

    #[test]
    fn telemetry_definition_eqns() -> TestResult {
        let def = TelemetryDefinition::from_text("EQNS.0,0.1,0,0,0.5,0,0,1,0,0,2,0,0,3,0")?
            .ok_or("missing")?;
        let TelemetryDefinition::Equations(eqs) = def else {
            return Err("expected Equations".into());
        };
        assert_eq!(
            eqs.as_slice(),
            &[
                0.0, 0.1, 0.0, 0.0, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 2.0, 0.0, 0.0, 3.0, 0.0
            ]
        );
        Ok(())
    }

    #[test]
    fn telemetry_definition_eqns_strict_rejects_malformed_slot() {
        for text in [
            "EQNS.0,0.1,0,0,hello,0,0,1,0,0,2,0,0,3,0",
            "EQNS.NaN",
            "EQNS.inf",
            "EQNS.1,,2",
        ] {
            assert!(
                TelemetryDefinition::from_text(text).is_err(),
                "accepted {text}"
            );
        }
    }

    #[test]
    fn telemetry_definition_eqns_strict_rejects_missing_slot() -> TestResult {
        // A short input retains its exact prefix, including a partial triple.
        let def = TelemetryDefinition::from_text("EQNS.0,0.1,0,0,0.5,0,2")?.ok_or("missing")?;
        let TelemetryDefinition::Equations(eqs) = def else {
            return Err("expected Equations".into());
        };
        assert_eq!(eqs.as_slice(), &[0.0, 0.1, 0.0, 0.0, 0.5, 0.0, 2.0]);
        Ok(())
    }

    #[test]
    fn telemetry_definition_bits() -> TestResult {
        let def = TelemetryDefinition::from_text("BITS.11111111,WX station telemetry")?
            .ok_or("missing")?;
        let TelemetryDefinition::Bits { bits, title } = def else {
            return Err("expected Bits".into());
        };
        assert_eq!(bits.bits(), 255);
        assert_eq!(title.as_str(), "WX station telemetry");
        Ok(())
    }

    #[test]
    fn telemetry_definition_bits_exactly_eight() -> TestResult {
        // APRS 1.0.1 §13 p.70: the bit-sense field is exactly 8 binary
        // digits. A correct 8-digit mask parses.
        let def = TelemetryDefinition::from_text("BITS.10110000,Title")?.ok_or("expected Bits")?;
        let TelemetryDefinition::Bits { bits, title } = def else {
            return Err("expected Bits".into());
        };
        assert_eq!(bits.bits(), 0b1011_0000);
        assert_eq!(title.as_str(), "Title");
        Ok(())
    }

    #[test]
    fn telemetry_definition_bits_rejects_empty_mask() {
        // A bare "BITS." has no active-bit mask and must be rejected.
        assert!(TelemetryDefinition::from_text("BITS.").is_err());
    }

    #[test]
    fn telemetry_definition_bits_rejects_short_mask() {
        // Fewer than 8 digits is malformed.
        assert!(TelemetryDefinition::from_text("BITS.101").is_err());
    }

    #[test]
    fn telemetry_definition_bits_rejects_non_binary() {
        // Right length but a non-binary digit is malformed.
        assert!(TelemetryDefinition::from_text("BITS.10110002,Title").is_err());
    }

    #[test]
    fn telemetry_definition_unknown_returns_none() {
        assert_eq!(TelemetryDefinition::from_text("hello world"), Ok(None));
    }

    #[test]
    fn telemetry_definitions_enforce_bounded_positional_records() -> TestResult {
        assert!(TelemetryDefinition::from_text("PARM.a,b,c,d,e,f,g,h,i,j,k,l,m,n").is_err());
        assert!(TelemetryDefinition::from_text("EQNS.0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0").is_err());
        let labels = TelemetryDefinition::from_text("PARM.A,,C")?.ok_or("labels")?;
        let TelemetryDefinition::Parameters(labels) = labels else {
            return Err("parameters".into());
        };
        assert_eq!(labels.as_slice(), &["A", "", "C"]);
        for locator in ["aa00", "AA00XX", "Aa00bX", "IO91SX"] {
            assert_eq!(MaidenheadGrid::new(locator)?.as_str(), locator);
        }
        let owned = String::from("Aa00bX");
        let allocation = owned.as_ptr();
        let grid = MaidenheadGrid::try_from(owned)?;
        assert_eq!(grid.as_str().as_ptr(), allocation);
        assert!(MaidenheadGrid::try_from(String::from("ZZ00")).is_err());
        Ok(())
    }

    #[test]
    fn dao_null_unknown_datum_and_malformed_candidates_are_distinct() -> TestResult {
        let null = DaoExtension::from_bytes(*b"!W  !")?;
        assert_eq!(null.datum().as_byte(), b'W');
        assert_eq!(null.precision(), DaoPrecision::None);
        assert_eq!(null.latitude_offset_degrees(), None);
        let unknown = DaoExtension::from_bytes(*b"!123!")?;
        assert_eq!(unknown.precision(), DaoPrecision::Unspecified);
        assert_eq!(unknown.to_bytes(), *b"!123!");
        assert_eq!(unknown.longitude_offset_degrees(), None);
        for bytes in [*b"!W2 !", *b"!WAB!", *b"!w|b!", *b"xW23!"] {
            assert!(
                DaoExtension::from_bytes(bytes).is_err(),
                "accepted {bytes:?}"
            );
        }
        assert_eq!(
            AprsDataExtension::from_comment("!WAB! !W23!")
                .dao()
                .ok_or("later DAO")?
                .to_bytes(),
            *b"!W23!"
        );
        Ok(())
    }
}
