//! NMEA 0183 sentences from the radio's GPS PC output.
//!
//! With GPS PC output on (Menu 403, the second `GP` field) the radio sends
//! the sentences Menu 405 selects ([`crate::types::NmeaSentences`], `GS`) to
//! the interface Menu 982 selects, as `$GP` sentences or, with Menu 404's
//! "GPS/QZSS/Galileo", the receiver's own `$GN`, `$GQ` and `$GA` talkers (User
//! Manual). Output pauses in KISS and DV/DR modes, and on the interface DV
//! Gateway mode uses. [`Sentence::parse`] checks one line's framing and
//! checksum and decodes GGA and RMC; [`LineReader`] picks the sentences out of
//! a byte stream that may interleave CAT replies. The decoding follows the
//! NMEA 0183 sentence formats; no capture of the radio's own output has been
//! decoded yet.

use std::fmt::Write as _;

/// Longest sentence NMEA 0183 allows: 82 characters from `$` through the
/// terminator.
pub const MAX_SENTENCE_LENGTH: usize = 82;

/// One checksum-valid sentence: its address and comma-separated fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSentence {
    address: String,
    fields: Vec<String>,
}

impl RawSentence {
    /// Parse one line without its terminator: `$`, the address, the fields
    /// and `*` with two hexadecimal checksum digits.
    ///
    /// # Errors
    ///
    /// Returns [`NmeaError::Framing`] for a line that is not printable ASCII
    /// or lacks the `$` or the `*HH` suffix, [`NmeaError::Address`] for an
    /// address that is not five uppercase letters or digits (or `P` and a
    /// proprietary code), and [`NmeaError::Checksum`] when the checksum
    /// differs.
    pub fn parse(line: &str) -> Result<Self, NmeaError> {
        let framing = || NmeaError::Framing {
            line: line.to_owned(),
        };
        if !line
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(framing());
        }
        let body = line.strip_prefix('$').ok_or_else(framing)?;
        let (body, checksum) = body.rsplit_once('*').ok_or_else(framing)?;
        if checksum.len() != 2 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(framing());
        }
        let stated = u8::from_str_radix(checksum, 16).map_err(|_| framing())?;
        let computed = body.bytes().fold(0, |sum, byte| sum ^ byte);
        if stated != computed {
            return Err(NmeaError::Checksum {
                line: line.to_owned(),
                stated,
                computed,
            });
        }
        let mut parts = body.split(',');
        let address = parts.next().unwrap_or_default();
        let proprietary = address.starts_with('P') && address.len() >= 4;
        if !(address.len() == 5 || proprietary)
            || !address
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        {
            return Err(NmeaError::Address {
                line: line.to_owned(),
            });
        }
        Ok(Self {
            address: address.to_owned(),
            fields: parts.map(str::to_owned).collect(),
        })
    }

    /// The two-character talker, `GP` or `GN` for example; `None` for a
    /// proprietary sentence.
    #[must_use]
    pub fn talker(&self) -> Option<&str> {
        (self.address.len() == 5)
            .then(|| self.address.get(..2))
            .flatten()
    }

    /// The sentence formatter, `GGA` for example, or a proprietary address
    /// after its `P`.
    #[must_use]
    pub fn formatter(&self) -> &str {
        if self.address.len() == 5 {
            self.address.get(2..).unwrap_or_default()
        } else {
            self.address.get(1..).unwrap_or_default()
        }
    }

    /// The fields after the address, empty ones included.
    #[must_use]
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
}

/// One decoded sentence.
#[derive(Debug, Clone, PartialEq)]
pub enum Sentence {
    /// Global positioning fix data.
    Gga(Gga),
    /// Recommended minimum navigation data.
    Rmc(Rmc),
    /// Any other sentence, with its fields.
    Other(RawSentence),
}

impl Sentence {
    /// Parse one line and decode GGA and RMC.
    ///
    /// # Errors
    ///
    /// Returns the [`RawSentence::parse`] errors, [`NmeaError::FieldCount`]
    /// for a GGA or RMC with too few fields and [`NmeaError::Field`] for a
    /// field outside its format.
    pub fn parse(line: &str) -> Result<Self, NmeaError> {
        let raw = RawSentence::parse(line)?;
        match (raw.talker(), raw.formatter()) {
            (Some(_), "GGA") => Gga::decode(&raw).map(Self::Gga),
            (Some(_), "RMC") => Rmc::decode(&raw).map(Self::Rmc),
            _ => Ok(Self::Other(raw)),
        }
    }
}

/// UTC time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime {
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 60 (a leap second).
    pub second: u8,
    /// Millisecond, from at most three fractional digits.
    pub millisecond: u16,
}

