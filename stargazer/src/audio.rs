// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE → PCM decoding with seq-gap concealment.
//!
//! One fresh [`AmbeDecoder`] per stream: its adaptive-smoothing
//! state must never bleed across talkers. Missing frames (seq
//! discontinuities) are filled with the decoder's concealment output
//! so the WAV timeline matches the transmission's codec time.

use mbelib_rs::AmbeDecoder;

use crate::capture::FrameRecord;

/// Result of decoding one stream's frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedAudio {
    /// 8 kHz mono PCM, 160 samples per (received or concealed) frame.
    pcm: Vec<i16>,
    /// Frames synthesized by concealment to fill seq gaps.
    concealed_frames: u64,
}

/// A decoded stream cannot be represented or allocated.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// Received frames and inferred gaps exceed the platform's sample capacity.
    #[error("decoded audio exceeds the platform's addressable sample count")]
    TooLong,
    /// Reserving the complete sample buffer failed.
    #[error("PCM allocation: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
}

impl DecodedAudio {
    /// Decode a stream with a fresh decoder and conceal each inferred sequence gap.
    ///
    /// Raw sequence bytes outside 0..=20 and duplicates infer no missing frames.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::TooLong`] before decoding when the complete sample
    /// count cannot be represented, or [`AudioError::Allocation`] when reservation fails.
    pub fn from_frames(frames: &[FrameRecord]) -> Result<Self, AudioError> {
        let expected = expected_frames(frames).ok_or(AudioError::TooLong)?;
        let samples = usize::try_from(expected)
            .ok()
            .and_then(|count| count.checked_mul(160))
            .ok_or(AudioError::TooLong)?;
        let mut decoder = AmbeDecoder::new();
        let mut pcm = Vec::new();
        pcm.try_reserve_exact(samples)
            .map_err(AudioError::Allocation)?;
        let mut concealed_frames: u64 = 0;
        let mut prev_seq: Option<u8> = None;

        for frame in frames {
            // Out-of-alphabet sequence bytes remain lossless observations;
            // only the 21-value alphabet contributes inferred gaps.
            if let Some(prev) = prev_seq {
                for _ in 0..crate::capture::seq_gap(prev, frame.seq) {
                    pcm.extend_from_slice(&decoder.conceal_frame());
                    concealed_frames += 1;
                }
            }
            prev_seq = Some(frame.seq);
            pcm.extend_from_slice(&decoder.decode_frame(&frame.ambe));
        }

        Ok(Self {
            pcm,
            concealed_frames,
        })
    }
    /// Borrow the complete 8 kHz signed PCM stream.
    #[must_use]
    pub fn pcm(&self) -> &[i16] {
        &self.pcm
    }
    /// Number of inferred missing frames synthesized by the decoder.
    #[must_use]
    pub const fn concealed_frames(&self) -> u64 {
        self.concealed_frames
    }
    /// Take the PCM buffer without copying.
    #[must_use]
    pub fn into_pcm(self) -> Vec<i16> {
        self.pcm
    }
}

pub(crate) fn expected_frames(frames: &[FrameRecord]) -> Option<u64> {
    let received = u64::try_from(frames.len()).ok()?;
    frames.windows(2).try_fold(received, |total, pair| {
        let previous = pair.first()?;
        let next = pair.get(1)?;
        total.checked_add(crate::capture::seq_gap(previous.seq, next.seq))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::FrameRecord;

    fn rec(seq: u8) -> FrameRecord {
        FrameRecord {
            seq,
            ambe: [0u8; 9],
            slow_data: [0u8; 3],
        }
    }

    #[test]
    fn contiguous_frames_decode_to_160_samples_each() -> Result<(), Box<dyn std::error::Error>> {
        let out = DecodedAudio::from_frames(&[rec(0), rec(1), rec(2)])?;
        assert_eq!(out.pcm.len(), 3 * 160);
        assert_eq!(out.concealed_frames, 0);
        Ok(())
    }

    #[test]
    fn seq_gap_is_concealed_to_keep_the_timeline() -> Result<(), Box<dyn std::error::Error>> {
        // 18, 19, [20, 0 missing], 1 → 3 received + 2 concealed
        let out = DecodedAudio::from_frames(&[rec(18), rec(19), rec(1)])?;
        assert_eq!(out.pcm.len(), 5 * 160);
        assert_eq!(out.concealed_frames, 2);
        Ok(())
    }

    #[test]
    fn duplicate_seq_decodes_without_concealment() -> Result<(), Box<dyn std::error::Error>> {
        let out = DecodedAudio::from_frames(&[rec(3), rec(3)])?;
        assert_eq!(out.pcm.len(), 2 * 160);
        assert_eq!(out.concealed_frames, 0);
        Ok(())
    }

    #[test]
    fn out_of_alphabet_seq_decodes_without_concealment() -> Result<(), Box<dyn std::error::Error>> {
        // Same corrupted-wire-byte scenario the capture core guards:
        // decode must not underflow or conceal against wild seqs.
        let out = DecodedAudio::from_frames(&[rec(3), rec(200), rec(3), rec(255), rec(4)])?;
        assert_eq!(out.pcm.len(), 5 * 160);
        assert_eq!(out.concealed_frames, 0);
        Ok(())
    }

    #[test]
    fn empty_stream_is_empty_audio() -> Result<(), Box<dyn std::error::Error>> {
        let out = DecodedAudio::from_frames(&[])?;
        assert!(out.pcm.is_empty());
        assert_eq!(out.concealed_frames, 0);
        Ok(())
    }
}
