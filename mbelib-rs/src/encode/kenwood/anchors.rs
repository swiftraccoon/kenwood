// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Hardware-in-the-loop verification anchors for the Kenwood-exact
// encoder. Each constant in this module was derived from radio-captured
// AMBE bytes — TH-D75 in D-STAR TX mode, AMBE_CAPTURE env var directing
// the firmware's encoded output to a file. The bit-level structure was
// reverse-engineered by feeding the radio known sinusoidal tones via
// the mic and observing which bits are stable (the actual voice
// encoding) vs. volatile (slow-data interleave + Golay parity).

//! Bit-exact verification anchors from TH-D75 radio captures.
//!
//! For the Rust encoder to be "Kenwood-perfect" it must, given a
//! synthesized sinusoidal input matching one of the captured tones,
//! emit a 9-byte AMBE frame whose 58 stable bits ([`STABLE_BIT_MASK`])
//! match the corresponding anchor in [`PITCH_ANCHORS`] byte-for-byte.
//!
//! ## Wire-format caveat (critical)
//!
//! Decoding a Kenwood 210 Hz raw dominant frame through
//! [`crate::decode_trace`] produces `b0=126` (the AMBE erasure code),
//! `L=0` — NOT a sensible 210 Hz pitch index. The same `decode_trace`
//! recovers `b0=86, L=34` correctly when fed our own encoder's output.
//! Conclusion: **TH-D75 does not use the DSD/mbelib wire format**.
//!
//! Bit-mask comparison via [`STABLE_BIT_MASK`] is therefore a NECESSARY
//! but not SUFFICIENT criterion — bit-identical wire bytes guarantee
//! identical decoded fields only if both encoders use the same
//! bit-permutation. Until the Kenwood interleaver (firmware
//! `ambe_bit_interleaver` at 0x11815550) is ported, the masked
//! comparison effectively measures byte-identity in two different
//! coordinate systems.
//!
//! Order of operations to actually achieve "Kenwood-perfect":
//!   1. Port the firmware interleaver to a `kenwood-tables` feature
//!      branch (replaces DSD's `dW[72]/dX[72]`).
//!   2. Re-derive [`STABLE_BIT_MASK`] / [`VOLATILE_BIT_MASK`] in the
//!      Kenwood coordinate system (the volatility analysis remains
//!      valid; only the bit positions translate).
//!   3. Re-run [`PITCH_ANCHORS`] capture-vs-encoder comparison.
//!
//! ## Frame structure (validated against 6 distinct tone captures)
//!
//! - **9 bytes per voice frame, no header** — confirmed via stride-9
//!   repetition in steady-state captures (99/99 frames byte-identical
//!   in the 440 Hz 2nd half). This matches the firmware's
//!   `radio_send_dsp_command_9b` IPC name identified during the
//!   static RE pass. Captures begin at byte 0; nothing precedes the
//!   first frame.
//! - **14 of 72 bits volatile per frame** — 8 slow-data positions at
//!   ~31% volatility + 6 FEC parity positions at ~11-18% volatility.
//!   The Rust encoder is responsible for the 58 stable bits; slow-data
//!   bits are protocol-layer state injected outside the codec.
//!
//! ## Volatility analysis
//!
//! Across the 197-frame 440 Hz capture, every bit position in
//! [`VOLATILE_BIT_MASK`] flipped at least 11% of frames; every bit
//! position in [`STABLE_BIT_MASK`] was constant. The same masks held
//! across 210/550/660 Hz captures, confirming the volatility pattern
//! is per-frame protocol overhead, not per-pitch.

/// Length of one D-STAR voice frame in bytes (9 = 72 bits).
///
/// Matches the firmware-side IPC command name
/// `radio_send_dsp_command_9b` at ARM address 0xC005D928.
pub const FRAME_LEN: usize = 9;

