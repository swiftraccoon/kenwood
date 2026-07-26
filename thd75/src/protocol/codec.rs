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
//! capped at 64 KB to prevent unbounded growth if the serial link
//! delivers noise without any `\r` terminators.

/// Frame-level codec for `\r`-terminated CAT protocol messages.
///
/// Buffers incoming bytes and emits complete frames. Handles partial
/// reads gracefully; the radio may send responses in multiple chunks.
#[derive(Debug)]
pub struct Codec {
    buffer: Vec<u8>,
}

impl Codec {
    /// Creates a new codec with an empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    /// Maximum buffer size (64 KB). Prevents unbounded growth if the
    /// radio never sends a `\r` terminator (e.g., corrupted serial link).
    const MAX_BUFFER: usize = 64 * 1024;

    /// Appends raw bytes to the internal buffer.
    ///
    /// If the buffer would exceed 64 KB, it is truncated to prevent
    /// unbounded memory growth.
    pub fn feed(&mut self, data: &[u8]) {
        tracing::trace!(bytes = data.len(), "codec: feeding bytes");
        self.buffer.extend_from_slice(data);
        if self.buffer.len() > Self::MAX_BUFFER {
            tracing::warn!(
                len = self.buffer.len(),
                "codec buffer exceeded max size, truncating"
            );
            drop(self.buffer.drain(..self.buffer.len() - Self::MAX_BUFFER));
        }
    }

    /// Discards all buffered bytes.
    ///
    /// Used to resynchronize after a command timeout: a partial frame
    /// left in the buffer belongs to a response that will never be
    /// completed and must not prefix the next command's response.
    pub fn clear(&mut self) {
        if !self.buffer.is_empty() {
            tracing::debug!(
                discarded = self.buffer.len(),
                "codec: clearing stale buffered bytes"
            );
            self.buffer.clear();
        }
    }

    /// Reports whether no partial or complete frame is buffered.
    ///
    /// Strict exchanges use this to refuse an attestation when bytes from an
    /// earlier command could be mistaken for the response being proved.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.buffer.is_empty()
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
        tracing::trace!(frame = %String::from_utf8_lossy(&frame), "codec: frame content");
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

    #[test]
    fn single_complete_frame() {
        let mut codec = Codec::new();
        codec.feed(b"FV 1.03.000\r");
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn partial_then_complete() {
        let mut codec = Codec::new();
        codec.feed(b"FV 1.0");
        assert_eq!(codec.next_frame(), None);
        codec.feed(b"3.000\r");
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
    }

    #[test]
    fn multiple_frames_in_one_feed() {
        let mut codec = Codec::new();
        codec.feed(b"ID TH-D75\rFV 1.03.000\r");
        assert_eq!(codec.next_frame(), Some(b"ID TH-D75".to_vec()));
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn error_frame() {
        let mut codec = Codec::new();
        codec.feed(b"?\r");
        assert_eq!(codec.next_frame(), Some(b"?".to_vec()));
    }

    #[test]
    fn empty_feed() {
        let mut codec = Codec::new();
        codec.feed(b"");
        assert_eq!(codec.next_frame(), None);
    }

    #[test]
    fn frame_with_commas() -> Result<(), Box<dyn std::error::Error>> {
        let mut codec = Codec::new();
        codec.feed(b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r");
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
        codec.feed(b"$GPGGA,123519,4807.038,N*47\r\nFV 1.03.000\r");
        assert_eq!(
            codec.next_frame(),
            Some(b"$GPGGA,123519,4807.038,N*47".to_vec())
        );
        assert_eq!(codec.next_frame(), Some(b"FV 1.03.000".to_vec()));
    }

    #[test]
    fn skips_leading_newline_at_buffer_start() {
        let mut codec = Codec::new();
        codec.feed(b"\nID TH-D75\r");
        assert_eq!(codec.next_frame(), Some(b"ID TH-D75".to_vec()));
    }

    #[test]
    fn clear_discards_partial_frame() {
        let mut codec = Codec::new();
        codec.feed(b"FQ 0,01455");
        codec.clear();
        codec.feed(b"MD 0,0\r");
        // The stale partial frame must not prefix the new one.
        assert_eq!(codec.next_frame(), Some(b"MD 0,0".to_vec()));
    }

    #[test]
    fn buffer_capped_at_max_size() {
        let mut codec = Codec::new();
        // Feed >64KB without a \r terminator
        let chunk = [b'A'; 4096];
        for _ in 0..20 {
            codec.feed(&chunk); // 20 * 4096 = 80KB
        }
        assert!(codec.buffer.len() <= Codec::MAX_BUFFER);
        // No frame should be extractable (no \r in the noise)
        assert_eq!(codec.next_frame(), None);
    }
}
