//! Strict, lossless parser for TH-D75 NMEA 0183 GPS log `.nme` files.
//!
//! The radio records one checksum-protected NMEA sentence per line under
//! `/KENWOOD/TH-D75/GPS_LOG/`. RMC and GGA sentences are decoded into typed
//! navigation records. Every other checksum-valid sentence is retained
//! verbatim as [`GpsLogSentence::Unmodeled`]. A malformed line is never reduced
//! to a counter: [`RejectedNmeaLine`] retains its line number, exact ASCII text,
//! and a typed [`NmeaLineError`].
//!
//! The parser accepts CRLF, LF, and a final line without a terminator. Sentence
//! text must otherwise use printable 7-bit ASCII and the NMEA `*HH` checksum
//! suffix must be the exact end of the line.

use std::fmt;

use super::{SdCardError, decode_utf8};

/// Maximum NMEA sentence length without its terminating CRLF bytes.
///
/// NMEA 0183 limits a complete sentence to 82 characters including CRLF.
pub const MAX_SENTENCE_BYTES: usize = 80;

/// UTC time carried by an NMEA sentence.
///
/// The retained spelling is `HHMMSS` with an optional, non-empty decimal
/// fraction. Hours are `00..=23`, minutes are `00..=59`, and seconds are
/// `00..=60` so a UTC leap second remains representable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UtcTime {
    text: String,
    hour: u8,
    minute: u8,
    second: u8,
    fractional_digits: Option<String>,
}

impl UtcTime {
    /// Return the exact validated NMEA field spelling.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Return the UTC hour in the range `0..=23`.
    #[must_use]
    pub const fn hour(&self) -> u8 {
        self.hour
    }

    /// Return the UTC minute in the range `0..=59`.
    #[must_use]
    pub const fn minute(&self) -> u8 {
        self.minute
    }

    /// Return the UTC second in the range `0..=60`.
    #[must_use]
    pub const fn second(&self) -> u8 {
        self.second
    }

    /// Return the exact digits after the decimal point, when present.
    #[must_use]
    pub fn fractional_digits(&self) -> Option<&str> {
        self.fractional_digits.as_deref()
    }

    fn parse(value: &str) -> Result<Self, &'static str> {
        let (whole, fractional_digits) = match value.split_once('.') {
            Some((whole, fraction)) => {
                if fraction.is_empty() || fraction.contains('.') || !is_ascii_digits(fraction) {
                    return Err("fractional seconds must contain one or more decimal digits");
                }
                (whole, Some(fraction.to_owned()))
            }
            None => (value, None),
        };
        if whole.len() != 6 || !is_ascii_digits(whole) {
            return Err("UTC time must use HHMMSS with an optional decimal fraction");
        }

        let hour = parse_two_digits(whole, 0)?;
        let minute = parse_two_digits(whole, 2)?;
        let second = parse_two_digits(whole, 4)?;
        if hour > 23 {
            return Err("UTC hour must be in 00..=23");
        }
        if minute > 59 {
            return Err("UTC minute must be in 00..=59");
        }
        if second > 60 {
            return Err("UTC second must be in 00..=60");
        }

        Ok(Self {
            text: value.to_owned(),
            hour,
            minute,
            second,
            fractional_digits,
        })
    }
}

impl fmt::Display for UtcTime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// UTC calendar date carried by an RMC sentence.
///
/// NMEA carries a two-digit year and does not identify its century. The exact
/// `DDMMYY` spelling is therefore retained rather than inventing a century.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UtcDate {
    text: String,
    day: u8,
    month: u8,
    two_digit_year: u8,
}

impl UtcDate {
    /// Return the exact validated `DDMMYY` field spelling.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Return the day of month.
    #[must_use]
    pub const fn day(&self) -> u8 {
        self.day
    }

    /// Return the month in the range `1..=12`.
    #[must_use]
    pub const fn month(&self) -> u8 {
        self.month
    }

    /// Return the two-digit year exactly as represented by NMEA.
    #[must_use]
    pub const fn two_digit_year(&self) -> u8 {
        self.two_digit_year
    }

    fn parse(value: &str) -> Result<Self, &'static str> {
        if value.len() != 6 || !is_ascii_digits(value) {
            return Err("UTC date must contain exactly six DDMMYY digits");
        }
        let day = parse_two_digits(value, 0)?;
        let month = parse_two_digits(value, 2)?;
        let two_digit_year = parse_two_digits(value, 4)?;
        let maximum_day =
            days_in_month(month, two_digit_year).ok_or("UTC date month must be in 01..=12")?;
        if day == 0 || day > maximum_day {
            return Err("UTC date day is not valid for its month");
        }
        Ok(Self {
            text: value.to_owned(),
            day,
            month,
            two_digit_year,
        })
    }
}

impl fmt::Display for UtcDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// RMC navigation status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmcStatus {
    /// `A`: the navigation data is active and valid.
    Active,
    /// `V`: the navigation data is void.
    Void,
}

impl RmcStatus {
    /// Return the exact NMEA status character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Active => 'A',
            Self::Void => 'V',
        }
    }

    /// Report whether the sentence declares an active fix.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Optional RMC mode indicator introduced by NMEA 0183 version 2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmcMode {
    /// `A`: autonomous fix.
    Autonomous,
    /// `D`: differential fix.
    Differential,
    /// `E`: estimated or dead-reckoned fix.
    Estimated,
    /// `M`: manually entered position.
    Manual,
    /// `N`: data is not valid.
    Invalid,
    /// `S`: simulator mode.
    Simulator,
    /// `R`: fixed real-time kinematic solution.
    RtkFixed,
    /// `F`: floating real-time kinematic solution.
    RtkFloat,
    /// `P`: precise positioning service solution.
    Precise,
}

impl RmcMode {
    /// Return the exact NMEA mode character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Autonomous => 'A',
            Self::Differential => 'D',
            Self::Estimated => 'E',
            Self::Manual => 'M',
            Self::Invalid => 'N',
            Self::Simulator => 'S',
            Self::RtkFixed => 'R',
            Self::RtkFloat => 'F',
            Self::Precise => 'P',
        }
    }
}

/// Optional RMC navigational status introduced by NMEA 0183 version 4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmcNavigationStatus {
    /// `S`: navigation is safe.
    Safe,
    /// `C`: exercise caution.
    Caution,
    /// `U`: navigation is unsafe.
    Unsafe,
    /// `V`: navigational status is not valid.
    Invalid,
}

impl RmcNavigationStatus {
    /// Return the exact NMEA navigational-status character.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Safe => 'S',
            Self::Caution => 'C',
            Self::Unsafe => 'U',
            Self::Invalid => 'V',
        }
    }
}

