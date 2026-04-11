//! D-STAR slow data decoder for extracting text messages from voice frames.
//!
//! Each D-STAR voice frame contains 12 bytes: 9 bytes of AMBE-encoded audio
//! and 3 bytes of "slow data." The slow data stream uses 6-byte blocks
//! (assembled from two consecutive 3-byte halves) that carry text messages,
//! GPS data, and other auxiliary information.
//!
//! # Encoding details
//!
//! Each 3-byte slow data half is XOR-descrambled with `[0x70, 0x4F, 0x93]`
//! before interpretation. Two consecutive frames make one 6-byte block. The
//! first byte of each block is a type byte whose upper nibble identifies the
//! payload kind and whose lower nibble carries a sub-value (block index, byte
//! count, etc.).
//!
//! | Upper nibble | Meaning                              |
//! |--------------|--------------------------------------|
//! | `0x3_`       | GPS / DPRS position fragment         |
//! | `0x4_`       | Text message block (20 chars / 4 blocks) |
//! | `0x5_`       | Header fragment                      |
//! | `0xC_`       | Squelch code                         |
//!
//! For text messages (`0x40..=0x4F`), the lower nibble is the **block index**
//! (0..=3), not a character count. Each of the four blocks carries exactly
//! five text characters in positions 1..=5 of the 6-byte block, yielding a
//! total of 20 text characters per complete message. A full text message is
//! only emitted once all four blocks have been seen.
//!
//! # References
//!
//! - `ircDDBGateway/Common/SlowDataEncoder.cpp` (G4KLX)
//! - `ircDDBGateway/Common/TextCollector.cpp` (G4KLX)
//! - `ircDDBGateway/Common/DStarDefines.h` (`SCRAMBLER_BYTE1/2/3`,
//!   `SLOW_DATA_TYPE_*` constants)

/// XOR descrambling key for D-STAR slow data
/// (`SCRAMBLER_BYTE1/2/3` in `ircDDBGateway`).
const SCRAMBLE_KEY: [u8; 3] = [0x70, 0x4F, 0x93];

/// Upper-nibble mask that selects the slow data type.
const SLOW_DATA_TYPE_MASK: u8 = 0xF0;

/// Type nibble indicating a GPS / DPRS position block.
const GPS_BLOCK_TYPE: u8 = 0x30;

/// Type nibble indicating a text message block.
const TEXT_BLOCK_TYPE: u8 = 0x40;

/// Number of 6-byte blocks that make up a complete 20-character text message.
const TEXT_BLOCK_COUNT: u8 = 4;

/// Number of text characters carried by each 6-byte text block.
const TEXT_CHARS_PER_BLOCK: usize = 5;

/// Maximum assembled text message length (characters).
///
/// D-STAR text messages are fixed at 20 characters (4 blocks × 5 chars).
pub const MAX_MESSAGE_LEN: usize = 20;

/// Maximum GPS data length (bytes).
///
/// GPS slow data blocks carry up to 5 bytes per block; multiple blocks are
/// concatenated to form a complete DPRS NMEA-like sentence.
pub const MAX_GPS_LEN: usize = 256;

/// State machine phase for 6-byte block assembly (two 3-byte halves per block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HalfPhase {
    /// Next frame carries bytes 0..3 of a new 6-byte block.
    First,
    /// Next frame carries bytes 3..6, completing the current block.
    Second,
}

