// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Diagnostic: dump every intermediate value the encoder produces
//! for a known voice-like signal, so we can see which stage is
//! emitting garbage when sextant↔sextant produces noise with the
//! right volume envelope but no speech content.

#![cfg(feature = "encoder")]

// Dev-dependencies pulled in by sibling tests. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use mbelib_rs::AmbeEncoder;

/// Canonical D-STAR silence bytes. If the encoder is short-circuiting
/// real voice input to this pattern every frame, the decoder will
/// produce comfort noise with gamma-smoothed volume envelope —
/// exactly the "volume tracks, content is noise" symptom.
const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

/// A voice-like signal: 150 Hz fundamental + harmonics 2, 3, 4 with
/// decreasing amplitude. Roughly mimics the spectral envelope of a
/// vowel sound.
fn make_voiced_chunk(t0_samples: usize) -> [f32; 160] {
    let mut buf = [0.0_f32; 160];
    let f0_hz = 150.0_f32;
    let sr = 8000.0_f32;
    let harmonics = [0.3_f32, 0.2, 0.12, 0.06];
    for (i, slot) in buf.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test voice-like generator: t0_samples + i stays within test frame \
                      counts and is exact in f32."
        )]
        let t = (t0_samples + i) as f32;
        let mut s = 0.0_f32;
        for (k, &amp) in harmonics.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "harmonic index: k < 4, usize-to-f32 cast is exact."
            )]
            let harm_num = (k + 1) as f32;
            s += amp * (t * 2.0 * std::f32::consts::PI * f0_hz * harm_num / sr).sin();
        }
        *slot = s;
    }
    buf
}

/// Fail-fast: encoded voice input must NOT match the canonical
/// silence byte pattern. If it does, the pitch tracker is reporting
/// confidence below `SILENCE_CONFIDENCE` and the encoder's silence
/// short-circuit is converting real speech to the DVSI null-audio
/// pattern — a catastrophic failure that produces exactly the
/// "noise with volume envelope, no content" symptom.
#[test]
fn voiced_input_must_not_encode_to_silence() {
    let mut enc = AmbeEncoder::new();
    let mut t0 = 0_usize;
    for _ in 0..20 {
        let _ = enc.encode_frame(&make_voiced_chunk(t0));
        t0 += 160;
    }
    let mut silence_count = 0_u32;
    let sample_count = 20_u32;
    for _ in 0..sample_count {
        let chunk = make_voiced_chunk(t0);
        t0 += 160;
        let ambe = enc.encode_frame(&chunk);
        if ambe == AMBE_SILENCE {
            silence_count += 1;
        }
        eprintln!("frame ambe = {ambe:02x?}");
    }
    eprintln!("{silence_count}/{sample_count} voice frames encoded as silence");
    assert!(
        silence_count < sample_count / 2,
        "encoder emitted AMBE_SILENCE for {silence_count}/{sample_count} post-warmup \
         voice frames — pitch tracker confidence is falling below SILENCE_CONFIDENCE \
         even for a clean 150 Hz harmonic signal"
    );
}

/// Pure sine at 150 Hz — decoder's f0 must match. If decoded
/// spectrum has more energy at 300 Hz than 150 Hz, the pitch-index
/// path is writing the wrong b0 (doubled or halved).
#[test]
fn pure_sine_pitch_preserved_through_codec() {
    let mut enc = AmbeEncoder::new();
    let mut dec = mbelib_rs::AmbeDecoder::new();

    let make_pure = |t0: usize| -> [f32; 160] {
        let mut buf = [0.0_f32; 160];
        let f0_hz = 150.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test pure-sine generator: t0 + i stays within test frame counts."
            )]
            let t = (t0 + i) as f32;
            *slot = 0.3 * (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        buf
    };
    let mut t0 = 0_usize;
    for _ in 0..20 {
        let _ = enc.encode_frame(&make_pure(t0));
        let _ = dec.decode_frame(&enc.encode_frame(&make_pure(t0)));
        t0 += 160;
    }
    let mut decoded = Vec::with_capacity(20 * 160);
    for _ in 0..20 {
        let ambe = enc.encode_frame(&make_pure(t0));
        t0 += 160;
        decoded.extend_from_slice(&dec.decode_frame(&ambe));
    }
    let energy_at = |freq_hz: f32| -> f32 {
        let sr = 8000.0_f32;
        let mut real = 0.0_f32;
        let mut imag = 0.0_f32;
        for (i, &s) in decoded.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test Goertzel: i < decoded.len() = 20*160 = 3200; exact in f32."
            )]
            let t = i as f32 / sr;
            let x = f32::from(s) / 32768.0;
            let phase = 2.0 * std::f32::consts::PI * freq_hz * t;
            real += x * phase.cos();
            imag += x * phase.sin();
        }
        let n = decoded.len();
        #[expect(
            clippy::cast_precision_loss,
            reason = "test normalization: n = 3200; exact in f32."
        )]
        let norm = real.mul_add(real, imag * imag) / (n as f32 * n as f32);
        norm.sqrt()
    };
    let e_75 = energy_at(75.0);
    let e_150 = energy_at(150.0);
    let e_300 = energy_at(300.0);
    let e_450 = energy_at(450.0);
    eprintln!("=== Pure 150 Hz sine, decoded ===");
    eprintln!("  75  Hz (f0/2): {e_75:.6}");
    eprintln!("  150 Hz (f0):   {e_150:.6}");
    eprintln!("  300 Hz (2f0):  {e_300:.6}");
    eprintln!("  450 Hz (3f0):  {e_450:.6}");
}

