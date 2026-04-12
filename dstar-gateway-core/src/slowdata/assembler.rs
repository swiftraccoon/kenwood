//! Stateful slow data block assembler.
//!
//! Accumulates 3-byte slow-data fragments across consecutive voice
//! frames into complete typed blocks. The assembler descrambles each
//! incoming fragment, then decodes the assembled payload into a
//! [`SlowDataBlock`] based on the type byte's high nibble.

use crate::header::{DStarHeader, ENCODED_LEN};

use super::block::{SlowDataBlock, SlowDataBlockKind, SlowDataText};
use super::scrambler::descramble;

/// Maximum scratch size — slow data blocks are at most ~20 bytes,
/// and we need headroom so that a 3-byte append on a nearly-full
/// scratch buffer can be guarded cleanly.
const SCRATCH_SIZE: usize = 48;

/// Stateful slow data accumulator.
///
/// Feed 3-byte fragments via [`Self::push`]. Returns `Some(block)`
/// when a complete block has assembled; returns `None` otherwise.
///
/// Internally holds at most one in-progress block (`SCRATCH_SIZE`
/// bytes of scratch).
#[derive(Debug)]
pub struct SlowDataAssembler {
    scratch: [u8; SCRATCH_SIZE],
    cursor: usize,
    type_byte: Option<u8>,
    expected_len: Option<usize>,
}

impl Default for SlowDataAssembler {
    fn default() -> Self {
        Self::new()
    }
}

impl SlowDataAssembler {
    /// Create a new, empty assembler.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scratch: [0u8; SCRATCH_SIZE],
            cursor: 0,
            type_byte: None,
            expected_len: None,
        }
    }

    /// Feed a single voice frame's 3-byte slow data into the assembler.
    ///
    /// Returns `Some(block)` when a complete block has assembled,
    /// `None` otherwise. Resets internal state on completion (or
    /// on overflow, silently dropping any partial block).
    pub fn push(&mut self, fragment: [u8; 3]) -> Option<SlowDataBlock> {
        let descrambled = descramble(fragment);

        // Append the 3 bytes to scratch, guarding against overflow.
        for &byte in &descrambled {
            if self.cursor >= SCRATCH_SIZE {
                self.reset();
                return None;
            }
            if let Some(slot) = self.scratch.get_mut(self.cursor) {
                *slot = byte;
            }
            self.cursor += 1;
        }

        // If we now have at least 1 byte, we know the type byte and
        // expected length.
        if self.type_byte.is_none() && self.cursor >= 1 {
            let t = self.scratch.first().copied().unwrap_or(0);
            self.type_byte = Some(t);
            // Low nibble = number of *additional* payload bytes
            // beyond the type byte itself. Reference:
            // `ircDDBGateway/Common/SlowDataEncoder.cpp` — the
            // encoder packs the byte count into the low nibble.
            self.expected_len = Some(usize::from(t & 0x0F));
        }

        // Check for completion.
        let expected = self.expected_len?;
        if self.cursor > expected {
            // We have a complete block (type byte at index 0 plus
            // `expected` payload bytes, so cursor > expected means
            // cursor >= 1 + expected).
            let type_byte = self.type_byte.unwrap_or(0);
            let block = self.decode_block(type_byte, expected);
            self.reset();
            return Some(block);
        }

        None
    }

    fn decode_block(&self, type_byte: u8, payload_len: usize) -> SlowDataBlock {
        let kind = SlowDataBlockKind::from_type_byte(type_byte);
        // Payload starts at index 1 of scratch.
        let payload_end = 1 + payload_len;
        let payload = self.scratch.get(1..payload_end).unwrap_or(&[]);

        match kind {
            SlowDataBlockKind::Gps => {
                let text = String::from_utf8_lossy(payload).to_string();
                SlowDataBlock::Gps(text)
            }
            SlowDataBlockKind::Text => {
                let raw = String::from_utf8_lossy(payload).to_string();
                let trimmed = raw.trim_end_matches([' ', '\0']).to_string();
                SlowDataBlock::Text(SlowDataText { text: trimmed })
            }
            SlowDataBlockKind::HeaderRetx => {
                // A D-STAR header is exactly 41 bytes. If the payload
                // is shorter, fall back to Unknown.
                if payload.len() >= ENCODED_LEN {
                    let mut arr = [0u8; ENCODED_LEN];
                    if let Some(src) = payload.get(..ENCODED_LEN) {
                        arr.copy_from_slice(src);
                    }
                    let header = DStarHeader::decode(&arr);
                    SlowDataBlock::HeaderRetx(header)
                } else {
                    SlowDataBlock::Unknown {
                        type_byte,
                        payload: payload.to_vec(),
                    }
                }
            }
            SlowDataBlockKind::FastData1 | SlowDataBlockKind::FastData2 => {
                SlowDataBlock::FastData(payload.to_vec())
            }
            SlowDataBlockKind::Squelch => {
                let code = payload.first().copied().unwrap_or(0);
                SlowDataBlock::Squelch { code }
            }
            SlowDataBlockKind::Unknown { .. } => SlowDataBlock::Unknown {
                type_byte,
                payload: payload.to_vec(),
            },
        }
    }

    const fn reset(&mut self) {
        self.cursor = 0;
        self.type_byte = None;
        self.expected_len = None;
    }
}

