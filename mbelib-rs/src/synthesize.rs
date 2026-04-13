// SPDX-FileCopyrightText: 2010 szechyjs (mbelib)
// SPDX-FileCopyrightText: 2025 arancormonk (mbelib-neo)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Speech synthesis from decoded AMBE parameters.
//!
//! Converts decoded harmonic speech model parameters back into a PCM
//! audio waveform. There are three contributions to the output:
//!
//! 1. **Voiced bands** (per-band): a windowed cosine oscillator,
//!    cross-faded between previous and current frames using the `Ws`
//!    synthesis window. For low harmonics with stable pitch, JMBE
//!    phase/amplitude interpolation (algorithms #134-138) replaces the
//!    windowed-oscillator approach for smoother attack and reduced
//!    "buzziness."
//!
//! 2. **Unvoiced bands** (one FFT pass per frame): JMBE algorithms
//!    #117-126 use a 256-point FFT to scale white noise per harmonic
//!    band according to the band's magnitude, then WOLA-combine with
//!    the previous frame's stored output. Implemented in
//!    [`crate::unvoiced_fft`].
//!
//! 3. **Soft clipping** at frame end keeps the float-domain output in
//!    range so the float→i16 conversion path doesn't produce
//!    wrap-around artifacts.
//!
//! Base port from szechyjs/mbelib `mbe_synthesizeSpeechf` (originally
//! ISC). Restructuring for FFT-based unvoiced + JMBE phase/amplitude
//! interpolation adapted from arancormonk/mbelib-neo
//! (`mbe_synthesizeSpeechf` in `mbelib.c`, GPL-2.0-or-later).

use crate::math::CosOscillator;
use crate::params::MbeParams;
use crate::tables;
use crate::unvoiced_fft;

/// Number of PCM samples per AMBE frame (20 ms at 8000 Hz).
pub(crate) const FRAME_SAMPLES: usize = 160;

/// Soft-clipping threshold in float domain.
///
/// Matches mbelib-neo's `MBE_AUDIO_SOFT_CLIP_FLOAT`: 95% of i16 max
/// divided by the float→i16 gain factor of 7. Keeps the synthesized
/// signal within bounds the float→i16 path can convert without
/// wrap-around.
const SOFT_CLIP_FLOAT: f32 = (0.95 * 32_767.0) / 7.0;

/// JMBE Algorithm #140 noise-to-radians scaler: `2π / 53125`.
///
/// Maps an LCG noise sample (in `[0, 53125)`) into the range `[0, 2π)`,
/// which is then offset by `-π` to give phase jitter in `[-π, +π)`.
const NOISE_PHASE_SCALE: f32 = std::f32::consts::TAU / 53_125.0;

/// JMBE pitch-stability threshold for choosing the interpolation path.
///
/// When `|cw0 - pw0| < STABLE_PITCH_FRAC * cw0`, low harmonics use the
/// phase/amplitude interpolation path (algorithms #134-138) instead of
/// the windowed-oscillator approach.
const STABLE_PITCH_FRAC: f32 = 0.1;

/// JMBE harmonic count above which the windowed-oscillator approach is
/// always used (interpolation only applies to bands l < 8).
const INTERPOLATION_MAX_BAND: usize = 8;

