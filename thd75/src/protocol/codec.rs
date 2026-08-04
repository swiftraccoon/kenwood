//! Frame-level codec for `\r`-terminated CAT protocol messages.
//!
//! The TH-D75 CAT protocol uses carriage return (`\r`, 0x0D) as the
//! frame delimiter for both commands and responses. Each message is a
//! sequence of ASCII bytes terminated by a single `\r`. There is no
//! length prefix or checksum; framing relies entirely on the delimiter.
//!
//! This codec sits between the raw serial byte stream and the protocol
//! parser. The data flow is:
//!
//! ```text
//! Serial port  -->  Codec::feed()  -->  Codec::next_frame()  -->  parse()
//!              raw bytes          buffered             complete frame    typed Response
//! ```
//!
//! On the transmit side, [`super::serialize`] produces the wire bytes
//! (including the trailing `\r`) that are written directly to the serial
//! port; the codec is not involved in outbound framing.
//!
//! The codec maintains an internal buffer that accumulates bytes from
//! successive [`Codec::feed`] calls. When [`Codec::next_frame`] finds a
//! `\r`, it extracts everything before it as a complete frame (without
//! the delimiter) and drains those bytes from the buffer. The buffer is
//! bounded at 64 KiB to prevent unbounded growth if the serial link delivers
//! noise without any `\r` terminators. Exceeding that bound returns an error
//! and poisons the codec; no suffix is retained and reinterpreted as a fresh
//! frame. The underlying byte stream must be reopened or brought to a proven
//! frame boundary before [`Codec::clear`] resets that poison.

use crate::error::ProtocolError;

#[derive(Debug, Clone, Copy)]
struct FrameOverflow {
    buffered: usize,
    incoming: usize,
}

impl FrameOverflow {
    const fn into_error(self) -> ProtocolError {
        ProtocolError::FrameTooLong {
            maximum: Codec::MAX_BUFFERED_BYTES,
            buffered: self.buffered,
            incoming: self.incoming,
        }
    }
}

/// Frame-level codec for `\r`-terminated CAT protocol messages.
///
/// Buffers incoming bytes and emits complete frames. Handles partial
/// reads gracefully; the radio may send responses in multiple chunks.
#[derive(Debug)]
pub struct Codec {
    buffer: Vec<u8>,
    overflow: Option<FrameOverflow>,
}

impl Codec {
    /// Creates a new codec with an empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            overflow: None,
        }
    }

    /// Maximum unconsumed input retained by the framing codec (64 KiB).
    ///
    /// A normal TH-D75 CAT frame is tiny compared with this defensive bound.
    /// Reaching it indicates a missing delimiter, transport corruption, or a
    /// caller that is not draining complete frames.
    pub const MAX_BUFFERED_BYTES: usize = 64 * 1024;

    /// Appends raw bytes to the internal buffer.
    ///
    /// If the feed would exceed [`Self::MAX_BUFFERED_BYTES`], all buffered
    /// bytes are discarded and the codec is poisoned. The error must be
    /// handled by reopening the underlying stream or otherwise proving a new
    /// frame boundary, then calling [`Self::clear`] before more input can be
    /// accepted. This fail-closed state prevents an arbitrary suffix of an
    /// oversized frame from being parsed as a valid response.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::FrameTooLong`] on the feed that crosses the
    /// bound and on every later feed until the codec is cleared.
    pub fn feed(&mut self, data: &[u8]) -> Result<(), ProtocolError> {
        if let Some(overflow) = self.overflow {
            return Err(overflow.into_error());
        }

        tracing::trace!(bytes = data.len(), "codec: feeding bytes");
        let buffered = self.buffer.len();
        let fits = buffered
            .checked_add(data.len())
            .is_some_and(|total| total <= Self::MAX_BUFFERED_BYTES);
        if !fits {
            let overflow = FrameOverflow {
                buffered,
                incoming: data.len(),
            };
            tracing::warn!(
                buffered,
                incoming = data.len(),
                maximum = Self::MAX_BUFFERED_BYTES,
                "codec framing buffer exceeded its maximum; poisoning stream"
            );
            self.buffer.clear();
            self.overflow = Some(overflow);
            return Err(overflow.into_error());
        }
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    /// Discards all buffered bytes.
    ///
    /// Used to resynchronize after a command timeout: a partial frame left in
    /// the buffer belongs to a response that will never be completed and must
    /// not prefix the next command's response. If an oversized feed poisoned
    /// the codec, the caller must first reopen the underlying stream or prove
    /// that it is positioned at a new frame boundary.
    pub fn clear(&mut self) {
        if !self.buffer.is_empty() || self.overflow.is_some() {
            tracing::debug!(
                discarded = self.buffer.len(),
                poisoned = self.overflow.is_some(),
                "codec: clearing stale buffered bytes"
            );
            self.buffer.clear();
            self.overflow = None;
        }
    }

    /// Reports whether no partial or complete frame is buffered.
    ///
    /// Strict exchanges use this to refuse an attestation when bytes from an
    /// earlier command could be mistaken for the response being proved.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buffer.is_empty() && self.overflow.is_none()
    }

    /// Extracts the next complete frame from the buffer, if available.
    ///
    /// Searches for a `\r` delimiter, extracts everything before it as a
    /// frame (without the trailing `\r`), and removes the consumed bytes
    /// from the buffer. Returns `None` if no complete frame is available.
    ///
    /// Leading `\n` bytes are skipped before framing: NMEA sentences on
    /// the shared serial stream end `\r\n`, so after splitting on `\r`
    /// the stray `\n` would otherwise corrupt the next frame's mnemonic.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let skip = self.buffer.iter().take_while(|&&b| b == b'\n').count();
        if skip > 0 {
            drop(self.buffer.drain(..skip));
        }
        let pos = self.buffer.iter().position(|&b| b == b'\r')?;
        let frame = self.buffer.get(..pos)?.to_vec();
        drop(self.buffer.drain(..=pos));
        tracing::debug!(frame_len = frame.len(), "codec: extracted frame");
        tracing::trace!(frame = ?frame, "codec: frame content");
        Some(frame)
    }
}