/// TH-D75 firmware FEC whitening pattern (32-bit, cycled).
///
/// Source: DSP firmware function at virtual address 0x118123E0.
/// Applied as `out[i] = in[i] ^ WHITENING[i % 4]` to the post-Golay
/// AMBE bit stream. When dewhitened, a Kenwood-captured frame decodes
/// through the standard DSD/mbelib ECC pipeline.
///
/// Applying [`apply_whitening`] to a Rust encoder's output (or
/// equivalently, dewhitening a Kenwood capture) is the simple wire-
/// format adapter between mbelib's wire format and TH-D75's wire
/// format. Verified across the 210/440/550/660 Hz captures — every
/// dewhitened steady-state frame decodes to a sensible AMBE
/// `(b0, b1, L)` tuple.
pub const FIRMWARE_WHITENING: [u8; 4] = [0x70, 0x4F, 0x93, 0x40];

/// Apply the TH-D75 firmware whitening XOR to a 9-byte frame.
///
/// Self-inverse — calling twice returns the original. Use to convert
/// between mbelib wire format and TH-D75 wire format in either
/// direction.
#[must_use]
pub fn apply_whitening(frame: [u8; FRAME_LEN]) -> [u8; FRAME_LEN] {
    let mut out = frame;
    for (i, byte) in out.iter_mut().enumerate() {
        // Iter pos i in 0..9, FIRMWARE_WHITENING.len() = 4 (compile-time
        // const), so i % 4 ∈ 0..4 is statically bounded for the index.
        let key = FIRMWARE_WHITENING
            .get(i % FIRMWARE_WHITENING.len())
            .copied()
            .unwrap_or(0);
        *byte ^= key;
    }
    out
}

/// Bit positions where steady-state-tone captures show >5% volatility.
///
/// The bits in this mask carry slow-data injection (~31% volatility)
/// or Golay-parity-of-slow-data (~11-18% volatility). They are NOT
/// produced by the AMBE codec — they are protocol-layer overhead
/// stamped on top of the codec output.
///
/// To verify the AMBE codec's bit-exact output, AND-mask the encoder's
/// frame with [`STABLE_BIT_MASK`] (the inverse) and compare to a
/// [`PITCH_ANCHORS`] entry.
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
/// captures from a given pitch. Inverse of [`VOLATILE_BIT_MASK`].
///
/// Use this mask to extract the 58 codec-determined bits from a raw
/// 9-byte AMBE frame: `frame[i] & STABLE_BIT_MASK[i]`. The result must
/// match the corresponding [`PITCH_ANCHORS`] entry for "Kenwood-exact"
/// encoder verification.
pub const STABLE_BIT_MASK: [u8; FRAME_LEN] = [0xF7, 0x7D, 0xDF, 0xF3, 0xFC, 0x5F, 0xD7, 0x7F, 0xDF];

