//! Validated text values for APRS on-air fields.
//!
//! APRS text fields are byte-oriented ASCII, not arbitrary UTF-8 strings.
//! These types validate complete caller-provided values and never truncate or
//! silently replace bytes.

use std::fmt;

use thiserror::Error;

use crate::error::AprsError;
use crate::units::MessageId;

/// Identifies the APRS field that failed text validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AprsTextField {
    /// The unpadded semantic value of a message addressee field.
    MessageAddressee,
    /// The body of an APRS message, excluding any message ID trailer.
    MessageText,
    /// The body of an APRS bulletin or announcement.
    BulletinText,
    /// An untimestamped APRS status body.
    StatusText,
    /// The text portion of a timestamped APRS status body.
    TimestampedStatusText,
    /// A trailing APRS positionless-weather station comment.
    WeatherComment,
    /// Free text trailing an uncompressed APRS position, object, or item report.
    PositionReportText,
    /// Free text trailing an APRS compressed-position report.
    CompressedPositionText,
    /// Status text trailing the mandatory Mic-E position bytes.
    MiceStatusText,
    /// The semantic value of a fixed-width APRS object name.
    ObjectName,
    /// The semantic value of a variable-width APRS item name.
    ItemName,
}

impl fmt::Display for AprsTextField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MessageAddressee => "APRS message addressee",
            Self::MessageText => "APRS message text",
            Self::BulletinText => "APRS bulletin text",
            Self::StatusText => "APRS status text",
            Self::TimestampedStatusText => "APRS timestamped status text",
            Self::WeatherComment => "APRS weather comment",
            Self::PositionReportText => "APRS position-report text",
            Self::CompressedPositionText => "APRS compressed-position text",
            Self::MiceStatusText => "APRS Mic-E status text",
            Self::ObjectName => "APRS object name",
            Self::ItemName => "APRS item name",
        })
    }
}

/// A precise validation failure for an APRS text value.
///
/// All indices are zero-based byte indices in the caller-provided string.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AprsTextError {
    /// A field whose APRS representation requires a value was empty.
    #[error("{field} must not be empty")]
    Empty {
        /// The field being validated.
        field: AprsTextField,
    },

    /// The encoded ASCII value was shorter than its on-air minimum width.
    #[error("{field} is {actual} bytes; minimum is {minimum} bytes")]
    TooShort {
        /// The field being validated.
        field: AprsTextField,
        /// The minimum permitted encoded length.
        minimum: usize,
        /// The caller-provided encoded length.
        actual: usize,
    },

    /// The encoded ASCII value would exceed its on-air field width.
    #[error("{field} is {actual} bytes; maximum is {maximum} bytes")]
    TooLong {
        /// The field being validated.
        field: AprsTextField,
        /// The maximum permitted encoded length.
        maximum: usize,
        /// The caller-provided encoded length.
        actual: usize,
    },

    /// A Unicode character cannot be represented in an APRS ASCII field.
    #[error("{field} contains non-ASCII character {character:?} at byte index {index}")]
    NonAscii {
        /// The field being validated.
        field: AprsTextField,
        /// The UTF-8 byte index at which the character begins.
        index: usize,
        /// The character that cannot be represented.
        character: char,
    },

    /// An ASCII control byte or DEL appeared where printable ASCII is required.
    #[error("{field} contains non-printable ASCII byte {byte:#04X} at byte index {index}")]
    NonPrintableAscii {
        /// The field being validated.
        field: AprsTextField,
        /// The zero-based byte index.
        index: usize,
        /// The invalid ASCII byte.
        byte: u8,
    },

    /// APRS reserves a printable character for another wire-level purpose.
    #[error("{field} contains reserved character {character:?} at byte index {index}")]
    ReservedCharacter {
        /// The field being validated.
        field: AprsTextField,
        /// The zero-based byte index.
        index: usize,
        /// The reserved character.
        character: char,
    },

    /// A semantic addressee included a space used for on-air field padding.
    #[error(
        "APRS message addressee contains padding space at byte index {index}; provide the unpadded value"
    )]
    AmbiguousAddresseePadding {
        /// The zero-based index of the space.
        index: usize,
    },

    /// A semantic object name ended in a space used for fixed-width padding.
    #[error(
        "APRS object name ends with ambiguous padding space at byte index {index}; remove trailing spaces"
    )]
    AmbiguousObjectNamePadding {
        /// The zero-based index of the final space.
        index: usize,
    },
}

