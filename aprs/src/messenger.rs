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

use std::collections::{HashMap, HashSet};
use std::num::{NonZeroU8, NonZeroU16, NonZeroUsize};
use std::time::{Duration, Instant};

use ax25_codec::{Ax25Address, DigipeaterPath};

use crate::error::AprsError;
use crate::message::{AprsMessage, DirectMessageAddressee, MessageControl};
use crate::packet::MessageKind;
use crate::text::MessageText;
use crate::units::MessageId;
use crate::{AprsFrameHeader, Tocall};

/// How long an incoming message's `(source, msgno)` stays in the dedup
/// cache before being purged.
pub const INCOMING_DEDUP_WINDOW: Duration = Duration::from_mins(5);

/// Maximum number of transmission attempts per message before giving up
/// (the default used when [`MessengerConfig::default`] is in play).
pub const MAX_RETRIES: u8 = 5;

/// Default interval between retry attempts.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// Configuration knobs for the APRS messenger.
///
/// The default is five transmission attempts, a 30-second retry interval,
/// a five-minute incoming deduplication window, 256 pending messages, and
/// 1024 retained incoming identities. Capacities are host resource policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessengerConfig {
    /// Maximum number of transmission attempts per message.
    max_retries: NonZeroU8,
    /// Interval between retry attempts.
    retry_interval: Duration,
    /// TTL for the incoming-message dedup cache.
    incoming_dedup_window: Duration,
    /// Maximum pending messages, bounded by the nonzero `u16` identifier space.
    max_pending: NonZeroU16,
    /// Maximum retained `(source, message ID)` pairs.
    max_incoming: NonZeroUsize,
}

impl MessengerConfig {
    /// Set pending-message capacity with default retry and incoming policies.
    #[must_use]
    pub const fn new(max_pending: NonZeroU16) -> Self {
        Self {
            max_retries: const { NonZeroU8::new(MAX_RETRIES).unwrap() },
            retry_interval: RETRY_INTERVAL,
            incoming_dedup_window: INCOMING_DEDUP_WINDOW,
            max_pending,
            max_incoming: const { NonZeroUsize::new(1024).unwrap() },
        }
    }

    /// Set the attempt limit and the interval between committed attempts.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidMessengerInterval`] for a zero interval.
    pub const fn with_retry_policy(
        mut self,
        max_retries: NonZeroU8,
        retry_interval: Duration,
    ) -> Result<Self, AprsError> {
        if retry_interval.is_zero() {
            return Err(AprsError::InvalidMessengerInterval);
        }
        self.max_retries = max_retries;
        self.retry_interval = retry_interval;
        Ok(self)
    }

    /// Set the incoming identity retention window and entry capacity.
    ///
    /// Full history rejects new identities until an entry expires; existing
    /// identities can be refreshed without consuming another entry.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::InvalidMessengerInterval`] for a zero window.
    pub const fn with_incoming_deduplication(
        mut self,
        window: Duration,
        capacity: NonZeroUsize,
    ) -> Result<Self, AprsError> {
        if window.is_zero() {
            return Err(AprsError::InvalidMessengerInterval);
        }
        self.incoming_dedup_window = window;
        self.max_incoming = capacity;
        Ok(self)
    }

    /// Maximum transmission attempts per pending message.
    #[must_use]
    pub const fn max_retries(&self) -> NonZeroU8 {
        self.max_retries
    }

    /// Minimum elapsed monotonic time between committed attempts.
    #[must_use]
    pub const fn retry_interval(&self) -> Duration {
        self.retry_interval
    }

    /// Duration for retaining incoming message identities.
    #[must_use]
    pub const fn incoming_dedup_window(&self) -> Duration {
        self.incoming_dedup_window
    }

    /// Maximum pending messages, including messages awaiting expiry cleanup.
    #[must_use]
    pub const fn max_pending(&self) -> NonZeroU16 {
        self.max_pending
    }

    /// Maximum retained incoming source/identifier pairs.
    #[must_use]
    pub const fn max_incoming(&self) -> NonZeroUsize {
        self.max_incoming
    }
}

impl Default for MessengerConfig {
    fn default() -> Self {
        Self::new(const { NonZeroU16::new(256).unwrap() })
    }
}

/// A message awaiting acknowledgement.
#[derive(Debug)]
struct PendingMessage {
    /// Sequence ID for ack matching.
    message_id: MessageId,
    /// Station this message was sent to. An acknowledgement is only
    /// honoured when it arrives FROM this station: over open RF, a
    /// message number alone is not proof of delivery, and matching on
    /// it lets any third party cancel the pending retries.
    addressee: DirectMessageAddressee,
    /// Pre-built KISS wire frame for retransmission.
    wire_frame: Vec<u8>,
    /// Number of transmission attempts so far.
    attempts: u8,
    /// Timestamp of the most recent transmission, or `None` if the
    /// message has never been sent yet and is immediately eligible.
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
    digipeater_path: DigipeaterPath,
    /// Caller-selected software identity for every outgoing message frame.
    tocall: Tocall,
    /// Messages awaiting acknowledgement.
    pending_messages: Vec<PendingMessage>,
    /// Counter for generating unique message IDs.
    next_message_id: NonZeroU16,
    /// Dedup cache for incoming messages keyed on `(source_call, msgno)`.
    incoming_seen: HashMap<(Ax25Address, MessageId), Instant>,
    /// Tunable retry / dedup behaviour.
    config: MessengerConfig,
}

impl AprsMessenger {
    /// Create a new messenger with the default config.
    #[must_use]
    pub fn new(header: AprsFrameHeader) -> Self {
        Self::with_config(header, MessengerConfig::default())
    }

