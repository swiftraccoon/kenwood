//! APRS message packets and ack/rej classification (APRS 1.0.1 ch. 14).

use crate::error::AprsError;
use crate::packet::MessageKind;
use crate::text::decode_wire_ascii;

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
/// The TH-D75 displays received messages on-screen and can store
/// up to 100 messages in the station list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsMessage {
    /// Destination callsign (up to 9 chars, trimmed).
    pub addressee: String,
    /// Message text content.
    pub text: String,
    /// Optional message sequence number (for ack/rej tracking).
    pub message_id: Option<String>,
    /// Optional APRS 1.2 reply-ack: when the sender bundles an
    /// acknowledgement for a previously-received message into a new
    /// outgoing message. Format on wire is `{MM}AA` where `AA` is the
    /// acknowledged msgno. A bare `{MM}` trailer (the reply-ack
    /// capability advertisement) leaves this `None`.
    pub reply_ack: Option<String>,
}

impl AprsMessage {
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
    /// 4. **`GroupBulletin { group }`**: addressee is `BLN` + 1-5
    ///    alnum chars not matched by (2) or (3), per APRS 1.0.1 §14
    ///    p.74 (the spec's canonical form is `BLN<digit><group>` but
    ///    in-the-wild traffic uses many variations, so we accept any
    ///    multi-char tail).
    /// 5. **`AckRej`**: for a regular station addressee, text begins with
    ///    `ack`/`rej` + 1-5 alnum.
    /// 6. **`Direct`**: anything else.
    #[must_use]
    pub fn kind(&self) -> MessageKind {
        let addr = self.addressee.trim();
        // Bulletin addressees select bulletin text semantics regardless of
        // body spelling. In particular, a legitimate bulletin body such as
        // `ack123` is not an acknowledgement control frame.
        if let Some(kind) = bulletin_kind_from_addressee(addr) {
            return kind;
        }

        // Control frames use a regular station addressee; the text shape
        // disambiguates them from other directed messages.
        if classify_ack_rej(&self.text).is_some() {
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
                number: byte - b'0',
            });
        }
        if byte.is_ascii_uppercase() {
            return Some(MessageKind::Announcement {
                letter: byte as char,
            });
        }
    }
    if (2..=5).contains(&rest.len()) && rest.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Some(MessageKind::GroupBulletin {
            group: rest.to_owned(),
        });
    }
    None
}

/// Classify a message text as an APRS ack or rej control frame.
///
/// Per APRS 1.0.1 §14, ack and rej frames have text of the exact form
/// `ack<id>` or `rej<id>` where `<id>` is 1-5 alphanumeric characters and
/// nothing follows. This helper avoids the false-positive that naive
/// `starts_with("ack")` matching would produce for legitimate messages
/// that happen to begin with those letters (e.g. "acknowledge receipt").
///
/// Returns `Some((is_ack, message_id))` for control frames, `None`
/// otherwise. `is_ack` is `true` for `ack`, `false` for `rej`.
#[must_use]
pub fn classify_ack_rej(text: &str) -> Option<(bool, &str)> {
    let trimmed = text.trim_end_matches(['\r', '\n', ' ']);
    let (is_ack, rest) = if let Some(rest) = trimmed.strip_prefix("ack") {
        (true, rest)
    } else if let Some(rest) = trimmed.strip_prefix("rej") {
        (false, rest)
    } else {
        return None;
    };
    if !(1..=5).contains(&rest.len()) {
        return None;
    }
    if !rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    Some((is_ack, rest))
}