/// GGA fix-quality indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GgaFixQuality {
    /// `0`: no valid fix.
    Invalid,
    /// `1`: autonomous GNSS fix.
    Autonomous,
    /// `2`: differential GNSS fix.
    Differential,
    /// `3`: precise positioning service fix.
    PrecisePositioning,
    /// `4`: fixed real-time kinematic solution.
    RtkFixed,
    /// `5`: floating real-time kinematic solution.
    RtkFloat,
    /// `6`: estimated or dead-reckoned solution.
    Estimated,
    /// `7`: manually entered position.
    Manual,
    /// `8`: simulator solution.
    Simulator,
}

impl GgaFixQuality {
    /// Return the exact NMEA quality digit.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Invalid => 0,
            Self::Autonomous => 1,
            Self::Differential => 2,
            Self::PrecisePositioning => 3,
            Self::RtkFixed => 4,
            Self::RtkFloat => 5,
            Self::Estimated => 6,
            Self::Manual => 7,
            Self::Simulator => 8,
        }
    }

    /// Report whether the quality indicator represents a position fix.
    #[must_use]
    pub const fn has_fix(self) -> bool {
        !matches!(self, Self::Invalid)
    }

    fn parse(value: &str) -> Result<Self, &'static str> {
        match value {
            "0" => Ok(Self::Invalid),
            "1" => Ok(Self::Autonomous),
            "2" => Ok(Self::Differential),
            "3" => Ok(Self::PrecisePositioning),
            "4" => Ok(Self::RtkFixed),
            "5" => Ok(Self::RtkFloat),
            "6" => Ok(Self::Estimated),
            "7" => Ok(Self::Manual),
            "8" => Ok(Self::Simulator),
            _ => Err("GGA quality must be one digit in 0..=8"),
        }
    }
}

/// Latitude and longitude in decimal degrees.
///
/// Latitude is always finite and in `-90.0..=90.0`; longitude is always
/// finite and in `-180.0..=180.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLon {
    latitude_degrees: f64,
    longitude_degrees: f64,
}

impl LatLon {
    /// Return latitude in signed decimal degrees.
    #[must_use]
    pub const fn latitude_degrees(self) -> f64 {
        self.latitude_degrees
    }

    /// Return longitude in signed decimal degrees.
    #[must_use]
    pub const fn longitude_degrees(self) -> f64 {
        self.longitude_degrees
    }
}

/// A checksum-validated NMEA sentence in its original spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawNmeaSentence {
    text: String,
    payload: String,
    identifier: String,
    delimiter: char,
    checksum: u8,
}

impl RawNmeaSentence {
    /// Return the complete sentence, including its delimiter and checksum.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        self.text.as_str()
    }

    /// Return the checksum-covered payload without the leading delimiter.
    #[must_use]
    pub const fn payload(&self) -> &str {
        self.payload.as_str()
    }

    /// Return the sentence identifier without its leading delimiter.
    #[must_use]
    pub const fn identifier(&self) -> &str {
        self.identifier.as_str()
    }

    /// Return the leading `$` or `!` delimiter.
    #[must_use]
    pub const fn delimiter(&self) -> char {
        self.delimiter
    }

    /// Return the declared and verified checksum byte.
    #[must_use]
    pub const fn checksum(&self) -> u8 {
        self.checksum
    }

    /// Return a standard two-character talker identifier when this sentence
    /// uses the five-character `ttXXX` identifier shape.
    #[must_use]
    pub fn talker_id(&self) -> Option<&str> {
        if self.identifier.len() == 5 {
            self.identifier.get(..2)
        } else {
            None
        }
    }

    /// Return a standard three-character formatter when this sentence uses
    /// the five-character `ttXXX` identifier shape.
    #[must_use]
    pub fn formatter(&self) -> Option<&str> {
        if self.identifier.len() == 5 {
            self.identifier.get(2..)
        } else {
            None
        }
    }
}

impl fmt::Display for RawNmeaSentence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One parsed RMC (Recommended Minimum Navigation Information) sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct RmcFix {
    raw: RawNmeaSentence,
    time: Option<UtcTime>,
    status: RmcStatus,
    position: Option<LatLon>,
    speed_knots: Option<f64>,
    course_degrees: Option<f64>,
    date: Option<UtcDate>,
    magnetic_variation_degrees: Option<f64>,
    mode: Option<RmcMode>,
    navigation_status: Option<RmcNavigationStatus>,
}

impl RmcFix {
    /// Return the exact checksum-validated source sentence.
    #[must_use]
    pub const fn as_raw(&self) -> &RawNmeaSentence {
        &self.raw
    }

    /// Return UTC time, or `None` when a void sentence leaves it empty.
    #[must_use]
    pub const fn utc_time(&self) -> Option<&UtcTime> {
        self.time.as_ref()
    }

    /// Return the RMC navigation status.
    #[must_use]
    pub const fn status(&self) -> RmcStatus {
        self.status
    }

    /// Return the position when all four NMEA coordinate fields are present.
    #[must_use]
    pub const fn position(&self) -> Option<LatLon> {
        self.position
    }

    /// Return speed over ground in knots when present.
    #[must_use]
    pub const fn speed_knots(&self) -> Option<f64> {
        self.speed_knots
    }

    /// Return course over ground in true degrees when present.
    #[must_use]
    pub const fn course_degrees(&self) -> Option<f64> {
        self.course_degrees
    }

    /// Return the UTC date, or `None` when a void sentence leaves it empty.
    #[must_use]
    pub const fn utc_date(&self) -> Option<&UtcDate> {
        self.date.as_ref()
    }

    /// Return signed magnetic variation in degrees when present.
    ///
    /// East is positive and west is negative.
    #[must_use]
    pub const fn magnetic_variation_degrees(&self) -> Option<f64> {
        self.magnetic_variation_degrees
    }

    /// Return the optional NMEA 2.3 mode indicator.
    #[must_use]
    pub const fn mode(&self) -> Option<RmcMode> {
        self.mode
    }

    /// Return the optional NMEA 4.1 navigational-status indicator.
    #[must_use]
    pub const fn navigation_status(&self) -> Option<RmcNavigationStatus> {
        self.navigation_status
    }
}

/// One parsed GGA (Global Positioning System Fix Data) sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct GgaFix {
    raw: RawNmeaSentence,
    time: Option<UtcTime>,
    position: Option<LatLon>,
    quality: GgaFixQuality,
    satellites: Option<u8>,
    hdop: Option<f64>,
    altitude_meters: Option<f64>,
    geoid_separation_meters: Option<f64>,
    differential_age_seconds: Option<f64>,
    differential_station_id: Option<u16>,
}

impl GgaFix {
    /// Return the exact checksum-validated source sentence.
    #[must_use]
    pub const fn as_raw(&self) -> &RawNmeaSentence {
        &self.raw
    }

    /// Return UTC time, or `None` when a no-fix sentence leaves it empty.
    #[must_use]
    pub const fn utc_time(&self) -> Option<&UtcTime> {
        self.time.as_ref()
    }

    /// Return the position when all four NMEA coordinate fields are present.
    #[must_use]
    pub const fn position(&self) -> Option<LatLon> {
        self.position
    }