    /// Create a new messenger with a caller-supplied [`MessengerConfig`].
    #[must_use]
    pub fn with_config(header: AprsFrameHeader, config: MessengerConfig) -> Self {
        Self {
            my_callsign: header.source,
            digipeater_path: header.path,
            tocall: header.tocall,
            pending_messages: Vec::new(),
            next_message_id: NonZeroU16::MIN,
            incoming_seen: HashMap::new(),
            config,
        }
    }

    /// Queue a validated message for transmission and return its assigned ID.
    ///
    /// The message is immediately available from
    /// [`next_frame_to_send`](Self::next_frame_to_send). The typed addressee
    /// and text ensure the frame is wire-representable without truncation.
    ///
    /// The freshly-queued message records no `last_sent` time (`None`),
    /// which marks it immediately eligible for transmission on the next
    /// call to [`next_frame_to_send`](Self::next_frame_to_send),
    /// regardless of the monotonic clock's origin.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::PendingMessageCapacity`] without changing the queue
    /// when the configured pending capacity is full.
    pub fn queue_message(
        &mut self,
        addressee: &DirectMessageAddressee,
        text: &MessageText,
    ) -> Result<MessageId, AprsError> {
        let capacity = self.config.max_pending;
        if self.pending_messages.len() >= usize::from(capacity.get()) {
            return Err(AprsError::PendingMessageCapacity { capacity });
        }
        let occupied: HashSet<_> = self
            .pending_messages
            .iter()
            .map(|message| &message.message_id)
            .collect();
        let mut available = None;
        for _ in 0..=occupied.len() {
            let candidate = MessageId::from_sequence_number(self.next_message_id);
            self.next_message_id = self
                .next_message_id
                .checked_add(1)
                .unwrap_or(NonZeroU16::MIN);
            if !occupied.contains(&candidate) {
                available = Some(candidate);
                break;
            }
        }
        let message_id = available.ok_or(AprsError::PendingMessageCapacity { capacity })?;

        let wire_frame = self
            .frame_header()
            .message(addressee, text, Some(&message_id));

        self.pending_messages.push(PendingMessage {
            message_id: message_id.clone(),
            addressee: addressee.clone(),
            wire_frame,
            attempts: 0,
            last_sent: None,
        });

        Ok(message_id)
    }

    /// Select a frame and immediately record one transmission attempt.
    ///
    /// Returns `None` if no messages need sending right now. Retries
    /// happen at [`MessengerConfig::retry_interval`], up to
    /// [`MessengerConfig::max_retries`] attempts.
    ///
    /// `now` is compared against each pending message's `last_sent` to
    /// decide whether the retry interval has elapsed.
    /// For cancellable or fallible I/O, use [`Self::peek_frame_to_send`] and
    /// record the attempt with [`Self::commit_send`] after writing the frame.
    #[must_use]
    pub fn next_frame_to_send(&mut self, now: Instant) -> Option<Vec<u8>> {
        let max_retries = self.config.max_retries.get();
        let retry_interval = self.config.retry_interval;
        for msg in &mut self.pending_messages {
            if msg.attempts >= max_retries {
                continue;
            }
            // A never-sent message (`last_sent == None`) is eligible
            // immediately; a previously-sent one only once the retry
            // interval has elapsed.
            let eligible = msg.last_sent.is_none_or(|last| {
                now.checked_duration_since(last)
                    .is_some_and(|elapsed| elapsed >= retry_interval)
            });
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
    pub fn peek_frame_to_send(&self, now: Instant) -> Option<(MessageId, Vec<u8>)> {
        let max_retries = self.config.max_retries.get();
        let retry_interval = self.config.retry_interval;
        self.pending_messages
            .iter()
            .find(|msg| {
                msg.attempts < max_retries
                    && msg.last_sent.is_none_or(|last| {
                        now.checked_duration_since(last)
                            .is_some_and(|elapsed| elapsed >= retry_interval)
                    })
            })
            .map(|msg| (msg.message_id.clone(), msg.wire_frame.clone()))
    }

    /// Record that the frame for `message_id` was transmitted at
    /// `now`. The counterpart of [`Self::peek_frame_to_send`].
    ///
    /// Returns the new attempt count. A rejected commit leaves the message unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::UnknownPendingMessage`] for a stale identifier,
    /// [`AprsError::MessageAttemptsExhausted`] at the attempt limit, or
    /// [`AprsError::MessageRetryNotDue`] before the interval or on regressing time.
    pub fn commit_send(
        &mut self,
        message_id: &MessageId,
        now: Instant,
    ) -> Result<NonZeroU8, AprsError> {
        let msg = self
            .pending_messages
            .iter_mut()
            .find(|m| &m.message_id == message_id)
            .ok_or(AprsError::UnknownPendingMessage)?;
        if msg.attempts >= self.config.max_retries.get() {
            return Err(AprsError::MessageAttemptsExhausted);
        }
        if msg.last_sent.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_none_or(|elapsed| elapsed < self.config.retry_interval)
        }) {
            return Err(AprsError::MessageRetryNotDue);
        }
        msg.attempts += 1;
        msg.last_sent = Some(now);
        NonZeroU8::new(msg.attempts).ok_or(AprsError::MessageAttemptsExhausted)
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
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::IncomingMessageCapacity`] for a new numbered message
    /// when every retained identity is still inside the configured window.
    pub fn is_new_incoming(
        &mut self,
        source: &Ax25Address,
        msg: &AprsMessage,
        now: Instant,
    ) -> Result<bool, AprsError> {
        if self.is_duplicate_incoming(source, msg, now) {
            return Ok(false);
        }
        self.mark_incoming_seen(source, msg, now)?;
        Ok(true)
    }

    /// Non-mutating duplicate check: is this message already in the
    /// dedup cache?
    ///
    /// Async callers check capacity with [`Self::check_incoming_capacity`],
    /// complete delivery, and then mark the identity with
    /// [`Self::mark_incoming_seen`]. A cancelled delivery leaves it unmarked.
    #[must_use]
    pub fn is_duplicate_incoming(
        &self,
        source: &Ax25Address,
        msg: &AprsMessage,
        now: Instant,
    ) -> bool {
        let window = self.config.incoming_dedup_window;
        let Some(id) = msg.message_id() else {
            return false;
        };
        let key = (source.clone(), id.clone());
        self.incoming_seen.get(&key).is_some_and(|seen_at| {
            now.checked_duration_since(*seen_at)
                .is_none_or(|elapsed| elapsed < window)
        })
    }

    /// Check whether this message can be recorded without evicting live history.
    ///
    /// Existing unexpired identities and unnumbered messages need no new slot.
    /// This check preserves state; [`Self::mark_incoming_seen`] checks again
    /// before changing the bounded history.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::IncomingMessageCapacity`] when no slot is available.
    pub fn check_incoming_capacity(
        &self,
        source: &Ax25Address,
        msg: &AprsMessage,
        now: Instant,
    ) -> Result<(), AprsError> {
        if msg.message_id().is_none() || self.is_duplicate_incoming(source, msg, now) {
            return Ok(());
        }
        let retained = self
            .incoming_seen
            .values()
            .filter(|&&seen_at| {
                now.checked_duration_since(seen_at)
                    .is_none_or(|elapsed| elapsed < self.config.incoming_dedup_window)
            })
            .count();
        if retained >= self.config.max_incoming.get() {
            return Err(AprsError::IncomingMessageCapacity {
                capacity: self.config.max_incoming,
            });
        }
        Ok(())
    }

