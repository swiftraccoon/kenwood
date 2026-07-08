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

use ax25_codec::{Ax25Address, RouteEntry};

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
    /// Timestamp of the most recent transmission, or `None` if the
    /// message has never been sent yet (in which case it is immediately
    /// eligible). Representing "never sent" explicitly — rather than by
    /// backdating `last_sent` into the past — keeps first-send eligibility
    /// independent of the monotonic clock's origin (on Linux
    /// `CLOCK_MONOTONIC` is boot-relative, so subtracting the retry
    /// interval from an early `Instant` would saturate and spuriously
    /// withhold the first transmission).
    last_sent: Option<Instant>,
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
    digipeater_path: Vec<RouteEntry>,
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
    pub fn new(callsign: Ax25Address, digipeater_path: Vec<RouteEntry>) -> Self {
        Self::with_config(callsign, digipeater_path, MessengerConfig::default())
    }

    /// Create a new messenger with a caller-supplied [`MessengerConfig`].
    #[must_use]
    pub fn with_config(
        callsign: Ax25Address,
        digipeater_path: Vec<RouteEntry>,
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
    /// The freshly-queued message records no `last_sent` time (`None`),
    /// which marks it immediately eligible for transmission on the next
    /// call to [`next_frame_to_send`](Self::next_frame_to_send),
    /// regardless of the monotonic clock's origin. `now` is accepted for
    /// API consistency with the other time-aware methods.
    pub fn send_message(&mut self, addressee: &str, text: &str, _now: Instant) -> String {
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

        // `last_sent: None` marks the message as never-sent, hence
        // immediately eligible on the next `next_frame_to_send`. This
        // avoids backdating into the past, which would saturate (and so
        // spuriously withhold the first send) near the monotonic clock's
        // origin.
        self.pending_messages.push(PendingMessage {
            message_id: message_id.clone(),
            wire_frame,
            attempts: 0,
            last_sent: None,
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
            if msg.attempts >= max_retries {
                continue;
            }
            // A never-sent message (`last_sent == None`) is eligible
            // immediately; a previously-sent one only once the retry
            // interval has elapsed.
            let eligible = msg
                .last_sent
                .is_none_or(|last| now.duration_since(last) >= retry_interval);
            if eligible {
                msg.attempts += 1;
                msg.last_sent = Some(now);
                return Some(msg.wire_frame.clone());
            }
        }
        None
    }

    /// Next retry-eligible frame WITHOUT recording a transmission
    /// attempt. Returns `(message_id, wire_frame)`.
    ///
    /// Pair with [`Self::commit_send`] once the frame has actually
    /// been written: an async caller can be cancelled between
    /// obtaining a frame and transmitting it, and a burned attempt
    /// for a frame that never went on air would silently exhaust the
    /// retry budget.
    #[must_use]
    pub fn peek_frame_to_send(&self, now: Instant) -> Option<(String, Vec<u8>)> {
        let max_retries = self.config.max_retries;
        let retry_interval = self.config.retry_interval;
        self.pending_messages
            .iter()
            .find(|msg| {
                msg.attempts < max_retries
                    && msg
                        .last_sent
                        .is_none_or(|last| now.duration_since(last) >= retry_interval)
            })
            .map(|msg| (msg.message_id.clone(), msg.wire_frame.clone()))
    }

    /// Record that the frame for `message_id` was transmitted at
    /// `now` — the counterpart of [`Self::peek_frame_to_send`].
    pub fn commit_send(&mut self, message_id: &str, now: Instant) {
        if let Some(msg) = self
            .pending_messages
            .iter_mut()
            .find(|m| m.message_id == message_id)
        {
            msg.attempts += 1;
            msg.last_sent = Some(now);
        }
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
        if self.is_duplicate_incoming(source, msg, now) {
            return false;
        }
        self.mark_incoming_seen(source, msg, now);
        true
    }

    /// Non-mutating duplicate check: is this message already in the
    /// dedup cache?
    ///
    /// Split from [`Self::mark_incoming_seen`] so an async caller can
    /// check, fully process the message (through awaits that may be
    /// cancelled), and mark it seen only once delivery is assured — a
    /// message marked before delivery would be permanently lost if
    /// the delivery future is cancelled, because every RF retry of it
    /// then dedups away.
    #[must_use]
    pub fn is_duplicate_incoming(&self, source: &str, msg: &AprsMessage, now: Instant) -> bool {
        let window = self.config.incoming_dedup_window;
        let Some(ref id) = msg.message_id else {
            return false;
        };
        let key = (source.to_owned(), id.clone());
        self.incoming_seen
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < window)
    }

    /// Record an incoming message in the dedup cache — the counterpart
    /// of [`Self::is_duplicate_incoming`]. Also expires stale entries.
    pub fn mark_incoming_seen(&mut self, source: &str, msg: &AprsMessage, now: Instant) {
        let window = self.config.incoming_dedup_window;
        self.incoming_seen
            .retain(|_, t| now.duration_since(*t) < window);
        if let Some(ref id) = msg.message_id {
            let _prior = self
                .incoming_seen
                .insert((source.to_owned(), id.clone()), now);
        }
    }

    /// Process an incoming APRS message for acknowledgements of our own
    /// outbound traffic.
    ///
    /// Two acknowledgement carriers are recognised, and both clear the
    /// matching pending message:
    ///
    /// 1. A standalone ack/rej control frame (per [`classify_ack_rej`]):
    ///    text of the exact form `ack<id>` / `rej<id>`.
    /// 2. An APRS 1.1/1.2 reply-ack (`msg.reply_ack`): an ordinary
    ///    message whose trailer was `{MM}AA`, where `AA` acknowledges our
    ///    previously-sent message number. Modern clients (`APRSdroid`,
    ///    `YAAC`, `aprs.fi`) bundle the ack this way instead of sending a
    ///    separate `ackNN` frame.
    ///
    /// Returns `true` if at least one pending message was cleared. Note a
    /// reply-ack-bearing message is *also* a new inbound message in its own
    /// right (it carries `msg.message_id` "MM" and display text); callers
    /// must still route it through [`is_new_incoming`](Self::is_new_incoming)
    /// and surface it to the operator. This method only handles the
    /// outbound-ack side and never suppresses that.
    ///
    /// Returns `false` for regular messages with no acknowledgement,
    /// including ones that merely start with the letters `ack`/`rej` but
    /// aren't valid control frames.
    pub fn process_incoming(&mut self, msg: &AprsMessage) -> bool {
        let before = self.pending_messages.len();

        // (1) Standalone ack/rej control frame: `ack<id>` / `rej<id>`.
        if let Some((_is_ack, id)) = classify_ack_rej(&msg.text) {
            self.pending_messages.retain(|p| p.message_id != id);
        }

        // (2) APRS 1.1/1.2 reply-ack: the `{MM}AA` trailer's `AA` field
        // acknowledges our outbound message number. Compared verbatim,
        // matching the format the standalone-ack path uses.
        if let Some(ref acked) = msg.reply_ack {
            self.pending_messages.retain(|p| &p.message_id != acked);
        }

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
            .unwrap_or_else(|_| unreachable!("N0CALL-7 is statically valid"))
    }

    fn default_digipeater_path() -> Vec<RouteEntry> {
        vec![
            RouteEntry::new("WIDE1", 1)
                .unwrap_or_else(|_| unreachable!("WIDE1-1 is statically valid")),
            RouteEntry::new("WIDE2", 1)
                .unwrap_or_else(|_| unreachable!("WIDE2-1 is statically valid")),
        ]
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
    fn peek_does_not_burn_a_retry_attempt() -> TestResult {
        // An async caller may be cancelled between obtaining a frame
        // and actually transmitting it — peeking must not record an
        // attempt, only an explicit commit does.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "Test", t0);

        let (peek_id, _wire) = m.peek_frame_to_send(t0).ok_or("expected a frame")?;
        assert_eq!(peek_id, id);
        // Peeked but never committed: the frame must still be
        // available (the attempt was not burned).
        let again = m.peek_frame_to_send(t0);
        assert!(
            again.is_some(),
            "uncommitted peek must not consume the send"
        );

        // After a commit, the retry interval gates it.
        m.commit_send(&id, t0);
        assert!(m.peek_frame_to_send(t0).is_none());
        Ok(())
    }

    #[test]
    fn duplicate_check_and_mark_are_separate() -> TestResult {
        // An async caller must be able to CHECK for a duplicate,
        // fully process the message (including awaits that may be
        // cancelled), and only then MARK it seen — otherwise a
        // cancelled delivery permanently eats the message and every
        // RF retry of it.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = parse_msg(b":N0CALL   :Hello{7")?;

        assert!(!m.is_duplicate_incoming("W1AW", &msg, t0));
        // Not yet marked: still not a duplicate (a cancelled delivery
        // leaves it deliverable).
        assert!(!m.is_duplicate_incoming("W1AW", &msg, t0));

        m.mark_incoming_seen("W1AW", &msg, t0);
        assert!(m.is_duplicate_incoming("W1AW", &msg, t0));
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
    fn process_incoming_reply_ack_clears_pending() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        // Pending outbound message "1".
        let id = m.send_message("W1AW", "ping", t0);
        assert_eq!(id, "1");
        assert_eq!(m.pending_count(), 1);

        // Incoming ":N0CALL   :hi{05}1" — a *new* inbound message id "05"
        // whose reply-ack "1" acknowledges our pending "1". Mirrors what
        // parse_aprs_message yields for that wire form.
        let reply_ack = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hi".to_owned(),
            message_id: Some("05".to_owned()),
            reply_ack: Some(id),
        };
        assert!(
            m.process_incoming(&reply_ack),
            "reply-ack should clear the matching pending message",
        );
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn process_incoming_reply_ack_message_is_still_new_incoming() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = m.send_message("W1AW", "ping", t0);

        // A reply-ack message acks our outbound AND is a fresh inbound
        // message in its own right — is_new_incoming must still surface it.
        let reply_ack = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hi".to_owned(),
            message_id: Some("05".to_owned()),
            reply_ack: Some(id),
        };
        assert!(
            m.is_new_incoming("W1AW", &reply_ack, t0),
            "reply-ack message id 05 must be surfaced as a new incoming",
        );
        assert!(m.process_incoming(&reply_ack));
        assert_eq!(m.pending_count(), 0);
        // Same message arriving again is a duplicate by (source, msgno).
        assert!(!m.is_new_incoming("W1AW", &reply_ack, t0));
    }

    #[test]
    fn process_incoming_reply_ack_no_match_does_not_panic() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "ping", t0); // pending "1"

        // Reply-ack "99" matches no pending message: must not panic, must
        // not clear anything, and the message is still a new incoming.
        let reply_ack = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "unrelated".to_owned(),
            message_id: Some("07".to_owned()),
            reply_ack: Some("99".to_owned()),
        };
        assert!(
            !m.process_incoming(&reply_ack),
            "reply-ack with no matching pending clears nothing",
        );
        assert_eq!(m.pending_count(), 1);
        assert!(
            m.is_new_incoming("W1AW", &reply_ack, t0),
            "non-matching reply-ack message is still surfaced as new incoming",
        );
    }

    #[test]
    fn first_frame_emitted_immediately_then_second_waits() {
        // BUG-2 regression: the first transmission must be eligible at the
        // very same `now` the message was queued, independent of the
        // monotonic clock's origin (no checked_sub backdating). The second
        // send must still wait a full retry_interval.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = m.send_message("W1AW", "ping", t0);

        // First frame: emitted immediately at t0.
        let first = m.next_frame_to_send(t0);
        assert!(
            first.is_some(),
            "first frame must be eligible at the queue time regardless of clock origin",
        );

        // Still within the retry interval → nothing more to send yet.
        assert!(
            m.next_frame_to_send(t0).is_none(),
            "second send must not fire before retry_interval elapses",
        );
        // A probe a hair into the window (well below RETRY_INTERVAL) is
        // still too early for the retry. Built by addition to avoid any
        // Duration subtraction.
        let within = t0 + Duration::from_millis(1);
        assert!(
            m.next_frame_to_send(within).is_none(),
            "second send must still be withheld before retry_interval elapses",
        );

        // Exactly at retry_interval → the retry fires.
        let due = t0 + RETRY_INTERVAL;
        assert!(
            m.next_frame_to_send(due).is_some(),
            "second send must fire once retry_interval has elapsed",
        );
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