/// Synthesizes one frame of PCM audio from decoded model parameters.
///
/// Generates 160 samples (20 ms at 8 kHz) using a hybrid approach:
/// per-band voiced contributions plus a single FFT-based unvoiced pass.
///
/// # Arguments
///
/// * `pcm` - Output buffer, filled with 160 float PCM samples.
/// * `cur` - Current frame's enhanced parameters. Modified in-place:
///   `psi_l`, `phi_l`, `previous_uw`, `noise_seed`, `noise_overlap`
///   are updated for next-frame state.
/// * `prev_enh` - Previous frame's enhanced parameters. Modified
///   in-place: `ml` and `vl` are extended when the current frame has
///   more bands; `psi_l` is wrapped to `[0, 2π)`.
pub(crate) fn synthesize_speech(
    pcm: &mut [f32; FRAME_SAMPLES],
    cur: &mut MbeParams,
    prev_enh: &mut MbeParams,
) {
    *pcm = [0.0_f32; FRAME_SAMPLES];

    let cw0 = cur.w0;
    let pw0 = prev_enh.w0;

    // Algorithm #117: generate one shared 256-sample noise buffer for
    // both the phase-jitter computation (algorithm #140) and the FFT-
    // based unvoiced synthesis below.
    let noise_buffer = unvoiced_fft::make_noise_buffer(cur);

    // Band extension (eq 128-129): pad the shorter frame with zero
    // magnitudes so per-band synthesis can iterate to max(L_prev, L_cur).
    let maxl = extend_bands(cur, prev_enh);

    // Count unvoiced bands (algorithm #140 uses this in the phase
    // jitter formula).
    let num_uv: usize = (1..=cur.l)
        .filter(|&l| !cur.vl.get(l).copied().unwrap_or(true))
        .count();

    // Update PSI (smooth phase) and PHI (jittered phase). Uses noise
    // buffer for jitter to match JMBE's algorithm #140.
    update_phases(cur, prev_enh, cw0, pw0, num_uv, &noise_buffer);

    // Per-band voiced synthesis. Unvoiced contributions are skipped
    // here — they are handled by the single FFT call after this loop.
    for l in 1..=maxl {
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let l_f32 = l as f32;
        let cw0l = cw0 * l_f32;
        let pw0l = pw0 * l_f32;

        let cur_voiced = cur.vl.get(l).copied().unwrap_or(true);
        let prev_voiced = prev_enh.vl.get(l).copied().unwrap_or(true);

        if !cur_voiced && !prev_voiced {
            continue; // FFT path handles all unvoiced bands.
        }

        let prev_ml = prev_enh.ml.get(l).copied().unwrap_or(0.0);
        let cur_ml = cur.ml.get(l).copied().unwrap_or(0.0);
        let prev_phi = prev_enh.phi_l.get(l).copied().unwrap_or(0.0);
        let cur_phi = cur.phi_l.get(l).copied().unwrap_or(0.0);

        // For low harmonics with stable pitch, use phase/amplitude
        // interpolation (algorithms #134-138). This produces smoother
        // attack and reduced "buzziness" compared to the windowed
        // oscillator approach.
        let stable_pitch = (cw0 - pw0).abs() < (STABLE_PITCH_FRAC * cw0);
        let use_interpolation =
            l < INTERPOLATION_MAX_BAND && cur_voiced && prev_voiced && stable_pitch;

        if use_interpolation {
            synth_interpolated(
                pcm, l_f32, pw0l, cw0, pw0, prev_ml, cur_ml, prev_phi, cur_phi,
            );
        } else if prev_voiced && cur_voiced {
            synth_voiced_overlap(pcm, prev_ml, cur_ml, prev_phi, cur_phi, pw0l, cw0l);
        } else if prev_voiced {
            synth_voiced_only_prev(pcm, prev_ml, prev_phi, pw0l);
        } else {
            // cur_voiced only
            synth_voiced_only_cur(pcm, cur_ml, cur_phi, cw0l);
        }
    }

    // Algorithms #117-126: FFT-based unvoiced synthesis, single pass
    // for all unvoiced bands across the previous→current transition.
    unvoiced_fft::synthesize_unvoiced(pcm, cur, prev_enh, &noise_buffer);

    // Soft clipping to keep float output within the float→i16 range.
    for sample in pcm.iter_mut() {
        *sample = sample.clamp(-SOFT_CLIP_FLOAT, SOFT_CLIP_FLOAT);
    }
}

/// Extends the shorter band array with zero-magnitude voiced bands.
///
/// When the current and previous frames have different harmonic counts,
/// the shorter one is padded so the cross-fade can operate across all
/// bands (equations 128-129 from the AMBE spec).
fn extend_bands(cur: &mut MbeParams, prev_enh: &mut MbeParams) -> usize {
    if cur.l > prev_enh.l {
        for l in (prev_enh.l + 1)..=cur.l {
            if let Some(slot) = prev_enh.ml.get_mut(l) {
                *slot = 0.0;
            }
            if let Some(slot) = prev_enh.vl.get_mut(l) {
                *slot = true;
            }
        }
        cur.l
    } else {
        for l in (cur.l + 1)..=prev_enh.l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot = 0.0;
            }
            if let Some(slot) = cur.vl.get_mut(l) {
                *slot = true;
            }
        }
        prev_enh.l
    }
}

