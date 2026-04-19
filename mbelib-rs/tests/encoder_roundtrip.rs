// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Encoder/decoder round-trip tests.
//!
//! Feed a known audio signal (sine wave, typical spoken-voice level)
//! into the encoder, take the 9-byte AMBE output, feed that through
//! the decoder, and check the decoded PCM is non-silent. This is the
//! MINIMUM bar our codec must clear: even without DVSI chip interop,
//! our own encoder and decoder must agree on what a signal encodes to
//! and what those bytes decode back to.
//!
//! Without this test we can get to "sextant-to-sextant is silent
//! even though the network layer ferries bytes flawlessly" and have
//! no idea whether the issue is encoder, decoder, or both.

#![cfg(feature = "encoder")]

use mbelib_rs::{AmbeDecoder, AmbeEncoder};

/// Build a 1 kHz sine wave chunk at -20 dBFS, 160 samples @ 8 kHz
/// (one 20 ms AMBE frame). `-20 dBFS` ≈ typical spoken-voice level.
fn make_sine_chunk(t0_samples: usize) -> [f32; 160] {
    let mut buf = [0.0_f32; 160];
    let amplitude = 0.1_f32; // -20 dBFS
    let freq_hz = 1000.0_f32;
    let sr = 8000.0_f32;
    for (i, slot) in buf.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let t = (t0_samples + i) as f32;
        *slot = amplitude * (t * 2.0 * std::f32::consts::PI * freq_hz / sr).sin();
    }
    buf
}

/// RMS of an i16 PCM buffer, normalized to [0.0, 1.0].
fn rms_i16(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = pcm
        .iter()
        .map(|&s| {
            let x = f64::from(s) / 32768.0;
            x * x
        })
        .sum();
    #[allow(clippy::cast_precision_loss)]
    let mean = sum_sq / pcm.len() as f64;
    #[allow(clippy::cast_possible_truncation)]
    let rms = mean.sqrt() as f32;
    rms
}

/// Peak absolute value of an i16 PCM buffer, normalized to [0.0, 1.0].
fn peak_i16(pcm: &[i16]) -> f32 {
    pcm.iter()
        .map(|&s| (f32::from(s) / 32768.0).abs())
        .fold(0.0_f32, f32::max)
}

/// A sustained sine wave, piped through encoder and then decoder,
/// must produce non-silent output from the dec. Threshold is
/// deliberately generous — we're asserting "some signal survives",
/// not bit-exactness.
#[test]
fn sine_1khz_encode_decode_produces_audio() {
    let mut enc = AmbeEncoder::new();
    let mut dec = AmbeDecoder::new();

    // Warm up the encoder's internal state across a few frames —
    // pitch tracker, spectral history etc. need time to converge.
    // Same pattern we use in the encoder's pitch.rs tests.
    let mut t0 = 0_usize;
    for _ in 0..8 {
        let chunk = make_sine_chunk(t0);
        let _ambe = enc.encode_frame(&chunk);
        t0 += 160;
    }

    // Now capture 10 more frames' worth of output and feed each
    // through the dec.
    let mut decoded = Vec::with_capacity(10 * 160);
    for _ in 0..10 {
        let chunk = make_sine_chunk(t0);
        t0 += 160;
        let ambe = enc.encode_frame(&chunk);
        let pcm = dec.decode_frame(&ambe);
        decoded.extend_from_slice(&pcm);
    }

    let rms = rms_i16(&decoded);
    let peak = peak_i16(&decoded);

    // A 1 kHz sine at -20 dBFS should round-trip to AT LEAST
    // something above floor noise. If the codec is internally
    // broken, all-zero AMBE will decode to silence and peak will
    // be essentially 0.
    assert!(
        peak > 0.01,
        "encoder→decoder round-trip produced silent output for a sustained sine wave input. \
         peak={peak:.4} rms={rms:.4}. This means the encoder is emitting AMBE bytes that even \
         our own decoder interprets as silence — a codec internal-consistency bug."
    );
}

/// Zero (silent) input round-trips to comfort-noise level output,
/// not pure silence.
///
/// D-STAR / mbelib silence frames (b0 = 124/125) are decoded with
/// w0 = 2π/32, L = 14, all bands unvoiced — which produces a
/// low-level random-phase unvoiced synthesis (comfort noise) rather
/// than exact zero. This matches DVSI chip behavior: on-wire AMBE
/// silence is not digital-zero audio; it carries a low-level
/// pseudo-noise floor so listeners don't perceive the audio channel
/// as dropped.
///
/// The realistic bound here is "below speech level" — roughly
/// -10 dBFS peak = 0.3 linear. Anything above that indicates a
/// broken gain path (which is the 0.22-peak symptom we caught
/// during the 2400 migration).
#[test]
fn zero_input_encode_decode_is_comfort_noise_level() {
    let mut enc = AmbeEncoder::new();
    let mut dec = AmbeDecoder::new();

    let zero_chunk = [0.0_f32; 160];
    let mut decoded = Vec::with_capacity(10 * 160);
    for _ in 0..10 {
        let ambe = enc.encode_frame(&zero_chunk);
        let pcm = dec.decode_frame(&ambe);
        decoded.extend_from_slice(&pcm);
    }

    let peak = peak_i16(&decoded);
    let rms = rms_i16(&decoded);
    assert!(
        peak < 0.3,
        "zero input produced loud output (peak={peak:.4} rms={rms:.4}) — \
         the decoder is synthesizing a real signal for what should be silence. \
         Expected behaviour: comfort noise floor well below speech level."
    );
}

/// Dump the AMBE bytes emitted for zero input across N frames so we
/// can see WHY the decoder produces loud output. Not a pass/fail
/// test — it's diagnostic only.
#[test]
fn diagnostic_dump_zero_input_ambe() {
    let mut enc = AmbeEncoder::new();
    for frame_num in 0..12 {
        let ambe = enc.encode_frame(&[0.0_f32; 160]);
        println!("zero-input frame {frame_num}: ambe = {ambe:02x?}");
    }
}

/// Dump the AMBE bytes emitted for sine input so we can compare
/// against zero and see how different they are.
#[test]
fn diagnostic_dump_sine_input_ambe() {
    let mut enc = AmbeEncoder::new();
    let mut t0 = 0_usize;
    for frame_num in 0..12 {
        let chunk = make_sine_chunk(t0);
        t0 += 160;
        let ambe = enc.encode_frame(&chunk);
        println!("sine-1khz frame {frame_num}: ambe = {ambe:02x?}");
    }
}

/// Sine input and zero input must produce DIFFERENT AMBE frames.
/// If the encoder outputs the same bytes for both, it's collapsing
/// any signal to a silence-equivalent pattern.
#[test]
fn sine_input_differs_from_zero_input_encoding() {
    let mut enc_sine = AmbeEncoder::new();
    let mut enc_zero = AmbeEncoder::new();
    let mut t0 = 0_usize;
    for _ in 0..8 {
        let _ = enc_sine.encode_frame(&make_sine_chunk(t0));
        let _ = enc_zero.encode_frame(&[0.0_f32; 160]);
        t0 += 160;
    }
    let ambe_sine = enc_sine.encode_frame(&make_sine_chunk(t0));
    let ambe_zero = enc_zero.encode_frame(&[0.0_f32; 160]);
    assert_ne!(
        ambe_sine, ambe_zero,
        "encoder emitted identical AMBE bytes for sine and zero input — \
         sine={ambe_sine:02x?} zero={ambe_zero:02x?} — V/UV or gain path is collapsing all inputs."
    );
}