    /// Record an incoming identity after delivery, expiring stale entries.
    ///
    /// A duplicate refresh never moves its timestamp backwards. Regressing time
    /// retains existing history and cannot free capacity.
    ///
    /// # Errors
    ///
    /// Returns [`AprsError::IncomingMessageCapacity`] before changing state if
    /// a new identity would exceed the configured capacity.
    pub fn mark_incoming_seen(
        &mut self,
        source: &Ax25Address,
        msg: &AprsMessage,
        now: Instant,
    ) -> Result<(), AprsError> {
        self.check_incoming_capacity(source, msg, now)?;
        let window = self.config.incoming_dedup_window;
        self.incoming_seen.retain(|_, seen_at| {
            now.checked_duration_since(*seen_at)
                .is_none_or(|elapsed| elapsed < window)
        });
        if let Some(id) = msg.message_id() {
            let key = (source.clone(), id.clone());
            let _seen_at = self
                .incoming_seen
                .entry(key)
                .and_modify(|seen_at| {
                    if now > *seen_at {
                        *seen_at = now;
                    }
                })
                .or_insert(now);
        }
        Ok(())
    }

    /// Process an incoming APRS message for acknowledgements of this
    /// station's outbound traffic.
    ///
    /// Two acknowledgement carriers are recognised, and both clear the
    /// matching pending message:
    ///
    /// 1. A standalone ack/rej control frame (per [`MessageControl`]):
    ///    text of the exact form `ack<id>` / `rej<id>`.
    /// 2. An APRS 1.1/1.2 reply-ack (`msg.reply_ack`): an ordinary
    ///    message whose trailer was `{MM}AA`, where `AA` acknowledges one
    ///    of this station's sent message numbers. Modern clients (`APRSdroid`,
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
    ///
    /// `source` is the complete AX.25 address of the station that transmitted the
    /// frame. An acknowledgement is honoured ONLY when it arrives from
    /// the station the pending message was addressed to: over open RF a
    /// message number is not a secret, so matching on it alone would let
    /// any third party silently cancel the delivery retries. Comparison
    /// includes the SSID. The recipient must equal this messenger's complete
    /// address. Bulletin destinations never carry acknowledgement semantics.
    pub fn process_incoming(&mut self, source: &Ax25Address, msg: &AprsMessage) -> bool {
        let kind = msg.kind();
        if !msg
            .addressee()
            .as_str()
            .eq_ignore_ascii_case(&self.my_callsign.to_string())
            || !matches!(kind, MessageKind::Direct | MessageKind::AckRej)
        {
            return false;
        }
        let sender = source.to_string();
        let before = self.pending_messages.len();

        // (1) Standalone ack/rej control frame: `ack<id>` / `rej<id>`.
        if kind == MessageKind::AckRej
            && let Some(control) = MessageControl::from_text(msg.text())
        {
            self.pending_messages.retain(|p| {
                !(p.message_id == *control.message_id()
                    && p.addressee.as_str().eq_ignore_ascii_case(&sender))
            });
        }

        // (2) APRS 1.1/1.2 reply-ack: the `{MM}AA` trailer's `AA` field
        // acknowledges our outbound message number. Compared verbatim,
        // matching the format the standalone-ack path uses.
        if let Some(acked) = msg.reply_ack() {
            self.pending_messages.retain(|p| {
                !(p.message_id == *acked && p.addressee.as_str().eq_ignore_ascii_case(&sender))
            });
        }

        self.pending_messages.len() < before
    }

    /// Build an ack frame for a received message.
    ///
    /// The ack is sent back to `from` with text `ack{message_id}`.
    #[must_use]
    pub fn build_ack(&self, from: &DirectMessageAddressee, message_id: &MessageId) -> Vec<u8> {
        let text = MessageText::acknowledgement(message_id);
        self.frame_header().message(from, &text, None)
    }

