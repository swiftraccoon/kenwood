// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
//! Historical TH-D75 capture candidates and byte-comparison helpers.
//!
//! The stored candidates and masks come from captures whose framing has not
//! been requalified. Their values remain available for diagnostic comparison;
//! they are not firmware wire-format or encoder-correctness requirements.
//! Recapture with verified framing before drawing protocol or audio conclusions.
//! None of these candidates changes the encoder or decoder processing path.

/// Length of one D-STAR voice frame in bytes (9 = 72 bits).
pub const FRAME_LEN: usize = 9;

/// Four-byte repeating XOR pattern for historical diagnostic transforms.
///
/// This pattern is not a voice-frame wire-format adapter. It is used only
/// by ignored capture probes, never by codec processing.
pub const DIAGNOSTIC_XOR_PATTERN: [u8; 4] = [0x70, 0x4F, 0x93, 0x40];

/// XOR nine diagnostic bytes with the repeating [`DIAGNOSTIC_XOR_PATTERN`].
///
/// Calling twice returns the original. This transform has no protocol
/// interpretation and does not convert between voice-frame formats.
#[must_use]
pub fn apply_diagnostic_xor(frame: [u8; FRAME_LEN]) -> [u8; FRAME_LEN] {
    let mut out = frame;
    for (i, byte) in out.iter_mut().enumerate() {
        let key = DIAGNOSTIC_XOR_PATTERN
            .get(i % DIAGNOSTIC_XOR_PATTERN.len())
            .copied()
            .unwrap_or(0);
        *byte ^= key;
    }
    out
}

/// Bit positions where steady-state-tone captures show >5% volatility.
///
/// This describes the stored candidate bytes, without assigning protocol
/// meaning to their changing bit positions. [`STABLE_BIT_MASK`] is its inverse.
///
/// Volatile bit positions (byte:bit notation, derived from 440 Hz
/// capture and confirmed across 210/550/660 Hz):
///
/// | byte | 0   | 1     | 2   | 3     | 4     | 5     | 6     | 7   | 8   |
/// |------|-----|-------|-----|-------|-------|-------|-------|-----|-----|
/// | bits | 3   | 7, 1  | 5   | 3, 2  | 1, 0  | 7, 5  | 5, 3  | 7   | 5   |
pub const VOLATILE_BIT_MASK: [u8; FRAME_LEN] =
    [0x08, 0x82, 0x20, 0x0C, 0x03, 0xA0, 0x28, 0x80, 0x20];

/// Bit positions stable across all observed steady-state-tone
/// capture candidates from a given pitch. Inverse of [`VOLATILE_BIT_MASK`].
///
/// This selects 58 bit positions for historical byte comparisons.
pub const STABLE_BIT_MASK: [u8; FRAME_LEN] = [0xF7, 0x7D, 0xDF, 0xF3, 0xFC, 0x5F, 0xD7, 0x7F, 0xDF];

/// One historical byte-comparison candidate and its recorded tone metadata.
///
/// `frame` is the stored masked 9-byte capture candidate. Lock
/// quality is the fraction of 2nd-half-of-capture frames that exactly
/// matched this anchor (>=0.95 is considered a clean lock).
///
/// `in_op25_pitch_range` flags whether the input frequency's pitch
/// period (`8000.0 / frequency_hz`) falls inside the OP25 pitch
/// tracker's supported range of 21..122 samples (= 65.6..381 Hz).
/// The record is passive diagnostic metadata, not an accepted encoder input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchAnchor {
    /// Input fundamental frequency in Hz.
    pub frequency_hz: f32,
    /// Stored masked capture candidate.
    /// Compare to `(rust_encoder_output[i] & STABLE_BIT_MASK[i]) for i in 0..9`.
    pub frame: [u8; FRAME_LEN],
    /// Stored unmasked capture candidate.
    ///
    /// At this frequency in 2nd-half steady state. Includes the
    /// bit positions that vary frame-to-frame; this exact
    /// byte sequence is the most-frequent raw frame in the
    /// corresponding capture. Use for ECC-aware decoding
    /// (`decode_trace`) since masking destroys Golay codeword
    /// integrity.
    pub raw_dominant_frame: [u8; FRAME_LEN],
    /// Fraction of 2nd-half-of-capture frames matching `frame` exactly
    /// (after [`STABLE_BIT_MASK`] applied). 1.00 = perfect lock.
    pub lock_quality: f32,
    /// Whether the input frequency's pitch period is inside the OP25
    /// pitch tracker's 21..122-sample (65.6..381 Hz) search range.
    pub in_op25_pitch_range: bool,
}