/// An APRS message addressee before padding to its nine-byte on-air field.
///
/// APRS uses this field for more than AX.25 callsigns: `ALL`, `QST`, `CQ`,
/// `BLN` bulletin/announcement/group identifiers, weather bulletin names,
/// and application names are all legitimate. Validation therefore accepts
/// any visible ASCII rather than imposing callsign syntax. Spaces are
/// rejected because they are wire padding and would make the semantic value
/// ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageAddressee(String);

impl MessageAddressee {
    /// Maximum encoded length of an unpadded addressee.
    pub const MAX_LEN: usize = 9;

    /// Create a message addressee from its unpadded semantic value.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` is empty, exceeds nine bytes,
    /// contains non-ASCII/non-printable data, or contains a padding space.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_message_addressee(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the unpadded addressee.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A message body excluding its optional `{message-id` trailer.
///
/// APRS 1.0.1 permits zero to 67 printable ASCII bytes. `|` and `~` are
/// reserved for telemetry, while `{` starts the separately encoded message
/// ID trailer, so none may occur in this value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageText(String);

impl MessageText {
    /// Maximum encoded message-body length.
    pub const MAX_LEN: usize = 67;

    /// Create an APRS message body.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 67 bytes, is not printable
    /// ASCII, or contains one of the reserved characters `|`, `~`, or `{`.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_message_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Create an APRS acknowledgement control-frame body (`ack<ID>`).
    ///
    /// The fixed prefix is printable ASCII and a [`MessageId`] contains one
    /// to five ASCII alphanumerics, so the resulting four-to-eight-byte body
    /// always satisfies this type's invariant.
    #[must_use]
    pub fn acknowledgement(message_id: &MessageId) -> Self {
        Self(format!("ack{message_id}"))
    }

    /// Create an APRS rejection control-frame body (`rej<ID>`).
    ///
    /// The fixed prefix is printable ASCII and a [`MessageId`] contains one
    /// to five ASCII alphanumerics, so the resulting four-to-eight-byte body
    /// always satisfies this type's invariant.
    #[must_use]
    pub fn rejection(message_id: &MessageId) -> Self {
        Self(format!("rej{message_id}"))
    }

    /// Return the validated message body.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A bulletin or announcement body.
///
/// APRS 1.0.1 distinguishes bulletin/announcement bodies from directed
/// message bodies: both permit up to 67 printable ASCII bytes and reserve
/// `|` and `~`, but `{` is ordinary bulletin text rather than an optional
/// message-ID delimiter. Use this type with
/// [`crate::build_aprs_bulletin`] instead of [`MessageText`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BulletinText(String);

impl BulletinText {
    /// Maximum encoded bulletin-body length.
    pub const MAX_LEN: usize = 67;

    /// Create an APRS bulletin or announcement body.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 67 bytes, is not printable
    /// ASCII, or contains either reserved character `|` or `~`.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_bulletin_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the validated bulletin body.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An untimestamped APRS status body.
///
/// APRS 1.0.1 permits zero to 62 printable ASCII bytes in this form. Use a
/// separate timestamp field when constructing a timestamped status; its text
/// limit is 55 bytes rather than the limit represented by this type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusText(String);

impl StatusText {
    /// Maximum encoded length of an untimestamped status body.
    pub const MAX_LEN: usize = 62;

    /// Create an untimestamped APRS status body.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 62 bytes, is not printable
    /// ASCII, or contains the reserved character `|` or `~`.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_status_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the validated status body.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The text portion of a timestamped APRS status body.
///
/// A status timestamp occupies seven of the format's 62 available bytes, so
/// this value permits at most 55 printable ASCII bytes. Keeping this limit in
/// the type prevents a timestamped-status builder from emitting an overlong
/// report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimestampedStatusText(String);

impl TimestampedStatusText {
    /// Maximum encoded text length after a seven-byte status timestamp.
    pub const MAX_LEN: usize = 55;

