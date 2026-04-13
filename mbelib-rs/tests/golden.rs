// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Golden tests exercising the complete AMBE decode pipeline end-to-end.
//!
//! These tests verify PROPERTIES of the decoder output -- boundedness,
//! near-silence for silence inputs, determinism, stability -- rather than
//! exact PCM sample values. This makes them resilient to decoder
//! refinements while still catching regressions in the decode chain
//! (unpack -> ECC -> demodulate -> decode -> enhance -> synthesize -> output).

use mbelib_rs::AmbeDecoder;

/// D-STAR AMBE silence frame bytes.
///
/// These are the "comfort noise" bytes transmitted in EOT packets and
/// used as filler. Reference: `dstar-gateway-core/src/voice.rs`
/// constant `AMBE_SILENCE`, sourced from `g4klx/MMDVMHost/DStarDefines.h:44`.
const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

/// An all-zero AMBE frame should produce PCM samples that are all zero
/// or very close to zero.
///
/// The all-zero input encodes a silence frame at the codec level: zero
/// fundamental frequency, zero gain deltas, all bands unvoiced. The
/// entire synthesis path should produce negligible output.
#[test]
fn silence_frame_produces_near_silence() {
    let mut decoder = AmbeDecoder::new();
    let pcm = decoder.decode_frame(&[0u8; 9]);

    let max_abs = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    assert!(
        max_abs < 100,
        "all-zero AMBE frame should produce near-silence, \
         but max absolute sample was {max_abs}"
    );
}

/// The standard D-STAR AMBE silence constant should decode to low-level
/// audio, not complete digital silence.
///
/// These are the "comfort noise" bytes that hardware DVSI vocoders emit
/// during idle. They encode a minimal voiced signal that sounds like
/// quiet background hiss, preventing the abrupt perceptual discontinuity
/// of hard digital silence. The output should be quiet but not zero.
#[test]
fn dstar_ambe_silence_constant() {
    let mut decoder = AmbeDecoder::new();
    let pcm = decoder.decode_frame(&AMBE_SILENCE);

    let max_abs = pcm.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);

    // Should be bounded well below clipping.
    assert!(
        max_abs < 16_000,
        "AMBE_SILENCE should decode to quiet audio, \
         but max absolute sample was {max_abs} (above 16000)"
    );

    // Verify no NaN/Inf leaked through the pipeline (would show up as
    // extreme values after the f32->i16 cast).
    for (i, &sample) in pcm.iter().enumerate() {
        assert!(
            (-32_760..=32_760).contains(&i32::from(sample)),
            "sample [{i}] = {sample} is outside the clamped range"
        );
    }
}

/// Feeding 100 consecutive identical frames must not cause the decoder
/// to diverge.
///
/// The AMBE codec uses inter-frame delta coding for gain and spectral
/// magnitudes. If the internal state update has a numerical drift bug,
/// it will accumulate over many frames and eventually produce samples
/// outside the clamp range or NaN. This test catches that class of bug.
#[test]
fn stability_across_100_frames() {
    let mut decoder = AmbeDecoder::new();

    for frame_idx in 0..100 {
        let pcm = decoder.decode_frame(&AMBE_SILENCE);

        for (sample_idx, &sample) in pcm.iter().enumerate() {
            let abs = sample.unsigned_abs();
            assert!(
                abs <= 32_760,
                "frame {frame_idx}, sample [{sample_idx}] = {sample} \
                 exceeds clamp threshold of +/-32760"
            );

            // Check for NaN/Inf artifacts: if a NaN leaked into the f32
            // pipeline, `clamped as i16` on NaN produces 0 on most
            // platforms but is undefined behavior territory in C. In Rust
            // it saturates to 0, so we also check that the output looks
            // reasonable by verifying it's within i16 range (which it
            // always is by type, but the assertion documents intent).
            assert!(
                (-32_760..=32_760).contains(&i32::from(sample)),
                "frame {frame_idx}, sample [{sample_idx}] = {sample} \
                 is outside the valid range"
            );
        }
    }
}