/// Historical capture candidates, indexed by the recorded input tone frequency.
pub const PITCH_ANCHORS: &[PitchAnchor] = &[
    PitchAnchor {
        frequency_hz: 210.0,
        frame: [0x41, 0x11, 0x04, 0xC3, 0x80, 0x12, 0x01, 0x33, 0x04],
        raw_dominant_frame: [0x49, 0x91, 0x04, 0xCB, 0x80, 0x32, 0x09, 0xB3, 0x04],
        lock_quality: 0.97,
        in_op25_pitch_range: true, // period 38 samples: solidly inside 21..122
    },
    PitchAnchor {
        frequency_hz: 440.0,
        frame: [0x43, 0x50, 0x04, 0x41, 0x68, 0x02, 0x07, 0x12, 0x18],
        raw_dominant_frame: [0x43, 0x52, 0x04, 0x4D, 0x6B, 0xA2, 0x2F, 0x12, 0x18],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 18 samples: below 21 minimum
    },
    PitchAnchor {
        frequency_hz: 550.0,
        frame: [0x45, 0x30, 0x14, 0x43, 0xE0, 0x18, 0xC7, 0x70, 0x10],
        raw_dominant_frame: [0x4D, 0x32, 0x34, 0x4B, 0xE3, 0x18, 0xC7, 0x70, 0x10],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 14.5 samples: below 21 minimum
    },
    PitchAnchor {
        frequency_hz: 660.0,
        frame: [0x45, 0x30, 0x1C, 0xC1, 0xE0, 0x02, 0x47, 0x61, 0x0C],
        raw_dominant_frame: [0x4D, 0x32, 0x3C, 0xC5, 0xE2, 0xA2, 0x6F, 0x61, 0x0C],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 12 samples: below 21 minimum
    },
];

/// Apply [`STABLE_BIT_MASK`] to a raw 9-byte AMBE frame, zeroing the
/// 14 unselected positions. The result can be compared with stored candidates.
#[must_use]
pub fn mask_stable_bits(frame: [u8; FRAME_LEN]) -> [u8; FRAME_LEN] {
    let mut out = [0_u8; FRAME_LEN];
    for ((dst, &src), &mask) in out.iter_mut().zip(frame.iter()).zip(STABLE_BIT_MASK.iter()) {
        *dst = src & mask;
    }
    out
}

/// Locate the [`PitchAnchor`] for a given target frequency.
///
/// Returns `None` if no anchor exists for that frequency (only the
/// frequencies in [`PITCH_ANCHORS`] are valid). Uses an exact `f32`
/// equality match: pass the same value as the anchor's
/// `frequency_hz`.
#[must_use]
pub fn anchor_for(frequency_hz: f32) -> Option<&'static PitchAnchor> {
    PITCH_ANCHORS
        .iter()
        .find(|a| a.frequency_hz.to_bits() == frequency_hz.to_bits())
}

/// Count bit positions where `lhs` and `rhs` differ. Used by HITL
/// tests to report Hamming distance between the Rust encoder's output
/// and a [`PitchAnchor::frame`]. Zero on perfect match.
#[must_use]
pub fn hamming_distance(lhs: [u8; FRAME_LEN], rhs: [u8; FRAME_LEN]) -> u32 {
    lhs.iter()
        .zip(rhs.iter())
        .map(|(&l, &r)| (l ^ r).count_ones())
        .sum()
}

#[cfg(test)]
mod diagnostic_xor_tests {
    use super::{DIAGNOSTIC_XOR_PATTERN, FRAME_LEN, apply_diagnostic_xor};

