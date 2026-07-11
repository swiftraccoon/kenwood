// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Minimal WAV writer: 8 kHz, mono, 16-bit PCM, 44-byte RIFF header.

/// Sample rate of mbelib-rs decoder output.
const SAMPLE_RATE: u32 = 8000;

/// Serialize PCM samples into a complete WAV file byte vector.
///
/// Standard 44-byte RIFF/WAVE header (PCM format tag 1, mono,
/// 8 kHz, 16-bit little-endian) followed by the raw samples. Sizes
/// saturate at `u32::MAX` — unreachable in practice, since stream
/// length is bounded by the session inactivity timeout.
#[must_use]
pub fn wav_bytes(pcm: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(pcm.len().saturating_mul(2)).unwrap_or(u32::MAX);
    let riff_len = data_len.saturating_add(36);
    let mut out = Vec::with_capacity(44 + pcm.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn le16(bytes: &[u8], at: usize) -> Result<u16, Box<dyn std::error::Error>> {
        Ok(u16::from_le_bytes(
            bytes.get(at..at + 2).ok_or("short u16")?.try_into()?,
        ))
    }

    fn le32(bytes: &[u8], at: usize) -> Result<u32, Box<dyn std::error::Error>> {
        Ok(u32::from_le_bytes(
            bytes.get(at..at + 4).ok_or("short u32")?.try_into()?,
        ))
    }

    #[test]
    fn header_is_canonical_for_known_length() -> TestResult {
        let bytes = wav_bytes(&[0i16; 160]);
        assert_eq!(bytes.len(), 44 + 320);
        assert_eq!(bytes.get(..4).ok_or("riff")?, b"RIFF");
        assert_eq!(bytes.get(8..12).ok_or("wave")?, b"WAVE");
        assert_eq!(bytes.get(12..16).ok_or("fmt")?, b"fmt ");
        assert_eq!(bytes.get(36..40).ok_or("data")?, b"data");
        // riff size = 36 + data
        assert_eq!(le32(&bytes, 4)?, 36 + 320);
        // PCM format tag 1, mono, 8000 Hz, byte rate 16000, block align 2, 16 bits
        assert_eq!(le16(&bytes, 20)?, 1);
        assert_eq!(le16(&bytes, 22)?, 1);
        assert_eq!(le32(&bytes, 24)?, 8000);
        assert_eq!(le32(&bytes, 28)?, 16_000);
        assert_eq!(le16(&bytes, 32)?, 2);
        assert_eq!(le16(&bytes, 34)?, 16);
        assert_eq!(le32(&bytes, 40)?, 320);
        Ok(())
    }

    #[test]
    fn samples_are_little_endian_in_order() -> TestResult {
        let bytes = wav_bytes(&[1i16, -2]);
        assert_eq!(bytes.get(44..).ok_or("samples")?, &[0x01, 0x00, 0xFE, 0xFF]);
        Ok(())
    }
}