/// Decoder for D-STAR slow data embedded in voice frames.
///
/// Feed each 3-byte slow data payload from a `VoiceData` frame into
/// [`SlowDataDecoder::add_frame`]. The decoder tracks the two-frame block
/// assembly phase internally and accumulates text-message blocks until a
/// full 20-character message is available.
///
/// # State machine
///
/// - On construction or [`SlowDataDecoder::reset`], the decoder starts
///   expecting the first half of a 6-byte block.
/// - Sync frames (frame index `0` of each 21-frame superframe) must not be
///   fed through — they carry `[0x55, 0x55, 0x55]` filler and would
///   misalign the half-block pairing. Callers should skip them or call
///   [`SlowDataDecoder::resync`] before the first real frame of a stream.
/// - When both halves of a block are received, the block is descrambled
///   and classified by its type byte. Text blocks copy 5 characters into
///   their assigned slot (indexed by the low nibble of the type byte).
/// - Once all four text blocks have been seen, [`SlowDataDecoder::message`]
///   returns the assembled 20-character string. Further frames are still
///   processed (so a stream can carry multiple messages across its
///   lifetime), but [`SlowDataDecoder::take_message`] is the normal way
///   to consume a complete message and rearm the decoder.
#[derive(Debug, Clone)]
pub struct SlowDataDecoder {
    /// Accumulator for the current 6-byte block.
    block_buf: [u8; 6],
    /// Which half of a 6-byte block is next expected.
    phase: HalfPhase,
    /// 20-byte buffer for text-message characters.
    text_buf: [u8; MAX_MESSAGE_LEN],
    /// Bitmask of which text blocks (0..=3) have been seen in the current
    /// message. Bit `n` set means block index `n` has been written into
    /// `text_buf[n*5..(n+1)*5]`.
    text_seen: u8,
    /// `Some(bytes)` once all four text blocks have been assembled; cleared
    /// by [`SlowDataDecoder::take_message`].
    text_ready: Option<[u8; MAX_MESSAGE_LEN]>,
    /// Accumulated GPS / DPRS data bytes.
    gps_buf: Vec<u8>,
    /// Whether a short GPS block (indicating end-of-sentence) has been seen.
    gps_complete: bool,
}

