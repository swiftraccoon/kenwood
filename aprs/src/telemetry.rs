//! APRS telemetry data reports (APRS 1.0.1 ch. 13).

use std::fmt;

use thiserror::Error;

use crate::error::AprsError;
use crate::message::MAX_APRS_MESSAGE_TEXT_LEN;
use crate::text::decode_wire_ascii;

/// Errors produced while constructing typed APRS telemetry values.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum TelemetryValueError {
    /// A numeric telemetry sequence exceeded its three-digit wire field.
    #[error("APRS telemetry sequence must be 0..=999 (got {value})")]
    SequenceOutOfRange {
        /// The rejected sequence number.
        value: u16,
    },

    /// An analog reading exceeded its three-digit wire field.
    #[error("APRS telemetry analog value must be 0..=999 (got {value})")]
    AnalogValueOutOfRange {
        /// The rejected analog reading.
        value: u16,
    },

    /// `Some(comment)` must contain at least one byte; `None` represents no comment.
    #[error("APRS telemetry comment must not be empty")]
    EmptyComment,

    /// A telemetry comment contained a character that APRS cannot encode.
    #[error(
        "APRS telemetry comment contains non-ASCII character {character:?} at byte index {index}"
    )]
    NonAsciiComment {
        /// The UTF-8 byte index at which the character begins.
        index: usize,
        /// The character that cannot be represented.
        character: char,
    },

    /// A telemetry comment contained an ASCII control byte or DEL.
    #[error(
        "APRS telemetry comment contains non-printable ASCII byte {byte:#04X} at byte index {index}"
    )]
    NonPrintableComment {
        /// The zero-based byte index.
        index: usize,
        /// The rejected ASCII byte.
        byte: u8,
    },

    /// A `PARM.` or `UNIT.` definition omitted the required A1 field.
    #[error("APRS telemetry definition must contain at least the A1 field")]
    EmptyDefinitionFields,

    /// A `PARM.` or `UNIT.` definition contained fields beyond B8.
    #[error("APRS telemetry definition has {actual} fields; maximum is {maximum}")]
    TooManyDefinitionFields {
        /// The maximum number of A1-A5 and B1-B8 fields.
        maximum: usize,
        /// The caller-provided field count.
        actual: usize,
    },

    /// The required first definition field, A1, was empty.
    #[error("APRS telemetry definition field {channel} must not be empty")]
    EmptyDefinitionField {
        /// The empty channel field.
        channel: TelemetryChannel,
    },

    /// A definition field exceeded its position-specific wire width.
    #[error(
        "APRS telemetry definition field {channel} is {actual} bytes; maximum is {maximum} bytes"
    )]
    DefinitionFieldTooLong {
        /// The channel whose field was too long.
        channel: TelemetryChannel,
        /// The maximum permitted field length, excluding its comma.
        maximum: usize,
        /// The caller-provided encoded length.
        actual: usize,
    },

    /// A definition field contained a character APRS cannot encode.
    #[error(
        "APRS telemetry definition field {channel} contains non-ASCII character {character:?} at byte index {index}"
    )]
    NonAsciiDefinitionField {
        /// The channel whose field was invalid.
        channel: TelemetryChannel,
        /// The UTF-8 byte index at which the character begins.
        index: usize,
        /// The character that cannot be represented.
        character: char,
    },

    /// A definition field contained an ASCII control byte or DEL.
    #[error(
        "APRS telemetry definition field {channel} contains non-printable ASCII byte {byte:#04X} at byte index {index}"
    )]
    NonPrintableDefinitionField {
        /// The channel whose field was invalid.
        channel: TelemetryChannel,
        /// The zero-based byte index.
        index: usize,
        /// The rejected ASCII byte.
        byte: u8,
    },

    /// A definition field contained a comma or APRS message delimiter.
    #[error(
        "APRS telemetry definition field {channel} contains reserved character {character:?} at byte index {index}"
    )]
    ReservedDefinitionCharacter {
        /// The channel whose field was invalid.
        channel: TelemetryChannel,
        /// The zero-based byte index.
        index: usize,
        /// The reserved delimiter.
        character: char,
    },

    /// An `EQNS.` definition contained no coefficient fields.
    #[error("APRS telemetry equation definition must contain at least one coefficient")]
    EmptyEquationCoefficients,

    /// An `EQNS.` definition contained fields beyond A5's `c` coefficient.
    #[error("APRS telemetry equation definition has {actual} coefficients; maximum is {maximum}")]
    TooManyEquationCoefficients {
        /// The maximum number of coefficient fields.
        maximum: usize,
        /// The caller-provided coefficient count.
        actual: usize,
    },

    /// An equation coefficient could not be represented as a finite number.
    #[error("APRS telemetry equation coefficient at index {index} must be finite")]
    NonFiniteEquationCoefficient {
        /// The zero-based coefficient index.
        index: usize,
    },

    /// Canonical coefficient text would exceed an APRS message body.
    #[error("APRS telemetry equation definition is {actual} bytes; maximum is {maximum} bytes")]
    EquationDefinitionTooLong {
        /// The maximum APRS message-text length.
        maximum: usize,
        /// The encoded `EQNS.` body length.
        actual: usize,
    },

    /// A BITS project title exceeded its spec-defined width.
    #[error("APRS telemetry project title is {actual} bytes; maximum is {maximum} bytes")]
    ProjectTitleTooLong {
        /// The maximum permitted title length.
        maximum: usize,
        /// The caller-provided encoded length.
        actual: usize,
    },

    /// A BITS project title contained a character APRS cannot encode.
    #[error(
        "APRS telemetry project title contains non-ASCII character {character:?} at byte index {index}"
    )]
    NonAsciiProjectTitle {
        /// The UTF-8 byte index at which the character begins.
        index: usize,
        /// The character that cannot be represented.
        character: char,
    },

    /// A BITS project title contained an ASCII control byte or DEL.
    #[error(
        "APRS telemetry project title contains non-printable ASCII byte {byte:#04X} at byte index {index}"
    )]
    NonPrintableProjectTitle {
        /// The zero-based byte index.
        index: usize,
        /// The rejected ASCII byte.
        byte: u8,
    },

    /// A BITS project title contained an APRS message delimiter.
    #[error(
        "APRS telemetry project title contains reserved character {character:?} at byte index {index}"
    )]
    ReservedProjectTitleCharacter {
        /// The zero-based byte index.
        index: usize,
        /// The reserved delimiter.
        character: char,
    },
}