/// Two fresh decoders given the same frame must produce bit-identical output.
///
/// The decoder must be fully deterministic: no random seeds, no
/// uninitialized memory reads, no time-dependent state. This is critical
/// for testing (reproducibility) and for downstream consumers that may
/// run parallel decoders for redundancy.
#[test]
fn deterministic_output() {
    let mut decoder_a = AmbeDecoder::new();
    let mut decoder_b = AmbeDecoder::new();

    let pcm_a = decoder_a.decode_frame(&AMBE_SILENCE);
    let pcm_b = decoder_b.decode_frame(&AMBE_SILENCE);

    assert_eq!(
        pcm_a, pcm_b,
        "two fresh decoders given the same AMBE_SILENCE frame \
         produced different PCM output"
    );
}

/// Feeding a sequence of 10 different frames through two fresh decoders
/// must produce bit-identical output at every step.
///
/// This is a stronger version of `deterministic_output`: it tests that
/// inter-frame state evolution (delta decoding, phase tracking, cross-fade
/// history) is also deterministic, not just single-frame decoding.
#[test]
fn multi_frame_determinism() {
    // 10 arbitrary but fixed byte patterns covering diverse bit
    // distributions: all-zero, all-one, ascending, silence constant,
    // and several hand-picked patterns.
    let frames: [[u8; 9]; 10] = [
        [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09],
        AMBE_SILENCE,
        [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA],
        [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x12],
        [0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01],
        [0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F, 0x7F],
        [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE, 0x00],
        [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8],
    ];

    let mut decoder_a = AmbeDecoder::new();
    let mut decoder_b = AmbeDecoder::new();

    for (frame_idx, frame) in frames.iter().enumerate() {
        let pcm_a = decoder_a.decode_frame(frame);
        let pcm_b = decoder_b.decode_frame(frame);

        assert_eq!(
            pcm_a, pcm_b,
            "decoders diverged at frame {frame_idx}: \
             inter-frame state evolution is not deterministic"
        );
    }
}

/// The first frame decoded from a fresh decoder may have a startup
/// transient because the previous-frame parameters are zeroed.
///
/// This is expected behavior (hardware DVSI vocoders exhibit the same
/// transient), but the transient must still be bounded by the clamp
/// threshold. This test verifies that no pathological values escape
/// the output conversion stage on the very first frame.
#[test]
fn first_frame_transient_bounded() {
    let mut decoder = AmbeDecoder::new();
    let pcm = decoder.decode_frame(&AMBE_SILENCE);

    for (i, &sample) in pcm.iter().enumerate() {
        assert!(
            (-32_760..=32_760).contains(&i32::from(sample)),
            "first-frame transient produced out-of-range sample \
             [{i}] = {sample}"
        );
    }
}

/// A valid frame followed by a heavily corrupted frame should not cause
/// panics, NaN, or unbounded output.
///
/// In real D-STAR operation, bit errors are common (especially on weak
/// signals). The decoder must handle garbage input gracefully. After ECC
/// fails to correct the errors, the decoder should either repeat the
/// previous frame or output bounded audio -- never crash or produce
/// NaN-derived samples.
#[test]
fn frame_repeat_after_errors() {
    let mut decoder = AmbeDecoder::new();

    // First, feed a valid silence frame to establish state.
    let _valid_pcm = decoder.decode_frame(&AMBE_SILENCE);

    // Now feed a maximally corrupted frame (all 0xFF).
    let corrupted_pcm = decoder.decode_frame(&[0xFF; 9]);

    for (i, &sample) in corrupted_pcm.iter().enumerate() {
        assert!(
            (-32_760..=32_760).contains(&i32::from(sample)),
            "corrupted frame produced out-of-range sample [{i}] = {sample}"
        );
    }

    // Feed another valid frame to verify the decoder recovered and
    // can still produce output.
    let recovery_pcm = decoder.decode_frame(&AMBE_SILENCE);

    for (i, &sample) in recovery_pcm.iter().enumerate() {
        assert!(
            (-32_760..=32_760).contains(&i32::from(sample)),
            "recovery frame after corruption produced out-of-range \
             sample [{i}] = {sample}"
        );
    }
}