/// UTC calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcDate {
    /// Year: a two-digit year below 80 is 20xx, any other 19xx.
    pub year: u16,
    /// Month, 1 to 12.
    pub month: u8,
    /// Day of the month, 1 to that month's length in the Gregorian calendar.
    pub day: u8,
}

/// A position in signed decimal degrees, negative to the south and west.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    /// Latitude, −90 to 90.
    pub latitude: f64,
    /// Longitude, −180 to 180.
    pub longitude: f64,
}

/// GGA field 6: the kind of fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FixQuality {
    /// `0`: no fix.
    Invalid,
    /// `1`: a GPS fix.
    Gps,
    /// `2`: a differential fix.
    Differential,
    /// `6`: dead reckoning.
    Estimated,
    /// Any other value, retained exactly.
    Unnamed(u8),
}

impl From<u8> for FixQuality {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Invalid,
            1 => Self::Gps,
            2 => Self::Differential,
            6 => Self::Estimated,
            other => Self::Unnamed(other),
        }
    }
}

/// RMC field 2: whether the data is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RmcStatus {
    /// `A`: valid.
    Active,
    /// `V`: not valid.
    Void,
}

/// Global positioning fix data; an empty field decodes as `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Gga {
    /// The talker, `GP` or `GN` for example.
    pub talker: String,
    /// Time of the fix.
    pub time: Option<UtcTime>,
    /// Position of the fix.
    pub position: Option<Position>,
    /// Kind of fix.
    pub quality: FixQuality,
    /// Satellites in use.
    pub satellites: Option<u8>,
    /// Horizontal dilution of precision.
    pub hdop: Option<f64>,
    /// Antenna altitude above mean sea level, in metres.
    pub altitude_m: Option<f64>,
    /// Height of the geoid above the WGS-84 ellipsoid, in metres.
    pub geoid_separation_m: Option<f64>,
}

impl Gga {
    fn decode(raw: &RawSentence) -> Result<Self, NmeaError> {
        let fields = Fields::new(raw, "GGA", 14)?;
        Ok(Self {
            talker: raw.talker().unwrap_or_default().to_owned(),
            time: fields.time(0)?,
            position: fields.position(1)?,
            quality: FixQuality::from(fields.integer(5, "fix quality")?.unwrap_or(0)),
            satellites: fields.integer(6, "satellites")?,
            hdop: fields.decimal(7, "HDOP")?,
            altitude_m: fields.decimal(8, "altitude")?,
            geoid_separation_m: fields.decimal(10, "geoid separation")?,
        })
    }
}

/// Recommended minimum navigation data; an empty field decodes as `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Rmc {
    /// The talker, `GP` or `GN` for example.
    pub talker: String,
    /// Time of the fix.
    pub time: Option<UtcTime>,
    /// Whether the data is valid.
    pub status: RmcStatus,
    /// Position of the fix.
    pub position: Option<Position>,
    /// Speed over ground, in knots.
    pub speed_knots: Option<f64>,
    /// Course over ground, in degrees true.
    pub course_degrees: Option<f64>,
    /// Date of the fix.
    pub date: Option<UtcDate>,
    /// The mode indicator of NMEA 2.3 and later (`A` autonomous, `D`
    /// differential, `E` estimated, `N` not valid); `None` in the NMEA 2.0
    /// form.
    pub mode: Option<char>,
}

impl Rmc {
    fn decode(raw: &RawSentence) -> Result<Self, NmeaError> {
        let fields = Fields::new(raw, "RMC", 11)?;
        let status = match fields.text(1) {
            "A" => RmcStatus::Active,
            "V" => RmcStatus::Void,
            _ => return Err(fields.error(1, "status")),
        };
        let mode = match fields.text(11) {
            "" => None,
            text => {
                let mut characters = text.chars();
                match (characters.next(), characters.next()) {
                    (Some(mode), None) if mode.is_ascii_uppercase() => Some(mode),
                    _ => return Err(fields.error(11, "mode")),
                }
            }
        };
        Ok(Self {
            talker: raw.talker().unwrap_or_default().to_owned(),
            time: fields.time(0)?,
            status,
            position: fields.position(2)?,
            speed_knots: fields.decimal(6, "speed")?,
            course_degrees: fields.decimal(7, "course")?,
            date: fields.date(8)?,
            mode,
        })
    }
}

/// The fields of one sentence being decoded.
struct Fields<'a> {
    raw: &'a RawSentence,
    sentence: &'static str,
}

