//! Frame-level codec for `\r`-terminated CAT protocol messages.

/// Frame-level codec for `\r`-terminated CAT protocol messages.
///
/// Buffers incoming bytes and emits complete frames. Handles partial
/// reads gracefully — the radio may send responses in multiple chunks.
#[derive(Debug)]
pub struct Codec {
    buffer: Vec<u8>,
}

impl Codec {
    /// Creates a new codec with an empty buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
        }
    }

    /// Appends raw bytes to the internal buffer.
    pub fn feed(&mut self, data: &[u8]) {
        tracing::trace!(bytes = data.len(), "codec: feeding bytes");
        self.buffer.extend_from_slice(data);
    }

    /// Extracts the next complete frame from the buffer, if available.
    ///
    /// Searches for a `\r` delimiter, extracts everything before it as a
    /// frame (without the trailing `\r`), and removes the consumed bytes
    /// from the buffer. Returns `None` if no complete frame is available.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let pos = self.buffer.iter().position(|&b| b == b'\r')?;
        let frame = self.buffer[..pos].to_vec();
        let _ = self.buffer.drain(..=pos);
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
    fn frame_with_commas() {
        let mut codec = Codec::new();
        codec.feed(b"FO 0,0145000000,0000600000,5,1,0,1,0,0,0,0,0,0,0,08,08,000,0,,0,00\r");
        let frame = codec.next_frame().unwrap();
        assert!(frame.starts_with(b"FO"));
    }
}