    /// Create the text portion of a timestamped APRS status.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 55 bytes, is not printable
    /// ASCII, or contains the reserved character `|` or `~`.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_timestamped_status_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the validated timestamped-status text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A trailing comment or station-type suffix on a positionless weather report.
///
/// APRS text is byte-oriented printable ASCII. The weather grammar does not
/// assign a fixed width to this suffix, so this type validates representation
/// without silently truncating caller input. The empty string represents a
/// report with no trailing comment. A non-empty comment may not begin with a
/// weather-field tag because a strict decoder must interpret that byte as
/// another tagged measurement rather than as free text.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WeatherComment(String);

impl WeatherComment {
    /// Create a weather comment from its exact on-air text.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` contains non-ASCII/non-printable
    /// bytes or begins with a weather-field tag.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_weather_comment(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact validated comment text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Free text trailing an uncompressed APRS position, object, or item report.
///
/// APRS 1.0.1 limits position, object, and item content after the symbol code
/// to 43 characters. That content includes any data extension and trailing
/// prose.
///
/// APRS text is byte-oriented. This type accepts the empty string and visible
/// seven-bit ASCII (`0x20..=0x7E`) exactly as supplied. It rejects controls,
/// DEL, non-ASCII text, and overlength input without replacing or truncating
/// anything. APRS also reserves `|` and `~` for TNC channel switching, so
/// neither byte is representable here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PositionReportText(String);

impl PositionReportText {
    /// Maximum text length after an uncompressed position symbol code.
    pub const MAX_LEN: usize = 43;

    /// Create position-report text from its exact on-air representation.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 43 bytes, contains a
    /// non-ASCII byte, an ASCII control byte or DEL, or contains reserved
    /// `|`/`~` channel-switching delimiters.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_position_report_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact validated text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Free text trailing an APRS compressed-position report.
///
/// A compressed position has three fixed `csT` data-extension bytes after its
/// symbol code. Those bytes consume three of the 43 characters available after
/// the symbol, leaving at most 40 bytes for trailing text.
///
/// The value is stored exactly as supplied and must be printable seven-bit
/// ASCII. Controls, DEL, non-ASCII text, `|`, and `~` are rejected without
/// replacement or truncation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompressedPositionText(String);

impl CompressedPositionText {
    /// Maximum text length after a compressed position's fixed `csT` bytes.
    pub const MAX_LEN: usize = 40;

    /// Create compressed-position text from its exact on-air representation.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 40 bytes, contains a
    /// non-ASCII byte, an ASCII control byte or DEL, or contains reserved
    /// `|`/`~` channel-switching delimiters.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_compressed_position_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact validated text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Status text trailing the nine mandatory bytes of a Mic-E information field.
///
/// AX.25 permits a 256-octet information field. The Mic-E data type, longitude,
/// speed/course, symbol code, and symbol table occupy nine bytes, leaving at
/// most 247 bytes for status text. APRS 1.0.1 also forbids Mic-E status text
/// from beginning with either printable telemetry flag (grave accent or
/// apostrophe) or byte `0x1D`; `0x1D` is already excluded by the
/// printable-ASCII requirement.
///
/// The value is stored exactly as supplied. It is never replaced or truncated,
/// and `|` and `~` are rejected as reserved channel-switching delimiters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MiceStatusText(String);

impl MiceStatusText {
    /// Maximum status-text length within a 256-octet Mic-E information field.
    pub const MAX_LEN: usize = 247;

    /// Create Mic-E status text from its exact on-air representation.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` exceeds 247 bytes, contains a
    /// non-ASCII byte, an ASCII control byte or DEL, contains reserved `|`/`~`,
    /// or begins with a grave accent or apostrophe telemetry flag.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_mice_status_text(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the exact validated status text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An APRS object name before padding to its fixed nine-byte on-air field.
///
/// Object names are case-sensitive and may contain printable ASCII bytes.
/// Trailing spaces are rejected because the fixed-width wire field uses them
/// as padding; accepting one would make distinct semantic names serialize to
/// the same packet and an all-space value would parse back as empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectName(String);

impl ObjectName {
    /// Maximum encoded object-name length before wire padding.
    pub const MAX_LEN: usize = 9;

