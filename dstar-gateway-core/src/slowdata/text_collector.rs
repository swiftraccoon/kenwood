//! D-STAR slow-data text-message collector.
//!
//! Assembles four 5-character text blocks into a complete 20-character
//! message. Unlike [`SlowDataAssembler`], which treats the low nibble
//! of the type byte as a variable payload length, this collector uses
//! the fixed-block-index protocol defined in
//! `ircDDBGateway/Common/TextCollector.cpp`:
//!
//! - Type byte high nibble `0x4` identifies a text block.
//! - Type byte low nibble (`0x0..=0x3`) is the block index.
//! - Each block carries exactly 5 text characters in byte positions 1..=5.
//! - Four blocks compose a 20-character message (4 × 5 = 20).
//!
//! Each 6-byte block is transmitted as two consecutive 3-byte slow-data
//! halves in voice-frame slow-data fields. Sync frames (frame index 0)
//! carry `[0x55, 0x55, 0x55]` filler and must not be fed into the
//! collector — they break half-block alignment. Callers either skip
//! them or pass `frame_index == 0` to trigger automatic resync.
//!
//! [`SlowDataAssembler`]: super::SlowDataAssembler

use super::scrambler::descramble;

/// Fixed block count in a complete text message.
const TEXT_BLOCK_COUNT: u8 = 4;

/// Fixed text-char count per block.
const TEXT_CHARS_PER_BLOCK: usize = 5;

/// Assembled text message length (4 blocks × 5 chars).
pub const MAX_MESSAGE_LEN: usize = 20;

/// Upper nibble of a text-block type byte.
const TEXT_BLOCK_TYPE: u8 = 0x40;

/// Upper-nibble mask.
const SLOW_DATA_TYPE_MASK: u8 = 0xF0;

/// Phase of the 6-byte block assembly (two 3-byte halves per block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HalfPhase {
    /// Next frame is bytes 0..3 of a new block.
    First,
    /// Next frame is bytes 3..6 completing the current block.
    Second,
}

/// D-STAR slow-data text-message collector.
///
/// See the module docs for wire-format details. Feed each voice-frame
/// slow-data payload via [`Self::push`]. When all four indexed blocks
/// have been seen, [`Self::take_message`] returns the 20-character
/// message; the collector then rearms for the next message.
#[derive(Debug, Clone)]
pub struct SlowDataTextCollector {
    /// Current 6-byte block being assembled (half1 in [0..3], half2 in [3..6]).
    block_buffer: [u8; 6],
    /// Which half of the block we expect next.
    phase: HalfPhase,
    /// Four 5-char text slots, one per block index (0..=3).
    slots: [[u8; TEXT_CHARS_PER_BLOCK]; 4],
    /// Bit i set ⇒ slot i has been filled.
    seen_mask: u8,
    /// Last frame index observed (for sync-frame detection in `push`).
    last_frame_index: u8,
}