impl<'a> Fields<'a> {
    const fn new(
        raw: &'a RawSentence,
        sentence: &'static str,
        minimum: usize,
    ) -> Result<Self, NmeaError> {
        if raw.fields.len() < minimum {
            return Err(NmeaError::FieldCount {
                sentence,
                actual: raw.fields.len(),
                minimum,
            });
        }
        Ok(Self { raw, sentence })
    }

    fn text(&self, index: usize) -> &str {
        self.raw.fields.get(index).map_or("", String::as_str)
    }

    fn error(&self, index: usize, field: &'static str) -> NmeaError {
        NmeaError::Field {
            sentence: self.sentence,
            field,
            text: self.text(index).to_owned(),
        }
    }

    fn integer<T: std::str::FromStr>(
        &self,
        index: usize,
        field: &'static str,
    ) -> Result<Option<T>, NmeaError> {
        let text = self.text(index);
        if text.is_empty() {
            return Ok(None);
        }
        if !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(self.error(index, field));
        }
        text.parse().map(Some).map_err(|_| self.error(index, field))
    }

    /// A decimal of digits with an optional sign and fraction; no exponent,
    /// infinity or not-a-number.
    fn decimal(&self, index: usize, field: &'static str) -> Result<Option<f64>, NmeaError> {
        let text = self.text(index);
        if text.is_empty() {
            return Ok(None);
        }
        let digits = text.strip_prefix('-').unwrap_or(text);
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, "0"));
        if whole.is_empty()
            || fraction.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(self.error(index, field));
        }
        text.parse().map(Some).map_err(|_| self.error(index, field))
    }

    fn time(&self, index: usize) -> Result<Option<UtcTime>, NmeaError> {
        let text = self.text(index);
        if text.is_empty() {
            return Ok(None);
        }
        let invalid = || self.error(index, "time");
        let (clock, fraction) = text.split_once('.').unwrap_or((text, ""));
        if clock.len() != 6 || fraction.len() > 3 {
            return Err(invalid());
        }
        let two = |start: usize| -> Result<u8, NmeaError> {
            clock
                .get(start..start + 2)
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_digit()))
                .and_then(|digits| digits.parse().ok())
                .ok_or_else(invalid)
        };
        let (hour, minute, second) = (two(0)?, two(2)?, two(4)?);
        if hour > 23 || minute > 59 || second > 60 {
            return Err(invalid());
        }
        let mut millisecond: u16 = 0;
        for (place, byte) in [100, 10, 1].into_iter().zip(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(invalid());
            }
            millisecond += place * u16::from(byte - b'0');
        }
        Ok(Some(UtcTime {
            hour,
            minute,
            second,
            millisecond,
        }))
    }

    fn date(&self, index: usize) -> Result<Option<UtcDate>, NmeaError> {
        let text = self.text(index);
        if text.is_empty() {
            return Ok(None);
        }
        let invalid = || self.error(index, "date");
        if text.len() != 6 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        let two = |start: usize| -> u8 {
            text.get(start..start + 2)
                .and_then(|digits| digits.parse().ok())
                .unwrap_or_default()
        };
        let (day, month, two_digit_year) = (two(0), two(2), two(4));
        let century: u16 = if two_digit_year < 80 { 2000 } else { 1900 };
        let year = century + u16::from(two_digit_year);
        if !(1..=12).contains(&month) || !(1..=days_in_month(year, month)).contains(&day) {
            return Err(invalid());
        }
        Ok(Some(UtcDate { year, month, day }))
    }

    /// A latitude, its hemisphere, a longitude and its hemisphere from
    /// `index`; `None` when all four are empty.
    fn position(&self, index: usize) -> Result<Option<Position>, NmeaError> {
        if (index..index + 4).all(|field| self.text(field).is_empty()) {
            return Ok(None);
        }
        let latitude = self.angle(index, 2, 90, "latitude")?;
        let longitude = self.angle(index + 2, 3, 180, "longitude")?;
        let latitude = match self.text(index + 1) {
            "N" => latitude,
            "S" => -latitude,
            _ => return Err(self.error(index + 1, "latitude hemisphere")),
        };
        let longitude = match self.text(index + 3) {
            "E" => longitude,
            "W" => -longitude,
            _ => return Err(self.error(index + 3, "longitude hemisphere")),
        };
        Ok(Some(Position {
            latitude,
            longitude,
        }))
    }

    /// `degree_digits` digits of degrees then decimal minutes below 60, at
    /// most `limit` degrees, as decimal degrees.
    fn angle(
        &self,
        index: usize,
        degree_digits: usize,
        limit: u8,
        field: &'static str,
    ) -> Result<f64, NmeaError> {
        let text = self.text(index);
        let invalid = || self.error(index, field);
        let (degrees, minutes) = (
            text.get(..degree_digits).ok_or_else(invalid)?,
            text.get(degree_digits..).ok_or_else(invalid)?,
        );
        if !degrees.bytes().all(|byte| byte.is_ascii_digit()) || minutes.len() < 2 {
            return Err(invalid());
        }
        let (whole, fraction) = minutes.split_once('.').unwrap_or((minutes, "0"));
        if whole.len() != 2
            || fraction.is_empty()
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(invalid());
        }
        let degrees: u8 = degrees.parse().map_err(|_| invalid())?;
        let minutes: f64 = minutes.parse().map_err(|_| invalid())?;
        if minutes >= 60.0 || degrees > limit || (degrees == limit && minutes > 0.0) {
            return Err(invalid());
        }
        Ok(f64::from(degrees) + minutes / 60.0)
    }
}