/// One of the five analog or eight digital APRS telemetry channels.
///
/// APRS 1.0.1 §13 p.69 assigns different definition-field widths to each
/// position. The widths returned by [`Self::definition_field_max_len`] exclude
/// the leading comma counted in the spec table for A2 through B8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TelemetryChannel {
    /// Analog channel A1.
    Analog1,
    /// Analog channel A2.
    Analog2,
    /// Analog channel A3.
    Analog3,
    /// Analog channel A4.
    Analog4,
    /// Analog channel A5.
    Analog5,
    /// Digital channel B1.
    Digital1,
    /// Digital channel B2.
    Digital2,
    /// Digital channel B3.
    Digital3,
    /// Digital channel B4.
    Digital4,
    /// Digital channel B5.
    Digital5,
    /// Digital channel B6.
    Digital6,
    /// Digital channel B7.
    Digital7,
    /// Digital channel B8.
    Digital8,
}

impl TelemetryChannel {
    /// Channels in their A1-A5, B1-B8 wire order.
    pub const ALL: [Self; 13] = [
        Self::Analog1,
        Self::Analog2,
        Self::Analog3,
        Self::Analog4,
        Self::Analog5,
        Self::Digital1,
        Self::Digital2,
        Self::Digital3,
        Self::Digital4,
        Self::Digital5,
        Self::Digital6,
        Self::Digital7,
        Self::Digital8,
    ];

    /// Maximum definition label/unit length for this channel, excluding the
    /// comma preceding A2 through B8.
    #[must_use]
    pub const fn definition_field_max_len(self) -> usize {
        match self {
            Self::Analog1 => 7,
            Self::Analog2 => 6,
            Self::Analog3 | Self::Analog4 | Self::Digital1 => 5,
            Self::Analog5 | Self::Digital2 => 4,
            Self::Digital3 | Self::Digital4 | Self::Digital5 => 3,
            Self::Digital6 | Self::Digital7 | Self::Digital8 => 2,
        }
    }

    const fn from_index(index: usize) -> Option<Self> {
        match index {
            0 => Some(Self::Analog1),
            1 => Some(Self::Analog2),
            2 => Some(Self::Analog3),
            3 => Some(Self::Analog4),
            4 => Some(Self::Analog5),
            5 => Some(Self::Digital1),
            6 => Some(Self::Digital2),
            7 => Some(Self::Digital3),
            8 => Some(Self::Digital4),
            9 => Some(Self::Digital5),
            10 => Some(Self::Digital6),
            11 => Some(Self::Digital7),
            12 => Some(Self::Digital8),
            _ => None,
        }
    }
}

impl fmt::Display for TelemetryChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Analog1 => "A1",
            Self::Analog2 => "A2",
            Self::Analog3 => "A3",
            Self::Analog4 => "A4",
            Self::Analog5 => "A5",
            Self::Digital1 => "B1",
            Self::Digital2 => "B2",
            Self::Digital3 => "B3",
            Self::Digital4 => "B4",
            Self::Digital5 => "B5",
            Self::Digital6 => "B6",
            Self::Digital7 => "B7",
            Self::Digital8 => "B8",
        })
    }
}

/// A three-character APRS telemetry report sequence.
///
/// The wire value is either a zero-padded number in `000..=999` or the
/// special literal `MIC`. Its representation is private so an out-of-range
/// number cannot reach a builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TelemetrySequence(TelemetrySequenceValue);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum TelemetrySequenceValue {
    Number(u16),
    Mic,
}

impl TelemetrySequence {
    /// Smallest numeric sequence value.
    pub const MIN: u16 = 0;
    /// Largest numeric sequence value.
    pub const MAX: u16 = 999;
    /// The special three-byte `MIC` sequence token.
    pub const MIC: Self = Self(TelemetrySequenceValue::Mic);

