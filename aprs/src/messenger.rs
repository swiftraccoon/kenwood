//! APRS message ack/retry manager.
//!
//! Provides reliable APRS messaging with automatic acknowledgement tracking
//! and retry logic. Messages are retried up to [`MAX_RETRIES`] times at
//! [`RETRY_INTERVAL`] intervals until acknowledged or expired. Incoming
//! duplicates are suppressed via a rolling dedup cache keyed on
//! `(source, msgno)` with a [`INCOMING_DEDUP_WINDOW`] TTL.
//!
//! # Time handling
//!
//! Per the crate-level convention, this module is sans-io and never calls
//! `std::time::Instant::now()` internally. Every stateful method that
//! reads the clock accepts a `now: Instant` parameter; callers (typically
//! the tokio shell) read the wall clock once per iteration and thread
//! it down.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ax25_codec::Ax25Address;

use crate::build::build_aprs_message;
use crate::error::AprsError;
use crate::message::{AprsMessage, MAX_APRS_MESSAGE_TEXT_LEN, classify_ack_rej};

/// How long an incoming message's `(source, msgno)` stays in the dedup
/// cache before being purged.
pub const INCOMING_DEDUP_WINDOW: Duration = Duration::from_secs(5 * 60);

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
    /// The message is immediately available from
    /// [`next_frame_to_send`](Self::next_frame_to_send). Text longer than
    /// [`MAX_APRS_MESSAGE_TEXT_LEN`] (67 bytes, APRS 1.0.1 §14) is silently
    /// truncated; use [`Self::send_message_checked`] if you want a hard
    /// error instead.
    ///
    /// `now` is used to initialise the message's `last_sent` timestamp to
    /// a time in the past so the message is immediately eligible for
    /// transmission on the next call to
    /// [`next_frame_to_send`](Self::next_frame_to_send).
    pub fn send_message(&mut self, addressee: &str, text: &str, now: Instant) -> String {
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
    ///
    /// `now` is compared against each pending message's `last_sent` to
    /// decide whether the retry interval has elapsed.
    #[must_use]
    pub fn next_frame_to_send(&mut self, now: Instant) -> Option<Vec<u8>> {
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
    /// Returns [`AprsError::MessageTooLong`] when the text is too long.
    pub fn send_message_checked(
        &mut self,
        addressee: &str,
        text: &str,
        now: Instant,
    ) -> Result<String, AprsError> {
        if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
            return Err(AprsError::MessageTooLong(text.len()));
        }
        Ok(self.send_message(addressee, text, now))
    }

    /// Check whether an incoming message is a duplicate of one recently
    /// seen from the same source station with the same msgno.
    ///
    /// Returns `true` if this is a new message (first time seen within
    /// [`INCOMING_DEDUP_WINDOW`]), `false` if it's a duplicate that
    /// should be ignored by the caller. Stateful: records the message
    /// in the dedup cache on `true`. Messages without a `message_id`
    /// are always considered new.
    ///
    /// `now` is used to expire stale dedup entries and to record the
    /// arrival time of the current message.
    pub fn is_new_incoming(&mut self, source: &str, msg: &AprsMessage, now: Instant) -> bool {
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
    ///
    /// Takes `now: Instant` for API consistency with the other time-aware
    /// methods even though no clock-dependent logic is currently used here
    /// — the decision is based on attempt count, not elapsed time.
    pub fn cleanup_expired(&mut self, _now: Instant) -> Vec<String> {
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
    use ax25_codec::parse_ax25;
    use kiss_tnc::decode_kiss_frame;

    use crate::message::parse_aprs_message as parse_msg;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn test_callsign() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
    }

    fn default_digipeater_path() -> Vec<Ax25Address> {
        vec![Ax25Address::new("WIDE1", 1), Ax25Address::new("WIDE2", 1)]
    }

    fn test_messenger() -> AprsMessenger {
        AprsMessenger::new(test_callsign(), default_digipeater_path())
    }

    #[test]
    fn send_message_assigns_incrementing_ids() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id1 = m.send_message("W1AW", "Hello", t0);
        let id2 = m.send_message("W1AW", "World", t0);
        assert_eq!(id1, "1");
        assert_eq!(id2, "2");
        assert_eq!(m.pending_count(), 2);
    }

    #[test]
    fn next_frame_returns_pending_message() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Test", t0);

        // Message was created with last_sent in the past, so it should be ready.
        let frame = m.next_frame_to_send(t0);
        let wire = frame.ok_or("expected a frame to send")?;

        // Verify the frame decodes to a valid APRS message.
        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let msg = parse_msg(&packet.info)?;
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "Test");
        assert_eq!(msg.message_id, Some("1".to_owned()));
        Ok(())
    }

    #[test]
    fn next_frame_returns_none_when_recently_sent() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Test", t0);

        // First call sends the message.
        let _frame = m.next_frame_to_send(t0);
        // Second call should return None (retry interval not elapsed).
        assert!(m.next_frame_to_send(t0).is_none());
    }

    #[test]
    fn process_incoming_ack_removes_pending() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Hello", t0);
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
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Hello", t0);

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
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Hello", t0);

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
    fn build_ack_produces_valid_frame() -> TestResult {
        let m = test_messenger();
        let wire = m.build_ack("W1AW", "42");

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let msg = parse_msg(&packet.info)?;
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "ack42");
        Ok(())
    }

    #[test]
    fn build_rej_produces_valid_frame() -> TestResult {
        let m = test_messenger();
        let wire = m.build_rej("W1AW", "42");

        let kiss = decode_kiss_frame(&wire)?;
        let packet = parse_ax25(&kiss.data)?;
        let msg = parse_msg(&packet.info)?;
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "rej42");
        Ok(())
    }

    #[test]
    fn cleanup_expired_removes_maxed_messages() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Test", t0);

        // Exhaust all retries by advancing time past the retry interval
        // each round. Sans-io: we mint the timestamps; no real waiting.
        let mut clock = t0;
        for _ in 0..MAX_RETRIES {
            clock += RETRY_INTERVAL;
            drop(m.next_frame_to_send(clock));
        }

        assert_eq!(m.pending_count(), 1); // Still present, just exhausted.
        let expired = m.cleanup_expired(clock);
        assert_eq!(expired, vec![id]);
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn send_message_checked_rejects_too_long_text() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let long = "x".repeat(100);
        assert!(m.send_message_checked("W1AW", &long, t0).is_err());
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn send_message_checked_accepts_boundary_length() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let text = "x".repeat(67);
        assert!(m.send_message_checked("W1AW", &text, t0).is_ok());
    }

    #[test]
    fn is_new_incoming_dedup_matches_source_msgno() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: Some("42".to_owned()),
            reply_ack: None,
        };
        assert!(m.is_new_incoming("W1AW", &msg, t0));
        assert!(!m.is_new_incoming("W1AW", &msg, t0));
        // Different source → not a duplicate.
        assert!(m.is_new_incoming("W2AW", &msg, t0));
    }

    #[test]
    fn is_new_incoming_no_id_always_new() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert!(m.is_new_incoming("W1AW", &msg, t0));
        assert!(m.is_new_incoming("W1AW", &msg, t0));
    }

    #[test]
    fn is_new_incoming_expires_stale_entries() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: Some("42".to_owned()),
            reply_ack: None,
        };
        assert!(m.is_new_incoming("W1AW", &msg, t0));
        // Jump past the dedup window — the entry should be expired.
        let later = t0 + INCOMING_DEDUP_WINDOW + Duration::from_secs(1);
        assert!(m.is_new_incoming("W1AW", &msg, later));
    }

    #[test]
    fn process_incoming_ignores_false_positive_message() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "Hello", t0);

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
        let t0 = Instant::now();
        let mut m = test_messenger();
        m.next_message_id = u16::MAX;
        let id1 = m.send_message("W1AW", "A", t0);
        assert_eq!(id1, u16::MAX.to_string());
        // After wrapping, 0 is skipped, so next is 1.
        let id2 = m.send_message("W1AW", "B", t0);
        assert_eq!(id2, "1");
    }
}
