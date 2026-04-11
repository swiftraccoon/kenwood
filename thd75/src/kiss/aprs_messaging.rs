//! APRS message ack/retry manager.
//!
//! Provides reliable APRS messaging with automatic acknowledgement tracking
//! and retry logic. Messages are retried up to [`MAX_RETRIES`] times at
//! [`RETRY_INTERVAL`] intervals until acknowledged or expired. Incoming
//! duplicates are suppressed via a rolling dedup cache keyed on
//! `(source, msgno)` with a [`INCOMING_DEDUP_WINDOW`] TTL.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{AprsMessage, Ax25Address, MAX_APRS_MESSAGE_TEXT_LEN, build_aprs_message};
use crate::error::ValidationError;

/// How long an incoming message's `(source, msgno)` stays in the dedup
/// cache before being purged.
pub const INCOMING_DEDUP_WINDOW: Duration = Duration::from_secs(5 * 60);

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

/// Maximum number of transmission attempts per message before giving up
/// (the default used when [`MessengerConfig::default`] is in play).
pub const MAX_RETRIES: u8 = 5;

/// Default interval between retry attempts.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Configuration knobs for the APRS messenger.
///
/// All fields are tunable; the defaults match APRS community conventions
/// (5 retries at 30-second intervals, 5-minute incoming dedup window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessengerConfig {
    /// Maximum number of transmission attempts per message.
    pub max_retries: u8,
    /// Interval between retry attempts.
    pub retry_interval: Duration,
    /// TTL for the incoming-message dedup cache.
    pub incoming_dedup_window: Duration,
}

impl Default for MessengerConfig {
    fn default() -> Self {
        Self {
            max_retries: MAX_RETRIES,
            retry_interval: RETRY_INTERVAL,
            incoming_dedup_window: INCOMING_DEDUP_WINDOW,
        }
    }
}

/// A message awaiting acknowledgement.
#[derive(Debug)]
struct PendingMessage {
    /// Sequence ID for ack matching.
    message_id: String,
    /// Pre-built KISS wire frame for retransmission.
    wire_frame: Vec<u8>,
    /// Number of transmission attempts so far.
    attempts: u8,
    /// Timestamp of the most recent transmission.
    last_sent: Instant,
}

/// Manages APRS message send/receive with automatic ack/retry.
///
/// Queues outbound messages, assigns sequence IDs, tracks pending
/// acknowledgements, generates retry frames on schedule, and suppresses
/// duplicate deliveries of the same incoming message via a rolling
/// `(source, msgno)` cache.
#[derive(Debug)]
pub struct AprsMessenger {
    /// This station's callsign/SSID.
    my_callsign: Ax25Address,
    /// Digipeater path used for outgoing message frames.
    digipeater_path: Vec<Ax25Address>,
    /// Messages awaiting acknowledgement.
    pending_messages: Vec<PendingMessage>,
    /// Counter for generating unique message IDs.
    next_message_id: u16,
    /// Dedup cache for incoming messages keyed on `(source_call, msgno)`.
    incoming_seen: HashMap<(String, String), Instant>,
    /// Tunable retry / dedup behaviour.
    config: MessengerConfig,
}

impl AprsMessenger {
    /// Create a new messenger with the default config.
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // HashMap::new is not const
    pub fn new(callsign: Ax25Address, digipeater_path: Vec<Ax25Address>) -> Self {
        Self::with_config(callsign, digipeater_path, MessengerConfig::default())
    }

    /// Create a new messenger with a caller-supplied [`MessengerConfig`].
    #[must_use]
    #[allow(clippy::missing_const_for_fn)] // HashMap::new is not const
    pub fn with_config(
        callsign: Ax25Address,
        digipeater_path: Vec<Ax25Address>,
        config: MessengerConfig,
    ) -> Self {
        Self {
            my_callsign: callsign,
            digipeater_path,
            pending_messages: Vec::new(),
            next_message_id: 1,
            incoming_seen: HashMap::new(),
            config,
        }
    }