    /// Create a numeric telemetry sequence.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError::SequenceOutOfRange`] when `value` is
    /// greater than 999.
    pub const fn new(value: u16) -> Result<Self, TelemetryValueError> {
        if value > Self::MAX {
            return Err(TelemetryValueError::SequenceOutOfRange { value });
        }
        Ok(Self(TelemetrySequenceValue::Number(value)))
    }

    /// Return the numeric sequence, or `None` for [`Self::MIC`].
    #[must_use]
    pub const fn as_number(self) -> Option<u16> {
        match self.0 {
            TelemetrySequenceValue::Number(value) => Some(value),
            TelemetrySequenceValue::Mic => None,
        }
    }

    /// Return whether this is the special `MIC` sequence.
    #[must_use]
    pub const fn is_mic(self) -> bool {
        matches!(self.0, TelemetrySequenceValue::Mic)
    }
}

impl fmt::Display for TelemetrySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            TelemetrySequenceValue::Number(value) => write!(f, "{value:03}"),
            TelemetrySequenceValue::Mic => f.write_str("MIC"),
        }
    }
}

impl TryFrom<u16> for TelemetrySequence {
    type Error = TelemetryValueError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// One APRS telemetry analog-channel reading in `000..=999`.
///
/// APRS 1.0.1 defines `000..=255`. The APRS 1.2 proposal page describes
/// expanding this field to `000..=999`; this crate retains that established
/// project compatibility while still enforcing the three-byte field width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TelemetryAnalogValue(u16);

impl TelemetryAnalogValue {
    /// Smallest analog reading.
    pub const MIN: u16 = 0;
    /// Largest analog reading supported by this crate.
    pub const MAX: u16 = 999;

    /// Create an analog-channel reading.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError::AnalogValueOutOfRange`] when `value` is
    /// greater than 999.
    pub const fn new(value: u16) -> Result<Self, TelemetryValueError> {
        if value > Self::MAX {
            return Err(TelemetryValueError::AnalogValueOutOfRange { value });
        }
        Ok(Self(value))
    }

    /// Return the numeric analog reading.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }
}

impl fmt::Display for TelemetryAnalogValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:03}", self.0)
    }
}

impl TryFrom<u16> for TelemetryAnalogValue {
    type Error = TelemetryValueError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<TelemetryAnalogValue> for u16 {
    fn from(value: TelemetryAnalogValue) -> Self {
        value.0
    }
}

/// A non-empty printable-ASCII comment following a telemetry digital value.
///
/// The wire format has no separator before this comment. The first eight
/// bytes after analog channel 5 are always the digital value; every remaining
/// byte belongs to the comment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TelemetryComment(String);

impl TelemetryComment {
    /// Create a telemetry comment.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError`] if `value` is empty or contains
    /// anything other than printable ASCII.
    pub fn new(value: &str) -> Result<Self, TelemetryValueError> {
        validate_comment(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the comment text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TelemetryComment {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for TelemetryComment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl TryFrom<&str> for TelemetryComment {
    type Error = TelemetryValueError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for TelemetryComment {
    type Error = TelemetryValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_comment(&value)?;
        Ok(Self(value))
    }
}

impl From<TelemetryComment> for String {
    fn from(value: TelemetryComment) -> Self {
        value.0
    }
}

/// An ordered prefix of A1-A5 and B1-B8 labels for `PARM.` or `UNIT.`.
///
/// APRS 1.0.1 §13 p.69 permits the list to stop after any field. Empty fields
/// after A1 are preserved so callers can skip an intermediate channel without
/// fabricating labels or trailing fields. Each field is checked against its
/// position-specific byte width and against APRS message delimiters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TelemetryLabels(Vec<String>);

impl TelemetryLabels {
    /// Maximum number of fields: five analog plus eight digital channels.
    pub const MAX_FIELDS: usize = 13;

    /// Create a telemetry label/unit prefix from exact wire-field values.
    ///
    /// The first value names A1 and must be non-empty. Later empty values are
    /// meaningful and are retained. Omit trailing fields by ending the slice.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError`] if the list is empty, contains more
    /// than 13 fields, or a field violates its positional width, printable
    /// ASCII representation, or delimiter rules.
    pub fn new(fields: &[&str]) -> Result<Self, TelemetryValueError> {
        let owned = fields.iter().map(|field| (*field).to_owned()).collect();
        Self::from_strings(owned)
    }

    /// Create a telemetry label/unit prefix without copying owned strings.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::new`].
    pub fn from_strings(fields: Vec<String>) -> Result<Self, TelemetryValueError> {
        validate_definition_fields(&fields)?;
        Ok(Self(fields))
    }

    /// Return the exact ordered fields, including explicit empty fields.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl fmt::Display for TelemetryLabels {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, field) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            formatter.write_str(field)?;
        }
        Ok(())
    }
}

impl TryFrom<Vec<String>> for TelemetryLabels {
    type Error = TelemetryValueError;

    fn try_from(fields: Vec<String>) -> Result<Self, Self::Error> {
        Self::from_strings(fields)
    }
}

impl From<TelemetryLabels> for Vec<String> {
    fn from(labels: TelemetryLabels) -> Self {
        labels.0
    }
}

/// A non-empty ordered prefix of `EQNS.` coefficient fields.
///
/// The coefficient order is A1 `(a,b,c)`, then A2 through A5. APRS 1.0.1
/// §13 p.70 permits the list to stop after any coefficient, so this type does
/// not synthesize missing zeroes or require complete triples. Values must be
/// finite and their canonical text must fit the 67-byte APRS message body.
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryEquationCoefficients(Vec<f64>);

impl TelemetryEquationCoefficients {
    /// Maximum coefficient count: three coefficients for five channels.
    pub const MAX_COUNT: usize = 15;
    /// Maximum complete `EQNS.` message-body length.
    pub const MAX_BODY_LEN: usize = MAX_APRS_MESSAGE_TEXT_LEN;

    /// Create a coefficient prefix from floating-point values.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError`] if the slice is empty, contains more
    /// than 15 coefficients, contains a non-finite value, or its canonical
    /// `EQNS.` representation would exceed 67 bytes.
    pub fn new(values: &[f64]) -> Result<Self, TelemetryValueError> {
        Self::from_values(values.to_vec())
    }

    /// Create a coefficient prefix without copying an owned vector.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::new`].
    pub fn from_values(values: Vec<f64>) -> Result<Self, TelemetryValueError> {
        validate_equation_coefficients(&values)?;
        Ok(Self(values))
    }

    /// Return the exact coefficient prefix.
    #[must_use]
    pub fn as_slice(&self) -> &[f64] {
        &self.0
    }
}

impl fmt::Display for TelemetryEquationCoefficients {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, value) in self.0.iter().copied().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            formatter.write_str(&format_telemetry_float(value))?;
        }
        Ok(())
    }
}