/// The days of `month`, 1 to 12, in `year` of the Gregorian calendar.
const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Splits a byte stream into lines and parses those that start with `$`.
///
/// CR, LF and CRLF all end a line, so CAT replies (CR) and sentences (CRLF)
/// can share one stream; lines that do not start with `$` are skipped. A
/// sentence longer than [`MAX_SENTENCE_LENGTH`] is reported once and the
/// reader resumes at the next terminator.
#[derive(Debug, Clone, Default)]
pub struct LineReader {
    line: Vec<u8>,
    length: usize,
    overflowed: bool,
}

impl LineReader {
    /// An empty reader.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            line: Vec::new(),
            length: 0,
            overflowed: false,
        }
    }

    /// Consume `bytes` and return each sentence they complete, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Result<Sentence, NmeaError>> {
        let mut sentences = Vec::new();
        for &byte in bytes {
            if byte == b'\r' || byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                let length = std::mem::take(&mut self.length);
                if std::mem::take(&mut self.overflowed) {
                    sentences.push(Err(NmeaError::TooLong { length }));
                } else if line.first() == Some(&b'$') {
                    sentences.push(std::str::from_utf8(&line).map_or_else(
                        |_| {
                            Err(NmeaError::Framing {
                                line: printable(&line),
                            })
                        },
                        Sentence::parse,
                    ));
                }
                continue;
            }
            self.length += 1;
            if self.overflowed {
                continue;
            }
            self.line.push(byte);
            if self.line.first() == Some(&b'$') && self.line.len() > MAX_SENTENCE_LENGTH - 2 {
                self.overflowed = true;
                self.line.clear();
            }
        }
        sentences
    }
}

fn printable(bytes: &[u8]) -> String {
    let mut text = String::new();
    for &byte in bytes {
        if byte.is_ascii_graphic() || byte == b' ' {
            text.push(char::from(byte));
        } else {
            let _written = write!(text, "\\x{byte:02X}");
        }
    }
    text
}

/// An NMEA line or field that cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NmeaError {
    /// The line is not printable ASCII, or lacks `$` or the `*HH` suffix.
    #[error("{line:?} is not a $...*HH sentence of printable ASCII")]
    Framing {
        /// The line, with unprintable bytes escaped.
        line: String,
    },
    /// The address is not a five-character talker and formatter, or `P` and
    /// a proprietary code.
    #[error("{line:?} has no valid address")]
    Address {
        /// The line.
        line: String,
    },
    /// The stated checksum differs from the XOR of the bytes it covers.
    #[error("{line:?} states checksum {stated:02X}, computed {computed:02X}")]
    Checksum {
        /// The line.
        line: String,
        /// The checksum after `*`.
        stated: u8,
        /// The checksum of the bytes between `$` and `*`.
        computed: u8,
    },
    /// A sentence longer than [`MAX_SENTENCE_LENGTH`].
    #[error("a sentence of {length} bytes exceeds {MAX_SENTENCE_LENGTH}")]
    TooLong {
        /// The bytes received before the terminator.
        length: usize,
    },
    /// A decoded sentence has fewer fields than its format.
    #[error("{sentence} has {actual} fields, fewer than {minimum}")]
    FieldCount {
        /// The formatter.
        sentence: &'static str,
        /// The fields present.
        actual: usize,
        /// The fields its format requires.
        minimum: usize,
    },
    /// A field outside its format.
    #[error("{sentence} {field} {text:?} is outside its format")]
    Field {
        /// The formatter.
        sentence: &'static str,
        /// The field.
        field: &'static str,
        /// The field's text.
        text: String,
    },
}

#[cfg(test)]
#[path = "nmea_tests.rs"]
mod tests;