    /// Queue a message for transmission. Returns the assigned message ID.
    ///
    /// The message is immediately available from [`next_frame_to_send`](Self::next_frame_to_send).
    /// Text longer than [`MAX_APRS_MESSAGE_TEXT_LEN`] (67 bytes, APRS
    /// 1.0.1 §14) is silently truncated; use [`Self::send_message_checked`]
    /// if you want a hard error instead.
    pub fn send_message(&mut self, addressee: &str, text: &str) -> String {
        // Pick a fresh ID, skipping any that clash with still-pending
        // messages. The ID space is `1..=u16::MAX` (65 535 slots), far
        // more than MAX_RETRIES of in-flight messages, so this loop
        // always terminates.
        let message_id = loop {
            let candidate = self.next_message_id.to_string();
            self.next_message_id = self.next_message_id.wrapping_add(1);
            if self.next_message_id == 0 {
                self.next_message_id = 1;
            }
            if !self
                .pending_messages
                .iter()
                .any(|p| p.message_id == candidate)
            {
                break candidate;
            }
        };

        let wire_frame = build_aprs_message(
            &self.my_callsign,
            addressee,
            text,
            Some(&message_id),
            &self.digipeater_path,
        );

        // Use a time in the past so the message is immediately eligible.
        let now = Instant::now();
        let past = now.checked_sub(self.config.retry_interval).unwrap_or(now);

        self.pending_messages.push(PendingMessage {
            message_id: message_id.clone(),
            wire_frame,
            attempts: 0,
            last_sent: past,
        });

        message_id
    }

    /// Get the next frame that needs to be sent (initial or retry).
    ///
    /// Returns `None` if no messages need sending right now. Retries
    /// happen at [`MessengerConfig::retry_interval`], up to
    /// [`MessengerConfig::max_retries`] attempts.
    #[must_use]
    pub fn next_frame_to_send(&mut self) -> Option<Vec<u8>> {
        let now = Instant::now();
        let max_retries = self.config.max_retries;
        let retry_interval = self.config.retry_interval;
        for msg in &mut self.pending_messages {
            if msg.attempts < max_retries && now.duration_since(msg.last_sent) >= retry_interval {
                msg.attempts += 1;
                msg.last_sent = now;
                return Some(msg.wire_frame.clone());
            }
        }
        None
    }

