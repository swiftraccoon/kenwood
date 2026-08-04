//! Stateful slow data block assembler.
//!
//! Accumulates 3-byte slow-data fragments across consecutive voice
//! frames into complete typed blocks. The assembler descrambles each
//! incoming fragment, then decodes the assembled payload into a
//! [`SlowDataBlock`] based on the type byte's high nibble.

use crate::header::{DstarHeader, ENCODED_LEN, crc_ccitt};

use super::block::{SlowDataBlock, SlowDataBlockKind};
use super::scrambler::descramble;
use super::text::SlowDataText;

/// Every typed slow-data element occupies two 3-byte voice-frame
/// fragments: one type byte followed by five payload bytes.
const BLOCK_LEN: usize = 6;
const BLOCK_PAYLOAD_LEN: usize = BLOCK_LEN - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HalfPhase {
    First,
    Second,
}

/// Stateful slow data accumulator.
///
/// Feed 3-byte fragments via [`Self::push`]. Returns `Some(block)`
/// when a complete block has assembled; returns `None` otherwise.
///
/// Header retransmission is the one multi-block value: eight `0x55`
/// elements carry five bytes each and a final `0x51` element carries
/// the 41st byte. The assembler retains those payloads until the
/// complete header and its CRC have arrived.
#[derive(Debug)]
pub struct SlowDataAssembler {
    block: [u8; BLOCK_LEN],
    phase: HalfPhase,
    header: [u8; ENCODED_LEN],
    header_cursor: usize,
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
            block: [0_u8; BLOCK_LEN],
            phase: HalfPhase::First,
            header: [0_u8; ENCODED_LEN],
            header_cursor: 0,
        }
    }

    /// Feed a single voice frame's 3-byte slow data into the assembler.
    ///
    /// Returns `Some(block)` when a complete block has assembled,
    /// `None` otherwise. `frame_index == 0` is the D-STAR superframe
    /// sync slot; it resets half-block alignment and any incomplete
    /// retransmitted header without interpreting the sync bytes as data.
    pub fn push(&mut self, fragment: [u8; 3], frame_index: u8) -> Option<SlowDataBlock> {
        if frame_index == 0 {
            self.reset();
            return None;
        }

        let plain = descramble(fragment);
        match self.phase {
            HalfPhase::First => {
                self.block[..3].copy_from_slice(&plain);
                self.phase = HalfPhase::Second;
                None
            }
            HalfPhase::Second => {
                self.block[3..].copy_from_slice(&plain);
                self.phase = HalfPhase::First;
                self.commit_block()
            }
        }
    }

    fn commit_block(&mut self) -> Option<SlowDataBlock> {
        let type_byte = self.block[0];
        let kind = SlowDataBlockKind::from_type_byte(type_byte);
        if kind == SlowDataBlockKind::HeaderRetx {
            return self.commit_header_block(type_byte);
        }
        self.header_cursor = 0;

        let payload_len = if kind == SlowDataBlockKind::Text {
            // Text's low nibble is a block index, not a length.
            BLOCK_PAYLOAD_LEN
        } else {
            usize::from(type_byte & 0x0F).min(BLOCK_PAYLOAD_LEN)
        };
        Some(self.decode_single_block(type_byte, payload_len))
    }

    fn commit_header_block(&mut self, type_byte: u8) -> Option<SlowDataBlock> {
        let payload_len = usize::from(type_byte & 0x0F);
        let position_is_valid = payload_len == BLOCK_PAYLOAD_LEN
            && self.header_cursor <= ENCODED_LEN - BLOCK_PAYLOAD_LEN
            || payload_len == 1 && self.header_cursor == ENCODED_LEN - 1;
        if !position_is_valid {
            self.header_cursor = 0;
            let payload_end = 1 + payload_len.min(BLOCK_PAYLOAD_LEN);
            let payload = self.block.get(1..payload_end).unwrap_or(&[]).to_vec();
            return Some(SlowDataBlock::Unknown { type_byte, payload });
        }

        let end = self.header_cursor + payload_len;
        let payload_end = 1 + payload_len;
        let source = self.block.get(1..payload_end)?;
        let destination = self.header.get_mut(self.header_cursor..end)?;
        destination.copy_from_slice(source);
        self.header_cursor = end;
        if self.header_cursor != ENCODED_LEN {
            return None;
        }

        self.header_cursor = 0;
        let checksum_bytes: [u8; 2] = self.header.get(39..)?.try_into().ok()?;
        let stored_crc = u16::from_le_bytes(checksum_bytes);
        if crc_ccitt(self.header.get(..39).unwrap_or(&[])) != stored_crc {
            return Some(SlowDataBlock::Unknown {
                type_byte,
                payload: self.header.to_vec(),
            });
        }
        Some(SlowDataBlock::HeaderRetx(DstarHeader::decode(&self.header)))
    }

    fn decode_single_block(&self, type_byte: u8, payload_len: usize) -> SlowDataBlock {
        let kind = SlowDataBlockKind::from_type_byte(type_byte);
        let payload_end = 1 + payload_len;
        let payload = self.block.get(1..payload_end).unwrap_or(&[]);

        match kind {
            SlowDataBlockKind::Gps => SlowDataBlock::Gps(payload.to_vec()),
            SlowDataBlockKind::Text => {
                SlowDataBlock::Text(SlowDataText::from_wire_bytes(payload.to_vec()))
            }
            SlowDataBlockKind::HeaderRetx => unreachable!("header blocks use commit_header_block"),
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
        self.phase = HalfPhase::First;
        self.header_cursor = 0;
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
        asm.push(scramble(bytes), 1)
    }

    #[test]
    fn text_block_waits_for_both_halves() -> TestResult {
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x40, b'H', b'E']).is_none());
        let block = push_descrambled(&mut asm, [b'L', b'L', b'O'])
            .ok_or("expected text block after second half")?;
        assert!(
            matches!(&block, SlowDataBlock::Text(t) if t.as_bytes() == b"HELLO"),
            "expected Text(HELLO), got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn text_block_assembles_across_two_frames() -> TestResult {
        // Text block: low nibble is the block index, not a length.
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x43, b'H', b'E']).is_none());
        // Frame 2: ['L', 'L', 'O'] = remaining 3 payload bytes
        let block = push_descrambled(&mut asm, [b'L', b'L', b'O'])
            .ok_or("expected block after second frame")?;
        assert!(
            matches!(&block, SlowDataBlock::Text(t) if t.as_bytes() == b"HELLO" && t.text() == Ok("HELLO")),
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
        // GPS doesn't trim: it includes the exact 4 payload bytes.
        assert!(
            matches!(&block, SlowDataBlock::Gps(bytes) if bytes == b"TEST"),
            "expected exact GPS bytes, got {block:?}"
        );
        Ok(())
    }

    #[test]
    fn gps_block_preserves_non_utf8_wire_bytes() -> TestResult {
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x35, b'$', 0xFF]).is_none());
        let block = push_descrambled(&mut asm, [0x80, b'X', b'Y'])
            .ok_or("expected block after second frame")?;
        assert_eq!(
            block,
            SlowDataBlock::Gps(vec![b'$', 0xFF, 0x80, b'X', b'Y'])
        );
        Ok(())
    }

    #[test]
    fn squelch_block_captures_code() -> TestResult {
        // Squelch block: byte 0 = 0xC1 (squelch, length 1), byte 1 = 0x42
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0xC1, 0x42, 0x00]).is_none());
        let block = push_descrambled(&mut asm, [0x00; 3]).ok_or("expected squelch block")?;
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
        assert!(push_descrambled(&mut asm, [0xA2, 0x11, 0x22]).is_none());
        let block = push_descrambled(&mut asm, [0x00; 3]).ok_or("expected unknown block")?;
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

    fn test_header() -> DstarHeader {
        use crate::types::{Callsign, Suffix};
        DstarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    fn push_block(asm: &mut SlowDataAssembler, block: [u8; BLOCK_LEN]) -> Option<SlowDataBlock> {
        let [first, second, third, fourth, fifth, sixth] = block;
        assert!(push_descrambled(asm, [first, second, third]).is_none());
        push_descrambled(asm, [fourth, fifth, sixth])
    }

    #[test]
    fn header_retransmission_assembles_all_nine_blocks() -> TestResult {
        let header = test_header();
        let encoded = header.encode();
        let mut asm = SlowDataAssembler::new();
        for chunk in encoded
            .get(..40)
            .ok_or("header prefix missing")?
            .chunks_exact(BLOCK_PAYLOAD_LEN)
        {
            let [first, second, third, fourth, fifth] = <[u8; BLOCK_PAYLOAD_LEN]>::try_from(chunk)?;
            let block = [0x55, first, second, third, fourth, fifth];
            assert!(push_block(&mut asm, block).is_none());
        }
        let final_byte = *encoded.get(40).ok_or("final header byte missing")?;
        let block = push_block(&mut asm, [0x51, final_byte, 0x66, 0x66, 0x66, 0x66])
            .ok_or("final header block did not complete")?;
        assert_eq!(block, SlowDataBlock::HeaderRetx(header));
        Ok(())
    }

    #[test]
    fn header_retransmission_with_bad_crc_is_not_decoded() -> TestResult {
        let mut encoded = test_header().encode();
        let final_byte = encoded.get_mut(40).ok_or("final header byte missing")?;
        *final_byte ^= 0x80;
        let mut asm = SlowDataAssembler::new();
        for chunk in encoded
            .get(..40)
            .ok_or("header prefix missing")?
            .chunks_exact(BLOCK_PAYLOAD_LEN)
        {
            let [first, second, third, fourth, fifth] = <[u8; BLOCK_PAYLOAD_LEN]>::try_from(chunk)?;
            let block = [0x55, first, second, third, fourth, fifth];
            assert!(push_block(&mut asm, block).is_none());
        }
        let final_byte = *encoded.get(40).ok_or("final header byte missing")?;
        let block = push_block(&mut asm, [0x51, final_byte, 0, 0, 0, 0])
            .ok_or("bad header did not produce an observable block")?;
        assert!(matches!(block, SlowDataBlock::Unknown { payload, .. } if payload == encoded));
        Ok(())
    }

    #[test]
    fn sync_fragment_resets_half_block_alignment() -> TestResult {
        let mut asm = SlowDataAssembler::new();
        assert!(push_descrambled(&mut asm, [0x40, b'B', b'A']).is_none());
        assert!(asm.push([0x55, 0x2D, 0x16], 0).is_none());
        assert!(push_descrambled(&mut asm, [0x40, b'H', b'E']).is_none());
        let block = push_descrambled(&mut asm, [b'L', b'L', b'O'])
            .ok_or("text after sync did not complete")?;
        assert!(matches!(block, SlowDataBlock::Text(text) if text.as_bytes() == b"HELLO"));
        Ok(())
    }
}
