// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Minimal WAV writer: 8 kHz, mono, 16-bit PCM, 44-byte RIFF header.

/// Sample rate of mbelib-rs decoder output.
const SAMPLE_RATE: u32 = 8000;

/// Complete 8 kHz mono PCM16 WAV bytes with exact RIFF size fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavFile(Vec<u8>);

/// A PCM recording cannot be represented or allocated as a RIFF file.
#[derive(Debug, thiserror::Error)]
pub enum WavError {
    /// The data and 36-byte RIFF payload header exceed the 32-bit RIFF size.
    #[error("{samples} PCM samples exceed the RIFF/WAVE size fields")]
    TooLong {
        /// Supplied PCM sample count.
        samples: usize,
    },
    /// The complete output buffer could not be reserved.
    #[error("WAV allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
}

impl WavFile {
    /// Serialize signed samples without changing their values or order.
    ///
    /// # Errors
    ///
    /// Returns [`WavError::TooLong`] before allocation if RIFF's 32-bit
    /// size or this platform's address space cannot hold the output, and
    /// [`WavError::Allocation`] if reserving that exact output fails.
    pub fn from_pcm(pcm: &[i16]) -> Result<Self, WavError> {
        let WavSizes {
            data_len,
            riff_len,
            total,
        } = sizes(pcm.len())?;
        let mut out = Vec::new();
        out.try_reserve_exact(total).map_err(WavError::Allocation)?;
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
        Ok(Self(out))
    }
    /// Borrow the complete RIFF/WAVE file.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    /// Take the encoded file without copying.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

struct WavSizes {
    data_len: u32,
    riff_len: u32,
    total: usize,
}

fn sizes(samples: usize) -> Result<WavSizes, WavError> {
    let too_long = || WavError::TooLong { samples };
    let data_len = samples
        .checked_mul(2)
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(too_long)?;
    let riff_len = data_len.checked_add(36).ok_or_else(too_long)?;
    let total = usize::try_from(data_len)
        .ok()
        .and_then(|bytes| bytes.checked_add(44))
        .ok_or_else(too_long)?;
    Ok(WavSizes {
        data_len,
        riff_len,
        total,
    })
}
impl TryFrom<&[i16]> for WavFile {
    type Error = WavError;
    fn try_from(value: &[i16]) -> Result<Self, Self::Error> {
        Self::from_pcm(value)
    }
}
impl AsRef<[u8]> for WavFile {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl From<WavFile> for Vec<u8> {
    fn from(value: WavFile) -> Self {
        value.into_bytes()
    }
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
        let bytes = WavFile::from_pcm(&[0i16; 160])?.into_bytes();
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
        let bytes = WavFile::from_pcm(&[1i16, -2])?.into_bytes();
        assert_eq!(bytes.get(44..).ok_or("samples")?, &[0x01, 0x00, 0xFE, 0xFF]);
        Ok(())
    }

    #[test]
    fn riff_size_boundaries_reject_instead_of_saturating() -> TestResult {
        let maximum = usize::try_from((u32::MAX - 36) / 2)?;
        let WavSizes {
            data_len: data,
            riff_len: riff,
            total,
        } = sizes(maximum)?;
        assert_eq!(data, 4_294_967_258);
        assert_eq!(riff, 4_294_967_294);
        assert_eq!(total, maximum * 2 + 44);
        assert!(matches!(sizes(maximum + 1), Err(WavError::TooLong { .. })));
        assert!(sizes(usize::MAX).is_err());
        Ok(())
    }
}