    /// Return the typed GGA fix-quality indicator.
    #[must_use]
    pub const fn quality(&self) -> GgaFixQuality {
        self.quality
    }

    /// Return the number of satellites used when present.
    #[must_use]
    pub const fn satellites(&self) -> Option<u8> {
        self.satellites
    }

    /// Return horizontal dilution of precision when present.
    #[must_use]
    pub const fn hdop(&self) -> Option<f64> {
        self.hdop
    }

    /// Return orthometric altitude above mean sea level in meters.
    #[must_use]
    pub const fn altitude_meters(&self) -> Option<f64> {
        self.altitude_meters
    }

    /// Return geoid separation in meters when present.
    #[must_use]
    pub const fn geoid_separation_meters(&self) -> Option<f64> {
        self.geoid_separation_meters
    }

    /// Return age of differential corrections in seconds when present.
    #[must_use]
    pub const fn differential_age_seconds(&self) -> Option<f64> {
        self.differential_age_seconds
    }

    /// Return the differential reference-station identifier when present.
    #[must_use]
    pub const fn differential_station_id(&self) -> Option<u16> {
        self.differential_station_id
    }
}

/// A checksum-valid sentence from a TH-D75 GPS log.
///
/// This name is intentionally distinct from `crate::types::NmeaSentence`,
/// which represents the radio's sentence-enable bit flags rather than parsed
/// wire data.
#[derive(Debug, Clone, PartialEq)]
pub enum GpsLogSentence {
    /// Recommended minimum navigation information.
    Rmc(RmcFix),
    /// Position fix and quality information.
    Gga(GgaFix),
    /// A checksum-valid sentence whose formatter is not modeled here.
    Unmodeled(RawNmeaSentence),
}

impl GpsLogSentence {
    /// Return the exact checksum-validated source sentence.
    #[must_use]
    pub const fn as_raw(&self) -> &RawNmeaSentence {
        match self {
            Self::Rmc(fix) => fix.as_raw(),
            Self::Gga(fix) => fix.as_raw(),
            Self::Unmodeled(raw) => raw,
        }
    }
}

/// Reason one GPS-log line was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NmeaLineError {
    /// The file contains an empty record between line terminators.
    EmptyLine,
    /// A control character appears inside the sentence text.
    InvalidCharacter {
        /// Zero-based byte offset in the line.
        offset: usize,
        /// Exact invalid ASCII byte.
        byte: u8,
    },
    /// The line exceeds the NMEA sentence limit.
    TooLong {
        /// Maximum bytes allowed without CRLF.
        maximum: usize,
        /// Actual line length in bytes.
        actual: usize,
    },
    /// The line does not begin with `$` or `!`.
    MissingStartDelimiter,
    /// The line has no checksum delimiter.
    MissingChecksumDelimiter,
    /// More than one `*` checksum delimiter appears in the line.
    MultipleChecksumDelimiters,
    /// The checksum suffix is not exactly two characters.
    ChecksumSuffixLength {
        /// Actual number of bytes after `*`.
        actual: usize,
    },
    /// The checksum suffix is not two hexadecimal digits.
    InvalidChecksum {
        /// Exact suffix found after `*`.
        value: String,
    },
    /// The declared checksum does not equal the computed XOR.
    ChecksumMismatch {
        /// Checksum declared by the sentence.
        declared: u8,
        /// XOR computed from the sentence payload.
        computed: u8,
    },
    /// The sentence identifier is empty or contains invalid characters.
    InvalidIdentifier {
        /// Exact identifier found before the first comma.
        value: String,
    },
    /// A modeled sentence has the wrong number of comma-separated fields.
    FieldCount {
        /// Exact sentence identifier.
        sentence_id: String,
        /// Human-readable accepted field count.
        expected: &'static str,
        /// Actual field count, including the sentence identifier.
        actual: usize,
    },
    /// One field in a modeled sentence violates its grammar or domain.
    InvalidField {
        /// Exact sentence identifier.
        sentence_id: String,
        /// Standard field name.
        field: &'static str,
        /// Exact field spelling from the sentence.
        value: String,
        /// Precise validation failure.
        reason: &'static str,
    },
    /// Individually valid fields contradict the sentence's status or quality.
    InconsistentFields {
        /// Exact sentence identifier.
        sentence_id: String,
        /// Precise relationship that was violated.
        reason: &'static str,
    },
}

impl fmt::Display for NmeaLineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyLine => formatter.write_str("empty GPS-log line"),
            Self::InvalidCharacter { offset, byte } => write!(
                formatter,
                "invalid ASCII control byte 0x{byte:02X} at line offset {offset}"
            ),
            Self::TooLong { maximum, actual } => write!(
                formatter,
                "NMEA sentence is {actual} bytes; maximum without CRLF is {maximum}"
            ),
            Self::MissingStartDelimiter => {
                formatter.write_str("NMEA sentence must begin with '$' or '!'")
            }
            Self::MissingChecksumDelimiter => {
                formatter.write_str("NMEA sentence has no '*' checksum delimiter")
            }
            Self::MultipleChecksumDelimiters => {
                formatter.write_str("NMEA sentence contains more than one '*' delimiter")
            }
            Self::ChecksumSuffixLength { actual } => write!(
                formatter,
                "NMEA checksum suffix must contain exactly 2 bytes, got {actual}"
            ),
            Self::InvalidChecksum { value } => {
                write!(formatter, "NMEA checksum {value:?} is not hexadecimal")
            }
            Self::ChecksumMismatch { declared, computed } => write!(
                formatter,
                "NMEA checksum mismatch: declared {declared:02X}, computed {computed:02X}"
            ),
            Self::InvalidIdentifier { value } => write!(
                formatter,
                "NMEA sentence identifier {value:?} must be non-empty uppercase ASCII alphanumeric text"
            ),
            Self::FieldCount {
                sentence_id,
                expected,
                actual,
            } => write!(
                formatter,
                "{sentence_id} requires {expected} comma-separated fields, got {actual}"
            ),
            Self::InvalidField {
                sentence_id,
                field,
                value,
                reason,
            } => write!(
                formatter,
                "{sentence_id} field {field} has invalid value {value:?}: {reason}"
            ),
            Self::InconsistentFields {
                sentence_id,
                reason,
            } => write!(formatter, "{sentence_id} fields are inconsistent: {reason}"),
        }
    }
}

impl std::error::Error for NmeaLineError {}

/// One rejected GPS-log line with exact source context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedNmeaLine {
    line_number: usize,
    text: String,
    error: NmeaLineError,
}

impl RejectedNmeaLine {
    /// Return the one-based source line number.
    #[must_use]
    pub const fn line_number(&self) -> usize {
        self.line_number
    }

    /// Return the exact ASCII line without its line terminator.
    #[must_use]
    pub const fn text(&self) -> &str {
        self.text.as_str()
    }

    /// Return the typed rejection reason.
    #[must_use]
    pub const fn error(&self) -> &NmeaLineError {
        &self.error
    }
}