impl TryFrom<Vec<f64>> for TelemetryEquationCoefficients {
    type Error = TelemetryValueError;

    fn try_from(values: Vec<f64>) -> Result<Self, Self::Error> {
        Self::from_values(values)
    }
}

impl From<TelemetryEquationCoefficients> for Vec<f64> {
    fn from(coefficients: TelemetryEquationCoefficients) -> Self {
        coefficients.0
    }
}

/// A BITS telemetry project title of zero to 23 printable ASCII bytes.
///
/// Commas are retained because only the first comma separates the bit-sense
/// value from the title. APRS message delimiters `|`, `~`, and `{` are
/// rejected so the definition cannot be reinterpreted as another wire field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TelemetryProjectTitle(String);

impl TelemetryProjectTitle {
    /// Maximum project-title length from APRS 1.0.1 §13 p.70.
    pub const MAX_LEN: usize = 23;

    /// Create a telemetry project title.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryValueError`] if `value` exceeds 23 bytes, contains
    /// anything other than printable ASCII, or contains an APRS message
    /// delimiter.
    pub fn new(value: &str) -> Result<Self, TelemetryValueError> {
        validate_project_title(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact validated title.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TelemetryProjectTitle {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for TelemetryProjectTitle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<&str> for TelemetryProjectTitle {
    type Error = TelemetryValueError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for TelemetryProjectTitle {
    type Error = TelemetryValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_project_title(&value)?;
        Ok(Self(value))
    }
}

impl From<TelemetryProjectTitle> for String {
    fn from(title: TelemetryProjectTitle) -> Self {
        title.0
    }
}

/// A parsed APRS telemetry data report.
///
/// Format: `T#seq,val1,val2,val3,val4,val5,ddddddddcomment`, with the
/// comma after `MIC` optionally omitted on receive. The model contains exactly
/// five analog channels because every valid report carries all five.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsTelemetry {
    /// Three-character numeric or `MIC` sequence.
    pub sequence: TelemetrySequence,
    /// Exactly five analog-channel readings.
    pub analog: [TelemetryAnalogValue; 5],
    /// Eight digital-channel bits packed most-significant bit first.
    pub digital: u8,
    /// Optional printable-ASCII text following the digital bits.
    pub comment: Option<TelemetryComment>,
}

/// Parse an APRS telemetry data report.
///
/// Numeric sequences are exactly three decimal bytes. `MIC` sequences accept
/// both spec-defined forms: `T#MIC,aaa,...` and `T#MICaaa,...`. Every report
/// must then contain exactly five three-digit analog fields and eight binary
/// digital bytes. Any remaining printable ASCII is preserved as `comment`.
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for a malformed prefix, sequence,
/// field count, analog field, digital field, or comment control byte.
/// Non-ASCII input is reported as [`AprsError::InvalidTextByte`].
pub fn parse_aprs_telemetry(info: &[u8]) -> Result<AprsTelemetry, AprsError> {
    if info.first() != Some(&b'T') || info.get(1) != Some(&b'#') {
        return Err(AprsError::InvalidFormat);
    }

    let body_bytes = info.get(2..).ok_or(AprsError::InvalidFormat)?;
    let body = decode_wire_ascii("APRS telemetry body", body_bytes)?;
    let sequence_wire = body.get(..3).ok_or(AprsError::InvalidFormat)?;
    let after_sequence = body.get(3..).ok_or(AprsError::InvalidFormat)?;

    let (sequence, fields) = if sequence_wire == "MIC" {
        (
            TelemetrySequence::MIC,
            after_sequence.strip_prefix(',').unwrap_or(after_sequence),
        )
    } else {
        let number = parse_three_decimal_bytes(sequence_wire)?;
        let sequence = TelemetrySequence::new(number).map_err(|_| AprsError::InvalidFormat)?;
        let fields = after_sequence
            .strip_prefix(',')
            .ok_or(AprsError::InvalidFormat)?;
        (sequence, fields)
    };

    // Five `aaa,` fields followed by eight digital bytes occupy 28 bytes.
    // Anything after byte 28 is the optional comment, including commas or
    // bytes that happen to be `0`/`1`.
    if fields.len() < 28 {
        return Err(AprsError::InvalidFormat);
    }
    for comma_index in [3usize, 7, 11, 15, 19] {
        if fields.as_bytes().get(comma_index) != Some(&b',') {
            return Err(AprsError::InvalidFormat);
        }
    }

    let analog = [
        parse_analog(fields.get(0..3).ok_or(AprsError::InvalidFormat)?)?,
        parse_analog(fields.get(4..7).ok_or(AprsError::InvalidFormat)?)?,
        parse_analog(fields.get(8..11).ok_or(AprsError::InvalidFormat)?)?,
        parse_analog(fields.get(12..15).ok_or(AprsError::InvalidFormat)?)?,
        parse_analog(fields.get(16..19).ok_or(AprsError::InvalidFormat)?)?,
    ];

    let digital_wire = fields.get(20..28).ok_or(AprsError::InvalidFormat)?;
    let digital = parse_digital(digital_wire)?;
    let comment_wire = fields.get(28..).ok_or(AprsError::InvalidFormat)?;
    let comment = if comment_wire.is_empty() {
        None
    } else {
        Some(TelemetryComment::new(comment_wire).map_err(|_| AprsError::InvalidFormat)?)
    };

    Ok(AprsTelemetry {
        sequence,
        analog,
        digital,
        comment,
    })
}

fn parse_analog(wire: &str) -> Result<TelemetryAnalogValue, AprsError> {
    let value = parse_three_decimal_bytes(wire)?;
    TelemetryAnalogValue::new(value).map_err(|_| AprsError::InvalidFormat)
}

fn parse_three_decimal_bytes(wire: &str) -> Result<u16, AprsError> {
    let bytes = wire.as_bytes();
    if bytes.len() != 3 || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(AprsError::InvalidFormat);
    }
    let mut digits = bytes.iter().copied();
    let hundreds = digits.next().ok_or(AprsError::InvalidFormat)? - b'0';
    let tens = digits.next().ok_or(AprsError::InvalidFormat)? - b'0';
    let ones = digits.next().ok_or(AprsError::InvalidFormat)? - b'0';
    Ok(u16::from(hundreds) * 100 + u16::from(tens) * 10 + u16::from(ones))
}

fn parse_digital(wire: &str) -> Result<u8, AprsError> {
    if wire.len() != 8 {
        return Err(AprsError::InvalidFormat);
    }
    let mut value = 0u8;
    for byte in wire.bytes() {
        value <<= 1;
        match byte {
            b'0' => {}
            b'1' => value |= 1,
            _ => return Err(AprsError::InvalidFormat),
        }
    }
    Ok(value)
}

fn validate_comment(value: &str) -> Result<(), TelemetryValueError> {
    if value.is_empty() {
        return Err(TelemetryValueError::EmptyComment);
    }
    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii())
    {
        return Err(TelemetryValueError::NonAsciiComment { index, character });
    }
    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| !(b' '..=b'~').contains(byte))
    {
        return Err(TelemetryValueError::NonPrintableComment { index, byte });
    }
    Ok(())
}