/// One bit-exact AMBE frame anchor for a specific input tone.
///
/// `frame` is the masked 9-byte voice frame the TH-D75 firmware emits
/// in steady state for a sinusoidal input at `frequency_hz`. Lock
/// quality is the fraction of 2nd-half-of-capture frames that exactly
/// matched this anchor (>=0.95 is considered a clean lock).
///
/// `in_op25_pitch_range` flags whether the input frequency's pitch
/// period (`8000.0 / frequency_hz`) falls inside the OP25 pitch
/// tracker's supported range of 21..122 samples (= 65.6..381 Hz).
/// Anchors outside this range will not be reachable by the current
/// encoder until the pitch search is widened to cover the full
/// IMBE-spec range (~50..625 Hz). The diagnostic
/// `pitch_tracker_diagnostic` confirms 440/550/660 Hz inputs
/// octave-fold to ~220/275/220 Hz inside the OP25 range, producing
/// a different b0 than Kenwood emits.
#[derive(Debug, Clone, Copy)]
pub struct PitchAnchor {
    /// Input fundamental frequency in Hz.
    pub frequency_hz: f32,
    /// Steady-state masked frame the TH-D75 firmware produces.
    /// Compare to `(rust_encoder_output[i] & STABLE_BIT_MASK[i]) for i in 0..9`.
    pub frame: [u8; FRAME_LEN],
    /// Steady-state RAW (unmasked) frame the TH-D75 firmware produces.
    ///
    /// At this frequency in 2nd-half steady state. Includes the
    /// slow-data + parity bits that vary frame-to-frame; this exact
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

/// Bit-exact reference frames captured from a TH-D75 in D-STAR TX
/// mode, indexed by input tone frequency.
///
/// The Rust encoder, fed a synthetic sinusoidal PCM stream at
/// `frequency_hz`, must produce 9-byte AMBE frames whose stable bits
/// match `frame` byte-for-byte (allow encoder transient ~50 frames
/// before the first match).
///
/// All four tones lock cleanly (>=0.95 of 2nd-half frames identical).
/// 100 Hz and 320 Hz captures exist but did not lock — likely due to
/// reproduction quality limits of the phone-speaker source used during
/// recording, not codec behaviour. Excluded here.
pub const PITCH_ANCHORS: &[PitchAnchor] = &[
    PitchAnchor {
        frequency_hz: 210.0,
        frame: [0x41, 0x11, 0x04, 0xC3, 0x80, 0x12, 0x01, 0x33, 0x04],
        raw_dominant_frame: [0x49, 0x91, 0x04, 0xCB, 0x80, 0x32, 0x09, 0xB3, 0x04],
        lock_quality: 0.97,
        in_op25_pitch_range: true, // period 38 samples — solidly inside 21..122
    },
    PitchAnchor {
        frequency_hz: 440.0,
        frame: [0x43, 0x50, 0x04, 0x41, 0x68, 0x02, 0x07, 0x12, 0x18],
        raw_dominant_frame: [0x43, 0x52, 0x04, 0x4D, 0x6B, 0xA2, 0x2F, 0x12, 0x18],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 18 samples — below 21 minimum
    },
    PitchAnchor {
        frequency_hz: 550.0,
        frame: [0x45, 0x30, 0x14, 0x43, 0xE0, 0x18, 0xC7, 0x70, 0x10],
        raw_dominant_frame: [0x4D, 0x32, 0x34, 0x4B, 0xE3, 0x18, 0xC7, 0x70, 0x10],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 14.5 samples — below 21 minimum
    },
    PitchAnchor {
        frequency_hz: 660.0,
        frame: [0x45, 0x30, 0x1C, 0xC1, 0xE0, 0x02, 0x47, 0x61, 0x0C],
        raw_dominant_frame: [0x4D, 0x32, 0x3C, 0xC5, 0xE2, 0xA2, 0x6F, 0x61, 0x0C],
        lock_quality: 1.00,
        in_op25_pitch_range: false, // period 12 samples — below 21 minimum
    },
];

/// Apply [`STABLE_BIT_MASK`] to a raw 9-byte AMBE frame, zeroing the
/// 14 protocol-overhead bits.
///
/// The result is the codec-determined portion of the frame, suitable
/// for byte-for-byte comparison against a [`PitchAnchor::frame`].
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
/// equality match — pass the same value as the anchor's
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
mod whitener_tests {
    use super::{FIRMWARE_WHITENING, FRAME_LEN, apply_whitening};

    #[test]
    fn whitening_is_self_inverse() {
        let original = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x42];
        assert_eq!(apply_whitening(apply_whitening(original)), original);
    }

    #[test]
    fn whitening_xor_pattern() {
        // First 4 bytes XOR FIRMWARE_WHITENING; subsequent bytes cycle.
        let zeros = [0_u8; FRAME_LEN];
        let whitened = apply_whitening(zeros);
        for (i, (&b, &expected)) in whitened
            .iter()
            .zip(FIRMWARE_WHITENING.iter().cycle())
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

    /// Each anchor frame must already have the volatile bits zeroed —
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