impl fmt::Display for RejectedNmeaLine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GPS-log line {}: {}",
            self.line_number, self.error
        )
    }
}

impl std::error::Error for RejectedNmeaLine {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// One ordered record from a parsed GPS log.
#[derive(Debug, Clone, PartialEq)]
pub enum GpsLogRecord {
    /// A checksum-valid sentence.
    Sentence(GpsLogSentence),
    /// A rejected line with exact diagnostic context.
    Rejected(RejectedNmeaLine),
}

/// A complete TH-D75 GPS log with every non-terminator record preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct GpsLog {
    records: Vec<GpsLogRecord>,
}

impl GpsLog {
    /// Return all parsed and rejected records in file order.
    #[must_use]
    pub fn records(&self) -> &[GpsLogRecord] {
        &self.records
    }

    /// Iterate over checksum-valid sentences in file order.
    pub fn sentences(&self) -> impl Iterator<Item = &GpsLogSentence> {
        self.records.iter().filter_map(|record| match record {
            GpsLogRecord::Sentence(sentence) => Some(sentence),
            GpsLogRecord::Rejected(_) => None,
        })
    }

    /// Iterate over rejected lines in file order.
    pub fn diagnostics(&self) -> impl Iterator<Item = &RejectedNmeaLine> {
        self.records.iter().filter_map(|record| match record {
            GpsLogRecord::Rejected(diagnostic) => Some(diagnostic),
            GpsLogRecord::Sentence(_) => None,
        })
    }

    /// Return the number of rejected lines.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.diagnostics().count()
    }

    /// Iterate over parsed RMC fixes in file order.
    pub fn rmc_fixes(&self) -> impl Iterator<Item = &RmcFix> {
        self.sentences().filter_map(|sentence| match sentence {
            GpsLogSentence::Rmc(fix) => Some(fix),
            GpsLogSentence::Gga(_) | GpsLogSentence::Unmodeled(_) => None,
        })
    }

    /// Iterate over parsed GGA fixes in file order.
    pub fn gga_fixes(&self) -> impl Iterator<Item = &GgaFix> {
        self.sentences().filter_map(|sentence| match sentence {
            GpsLogSentence::Gga(fix) => Some(fix),
            GpsLogSentence::Rmc(_) | GpsLogSentence::Unmodeled(_) => None,
        })
    }

    /// Iterate over active RMC fixes in file order.
    pub fn valid_fixes(&self) -> impl Iterator<Item = &RmcFix> {
        self.rmc_fixes().filter(|fix| fix.status().is_active())
    }
}

/// Parse a TH-D75 NMEA GPS log from raw bytes.
///
/// A malformed sentence becomes an ordered [`GpsLogRecord::Rejected`] record;
/// it does not abort the remaining file or disappear into a counter.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] for an empty file,
/// [`SdCardError::InvalidAscii`] for the first byte outside 7-bit ASCII, or
/// [`SdCardError::InvalidUtf8`] if ASCII decoding unexpectedly fails.
pub fn parse(data: &[u8]) -> Result<GpsLog, SdCardError> {
    if data.is_empty() {
        return Err(SdCardError::FileTooSmall {
            expected: 1,
            actual: 0,
        });
    }
    if let Some((offset, byte)) = data
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| !byte.is_ascii())
    {
        return Err(SdCardError::InvalidAscii {
            file_type: "GPS NMEA log",
            offset,
            byte,
        });
    }
    let text = decode_utf8(data, "GPS NMEA log")?;
    let records = text
        .split_terminator('\n')
        .enumerate()
        .map(|(line_index, wire_line)| {
            let line = wire_line.strip_suffix('\r').unwrap_or(wire_line);
            parse_record(line_index + 1, line)
        })
        .collect();
    Ok(GpsLog { records })
}

fn parse_record(line_number: usize, line: &str) -> GpsLogRecord {
    match parse_raw_sentence(line).and_then(parse_modeled_sentence) {
        Ok(sentence) => GpsLogRecord::Sentence(sentence),
        Err(error) => GpsLogRecord::Rejected(RejectedNmeaLine {
            line_number,
            text: line.to_owned(),
            error,
        }),
    }
}

fn parse_raw_sentence(line: &str) -> Result<RawNmeaSentence, NmeaLineError> {
    if line.is_empty() {
        return Err(NmeaLineError::EmptyLine);
    }
    if line.len() > MAX_SENTENCE_BYTES {
        return Err(NmeaLineError::TooLong {
            maximum: MAX_SENTENCE_BYTES,
            actual: line.len(),
        });
    }
    if let Some((offset, byte)) = line
        .bytes()
        .enumerate()
        .find(|(_, byte)| !(0x20..=0x7E).contains(byte))
    {
        return Err(NmeaLineError::InvalidCharacter { offset, byte });
    }

    let Some(delimiter) = line.chars().next() else {
        return Err(NmeaLineError::EmptyLine);
    };
    if !matches!(delimiter, '$' | '!') {
        return Err(NmeaLineError::MissingStartDelimiter);
    }

    let Some((framed_payload, suffix)) = line.split_once('*') else {
        return Err(NmeaLineError::MissingChecksumDelimiter);
    };
    if suffix.contains('*') {
        return Err(NmeaLineError::MultipleChecksumDelimiters);
    }
    if suffix.len() != 2 {
        return Err(NmeaLineError::ChecksumSuffixLength {
            actual: suffix.len(),
        });
    }
    if !suffix
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
    {
        return Err(NmeaLineError::InvalidChecksum {
            value: suffix.to_owned(),
        });
    }
    let declared = u8::from_str_radix(suffix, 16).map_err(|_| NmeaLineError::InvalidChecksum {
        value: suffix.to_owned(),
    })?;
    let Some(payload) = framed_payload.get(1..) else {
        return Err(NmeaLineError::InvalidIdentifier {
            value: String::new(),
        });
    };
    let computed = payload.bytes().fold(0_u8, |checksum, byte| checksum ^ byte);
    if declared != computed {
        return Err(NmeaLineError::ChecksumMismatch { declared, computed });
    }

    let identifier = payload
        .split_once(',')
        .map_or(payload, |(identifier, _)| identifier);
    if identifier.is_empty()
        || !identifier
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    {
        return Err(NmeaLineError::InvalidIdentifier {
            value: identifier.to_owned(),
        });
    }

    Ok(RawNmeaSentence {
        text: line.to_owned(),
        payload: payload.to_owned(),
        identifier: identifier.to_owned(),
        delimiter,
        checksum: declared,
    })
}

fn parse_modeled_sentence(raw: RawNmeaSentence) -> Result<GpsLogSentence, NmeaLineError> {
    let formatter = raw.formatter();
    if raw.delimiter() != '$' {
        return Ok(GpsLogSentence::Unmodeled(raw));
    }
    match formatter {
        Some("RMC") => parse_rmc(&raw).map(GpsLogSentence::Rmc),
        Some("GGA") => parse_gga(&raw).map(GpsLogSentence::Gga),
        _ => Ok(GpsLogSentence::Unmodeled(raw)),
    }
}