    /// Create an APRS object name.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` is empty, exceeds nine bytes,
    /// contains anything other than printable ASCII, or ends in a padding
    /// space.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_object_name(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the unpadded object name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An APRS item name before its live (`!`) or killed (`_`) delimiter.
///
/// Item names contain three to nine printable ASCII bytes. The delimiter
/// characters are excluded because either one would terminate the name on
/// the wire.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemName(String);

impl ItemName {
    /// Minimum encoded item-name length.
    pub const MIN_LEN: usize = 3;
    /// Maximum encoded item-name length.
    pub const MAX_LEN: usize = 9;

    /// Create an APRS item name.
    ///
    /// # Errors
    ///
    /// Returns [`AprsTextError`] if `value` is shorter than three bytes,
    /// exceeds nine bytes, contains anything other than printable ASCII, or
    /// contains the reserved delimiter `!` or `_`.
    pub fn new(value: &str) -> Result<Self, AprsTextError> {
        validate_item_name(value)?;
        Ok(Self(value.to_owned()))
    }

    /// Return the validated item name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! impl_text_value_traits {
    ($type:ident, $validate:ident) => {
        impl AsRef<str> for $type {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl TryFrom<&str> for $type {
            type Error = AprsTextError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $type {
            type Error = AprsTextError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                $validate(&value)?;
                Ok(Self(value))
            }
        }

        impl From<$type> for String {
            fn from(value: $type) -> Self {
                value.0
            }
        }
    };
}

impl_text_value_traits!(MessageAddressee, validate_message_addressee);
impl_text_value_traits!(MessageText, validate_message_text);
impl_text_value_traits!(BulletinText, validate_bulletin_text);
impl_text_value_traits!(StatusText, validate_status_text);
impl_text_value_traits!(TimestampedStatusText, validate_timestamped_status_text);
impl_text_value_traits!(WeatherComment, validate_weather_comment);
impl_text_value_traits!(PositionReportText, validate_position_report_text);
impl_text_value_traits!(CompressedPositionText, validate_compressed_position_text);
impl_text_value_traits!(MiceStatusText, validate_mice_status_text);
impl_text_value_traits!(ObjectName, validate_object_name);
impl_text_value_traits!(ItemName, validate_item_name);

/// Decode a seven-bit ASCII field without replacing malformed wire bytes.
pub(crate) fn decode_wire_ascii<'a>(
    field: &'static str,
    bytes: &'a [u8],
) -> Result<&'a str, AprsError> {
    if let Some((index, byte)) = bytes
        .iter()
        .copied()
        .enumerate()
        .find(|(_, byte)| !byte.is_ascii())
    {
        return Err(AprsError::InvalidTextByte { field, index, byte });
    }

