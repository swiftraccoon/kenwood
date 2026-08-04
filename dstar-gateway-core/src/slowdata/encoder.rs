//! D-STAR slow-data text-message encoder.
//!
//! Encodes a text message into eight scrambled 3-byte fragments (four
//! blocks × two halves each) suitable for embedding in voice-frame
//! slow-data fields.
//!
//! Reference: `ircDDBGateway/Common/SlowDataEncoder.cpp`.

use super::{SlowDataTextMessage, scrambler::scramble};

/// Encode a text message into eight scrambled 3-byte slow-data payloads.
///
/// Construction of [`SlowDataTextMessage`] proves that the input is exactly
/// 20 wire bytes containing printable ASCII plus right-side space padding.
/// The output is therefore always exactly eight payloads (four blocks × two
/// halves), with no encoder-side truncation or replacement.
#[must_use]
pub const fn encode_text_message(message: SlowDataTextMessage) -> [[u8; 3]; 8] {
    let [
        b0,
        b1,
        b2,
        b3,
        b4,
        b5,
        b6,
        b7,
        b8,
        b9,
        b10,
        b11,
        b12,
        b13,
        b14,
        b15,
        b16,
        b17,
        b18,
        b19,
    ] = message.into_bytes();

    [
        scramble([0x40, b0, b1]),
        scramble([b2, b3, b4]),
        scramble([0x41, b5, b6]),
        scramble([b7, b8, b9]),
        scramble([0x42, b10, b11]),
        scramble([b12, b13, b14]),
        scramble([0x43, b15, b16]),
        scramble([b17, b18, b19]),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::SlowDataTextCollector;
    use super::super::scrambler::descramble;
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn empty_text_is_an_explicit_all_space_message() -> TestResult {
        let out = encode_text_message(SlowDataTextMessage::try_from_text("")?);
        let mut collector = SlowDataTextCollector::new();
        for (index, fragment) in (1u8..).zip(out) {
            collector.push(fragment, index);
        }
        let message = collector.take_message().ok_or("complete")?;
        assert_eq!(message.as_bytes(), b"                    ");
        Ok(())
    }

    #[test]
    fn short_input_pads_with_spaces() -> TestResult {
        let out = encode_text_message(SlowDataTextMessage::try_from_text("Hi")?);
        assert_eq!(out.len(), 8);

        let mut c = SlowDataTextCollector::new();
        for (idx, h) in (1u8..).zip(out.iter()) {
            c.push(*h, idx);
        }
        let msg = c.take_message().ok_or("complete")?;
        assert_eq!(msg.as_bytes(), b"Hi                  ");
        Ok(())
    }

    #[test]
    fn exactly_20_chars_roundtrip() -> TestResult {
        let out = encode_text_message(SlowDataTextMessage::try_from_text("ABCDEFGHIJKLMNOPQRST")?);
        let mut c = SlowDataTextCollector::new();
        for (idx, h) in (1u8..).zip(out.iter()) {
            c.push(*h, idx);
        }
        let msg = c.take_message().ok_or("complete")?;
        assert_eq!(msg.as_bytes(), b"ABCDEFGHIJKLMNOPQRST");
        Ok(())
    }

    #[test]
    fn output_is_always_eight_payloads() -> TestResult {
        for text in &["A", "Hello", "Hello world", "X".repeat(20).as_str()] {
            let message = SlowDataTextMessage::try_from_text(text)?;
            let out = encode_text_message(message);
            assert_eq!(out.len(), 8, "text = {text:?}");
        }
        Ok(())
    }

    #[test]
    fn descramble_reveals_block_index_and_text_chars() -> TestResult {
        let out = encode_text_message(SlowDataTextMessage::try_from_text("ABCDEFGHIJKLMNOPQRST")?);
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