fn parse_rmc(raw: &RawNmeaSentence) -> Result<RmcFix, NmeaLineError> {
    let fields: Vec<&str> = raw.payload().split(',').collect();
    match fields.as_slice() {
        [
            _,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
        ] => build_rmc(
            raw,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
            None,
            None,
        ),
        [
            _,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
            mode,
        ] => build_rmc(
            raw,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
            Some(mode),
            None,
        ),
        [
            _,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
            mode,
            navigation_status,
        ] => build_rmc(
            raw,
            time,
            status,
            latitude,
            north_south,
            longitude,
            east_west,
            speed,
            course,
            date,
            variation,
            variation_direction,
            Some(mode),
            Some(navigation_status),
        ),
        _ => Err(NmeaLineError::FieldCount {
            sentence_id: raw.identifier().to_owned(),
            expected: "12, 13, or 14",
            actual: fields.len(),
        }),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "arguments correspond one-for-one with the ordered RMC wire fields"
)]
fn build_rmc(
    raw: &RawNmeaSentence,
    time: &str,
    status: &str,
    latitude: &str,
    north_south: &str,
    longitude: &str,
    east_west: &str,
    speed: &str,
    course: &str,
    date: &str,
    variation: &str,
    variation_direction: &str,
    mode: Option<&str>,
    navigation_status: Option<&str>,
) -> Result<RmcFix, NmeaLineError> {
    let status = match status {
        "A" => RmcStatus::Active,
        "V" => RmcStatus::Void,
        value => return Err(invalid_field(raw, "status", value, "expected A or V")),
    };
    let time = parse_optional_time(raw, time)?;
    let position = parse_position(raw, latitude, north_south, longitude, east_west)?;
    let speed_knots = parse_optional_nonnegative_decimal(raw, "speed over ground", speed)?;
    let course_degrees = parse_optional_nonnegative_decimal(raw, "course over ground", course)?;
    if course_degrees.is_some_and(|degrees| degrees >= 360.0) {
        return Err(invalid_field(
            raw,
            "course over ground",
            course,
            "course must be in 0.0..<360.0 degrees",
        ));
    }
    let date = parse_optional_date(raw, date)?;
    let magnetic_variation_degrees = parse_magnetic_variation(raw, variation, variation_direction)?;
    let mode = mode.map_or(Ok(None), |value| parse_optional_rmc_mode(raw, value))?;
    let navigation_status = navigation_status.map_or(Ok(None), |value| {
        parse_optional_navigation_status(raw, value)
    })?;

    if status.is_active() && (time.is_none() || position.is_none() || date.is_none()) {
        return Err(NmeaLineError::InconsistentFields {
            sentence_id: raw.identifier().to_owned(),
            reason: "active RMC data requires time, complete position, and date",
        });
    }

    Ok(RmcFix {
        raw: raw.clone(),
        time,
        status,
        position,
        speed_knots,
        course_degrees,
        date,
        magnetic_variation_degrees,
        mode,
        navigation_status,
    })
}

fn parse_gga(raw: &RawNmeaSentence) -> Result<GgaFix, NmeaLineError> {
    let fields: Vec<&str> = raw.payload().split(',').collect();
    let [
        _,
        time,
        latitude,
        north_south,
        longitude,
        east_west,
        quality,
        satellites,
        hdop,
        altitude,
        altitude_unit,
        geoid,
        geoid_unit,
        differential_age,
        station_id,
    ] = fields.as_slice()
    else {
        return Err(NmeaLineError::FieldCount {
            sentence_id: raw.identifier().to_owned(),
            expected: "exactly 15",
            actual: fields.len(),
        });
    };

    let time = parse_optional_time(raw, time)?;
    let position = parse_position(raw, latitude, north_south, longitude, east_west)?;
    let quality = GgaFixQuality::parse(quality)
        .map_err(|reason| invalid_field(raw, "quality", quality, reason))?;
    let satellites = parse_optional_satellite_count(raw, satellites)?;
    let hdop = parse_optional_nonnegative_decimal(raw, "HDOP", hdop)?;
    let altitude_meters = parse_optional_meters(raw, "altitude", altitude, altitude_unit)?;
    let geoid_separation_meters =
        parse_optional_meters(raw, "geoid separation", geoid, geoid_unit)?;
    let differential_age_seconds =
        parse_optional_nonnegative_decimal(raw, "differential age", differential_age)?;
    let differential_station_id = parse_optional_station_id(raw, station_id)?;

    if quality.has_fix() && (time.is_none() || position.is_none()) {
        return Err(NmeaLineError::InconsistentFields {
            sentence_id: raw.identifier().to_owned(),
            reason: "a nonzero GGA quality requires time and a complete position",
        });
    }

    Ok(GgaFix {
        raw: raw.clone(),
        time,
        position,
        quality,
        satellites,
        hdop,
        altitude_meters,
        geoid_separation_meters,
        differential_age_seconds,
        differential_station_id,
    })
}