    /// Build a rej (reject) frame for a received message.
    ///
    /// The rej is sent back to `from` with text `rej{message_id}`.
    #[must_use]
    pub fn build_rej(&self, from: &DirectMessageAddressee, message_id: &MessageId) -> Vec<u8> {
        let text = MessageText::rejection(message_id);
        self.frame_header().message(from, &text, None)
    }

    fn frame_header(&self) -> AprsFrameHeader {
        AprsFrameHeader::new(
            self.my_callsign.clone(),
            self.tocall.clone(),
            self.digipeater_path.clone(),
        )
    }

    /// Removes exhausted messages after one retry interval following their last
    /// recorded transmission, returning their IDs for caller notification.
    /// The interval lets a final attempt receive an acknowledgement. Regressing
    /// supplied time retains the pending message.
    pub fn cleanup_expired(&mut self, now: Instant) -> Vec<MessageId> {
        let mut expired = Vec::new();
        let max_retries = self.config.max_retries.get();
        self.pending_messages.retain(|m| {
            if m.attempts >= max_retries
                && m.last_sent.is_some_and(|last| {
                    now.checked_duration_since(last)
                        .is_some_and(|elapsed| elapsed >= self.config.retry_interval)
                })
            {
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
    use ax25_codec::{Ax25Packet, RouteEntry};
    use kiss_tnc::KissFrame;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn parse_msg(bytes: &[u8]) -> Result<AprsMessage, AprsError> {
        AprsMessage::from_bytes(bytes)
    }

    fn test_callsign() -> Ax25Address {
        Ax25Address::new("N0CALL", 0)
            .unwrap_or_else(|_| unreachable!("N0CALL is a valid static test address"))
    }

    fn default_digipeater_path() -> DigipeaterPath {
        let wide1 = RouteEntry::new("WIDE1", 1)
            .unwrap_or_else(|_| unreachable!("WIDE1-1 is a valid static test route"));
        let wide2 = RouteEntry::new("WIDE2", 1)
            .unwrap_or_else(|_| unreachable!("WIDE2-1 is a valid static test route"));
        DigipeaterPath::new(vec![wide1, wide2])
            .unwrap_or_else(|_| unreachable!("two entries fit an AX.25 path"))
    }

    fn test_messenger() -> AprsMessenger {
        AprsMessenger::new(AprsFrameHeader::new(
            test_callsign(),
            Tocall::th_d75(),
            default_digipeater_path(),
        ))
    }

    fn test_source(value: &str) -> Ax25Address {
        value
            .to_ascii_uppercase()
            .parse()
            .unwrap_or_else(|_| unreachable!("test callers supply valid station addresses"))
    }

    fn test_addressee(value: &str) -> DirectMessageAddressee {
        DirectMessageAddressee::new(value)
            .unwrap_or_else(|_| unreachable!("test callers provide valid addressees"))
    }

    fn test_text(value: &str) -> MessageText {
        MessageText::new(value)
            .unwrap_or_else(|_| unreachable!("test callers provide valid message text"))
    }

    fn test_message_id(value: &str) -> MessageId {
        MessageId::new(value)
            .unwrap_or_else(|_| unreachable!("test callers provide valid message IDs"))
    }

    fn queue_test_message(messenger: &mut AprsMessenger, addressee: &str, text: &str) -> MessageId {
        messenger
            .queue_message(&test_addressee(addressee), &test_text(text))
            .unwrap_or_else(|_| unreachable!("test queues remain within their configured capacity"))
    }

    #[test]
    fn send_message_assigns_incrementing_ids() {
        let mut m = test_messenger();
        let id1 = queue_test_message(&mut m, "W1AW", "Hello");
        let id2 = queue_test_message(&mut m, "W1AW", "World");
        assert_eq!(id1.as_str(), "1");
        assert_eq!(id2.as_str(), "2");
        assert_eq!(m.pending_count(), 2);
    }

    #[test]
    fn pending_capacity_rejects_before_search_and_skips_occupied_identifiers() -> TestResult {
        let config = MessengerConfig::new(NonZeroU16::new(2).ok_or("pending capacity")?)
            .with_retry_policy(NonZeroU8::new(2).ok_or("attempt count")?, RETRY_INTERVAL)?;
        let mut messenger = AprsMessenger::with_config(
            AprsFrameHeader::new(test_callsign(), Tocall::th_d75(), default_digipeater_path()),
            config,
        );
        let first = messenger.queue_message(&test_addressee("W1AW"), &test_text("one"))?;
        let second = messenger.queue_message(&test_addressee("W1AW"), &test_text("two"))?;
        let rejected = messenger.queue_message(&test_addressee("W1AW"), &test_text("three"));
        assert!(
            matches!(rejected, Err(AprsError::PendingMessageCapacity { capacity }) if capacity.get() == 2)
        );
        assert_eq!(messenger.pending_count(), 2);
        assert_eq!(messenger.next_message_id.get(), 3);
        let removed = messenger.pending_messages.remove(0);
        assert_eq!(removed.message_id, first);
        messenger.next_message_id = NonZeroU16::new(2).ok_or("occupied counter")?;
        let next = messenger.queue_message(&test_addressee("W1AW"), &test_text("three"))?;
        assert_eq!(second.as_str(), "2");
        assert_eq!(next.as_str(), "3");
        Ok(())
    }

    #[test]
    fn commit_rejects_early_regressing_exhausted_and_stale_transitions() -> TestResult {
        let now = Instant::now();
        let later = now + RETRY_INTERVAL;
        let config = MessengerConfig::new(NonZeroU16::MIN)
            .with_retry_policy(NonZeroU8::new(2).ok_or("attempt count")?, RETRY_INTERVAL)?;
        let mut messenger = AprsMessenger::with_config(
            AprsFrameHeader::new(test_callsign(), Tocall::th_d75(), default_digipeater_path()),
            config,
        );
        let id = messenger.queue_message(&test_addressee("W1AW"), &test_text("hello"))?;
        assert_eq!(messenger.commit_send(&id, later)?.get(), 1);
        assert_eq!(
            messenger.commit_send(&id, now),
            Err(AprsError::MessageRetryNotDue)
        );
        assert_eq!(
            messenger.commit_send(&id, later),
            Err(AprsError::MessageRetryNotDue)
        );
        assert_eq!(messenger.commit_send(&id, later + RETRY_INTERVAL)?.get(), 2);
        assert_eq!(
            messenger.commit_send(&id, later + RETRY_INTERVAL * 2),
            Err(AprsError::MessageAttemptsExhausted)
        );
        assert_eq!(messenger.cleanup_expired(later), Vec::<MessageId>::new());
        assert_eq!(
            messenger.cleanup_expired(later + RETRY_INTERVAL),
            Vec::<MessageId>::new()
        );
        assert_eq!(
            messenger.cleanup_expired(later + RETRY_INTERVAL * 2),
            std::slice::from_ref(&id)
        );
        assert_eq!(
            messenger.commit_send(&id, later),
            Err(AprsError::UnknownPendingMessage)
        );
        Ok(())
    }

    #[test]
    fn messenger_configuration_rejects_zero_timing() {
        assert_eq!(
            MessengerConfig::new(NonZeroU16::MIN).with_retry_policy(NonZeroU8::MIN, Duration::ZERO),
            Err(AprsError::InvalidMessengerInterval)
        );
        assert_eq!(
            MessengerConfig::new(NonZeroU16::MIN)
                .with_incoming_deduplication(Duration::ZERO, NonZeroUsize::MIN),
            Err(AprsError::InvalidMessengerInterval)
        );
    }

    #[test]
    fn incoming_capacity_refuses_new_identities_without_evicting_live_history() -> TestResult {
        let capacity = NonZeroUsize::new(2).ok_or("two is nonzero")?;
        let config = MessengerConfig::default()
            .with_incoming_deduplication(INCOMING_DEDUP_WINDOW, capacity)?;
        let mut messenger = AprsMessenger::with_config(
            AprsFrameHeader::new(test_callsign(), Tocall::th_d75(), default_digipeater_path()),
            config,
        );
        let now = Instant::now();
        let first = parse_msg(b":N0CALL   :first{1")?;
        let second = parse_msg(b":N0CALL   :second{2")?;
        let third = parse_msg(b":N0CALL   :third{3")?;
        let source = test_source("W1AW-1");
        assert!(messenger.is_new_incoming(&source, &first, now)?);
        assert!(messenger.is_new_incoming(&source, &second, now)?);
        let retained = messenger.incoming_seen.clone();
        assert_eq!(
            messenger.check_incoming_capacity(&source, &third, now),
            Err(AprsError::IncomingMessageCapacity { capacity })
        );
        assert_eq!(
            messenger.mark_incoming_seen(&source, &third, now),
            Err(AprsError::IncomingMessageCapacity { capacity })
        );
        assert_eq!(
            messenger.is_new_incoming(&source, &third, now),
            Err(AprsError::IncomingMessageCapacity { capacity })
        );
        assert_eq!(messenger.incoming_seen, retained);
        assert!(!messenger.is_new_incoming(&source, &first, now)?);
        let unnumbered = parse_msg(b":N0CALL   :no identifier")?;
        assert!(messenger.is_new_incoming(&source, &unnumbered, now)?);
        assert_eq!(messenger.incoming_seen, retained);
        Ok(())
    }

    #[test]
    fn incoming_capacity_refresh_and_expiry_preserve_regressing_time_history() -> TestResult {
        let config = MessengerConfig::default()
            .with_incoming_deduplication(INCOMING_DEDUP_WINDOW, NonZeroUsize::MIN)?;
        let mut messenger = AprsMessenger::with_config(
            AprsFrameHeader::new(test_callsign(), Tocall::th_d75(), default_digipeater_path()),
            config,
        );
        let earlier = Instant::now();
        let now = earlier + Duration::from_secs(1);
        let source = test_source("W1AW-1");
        let message = parse_msg(b":N0CALL   :first{1")?;
        let replacement = parse_msg(b":N0CALL   :second{2")?;
        messenger.mark_incoming_seen(&source, &message, now)?;
        assert_eq!(
            messenger.check_incoming_capacity(&source, &replacement, earlier),
            Err(AprsError::IncomingMessageCapacity {
                capacity: NonZeroUsize::MIN
            })
        );
        messenger.mark_incoming_seen(&source, &message, earlier)?;
        let refreshed_at = now + Duration::from_secs(2);
        messenger.check_incoming_capacity(&source, &message, refreshed_at)?;
        messenger.mark_incoming_seen(&source, &message, refreshed_at)?;
        assert_eq!(
            messenger.check_incoming_capacity(&source, &replacement, now + INCOMING_DEDUP_WINDOW),
            Err(AprsError::IncomingMessageCapacity {
                capacity: NonZeroUsize::MIN
            })
        );
        let expires_at = refreshed_at + INCOMING_DEDUP_WINDOW;
        messenger.check_incoming_capacity(&source, &replacement, expires_at)?;
        assert!(messenger.is_new_incoming(&source, &replacement, expires_at)?);
        assert_eq!(messenger.incoming_seen.len(), 1);
        assert!(!messenger.is_duplicate_incoming(&source, &message, expires_at));
        assert!(messenger.is_duplicate_incoming(&source, &replacement, expires_at));
        Ok(())
    }

    #[test]
    fn messages_and_controls_retain_the_selected_frame_header() -> TestResult {
        let source = Ax25Address::new("N0CALL", 7)?;
        let path = default_digipeater_path();
        let header = AprsFrameHeader::new(source.clone(), Tocall::new("APTEST")?, path.clone());
        let mut messenger = AprsMessenger::new(header);
        let addressee = DirectMessageAddressee::new("W1AW-3")?;
        let id = messenger.queue_message(&addressee, &MessageText::new("hello")?)?;
        let (_, message) = messenger
            .peek_frame_to_send(Instant::now())
            .ok_or("queued frame must be eligible")?;
        for wire in [
            message,
            messenger.build_ack(&addressee, &id),
            messenger.build_rej(&addressee, &id),
        ] {
            let kiss = KissFrame::from_bytes(&wire)?;
            let packet = Ax25Packet::from_bytes(&kiss.data)?;
            assert_eq!(packet.header.source, source);
            assert_eq!(packet.header.destination.callsign.as_str(), "APTEST");
            assert_eq!(packet.header.digipeaters, path);
        }
        Ok(())
    }

    #[test]
    fn acknowledgements_require_full_sender_recipient_and_direct_message_kind() -> TestResult {
        let local = test_source("N0CALL-7");
        let sender = test_source("W1AW-3");
        let mut messenger = AprsMessenger::new(AprsFrameHeader::new(
            local,
            Tocall::th_d75(),
            default_digipeater_path(),
        ));
        let id = messenger.queue_message(&test_addressee("W1AW-3"), &test_text("hello"))?;
        let incoming = parse_msg(format!(":N0CALL   :ack{id}").as_bytes())?;
        assert!(
            !messenger.process_incoming(&sender, &incoming),
            "base callsign consumed an SSID-specific acknowledgement"
        );
        let incoming = parse_msg(format!(":N0CALL-7 :ack{id}").as_bytes())?;
        assert!(
            !messenger.process_incoming(&test_source("W1AW"), &incoming),
            "different sender SSID consumed the acknowledgement"
        );
        let bulletin = parse_msg(format!(":BLN4ABCDE:ack{id}{{05}}{id}").as_bytes())?;
        assert!(
            !messenger.process_incoming(&sender, &bulletin),
            "bulletin body consumed a pending message"
        );
        assert!(messenger.process_incoming(&sender, &incoming));
        assert_eq!(messenger.pending_count(), 0);
        Ok(())
    }

    #[test]
    fn incoming_duplicate_identity_preserves_the_complete_station_address() -> TestResult {
        let now = Instant::now();
        let mut messenger = test_messenger();
        let message = parse_msg(b":N0CALL   :hello{42")?;
        assert!(messenger.is_new_incoming(&test_source("W1AW-1"), &message, now)?);
        assert!(messenger.is_new_incoming(&test_source("W1AW-2"), &message, now)?);
        assert!(!messenger.is_new_incoming(&test_source("W1AW-1"), &message, now)?);
        Ok(())
    }

    #[test]
    fn next_frame_returns_pending_message() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "Test");

        // A queued message is immediately eligible for its first transmission.
        let frame = m.next_frame_to_send(t0);
        let wire = frame.ok_or("expected a frame to send")?;

        // Verify the frame decodes to a valid APRS message.
        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let msg = parse_msg(packet.information())?;
        assert_eq!(msg.addressee().as_str(), "W1AW");
        assert_eq!(msg.text(), "Test");
        assert_eq!(msg.message_id().map(MessageId::as_str), Some("1"));
        Ok(())
    }

    #[test]
    fn peek_does_not_burn_a_retry_attempt() -> TestResult {
        // An async caller may be cancelled between obtaining a frame
        // and actually transmitting it, so peeking must not record an
        // attempt, only an explicit commit does.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Test");

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
        let _attempt = m.commit_send(&id, t0)?;
        assert!(m.peek_frame_to_send(t0).is_none());
        Ok(())
    }

    #[test]
    fn duplicate_check_and_mark_are_separate() -> TestResult {
        // An async caller must be able to CHECK for a duplicate,
        // fully process the message (including awaits that may be
        // cancelled), and only then MARK it seen; otherwise a
        // cancelled delivery permanently eats the message and every
        // RF retry of it.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = parse_msg(b":N0CALL   :Hello{7")?;

        assert!(!m.is_duplicate_incoming(&test_source("W1AW"), &msg, t0));
        // Not yet marked: still not a duplicate (a cancelled delivery
        // leaves it deliverable).
        assert!(!m.is_duplicate_incoming(&test_source("W1AW"), &msg, t0));

        m.mark_incoming_seen(&test_source("W1AW"), &msg, t0)?;
        assert!(m.is_duplicate_incoming(&test_source("W1AW"), &msg, t0));
        Ok(())
    }

    #[test]
    fn next_frame_returns_none_when_recently_sent() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "Test");

        // First call sends the message.
        let _frame = m.next_frame_to_send(t0);
        // Second call should return None (retry interval not elapsed).
        assert!(m.next_frame_to_send(t0).is_none());
    }