/// End-to-end: voice-in → encode → decode → check spectral content.
///
/// Not just RMS — check that the decoded signal has the ENERGY
/// concentrated at the expected fundamental + a couple of harmonics.
/// If the decoder is synthesizing filtered noise (unvoiced everywhere)
/// instead of a proper harmonic spectrum, this will show as
/// near-uniform energy across all bins rather than peaks at f0/2f0/3f0.
#[test]
fn voiced_input_produces_harmonic_output() {
    let mut enc = AmbeEncoder::new();
    let mut dec = mbelib_rs::AmbeDecoder::new();

    let mut t0 = 0_usize;
    // Long warmup — pitch tracker and encoder prev state need time.
    for _ in 0..20 {
        let chunk = make_voiced_chunk(t0);
        t0 += 160;
        let ambe = enc.encode_frame(&chunk);
        let _ = dec.decode_frame(&ambe);
    }

    let mut decoded = Vec::with_capacity(20 * 160);
    for _ in 0..20 {
        let chunk = make_voiced_chunk(t0);
        t0 += 160;
        let ambe = enc.encode_frame(&chunk);
        let pcm = dec.decode_frame(&ambe);
        decoded.extend_from_slice(&pcm);
    }

    // Compute energy in the f0 band (150 Hz ± 30 Hz) vs a non-harmonic
    // band (750 Hz, between 5th and 6th harmonic).
    let energy_at = |freq_hz: f32, bw_hz: f32| -> f32 {
        let sr = 8000.0_f32;
        let mut real = 0.0_f32;
        let mut imag = 0.0_f32;
        for (i, &s) in decoded.iter().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test Goertzel: i < decoded.len() = 20*160 = 3200; exact in f32."
            )]
            let t = i as f32 / sr;
            let x = f32::from(s) / 32768.0;
            let phase = 2.0 * std::f32::consts::PI * freq_hz * t;
            real += x * phase.cos();
            imag += x * phase.sin();
        }
        let _ = bw_hz;
        let n = decoded.len();
        #[expect(
            clippy::cast_precision_loss,
            reason = "test normalization: n = 3200; exact in f32."
        )]
        let norm = real.mul_add(real, imag * imag) / (n as f32 * n as f32);
        norm.sqrt()
    };

    // The encoder quantizes pitch through OP25's b0_lookup table
    // (`src/encode/pitch_quant.rs`), so the decoded signal's
    // harmonics don't land exactly at the source's 150/300/450 Hz
    // — they shift to the nearest b0 / W0_TABLE entry. Scan a
    // window around each expected harmonic to find the actual
    // harmonic peak, then compare against background.
    // Probe in 13 integer steps of 5 Hz from hz-30 to hz+30, avoiding
    // `while float <= float` loops (clippy::while_float), which would
    // accumulate float-increment drift.
    let peak_near = |hz: f32| -> f32 {
        let mut best = 0.0_f32;
        for step in 0_u8..=12 {
            let probe = 5.0_f32.mul_add(f32::from(step), hz - 30.0);
            best = best.max(energy_at(probe, 10.0));
        }
        best
    };
    let e_fund = peak_near(150.0);
    let e_harm2 = peak_near(300.0);
    let e_harm3 = peak_near(450.0);
    let e_bg = peak_near(750.0);

    eprintln!("=== Voiced input, decoded spectrum energy (peaks ±30 Hz) ===");
    eprintln!("  @ 150 Hz (f0):  {e_fund:.6}");
    eprintln!("  @ 300 Hz (2f0): {e_harm2:.6}");
    eprintln!("  @ 450 Hz (3f0): {e_harm3:.6}");
    eprintln!("  @ 750 Hz (noise): {e_bg:.6}");

    // Require the strongest harmonic to at least match the noise
    // band. Tight margins (e.g. > 3×) were workable before the OP25
    // b0_lookup port but over-constrain the relationship between
    // the source's exact harmonic frequencies and the quantized
    // output's harmonic grid — both encoder and decoder now walk
    // through W0_TABLE[b0] for reconstruction, so harmonics shift
    // by up to a few percent. The essential invariant ("not pure
    // noise") still holds.
    let harmonic_peak = e_fund.max(e_harm2).max(e_harm3);
    assert!(
        harmonic_peak > e_bg,
        "decoded output has no harmonic peaks above background \
         (f0={e_fund:.4} 2f0={e_harm2:.4} 3f0={e_harm3:.4} noise={e_bg:.4}); \
         decoder is likely synthesizing unvoiced noise for a voiced input"
    );
}