    /// Like [`Self::send_message`] but returns `Err(MessageTooLong)` if
    /// the text exceeds the APRS 1.0.1 §14 limit of 67 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AprsWireOutOfRange`] when the text is
    /// too long.
    pub fn send_message_checked(
        &mut self,
        addressee: &str,
        text: &str,
    ) -> Result<String, ValidationError> {
        if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
            return Err(ValidationError::AprsWireOutOfRange {
                field: "APRS message text",
                detail: "exceeds 67-byte limit (APRS 1.0.1 §14)",
            });
        }
        Ok(self.send_message(addressee, text))
    }

    /// Check whether an incoming message is a duplicate of one recently
    /// seen from the same source station with the same msgno.
    ///
    /// Returns `true` if this is a new message (first time seen within
    /// [`INCOMING_DEDUP_WINDOW`]), `false` if it's a duplicate that
    /// should be ignored by the caller. Stateful: records the message
    /// in the dedup cache on `true`. Messages without a `message_id`
    /// are always considered new.
    pub fn is_new_incoming(&mut self, source: &str, msg: &AprsMessage) -> bool {
        let now = Instant::now();
        let window = self.config.incoming_dedup_window;
        self.incoming_seen
            .retain(|_, t| now.duration_since(*t) < window);
        let Some(ref id) = msg.message_id else {
            return true;
        };
        let key = (source.to_owned(), id.clone());
        if self.incoming_seen.contains_key(&key) {
            return false;
        }
        let _prior = self.incoming_seen.insert(key, now);
        true
    }

    /// Process an incoming APRS message.
    ///
    /// If the text is an ack or rej control frame (per [`classify_ack_rej`])
    /// for a pending message, removes the pending entry and returns `true`.
    /// Returns `false` for regular messages, including ones that happen to
    /// start with the letters `ack`/`rej` but aren't valid control frames.
    pub fn process_incoming(&mut self, msg: &AprsMessage) -> bool {
        let Some((_is_ack, id)) = classify_ack_rej(&msg.text) else {
            return false;
        };
        let before = self.pending_messages.len();
        self.pending_messages.retain(|p| p.message_id != id);
        self.pending_messages.len() < before
    }

    /// Build an ack frame for a received message.
    ///
    /// The ack is sent back to `from` with text `ack{message_id}`.
    #[must_use]
    pub fn build_ack(&self, from: &str, message_id: &str) -> Vec<u8> {
        let text = format!("ack{message_id}");
        build_aprs_message(&self.my_callsign, from, &text, None, &self.digipeater_path)
    }

    /// Build a rej (reject) frame for a received message.
    ///
    /// The rej is sent back to `from` with text `rej{message_id}`.
    #[must_use]
    pub fn build_rej(&self, from: &str, message_id: &str) -> Vec<u8> {
        let text = format!("rej{message_id}");
        build_aprs_message(&self.my_callsign, from, &text, None, &self.digipeater_path)
    }

    /// Remove expired messages (those that have reached [`MAX_RETRIES`]
    /// attempts) and return their message IDs so callers can notify upstream.
    pub fn cleanup_expired(&mut self) -> Vec<String> {
        let mut expired = Vec::new();
        let max_retries = self.config.max_retries;
        self.pending_messages.retain(|m| {
            if m.attempts >= max_retries {
                expired.push(m.message_id.clone());
                false
            } else {
                true
            }
        });
        expired
    }

    /// Number of pending (unacknowledged) messages.
    #[must_use]
    pub const fn pending_count(&self) -> usize {
        self.pending_messages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiss::{
        decode_kiss_frame, default_digipeater_path, parse_aprs_message as parse_msg, parse_ax25,
    };

    fn test_callsign() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
    }

    fn test_messenger() -> AprsMessenger {
        AprsMessenger::new(test_callsign(), default_digipeater_path())
    }

    #[test]
    fn send_message_assigns_incrementing_ids() {
        let mut m = test_messenger();
        let id1 = m.send_message("W1AW", "Hello");
        let id2 = m.send_message("W1AW", "World");
        assert_eq!(id1, "1");
        assert_eq!(id2, "2");
        assert_eq!(m.pending_count(), 2);
    }

    #[test]
    fn next_frame_returns_pending_message() {
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Test");

        // Message was created with last_sent in the past, so it should be ready.
        let frame = m.next_frame_to_send();
        assert!(frame.is_some());

        // Verify the frame decodes to a valid APRS message.
        let wire = frame.unwrap();
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_msg(&packet.info).unwrap();
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "Test");
        assert_eq!(msg.message_id, Some("1".to_owned()));
    }

    #[test]
    fn next_frame_returns_none_when_recently_sent() {
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Test");

        // First call sends the message.
        let _frame = m.next_frame_to_send();
        // Second call should return None (retry interval not elapsed).
        assert!(m.next_frame_to_send().is_none());
    }

    #[test]
    fn process_incoming_ack_removes_pending() {
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Hello");
        assert_eq!(m.pending_count(), 1);

        let ack = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: format!("ack{id}"),
            message_id: None,
            reply_ack: None,
        };
        assert!(m.process_incoming(&ack));
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn process_incoming_rej_removes_pending() {
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Hello");

        let rej = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: format!("rej{id}"),
            message_id: None,
            reply_ack: None,
        };
        assert!(m.process_incoming(&rej));
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn process_incoming_unrelated_message_returns_false() {
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Hello");

        let unrelated = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "Just a regular message".to_owned(),
            message_id: Some("42".to_owned()),
            reply_ack: None,
        };
        assert!(!m.process_incoming(&unrelated));
        assert_eq!(m.pending_count(), 1);
    }

    #[test]
    fn build_ack_produces_valid_frame() {
        let m = test_messenger();
        let wire = m.build_ack("W1AW", "42");

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_msg(&packet.info).unwrap();
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "ack42");
    }

    #[test]
    fn build_rej_produces_valid_frame() {
        let m = test_messenger();
        let wire = m.build_rej("W1AW", "42");

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_msg(&packet.info).unwrap();
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "rej42");
    }

    #[test]
    fn cleanup_expired_removes_maxed_messages() {
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Test");

        // Exhaust all retries by sending MAX_RETRIES times.
        for _ in 0..MAX_RETRIES {
            // Force the message to be eligible by manipulating last_sent.
            // Since we can't travel in time, we set last_sent to the past.
            for msg in &mut m.pending_messages {
                msg.last_sent = Instant::now()
                    .checked_sub(RETRY_INTERVAL)
                    .unwrap_or_else(Instant::now);
            }
            let _ = m.next_frame_to_send();
        }

        assert_eq!(m.pending_count(), 1); // Still present, just exhausted.
        let expired = m.cleanup_expired();
        assert_eq!(expired, vec![id]);
        assert_eq!(m.pending_count(), 0);
    }

    // ---- classify_ack_rej ----

    #[test]
    fn classify_valid_ack() {
        assert_eq!(classify_ack_rej("ack42"), Some((true, "42")));
        assert_eq!(classify_ack_rej("ack1"), Some((true, "1")));
        assert_eq!(classify_ack_rej("ack12345"), Some((true, "12345")));
        assert_eq!(classify_ack_rej("ackABC"), Some((true, "ABC")));
    }

    #[test]
    fn classify_valid_rej() {
        assert_eq!(classify_ack_rej("rej42"), Some((false, "42")));
        assert_eq!(classify_ack_rej("rej1"), Some((false, "1")));
    }

    #[test]
    fn classify_rejects_message_starting_with_ack() {
        // The original bug: "acknowledge receipt" was treated as an ack.
        assert_eq!(classify_ack_rej("acknowledge receipt"), None);
        assert_eq!(classify_ack_rej("rejection letter"), None);
        assert_eq!(classify_ack_rej("ack with space"), None);
    }

    #[test]
    fn classify_rejects_too_long_id() {
        // Message IDs are 1-5 chars per APRS 1.0.1 §14.
        assert_eq!(classify_ack_rej("ack123456"), None);
    }

    #[test]
    fn classify_rejects_empty_id() {
        assert_eq!(classify_ack_rej("ack"), None);
        assert_eq!(classify_ack_rej("rej"), None);
    }

    #[test]
    fn classify_rejects_non_alnum_id() {
        assert_eq!(classify_ack_rej("ack-12"), None);
        assert_eq!(classify_ack_rej("ack 42"), None);
    }

    #[test]
    fn classify_strips_trailing_whitespace() {
        assert_eq!(classify_ack_rej("ack42\r\n"), Some((true, "42")));
    }

    #[test]
    fn send_message_checked_rejects_too_long_text() {
        let mut m = test_messenger();
        let long = "x".repeat(100);
        assert!(m.send_message_checked("W1AW", &long).is_err());
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn send_message_checked_accepts_boundary_length() {
        let mut m = test_messenger();
        let text = "x".repeat(67);
        assert!(m.send_message_checked("W1AW", &text).is_ok());
    }

    #[test]
    fn is_new_incoming_dedup_matches_source_msgno() {
        let mut m = test_messenger();
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: Some("42".to_owned()),
            reply_ack: None,
        };
        assert!(m.is_new_incoming("W1AW", &msg));
        assert!(!m.is_new_incoming("W1AW", &msg));
        // Different source → not a duplicate.
        assert!(m.is_new_incoming("W2AW", &msg));
    }

    #[test]
    fn is_new_incoming_no_id_always_new() {
        let mut m = test_messenger();
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert!(m.is_new_incoming("W1AW", &msg));
        assert!(m.is_new_incoming("W1AW", &msg));
    }

    #[test]
    fn process_incoming_ignores_false_positive_message() {
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Hello");

        // Regression: this used to be treated as an ack for msg "nowle".
        let false_ack = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "acknowledge receipt".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert!(!m.process_incoming(&false_ack));
        assert_eq!(m.pending_count(), 1);
    }

    #[test]
    fn message_id_wraps_around_skipping_zero() {
        let mut m = test_messenger();
        m.next_message_id = u16::MAX;
        let id1 = m.send_message("W1AW", "A");
        assert_eq!(id1, u16::MAX.to_string());
        // After wrapping, 0 is skipped, so next is 1.
        let id2 = m.send_message("W1AW", "B");
        assert_eq!(id2, "1");
    }
}