impl Default for Codec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_ok(codec: &mut Codec, data: &[u8]) {
        let result = codec.feed(data);
        assert!(result.is_ok(), "valid feed failed: {result:?}");
    }

    #[test]
    fn single_complete_frame() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"FV 1.03.000\r");
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn partial_then_complete() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"FV 1.0");
        assert_eq!(codec.next_frame(), None);
        feed_ok(&mut codec, b"3.000\r");
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
    }

    #[test]
    fn multiple_frames_in_one_feed() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"ID TH-D75\rFV 1.03.000\r");
        assert_eq!(codec.next_frame(), Some(b"ID TH-D75".to_vec()));
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn error_frame() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"?\r");
        assert_eq!(codec.next_frame(), Some(b"?".to_vec()));
    }

    #[test]
    fn empty_feed() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"");
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn frame_with_commas() -> Result<(), Box<dyn std::error::Error>> {
        let mut codec = Codec::new();
        codec
            .feed(b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r")?;
        let frame = codec.next_frame().ok_or("next_frame returned None")?;
        assert!(frame.starts_with(b"FO"));
        Ok(())
    }

    #[test]
    fn skips_newline_residue_between_frames() {
        // NMEA sentences end "\r\n" while CAT frames end "\r". After
        // splitting an NMEA sentence on '\r', the stray '\n' must not
        // become the first byte of the next CAT frame (it would corrupt
        // the mnemonic to "\nF" and fail an otherwise-valid response).
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"$GPGGA,123519,4807.038,N*47\r\nFV 1.03.000\r");
        assert_eq!(
            codec.next_frame(),
            Some(b"$GPGGA,123519,4807.038,N*47".to_vec())
        );
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
    }

    #[test]
    fn skips_leading_newline_at_buffer_start() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"\nID TH-D75\r");
        assert_eq!(codec.next_frame(), Some(b"ID TH-D75".to_vec()));
    }

    #[test]
    fn clear_discards_partial_frame() {
        let mut codec = Codec::new();
        feed_ok(&mut codec, b"FQ 0,01455");
        codec.clear();
        feed_ok(&mut codec, b"MD 0,0\r");
        // The stale partial frame must not prefix the new one.
        assert_eq!(codec.next_frame(), Some(b"MD 0,0".to_vec()));
    }

    #[test]
    fn oversized_input_poison_requires_explicit_resynchronization() {
        let mut codec = Codec::new();
        // Fill the exact bound without a delimiter, then cross it.
        let chunk = [b'A'; 4096];
        for _ in 0..16 {
            feed_ok(&mut codec, &chunk);
        }
        assert_eq!(codec.buffer.len(), Codec::MAX_BUFFERED_BYTES);

        let overflow = codec.feed(&chunk);
        assert!(matches!(
            overflow,
            Err(ProtocolError::FrameTooLong {
                maximum: Codec::MAX_BUFFERED_BYTES,
                buffered: Codec::MAX_BUFFERED_BYTES,
                incoming: 4096,
            })
        ));
        assert!(
            codec.buffer.is_empty(),
            "oversized prefix must be discarded"
        );
        assert!(!codec.is_empty(), "a poisoned codec is not synchronized");
        assert_eq!(codec.next_frame(), None);

        let retry = codec.feed(b"ID TH-D75\r");
        assert!(matches!(retry, Err(ProtocolError::FrameTooLong { .. })));

        codec.clear();
        assert!(codec.is_empty());
        feed_ok(&mut codec, b"ID TH-D75\r");
        assert_eq!(codec.next_frame(), Some(b"ID TH-D75".to_vec()));
    }
}