#[cfg(test)]
mod tests {
    use super::super::scrambler::scramble;
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Helper: push a logical (already-descrambled) 3-byte fragment by
    /// scrambling it first, so the assembler sees the "real wire" form.
    fn push_descrambled(asm: &mut SlowDataAssembler, bytes: [u8; 3]) -> Option<SlowDataBlock> {
        asm.push(scramble(bytes))
    }

    #[test]
    fn empty_assembler_returns_zero_length_text_block() {
        let mut asm = SlowDataAssembler::new();
        // Zero-length text block: type 0x40 with length nibble 0.
        // The assembler sees a complete block with zero payload bytes
        // immediately after ingesting the first 3-byte fragment.
        let block = push_descrambled(&mut asm, [0x40, 0x00, 0x00]);
        assert!(block.is_some(), "zero-length text block should complete");
        assert!(
            matches!(&block, Some(SlowDataBlock::Text(t)) if t.text.is_empty()),
            "expected Text with empty string, got {block:?}"
        );
    }

    #[test]
    fn text_block_assembles_across_two_frames() -> TestResult {
        // Text block: byte 0 = 0x45 (text, length 5), payload = "HELLO"
        let mut asm = SlowDataAssembler::new();
        // Frame 1: [0x45, 'H', 'E'] — type byte + 2 payload bytes
        assert!(push_descrambled(&mut asm, [0x45, b'H', b'E']).is_none());
        // Frame 2: ['L', 'L', 'O'] — remaining 3 payload bytes
        let block = push_descrambled(&mut asm, [b'L', b'L', b'O'])
            .ok_or("expected block after second frame")?;
        assert!(
            matches!(&block, SlowDataBlock::Text(t) if t.text == "HELLO"),
            "expected Text(\"HELLO\"), got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn gps_block_assembles() -> TestResult {
        // GPS block: byte 0 = 0x34 (gps, length 4), payload = "TEST"
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x34, b'T', b'E']).is_none());
        let block = push_descrambled(&mut asm, [b'S', b'T', 0x00])
            .ok_or("expected block after second frame")?;
        // GPS doesn't trim — includes the exact 4 payload bytes.
        assert!(
            matches!(&block, SlowDataBlock::Gps(text) if text == "TEST"),
            "expected Gps(\"TEST\"), got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn squelch_block_captures_code() -> TestResult {
        // Squelch block: byte 0 = 0xC1 (squelch, length 1), byte 1 = 0x42
        let mut asm = SlowDataAssembler::new();
        let block =
            push_descrambled(&mut asm, [0xC1, 0x42, 0x00]).ok_or("expected squelch block")?;
        assert!(
            matches!(block, SlowDataBlock::Squelch { code } if code == 0x42),
            "expected Squelch {{ code: 0x42 }}, got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn unknown_kind_preserves_type_byte_and_payload() -> TestResult {
        // Unknown kind: byte 0 = 0xA2, length 2, payload [0x11, 0x22]
        let mut asm = SlowDataAssembler::new();
        let block =
            push_descrambled(&mut asm, [0xA2, 0x11, 0x22]).ok_or("expected unknown block")?;
        assert!(
            matches!(&block, SlowDataBlock::Unknown { type_byte, payload }
                if *type_byte == 0xA2 && *payload == vec![0x11, 0x22]),
            "expected Unknown {{ type_byte: 0xA2, payload: [0x11, 0x22] }}, got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn fast_data_block_two_frames() -> TestResult {
        // FastData1: byte 0 = 0x83, length 3, payload [0xDE, 0xAD, 0xBE]
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x83, 0xDE, 0xAD]).is_none());
        let block =
            push_descrambled(&mut asm, [0xBE, 0x00, 0x00]).ok_or("expected fast data block")?;
        assert!(
            matches!(&block, SlowDataBlock::FastData(payload) if *payload == vec![0xDE, 0xAD, 0xBE]),
            "expected FastData([0xDE, 0xAD, 0xBE]), got {block:?}"
        );
        Ok(())
    }
}