    // Every ASCII byte sequence is UTF-8. Retain a defensive error mapping
    // so this helper remains total if its validation changes.
    std::str::from_utf8(bytes).map_err(|_| AprsError::InvalidFormat)
}

fn validate_message_addressee(value: &str) -> Result<(), AprsTextError> {
    validate_printable_ascii(
        value,
        AprsTextField::MessageAddressee,
        MessageAddressee::MAX_LEN,
        false,
    )?;

    if let Some(index) = value.bytes().position(|byte| byte == b' ') {
        return Err(AprsTextError::AmbiguousAddresseePadding { index });
    }
    Ok(())
}

fn validate_message_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~{";

    validate_printable_ascii(
        value,
        AprsTextField::MessageText,
        MessageText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::MessageText, RESERVED)
}

fn validate_bulletin_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(
        value,
        AprsTextField::BulletinText,
        BulletinText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::BulletinText, RESERVED)
}

fn validate_status_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(value, AprsTextField::StatusText, StatusText::MAX_LEN, true)?;
    validate_not_reserved(value, AprsTextField::StatusText, RESERVED)
}

fn validate_timestamped_status_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(
        value,
        AprsTextField::TimestampedStatusText,
        TimestampedStatusText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::TimestampedStatusText, RESERVED)
}

fn validate_weather_comment(value: &str) -> Result<(), AprsTextError> {
    validate_printable_ascii(value, AprsTextField::WeatherComment, usize::MAX, true)?;
    if let Some(&byte) = value.as_bytes().first()
        && matches!(
            byte,
            b'c' | b's' | b'g' | b't' | b'r' | b'p' | b'P' | b'h' | b'b' | b'L' | b'l'
        )
    {
        return Err(AprsTextError::ReservedCharacter {
            field: AprsTextField::WeatherComment,
            index: 0,
            character: char::from(byte),
        });
    }
    Ok(())
}

fn validate_position_report_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(
        value,
        AprsTextField::PositionReportText,
        PositionReportText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::PositionReportText, RESERVED)
}

fn validate_compressed_position_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(
        value,
        AprsTextField::CompressedPositionText,
        CompressedPositionText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::CompressedPositionText, RESERVED)
}

fn validate_mice_status_text(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"|~";

    validate_printable_ascii(
        value,
        AprsTextField::MiceStatusText,
        MiceStatusText::MAX_LEN,
        true,
    )?;
    validate_not_reserved(value, AprsTextField::MiceStatusText, RESERVED)?;
    if let Some(character @ ('`' | '\'')) = value.chars().next() {
        return Err(AprsTextError::ReservedCharacter {
            field: AprsTextField::MiceStatusText,
            index: 0,
            character,
        });
    }
    Ok(())
}

fn validate_object_name(value: &str) -> Result<(), AprsTextError> {
    validate_printable_ascii(value, AprsTextField::ObjectName, ObjectName::MAX_LEN, false)?;
    if value.ends_with(' ') {
        return Err(AprsTextError::AmbiguousObjectNamePadding {
            index: value.len() - 1,
        });
    }
    Ok(())
}

fn validate_item_name(value: &str) -> Result<(), AprsTextError> {
    const RESERVED: &[u8] = b"!_";

    validate_printable_ascii(value, AprsTextField::ItemName, ItemName::MAX_LEN, true)?;
    if value.len() < ItemName::MIN_LEN {
        return Err(AprsTextError::TooShort {
            field: AprsTextField::ItemName,
            minimum: ItemName::MIN_LEN,
            actual: value.len(),
        });
    }
    validate_not_reserved(value, AprsTextField::ItemName, RESERVED)
}

fn validate_printable_ascii(
    value: &str,
    field: AprsTextField,
    maximum: usize,
    allow_empty: bool,
) -> Result<(), AprsTextError> {
    if !allow_empty && value.is_empty() {
        return Err(AprsTextError::Empty { field });
    }

    if let Some((index, character)) = value
        .char_indices()
        .find(|(_, character)| !character.is_ascii())
    {
        return Err(AprsTextError::NonAscii {
            field,
            index,
            character,
        });
    }

    if value.len() > maximum {
        return Err(AprsTextError::TooLong {
            field,
            maximum,
            actual: value.len(),
        });
    }

    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| !(b' '..=b'~').contains(byte))
    {
        return Err(AprsTextError::NonPrintableAscii { field, index, byte });
    }

    Ok(())
}

fn validate_not_reserved(
    value: &str,
    field: AprsTextField,
    reserved: &[u8],
) -> Result<(), AprsTextError> {
    if let Some((index, byte)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| reserved.contains(byte))
    {
        return Err(AprsTextError::ReservedCharacter {
            field,
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

    #[test]
    fn addressee_preserves_aprs_special_conventions() -> TestResult {
        for value in [
            "N0CALL-15",
            "ALL",
            "QST",
            "CQ",
            "BLN3",
            "BLNQ",
            "BLN4WX",
            "NWS-WARN",
            "EMAIL",
        ] {
            assert_eq!(MessageAddressee::new(value)?.as_str(), value);
        }
        Ok(())
    }

    #[test]
    fn addressee_enforces_width_and_unpadded_form() -> TestResult {
        assert_eq!(MessageAddressee::new("123456789")?.as_str(), "123456789");
        assert_eq!(
            MessageAddressee::new(""),
            Err(AprsTextError::Empty {
                field: AprsTextField::MessageAddressee,
            })
        );
        assert_eq!(
            MessageAddressee::new("1234567890"),
            Err(AprsTextError::TooLong {
                field: AprsTextField::MessageAddressee,
                maximum: 9,
                actual: 10,
            })
        );
        for (value, index) in [(" N0CALL", 0), ("N0 CALL", 2), ("N0CALL ", 6)] {
            assert_eq!(
                MessageAddressee::new(value),
                Err(AprsTextError::AmbiguousAddresseePadding { index })
            );
        }
        Ok(())
    }

    #[test]
    fn message_text_enforces_aprs_character_and_length_rules() -> TestResult {
        assert_eq!(MessageText::new("")?.as_str(), "");
        let maximum = "x".repeat(MessageText::MAX_LEN);
        assert_eq!(MessageText::new(&maximum)?.as_str(), maximum);

        let too_long = "x".repeat(MessageText::MAX_LEN + 1);
        assert_eq!(
            MessageText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::MessageText,
                maximum: 67,
                actual: 68,
            })
        );

        for character in ['|', '~', '{'] {
            let value = format!("ok{character}");
            assert_eq!(
                MessageText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::MessageText,
                    index: 2,
                    character,
                })
            );
        }

        assert_eq!(MessageText::new("?APRSH N0QBF")?.as_str(), "?APRSH N0QBF");
        assert_eq!(
            MessageText::new("closing } is text")?.as_str(),
            "closing } is text"
        );
        Ok(())
    }