/// Parse an APRS message (`:ADDRESSEE:text{id`).
///
/// # Errors
///
/// Returns [`AprsError::InvalidFormat`] for malformed input: missing
/// leading `:`, absence of the second `:`, or addressee shorter than 9
/// characters.
pub fn parse_aprs_message(info: &[u8]) -> Result<AprsMessage, AprsError> {
    // Minimum: : + 9 char addressee + : + at least 0 text = 11 bytes
    if info.first() != Some(&b':') {
        return Err(AprsError::InvalidFormat);
    }

    // Addressee is exactly 9 characters (space-padded)
    let addressee_raw = info.get(1..10).ok_or(AprsError::InvalidFormat)?;
    let addressee = decode_wire_ascii("APRS message addressee", addressee_raw)?
        .trim()
        .to_owned();

    if info.get(10) != Some(&b':') {
        return Err(AprsError::InvalidFormat);
    }

    let body = info.get(11..).unwrap_or(&[]);
    let body_str = decode_wire_ascii("APRS message body", body)?;
    let trimmed_body = body_str.trim_end_matches(['\r', '\n']);

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
    let (text, message_id, reply_ack) = if is_bulletin_addressee(&addressee) {
        (trimmed_body.to_owned(), None, None)
    } else {
        parse_message_trailer(trimmed_body)
    };

    Ok(AprsMessage {
        addressee,
        text,
        message_id,
        reply_ack,
    })
}

/// Whether an addressee selects APRS bulletin/announcement text semantics.
fn is_bulletin_addressee(addressee: &str) -> bool {
    bulletin_kind_from_addressee(addressee).is_some()
}

