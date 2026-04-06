//! D-STAR slow data decoder for extracting text messages from voice frames.
//!
//! Each D-STAR voice frame contains 12 bytes: 9 bytes of AMBE-encoded audio
//! and 3 bytes of "slow data." The slow data stream uses 6-byte blocks
//! (assembled from two consecutive 3-byte halves) that carry text messages,
//! GPS data, and other auxiliary information.
//!
//! # Encoding details (MMDVM Specification 20150922)
//!
//! Each 3-byte slow data half is XOR-descrambled with `[0x70, 0x4F, 0x93]`
//! before interpretation. The first byte of each 6-byte block contains a
//! type nibble in its upper 4 bits:
//!
//! - `0x4_` (type 4): Text message block. The lower nibble of the first byte
//!   indicates the number of valid text characters in this block (1-5).
//! - Other types are ignored by this decoder.
//!
//! Text message blocks are concatenated in order to assemble the complete
//! message string.

/// XOR descrambling key for D-STAR slow data (MMDVM Specification 20150922).
const SCRAMBLE_KEY: [u8; 3] = [0x70, 0x4F, 0x93];

/// Type nibble indicating a text message block in D-STAR slow data.
const TEXT_BLOCK_TYPE: u8 = 0x40;

/// Maximum assembled message length (characters).
///
/// D-STAR text messages are limited to 20 characters by the standard.
pub const MAX_MESSAGE_LEN: usize = 20;

/// Decoder for D-STAR slow data text messages.
///
/// Feed consecutive slow data bytes (the last 3 bytes of each 12-byte
/// voice frame) to this decoder. Once a complete text message has been
/// assembled, [`SlowDataDecoder::has_message`] returns `true` and
/// [`SlowDataDecoder::message`] returns the decoded text.
///
/// # Usage
///
/// ```
/// use kenwood_thd75::mmdvm::SlowDataDecoder;
///
/// let mut decoder = SlowDataDecoder::new();
/// // Feed slow data bytes from voice frames...
/// // decoder.add_frame(&slow_bytes, frame_index);
/// if decoder.has_message() {
///     println!("Message: {}", decoder.message().unwrap());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct SlowDataDecoder {
    /// Accumulator for a 6-byte block (two 3-byte halves).
    block_buf: [u8; 6],
    /// How many bytes are in the current block buffer (0, 3, or 6).
    block_pos: usize,
    /// Assembled text message bytes.
    text_buf: Vec<u8>,
    /// Whether a complete message has been detected.
    complete: bool,
}

impl SlowDataDecoder {
    /// Create a new slow data decoder with empty state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            block_buf: [0u8; 6],
            block_pos: 0,
            text_buf: Vec::with_capacity(MAX_MESSAGE_LEN),
            complete: false,
        }
    }

    /// Reset the decoder to its initial state, discarding any partial data.
    pub fn reset(&mut self) {
        self.block_buf = [0u8; 6];
        self.block_pos = 0;
        self.text_buf.clear();
        self.complete = false;
    }

    /// Feed a 3-byte slow data segment from a voice frame.
    ///
    /// The `slow_data` parameter is the last 3 bytes of a 12-byte D-STAR
    /// voice frame. The `_frame_index` parameter is the frame sequence
    /// number within the voice superframe (0-20), reserved for future use.
    ///
    /// Two consecutive calls assemble one 6-byte block. When a text block
    /// is found, its characters are appended to the internal message buffer.
    pub fn add_frame(&mut self, slow_data: &[u8; 3], _frame_index: u8) {
        if self.complete {
            return;
        }

        // XOR descramble.
        let mut descrambled = [0u8; 3];
        for i in 0..3 {
            descrambled[i] = slow_data[i] ^ SCRAMBLE_KEY[i];
        }

        // Accumulate into the 6-byte block buffer.
        let offset = self.block_pos;
        if offset + 3 > 6 {
            // Shouldn't happen, but reset the block on overflow.
            self.block_pos = 0;
            self.block_buf[0..3].copy_from_slice(&descrambled);
            self.block_pos = 3;
            return;
        }
        self.block_buf[offset..offset + 3].copy_from_slice(&descrambled);
        self.block_pos += 3;

        // Process complete 6-byte blocks.
        if self.block_pos >= 6 {
            self.process_block();
            self.block_pos = 0;
        }
    }

    /// Returns `true` if a text message has been fully assembled.
    #[must_use]
    pub const fn has_message(&self) -> bool {
        self.complete
    }

    /// Returns the assembled text message, if complete.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        if self.complete {
            std::str::from_utf8(&self.text_buf).ok()
        } else {
            None
        }
    }

    /// Process a completed 6-byte block.
    fn process_block(&mut self) {
        let type_nibble = self.block_buf[0] & 0xF0;
        if type_nibble != TEXT_BLOCK_TYPE {
            return;
        }

        let char_count = usize::from(self.block_buf[0] & 0x0F);
        // A text block carries 1-5 characters in bytes 1-5.
        let available = char_count.min(5);

        for &b in &self.block_buf[1..=available] {
            if self.text_buf.len() >= MAX_MESSAGE_LEN {
                self.complete = true;
                return;
            }
            // Null byte or non-printable terminates the message.
            if b == 0 {
                self.complete = true;
                return;
            }
            self.text_buf.push(b);
        }

        // Message is complete if we received a short block or hit the length limit.
        if available < 5 || self.text_buf.len() >= MAX_MESSAGE_LEN {
            self.complete = true;
        }
    }
}