fn parse_optional_time(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<UtcTime>, NmeaLineError> {
    if value.is_empty() {
        return Ok(None);
    }
    UtcTime::parse(value)
        .map(Some)
        .map_err(|reason| invalid_field(raw, "UTC time", value, reason))
}

fn parse_optional_date(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<UtcDate>, NmeaLineError> {
    if value.is_empty() {
        return Ok(None);
    }
    UtcDate::parse(value)
        .map(Some)
        .map_err(|reason| invalid_field(raw, "UTC date", value, reason))
}

fn parse_optional_rmc_mode(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<RmcMode>, NmeaLineError> {
    let mode = match value {
        "" => return Ok(None),
        "A" => RmcMode::Autonomous,
        "D" => RmcMode::Differential,
        "E" => RmcMode::Estimated,
        "M" => RmcMode::Manual,
        "N" => RmcMode::Invalid,
        "S" => RmcMode::Simulator,
        "R" => RmcMode::RtkFixed,
        "F" => RmcMode::RtkFloat,
        "P" => RmcMode::Precise,
        _ => {
            return Err(invalid_field(
                raw,
                "mode",
                value,
                "expected A, D, E, F, M, N, P, R, S, or an empty field",
            ));
        }
    };
    Ok(Some(mode))
}

fn parse_optional_navigation_status(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<RmcNavigationStatus>, NmeaLineError> {
    let status = match value {
        "" => return Ok(None),
        "S" => RmcNavigationStatus::Safe,
        "C" => RmcNavigationStatus::Caution,
        "U" => RmcNavigationStatus::Unsafe,
        "V" => RmcNavigationStatus::Invalid,
        _ => {
            return Err(invalid_field(
                raw,
                "navigational status",
                value,
                "expected S, C, U, V, or an empty field",
            ));
        }
    };
    Ok(Some(status))
}

fn parse_position(
    raw: &RawNmeaSentence,
    latitude: &str,
    north_south: &str,
    longitude: &str,
    east_west: &str,
) -> Result<Option<LatLon>, NmeaLineError> {
    if latitude.is_empty() && north_south.is_empty() && longitude.is_empty() && east_west.is_empty()
    {
        return Ok(None);
    }
    if latitude.is_empty() || north_south.is_empty() || longitude.is_empty() || east_west.is_empty()
    {
        return Err(NmeaLineError::InconsistentFields {
            sentence_id: raw.identifier().to_owned(),
            reason: "latitude, N/S, longitude, and E/W must be either all present or all empty",
        });
    }

    let latitude_degrees = parse_coordinate(raw, latitude, north_south, CoordinateKind::Latitude)?;
    let longitude_degrees = parse_coordinate(raw, longitude, east_west, CoordinateKind::Longitude)?;
    Ok(Some(LatLon {
        latitude_degrees,
        longitude_degrees,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoordinateKind {
    Latitude,
    Longitude,
}

impl CoordinateKind {
    const fn field_name(self) -> &'static str {
        match self {
            Self::Latitude => "latitude",
            Self::Longitude => "longitude",
        }
    }

    const fn degree_digits(self) -> usize {
        match self {
            Self::Latitude => 2,
            Self::Longitude => 3,
        }
    }

    const fn maximum_degrees(self) -> u16 {
        match self {
            Self::Latitude => 90,
            Self::Longitude => 180,
        }
    }
}

fn parse_coordinate(
    raw: &RawNmeaSentence,
    value: &str,
    direction: &str,
    kind: CoordinateKind,
) -> Result<f64, NmeaLineError> {
    let Some(dot_position) = value.find('.') else {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate must contain decimal minutes",
        ));
    };
    if dot_position != kind.degree_digits() + 2 {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate has the wrong number of degree or minute digits",
        ));
    }
    let Some(degrees_text) = value.get(..kind.degree_digits()) else {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate degree field is incomplete",
        ));
    };
    let Some(minutes_text) = value.get(kind.degree_digits()..) else {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate minute field is incomplete",
        ));
    };
    if !is_ascii_digits(degrees_text) {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate degrees must contain only decimal digits",
        ));
    }
    let degrees = degrees_text.parse::<u16>().map_err(|_| {
        invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate degrees are not representable",
        )
    })?;
    let minutes = parse_decimal(minutes_text, false)
        .map_err(|reason| invalid_field(raw, kind.field_name(), value, reason))?;
    if minutes >= 60.0 {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate minutes must be in 0.0..<60.0",
        ));
    }
    let maximum = kind.maximum_degrees();
    if degrees > maximum || (degrees == maximum && minutes != 0.0) {
        return Err(invalid_field(
            raw,
            kind.field_name(),
            value,
            "coordinate exceeds its geographic domain",
        ));
    }

    let sign = match (kind, direction) {
        (CoordinateKind::Latitude, "N") | (CoordinateKind::Longitude, "E") => 1.0,
        (CoordinateKind::Latitude, "S") | (CoordinateKind::Longitude, "W") => -1.0,
        _ => {
            return Err(invalid_field(
                raw,
                kind.field_name(),
                direction,
                "coordinate direction does not match its axis",
            ));
        }
    };
    Ok(sign * (f64::from(degrees) + minutes / 60.0))
}

fn parse_magnetic_variation(
    raw: &RawNmeaSentence,
    value: &str,
    direction: &str,
) -> Result<Option<f64>, NmeaLineError> {
    if value.is_empty() && direction.is_empty() {
        return Ok(None);
    }
    if value.is_empty() || direction.is_empty() {
        return Err(NmeaLineError::InconsistentFields {
            sentence_id: raw.identifier().to_owned(),
            reason: "magnetic variation and its E/W direction must be both present or both empty",
        });
    }
    let magnitude = parse_decimal(value, false)
        .map_err(|reason| invalid_field(raw, "magnetic variation", value, reason))?;
    if magnitude > 180.0 {
        return Err(invalid_field(
            raw,
            "magnetic variation",
            value,
            "magnetic variation must be in 0.0..=180.0 degrees",
        ));
    }
    match direction {
        "E" => Ok(Some(magnitude)),
        "W" => Ok(Some(-magnitude)),
        _ => Err(invalid_field(
            raw,
            "magnetic variation direction",
            direction,
            "expected E or W",
        )),
    }
}

fn parse_optional_nonnegative_decimal(
    raw: &RawNmeaSentence,
    field: &'static str,
    value: &str,
) -> Result<Option<f64>, NmeaLineError> {
    if value.is_empty() {
        return Ok(None);
    }
    parse_decimal(value, false)
        .map(Some)
        .map_err(|reason| invalid_field(raw, field, value, reason))
}

fn parse_optional_meters(
    raw: &RawNmeaSentence,
    field: &'static str,
    value: &str,
    unit: &str,
) -> Result<Option<f64>, NmeaLineError> {
    if value.is_empty() && unit.is_empty() {
        return Ok(None);
    }
    if value.is_empty() || unit != "M" {
        return Err(NmeaLineError::InconsistentFields {
            sentence_id: raw.identifier().to_owned(),
            reason: "a meter-valued GGA field requires both a number and the unit M",
        });
    }
    parse_decimal(value, true)
        .map(Some)
        .map_err(|reason| invalid_field(raw, field, value, reason))
}

fn parse_optional_satellite_count(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<u8>, NmeaLineError> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > 2 || !is_ascii_digits(value) {
        return Err(invalid_field(
            raw,
            "satellites in use",
            value,
            "satellite count must contain one or two decimal digits",
        ));
    }
    value.parse::<u8>().map(Some).map_err(|_| {
        invalid_field(
            raw,
            "satellites in use",
            value,
            "satellite count is not representable",
        )
    })
}

fn parse_optional_station_id(
    raw: &RawNmeaSentence,
    value: &str,
) -> Result<Option<u16>, NmeaLineError> {
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 4 || !is_ascii_digits(value) {
        return Err(invalid_field(
            raw,
            "differential station ID",
            value,
            "station ID must contain exactly four decimal digits",
        ));
    }
    let station_id = value.parse::<u16>().map_err(|_| {
        invalid_field(
            raw,
            "differential station ID",
            value,
            "station ID is not representable",
        )
    })?;
    if station_id > 4095 {
        return Err(invalid_field(
            raw,
            "differential station ID",
            value,
            "station ID must be in 0000..=4095",
        ));
    }
    Ok(Some(station_id))
}

fn parse_decimal(value: &str, allow_negative: bool) -> Result<f64, &'static str> {
    let unsigned = match value.strip_prefix('-') {
        Some(unsigned) if allow_negative => unsigned,
        Some(_) => return Err("negative values are not permitted"),
        None => value,
    };
    if unsigned.is_empty() {
        return Err("decimal field contains no digits");
    }
    let mut pieces = unsigned.split('.');
    let whole = pieces.next().ok_or("decimal field contains no digits")?;
    let fraction = pieces.next();
    if pieces.next().is_some()
        || whole.is_empty()
        || !is_ascii_digits(whole)
        || fraction.is_some_and(|digits| digits.is_empty() || !is_ascii_digits(digits))
    {
        return Err("expected ordinary decimal notation without signs or exponents");
    }
    let parsed = value
        .parse::<f64>()
        .map_err(|_| "decimal value is not representable")?;
    if !parsed.is_finite() {
        return Err("decimal value must be finite");
    }
    Ok(parsed)
}