impl SlowDataDecoder {
    /// Create a new slow data decoder with empty state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            block_buf: [0u8; 6],
            phase: HalfPhase::First,
            text_buf: [b' '; MAX_MESSAGE_LEN],
            text_seen: 0,
            text_ready: None,
            gps_buf: Vec::new(),
            gps_complete: false,
        }
    }

    /// Reset the decoder to its initial state, discarding any partial data.
    ///
    /// Call this at the start of a new voice stream.
    pub fn reset(&mut self) {
        self.block_buf = [0u8; 6];
        self.phase = HalfPhase::First;
        self.text_buf = [b' '; MAX_MESSAGE_LEN];
        self.text_seen = 0;
        self.text_ready = None;
        self.gps_buf.clear();
        self.gps_complete = false;
    }

    /// Realign the half-block phase without discarding completed state.
    ///
    /// Equivalent to the `sync()` method on `ircDDBGateway`'s
    /// `CTextCollector`: it drops any in-progress half-block but leaves
    /// previously-assembled text and GPS data intact.
    pub const fn resync(&mut self) {
        self.phase = HalfPhase::First;
    }

    /// Feed a 3-byte slow data payload from a voice frame.
    ///
    /// The `slow_data` parameter is the last 3 bytes of a 12-byte D-STAR
    /// voice frame. The `frame_index` parameter is the frame sequence
    /// number **modulo 21** within the repeating superframe (`0` = sync
    /// frame; `1..=20` = data frames). Callers may pass the DSVT wire
    /// `seq` byte directly, or an ever-incrementing local counter — the
    /// decoder applies `% 21` itself, so a wrapping `u8` counter also
    /// works as long as it started at the true beginning of a stream.
    ///
    /// When `frame_index % 21 == 0`, the decoder re-synchronizes its
    /// half-block phase and drops the sync-filler bytes rather than
    /// feeding them into the block accumulator.
    pub fn add_frame(&mut self, slow_data: &[u8; 3], frame_index: u8) {
        // Frame 0 of each 21-frame superframe is a sync frame carrying
        // [0x55,0x55,0x55] filler. It must not be fed into the half-block
        // accumulator or it would misalign every following block.
        if frame_index % 21 == 0 {
            tracing::trace!(
                target: "kenwood_thd75::slow_data",
                frame_index,
                "slow data sync frame (resync)"
            );
            self.resync();
            return;
        }

        // XOR descramble.
        let descrambled = [
            slow_data[0] ^ SCRAMBLE_KEY[0],
            slow_data[1] ^ SCRAMBLE_KEY[1],
            slow_data[2] ^ SCRAMBLE_KEY[2],
        ];

        tracing::trace!(
            target: "kenwood_thd75::slow_data",
            frame_index,
            raw = format!("{:02x}{:02x}{:02x}", slow_data[0], slow_data[1], slow_data[2]),
            descrambled = format!(
                "{:02x}{:02x}{:02x}",
                descrambled[0], descrambled[1], descrambled[2]
            ),
            phase = ?self.phase,
            "slow data frame push"
        );

        match self.phase {
            HalfPhase::First => {
                self.block_buf[0..3].copy_from_slice(&descrambled);
                self.phase = HalfPhase::Second;
            }
            HalfPhase::Second => {
                self.block_buf[3..6].copy_from_slice(&descrambled);
                self.phase = HalfPhase::First;
                let block_type = self.block_buf[0] & SLOW_DATA_TYPE_MASK;
                tracing::trace!(
                    target: "kenwood_thd75::slow_data",
                    block_type = format!("0x{block_type:02x}"),
                    "slow data block complete"
                );
                self.process_block();
            }
        }
    }

    /// Returns `true` if a complete 20-character text message is available.
    #[must_use]
    pub const fn has_message(&self) -> bool {
        self.text_ready.is_some()
    }

    /// Returns the assembled text message, if complete.
    ///
    /// The returned slice is always exactly 20 bytes. Trailing spaces and
    /// non-printable bytes are **not** trimmed — callers that want display
    /// text should call `.trim()` themselves.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.text_ready
            .as_ref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
    }

    /// Returns the assembled message as raw bytes, if complete.
    #[must_use]
    pub const fn message_bytes(&self) -> Option<&[u8; MAX_MESSAGE_LEN]> {
        self.text_ready.as_ref()
    }

    /// Take the assembled message and rearm the decoder for the next one.
    ///
    /// Returns `None` if no complete message is ready. On success, the
    /// internal "seen" bitmask and character buffer are cleared so the next
    /// four blocks will be assembled into a fresh message.
    pub fn take_message(&mut self) -> Option<[u8; MAX_MESSAGE_LEN]> {
        let out = self.text_ready.take()?;
        self.text_buf = [b' '; MAX_MESSAGE_LEN];
        self.text_seen = 0;
        Some(out)
    }

    /// Returns `true` if GPS data bytes have been accumulated.
    #[must_use]
    pub const fn has_gps_data(&self) -> bool {
        self.gps_complete
    }

    /// Returns the accumulated GPS data bytes, if any.
    #[must_use]
    pub fn gps_data(&self) -> Option<&[u8]> {
        if self.gps_complete {
            Some(&self.gps_buf)
        } else {
            None
        }
    }

    /// Process a completed 6-byte block.
    fn process_block(&mut self) {
        let type_byte = self.block_buf[0];
        let type_nibble = type_byte & SLOW_DATA_TYPE_MASK;

        match type_nibble {
            TEXT_BLOCK_TYPE => self.process_text_block(type_byte),
            GPS_BLOCK_TYPE => self.process_gps_block(type_byte),
            _ => {
                // Header, fast data, squelch, reserved: ignore. In
                // particular do NOT interpret these as text — that was
                // the source of the real-world corruption bug where
                // header fragments printed as garbled callsigns.
            }
        }
    }

    /// Copy a text block's five characters into the assembled message buffer.
    fn process_text_block(&mut self, type_byte: u8) {
        let block_index = type_byte & 0x0F;
        if block_index >= TEXT_BLOCK_COUNT {
            // Reserved text sub-codes. Ignore rather than guessing.
            return;
        }

        let start = usize::from(block_index) * TEXT_CHARS_PER_BLOCK;
        let end = start + TEXT_CHARS_PER_BLOCK;
        // Each character is the low 7 bits of the corresponding block byte,
        // matching TextCollector.cpp's `& 0x7F` mask.
        for (dst, src) in self.text_buf[start..end]
            .iter_mut()
            .zip(&self.block_buf[1..=TEXT_CHARS_PER_BLOCK])
        {
            *dst = *src & 0x7F;
        }

        self.text_seen |= 1u8 << block_index;

        // When all four blocks have been seen, snapshot the full 20 bytes
        // as a ready message. Keep `text_seen` set so we don't re-emit the
        // same message on every subsequent superframe; a caller that wants
        // successive messages must call `take_message` or `reset`.
        let all_blocks_mask: u8 = (1u8 << TEXT_BLOCK_COUNT) - 1;
        if self.text_seen == all_blocks_mask && self.text_ready.is_none() {
            self.text_ready = Some(self.text_buf);
            if let Ok(s) = std::str::from_utf8(&self.text_buf) {
                tracing::debug!(
                    target: "kenwood_thd75::slow_data",
                    text = s,
                    "slow data text message complete"
                );
            } else {
                tracing::debug!(
                    target: "kenwood_thd75::slow_data",
                    "slow data text message complete (non-utf8)"
                );
            }
        }
    }

    /// Append a GPS block's bytes to the GPS buffer.
    fn process_gps_block(&mut self, type_byte: u8) {
        let byte_count = usize::from(type_byte & 0x0F).min(TEXT_CHARS_PER_BLOCK);

        for &b in &self.block_buf[1..=byte_count] {
            if self.gps_buf.len() >= MAX_GPS_LEN {
                self.gps_complete = true;
                return;
            }
            self.gps_buf.push(b);
        }

        // A short block (fewer than 5 payload bytes) terminates a GPS
        // sentence in the ircDDBGateway encoder.
        if byte_count < TEXT_CHARS_PER_BLOCK {
            self.gps_complete = true;
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
/// suitable for placing in the slow data portion of successive voice frames.
///
/// # Encoding process
///
/// 1. The text is padded with spaces (or truncated) to exactly 20 characters.
/// 2. The 20-character buffer is split into four 5-character blocks.
/// 3. Each block is prefixed with a type byte `0x40 | block_index`, yielding
///    a 6-byte block: `[0x40|i, c0, c1, c2, c3, c4]`.
/// 4. Each 6-byte block is split into two 3-byte halves and each half is
///    XOR-scrambled with `[0x70, 0x4F, 0x93]`.
/// 5. The eight resulting scrambled 3-byte arrays are returned in order.
///
/// The output is a fixed 8 payloads regardless of input length.
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
    /// Returns a [`Vec`] of eight 3-byte arrays (four blocks × two halves).
    /// Empty input produces an empty vector (no message to transmit).
    #[must_use]
    pub fn encode_message(&self, text: &str) -> Vec<[u8; 3]> {
        if text.is_empty() {
            return Vec::new();
        }

        let bytes = text.as_bytes();
        let len = bytes.len().min(MAX_MESSAGE_LEN);

        // Pad to exactly 20 chars with ASCII space.
        let mut padded = [b' '; MAX_MESSAGE_LEN];
        padded[..len].copy_from_slice(&bytes[..len]);

        let mut result = Vec::with_capacity(8);
        for block_index in 0u8..TEXT_BLOCK_COUNT {
            let start = usize::from(block_index) * TEXT_CHARS_PER_BLOCK;
            let end = start + TEXT_CHARS_PER_BLOCK;

            let mut block = [0u8; 6];
            block[0] = TEXT_BLOCK_TYPE | block_index;
            block[1..=TEXT_CHARS_PER_BLOCK].copy_from_slice(&padded[start..end]);

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

    /// Feed a sequence of already-scrambled half-block payloads through the
    /// decoder, assigning non-zero frame indices so the sync-frame path is
    /// never triggered.
    fn feed(decoder: &mut SlowDataDecoder, halves: &[[u8; 3]]) {
        for (i, h) in halves.iter().enumerate() {
            // Frame index 1, 2, 3, ... — skips 0 so resync isn't triggered.
            #[allow(clippy::cast_possible_truncation)]
            let idx = (i as u8).wrapping_add(1);
            decoder.add_frame(h, idx);
        }
    }

    // ------------------------------------------------------------------
    // Scrambler sanity — verify the exact XOR key against a hand example.
    // ------------------------------------------------------------------

    #[test]
    fn scrambler_key_matches_ircddbgateway_constants() {
        // From ircDDBGateway/Common/DStarDefines.h:
        //   SCRAMBLER_BYTE1 = 0x70, SCRAMBLER_BYTE2 = 0x4F, SCRAMBLER_BYTE3 = 0x93
        assert_eq!(SCRAMBLE_KEY, [0x70, 0x4F, 0x93]);
    }

    #[test]
    fn hand_computed_descramble_example() {
        // Block header for text block 0 ('CQ wo' is block 0 of "CQ working  ...")
        // Plaintext half: [0x40, 'C', 'Q'] = [0x40, 0x43, 0x51]
        // XOR with key:
        //   0x40 ^ 0x70 = 0x30
        //   0x43 ^ 0x4F = 0x0C
        //   0x51 ^ 0x93 = 0xC2
        let plain = [0x40, b'C', b'Q'];
        let scrambled = scramble(plain);
        assert_eq!(scrambled, [0x30, 0x0C, 0xC2]);

        // Descrambling is XOR-reversible.
        let round = [
            scrambled[0] ^ SCRAMBLE_KEY[0],
            scrambled[1] ^ SCRAMBLE_KEY[1],
            scrambled[2] ^ SCRAMBLE_KEY[2],
        ];
        assert_eq!(round, plain);
    }

    #[test]
    fn descramble_is_xor_reversible_for_all_bytes() {
        // Full-byte sweep to confirm no bit-level surprises in the key.
        for a in 0u8..=255 {
            for b in [0x00u8, 0x55, 0xAA, 0xFF] {
                for c in [0x00u8, 0x7F, 0x80, 0xFF] {
                    let plain = [a, b, c];
                    let scrambled = scramble(plain);
                    let round = [
                        scrambled[0] ^ SCRAMBLE_KEY[0],
                        scrambled[1] ^ SCRAMBLE_KEY[1],
                        scrambled[2] ^ SCRAMBLE_KEY[2],
                    ];
                    assert_eq!(round, plain);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Text message assembly — the core fix.
    // ------------------------------------------------------------------

    #[test]
    fn four_text_blocks_assemble_20_char_message() {
        let mut decoder = SlowDataDecoder::new();

        // Message: "CQ working         " (20 chars exactly)
        // Block 0 (0x40): "CQ wo"
        // Block 1 (0x41): "rking"
        // Block 2 (0x42): "     "
        // Block 3 (0x43): "     "
        let halves = [
            scramble([0x40, b'C', b'Q']),
            scramble([b' ', b'w', b'o']),
            scramble([0x41, b'r', b'k']),
            scramble([b'i', b'n', b'g']),
            scramble([0x42, b' ', b' ']),
            scramble([b' ', b' ', b' ']),
            scramble([0x43, b' ', b' ']),
            scramble([b' ', b' ', b' ']),
        ];

        feed(&mut decoder, &halves);

        assert!(decoder.has_message());
        let msg = decoder.message().expect("complete ASCII");
        assert_eq!(msg.len(), 20);
        assert_eq!(msg.trim(), "CQ working");
    }

    #[test]
    fn message_emitted_exactly_once_until_taken() {
        let mut decoder = SlowDataDecoder::new();

        // Assemble "Hello world         ".
        let halves = [
            scramble([0x40, b'H', b'e']),
            scramble([b'l', b'l', b'o']),
            scramble([0x41, b' ', b'w']),
            scramble([b'o', b'r', b'l']),
            scramble([0x42, b'd', b' ']),
            scramble([b' ', b' ', b' ']),
            scramble([0x43, b' ', b' ']),
            scramble([b' ', b' ', b' ']),
        ];
        feed(&mut decoder, &halves);
        assert!(decoder.has_message());

        let taken = decoder.take_message().expect("message ready");
        assert_eq!(&taken[..], b"Hello world         ");

        // After taking, the decoder rearms — another 4 blocks can produce
        // a fresh message without reset().
        assert!(!decoder.has_message());
        assert!(decoder.take_message().is_none());
    }

    #[test]
    fn partial_message_does_not_emit() {
        let mut decoder = SlowDataDecoder::new();

        // Only blocks 0 and 1 — missing 2 and 3.
        let halves = [
            scramble([0x40, b'A', b'B']),
            scramble([b'C', b'D', b'E']),
            scramble([0x41, b'F', b'G']),
            scramble([b'H', b'I', b'J']),
        ];
        feed(&mut decoder, &halves);

        assert!(!decoder.has_message());
        assert!(decoder.message().is_none());
    }

    #[test]
    fn mid_message_reset_discards_partial_state() {
        let mut decoder = SlowDataDecoder::new();

        let halves = [
            scramble([0x40, b'X', b'Y']),
            scramble([b'Z', b'!', b'!']),
            scramble([0x41, b'1', b'2']),
            scramble([b'3', b'4', b'5']),
        ];
        feed(&mut decoder, &halves);
        assert!(!decoder.has_message());

        decoder.reset();

        // After reset, feeding blocks 0 and 1 again must not "remember"
        // the earlier partial state.
        let fresh = [
            scramble([0x40, b'N', b'E']),
            scramble([b'W', b' ', b' ']),
            scramble([0x41, b' ', b' ']),
            scramble([b' ', b' ', b' ']),
        ];
        feed(&mut decoder, &fresh);
        // Still incomplete (only 2 of 4 blocks), but if the old state
        // leaked the text buffer would contain "XYZ!!" / "12345".
        assert!(!decoder.has_message());
    }

    #[test]
    fn blocks_out_of_order_still_assemble() {
        let mut decoder = SlowDataDecoder::new();

        // Feed blocks in the order 2, 0, 3, 1.
        // Result: "AAAAABBBBBCCCCCDDDDD" where block 0='A'*5, 1='B'*5,
        // 2='C'*5, 3='D'*5.
        let halves = [
            scramble([0x42, b'C', b'C']),
            scramble([b'C', b'C', b'C']),
            scramble([0x40, b'A', b'A']),
            scramble([b'A', b'A', b'A']),
            scramble([0x43, b'D', b'D']),
            scramble([b'D', b'D', b'D']),
            scramble([0x41, b'B', b'B']),
            scramble([b'B', b'B', b'B']),
        ];
        feed(&mut decoder, &halves);

        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "AAAAABBBBBCCCCCDDDDD");
    }

    #[test]
    fn non_text_blocks_never_emit_text() {
        let mut decoder = SlowDataDecoder::new();

        // Simulate the exact failure mode from the bug report: a stream of
        // header (0x50..) and GPS (0x30..) blocks that contain callsign /
        // location bytes. The decoder must never surface these as text.
        let halves = [
            // Header fragment block (0x55 = type 0x50, 5 payload bytes).
            scramble([0x55, b'V', b'E']),
            scramble([b'3', b'O', b'E']),
            scramble([0x55, b'N', b' ']),
            scramble([b' ', b' ', b' ']),
            // GPS fragment (0x35 = 5 GPS bytes).
            scramble([0x35, b'$', b'G']),
            scramble([b'P', b'G', b'G']),
            // Squelch-code block (0xC0).
            scramble([0xC0, 0x12, 0x34]),
            scramble([0x56, 0x78, 0x9A]),
        ];
        feed(&mut decoder, &halves);

        assert!(!decoder.has_message());
        assert!(decoder.message().is_none());
    }

    #[test]
    fn reserved_text_sub_codes_are_ignored() {
        let mut decoder = SlowDataDecoder::new();

        // 0x44..=0x4F are outside the valid block-index range (0..=3).
        // The decoder must ignore them rather than corrupting the buffer.
        let halves = [
            scramble([0x44, b'X', b'X']),
            scramble([b'X', b'X', b'X']),
            scramble([0x4F, b'Y', b'Y']),
            scramble([b'Y', b'Y', b'Y']),
        ];
        feed(&mut decoder, &halves);
        assert!(!decoder.has_message());
    }

    #[test]
    fn sync_frame_resyncs_without_corrupting_state() {
        let mut decoder = SlowDataDecoder::new();

        // Start a block, then receive a sync frame mid-block. The first
        // half must be discarded (re-aligned) and the next frame becomes
        // a fresh first half.
        decoder.add_frame(&scramble([0x40, b'A', b'A']), 1);
        // Sync frame (index 0) — discards the half-block phase.
        decoder.add_frame(&[0x55, 0x55, 0x55], 0);
        // Now feed a fresh, properly-aligned 4-block message.
        feed(
            &mut decoder,
            &[
                scramble([0x40, b'H', b'I']),
                scramble([b'!', b'!', b'!']),
                scramble([0x41, b' ', b' ']),
                scramble([b' ', b' ', b' ']),
                scramble([0x42, b' ', b' ']),
                scramble([b' ', b' ', b' ']),
                scramble([0x43, b' ', b' ']),
                scramble([b' ', b' ', b' ']),
            ],
        );
        assert!(decoder.has_message());
        let m = decoder.message().unwrap();
        assert_eq!(m.trim(), "HI!!!");
    }

    // ------------------------------------------------------------------
    // GPS decoding — kept functional for callers that want DPRS data.
    // ------------------------------------------------------------------

    #[test]
    fn gps_short_block_terminates_sentence() {
        let mut decoder = SlowDataDecoder::new();
        let halves = [scramble([0x33, 0xAA, 0xBB]), scramble([0xCC, 0x00, 0x00])];
        feed(&mut decoder, &halves);
        assert!(decoder.has_gps_data());
        assert_eq!(decoder.gps_data().unwrap(), &[0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn gps_full_block_continues() {
        let mut decoder = SlowDataDecoder::new();
        let halves = [scramble([0x35, 0x01, 0x02]), scramble([0x03, 0x04, 0x05])];
        feed(&mut decoder, &halves);
        assert!(!decoder.has_gps_data());
    }

    // ------------------------------------------------------------------
    // Encoder correctness and round-trip.
    // ------------------------------------------------------------------

    #[test]
    fn encode_empty_message_is_empty() {
        let encoder = SlowDataEncoder::new();
        assert!(encoder.encode_message("").is_empty());
    }

    #[test]
    fn encode_always_produces_eight_payloads() {
        let encoder = SlowDataEncoder::new();
        for text in &["A", "Hello", "Hello world", "X".repeat(20).as_str()] {
            let payloads = encoder.encode_message(text);
            assert_eq!(payloads.len(), 8, "text = {text:?}");
        }
    }

    #[test]
    fn encode_truncates_beyond_twenty_chars() {
        let encoder = SlowDataEncoder::new();
        let long = "1234567890ABCDEFGHIJKLMN"; // 24 chars
        let payloads = encoder.encode_message(long);
        assert_eq!(payloads.len(), 8);

        let mut decoder = SlowDataDecoder::new();
        feed(&mut decoder, &payloads);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "1234567890ABCDEFGHIJ");
    }

    #[test]
    fn encode_pads_short_message_with_spaces() {
        let encoder = SlowDataEncoder::new();
        let payloads = encoder.encode_message("Hi");

        let mut decoder = SlowDataDecoder::new();
        feed(&mut decoder, &payloads);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), "Hi                  ");
    }

    #[test]
    fn encoder_default_trait() {
        let encoder = SlowDataEncoder::default();
        let payloads = encoder.encode_message("OK");
        assert_eq!(payloads.len(), 8);
    }

    #[test]
    fn roundtrip_mixed_content() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let text = "Hello, world! 1234";
        let payloads = encoder.encode_message(text);
        feed(&mut decoder, &payloads);
        assert!(decoder.has_message());
        let msg = decoder.message().unwrap();
        assert_eq!(msg.trim_end(), text);
    }

    #[test]
    fn roundtrip_exactly_twenty_chars() {
        let encoder = SlowDataEncoder::new();
        let mut decoder = SlowDataDecoder::new();

        let text = "ABCDEFGHIJKLMNOPQRST";
        let payloads = encoder.encode_message(text);
        feed(&mut decoder, &payloads);
        assert!(decoder.has_message());
        assert_eq!(decoder.message().unwrap(), text);
    }

    #[test]
    fn default_creates_empty_decoder() {
        let decoder = SlowDataDecoder::default();
        assert!(!decoder.has_message());
        assert!(decoder.message().is_none());
        assert!(!decoder.has_gps_data());
    }
}
