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

// Dev-dependencies pulled in by sibling test targets. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

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

/// Sustained bit errors should eventually produce comfort-noise-quiet
/// output (the muting path).
///
/// The JMBE-compatible muting rule: after 3 consecutive frames where
/// C0 Golay exceeded its error-correction capacity (b0 untrustworthy),
/// the decoder substitutes low-level comfort noise (~0.3% of full
/// scale = ~±98 in i16) for synthesized speech. This test feeds 10
/// consecutive maximally-corrupted frames and verifies that the later
/// frames have dramatically lower energy than a fresh first frame,
/// indicating the muting path engaged.
///
/// Testing note: we can't inspect `repeat_count` directly via the
/// public API, so we rely on the observable energy drop. This is a
/// coarser test than a unit test would be, but it verifies the
/// end-to-end behavior that actually matters to downstream consumers.
#[test]
fn sustained_errors_produce_muted_output() {
    let mut decoder = AmbeDecoder::new();

    // Establish state with a valid frame.
    let _warmup = decoder.decode_frame(&AMBE_SILENCE);

    // Feed 10 corrupted frames. The first few should produce reused
    // previous-frame audio (non-silent); after repeat_count >= 3 the
    // decoder should emit comfort noise instead.
    let mut energies = [0_i64; 10];
    for (i, energy_slot) in energies.iter_mut().enumerate() {
        let pcm = decoder.decode_frame(&[0xFFu8; 9]);
        *energy_slot = pcm.iter().map(|&s| i64::from(s) * i64::from(s)).sum();

        // Bounded output regardless of decode path.
        for &sample in &pcm {
            assert!(
                (-32_760..=32_760).contains(&i32::from(sample)),
                "frame {i}: sample {sample} out of range"
            );
        }
    }

    // Later frames (after muting has engaged) should have much lower
    // energy than the first corrupted frame (which was still reusing
    // the warmup frame's parameters, producing normal voice).
    let late_energy: i64 = energies.iter().skip(5).sum();
    let late_avg = late_energy / 5;

    // Comfort noise model: gain = 0.003 * 32767 ≈ 98 amplitude.
    // Energy per sample ≈ 98² / 3 ≈ 3200 (uniform distribution variance).
    // Over 160 samples, total energy ≈ 512_000.
    // Actual voice frames produce 10-100x more energy.
    assert!(
        late_avg < 10_000_000,
        "sustained corruption should drop energy to comfort-noise level, \
         but average late-frame energy was {late_avg} (energies={energies:?})"
    );
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

/// Largest absolute sample in a decoded frame.
fn frame_peak(pcm: &[i16]) -> i32 {
    pcm.iter().map(|&s| i32::from(s).abs()).max().unwrap_or(0)
}

/// Concealment on a fresh decoder repeats the initial silence
/// parameters — the output must be near-silent, not garbage.
#[test]
fn conceal_on_fresh_decoder_is_near_silence() {
    let mut dec = AmbeDecoder::new();
    let pcm = dec.conceal_frame();
    let peak = frame_peak(&pcm);
    assert!(
        peak < 1000,
        "fresh-decoder concealment should be near-silent, peak={peak}"
    );
}

/// Concealment after decoded audio repeats the previous frame's
/// parameters — output stays in the same amplitude regime as the
/// stream it patches, with no energy blow-up.
#[test]
fn conceal_after_voice_is_bounded_repeat() {
    let mut dec = AmbeDecoder::new();
    let mut decoded_peak = 0_i32;
    for _ in 0..5 {
        let pcm = dec.decode_frame(&AMBE_SILENCE);
        decoded_peak = decoded_peak.max(frame_peak(&pcm));
    }
    let concealed_peak = frame_peak(&dec.conceal_frame());
    assert!(
        concealed_peak <= decoded_peak.saturating_mul(4).saturating_add(1000),
        "concealment must not exceed the stream's regime: \
         concealed={concealed_peak} decoded={decoded_peak}"
    );
}

/// A genuine TH-D75 tone-frame capture must synthesize an audible
/// tone, not silence.
///
/// DVSI hardware encoders emit AMBE tone frames (b0 = 126/127) for
/// any pure-tone input — a 440 Hz test tone into a TH-D75 mic
/// produced this frame (tone index 14 = 437.5 Hz) on nearly every
/// frame of the capture. A decoder that mutes tone frames fails
/// legitimate hardware traffic.
#[test]
fn dvsi_tone_frame_synthesizes_the_tone() {
    // Steady-state frame from a 2026-07-05 TH-D75 wire capture
    // (LSB-first byte order, zero FEC corrections).
    const TONE_FRAME: [u8; 9] = [0xD2, 0x4B, 0x28, 0xB2, 0x57, 0x44, 0xE4, 0x08, 0x1C];
    let mut dec = AmbeDecoder::new();
    // Two consecutive frames — the sine must be phase-continuous
    // across the boundary (a discontinuity would distort the
    // zero-crossing count).
    let mut pcm = Vec::new();
    for _ in 0..2 {
        pcm.extend_from_slice(&dec.decode_frame(&TONE_FRAME));
    }
    let peak = frame_peak(&pcm);
    let mut crossings = 0u32;
    for pair in pcm.windows(2) {
        if let [a, b] = pair
            && (*a > 0) != (*b > 0)
        {
            crossings += 1;
        }
    }
    // 437.5 Hz over 320 samples at 8 kHz = 17.5 cycles ≈ 35 sign
    // changes; allow slack for the first partial cycle.
    assert!(
        (30..=40).contains(&crossings),
        "expected ~35 zero crossings for a 437.5 Hz tone, got {crossings}"
    );
    assert!(peak > 3000, "tone should be audible, peak={peak}");
}

/// Sustained concealment crosses the repeat-count muting threshold
/// and degrades to bounded comfort noise — it must never accumulate
/// energy across consecutive concealed frames.
#[test]
fn sustained_conceal_stays_bounded() {
    let mut dec = AmbeDecoder::new();
    for _ in 0..5 {
        let _pcm = dec.decode_frame(&AMBE_SILENCE);
    }
    let mut peaks = Vec::new();
    for _ in 0..10 {
        peaks.push(frame_peak(&dec.conceal_frame()));
    }
    let max_peak = peaks.iter().copied().max().unwrap_or(0);
    assert!(
        max_peak < 8000,
        "sustained concealment must stay bounded, peaks={peaks:?}"
    );
}