fn invalid_field(
    raw: &RawNmeaSentence,
    field: &'static str,
    value: &str,
    reason: &'static str,
) -> NmeaLineError {
    NmeaLineError::InvalidField {
        sentence_id: raw.identifier().to_owned(),
        field,
        value: value.to_owned(),
        reason,
    }
}

fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_two_digits(value: &str, offset: usize) -> Result<u8, &'static str> {
    let Some(digits) = value.as_bytes().get(offset..offset + 2) else {
        return Err("two-digit field is truncated");
    };
    let [tens, ones] = digits else {
        return Err("two-digit field is truncated");
    };
    if !tens.is_ascii_digit() || !ones.is_ascii_digit() {
        return Err("two-digit field contains a non-decimal character");
    }
    Ok((tens - b'0') * 10 + (ones - b'0'))
}

const fn days_in_month(month: u8, two_digit_year: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if two_digit_year.is_multiple_of(4) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn with_checksum(sentence: &str) -> Result<String, &'static str> {
        let body = sentence
            .strip_prefix(['$', '!'])
            .ok_or("NMEA sentence missing start delimiter")?;
        let checksum = body.bytes().fold(0_u8, |value, byte| value ^ byte);
        Ok(format!("{sentence}*{checksum:02X}\r\n"))
    }

    fn only_sentence(log: &GpsLog) -> Result<&GpsLogSentence, &'static str> {
        let mut sentences = log.sentences();
        let sentence = sentences.next().ok_or("no sentence parsed")?;
        if sentences.next().is_some() {
            return Err("more than one sentence parsed");
        }
        Ok(sentence)
    }

    #[test]
    fn parses_typed_rmc_without_losing_wire_spelling() -> TestResult {
        let source = "$GPRMC,143025.000,A,3545.1234,N,08234.5678,W,0.5,45.2,030426,5.2,W,A";
        let wire = with_checksum(source)?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Rmc(fix) = only_sentence(&log)? else {
            return Err("expected RMC sentence".into());
        };

        assert_eq!(fix.as_raw().as_str(), wire.trim_end());
        assert_eq!(fix.as_raw().talker_id(), Some("GP"));
        assert_eq!(fix.status(), RmcStatus::Active);
        let time = fix.utc_time().ok_or("active RMC has no time")?;
        assert_eq!((time.hour(), time.minute(), time.second()), (14, 30, 25));
        assert_eq!(time.fractional_digits(), Some("000"));
        let date = fix.utc_date().ok_or("active RMC has no date")?;
        assert_eq!(
            (date.day(), date.month(), date.two_digit_year()),
            (3, 4, 26)
        );
        assert_eq!(fix.mode(), Some(RmcMode::Autonomous));
        assert_eq!(fix.magnetic_variation_degrees(), Some(-5.2));
        let position = fix.position().ok_or("active RMC has no position")?;
        assert!((position.latitude_degrees() - 35.752_056_667).abs() < 0.000_001);
        assert!((position.longitude_degrees() + 82.576_13).abs() < 0.000_001);
        Ok(())
    }

    #[test]
    fn parses_current_rmc_mode_and_navigation_status() -> TestResult {
        let source = "$GNRMC,143025,A,3545.0,N,08234.0,W,0,0,030426,,,R,S";
        let wire = with_checksum(source)?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Rmc(fix) = only_sentence(&log)? else {
            return Err("expected RMC sentence".into());
        };
        assert_eq!(fix.mode(), Some(RmcMode::RtkFixed));
        assert_eq!(fix.navigation_status(), Some(RmcNavigationStatus::Safe));
        Ok(())
    }

    #[test]
    fn parses_all_standard_gga_fields() -> TestResult {
        let source = "$GNGGA,143025.50,3545.1234,N,08234.5678,W,2,18,0.8,345.6,M,-31.2,M,1.5,0042";
        let wire = with_checksum(source)?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Gga(fix) = only_sentence(&log)? else {
            return Err("expected GGA sentence".into());
        };

        assert_eq!(fix.quality(), GgaFixQuality::Differential);
        assert_eq!(fix.satellites(), Some(18));
        assert_eq!(fix.hdop(), Some(0.8));
        assert_eq!(fix.altitude_meters(), Some(345.6));
        assert_eq!(fix.geoid_separation_meters(), Some(-31.2));
        assert_eq!(fix.differential_age_seconds(), Some(1.5));
        assert_eq!(fix.differential_station_id(), Some(42));
        Ok(())
    }

    #[test]
    fn preserves_checksum_valid_unmodeled_sentence() -> TestResult {
        let source = "$GPGSV,1,1,00";
        let wire = with_checksum(source)?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Unmodeled(raw) = only_sentence(&log)? else {
            return Err("expected unmodeled sentence".into());
        };
        assert_eq!(raw.as_str(), wire.trim_end());
        assert_eq!(raw.identifier(), "GPGSV");
        assert_eq!(raw.formatter(), Some("GSV"));
        Ok(())
    }

    #[test]
    fn preserves_ais_bang_sentence_as_unmodeled() -> TestResult {
        let source = "!AIVDM,1,1,,A,13aG?P001oP@>TpE`TwP0?wN0<0u,0";
        let wire = with_checksum(source)?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Unmodeled(raw) = only_sentence(&log)? else {
            return Err("expected unmodeled AIS sentence".into());
        };
        assert_eq!(raw.delimiter(), '!');
        assert_eq!(raw.as_str(), wire.trim_end());
        Ok(())
    }

    #[test]
    fn retains_order_and_exact_diagnostics() -> TestResult {
        let valid = with_checksum("$GPGSV,1,1,00")?;
        let malformed = with_checksum("$GPRMC,250000,A,3545.0,N,08234.0,W,0,0,030426,,,A")?;
        let input = format!("{valid}not nmea\n{malformed}");
        let log = parse(input.as_bytes())?;

        assert_eq!(log.records().len(), 3);
        assert!(matches!(
            log.records().first(),
            Some(GpsLogRecord::Sentence(GpsLogSentence::Unmodeled(_)))
        ));
        let diagnostics: Vec<_> = log.diagnostics().collect();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(
            diagnostics
                .first()
                .ok_or("missing first diagnostic")?
                .line_number(),
            2
        );
        assert_eq!(
            diagnostics
                .first()
                .ok_or("missing first diagnostic")?
                .text(),
            "not nmea"
        );
        assert!(matches!(
            diagnostics
                .first()
                .ok_or("missing first diagnostic")?
                .error(),
            NmeaLineError::MissingStartDelimiter
        ));
        assert_eq!(
            diagnostics
                .get(1)
                .ok_or("missing second diagnostic")?
                .line_number(),
            3
        );
        assert!(matches!(
            diagnostics
                .get(1)
                .ok_or("missing second diagnostic")?
                .error(),
            NmeaLineError::InvalidField {
                field: "UTC time",
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn checksum_suffix_rejects_trailing_junk() -> TestResult {
        let valid = with_checksum("$GPGSV,1,1,00")?;
        let input = format!("{}junk\r\n", valid.trim_end());
        let log = parse(input.as_bytes())?;
        let diagnostic = log.diagnostics().next().ok_or("missing diagnostic")?;
        assert!(matches!(
            diagnostic.error(),
            NmeaLineError::ChecksumSuffixLength { actual: 6 }
        ));
        Ok(())
    }

    #[test]
    fn checksum_requires_standard_uppercase_hex() -> TestResult {
        let wire = with_checksum("$GPGLL,A")?;
        let lowercase = wire.replace("*3D", "*3d");
        let log = parse(lowercase.as_bytes())?;
        let diagnostic = log.diagnostics().next().ok_or("missing diagnostic")?;
        assert!(matches!(
            diagnostic.error(),
            NmeaLineError::InvalidChecksum { value } if value == "3d"
        ));
        Ok(())
    }

    #[test]
    fn checksum_mismatch_reports_declared_and_computed_values() -> TestResult {
        let log = parse(b"$GPGSV,1,1,00*00\r\n")?;
        let diagnostic = log.diagnostics().next().ok_or("missing diagnostic")?;
        assert!(matches!(
            diagnostic.error(),
            NmeaLineError::ChecksumMismatch {
                declared: 0,
                computed: 0x79
            }
        ));
        Ok(())
    }

    #[test]
    fn non_ascii_file_is_rejected_at_exact_byte() {
        let result = parse(b"$GPTXT,caf\xC3\xA9*00\r\n");
        assert!(matches!(
            result,
            Err(SdCardError::InvalidAscii {
                offset: 10,
                byte: 0xC3,
                ..
            })
        ));
    }

    #[test]
    fn sentence_limit_excludes_crlf() -> TestResult {
        let body = format!("$P{}", "A".repeat(MAX_SENTENCE_BYTES));
        let input = with_checksum(&body)?;
        let log = parse(input.as_bytes())?;
        let diagnostic = log.diagnostics().next().ok_or("missing diagnostic")?;
        assert!(matches!(
            diagnostic.error(),
            NmeaLineError::TooLong { maximum: 80, .. }
        ));
        Ok(())
    }

    #[test]
    fn modeled_sentence_field_count_is_exact() -> TestResult {
        let wire = with_checksum("$GPGGA,120000,1")?;
        let log = parse(wire.as_bytes())?;
        assert!(matches!(
            log.diagnostics()
                .next()
                .ok_or("missing diagnostic")?
                .error(),
            NmeaLineError::FieldCount {
                expected: "exactly 15",
                actual: 3,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn invalid_numeric_text_is_not_defaulted() -> TestResult {
        let wire = with_checksum("$GPGGA,120000,3545.0,N,08234.0,W,1,08,NaN,345.6,M,0.0,M,,")?;
        let log = parse(wire.as_bytes())?;
        assert!(matches!(
            log.diagnostics()
                .next()
                .ok_or("missing diagnostic")?
                .error(),
            NmeaLineError::InvalidField { field: "HDOP", .. }
        ));
        Ok(())
    }

    #[test]
    fn incomplete_coordinate_is_rejected_not_reinterpreted_as_absent() -> TestResult {
        let wire = with_checksum("$GPRMC,120000,A,3545.0,,08234.0,W,0,0,030426,,,A")?;
        let log = parse(wire.as_bytes())?;
        assert!(matches!(
            log.diagnostics()
                .next()
                .ok_or("missing diagnostic")?
                .error(),
            NmeaLineError::InconsistentFields { .. }
        ));
        Ok(())
    }

    #[test]
    fn invalid_calendar_date_is_rejected() -> TestResult {
        let wire = with_checksum("$GPRMC,120000,A,3545.0,N,08234.0,W,0,0,310426,,,A")?;
        let log = parse(wire.as_bytes())?;
        assert!(matches!(
            log.diagnostics()
                .next()
                .ok_or("missing diagnostic")?
                .error(),
            NmeaLineError::InvalidField {
                field: "UTC date",
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn leap_second_and_leap_day_are_representable() -> TestResult {
        let wire = with_checksum("$GPRMC,235960.25,A,9000.0,N,18000.0,E,0,0,290200,,,A")?;
        let log = parse(wire.as_bytes())?;
        let GpsLogSentence::Rmc(fix) = only_sentence(&log)? else {
            return Err("expected RMC sentence".into());
        };
        assert_eq!(fix.utc_time().ok_or("missing time")?.second(), 60);
        assert_eq!(fix.utc_date().ok_or("missing date")?.day(), 29);
        Ok(())
    }

    #[test]
    fn real_d75_void_fixes_remain_valid_records() -> TestResult {
        let data = b"\
$GPRMC,,V,,,,,,,,,,N*53\n\
$GPGGA,,,,,,0,,,,,,,,*66\n\
$GPRMC,,V,,,,,,,,,,N*53\n\
$GPGGA,,,,,,0,,,,,,,,*66\n";
        let log = parse(data)?;
        assert_eq!(log.error_count(), 0);
        assert_eq!(log.rmc_fixes().count(), 2);
        assert_eq!(log.gga_fixes().count(), 2);
        assert!(log.valid_fixes().next().is_none());
        assert!(log.rmc_fixes().all(|fix| {
            fix.status() == RmcStatus::Void
                && fix.position().is_none()
                && fix.speed_knots().is_none()
                && fix.course_degrees().is_none()
        }));
        assert!(log.gga_fixes().all(|fix| {
            fix.quality() == GgaFixQuality::Invalid
                && fix.position().is_none()
                && fix.satellites().is_none()
                && fix.hdop().is_none()
                && fix.altitude_meters().is_none()
        }));
        Ok(())
    }

    #[test]
    fn blank_record_is_diagnostic_but_final_terminator_is_not() -> TestResult {
        let valid = with_checksum("$GPGSV,1,1,00")?;
        let input = format!("{valid}\r\n{valid}");
        let log = parse(input.as_bytes())?;
        assert_eq!(log.sentences().count(), 2);
        let diagnostic = log
            .diagnostics()
            .next()
            .ok_or("missing blank-line diagnostic")?;
        assert_eq!(diagnostic.line_number(), 2);
        assert!(matches!(diagnostic.error(), NmeaLineError::EmptyLine));
        Ok(())
    }

    #[test]
    fn empty_file_returns_error() {
        assert!(matches!(
            parse(b""),
            Err(SdCardError::FileTooSmall {
                expected: 1,
                actual: 0
            })
        ));
    }
}