fn validate_definition_fields(fields: &[String]) -> Result<(), TelemetryValueError> {
    if fields.is_empty() {
        return Err(TelemetryValueError::EmptyDefinitionFields);
    }
    if fields.len() > TelemetryLabels::MAX_FIELDS {
        return Err(TelemetryValueError::TooManyDefinitionFields {
            maximum: TelemetryLabels::MAX_FIELDS,
            actual: fields.len(),
        });
    }

    for (index, field) in fields.iter().enumerate() {
        let Some(channel) = TelemetryChannel::from_index(index) else {
            return Err(TelemetryValueError::TooManyDefinitionFields {
                maximum: TelemetryLabels::MAX_FIELDS,
                actual: fields.len(),
            });
        };
        if channel == TelemetryChannel::Analog1 && field.is_empty() {
            return Err(TelemetryValueError::EmptyDefinitionField { channel });
        }
        validate_definition_field(field, channel)?;
    }
    Ok(())
}

fn validate_definition_field(
    value: &str,
    channel: TelemetryChannel,
) -> Result<(), TelemetryValueError> {
    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii())
    {
        return Err(TelemetryValueError::NonAsciiDefinitionField {
            channel,
            index,
            character,
        });
    }

    let maximum = channel.definition_field_max_len();
    if value.len() > maximum {
        return Err(TelemetryValueError::DefinitionFieldTooLong {
            channel,
            maximum,
            actual: value.len(),
        });
    }

    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| !(b' '..=b'~').contains(byte))
    {
        return Err(TelemetryValueError::NonPrintableDefinitionField {
            channel,
            index,
            byte,
        });
    }

    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| b",|~{".contains(byte))
    {
        return Err(TelemetryValueError::ReservedDefinitionCharacter {
            channel,
            index,
            character: char::from(byte),
        });
    }
    Ok(())
}