/// Parse the optional `{MM}AA` / `{MM}` / `{MM` trailer of an APRS
/// message body.
///
/// Returns `(text, message_id, reply_ack)` where the latter two are
/// `Some` only when the trailer has the exact well-formed shape. Per
/// the APRS 1.2 reply-ack addendum, `{MM}` with nothing after the
/// brace is the reply-ack capability advertisement: the message id is
/// `MM` and no ack is piggybacked.
fn parse_message_trailer(body: &str) -> (String, Option<String>, Option<String>) {
    let Some(brace_idx) = body.rfind('{') else {
        return (body.to_owned(), None, None);
    };
    let Some(after_brace) = body.get(brace_idx + 1..) else {
        return (body.to_owned(), None, None);
    };
    let Some(prefix) = body.get(..brace_idx) else {
        return (body.to_owned(), None, None);
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
                return (prefix.to_owned(), Some(mm.to_owned()), None);
            }
            if (1..=5).contains(&aa.len()) && aa.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return (prefix.to_owned(), Some(mm.to_owned()), Some(aa.to_owned()));
            }
        }
    }

    // Plain message id: `MM` (1-5 alnum, end of string).
    if (1..=5).contains(&after_brace.len())
        && after_brace.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return (prefix.to_owned(), Some(after_brace.to_owned()), None);
    }

    // Neither pattern matched; treat whole body as plain text.
    (body.to_owned(), None, None)
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
        assert_eq!(msg.addressee, "N0CALL");
        assert_eq!(msg.text, "Hello World");
        assert_eq!(msg.message_id, Some("123".to_string()));
        Ok(())
    }

    #[test]
    fn parse_message_no_id() -> TestResult {
        let info = b":KQ4NIT   :Test message";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.addressee, "KQ4NIT");
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
            MessageKind::Announcement { letter: 'A' }
        );
        assert_eq!(announcement.text, "VERSION {A1");
        assert_eq!(announcement.message_id, None);

        let control_like = parse_aprs_message(b":BLN3     :ack123")?;
        assert_eq!(
            control_like.kind(),
            MessageKind::Bulletin { number: 3 },
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
                field: "APRS message addressee",
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
                field: "APRS message body",
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
        // Earlier code generations treated it as plain text because
        // the empty-ack advertisement form was not recognised.
        let info = b":N0CALL   :reply {soon}";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "reply ");
        assert_eq!(msg.message_id, Some("soon".to_owned()));
        assert_eq!(msg.reply_ack, None);
        Ok(())
    }

    #[test]
    fn parse_message_accepts_valid_id_with_text_containing_brace() -> TestResult {
        let info = b":N0CALL   :json {foo}{42";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "json {foo}");
        assert_eq!(msg.message_id, Some("42".to_owned()));
        Ok(())
    }

    #[test]
    fn parse_message_reply_ack() -> TestResult {
        let info = b":N0CALL   :Hello back{3}7";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "Hello back");
        assert_eq!(msg.message_id, Some("3".to_owned()));
        assert_eq!(msg.reply_ack, Some("7".to_owned()));
        Ok(())
    }

    #[test]
    fn parse_message_plain_id_no_reply_ack() -> TestResult {
        let info = b":N0CALL   :Hello{3";
        let msg = parse_aprs_message(info)?;
        assert_eq!(msg.text, "Hello");
        assert_eq!(msg.message_id, Some("3".to_owned()));
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
        assert_eq!(msg.message_id, Some("05".to_owned()));
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
    fn message_kind_direct() {
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::Direct);
    }

    #[test]
    fn message_kind_numeric_bulletin() {
        let msg = AprsMessage {
            addressee: "BLN3".to_owned(),
            text: "event update".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::Bulletin { number: 3 });
    }

    #[test]
    fn message_kind_group_bulletin() {
        let msg = AprsMessage {
            addressee: "BLNWX".to_owned(),
            text: "weather watch".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(
            msg.kind(),
            MessageKind::GroupBulletin {
                group: "WX".to_owned()
            }
        );
    }

    #[test]
    fn message_kind_announcement() {
        // Regression guard (APRS 1.0.1 §14 p.73): a
        // `BLN<single letter>` addressee is an Announcement, structurally
        // distinct from a `BLN<single digit>` general bulletin and from
        // a `BLN<group>` (multi-char) group bulletin. Earlier code
        // generations collapsed announcements into `GroupBulletin`.
        for letter in 'A'..='Z' {
            let addressee = format!("BLN{letter}");
            let msg = AprsMessage {
                addressee,
                text: "evening net at 8pm".to_owned(),
                message_id: None,
                reply_ack: None,
            };
            assert_eq!(
                msg.kind(),
                MessageKind::Announcement { letter },
                "BLN{letter} should classify as Announcement",
            );
        }
    }

    #[test]
    fn message_kind_group_bulletin_requires_multichar_tail() {
        // A single-character tail is *not* a
        // group bulletin; it's either a Bulletin (digit) or
        // Announcement (letter). Group bulletins use 2-5 char tails.
        let msg = AprsMessage {
            addressee: "BLNWX".to_owned(),
            text: "wx watch".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(
            msg.kind(),
            MessageKind::GroupBulletin {
                group: "WX".to_owned(),
            },
        );
    }

    #[test]
    fn message_kind_nws_bulletin() {
        let msg = AprsMessage {
            addressee: "NWS-TOR".to_owned(),
            text: "tornado warning".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::NwsBulletin);
    }

    #[test]
    fn message_kind_ack_rej_frame() {
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "ack42".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::AckRej);
    }

    // ---- classify_ack_rej ----

    #[test]
    fn classify_ack_rej_ack_numeric() {
        assert_eq!(classify_ack_rej("ack42"), Some((true, "42")));
        assert_eq!(classify_ack_rej("ack1"), Some((true, "1")));
        assert_eq!(classify_ack_rej("ack12345"), Some((true, "12345")));
        assert_eq!(classify_ack_rej("ackABC"), Some((true, "ABC")));
    }

    #[test]
    fn classify_ack_rej_rej() {
        assert_eq!(classify_ack_rej("rej42"), Some((false, "42")));
        assert_eq!(classify_ack_rej("rej1"), Some((false, "1")));
    }

    #[test]
    fn classify_ack_rej_rejects_non_control() {
        assert_eq!(classify_ack_rej("acknowledge receipt"), None);
        assert_eq!(classify_ack_rej("rejection letter"), None);
        assert_eq!(classify_ack_rej("ack with space"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_overlong_id() {
        assert_eq!(classify_ack_rej("ack123456"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_empty_id() {
        assert_eq!(classify_ack_rej("ack"), None);
        assert_eq!(classify_ack_rej("rej"), None);
    }

    #[test]
    fn classify_ack_rej_rejects_non_alnum() {
        assert_eq!(classify_ack_rej("ack-12"), None);
        assert_eq!(classify_ack_rej("ack 42"), None);
    }

    #[test]
    fn classify_ack_rej_strips_trailing_whitespace() {
        assert_eq!(classify_ack_rej("ack42\r\n"), Some((true, "42")));
    }
}
