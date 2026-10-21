//! APRS message packets and ack/rej classification (APRS 1.0.1 ch. 14).

use crate::error::AprsError;
use crate::packet::MessageKind;
use crate::text::decode_wire_ascii;
use crate::{AprsWireTextField, BulletinText, MessageAddressee, MessageId, MessageText};

/// A rejected bulletin selector or message destination role.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MessageValueError {
    /// A general or group bulletin number exceeded one decimal digit.
    #[error("bulletin number must be 0 through 9; got {0}")]
    InvalidBulletinNumber(u8),
    /// An announcement selector was outside uppercase ASCII letters.
    #[error("announcement selector must be A through Z; got {0:?}")]
    InvalidAnnouncementLetter(char),
    /// A group was empty, exceeded five bytes or contained a non-visible-ASCII byte.
    #[error("bulletin group must contain one through five visible ASCII characters")]
    InvalidBulletinGroup,
    /// A direct-message construction selected a bulletin destination.
    #[error("direct messages require a non-bulletin addressee")]
    ExpectedDirectAddressee,
    /// A bulletin construction selected a direct-message destination.
    #[error("bulletins require a bulletin or announcement addressee")]
    ExpectedBulletinAddressee,
    /// The unpadded destination failed its text grammar.
    #[error("invalid message destination: {0}")]
    Addressee(#[from] crate::AprsTextError),
    /// A reply acknowledgement was requested without this message's own ID.
    #[error("reply acknowledgement requires a message ID")]
    MissingMessageId,
}

/// One decimal bulletin selector, `0` through `9`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BulletinNumber(u8);

impl BulletinNumber {
    /// Checks the single-digit bulletin number.
    ///
    /// # Errors
    /// Returns [`MessageValueError::InvalidBulletinNumber`] above nine.
    pub const fn new(value: u8) -> Result<Self, MessageValueError> {
        if value > 9 {
            return Err(MessageValueError::InvalidBulletinNumber(value));
        }
        Ok(Self(value))
    }

    /// Returns the numeric selector.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for BulletinNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::fmt::Binary for BulletinNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Binary::fmt(&self.0, formatter)
    }
}

impl std::fmt::Octal for BulletinNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Octal::fmt(&self.0, formatter)
    }
}

impl std::fmt::LowerHex for BulletinNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl std::fmt::UpperHex for BulletinNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::UpperHex::fmt(&self.0, formatter)
    }
}

impl TryFrom<u8> for BulletinNumber {
    type Error = MessageValueError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BulletinNumber> for u8 {
    fn from(value: BulletinNumber) -> Self {
        value.value()
    }
}

/// One uppercase ASCII announcement selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnnouncementLetter(char);

impl AnnouncementLetter {
    /// Checks the announcement letter.
    ///
    /// # Errors
    /// Returns [`MessageValueError::InvalidAnnouncementLetter`] outside `A..=Z`.
    pub const fn new(value: char) -> Result<Self, MessageValueError> {
        if !value.is_ascii_uppercase() {
            return Err(MessageValueError::InvalidAnnouncementLetter(value));
        }
        Ok(Self(value))
    }

    /// Returns the uppercase ASCII selector.
    #[must_use]
    pub const fn value(self) -> char {
        self.0
    }
}

impl std::fmt::Display for AnnouncementLetter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl TryFrom<char> for AnnouncementLetter {
    type Error = MessageValueError;
    fn try_from(value: char) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AnnouncementLetter> for char {
    fn from(value: AnnouncementLetter) -> Self {
        value.value()
    }
}

/// The one-to-five-character visible-ASCII name after a group bulletin number.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BulletinGroup(String);

impl BulletinGroup {
    /// Checks a group name without its `BLN` prefix or bulletin number.
    ///
    /// # Errors
    /// Returns [`MessageValueError::InvalidBulletinGroup`] for another width or alphabet.
    pub fn new(value: &str) -> Result<Self, MessageValueError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    fn validate(value: &str) -> Result<(), MessageValueError> {
        if !(1..=5).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(MessageValueError::InvalidBulletinGroup);
        }
        Ok(())
    }