/// Updates PSI (smooth predicted phase) and PHI (jittered phase).
///
/// Per JMBE algorithm #140, the phase jitter for bands above `L/4`
/// uses the shared noise buffer (sample `l`) mapped into `[-π, +π)`
/// rather than a separate PRNG, for consistent acoustic randomness
/// between phase calculation and unvoiced FFT synthesis.
///
/// PSI is wrapped to `[0, 2π)` on the previous frame before advancing,
/// matching JMBE's parity convention to keep the phase from growing
/// unboundedly.
fn update_phases(
    cur: &mut MbeParams,
    prev_enh: &mut MbeParams,
    cw0: f32,
    pw0: f32,
    num_uv: usize,
    noise_buffer: &[f32; unvoiced_fft::FFT_SIZE],
) {
    let two_pi = std::f32::consts::TAU;

    for l in 1..=56 {
        // Wrap previous PSI to [0, 2π) before advancing.
        let prev_psi_raw = prev_enh.psi_l.get(l).copied().unwrap_or(0.0);
        let mut prev_psi = prev_psi_raw % two_pi;
        if prev_psi < 0.0 {
            prev_psi += two_pi;
        }
        if let Some(slot) = prev_enh.psi_l.get_mut(l) {
            *slot = prev_psi;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "l and FRAME_SAMPLES are at most 160; product fits in f32"
        )]
        let half_phase = (pw0 + cw0) * (l as f32 * FRAME_SAMPLES as f32 / 2.0);
        let psi = prev_psi + half_phase;
        if let Some(slot) = cur.psi_l.get_mut(l) {
            *slot = psi;
        }

        // Algorithm #140: phase jitter from noise sample, scaled to
        // [-π, +π) and proportional to the unvoiced fraction.
        let phi = if l <= cur.l / 4 {
            psi
        } else if cur.l > 0 {
            let noise = noise_buffer.get(l).copied().unwrap_or(0.0);
            let pl = noise.mul_add(NOISE_PHASE_SCALE, -std::f32::consts::PI);
            #[expect(
                clippy::cast_precision_loss,
                reason = "num_uv and cur.l are at most 56; no precision loss"
            )]
            let scale = num_uv as f32 / cur.l as f32;
            scale.mul_add(pl, psi)
        } else {
            psi
        };
        if let Some(slot) = cur.phi_l.get_mut(l) {
            *slot = phi;
        }
    }
}

/// Helper: phase value of the current-frame oscillator at sample n=0.
///
/// The current-frame phase argument is `cw0l * (n - FRAME_SAMPLES) + cur_phi`,
/// which equals `(cur_phi - FRAME_SAMPLES*cw0l) + n*cw0l`. The
/// recurrence-based oscillator only needs the constant part as `phi_0`.
#[expect(
    clippy::cast_precision_loss,
    reason = "FRAME_SAMPLES is 160; no precision loss in f32"
)]
fn current_oscillator_phi_0(cw0l: f32, cur_phi: f32) -> f32 {
    (FRAME_SAMPLES as f32).mul_add(-cw0l, cur_phi)
}

/// Voiced overlap: both prev and cur frames are voiced for this band.
///
/// Sums two windowed cosine oscillators using `Ws` cross-fade. This is
/// the most common path during normal speech.
fn synth_voiced_overlap(
    pcm: &mut [f32; FRAME_SAMPLES],
    prev_ml: f32,
    cur_ml: f32,
    prev_phi: f32,
    cur_phi: f32,
    pw0l: f32,
    cw0l: f32,
) {
    let mut prev_osc = CosOscillator::new(prev_phi, pw0l);
    let mut cur_osc = CosOscillator::new(current_oscillator_phi_0(cw0l, cur_phi), cw0l);

    for n in 0..FRAME_SAMPLES {
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);
        let c1 = ws_prev * prev_ml * prev_osc.tick();
        let c2 = ws_cur * cur_ml * cur_osc.tick();
        if let Some(sample) = pcm.get_mut(n) {
            *sample += c1 + c2;
        }
    }
}

/// Voiced contribution from previous frame only (current is unvoiced).
///
/// The current-frame unvoiced contribution is added separately by the
/// FFT path. Here we only emit the previous voiced oscillator with the
/// ramp-down window.
fn synth_voiced_only_prev(pcm: &mut [f32; FRAME_SAMPLES], prev_ml: f32, prev_phi: f32, pw0l: f32) {
    let mut prev_osc = CosOscillator::new(prev_phi, pw0l);
    for n in 0..FRAME_SAMPLES {
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);
        let c = ws_prev * prev_ml * prev_osc.tick();
        if let Some(sample) = pcm.get_mut(n) {
            *sample += c;
        }
    }
}