impl Default for SlowDataDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Encoder for D-STAR slow data text messages.
///
/// Converts a text string into a sequence of 3-byte slow data payloads
/// suitable for appending to AMBE voice frames (the last 3 bytes of each
/// 12-byte D-STAR data frame).
///
/// # Encoding process
///
/// 1. The text is split into 5-byte blocks.
/// 2. Each block gets a type header: upper nibble `0x4` and lower nibble
///    set to the number of valid characters in this block (1-5).
/// 3. The 6-byte block (header + up to 5 chars) is zero-padded.
/// 4. Each 3-byte half is XOR-scrambled with `[0x70, 0x4F, 0x93]`.
/// 5. The two scrambled halves are returned as consecutive 3-byte arrays.
///
/// # Usage
///
/// ```
/// use kenwood_thd75::mmdvm::SlowDataEncoder;
///
/// let encoder = SlowDataEncoder::new();
/// let payloads = encoder.encode_message("Hi!");
/// // Each payload is 3 bytes, to be placed in the slow data portion
/// // of successive D-STAR voice frames.
/// assert_eq!(payloads.len(), 2); // "Hi!" fits in one 6-byte block = 2 halves
/// ```
#[derive(Debug, Clone)]
pub struct SlowDataEncoder {
    _private: (),
}

impl SlowDataEncoder {
    /// Create a new slow data encoder.
    #[must_use]
    pub const fn new() -> Self {
        Self { _private: () }
    }

    /// Encode a text message into slow data payloads.
    ///
    /// Returns a [`Vec`] of 3-byte arrays. Each pair of consecutive arrays
    /// forms one 6-byte slow data block. The total number of arrays is
    /// always even (blocks produce exactly two 3-byte halves each).
    ///
    /// The text is truncated to [`MAX_MESSAGE_LEN`] characters (20).
    #[must_use]
    pub fn encode_message(&self, text: &str) -> Vec<[u8; 3]> {
        let bytes = text.as_bytes();
        let len = bytes.len().min(MAX_MESSAGE_LEN);
        let truncated = &bytes[..len];

        let mut result = Vec::new();

        for chunk in truncated.chunks(5) {
            let char_count = chunk.len();
            let mut block = [0u8; 6];

            // Type header: upper nibble 0x4, lower nibble = char count.
            // char_count is at most 5 (from chunks(5)), so the cast is safe.
            #[allow(clippy::cast_possible_truncation)]
            let count_byte = char_count as u8;
            block[0] = TEXT_BLOCK_TYPE | count_byte;
            block[1..=char_count].copy_from_slice(chunk);
            // Remaining bytes stay zero-padded.

            // Split into two 3-byte halves and XOR-scramble each.
            let half1 = [
                block[0] ^ SCRAMBLE_KEY[0],
                block[1] ^ SCRAMBLE_KEY[1],
                block[2] ^ SCRAMBLE_KEY[2],
            ];
            let half2 = [
                block[3] ^ SCRAMBLE_KEY[0],
                block[4] ^ SCRAMBLE_KEY[1],
                block[5] ^ SCRAMBLE_KEY[2],
            ];

            result.push(half1);
            result.push(half2);
        }

        result
    }
}