    /// Borrows the checked group name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BulletinGroup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for BulletinGroup {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for BulletinGroup {
    type Err = MessageValueError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for BulletinGroup {
    type Error = MessageValueError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<BulletinGroup> for String {
    fn from(value: BulletinGroup) -> Self {
        value.0
    }
}

/// A checked addressee selecting direct-message text and acknowledgement grammar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirectMessageAddressee(MessageAddressee);

impl DirectMessageAddressee {
    /// Validates the unpadded destination and its direct-message role.
    ///
    /// # Errors
    /// Returns [`MessageValueError::Addressee`] for invalid text or
    /// [`MessageValueError::ExpectedDirectAddressee`] for bulletin destinations.
    pub fn new(value: &str) -> Result<Self, MessageValueError> {
        MessageAddressee::new(value)?.try_into()
    }

    /// Borrows the checked unpadded destination.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Borrows the common message destination representation.
    #[must_use]
    pub const fn addressee(&self) -> &MessageAddressee {
        &self.0
    }
}

impl TryFrom<MessageAddressee> for DirectMessageAddressee {
    type Error = MessageValueError;
    fn try_from(value: MessageAddressee) -> Result<Self, Self::Error> {
        if is_bulletin_addressee(value.as_str()) {
            return Err(MessageValueError::ExpectedDirectAddressee);
        }
        Ok(Self(value))
    }
}

impl From<DirectMessageAddressee> for MessageAddressee {
    fn from(value: DirectMessageAddressee) -> Self {
        value.0
    }
}

impl AsRef<str> for DirectMessageAddressee {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for DirectMessageAddressee {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for DirectMessageAddressee {
    type Err = MessageValueError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// A checked addressee selecting bulletin or announcement text grammar.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BulletinAddressee(MessageAddressee);

impl BulletinAddressee {
    /// Validates a general, group, announcement or weather-bulletin destination.
    ///
    /// # Errors
    /// Returns [`MessageValueError::Addressee`] for invalid text or
    /// [`MessageValueError::ExpectedBulletinAddressee`] for another destination.
    pub fn new(value: &str) -> Result<Self, MessageValueError> {
        MessageAddressee::new(value)?.try_into()
    }

    /// Borrows the checked unpadded destination.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Borrows the common message destination representation.
    #[must_use]
    pub const fn addressee(&self) -> &MessageAddressee {
        &self.0
    }
}

impl TryFrom<MessageAddressee> for BulletinAddressee {
    type Error = MessageValueError;
    fn try_from(value: MessageAddressee) -> Result<Self, Self::Error> {
        if !is_bulletin_addressee(value.as_str()) {
            return Err(MessageValueError::ExpectedBulletinAddressee);
        }
        Ok(Self(value))
    }
}

impl From<BulletinAddressee> for MessageAddressee {
    fn from(value: BulletinAddressee) -> Self {
        value.0
    }
}

impl AsRef<str> for BulletinAddressee {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for BulletinAddressee {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for BulletinAddressee {
    type Err = MessageValueError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// A complete acknowledgement or rejection control frame and its validated ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageControl {
    /// The peer acknowledged the identified message.
    Acknowledgement(MessageId),
    /// The peer rejected the identified message.
    Rejection(MessageId),
}

impl MessageControl {
    /// Recognizes `ack` or `rej` followed by one through five alphanumerics.
    /// Trailing spaces and terminal CR/LF are ignored on receive.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let trimmed = text.trim_end_matches(['\r', '\n', ' ']);
        trimmed.strip_prefix("ack").map_or_else(
            || {
                trimmed
                    .strip_prefix("rej")
                    .and_then(|id| MessageId::new(id).ok())
                    .map(Self::Rejection)
            },
            |id| MessageId::new(id).ok().map(Self::Acknowledgement),
        )
    }

    /// Borrows the control frame's checked message ID.
    #[must_use]
    pub const fn message_id(&self) -> &MessageId {
        match self {
            Self::Acknowledgement(id) | Self::Rejection(id) => id,
        }
    }

    /// Returns whether this is an acknowledgement rather than a rejection.
    #[must_use]
    pub const fn is_ack(&self) -> bool {
        matches!(self, Self::Acknowledgement(_))
    }
}

/// Maximum APRS message text length in bytes (APRS 1.0.1 §14).
pub const MAX_APRS_MESSAGE_TEXT_LEN: usize = 67;

/// An APRS message (data type `:`) addressed to a specific station or
/// group.
///
/// Format: `:ADDRESSEE:message text{ID` or, with the APRS 1.2 reply-ack
/// extension, `:ADDRESSEE:message text{MM}AA` where `MM` is this
/// message's ID and `AA` is an ack for a previously-received message.
/// A bare `:ADDRESSEE:message text{MM}` (nothing after the `}`) is the
/// reply-ack capability advertisement: id `MM`, no piggybacked ack.
/// - Addressee is exactly 9 characters, space-padded.
/// - Message text follows the second `:`.
/// - Optional message ID after `{` (for ack/rej).
/// - Optional reply-ack after `}` (APRS 1.2).
///
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsMessage {
    /// Destination callsign (up to 9 chars, trimmed).
    addressee: MessageAddressee,
    /// Message text content.
    text: String,
    /// Optional message sequence number (for ack/rej tracking).
    message_id: Option<MessageId>,
    /// Optional APRS 1.2 reply-ack: when the sender bundles an
    /// acknowledgement for a previously-received message into a new
    /// outgoing message. Format on wire is `{MM}AA` where `AA` is the
    /// acknowledged msgno. A bare `{MM}` trailer (the reply-ack
    /// capability advertisement) leaves this `None`.
    reply_ack: Option<MessageId>,
}

impl AprsMessage {
    /// Creates a direct message with destination-specific text grammar.
    #[must_use]
    pub fn direct(
        addressee: DirectMessageAddressee,
        text: MessageText,
        message_id: Option<MessageId>,
    ) -> Self {
        Self {
            addressee: addressee.into(),
            text: text.into(),
            message_id,
            reply_ack: None,
        }
    }

    /// Creates a bulletin without acknowledgement trailers.
    #[must_use]
    pub fn bulletin(addressee: BulletinAddressee, text: BulletinText) -> Self {
        Self {
            addressee: addressee.into(),
            text: text.into(),
            message_id: None,
            reply_ack: None,
        }
    }

    /// Adds a reply acknowledgement to a direct message with its own ID.
    ///
    /// # Errors
    /// Returns [`MessageValueError::MissingMessageId`] when this message has no ID.
    pub fn with_reply_ack(mut self, acknowledged: MessageId) -> Result<Self, MessageValueError> {
        if self.message_id.is_none() {
            return Err(MessageValueError::MissingMessageId);
        }
        self.reply_ack = Some(acknowledged);
        Ok(self)
    }

    /// Borrows the unpadded destination.
    #[must_use]
    pub const fn addressee(&self) -> &MessageAddressee {
        &self.addressee
    }
    /// Borrows the received text, including literal malformed trailers.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Borrows this message's validated ID when present.
    #[must_use]
    pub const fn message_id(&self) -> Option<&MessageId> {
        self.message_id.as_ref()
    }
    /// Borrows the piggybacked acknowledgement ID when present.
    #[must_use]
    pub const fn reply_ack(&self) -> Option<&MessageId> {
        self.reply_ack.as_ref()
    }

    /// Decodes the padded nine-byte destination and optional ID/reply-ack trailer.
    /// Malformed trailers are retained as literal printable-ASCII received text.
    /// Bulletin bodies retain braces as text. Terminal CR/LF is removed for TNC
    /// receive compatibility. `PARM.` and `UNIT.` accept their chapter-13
    /// 68-byte maximum; other ordinary bodies are limited to 67 bytes. Weather
    /// bulletins retain their variable-width chapter-14 bodies.
    ///
    /// # Errors
    /// Returns [`AprsError::InvalidFormat`] for missing framing or invalid padding,
    /// and [`AprsError::InvalidTextByte`] for non-ASCII destination or body bytes.
    pub fn from_bytes(info: &[u8]) -> Result<Self, AprsError> {
        parse_aprs_message(info)
    }
    /// Classify this message by addressee / text pattern per APRS 1.0.1
    /// §14 and bulletin conventions.
    ///
    /// The classification ladder is:
    ///
    /// 1. **`NwsBulletin`**: addressee starts with `NWS-`, `SKY-`,
    ///    `CWA-`, or `BOM-` per APRS 1.0.1 §14 p.74.
    /// 2. **`Bulletin { number }`**: addressee is exactly `BLN<digit>`
    ///    (general bulletin) per APRS 1.0.1 §14 p.73.
    /// 3. **`Announcement { letter }`**: addressee is exactly
    ///    `BLN<uppercase letter>` per APRS 1.0.1 §14 p.73.
    /// 4. **`GroupBulletin { number, group }`**: `BLN`, a decimal digit and
    ///    one through five visible ASCII group-name bytes (APRS 1.0.1 p.74).
    /// 5. **`AckRej`**: for a regular station addressee, text begins with
    ///    `ack`/`rej` + 1-5 alnum.
    /// 6. **`Direct`**: anything else.
    #[must_use]
    pub fn kind(&self) -> MessageKind {
        let addr = self.addressee.as_str();
        // Bulletin addressees select bulletin text semantics regardless of
        // body spelling. In particular, a legitimate bulletin body such as
        // `ack123` is not an acknowledgement control frame.
        if let Some(kind) = bulletin_kind_from_addressee(addr) {
            return kind;
        }

        // Control frames use a regular station addressee; the text shape
        // disambiguates them from other directed messages.
        if self.message_id.is_none()
            && self.reply_ack.is_none()
            && MessageControl::from_text(&self.text).is_some()
        {
            return MessageKind::AckRej;
        }
        MessageKind::Direct
    }
}

/// Classify the APRS bulletin/announcement family using only its addressee.
fn bulletin_kind_from_addressee(addr: &str) -> Option<MessageKind> {
    if addr.starts_with("NWS-")
        || addr.starts_with("SKY-")
        || addr.starts_with("CWA-")
        || addr.starts_with("BOM-")
    {
        return Some(MessageKind::NwsBulletin);
    }

    let rest = addr.strip_prefix("BLN")?;
    if rest.len() == 1
        && let Some(byte) = rest.bytes().next()
    {
        if byte.is_ascii_digit() {
            return Some(MessageKind::Bulletin {
                number: BulletinNumber::new(byte - b'0').ok()?,
            });
        }
        if byte.is_ascii_uppercase() {
            return Some(MessageKind::Announcement {
                letter: AnnouncementLetter::new(char::from(byte)).ok()?,
            });
        }
    }
    if (2..=6).contains(&rest.len()) {
        let number = BulletinNumber::new(rest.bytes().next()?.checked_sub(b'0')?).ok()?;
        let group = BulletinGroup::new(rest.get(1..)?).ok()?;
        return Some(MessageKind::GroupBulletin { number, group });
    }
    None
}

/// Parse an APRS message (`:ADDRESSEE:text{id`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for malformed input: missing
/// leading `:`, absence of the second `:`, or addressee shorter than 9
/// characters.
pub(crate) fn parse_aprs_message(info: &[u8]) -> Result<AprsMessage, AprsError> {
    // Minimum: : + 9 char addressee + : + at least 0 text = 11 bytes
    if info.first() != Some(&b':') {
        return Err(AprsError::InvalidFormat);
    }

    // Addressee is exactly 9 characters (space-padded)
    let addressee_raw = info.get(1..10).ok_or(AprsError::InvalidFormat)?;
    let addressee = MessageAddressee::new(
        decode_wire_ascii(AprsWireTextField::MessageAddressee, addressee_raw)?
            .trim_end_matches(' '),
    )
    .map_err(|_| AprsError::InvalidFormat)?;

    if info.get(10) != Some(&b':') {
        return Err(AprsError::InvalidFormat);
    }

    let body = info.get(11..).unwrap_or(&[]);
    let body_str = decode_wire_ascii(AprsWireTextField::MessageBody, body)?;
    let trimmed_body = body_str.trim_end_matches(['\r', '\n']);
    if trimmed_body
        .bytes()
        .any(|byte| !(b' '..=b'~').contains(&byte))
    {
        return Err(AprsError::InvalidFormat);
    }

    // Split on `{` for message ID and APRS 1.2 reply-ack extension only for
    // directed messages. APRS 1.0.1 reserves `{` in directed-message text
    // because it introduces this trailer, but explicitly permits it as
    // ordinary bulletin/announcement text. Applying the trailer grammar to a
    // bulletin would silently truncate bodies such as `AR_ASHLEY,{S9JbA`.
    // Four possible trailer forms to recognise (checked from richest):
    //   1. `text{MM}AA`  is reply-ack: MM is this msg's id, AA is ack
    //   2. `text{MM}`    is a reply-ack capability advertisement: MM is
    //                      this msg's id, no ack piggybacked
    //   3. `text{MM`     is a plain message id
    //   4. `text`        has no trailer
    let parsed = if is_bulletin_addressee(addressee.as_str()) {
        ParsedMessageBody {
            text: trimmed_body,
            message_id: None,
            reply_ack: None,
        }
    } else {
        parse_message_trailer(trimmed_body)
    };
    let is_weather_bulletin = matches!(
        bulletin_kind_from_addressee(addressee.as_str()),
        Some(MessageKind::NwsBulletin)
    );
    let maximum = if !is_bulletin_addressee(addressee.as_str())
        && (parsed.text.starts_with("PARM.") || parsed.text.starts_with("UNIT."))
    {
        68
    } else {
        MAX_APRS_MESSAGE_TEXT_LEN
    };
    if !is_weather_bulletin && parsed.text.len() > maximum {
        return Err(AprsError::MessageTooLong {
            actual: parsed.text.len(),
            maximum,
        });
    }

    Ok(AprsMessage {
        addressee,
        text: parsed.text.to_owned(),
        message_id: parsed.message_id.map(MessageId::new).transpose()?,
        reply_ack: parsed.reply_ack.map(MessageId::new).transpose()?,
    })
}

/// Whether an addressee selects APRS bulletin/announcement text semantics.
fn is_bulletin_addressee(addressee: &str) -> bool {
    bulletin_kind_from_addressee(addressee).is_some()
}

#[derive(Debug, Clone, Copy)]
struct ParsedMessageBody<'text> {
    text: &'text str,
    message_id: Option<&'text str>,
    reply_ack: Option<&'text str>,
}

/// Parse the optional `{MM}AA` / `{MM}` / `{MM` trailer of an APRS
/// message body.
///
/// Borrows text and IDs only when the trailer has the exact well-formed shape. Per
/// the APRS 1.2 reply-ack addendum, `{MM}` with nothing after the
/// brace is the reply-ack capability advertisement: the message id is
/// `MM` and no ack is piggybacked.
fn parse_message_trailer(body: &str) -> ParsedMessageBody<'_> {
    let plain = ParsedMessageBody {
        text: body,
        message_id: None,
        reply_ack: None,
    };
    let Some(brace_idx) = body.rfind('{') else {
        return plain;
    };
    let Some(after_brace) = body.get(brace_idx + 1..) else {
        return plain;
    };
    let Some(prefix) = body.get(..brace_idx) else {
        return plain;
    };

    // APRS 1.2 reply-ack: `MM}AA` where MM is 1-5 alnum and AA is
    // either 1-5 alnum (piggybacked ack) or empty (capability
    // advertisement). Any other AA shape is malformed and falls
    // through to the plain-text case.
    if let Some(close_idx) = after_brace.find('}') {
        let mm = after_brace.get(..close_idx).unwrap_or("");
        let aa = after_brace.get(close_idx + 1..).unwrap_or("");
        if (1..=5).contains(&mm.len()) && mm.bytes().all(|b| b.is_ascii_alphanumeric()) {
            if aa.is_empty() {
                return ParsedMessageBody {
                    text: prefix,
                    message_id: Some(mm),
                    reply_ack: None,
                };
            }
            if (1..=5).contains(&aa.len()) && aa.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return ParsedMessageBody {
                    text: prefix,
                    message_id: Some(mm),
                    reply_ack: Some(aa),
                };
            }
        }
    }

    // Plain message id: `MM` (1-5 alnum, end of string).
    if (1..=5).contains(&after_brace.len())
        && after_brace.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return ParsedMessageBody {
            text: prefix,
            message_id: Some(after_brace),
            reply_ack: None,
        };
    }

    // Neither pattern matched; treat whole body as plain text.
    plain
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    // ---- Message parsing ----

    #[test]
    fn parse_message_basic() -> TestResult {
        let info = b":N0CALL   :Hello World{123";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.addressee().as_str(), "N0CALL");
        assert_eq!(msg.text, "Hello World");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("123"));
        Ok(())
    }