fn validate_equation_coefficients(values: &[f64]) -> Result<(), TelemetryValueError> {
    if values.is_empty() {
        return Err(TelemetryValueError::EmptyEquationCoefficients);
    }
    if values.len() > TelemetryEquationCoefficients::MAX_COUNT {
        return Err(TelemetryValueError::TooManyEquationCoefficients {
            maximum: TelemetryEquationCoefficients::MAX_COUNT,
            actual: values.len(),
        });
    }
    if let Some(index) = values.iter().position(|value| !value.is_finite()) {
        return Err(TelemetryValueError::NonFiniteEquationCoefficient { index });
    }

    let coefficients_len = values
        .iter()
        .copied()
        .map(format_telemetry_float)
        .map(|value| value.len())
        .sum::<usize>();
    let separators = values.len().saturating_sub(1);
    let actual = "EQNS."
        .len()
        .saturating_add(coefficients_len)
        .saturating_add(separators);
    if actual > TelemetryEquationCoefficients::MAX_BODY_LEN {
        return Err(TelemetryValueError::EquationDefinitionTooLong {
            maximum: TelemetryEquationCoefficients::MAX_BODY_LEN,
            actual,
        });
    }
    Ok(())
}

/// Format one finite coefficient in the canonical representation used by the
/// transmit API. Integer-valued coefficients omit a redundant `.0` suffix.
fn format_telemetry_float(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a finite integral f64 with abs < 1e15 fits in i64 exactly"
        )]
        let integer = value as i64;
        return integer.to_string();
    }
    value.to_string()
}