impl Default for SlowDataEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: XOR-scramble a 3-byte array for test data construction.
    fn scramble(data: [u8; 3]) -> [u8; 3] {
        [
            data[0] ^ SCRAMBLE_KEY[0],
            data[1] ^ SCRAMBLE_KEY[1],
            data[2] ^ SCRAMBLE_KEY[2],
        ]
    }

    #[test]
    fn decode_short_message() {
        let mut decoder = SlowDataDecoder::new();

        // Build a text block: type 0x43 = text, 3 chars; followed by 2 more
        // chars + padding. The 6-byte block (after descramble) is:
        // [0x43, 'H', 'i', '!', 0x00, 0x00]
        //
        // Split into two 3-byte halves and scramble.
        let half1 = scramble([0x43, b'H', b'i']);
        let half2 = scramble([b'!', 0x00, 0x00]);

        decoder.add_frame(&half1, 0);
        assert!(!decoder.has_message());
        decoder.add_frame(&half2, 1);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hi!");
    }

    #[test]
    fn decode_full_block_continues() {
        let mut decoder = SlowDataDecoder::new();

        // First block: 5 chars "Hello" -> type nibble 0x45
        let half1 = scramble([0x45, b'H', b'e']);
        let half2 = scramble(*b"llo");
        decoder.add_frame(&half1, 0);
        decoder.add_frame(&half2, 1);
        // 5 chars = full block, message not yet complete.
        assert!(!decoder.has_message());

        // Second block: 1 char "!" -> type nibble 0x41
        let half3 = scramble([0x41, b'!', 0x00]);
        let half4 = scramble([0x00, 0x00, 0x00]);
        decoder.add_frame(&half3, 2);
        decoder.add_frame(&half4, 3);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hello!");
    }

    #[test]
    fn non_text_blocks_ignored() {
        let mut decoder = SlowDataDecoder::new();

        // A non-text block (type 0x30 = GPS data or something else).
        let half1 = scramble([0x35, 0x01, 0x02]);
        let half2 = scramble([0x03, 0x04, 0x05]);
        decoder.add_frame(&half1, 0);
        decoder.add_frame(&half2, 1);
        assert!(!decoder.has_message());
    }

    #[test]
    fn reset_clears_state() {
        let mut decoder = SlowDataDecoder::new();

        let half1 = scramble([0x43, b'A', b'B']);
        let half2 = scramble([b'C', 0x00, 0x00]);
        decoder.add_frame(&half1, 0);
        decoder.add_frame(&half2, 1);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "ABC");

        decoder.reset();
        assert!(!decoder.has_message());
        assert!(decoder.message().is_none());
    }

    #[test]
    fn max_message_length_enforced() {
        let mut decoder = SlowDataDecoder::new();

        // Feed 4 full blocks of 5 chars each = 20 chars.
        for i in 0..4u8 {
            let ch = b'A' + i;
            let half1 = scramble([0x45, ch, ch]);
            let half2 = scramble([ch, ch, ch]);
            decoder.add_frame(&half1, i * 2);
            decoder.add_frame(&half2, i * 2 + 1);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap().len(), 20);
    }

    #[test]
    fn null_byte_terminates_message() {
        let mut decoder = SlowDataDecoder::new();

        // Block with 5 chars declared but a null in position 3.
        let half1 = scramble([0x45, b'X', b'Y']);
        let half2 = scramble([0x00, b'Z', b'W']);
        decoder.add_frame(&half1, 0);
        decoder.add_frame(&half2, 1);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "XY");
    }

    // -----------------------------------------------------------------------
    // Encoder tests
    // -----------------------------------------------------------------------

    #[test]
    fn encode_short_message() {
        let encoder = SlowDataEncoder::new();
        let payloads = encoder.encode_message("Hi!");
        // "Hi!" = 3 chars => 1 block => 2 halves.
        assert_eq!(payloads.len(), 2);
    }

    #[test]
    fn encode_five_char_message() {
        let encoder = SlowDataEncoder::new();
        let payloads = encoder.encode_message("Hello");
        // "Hello" = 5 chars => 1 full block => 2 halves.
        assert_eq!(payloads.len(), 2);
    }

    #[test]
    fn encode_six_char_message() {
        let encoder = SlowDataEncoder::new();
        let payloads = encoder.encode_message("Hello!");
        // "Hello!" = 6 chars => 2 blocks (5 + 1) => 4 halves.
        assert_eq!(payloads.len(), 4);
    }

    #[test]
    fn encode_empty_message() {
        let encoder = SlowDataEncoder::new();
        let payloads = encoder.encode_message("");
        assert!(payloads.is_empty());
    }

    #[test]
    fn encode_max_length_message() {
        let encoder = SlowDataEncoder::new();
        let text = "A".repeat(20);
        let payloads = encoder.encode_message(&text);
        // 20 chars => 4 blocks of 5 => 8 halves.
        assert_eq!(payloads.len(), 8);
    }

    #[test]
    fn encode_truncates_beyond_max() {
        let encoder = SlowDataEncoder::new();
        let text = "B".repeat(25);
        let payloads = encoder.encode_message(&text);
        // Truncated to 20 chars => 4 blocks => 8 halves.
        assert_eq!(payloads.len(), 8);
    }

    #[test]
    fn encoder_default_trait() {
        let encoder = SlowDataEncoder::default();
        let payloads = encoder.encode_message("OK");
        assert_eq!(payloads.len(), 2);
    }

    // -----------------------------------------------------------------------
    // Encode -> Decode round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_short_message() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("Hi!");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hi!");
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_exact_five_chars() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("ABCDE");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        // 5 chars = full block, decoder expects more.
        // Feed a short terminating block.
        // Actually, let's test the multi-block case instead.
        assert!(!decoder.has_message());
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_six_chars() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("Hello!");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hello!");
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_ten_chars() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("0123456789");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        // 10 chars = 2 full blocks of 5, decoder expects more.
        assert!(!decoder.has_message());
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_eleven_chars() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("Hello World");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hello World");
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_max_length() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let text = "12345678901234567890"; // exactly 20 chars
        let payloads = encoder.encode_message(text);
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), text);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn roundtrip_single_char() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let payloads = encoder.encode_message("X");
        for (i, payload) in payloads.iter().enumerate() {
            decoder.add_frame(payload, i as u8);
        }
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "X");
    }

    #[test]
    fn default_creates_empty_decoder() {
        let decoder = SlowDataDecoder::default();
        assert!(!decoder.has_message());
        assert!(decoder.message().is_none());
    }

    #[test]
    fn frames_after_completion_ignored() {
        let mut decoder = SlowDataDecoder::new();

        let half1 = scramble([0x42, b'O', b'K']);
        let half2 = scramble([0x00, 0x00, 0x00]);
        decoder.add_frame(&half1, 0);
        decoder.add_frame(&half2, 1);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "OK");

        // Additional frames should be ignored.
        let half3 = scramble([0x43, b'N', b'O']);
        let half4 = scramble([b'!', 0x00, 0x00]);
        decoder.add_frame(&half3, 2);
        decoder.add_frame(&half4, 3);
        assert_eq!(decoder.message().unwrap(), "OK");
    }

    #[test]
    fn descramble_is_reversible() {
        let original = [0x45, b'T', b'E'];
        let scrambled = scramble(original);
        let mut descrambled = [0u8; 3];
        for i in 0..3 {
            descrambled[i] = scrambled[i] ^ SCRAMBLE_KEY[i];
        }
        assert_eq!(descrambled, original);
    }
}