/// Voiced contribution from current frame only (previous was unvoiced).
fn synth_voiced_only_cur(pcm: &mut [f32; FRAME_SAMPLES], cur_ml: f32, cur_phi: f32, cw0l: f32) {
    let mut cur_osc = CosOscillator::new(current_oscillator_phi_0(cw0l, cur_phi), cw0l);
    for n in 0..FRAME_SAMPLES {
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);
        let c = ws_cur * cur_ml * cur_osc.tick();
        if let Some(sample) = pcm.get_mut(n) {
            *sample += c;
        }
    }
}

/// Voiced phase/amplitude interpolation (JMBE algorithms #134-138).
///
/// For low harmonics (l < 8) with stable pitch, this produces smoother
/// transitions than the windowed-oscillator approach. The phase is
/// computed via quadratic frequency interpolation, and the amplitude
/// is linearly interpolated across the frame.
#[expect(clippy::too_many_arguments, reason = "tightly-coupled JMBE algorithm")]
fn synth_interpolated(
    pcm: &mut [f32; FRAME_SAMPLES],
    l_f32: f32,
    pw0l: f32,
    cw0: f32,
    pw0: f32,
    prev_ml: f32,
    cur_ml: f32,
    prev_phi: f32,
    cur_phi: f32,
) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "FRAME_SAMPLES is 160; no precision loss in f32"
    )]
    let n_f = FRAME_SAMPLES as f32;
    let two_pi = std::f32::consts::TAU;
    let pi = std::f32::consts::PI;

    // Algorithm #137: phase deviation at frame boundary.
    let deltaphil = (pw0 + cw0).mul_add(-(l_f32 * n_f / 2.0), cur_phi - prev_phi);

    // Algorithm #138: phase deviation rate, wrapped to [-π, π].
    let wrapped = two_pi.mul_add(-((deltaphil + pi) / two_pi).floor(), deltaphil);
    let deltawl = wrapped / n_f;

    let amp_step = (cur_ml - prev_ml) / n_f;
    let dw_quadratic_coeff = (cw0 - pw0) * l_f32 / (2.0 * n_f);
    let lin_coeff = pw0l + deltawl;

    for n in 0..FRAME_SAMPLES {
        #[expect(
            clippy::cast_precision_loss,
            reason = "n is at most 159; no precision loss"
        )]
        let n_f32 = n as f32;
        // Algorithm #136: quadratic phase interpolation.
        let theta = dw_quadratic_coeff.mul_add(n_f32 * n_f32, lin_coeff.mul_add(n_f32, prev_phi));
        // Algorithm #135: linear amplitude interpolation.
        let aln = amp_step.mul_add(n_f32, prev_ml);
        // Algorithm #134: synthesize sample (JMBE multiplies by 2.0).
        if let Some(sample) = pcm.get_mut(n) {
            *sample += 2.0 * aln * theta.cos();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::MbeParams;

    /// Silence parameters (all-zero magnitudes) should produce silence.
    #[test]
    fn silence_params_produce_silence() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];
        let mut cur = MbeParams::new();
        let mut prev = MbeParams::new();

        cur.l = 12;
        cur.w0 = 0.04;
        prev.l = 12;
        prev.w0 = 0.04;
        cur.noise_seed = 100.0; // skip cold-start

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        for (n, sample) in pcm.iter().enumerate() {
            assert!(
                sample.abs() < 0.1,
                "sample {n} should be near-zero for silence params, got {sample}"
            );
        }
    }

    /// Same input parameters always produce bit-identical output.
    #[test]
    fn deterministic_output() {
        let make_params = || {
            let mut p = MbeParams::new();
            p.l = 12;
            p.w0 = 0.04;
            p.noise_seed = 100.0;
            for l in 1..=p.l {
                p.ml[l] = 1.0;
                p.vl[l] = l <= 8;
            }
            p
        };

        let mut pcm1 = [0.0_f32; FRAME_SAMPLES];
        let mut cur1 = make_params();
        let mut prev1 = make_params();
        synthesize_speech(&mut pcm1, &mut cur1, &mut prev1);

        let mut pcm2 = [0.0_f32; FRAME_SAMPLES];
        let mut cur2 = make_params();
        let mut prev2 = make_params();
        synthesize_speech(&mut pcm2, &mut cur2, &mut prev2);

        for n in 0..FRAME_SAMPLES {
            let s1 = pcm1.get(n).copied().unwrap_or(f32::NAN);
            let s2 = pcm2.get(n).copied().unwrap_or(f32::NAN);
            assert_eq!(s1.to_bits(), s2.to_bits(), "sample {n}: {s1} vs {s2}");
        }
    }

    /// A single voiced band produces an oscillating waveform.
    #[test]
    fn single_voiced_band_produces_cosine() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 1;
        cur.w0 = 0.04;
        cur.ml[1] = 1.0;
        cur.vl[1] = true;
        cur.noise_seed = 100.0;

        let mut prev = MbeParams::new();
        prev.l = 1;
        prev.w0 = 0.04;
        prev.ml[1] = 1.0;
        prev.vl[1] = true;

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        let energy: f32 = pcm.iter().map(|s| s * s).sum();
        assert!(energy > 0.1, "energy={energy}");

        let has_pos = pcm.iter().any(|s| *s > 0.01);
        let has_neg = pcm.iter().any(|s| *s < -0.01);
        assert!(has_pos && has_neg);
    }

    /// All-unvoiced bands produce non-trivial output via the FFT path.
    #[test]
    fn unvoiced_band_produces_noise() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 10;
        cur.w0 = 0.03;
        cur.noise_seed = 100.0;
        for l in 1..=cur.l {
            cur.ml[l] = 1.0;
            cur.vl[l] = false;
        }

        let mut prev = MbeParams::new();
        prev.l = 10;
        prev.w0 = 0.03;
        for l in 1..=prev.l {
            prev.ml[l] = 1.0;
            prev.vl[l] = false;
        }

        // Run two frames so WOLA has previous-frame data to combine.
        synthesize_speech(&mut pcm, &mut cur, &mut prev);
        prev.copy_from(&cur);
        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        let energy: f32 = pcm.iter().map(|s| s * s).sum();
        assert!(energy > 0.001, "unvoiced output energy: {energy}");
    }

    /// Phase advance: PSI gets non-zero values after synthesis when w0 > 0.
    #[test]
    fn phase_updated_during_synthesis() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 5;
        cur.w0 = 0.04;
        cur.noise_seed = 100.0;
        for l in 1..=cur.l {
            cur.ml[l] = 1.0;
            cur.vl[l] = true;
        }

        let mut prev = MbeParams::new();
        prev.l = 5;
        prev.w0 = 0.04;

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        let psi_after = cur.psi_l.get(1).copied().unwrap_or(0.0);
        assert!(psi_after != 0.0, "PSI should advance: got {psi_after}");
    }

    /// Band extension: when cur.l > prev.l, prev is extended with zeros.
    #[test]
    fn band_extension_cur_longer() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 15;
        cur.w0 = 0.03;
        cur.noise_seed = 100.0;
        for l in 1..=cur.l {
            cur.ml[l] = 0.5;
            cur.vl[l] = true;
        }

        let mut prev = MbeParams::new();
        prev.l = 10;
        prev.w0 = 0.04;
        for l in 1..=prev.l {
            prev.ml[l] = 0.5;
            prev.vl[l] = true;
        }

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        for l in 11..=15 {
            let ml = prev.ml.get(l).copied().unwrap_or(f32::NAN);
            assert!(ml == 0.0, "extended band {l}: {ml}");
        }
    }

    /// Soft clipping caps the output at `SOFT_CLIP_FLOAT`.
    #[test]
    fn soft_clipping_bounds_output() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        // Drive the synthesis hard: all bands voiced, large magnitudes.
        let mut cur = MbeParams::new();
        cur.l = 50;
        cur.w0 = 0.02;
        cur.noise_seed = 100.0;
        for l in 1..=cur.l {
            cur.ml[l] = 1000.0; // very loud
            cur.vl[l] = true;
            cur.phi_l[l] = 0.0; // align phases for max amplitude
        }

        let mut prev = MbeParams::new();
        prev.l = 50;
        prev.w0 = 0.02;
        for l in 1..=prev.l {
            prev.ml[l] = 1000.0;
            prev.vl[l] = true;
            prev.phi_l[l] = 0.0;
        }

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        for (n, &s) in pcm.iter().enumerate() {
            assert!(
                s.abs() <= SOFT_CLIP_FLOAT + 1e-3,
                "sample {n} = {s} exceeds soft clip {SOFT_CLIP_FLOAT}"
            );
        }
    }
}