    #[test]
    fn parse_message_no_id() -> TestResult {
        let info = b":KQ4NIT   :Test message";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.addressee().as_str(), "KQ4NIT");
        assert_eq!(msg.text, "Test message");
        assert_eq!(msg.message_id, None);
        Ok(())
    }

    #[test]
    fn parse_bulletin_keeps_brace_suffix_as_literal_text() -> TestResult {
        let msg = parse_aprs_message(b":NWS-WARN :AR_ASHLEY,{S9JbA")?;
        assert_eq!(msg.kind(), MessageKind::NwsBulletin);
        assert_eq!(msg.text, "AR_ASHLEY,{S9JbA");
        assert_eq!(msg.message_id, None);
        assert_eq!(msg.reply_ack, None);

        let announcement = parse_aprs_message(b":BLNA     :VERSION {A1")?;
        assert_eq!(
            announcement.kind(),
            MessageKind::Announcement {
                letter: AnnouncementLetter::new('A')?
            }
        );
        assert_eq!(announcement.text, "VERSION {A1");
        assert_eq!(announcement.message_id, None);

        let control_like = parse_aprs_message(b":BLN3     :ack123")?;
        assert_eq!(
            control_like.kind(),
            MessageKind::Bulletin {
                number: BulletinNumber::new(3)?
            },
            "a bulletin addressee takes precedence over ack-like body text"
        );
        Ok(())
    }