    #[test]
    fn whitening_is_self_inverse() {
        let original = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x42];
        assert_eq!(
            apply_diagnostic_xor(apply_diagnostic_xor(original)),
            original
        );
    }

    #[test]
    fn whitening_xor_pattern() {
        // The first four bytes repeat across the nine-byte input.
        let zeros = [0_u8; FRAME_LEN];
        let whitened = apply_diagnostic_xor(zeros);
        for (i, (&b, &expected)) in whitened
            .iter()
            .zip(DIAGNOSTIC_XOR_PATTERN.iter().cycle())
            .enumerate()
        {
            assert_eq!(
                b, expected,
                "byte {i}: whitened {b:#x} != expected {expected:#x}",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PITCH_ANCHORS, STABLE_BIT_MASK, VOLATILE_BIT_MASK, hamming_distance, mask_stable_bits,
    };

    /// `STABLE_BIT_MASK` and `VOLATILE_BIT_MASK` must partition all 72 bits.
    #[test]
    fn masks_partition_all_72_bits() {
        for (i, (&stable, &volatile)) in STABLE_BIT_MASK
            .iter()
            .zip(VOLATILE_BIT_MASK.iter())
            .enumerate()
        {
            assert_eq!(
                stable | volatile,
                0xFF,
                "byte {i}: stable {stable:#x} | volatile {volatile:#x} != 0xFF",
            );
            assert_eq!(
                stable & volatile,
                0,
                "byte {i}: stable {stable:#x} & volatile {volatile:#x} != 0 (overlap)",
            );
        }
    }

    /// Volatile mask should cover exactly 14 bits (8 slow-data + 6 parity).
    #[test]
    fn volatile_mask_is_14_bits() {
        let n: u32 = VOLATILE_BIT_MASK.iter().map(|b| b.count_ones()).sum();
        assert_eq!(n, 14, "expected 14 volatile bits, got {n}");
    }

    /// Stable mask should cover the remaining 58 bits.
    #[test]
    fn stable_mask_is_58_bits() {
        let n: u32 = STABLE_BIT_MASK.iter().map(|b| b.count_ones()).sum();
        assert_eq!(n, 58, "expected 58 stable bits, got {n}");
    }

    /// Each anchor frame must already have the volatile bits zeroed;
    /// they are stored pre-masked so direct comparison works.
    #[test]
    fn anchor_frames_are_already_masked() {
        for anchor in PITCH_ANCHORS {
            for (i, (&byte, &volatile)) in anchor
                .frame
                .iter()
                .zip(VOLATILE_BIT_MASK.iter())
                .enumerate()
            {
                assert_eq!(
                    byte & volatile,
                    0,
                    "anchor {} Hz, byte {i}: volatile bits not zeroed in {byte:#x}",
                    anchor.frequency_hz
                );
            }
        }
    }

    /// `mask_stable_bits` is idempotent on already-masked data.
    #[test]
    fn mask_stable_bits_idempotent() {
        for anchor in PITCH_ANCHORS {
            let once = mask_stable_bits(anchor.frame);
            let twice = mask_stable_bits(once);
            assert_eq!(once, twice);
            assert_eq!(once, anchor.frame, "anchor was not pre-masked");
        }
    }

    /// `hamming_distance` is zero between an anchor and itself; nonzero
    /// between distinct anchors.
    #[test]
    fn hamming_distance_self_is_zero() {
        for anchor in PITCH_ANCHORS {
            assert_eq!(hamming_distance(anchor.frame, anchor.frame), 0);
        }
    }

    /// Sanity: every pair of distinct pitch anchors differs by 12-18 bits
    /// (close pitches share more structure; far pitches diverge more).
    /// Exact distances were measured during the RE pass and codified here
    /// to catch drift if any anchor is mistyped.
    #[test]
    fn cross_anchor_distances_match_measurements() -> Result<(), Box<dyn std::error::Error>> {
        // (lhs_hz, rhs_hz, expected_bits)
        let expected: &[(f32, f32, u32)] = &[
            (210.0, 440.0, 17),
            (210.0, 550.0, 18),
            (210.0, 660.0, 16),
            (440.0, 550.0, 17),
            (440.0, 660.0, 17),
            (550.0, 660.0, 12),
        ];
        for &(lhs_hz, rhs_hz, want) in expected {
            let lhs = PITCH_ANCHORS
                .iter()
                .find(|a| a.frequency_hz.to_bits() == lhs_hz.to_bits())
                .ok_or("lhs anchor present")?;
            let rhs = PITCH_ANCHORS
                .iter()
                .find(|a| a.frequency_hz.to_bits() == rhs_hz.to_bits())
                .ok_or("rhs anchor present")?;
            let got = hamming_distance(lhs.frame, rhs.frame);
            assert_eq!(
                got, want,
                "{lhs_hz} Hz vs {rhs_hz} Hz: got {got} bits, expected {want}"
            );
        }
        Ok(())
    }
}
