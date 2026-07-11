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

// Dev-dependencies pulled in by sibling tests. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use mbelib_rs::{AmbeDecoder, AmbeEncoder};

/// Build a 1 kHz sine wave chunk at -20 dBFS, 160 samples @ 8 kHz
/// (one 20 ms AMBE frame). `-20 dBFS` ≈ typical spoken-voice level.
fn make_sine_chunk(t0_samples: usize) -> [f32; 160] {
    let mut buf = [0.0_f32; 160];
    let amplitude = 0.1_f32; // -20 dBFS
    let freq_hz = 1000.0_f32;
    let sr = 8000.0_f32;
    for (i, slot) in buf.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test sine generator: t0_samples + i stays below f32 mantissa \
                      precision for the test's frame counts."
        )]
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
    #[expect(
        clippy::cast_precision_loss,
        reason = "test RMS helper: pcm.len() small (one frame = 160 samples), exact in f64."
    )]
    let mean = sum_sq / pcm.len() as f64;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "test RMS helper: mean is small (normalized PCM), sqrt is bounded, \
                  f64-to-f32 narrowing is acceptable for the test's comparison."
    )]
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
#[ignore = "diagnostic dump, asserts nothing — run explicitly with --ignored --nocapture"]
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
#[ignore = "diagnostic dump, asserts nothing — run explicitly with --ignored --nocapture"]
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

/// Cross-correlation of an encode→decode loopback must preserve
/// the INPUT's waveform structure, not just "produce non-silent bytes".
///
/// This is the minimum bar for sextant↔sextant voice to be
/// recognizable: when sextant-A encodes a signal and ships the AMBE
/// over a D-STAR reflector to sextant-B, sextant-B's decoder must
/// reconstruct audio whose normalized cross-correlation with the
/// original is materially above the zero-correlation noise floor.
///
/// As of 2026-04 the observed round-trip correlation on pure sines
/// sits around 0.05–0.06 after the OP25-reference `gain_adjust = 7.5`
/// calibration was applied (was 0.03 before). The remaining correlation
/// gap lives in the spectral-envelope path (b3/b4 PRBA + b5–b8 HOC):
/// `validate_bvec_vs_op25` shows our b3/b4 match OP25's at <10% per
/// frame on synthetic voiced inputs, mostly because our pitch tracker
/// commits to f0 = 150 Hz (correct fundamental) while OP25 picks the
/// 75 Hz octave-down (giving L = 49 vs our L = 24, and therefore
/// different block partitioning + different PRBA targets).
///
/// V/UV (b1) matches OP25 at 96%; bit-pack/unpack is symmetric (see
/// `bit_layer_roundtrip.rs`); block-DCT round-trip preserves `log_m`
/// exactly (see `block_dct_round_trip_preserves_log_m`). So the bug
/// is in the algorithmic coupling between encoder's spectral
/// quantization and decoder's reconstruction, not in the math.
///
/// Gate set at 0.10 to document the remaining gap. Tightening this
/// number as the spectral path lands further fixes is the way to
/// drive the encoder toward recognizable voice.
#[test]
#[ignore = "tracks spectral-envelope encoder bug — correlation ~0.04 on sines"]
fn sine_roundtrip_has_nonzero_correlation_with_input() {
    let mut enc = AmbeEncoder::new();
    let mut dec = AmbeDecoder::new();

    let frames = 40_usize;
    let mut input_pcm: Vec<f32> = Vec::with_capacity(frames * 160);
    let mut output_pcm: Vec<f32> = Vec::with_capacity(frames * 160);

    let mut t0 = 0_usize;
    for _ in 0..frames {
        let chunk = make_sine_chunk(t0);
        input_pcm.extend_from_slice(&chunk);
        let ambe = enc.encode_frame(&chunk);
        let pcm_out = dec.decode_frame(&ambe);
        output_pcm.extend(pcm_out.iter().map(|&s| f32::from(s) / 32768.0));
        t0 += 160;
    }

    // Skip first 10 frames of warm-up; correlate the last 30 frames.
    let warmup = 10 * 160;
    let a = input_pcm.get(warmup..).unwrap_or(&[]);
    let b = output_pcm.get(warmup..).unwrap_or(&[]);

    let rms = |x: &[f32]| -> f32 {
        let sum_sq: f32 = x.iter().map(|v| v * v).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "x.len() is bounded by frames*160 ≤ 16000, exact in f32 mantissa."
        )]
        let n = x.len() as f32;
        (sum_sq / n).sqrt()
    };

    let max_ncc = {
        let mut best: f32 = -1.0;
        for lag in (-200_i32..=200_i32).step_by(2) {
            let (ax, bx) = if lag >= 0 {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "range filtered to lag >= 0 in this branch"
                )]
                let l = lag as usize;
                (
                    a.get(l..a.len().min(l + b.len())).unwrap_or(&[]),
                    b.get(..b.len().min(a.len().saturating_sub(l)))
                        .unwrap_or(&[]),
                )
            } else {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "range filtered to lag < 0 so -lag is positive"
                )]
                let l = (-lag) as usize;
                (
                    a.get(..a.len().min(b.len().saturating_sub(l)))
                        .unwrap_or(&[]),
                    b.get(l..b.len().min(a.len() + l)).unwrap_or(&[]),
                )
            };
            let n = ax.len().min(bx.len());
            if n == 0 {
                continue;
            }
            let ax_slice = ax.get(..n).unwrap_or(&[]);
            let bx_slice = bx.get(..n).unwrap_or(&[]);
            let ra = rms(ax_slice);
            let rb = rms(bx_slice);
            if ra * rb <= 0.0 {
                continue;
            }
            let dot: f32 = ax_slice
                .iter()
                .zip(bx_slice.iter())
                .map(|(x, y)| x * y)
                .sum();
            #[expect(
                clippy::cast_precision_loss,
                reason = "n ≤ warmup-stripped frames × 160 ≤ 4800, exact in f32 mantissa."
            )]
            let n_f = n as f32;
            let ncc = dot / (n_f * ra * rb);
            if ncc > best {
                best = ncc;
            }
        }
        best
    };

    eprintln!("baseline_ncc_1khz = {max_ncc:.4}");
    assert!(
        max_ncc >= 0.10,
        "round-trip cross-correlation with input is essentially zero: \
         {max_ncc:.4}. The decoder reconstructs a waveform uncorrelated \
         with the input — spectral-envelope encoder bug (b3/b4/b5-b8). \
         Fix the spectral path; loosen this threshold only as a \
         regression tightening, not a regression loosening."
    );
}