    #[test]
    fn parse_message_ack() -> TestResult {
        let info = b":N0CALL   :ack123";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "ack123");
        Ok(())
    }

    #[test]
    fn parse_message_too_short() {
        assert!(
            parse_aprs_message(b":SHORT:hi").is_err(),
            "short input rejected",
        );
    }

    #[test]
    fn parse_message_rejects_non_ascii_addressee_without_replacement() -> TestResult {
        let mut info = b":N0CALL   :Hello".to_vec();
        *info
            .get_mut(1)
            .ok_or("static message fixture must contain its first addressee byte")? = 0xFF;

        assert_eq!(
            parse_aprs_message(&info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::MessageAddressee,
                index: 0,
                byte: 0xFF,
            }),
        );
        Ok(())
    }

    #[test]
    fn parse_message_rejects_non_ascii_body_without_replacement() -> TestResult {
        let mut info = b":N0CALL   :Hello".to_vec();
        *info
            .get_mut(11)
            .ok_or("static message fixture must contain its first body byte")? = 0xFF;

        assert_eq!(
            parse_aprs_message(&info),
            Err(AprsError::InvalidTextByte {
                field: AprsWireTextField::MessageBody,
                index: 0,
                byte: 0xFF,
            }),
        );
        Ok(())
    }

    #[test]
    fn parse_message_does_not_misinterpret_brace_in_text() -> TestResult {
        // Regression: a braced word that cannot be a message id
        // ("sooner" is 6 chars, above the 1-5 alnum limit) must NOT be
        // stripped from the text or produce a message id.
        let info = b":N0CALL   :reply {sooner}";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "reply {sooner}");
        assert_eq!(msg.message_id, None);
        Ok(())
    }

    #[test]
    fn parse_message_trailing_braced_word_is_advertisement() -> TestResult {
        // `{soon}` is on-wire indistinguishable from the APRS 1.2
        // reply-ack capability advertisement (`{MM}` with a 1-5 alnum
        // id and empty ack), so it parses as message id "soon".
        let info = b":N0CALL   :reply {soon}";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "reply ");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("soon"));
        assert_eq!(msg.reply_ack, None);
        Ok(())
    }

    #[test]
    fn parse_message_accepts_valid_id_with_text_containing_brace() -> TestResult {
        let info = b":N0CALL   :json {foo}{42";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "json {foo}");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("42"));
        Ok(())
    }

    #[test]
    fn parse_message_reply_ack() -> TestResult {
        let info = b":N0CALL   :Hello back{3}7";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "Hello back");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("3"));
        assert_eq!(msg.reply_ack().map(MessageId::as_str), Some("7"));
        Ok(())
    }

    #[test]
    fn parse_message_plain_id_no_reply_ack() -> TestResult {
        let info = b":N0CALL   :Hello{3";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "Hello");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("3"));
        assert_eq!(msg.reply_ack, None);
        Ok(())
    }

    #[test]
    fn parse_message_reply_ack_advertisement() -> TestResult {
        // APRS 1.2 reply-ack addendum: `{MM}` with nothing after the
        // brace advertises reply-ack capability: this message's id is
        // MM and no ack is piggybacked.
        let info = b":N0CALL   :hi{05}";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "hi");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("05"));
        assert_eq!(msg.reply_ack, None);
        Ok(())
    }

    #[test]
    fn parse_message_reply_ack_invalid_trailing_junk_stays_text() -> TestResult {
        // The post-`}` segment "xx!" is not 1-5 alphanumerics, so the
        // trailer is malformed: the whole body stays plain text and no
        // message id is extracted.
        let info = b":N0CALL   :hi{05}xx!";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "hi{05}xx!");
        assert_eq!(msg.message_id, None);
        assert_eq!(msg.reply_ack, None);
        Ok(())
    }

    // ---- MessageKind classification ----

    #[test]
    fn message_kind_direct() -> TestResult {
        let msg = AprsMessage::from_bytes(b":N0CALL   :hello")?;
        assert_eq!(msg.kind(), MessageKind::Direct);
        Ok(())
    }

    #[test]
    fn message_kind_numeric_bulletin() -> TestResult {
        let msg = AprsMessage::from_bytes(b":BLN3     :event update")?;
        assert_eq!(
            msg.kind(),
            MessageKind::Bulletin {
                number: BulletinNumber::new(3)?
            }
        );
        Ok(())
    }

    #[test]
    fn message_kind_group_bulletin() -> TestResult {
        let msg = AprsMessage::from_bytes(b":BLN4WX   :weather watch")?;
        assert_eq!(
            msg.kind(),
            MessageKind::GroupBulletin {
                number: BulletinNumber::new(4)?,
                group: BulletinGroup::new("WX")?,
            }
        );
        Ok(())
    }

    #[test]
    fn message_kind_announcement() -> TestResult {
        for letter in 'A'..='Z' {
            let msg = AprsMessage::from_bytes(
                format!(":BLN{letter}     :evening net at 8pm").as_bytes(),
            )?;
            assert_eq!(
                msg.kind(),
                MessageKind::Announcement {
                    letter: AnnouncementLetter::new(letter)?
                },
                "BLN{letter} should classify as Announcement",
            );
        }
        Ok(())
    }

    #[test]
    fn group_bulletin_accepts_five_group_bytes_and_keeps_literal_braces() -> TestResult {
        let msg = AprsMessage::from_bytes(b":BLN4ABCDE:wx watch{A1")?;
        assert_eq!(
            msg.kind(),
            MessageKind::GroupBulletin {
                number: BulletinNumber::new(4)?,
                group: BulletinGroup::new("ABCDE")?,
            },
        );
        assert_eq!(msg.text(), "wx watch{A1");
        assert_eq!(msg.message_id(), None);
        assert_eq!(
            AprsMessage::from_bytes(b":BLNWX    :weather")?.kind(),
            MessageKind::Direct
        );
        assert!(BulletinGroup::new("ABCDEF").is_err());
        assert_eq!(BulletinGroup::new("WX_!")?.as_str(), "WX_!");
        let owned = String::from("WX_!");
        let allocation = owned.as_ptr();
        let group = BulletinGroup::try_from(owned)?;
        assert_eq!(
            group.as_str().as_ptr(),
            allocation,
            "owned conversion copied the group"
        );
        assert!(BulletinGroup::try_from(String::from("ABCDEF")).is_err());
        Ok(())
    }

    #[test]
    fn message_kind_nws_bulletin() -> TestResult {
        let msg = AprsMessage::from_bytes(b":NWS-TOR  :tornado warning")?;
        assert_eq!(msg.kind(), MessageKind::NwsBulletin);
        Ok(())
    }

    #[test]
    fn message_kind_ack_rej_frame() -> TestResult {
        let msg = AprsMessage::from_bytes(b":N0CALL   :ack42")?;
        assert_eq!(msg.kind(), MessageKind::AckRej);
        assert_eq!(
            AprsMessage::from_bytes(b":N0CALL   :ack42{99")?.kind(),
            MessageKind::Direct
        );
        assert_eq!(
            AprsMessage::from_bytes(b":N0CALL   :ack42{99}1")?.kind(),
            MessageKind::Direct
        );
        Ok(())
    }

    #[test]
    fn message_construction_checks_destination_role_and_reply_ack_precondition() -> TestResult {
        assert!(matches!(
            DirectMessageAddressee::new("BLN4ABCDE"),
            Err(MessageValueError::ExpectedDirectAddressee)
        ));
        assert!(matches!(
            BulletinAddressee::new("N0CALL"),
            Err(MessageValueError::ExpectedBulletinAddressee)
        ));
        let bulletin = AprsMessage::bulletin(
            BulletinAddressee::new("BLN4ABCDE")?,
            BulletinText::new("literal{A")?,
        );
        assert_eq!(bulletin.text(), "literal{A");
        assert_eq!(bulletin.message_id(), None);
        let direct = AprsMessage::direct(
            DirectMessageAddressee::new("N0CALL")?,
            MessageText::new("hello")?,
            None,
        );
        assert!(matches!(
            direct.with_reply_ack(MessageId::new("42")?),
            Err(MessageValueError::MissingMessageId)
        ));
        let direct = AprsMessage::direct(
            DirectMessageAddressee::new("N0CALL")?,
            MessageText::new("hello")?,
            Some(MessageId::new("1")?),
        )
        .with_reply_ack(MessageId::new("42")?)?;
        assert_eq!(direct.reply_ack().map(MessageId::as_str), Some("42"));
        assert!(BulletinNumber::new(10).is_err());
        assert!(AnnouncementLetter::new('a').is_err());
        assert!(AnnouncementLetter::new('É').is_err());
        Ok(())
    }

    #[test]
    fn received_message_text_enforces_printability_and_contextual_lengths() -> TestResult {
        assert!(matches!(
            AprsMessage::from_bytes(b":N0CALL   :hello\0world"),
            Err(AprsError::InvalidFormat)
        ));
        let prefix = ":N0CALL   :";
        let maximum = "x".repeat(67);
        assert_eq!(
            AprsMessage::from_bytes(format!("{prefix}{maximum}{{12345").as_bytes())?.text(),
            maximum
        );
        assert!(matches!(
            AprsMessage::from_bytes(format!("{prefix}{maximum}x").as_bytes()),
            Err(AprsError::MessageTooLong {
                actual: 68,
                maximum: 67
            })
        ));
        let definition = format!("PARM.{}", "x".repeat(63));
        assert_eq!(
            AprsMessage::from_bytes(format!("{prefix}{definition}").as_bytes())?.text(),
            definition
        );
        assert_eq!(
            AprsMessage::from_bytes(format!(":NWS-WARN :{maximum}x").as_bytes())?
                .text()
                .len(),
            68
        );
        Ok(())
    }

    // ---- classify_ack_rej ----

    #[test]
    fn classify_ack_rej_ack_numeric() -> TestResult {
        for id in ["42", "1", "12345", "ABC"] {
            let control =
                MessageControl::from_text(&format!("ack{id}")).ok_or("ack not recognized")?;
            assert!(control.is_ack());
            assert_eq!(control.message_id().as_str(), id);
        }
        Ok(())
    }

    #[test]
    fn classify_ack_rej_rej() -> TestResult {
        for id in ["42", "1"] {
            let control =
                MessageControl::from_text(&format!("rej{id}")).ok_or("rej not recognized")?;
            assert!(!control.is_ack());
            assert_eq!(control.message_id().as_str(), id);
        }
        Ok(())
    }

    #[test]
    fn classify_ack_rej_rejects_non_control() {
        assert_eq!(MessageControl::from_text("acknowledge receipt"), None);
        assert_eq!(MessageControl::from_text("rejection letter"), None);
        assert_eq!(MessageControl::from_text("ack with space"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_overlong_id() {
        assert_eq!(MessageControl::from_text("ack123456"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_empty_id() {
        assert_eq!(MessageControl::from_text("ack"), None);
        assert_eq!(MessageControl::from_text("rej"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_non_alnum() {
        assert_eq!(MessageControl::from_text("ack-12"), None);
        assert_eq!(MessageControl::from_text("ack 42"), None);
    }

    #[test]
    fn classify_ack_rej_strips_trailing_whitespace() -> TestResult {
        assert_eq!(
            MessageControl::from_text("ack42\r\n"),
            Some(MessageControl::Acknowledgement(MessageId::new("42")?))
        );
        Ok(())
    }
}