    #[test]
    fn regressing_clock_does_not_retry_or_panic() {
        let earlier = Instant::now();
        let t0 = earlier + Duration::from_secs(1);
        let mut messenger = test_messenger();
        let _id = queue_test_message(&mut messenger, "W1AW", "Test");

        assert!(messenger.next_frame_to_send(t0).is_some());
        assert!(messenger.next_frame_to_send(earlier).is_none());
        assert!(messenger.peek_frame_to_send(earlier).is_none());
    }

    #[test]
    fn process_incoming_ack_removes_pending() -> TestResult {
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Hello");
        assert_eq!(m.pending_count(), 1);

        let ack = parse_msg(format!(":N0CALL   :ack{id}").as_bytes())?;
        assert!(m.process_incoming(&test_source("W1AW"), &ack));
        assert_eq!(m.pending_count(), 0);
        Ok(())
    }

    /// An ack only counts when it comes from the station the message
    /// was addressed to. Matching on the message number alone lets ANY
    /// station on the air cancel the delivery retries; over RF that is
    /// a trivially spoofable denial of delivery (the retries stop and
    /// the message later reports "expired" instead of being resent).
    #[test]
    fn process_incoming_ack_from_a_third_party_does_not_clear_pending() -> TestResult {
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Hello");
        assert_eq!(m.pending_count(), 1);

        // K9XYZ was never the addressee, so its ack must be ignored.
        let spoofed = parse_msg(format!(":N0CALL   :ack{id}").as_bytes())?;
        assert!(
            !m.process_incoming(&test_source("K9XYZ"), &spoofed),
            "an ack from a station we never messaged must not clear the pending message"
        );
        assert_eq!(
            m.pending_count(),
            1,
            "the message must still be pending after a spoofed ack"
        );

        // The real addressee still clears it.
        let genuine = parse_msg(format!(":N0CALL   :ack{id}").as_bytes())?;
        assert!(
            m.process_incoming(&test_source("w1aw"), &genuine),
            "callsign match is case-insensitive"
        );
        assert_eq!(m.pending_count(), 0);
        Ok(())
    }

