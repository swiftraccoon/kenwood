//! APRS message ack/retry manager.
//!
//! Provides reliable APRS messaging with automatic acknowledgement tracking
//! and retry logic. Messages are retried up to [`MAX_RETRIES`] times at
//! [`RETRY_INTERVAL`] intervals until acknowledged or expired.

use std::time::{Duration, Instant};

use super::{AprsMessage, Ax25Address, build_aprs_message};

/// Maximum number of transmission attempts per message before giving up.
pub const MAX_RETRIES: u8 = 5;

/// Interval between retry attempts.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(30);

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
/// acknowledgements, and generates retry frames on schedule.
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
}

impl AprsMessenger {
    /// Create a new messenger for the given callsign and digipeater path.
    #[must_use]
    pub const fn new(callsign: Ax25Address, digipeater_path: Vec<Ax25Address>) -> Self {
        Self {
            my_callsign: callsign,
            digipeater_path,
            pending_messages: Vec::new(),
            next_message_id: 1,
        }
    }

    /// Queue a message for transmission. Returns the assigned message ID.
    ///
    /// The message is immediately available from [`next_frame_to_send`](Self::next_frame_to_send).
    pub fn send_message(&mut self, addressee: &str, text: &str) -> String {
        let message_id = self.next_message_id.to_string();
        self.next_message_id = self.next_message_id.wrapping_add(1);
        if self.next_message_id == 0 {
            self.next_message_id = 1;
        }

        let wire_frame = build_aprs_message(
            &self.my_callsign,
            addressee,
            text,
            Some(&message_id),
            &self.digipeater_path,
        );

        // Use a time in the past so the message is immediately eligible.
        let now = Instant::now();
        let past = now.checked_sub(RETRY_INTERVAL).unwrap_or(now);

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
    /// Returns `None` if no messages need sending right now.
    /// Retries every 30 seconds, up to 5 attempts.
    #[must_use]
    pub fn next_frame_to_send(&mut self) -> Option<Vec<u8>> {
        let now = Instant::now();
        for msg in &mut self.pending_messages {
            if msg.attempts < MAX_RETRIES && now.duration_since(msg.last_sent) >= RETRY_INTERVAL {
                msg.attempts += 1;
                msg.last_sent = now;
                return Some(msg.wire_frame.clone());
            }
        }
        None
    }

    /// Process an incoming APRS message.
    ///
    /// If it is an ack for a pending message, removes the pending entry
    /// and returns `true`. Returns `false` otherwise.
    pub fn process_incoming(&mut self, msg: &AprsMessage) -> bool {
        // Check if this is an ack message: addressee is us, text starts with "ack"
        if msg.text.starts_with("ack") {
            let acked_id = &msg.text[3..];
            let before = self.pending_messages.len();
            self.pending_messages.retain(|p| p.message_id != acked_id);
            return self.pending_messages.len() < before;
        }

        // Check if this is a rej message: addressee is us, text starts with "rej"
        if msg.text.starts_with("rej") {
            let rejected_id = &msg.text[3..];
            let before = self.pending_messages.len();
            self.pending_messages
                .retain(|p| p.message_id != rejected_id);
            return self.pending_messages.len() < before;
        }

        false
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

    /// Remove expired messages (those that have reached [`MAX_RETRIES`] attempts).
    pub fn cleanup_expired(&mut self) {
        self.pending_messages.retain(|m| m.attempts < MAX_RETRIES);
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
        let _id = m.send_message("W1AW", "Test");

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
        m.cleanup_expired();
        assert_eq!(m.pending_count(), 0);
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