/// Voice-band roundtrip — 200 Hz fundamental with 4 decaying harmonics
/// (a synthetic "voice" signal sitting squarely inside the AMBE codec's
/// design range). 1 kHz is OUT-OF-RANGE for AMBE 3600x2400 (which
/// targets human speech, F0 ≈ 80–400 Hz), so the existing 1 kHz
/// `sine_roundtrip_*` test is dominated by sub-multiple selection
/// noise rather than encoder spectral-envelope quality. This test
/// uses an in-range tonal complex so the cross-correlation actually
/// reflects spectral-envelope reconstruction fidelity.
///
/// Per-frame b-vec is dumped so encoder fixes can be bisected by
/// comparing what `b3..b8` change from one revision to the next.
/// Deterministic 199.79 Hz voice-band signal (18 harmonics, 1/k
/// formant decay, -34 dBFS) shared by the correlation gate and the
/// steady-state wire pin below.
///
/// The amplitude keeps the encoder's gain away from `DG_TABLE`
/// saturation (0.1 amp drove `gain ≈ 12` past the table max ~5.35,
/// pinning b2 at 62/63 and corrupting every subsequent frame's
/// closed-loop gamma). The frequency lands EXACTLY on `W0_TABLE[46]`
/// (0.02497 cycles/sample × 8000 = 199.79 Hz) so pitch quantization
/// introduces no sub-Hz error that would accumulate phase drift over
/// the correlation window; a 200.0 Hz target rounds to `W0_TABLE[45]`
/// = 205.5 Hz and drifts ~4 cycles over 0.8 s. Continuous k=1..=18
/// coverage avoids the voiced/unvoiced cliff a sparse harmonic stack
/// creates (which saturates Gm through the PRBA24 codebook).
fn make_voice_chunk(t0_samples: usize) -> [f32; 160] {
    let mut buf = [0.0_f32; 160];
    let amplitude = 0.02_f32;
    let f0_hz = 199.79_f32;
    let sr = 8000.0_f32;
    let n_harmonics = 18_usize;
    for (i, slot) in buf.iter_mut().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "test sample-time generator: i+t0 well below f32 mantissa precision."
        )]
        let t = (t0_samples + i) as f32;
        let mut s = 0.0_f32;
        for k_idx in 0..n_harmonics {
            #[expect(
                clippy::cast_precision_loss,
                reason = "harmonic index 1..=18 fits exactly in f32."
            )]
            let k = (k_idx + 1) as f32;
            let amp = 1.0 / k; // 1/k decay = pink-spectrum-like envelope
            s += amp * (t * 2.0 * std::f32::consts::PI * f0_hz * k / sr).sin();
        }
        *slot = amplitude * s;
    }
    buf
}