    #[test]
    fn message_control_text_constructors_preserve_validated_id() -> TestResult {
        let shortest = MessageId::new("1")?;
        assert_eq!(MessageText::acknowledgement(&shortest).as_str(), "ack1");
        assert_eq!(MessageText::rejection(&shortest).as_str(), "rej1");

        let longest = MessageId::new("A1234")?;
        assert_eq!(MessageText::acknowledgement(&longest).as_str(), "ackA1234");
        assert_eq!(MessageText::rejection(&longest).as_str(), "rejA1234");
        Ok(())
    }

    #[test]
    fn bulletin_text_allows_brace_but_keeps_channel_delimiters_reserved() -> TestResult {
        assert_eq!(
            BulletinText::new("AR_ASHLEY,{S9JbA")?.as_str(),
            "AR_ASHLEY,{S9JbA"
        );
        let maximum = "b".repeat(BulletinText::MAX_LEN);
        assert_eq!(BulletinText::new(&maximum)?.as_str(), maximum);

        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                BulletinText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::BulletinText,
                    index: 2,
                    character,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn status_text_enforces_untimestamped_rules() -> TestResult {
        assert_eq!(StatusText::new("")?.as_str(), "");
        let maximum = "s".repeat(StatusText::MAX_LEN);
        assert_eq!(StatusText::new(&maximum)?.as_str(), maximum);

        let too_long = "s".repeat(StatusText::MAX_LEN + 1);
        assert_eq!(
            StatusText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::StatusText,
                maximum: 62,
                actual: 63,
            })
        );
        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                StatusText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::StatusText,
                    index: 2,
                    character,
                })
            );
        }
        assert_eq!(
            StatusText::new("object {active}")?.as_str(),
            "object {active}"
        );
        Ok(())
    }

    #[test]
    fn timestamped_status_text_enforces_reduced_wire_limit() -> TestResult {
        assert_eq!(TimestampedStatusText::new("")?.as_str(), "");
        let maximum = "s".repeat(TimestampedStatusText::MAX_LEN);
        assert_eq!(TimestampedStatusText::new(&maximum)?.as_str(), maximum);

        let too_long = "s".repeat(TimestampedStatusText::MAX_LEN + 1);
        assert_eq!(
            TimestampedStatusText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::TimestampedStatusText,
                maximum: 55,
                actual: 56,
            })
        );
        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                TimestampedStatusText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::TimestampedStatusText,
                    index: 2,
                    character,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn weather_comment_preserves_printable_ascii_and_allows_empty() -> TestResult {
        assert_eq!(WeatherComment::new("")?.as_str(), "");
        assert_eq!(
            WeatherComment::new("Davis VP2 | {station} ~")?.as_str(),
            "Davis VP2 | {station} ~",
        );
        assert_eq!(
            WeatherComment::new("bad\n"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::WeatherComment,
                index: 3,
                byte: b'\n',
            }),
        );
        assert_eq!(
            WeatherComment::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::WeatherComment,
                index: 1,
                character: 'é',
            }),
        );
        for character in ['c', 's', 'g', 't', 'r', 'p', 'P', 'h', 'b', 'L', 'l'] {
            let value = format!("{character}omment");
            assert_eq!(
                WeatherComment::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::WeatherComment,
                    index: 0,
                    character,
                }),
            );
        }
        Ok(())
    }

    #[test]
    fn position_report_text_enforces_exact_wire_capacity() -> TestResult {
        assert_eq!(PositionReportText::new("")?.as_str(), "");

        let maximum = "x".repeat(PositionReportText::MAX_LEN);
        assert_eq!(PositionReportText::new(&maximum)?.as_str(), maximum);

        let too_long = "x".repeat(PositionReportText::MAX_LEN + 1);
        assert_eq!(
            PositionReportText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::PositionReportText,
                maximum: PositionReportText::MAX_LEN,
                actual: PositionReportText::MAX_LEN + 1,
            })
        );
        Ok(())
    }

    #[test]
    fn position_report_text_rejects_unrepresentable_characters() {
        for (value, index, byte) in [
            ("bad\r", 3, b'\r'),
            ("bad\n", 3, b'\n'),
            ("bad\u{7f}", 3, 0x7F),
        ] {
            assert_eq!(
                PositionReportText::new(value),
                Err(AprsTextError::NonPrintableAscii {
                    field: AprsTextField::PositionReportText,
                    index,
                    byte,
                })
            );
        }

        assert_eq!(
            PositionReportText::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::PositionReportText,
                index: 1,
                character: 'é',
            })
        );

        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                PositionReportText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::PositionReportText,
                    index: 2,
                    character,
                })
            );
        }
    }

    #[test]
    fn compressed_position_text_enforces_40_byte_capacity_without_truncation() -> TestResult {
        assert_eq!(CompressedPositionText::new("")?.as_str(), "");
        assert_eq!(
            CompressedPositionText::new("exact text, {unchanged}")?.as_str(),
            "exact text, {unchanged}",
        );

        let maximum = "c".repeat(CompressedPositionText::MAX_LEN);
        assert_eq!(CompressedPositionText::new(&maximum)?.as_str(), maximum);

        let too_long = "c".repeat(CompressedPositionText::MAX_LEN + 1);
        assert_eq!(
            CompressedPositionText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::CompressedPositionText,
                maximum: 40,
                actual: 41,
            })
        );
        Ok(())
    }

    #[test]
    fn compressed_position_text_rejects_unrepresentable_or_reserved_text() {
        assert_eq!(
            CompressedPositionText::new("bad\n"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::CompressedPositionText,
                index: 3,
                byte: b'\n',
            })
        );
        assert_eq!(
            CompressedPositionText::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::CompressedPositionText,
                index: 1,
                character: 'é',
            })
        );
        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                CompressedPositionText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::CompressedPositionText,
                    index: 2,
                    character,
                })
            );
        }
    }

    #[test]
    fn mice_status_text_enforces_247_byte_capacity_without_truncation() -> TestResult {
        assert_eq!(MiceStatusText::new("")?.as_str(), "");
        assert_eq!(
            MiceStatusText::new("status, exactly {as supplied}")?.as_str(),
            "status, exactly {as supplied}",
        );

        let maximum = "m".repeat(MiceStatusText::MAX_LEN);
        assert_eq!(MiceStatusText::new(&maximum)?.as_str(), maximum);

        let too_long = "m".repeat(MiceStatusText::MAX_LEN + 1);
        assert_eq!(
            MiceStatusText::new(&too_long),
            Err(AprsTextError::TooLong {
                field: AprsTextField::MiceStatusText,
                maximum: 247,
                actual: 248,
            })
        );
        Ok(())
    }

    #[test]
    fn mice_status_text_rejects_forbidden_prefixes_and_reserved_text() -> TestResult {
        for character in ['`', '\''] {
            assert_eq!(
                MiceStatusText::new(&format!("{character}telemetry")),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::MiceStatusText,
                    index: 0,
                    character,
                })
            );
        }
        assert_eq!(
            MiceStatusText::new(",ordinary status")?.as_str(),
            ",ordinary status"
        );
        assert_eq!(
            MiceStatusText::new("\x1dstatus"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::MiceStatusText,
                index: 0,
                byte: 0x1d,
            })
        );
        assert_eq!(
            MiceStatusText::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::MiceStatusText,
                index: 1,
                character: 'é',
            })
        );
        for character in ['|', '~'] {
            let value = format!("ok{character}");
            assert_eq!(
                MiceStatusText::new(&value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::MiceStatusText,
                    index: 2,
                    character,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn object_name_accepts_printable_ascii_and_enforces_width() -> TestResult {
        assert_eq!(ObjectName::new("A")?.as_str(), "A");
        assert_eq!(ObjectName::new("FIRE BASE")?.as_str(), "FIRE BASE");
        assert_eq!(ObjectName::new("|~{*_")?.as_str(), "|~{*_");
        assert_eq!(
            ObjectName::new(""),
            Err(AprsTextError::Empty {
                field: AprsTextField::ObjectName,
            })
        );
        assert_eq!(
            ObjectName::new("1234567890"),
            Err(AprsTextError::TooLong {
                field: AprsTextField::ObjectName,
                maximum: 9,
                actual: 10,
            })
        );
        for value in ["ABC ", "         "] {
            assert_eq!(
                ObjectName::new(value),
                Err(AprsTextError::AmbiguousObjectNamePadding {
                    index: value.len() - 1,
                })
            );
        }
        Ok(())
    }

    #[test]
    fn item_name_accepts_three_to_nine_printable_ascii_bytes() -> TestResult {
        assert_eq!(ItemName::new("ABC")?.as_str(), "ABC");
        assert_eq!(ItemName::new("FIRE BASE")?.as_str(), "FIRE BASE");
        assert_eq!(ItemName::new("A B")?.as_str(), "A B");

        for value in ["", "A", "AB"] {
            assert_eq!(
                ItemName::new(value),
                Err(AprsTextError::TooShort {
                    field: AprsTextField::ItemName,
                    minimum: 3,
                    actual: value.len(),
                })
            );
        }
        assert_eq!(
            ItemName::new("1234567890"),
            Err(AprsTextError::TooLong {
                field: AprsTextField::ItemName,
                maximum: 9,
                actual: 10,
            })
        );
        Ok(())
    }

    #[test]
    fn item_name_rejects_delimiters_and_non_printable_text() {
        for (value, index, character) in [("A!B", 1, '!'), ("AB_", 2, '_')] {
            assert_eq!(
                ItemName::new(value),
                Err(AprsTextError::ReservedCharacter {
                    field: AprsTextField::ItemName,
                    index,
                    character,
                })
            );
        }
        assert_eq!(
            ItemName::new("A\tB"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::ItemName,
                index: 1,
                byte: b'\t',
            })
        );
        assert_eq!(
            ItemName::new("ABé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::ItemName,
                index: 2,
                character: 'é',
            })
        );
    }

    #[test]
    fn every_type_rejects_unicode_and_control_bytes_precisely() {
        assert_eq!(
            MessageText::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::MessageText,
                index: 1,
                character: 'é',
            })
        );
        assert_eq!(
            StatusText::new("ok\n"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::StatusText,
                index: 2,
                byte: b'\n',
            })
        );
        assert_eq!(
            TimestampedStatusText::new("Aé"),
            Err(AprsTextError::NonAscii {
                field: AprsTextField::TimestampedStatusText,
                index: 1,
                character: 'é',
            })
        );
        assert_eq!(
            ObjectName::new("A\u{7f}"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::ObjectName,
                index: 1,
                byte: 0x7f,
            })
        );
        assert!(matches!(
            MessageAddressee::new("N0C\tALL"),
            Err(AprsTextError::NonPrintableAscii {
                field: AprsTextField::MessageAddressee,
                index: 3,
                byte: b'\t',
            })
        ));
    }

    #[test]
    fn conversion_and_display_traits_preserve_validated_value() -> TestResult {
        let addressee = MessageAddressee::try_from("BLN4WX")?;
        assert_eq!(addressee.to_string(), "BLN4WX");
        assert_eq!(addressee.as_ref(), "BLN4WX");

        let text = MessageText::try_from(String::from("hello"))?;
        assert_eq!(text.to_string(), "hello");
        let owned: String = text.into();
        assert_eq!(owned, "hello");

        let item = ItemName::try_from(String::from("FIRE 1"))?;
        assert_eq!(item.to_string(), "FIRE 1");
        let owned: String = item.into();
        assert_eq!(owned, "FIRE 1");
        Ok(())
    }
}