impl Default for SlowDataTextCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SlowDataTextCollector {
    /// Create a new, empty collector.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            block_buffer: [0u8; 6],
            phase: HalfPhase::First,
            slots: [[b' '; TEXT_CHARS_PER_BLOCK]; 4],
            seen_mask: 0,
            last_frame_index: 1,
        }
    }

    /// Feed one voice frame's slow-data payload.
    ///
    /// `frame_index == 0` marks a D-STAR superframe sync frame: the
    /// partial half-block state is discarded and the next `push` is
    /// treated as the first half of a fresh block.
    pub fn push(&mut self, fragment: [u8; 3], frame_index: u8) {
        if frame_index == 0 {
            self.phase = HalfPhase::First;
            self.last_frame_index = 0;
            return;
        }
        self.last_frame_index = frame_index;

        let plain = descramble(fragment);
        match self.phase {
            HalfPhase::First => {
                self.block_buffer[0] = plain[0];
                self.block_buffer[1] = plain[1];
                self.block_buffer[2] = plain[2];
                self.phase = HalfPhase::Second;
            }
            HalfPhase::Second => {
                self.block_buffer[3] = plain[0];
                self.block_buffer[4] = plain[1];
                self.block_buffer[5] = plain[2];
                self.phase = HalfPhase::First;
                self.commit_block();
            }
        }
    }

    /// Process a completed 6-byte block.
    fn commit_block(&mut self) {
        let type_byte = self.block_buffer[0];
        if type_byte & SLOW_DATA_TYPE_MASK != TEXT_BLOCK_TYPE {
            return;
        }
        let block_index = type_byte & 0x0F;
        if block_index >= TEXT_BLOCK_COUNT {
            return;
        }
        let slot_idx = usize::from(block_index);
        let Some(slot) = self.slots.get_mut(slot_idx) else {
            return;
        };
        let Some(src) = self.block_buffer.get(1..=TEXT_CHARS_PER_BLOCK) else {
            return;
        };
        for (dst, s) in slot.iter_mut().zip(src.iter()) {
            *dst = *s;
        }
        self.seen_mask |= 1u8 << block_index;
        // Diagnostic: log every accepted text block so non-standard
        // slow-data streams (e.g. AMBEserver custom encodings that
        // happen to mimic 0x40-0x43 sub-codes) can be reverse-
        // engineered by inspecting which raw bytes triggered a commit.
        tracing::trace!(
            target: "dstar_gateway_core::slowdata::text_collector",
            slot = slot_idx,
            seen_mask = format_args!("{:#06b}", self.seen_mask),
            type_byte = format_args!("{:#04x}", type_byte),
            block = format_args!(
                "{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
                self.block_buffer[0], self.block_buffer[1], self.block_buffer[2],
                self.block_buffer[3], self.block_buffer[4], self.block_buffer[5]
            ),
            chars = format_args!("{:?}", String::from_utf8_lossy(src)),
            "text block accepted"
        );
    }

    /// Return the complete 20-char message if all four blocks have been seen.
    #[must_use]
    pub fn message(&self) -> Option<[u8; MAX_MESSAGE_LEN]> {
        if self.seen_mask != 0b1111 {
            return None;
        }
        let mut out = [0u8; MAX_MESSAGE_LEN];
        for (i, slot) in self.slots.iter().enumerate() {
            let start = i * TEXT_CHARS_PER_BLOCK;
            let end = start + TEXT_CHARS_PER_BLOCK;
            let dst = out.get_mut(start..end)?;
            dst.copy_from_slice(slot);
        }
        Some(out)
    }

    /// Consume the complete message and rearm the collector.
    pub fn take_message(&mut self) -> Option<[u8; MAX_MESSAGE_LEN]> {
        let msg = self.message()?;
        self.rearm();
        Some(msg)
    }

    /// Clear all state. Call at stream boundaries (EOT, new header).
    pub const fn reset(&mut self) {
        self.rearm();
        self.phase = HalfPhase::First;
        self.block_buffer = [0u8; 6];
    }

    const fn rearm(&mut self) {
        self.slots = [[b' '; TEXT_CHARS_PER_BLOCK]; 4];
        self.seen_mask = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::scrambler::scramble;
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Feed a sequence of already-descrambled logical halves by scrambling
    /// them first (so the collector sees real wire form) and assigning
    /// non-zero frame indices.
    fn feed(collector: &mut SlowDataTextCollector, halves: &[[u8; 3]]) {
        for (i, h) in halves.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "Test helper. `i` comes from `.enumerate()` over test-constructed \
                          fixtures of at most 8 halves (D-STAR slow-data encodes 20 chars \
                          as 4 packets × 2 halves), so `i as u8` is always lossless."
            )]
            let idx = (i as u8).wrapping_add(1);
            collector.push(scramble(*h), idx);
        }
    }

    #[test]
    fn four_text_blocks_assemble_20_char_message() -> TestResult {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x40, b'C', b'Q'],
                [b' ', b'w', b'o'],
                [0x41, b'r', b'k'],
                [b'i', b'n', b'g'],
                [0x42, b' ', b' '],
                [b' ', b' ', b' '],
                [0x43, b' ', b' '],
                [b' ', b' ', b' '],
            ],
        );
        let msg = c.take_message().ok_or("complete message")?;
        assert_eq!(&msg[..], b"CQ working          ");
        Ok(())
    }

    #[test]
    fn out_of_order_blocks_still_assemble() -> TestResult {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x42, b'C', b'C'],
                [b'C', b'C', b'C'],
                [0x40, b'A', b'A'],
                [b'A', b'A', b'A'],
                [0x43, b'D', b'D'],
                [b'D', b'D', b'D'],
                [0x41, b'B', b'B'],
                [b'B', b'B', b'B'],
            ],
        );
        let msg = c.take_message().ok_or("complete message")?;
        assert_eq!(&msg[..], b"AAAAABBBBBCCCCCDDDDD");
        Ok(())
    }

    #[test]
    fn partial_message_returns_none() {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x40, b'A', b'B'],
                [b'C', b'D', b'E'],
                [0x41, b'F', b'G'],
                [b'H', b'I', b'J'],
            ],
        );
        assert!(c.message().is_none());
        assert!(c.take_message().is_none());
    }

    #[test]
    fn reset_discards_partial_state() {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x40, b'X', b'Y'],
                [b'Z', b'!', b'!'],
                [0x41, b'1', b'2'],
                [b'3', b'4', b'5'],
            ],
        );
        c.reset();
        feed(
            &mut c,
            &[
                [0x40, b'N', b'E'],
                [b'W', b' ', b' '],
                [0x41, b' ', b' '],
                [b' ', b' ', b' '],
            ],
        );
        assert!(c.message().is_none());
    }

    #[test]
    fn sync_frame_resyncs_without_corrupting_state() -> TestResult {
        let mut c = SlowDataTextCollector::new();
        c.push(scramble([0x40, b'A', b'A']), 1);
        c.push([0x55, 0x55, 0x55], 0);
        feed(
            &mut c,
            &[
                [0x40, b'H', b'I'],
                [b'!', b'!', b'!'],
                [0x41, b' ', b' '],
                [b' ', b' ', b' '],
                [0x42, b' ', b' '],
                [b' ', b' ', b' '],
                [0x43, b' ', b' '],
                [b' ', b' ', b' '],
            ],
        );
        let msg = c.take_message().ok_or("message after resync")?;
        assert_eq!(&msg[..], b"HI!!!               ");
        Ok(())
    }

    #[test]
    fn non_text_blocks_are_ignored() {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x55, b'V', b'E'],
                [b'3', b'O', b'E'],
                [0x35, b'$', b'G'],
                [b'P', b'G', b'G'],
                [0xC0, 0x12, 0x34],
                [0x56, 0x78, 0x9A],
            ],
        );
        assert!(c.message().is_none());
    }

    #[test]
    fn reserved_text_sub_codes_are_ignored() {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x44, b'X', b'X'],
                [b'X', b'X', b'X'],
                [0x4F, b'Y', b'Y'],
                [b'Y', b'Y', b'Y'],
            ],
        );
        assert!(c.message().is_none());
    }

    #[test]
    fn take_message_rearms_collector() -> TestResult {
        let mut c = SlowDataTextCollector::new();
        feed(
            &mut c,
            &[
                [0x40, b'H', b'e'],
                [b'l', b'l', b'o'],
                [0x41, b' ', b'w'],
                [b'o', b'r', b'l'],
                [0x42, b'd', b' '],
                [b' ', b' ', b' '],
                [0x43, b' ', b' '],
                [b' ', b' ', b' '],
            ],
        );
        let taken = c.take_message().ok_or("message ready")?;
        assert_eq!(&taken[..], b"Hello world         ");
        assert!(c.message().is_none());
        Ok(())
    }

    #[test]
    fn default_creates_empty_collector() {
        let c = SlowDataTextCollector::default();
        assert!(c.message().is_none());
    }
}