    /// The reply-ack carrier gets the same source check, since it is
    /// the form modern clients actually send.
    #[test]
    fn process_incoming_reply_ack_from_a_third_party_does_not_clear_pending() -> TestResult {
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Hello");

        let spoofed = parse_msg(format!(":N0CALL   :hi{{07}}{id}").as_bytes())?;
        assert!(
            !m.process_incoming(&test_source("K9XYZ"), &spoofed),
            "a reply-ack from a third party must not clear the pending message"
        );
        assert_eq!(m.pending_count(), 1);

        let genuine = parse_msg(format!(":N0CALL   :hi{{08}}{id}").as_bytes())?;
        assert!(m.process_incoming(&test_source("W1AW"), &genuine));
        assert_eq!(m.pending_count(), 0);
        Ok(())
    }

    #[test]
    fn process_incoming_rej_removes_pending() -> TestResult {
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Hello");

        let rej = parse_msg(format!(":N0CALL   :rej{id}").as_bytes())?;
        assert!(m.process_incoming(&test_source("W1AW"), &rej));
        assert_eq!(m.pending_count(), 0);
        Ok(())
    }

    #[test]
    fn process_incoming_unrelated_message_returns_false() -> TestResult {
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "Hello");

        let unrelated = parse_msg(b":N0CALL   :Just a regular message{42")?;
        assert!(!m.process_incoming(&test_source("W1AW"), &unrelated));
        assert_eq!(m.pending_count(), 1);
        Ok(())
    }

    #[test]
    fn build_ack_produces_valid_frame() -> TestResult {
        let m = test_messenger();
        let wire = m.build_ack(&test_addressee("W1AW"), &test_message_id("42"));

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let msg = parse_msg(packet.information())?;
        assert_eq!(msg.addressee().as_str(), "W1AW");
        assert_eq!(msg.text(), "ack42");
        Ok(())
    }

    #[test]
    fn build_rej_produces_valid_frame() -> TestResult {
        let m = test_messenger();
        let wire = m.build_rej(&test_addressee("W1AW"), &test_message_id("42"));

        let kiss = KissFrame::from_bytes(&wire)?;
        let packet = Ax25Packet::from_bytes(&kiss.data)?;
        let msg = parse_msg(packet.information())?;
        assert_eq!(msg.addressee().as_str(), "W1AW");
        assert_eq!(msg.text(), "rej42");
        Ok(())
    }

    #[test]
    fn cleanup_expired_removes_maxed_messages() {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "Test");

        // Exhaust all retries by advancing time past the retry interval
        // each round. Sans-io: we mint the timestamps; no real waiting.
        let mut clock = t0;
        for _ in 0..MAX_RETRIES {
            clock += RETRY_INTERVAL;
            drop(m.next_frame_to_send(clock));
        }

        assert_eq!(m.pending_count(), 1); // Still present, just exhausted.
        assert_eq!(m.cleanup_expired(clock), Vec::<MessageId>::new());
        let expired = m.cleanup_expired(clock + RETRY_INTERVAL);
        assert_eq!(expired, vec![id]);
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn oversized_message_text_is_rejected_before_queueing() {
        let m = test_messenger();
        let long = "x".repeat(100);
        assert!(MessageText::new(&long).is_err());
        assert_eq!(m.pending_count(), 0);
    }

    #[test]
    fn send_message_accepts_boundary_length() {
        let mut m = test_messenger();
        let text = "x".repeat(67);
        let _id = queue_test_message(&mut m, "W1AW", &text);
        assert_eq!(m.pending_count(), 1);
    }

    #[test]
    fn is_new_incoming_dedup_matches_source_msgno() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = parse_msg(b":N0CALL   :hello{42")?;
        assert!(m.is_new_incoming(&test_source("W1AW"), &msg, t0)?);
        assert!(!m.is_new_incoming(&test_source("W1AW"), &msg, t0)?);
        // Different source → not a duplicate.
        assert!(m.is_new_incoming(&test_source("W2AW"), &msg, t0)?);
        Ok(())
    }

    #[test]
    fn is_new_incoming_no_id_always_new() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = parse_msg(b":N0CALL   :hello")?;
        assert!(m.is_new_incoming(&test_source("W1AW"), &msg, t0)?);
        assert!(m.is_new_incoming(&test_source("W1AW"), &msg, t0)?);
        Ok(())
    }

    #[test]
    fn is_new_incoming_expires_stale_entries() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let msg = parse_msg(b":N0CALL   :hello{42")?;
        assert!(m.is_new_incoming(&test_source("W1AW"), &msg, t0)?);
        // Jump past the dedup window; the entry should be expired.
        let later = t0 + INCOMING_DEDUP_WINDOW + Duration::from_secs(1);
        assert!(m.is_new_incoming(&test_source("W1AW"), &msg, later)?);
        Ok(())
    }

    #[test]
    fn regressing_clock_preserves_incoming_dedup_entry() -> TestResult {
        let earlier = Instant::now();
        let t0 = earlier + Duration::from_secs(1);
        let mut messenger = test_messenger();
        let message = parse_msg(b":N0CALL   :hello{42")?;

        messenger.mark_incoming_seen(&test_source("W1AW"), &message, t0)?;
        assert!(messenger.is_duplicate_incoming(&test_source("W1AW"), &message, earlier));

        messenger.mark_incoming_seen(&test_source("W1AW"), &message, earlier)?;
        let inside_window_offset = INCOMING_DEDUP_WINDOW
            .checked_sub(Duration::from_secs(1))
            .ok_or("incoming dedup window must exceed one second")?;
        let still_inside_original_window = t0
            .checked_add(inside_window_offset)
            .ok_or("test instant must support the dedup-window offset")?;
        assert!(messenger.is_duplicate_incoming(
            &test_source("W1AW"),
            &message,
            still_inside_original_window,
        ));
        Ok(())
    }

    #[test]
    fn process_incoming_ignores_false_positive_message() -> TestResult {
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "Hello");

        // Message text that merely resembles an ack must not clear a pending message.
        let false_ack = parse_msg(b":N0CALL   :acknowledge receipt")?;
        assert!(!m.process_incoming(&test_source("W1AW"), &false_ack));
        assert_eq!(m.pending_count(), 1);
        Ok(())
    }

    #[test]
    fn process_incoming_reply_ack_clears_pending() -> TestResult {
        let mut m = test_messenger();
        // Pending outbound message "1".
        let id = queue_test_message(&mut m, "W1AW", "ping");
        assert_eq!(id.as_str(), "1");
        assert_eq!(m.pending_count(), 1);

        // Incoming ":N0CALL   :hi{05}1", a *new* inbound message id "05"
        // whose reply-ack "1" acknowledges our pending "1". Mirrors what
        // parse_aprs_message yields for that wire form.
        let reply_ack = parse_msg(format!(":N0CALL   :hi{{05}}{id}").as_bytes())?;
        assert!(
            m.process_incoming(&test_source("W1AW"), &reply_ack),
            "reply-ack should clear the matching pending message",
        );
        assert_eq!(m.pending_count(), 0);
        Ok(())
    }

    #[test]
    fn process_incoming_reply_ack_message_is_still_new_incoming() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let id = queue_test_message(&mut m, "W1AW", "ping");

        // A reply-ack message acks our outbound AND is a fresh inbound
        // message in its own right, so is_new_incoming must still surface it.
        let reply_ack = parse_msg(format!(":N0CALL   :hi{{05}}{id}").as_bytes())?;
        assert!(
            m.is_new_incoming(&test_source("W1AW"), &reply_ack, t0)?,
            "reply-ack message id 05 must be surfaced as a new incoming",
        );
        assert!(m.process_incoming(&test_source("W1AW"), &reply_ack));
        assert_eq!(m.pending_count(), 0);
        // Same message arriving again is a duplicate by (source, msgno).
        assert!(!m.is_new_incoming(&test_source("W1AW"), &reply_ack, t0)?);
        Ok(())
    }

    #[test]
    fn process_incoming_reply_ack_no_match_does_not_panic() -> TestResult {
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "ping");

        // Reply-ack "99" matches no pending message: must not panic, must
        // not clear anything, and the message is still a new incoming.
        let reply_ack = parse_msg(b":N0CALL   :unrelated{07}99")?;
        assert!(
            !m.process_incoming(&test_source("W1AW"), &reply_ack),
            "reply-ack with no matching pending clears nothing",
        );
        assert_eq!(m.pending_count(), 1);
        assert!(
            m.is_new_incoming(&test_source("W1AW"), &reply_ack, t0)?,
            "non-matching reply-ack message is still surfaced as new incoming",
        );
        Ok(())
    }

    #[test]
    fn first_frame_emitted_immediately_then_second_waits() {
        // BUG-2 regression: the first transmission must be eligible at the
        // very same `now` the message was queued, independent of the
        // monotonic clock's origin (no checked_sub backdating). The second
        // send must still wait a full retry_interval.
        let t0 = Instant::now();
        let mut m = test_messenger();
        let _id = queue_test_message(&mut m, "W1AW", "ping");

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
        let mut m = test_messenger();
        m.next_message_id = NonZeroU16::MAX;
        let id1 = queue_test_message(&mut m, "W1AW", "A");
        assert_eq!(id1.as_str(), u16::MAX.to_string());
        // After wrapping, 0 is skipped, so next is 1.
        let id2 = queue_test_message(&mut m, "W1AW", "B");
        assert_eq!(id2.as_str(), "1");
    }
}