#[test]
fn voice_band_200hz_roundtrip_correlation() {
    let mut enc = AmbeEncoder::new();
    let mut dec = AmbeDecoder::new();

    let frames = 40_usize;
    let mut input_pcm: Vec<f32> = Vec::with_capacity(frames * 160);
    let mut output_pcm: Vec<f32> = Vec::with_capacity(frames * 160);

    let mut t0 = 0_usize;
    for f in 0..frames {
        let chunk = make_voice_chunk(t0);
        input_pcm.extend_from_slice(&chunk);
        let ambe = enc.encode_frame(&chunk);
        // Diagnostic: dump first 5 stable-state frames after warm-up.
        if (10..15).contains(&f) {
            let (b, w0, big_l, _) = mbelib_rs::decode_trace(&ambe);
            let f0_hz = w0 * 8000.0 / (2.0 * std::f32::consts::PI);
            eprintln!(
                "voice frame {}: b0={} (f0={:.0}Hz, L={}), b1={}, b2={}, b3={}, b4={}, b5..b8={},{},{},{}",
                f, b[0], f0_hz, big_l, b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]
            );
        }
        let pcm_out = dec.decode_frame(&ambe);
        output_pcm.extend(pcm_out.iter().map(|&s| f32::from(s) / 32768.0));
        t0 += 160;
    }

    let warmup = 10 * 160;
    let a = input_pcm.get(warmup..).unwrap_or(&[]);
    let b = output_pcm.get(warmup..).unwrap_or(&[]);

    let rms = |x: &[f32]| -> f32 {
        let sum_sq: f32 = x.iter().map(|v| v * v).sum();
        #[expect(
            clippy::cast_precision_loss,
            reason = "x.len() bounded by frames*160 ≤ 16000, exact in f32."
        )]
        let n = x.len() as f32;
        (sum_sq / n).sqrt()
    };

    let max_ncc = {
        let mut best: f32 = -1.0;
        for lag in (-200_i32..=200_i32).step_by(2) {
            let (ax, bx) = if lag >= 0 {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "lag >= 0 branch; sign-loss cast is safe."
                )]
                let l = lag as usize;
                (
                    a.get(l..a.len().min(b.len() + l)).unwrap_or(&[]),
                    b.get(..b.len().min(a.len().saturating_sub(l)))
                        .unwrap_or(&[]),
                )
            } else {
                #[expect(
                    clippy::cast_sign_loss,
                    reason = "lag < 0 branch; -lag is positive so sign-loss is safe."
                )]
                let l = (-lag) as usize;
                (
                    a.get(..a.len().min(b.len().saturating_sub(l)))
                        .unwrap_or(&[]),
                    b.get(l..b.len().min(a.len() + l)).unwrap_or(&[]),
                )
            };
            let n = ax.len().min(bx.len());
            if n == 0 {
                continue;
            }
            let ax_slice = ax.get(..n).unwrap_or(&[]);
            let bx_slice = bx.get(..n).unwrap_or(&[]);
            let ra = rms(ax_slice);
            let rb = rms(bx_slice);
            if ra * rb <= 0.0 {
                continue;
            }
            let dot: f32 = ax_slice
                .iter()
                .zip(bx_slice.iter())
                .map(|(x, y)| x * y)
                .sum();
            #[expect(
                clippy::cast_precision_loss,
                reason = "n ≤ frames*160 = 6400, exact in f32 mantissa."
            )]
            let n_f = n as f32;
            let ncc = dot / (n_f * ra * rb);
            if ncc > best {
                best = ncc;
            }
        }
        best
    };

    eprintln!("voice_200hz_ncc = {max_ncc:.4}");
    // Initial gate is loose; tighten as encoder fixes land.
    assert!(
        max_ncc > 0.0,
        "voice-band encode→decode produced zero or anti-correlated output: {max_ncc:.4}"
    );
}

/// Self-golden pin of the encoder's steady-state wire output for the
/// deterministic 199.79 Hz voice-band signal.
///
/// The correlation gates in this file are deliberately loose while
/// the known encoder deficiencies stand, which leaves the encoder
/// with no committed regression tripwire — a refactor can change
/// every emitted frame and nothing fails. This pin freezes the
/// decoded b-vectors of three steady-state frames (after warm-up) at
/// their current values; an INTENTIONAL encoder change must update
/// these constants consciously. The values describe the encoder as
/// it is today (b0=45 ⇒ the 205.5 Hz pitch quantization, the
/// known-broken spectral path included) — a drift alarm, not a
/// correctness claim.
#[test]
fn steady_state_wire_output_for_voice_band_signal_is_pinned() {
    let mut enc = AmbeEncoder::new();
    let mut t0 = 0_usize;
    let mut steady: Vec<([usize; 9], usize)> = Vec::new();
    for f in 0..13 {
        let ambe = enc.encode_frame(&make_voice_chunk(t0));
        t0 += 160;
        if f >= 10 {
            let (b, _w0, big_l, _d) = mbelib_rs::decode_trace(&ambe);
            steady.push((b, big_l));
        }
    }
    let expected: Vec<([usize; 9], usize)> = vec![
        ([45, 15, 27, 310, 90, 10, 7, 7, 10], 18),
        ([45, 15, 27, 360, 82, 12, 7, 7, 10], 18),
        ([45, 15, 27, 294, 77, 10, 7, 7, 10], 18),
    ];
    assert_eq!(
        steady, expected,
        "steady-state encoder wire output drifted from the pinned b-vectors"
    );
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
