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

/// D-STAR voice seq values cycle 0..=20.
const SEQ_MODULUS: u16 = 21;

/// Result of decoding one stream's frames.
#[derive(Debug, Clone)]
pub struct DecodedAudio {
    /// 8 kHz mono PCM, 160 samples per (received or concealed) frame.
    pub pcm: Vec<i16>,
    /// Frames synthesized by concealment to fill seq gaps.
    pub concealed_frames: u64,
}

/// Decode a stream's frames to PCM, concealing seq gaps in place.
#[must_use]
pub fn decode_stream(frames: &[FrameRecord]) -> DecodedAudio {
    let mut decoder = AmbeDecoder::new();
    let mut pcm: Vec<i16> = Vec::with_capacity(frames.len() * 160);
    let mut concealed_frames: u64 = 0;
    let mut prev_seq: Option<u8> = None;

    for frame in frames {
        // Concealment only makes sense inside the valid seq alphabet
        // (0..=20); corrupted wire bytes carry arbitrary values and
        // must not drive the modular distance (underflow observed
        // live on a corrupted frame).
        if let Some(prev) = prev_seq
            && u16::from(frame.seq) < SEQ_MODULUS
            && u16::from(prev) < SEQ_MODULUS
        {
            let distance = (u16::from(frame.seq) + SEQ_MODULUS - u16::from(prev)) % SEQ_MODULUS;
            for _ in 1..distance {
                pcm.extend_from_slice(&decoder.conceal_frame());
                concealed_frames += 1;
            }
        }
        prev_seq = Some(frame.seq);
        pcm.extend_from_slice(&decoder.decode_frame(&frame.ambe));
    }

    DecodedAudio {
        pcm,
        concealed_frames,
    }
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
    fn contiguous_frames_decode_to_160_samples_each() {
        let out = decode_stream(&[rec(0), rec(1), rec(2)]);
        assert_eq!(out.pcm.len(), 3 * 160);
        assert_eq!(out.concealed_frames, 0);
    }

    #[test]
    fn seq_gap_is_concealed_to_keep_the_timeline() {
        // 18, 19, [20, 0 missing], 1 → 3 received + 2 concealed
        let out = decode_stream(&[rec(18), rec(19), rec(1)]);
        assert_eq!(out.pcm.len(), 5 * 160);
        assert_eq!(out.concealed_frames, 2);
    }

    #[test]
    fn duplicate_seq_decodes_without_concealment() {
        let out = decode_stream(&[rec(3), rec(3)]);
        assert_eq!(out.pcm.len(), 2 * 160);
        assert_eq!(out.concealed_frames, 0);
    }

    #[test]
    fn out_of_alphabet_seq_decodes_without_concealment() {
        // Same corrupted-wire-byte scenario the capture core guards:
        // decode must not underflow or conceal against wild seqs.
        let out = decode_stream(&[rec(3), rec(200), rec(3), rec(255), rec(4)]);
        assert_eq!(out.pcm.len(), 5 * 160);
        assert_eq!(out.concealed_frames, 0);
    }

    #[test]
    fn empty_stream_is_empty_audio() {
        let out = decode_stream(&[]);
        assert!(out.pcm.is_empty());
        assert_eq!(out.concealed_frames, 0);
    }
}
