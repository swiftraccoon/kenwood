//! D-STAR slow-data text-message encoder.
//!
//! Encodes a text message into eight scrambled 3-byte fragments (four
//! blocks × two halves each) suitable for embedding in voice-frame
//! slow-data fields.
//!
//! Reference: `ircDDBGateway/Common/SlowDataEncoder.cpp`.

use super::scrambler::scramble;

/// Number of text blocks in a complete message.
const TEXT_BLOCK_COUNT: u8 = 4;

/// Characters per text block.
const TEXT_CHARS_PER_BLOCK: usize = 5;

/// Upper nibble of a text-block type byte.
const TEXT_BLOCK_TYPE: u8 = 0x40;

/// Fixed message length in characters (4 blocks × 5 chars).
const MAX_MESSAGE_LEN: usize = TEXT_BLOCK_COUNT as usize * TEXT_CHARS_PER_BLOCK;

/// Encode a text message into eight scrambled 3-byte slow-data payloads.
///
/// The output is always exactly 8 payloads (4 blocks × 2 halves) for
/// any non-empty input. Empty input returns an empty vector.
///
/// Messages longer than 20 characters are truncated; shorter messages
/// are right-padded with ASCII spaces.
#[must_use]
pub fn encode_text_message(text: &str) -> Vec<[u8; 3]> {
    if text.is_empty() {
        return Vec::new();
    }

    let bytes = text.as_bytes();
    let len = bytes.len().min(MAX_MESSAGE_LEN);

    let mut padded = [b' '; MAX_MESSAGE_LEN];
    if let (Some(dst), Some(src)) = (padded.get_mut(..len), bytes.get(..len)) {
        dst.copy_from_slice(src);
    }

    let mut out = Vec::with_capacity(8);
    for block_index in 0u8..TEXT_BLOCK_COUNT {
        let start = usize::from(block_index) * TEXT_CHARS_PER_BLOCK;
        let end = start + TEXT_CHARS_PER_BLOCK;

        let mut block = [0u8; 6];
        block[0] = TEXT_BLOCK_TYPE | block_index;
        let Some(chars) = padded.get(start..end) else {
            continue;
        };
        let Some(block_chars) = block.get_mut(1..=TEXT_CHARS_PER_BLOCK) else {
            continue;
        };
        block_chars.copy_from_slice(chars);

        let half1 = scramble([block[0], block[1], block[2]]);
        let half2 = scramble([block[3], block[4], block[5]]);
        out.push(half1);
        out.push(half2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::SlowDataTextCollector;
    use super::super::scrambler::descramble;
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn empty_input_returns_empty_vec() {
        assert!(encode_text_message("").is_empty());
    }

    #[test]
    fn short_input_pads_with_spaces() -> TestResult {
        let out = encode_text_message("Hi");
        assert_eq!(out.len(), 8);

        let mut c = SlowDataTextCollector::new();
        for (i, h) in out.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "`i` comes from `.enumerate()` over an 8-element fixture \
                          (`out.len() == 8` asserted above), so `i as u8` is always \
                          lossless. `wrapping_add(1)` then produces frame indices 1..=8."
            )]
            let idx = (i as u8).wrapping_add(1);
            c.push(*h, idx);
        }
        let msg = c.take_message().ok_or("complete")?;
        assert_eq!(&msg[..], b"Hi                  ");
        Ok(())
    }

    #[test]
    fn exactly_20_chars_roundtrip() -> TestResult {
        let out = encode_text_message("ABCDEFGHIJKLMNOPQRST");
        let mut c = SlowDataTextCollector::new();
        for (i, h) in out.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "`i` comes from `.enumerate()` over `encode_text_message`'s output \
                          which is bounded to at most 8 halves (D-STAR slow-data carries \
                          20 chars in 4 packets × 2 halves), so `i as u8` is always \
                          lossless."
            )]
            let idx = (i as u8).wrapping_add(1);
            c.push(*h, idx);
        }
        let msg = c.take_message().ok_or("complete")?;
        assert_eq!(&msg[..], b"ABCDEFGHIJKLMNOPQRST");
        Ok(())
    }

    #[test]
    fn long_input_truncates_to_20() -> TestResult {
        let out = encode_text_message("1234567890ABCDEFGHIJKLMN");
        let mut c = SlowDataTextCollector::new();
        for (i, h) in out.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "`i` comes from `.enumerate()` over `encode_text_message`'s output \
                          which is bounded to at most 8 halves (D-STAR slow-data carries \
                          20 chars in 4 packets × 2 halves), so `i as u8` is always \
                          lossless."
            )]
            let idx = (i as u8).wrapping_add(1);
            c.push(*h, idx);
        }
        let msg = c.take_message().ok_or("complete")?;
        assert_eq!(&msg[..], b"1234567890ABCDEFGHIJ");
        Ok(())
    }

    #[test]
    fn output_is_always_eight_payloads() {
        for text in &["A", "Hello", "Hello world", "X".repeat(20).as_str()] {
            let out = encode_text_message(text);
            assert_eq!(out.len(), 8, "text = {text:?}");
        }
    }

    #[test]
    fn descramble_reveals_block_index_and_text_chars() -> TestResult {
        let out = encode_text_message("ABCDEFGHIJKLMNOPQRST");
        for block in 0u8..4 {
            let half1 = *out
                .get(usize::from(block) * 2)
                .ok_or("block half1 present")?;
            let plain = descramble(half1);
            assert_eq!(plain[0] & 0xF0, 0x40, "block {block} high nibble");
            assert_eq!(plain[0] & 0x0F, block, "block {block} low nibble = index");
        }
        Ok(())
    }
}