fn validate_project_title(value: &str) -> Result<(), TelemetryValueError> {
    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii())
    {
        return Err(TelemetryValueError::NonAsciiProjectTitle { index, character });
    }

    if value.len() > TelemetryProjectTitle::MAX_LEN {
        return Err(TelemetryValueError::ProjectTitleTooLong {
            maximum: TelemetryProjectTitle::MAX_LEN,
            actual: value.len(),
        });
    }

    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| !(b' '..=b'~').contains(byte))
    {
        return Err(TelemetryValueError::NonPrintableProjectTitle { index, byte });
    }

    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| b"|~{".contains(byte))
    {
        return Err(TelemetryValueError::ReservedProjectTitleCharacter {
            index,
            character: char::from(byte),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn analog(
        [value_1, value_2, value_3, value_4, value_5]: [u16; 5],
    ) -> Result<[TelemetryAnalogValue; 5], TelemetryValueError> {
        Ok([
            TelemetryAnalogValue::new(value_1)?,
            TelemetryAnalogValue::new(value_2)?,
            TelemetryAnalogValue::new(value_3)?,
            TelemetryAnalogValue::new(value_4)?,
            TelemetryAnalogValue::new(value_5)?,
        ])
    }

    #[test]
    fn typed_values_enforce_ranges_and_wire_format() -> TestResult {
        let zero = TelemetrySequence::new(0)?;
        let maximum = TelemetrySequence::new(999)?;
        assert_eq!(zero.to_string(), "000");
        assert_eq!(maximum.to_string(), "999");
        assert_eq!(maximum.as_number(), Some(999));
        assert_eq!(TelemetrySequence::MIC.to_string(), "MIC");
        assert!(TelemetrySequence::MIC.is_mic(), "MIC must identify itself");
        assert_eq!(
            TelemetrySequence::new(1000),
            Err(TelemetryValueError::SequenceOutOfRange { value: 1000 })
        );

        let reading = TelemetryAnalogValue::new(7)?;
        assert_eq!(reading.value(), 7);
        assert_eq!(reading.to_string(), "007");
        assert_eq!(
            TelemetryAnalogValue::new(1000),
            Err(TelemetryValueError::AnalogValueOutOfRange { value: 1000 })
        );
        Ok(())
    }

    #[test]
    fn telemetry_comment_requires_nonempty_printable_ascii() -> TestResult {
        let comment = TelemetryComment::new(" pump 1,OK")?;
        assert_eq!(comment.as_str(), " pump 1,OK");
        assert_eq!(comment.to_string(), " pump 1,OK");
        assert_eq!(
            TelemetryComment::new(""),
            Err(TelemetryValueError::EmptyComment)
        );
        assert_eq!(
            TelemetryComment::new("A\n"),
            Err(TelemetryValueError::NonPrintableComment {
                index: 1,
                byte: b'\n',
            })
        );
        assert_eq!(
            TelemetryComment::new("Aé"),
            Err(TelemetryValueError::NonAsciiComment {
                index: 1,
                character: 'é',
            })
        );
        Ok(())
    }

    #[test]
    fn telemetry_channels_expose_spec_field_widths() {
        let expected = [
            (TelemetryChannel::Analog1, "A1", 7),
            (TelemetryChannel::Analog2, "A2", 6),
            (TelemetryChannel::Analog3, "A3", 5),
            (TelemetryChannel::Analog4, "A4", 5),
            (TelemetryChannel::Analog5, "A5", 4),
            (TelemetryChannel::Digital1, "B1", 5),
            (TelemetryChannel::Digital2, "B2", 4),
            (TelemetryChannel::Digital3, "B3", 3),
            (TelemetryChannel::Digital4, "B4", 3),
            (TelemetryChannel::Digital5, "B5", 3),
            (TelemetryChannel::Digital6, "B6", 2),
            (TelemetryChannel::Digital7, "B7", 2),
            (TelemetryChannel::Digital8, "B8", 2),
        ];
        for (channel, wire_name, maximum) in expected {
            assert_eq!(channel.to_string(), wire_name);
            assert_eq!(channel.definition_field_max_len(), maximum);
        }
    }

    #[test]
    fn telemetry_labels_preserve_exact_prefix_and_positional_maxima() -> TestResult {
        let labels = TelemetryLabels::new(&[
            "AAAAAAA", "BBBBBB", "CCCCC", "DDDDD", "EEEE", "FFFFF", "GGGG", "HHH", "III", "JJJ",
            "KK", "LL", "MM",
        ])?;
        assert_eq!(
            labels.to_string(),
            "AAAAAAA,BBBBBB,CCCCC,DDDDD,EEEE,FFFFF,GGGG,HHH,III,JJJ,KK,LL,MM"
        );

        let skipped = TelemetryLabels::new(&["A", "", "B", ""])?;
        assert_eq!(skipped.to_string(), "A,,B,");
        assert_eq!(skipped.as_slice().len(), 4);

        for (index, channel) in TelemetryChannel::ALL.iter().copied().enumerate() {
            let maximum = channel.definition_field_max_len();
            let mut fields = vec![String::from("A"); index + 1];
            if let Some(field) = fields.get_mut(index) {
                *field = "X".repeat(maximum + 1);
            }
            assert_eq!(
                TelemetryLabels::from_strings(fields),
                Err(TelemetryValueError::DefinitionFieldTooLong {
                    channel,
                    maximum,
                    actual: maximum + 1,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn telemetry_labels_reject_missing_extra_or_malformed_fields() {
        assert_eq!(
            TelemetryLabels::new(&[]),
            Err(TelemetryValueError::EmptyDefinitionFields)
        );
        assert_eq!(
            TelemetryLabels::new(&["", "A2"]),
            Err(TelemetryValueError::EmptyDefinitionField {
                channel: TelemetryChannel::Analog1,
            })
        );

        let too_many = vec![String::from("A"); TelemetryLabels::MAX_FIELDS + 1];
        assert_eq!(
            TelemetryLabels::from_strings(too_many),
            Err(TelemetryValueError::TooManyDefinitionFields {
                maximum: 13,
                actual: 14,
            })
        );
        assert_eq!(
            TelemetryLabels::new(&["A", "é"]),
            Err(TelemetryValueError::NonAsciiDefinitionField {
                channel: TelemetryChannel::Analog2,
                index: 0,
                character: 'é',
            })
        );
        assert_eq!(
            TelemetryLabels::new(&["A", "B\n"]),
            Err(TelemetryValueError::NonPrintableDefinitionField {
                channel: TelemetryChannel::Analog2,
                index: 1,
                byte: b'\n',
            })
        );
        for character in [',', '|', '~', '{'] {
            let value = format!("B{character}C");
            assert_eq!(
                TelemetryLabels::new(&["A", &value]),
                Err(TelemetryValueError::ReservedDefinitionCharacter {
                    channel: TelemetryChannel::Analog2,
                    index: 1,
                    character,
                })
            );
        }
    }

    #[test]
    fn telemetry_equation_coefficients_are_finite_bounded_prefixes() -> TestResult {
        let coefficients = TelemetryEquationCoefficients::new(&[0.0, 5.2, -32.0])?;
        assert_eq!(coefficients.to_string(), "0,5.2,-32");
        assert_eq!(coefficients.as_slice(), &[0.0, 5.2, -32.0]);
        assert_eq!(
            TelemetryEquationCoefficients::new(&[]),
            Err(TelemetryValueError::EmptyEquationCoefficients)
        );
        assert_eq!(
            TelemetryEquationCoefficients::new(&[0.0; 16]),
            Err(TelemetryValueError::TooManyEquationCoefficients {
                maximum: 15,
                actual: 16,
            })
        );
        assert_eq!(
            TelemetryEquationCoefficients::new(&[0.0, f64::NAN]),
            Err(TelemetryValueError::NonFiniteEquationCoefficient { index: 1 })
        );
        assert!(
            matches!(
                TelemetryEquationCoefficients::new(&[f64::MAX]),
                Err(TelemetryValueError::EquationDefinitionTooLong {
                    maximum: 67,
                    actual,
                }) if actual > 67
            ),
            "a coefficient whose canonical text exceeds the message body must be rejected",
        );
        Ok(())
    }

    #[test]
    fn telemetry_project_title_enforces_bits_field_contract() -> TestResult {
        assert_eq!(TelemetryProjectTitle::new("")?.as_str(), "");
        assert_eq!(
            TelemetryProjectTitle::new("Weather, north")?.as_str(),
            "Weather, north"
        );
        let maximum = "P".repeat(TelemetryProjectTitle::MAX_LEN);
        assert_eq!(TelemetryProjectTitle::new(&maximum)?.as_str(), maximum);
        assert_eq!(
            TelemetryProjectTitle::new(&"P".repeat(TelemetryProjectTitle::MAX_LEN + 1)),
            Err(TelemetryValueError::ProjectTitleTooLong {
                maximum: 23,
                actual: 24,
            })
        );
        assert_eq!(
            TelemetryProjectTitle::new("Aé"),
            Err(TelemetryValueError::NonAsciiProjectTitle {
                index: 1,
                character: 'é',
            })
        );
        assert_eq!(
            TelemetryProjectTitle::new("A\r"),
            Err(TelemetryValueError::NonPrintableProjectTitle {
                index: 1,
                byte: b'\r',
            })
        );
        for character in ['|', '~', '{'] {
            let value = format!("A{character}");
            assert_eq!(
                TelemetryProjectTitle::new(&value),
                Err(TelemetryValueError::ReservedProjectTitleCharacter {
                    index: 1,
                    character,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn parse_telemetry_full_and_comment_losslessly() -> TestResult {
        let info = b"T#123,100,200,300,400,500,101010101, pump,OK";
        let telemetry = parse_aprs_telemetry(info)?;
        assert_eq!(telemetry.sequence, TelemetrySequence::new(123)?);
        assert_eq!(telemetry.analog, analog([100, 200, 300, 400, 500])?);
        assert_eq!(telemetry.digital, 0b1010_1010);
        assert_eq!(
            telemetry.comment.as_ref().map(TelemetryComment::as_str),
            Some("1, pump,OK")
        );
        Ok(())
    }

    #[test]
    fn parse_telemetry_accepts_both_mic_separator_forms() -> TestResult {
        for info in [
            b"T#MIC,001,002,003,004,005,11111111".as_slice(),
            b"T#MIC001,002,003,004,005,11111111".as_slice(),
        ] {
            let telemetry = parse_aprs_telemetry(info)?;
            assert_eq!(telemetry.sequence, TelemetrySequence::MIC);
            assert_eq!(telemetry.analog, analog([1, 2, 3, 4, 5])?);
            assert_eq!(telemetry.digital, 0xFF);
            assert_eq!(telemetry.comment, None);
        }
        Ok(())
    }

    #[test]
    fn parse_telemetry_zero_values() -> TestResult {
        let telemetry = parse_aprs_telemetry(b"T#000,000,000,000,000,000,00000000")?;
        assert_eq!(telemetry.sequence, TelemetrySequence::new(0)?);
        assert_eq!(telemetry.analog, analog([0, 0, 0, 0, 0])?);
        assert_eq!(telemetry.digital, 0);
        assert_eq!(telemetry.comment, None);
        Ok(())
    }

    #[test]
    fn parse_telemetry_rejects_partial_or_malformed_analog_fields() {
        for info in [
            b"T#001,010,020,030".as_slice(),
            b"T#001,010,020,030,040,00000000".as_slice(),
            b"T#001,10,020,030,040,050,00000000".as_slice(),
            b"T#001,1000,020,030,040,050,00000000".as_slice(),
            b"T#001,01X,020,030,040,050,00000000".as_slice(),
            b"T#001,010,,030,040,050,00000000".as_slice(),
        ] {
            assert!(
                matches!(parse_aprs_telemetry(info), Err(AprsError::InvalidFormat)),
                "malformed analog report must be rejected: {info:?}",
            );
        }
    }

    #[test]
    fn parse_telemetry_rejects_malformed_sequence() {
        for info in [
            b"T#01,001,002,003,004,005,00000000".as_slice(),
            b"T#0001,001,002,003,004,005,00000000".as_slice(),
            b"T#ABC,001,002,003,004,005,00000000".as_slice(),
            b"T#mic,001,002,003,004,005,00000000".as_slice(),
            b"T#001001,002,003,004,005,00000000".as_slice(),
        ] {
            assert!(
                matches!(parse_aprs_telemetry(info), Err(AprsError::InvalidFormat)),
                "malformed sequence must be rejected: {info:?}",
            );
        }
    }

    #[test]
    fn parse_telemetry_requires_eight_binary_digital_bytes() {
        for info in [
            b"T#001,001,002,003,004,005,0000000".as_slice(),
            b"T#001,001,002,003,004,005,0000000X".as_slice(),
            b"T#001,001,002,003,004,005,00002000".as_slice(),
        ] {
            assert!(
                matches!(parse_aprs_telemetry(info), Err(AprsError::InvalidFormat)),
                "malformed digital field must be rejected: {info:?}",
            );
        }
    }

    #[test]
    fn parse_telemetry_rejects_non_ascii_body_without_replacement() {
        assert_eq!(
            parse_aprs_telemetry(b"T#001,001,002,003,004,005,00000000\xFF"),
            Err(AprsError::InvalidTextByte {
                field: "APRS telemetry body",
                index: 32,
                byte: 0xFF,
            }),
        );
    }

    #[test]
    fn parse_telemetry_rejects_comment_control_bytes() {
        assert_eq!(
            parse_aprs_telemetry(b"T#001,001,002,003,004,005,00000000ok\r"),
            Err(AprsError::InvalidFormat)
        );
    }

    #[test]
    fn parse_telemetry_invalid_prefix() {
        assert_eq!(
            parse_aprs_telemetry(b"T001,001,002,003,004,005,00000000"),
            Err(AprsError::InvalidFormat)
        );
    }
}
