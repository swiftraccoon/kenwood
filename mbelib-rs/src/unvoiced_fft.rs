// SPDX-FileCopyrightText: 2025 arancormonk (mbelib-neo)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! FFT-based unvoiced speech synthesis (JMBE algorithms #117-126).
//!
//! Replaces mbelib's per-band oscillator-bank multisine with a single
//! 256-point FFT pass that processes all unvoiced bands of a frame
//! simultaneously, followed by a Weighted Overlap-Add (WOLA) combine
//! with the previous frame's output. The audio quality is noticeably
//! cleaner — less "buzzy" — and the per-frame compute is substantially
//! lower (one FFT + one IFFT vs L × 160 × `UV_QUALITY` oscillator
//! evaluations).
//!
//! # Algorithm sketch
//!
//! 1. Generate a 256-sample white noise buffer using a JMBE-compatible
//!    LCG (`x' = 171·x + 11213 mod 53125`), with the first 96 samples
//!    inherited from the previous frame to maintain continuity.
//! 2. Multiply by the 211-element trapezoidal synthesis window (zero
//!    outside center ±105 samples).
//! 3. Forward 256-point real FFT.
//! 4. For each unvoiced band `l`, compute the bin range `[a_min, b_max)`
//!    that covers `(l ± 0.5)·w0`, then derive a per-band scale factor
//!    that normalizes the band's RMS to the desired magnitude `Ml`.
//! 5. Apply the scale factor to all bins in each unvoiced band's range.
//!    Voiced bands' bins are zeroed.
//! 6. Inverse FFT, scaled by `1/N` to undo realfft's scaling convention.
//! 7. WOLA-combine the resulting 256-sample buffer with the previous
//!    frame's stored output (`previous_uw`), producing a 160-sample
//!    contribution that is added to the output buffer.
//!
//! Algorithm port: arancormonk/mbelib-neo (`mbe_unvoiced_fft.c`,
//! GPL-2.0-or-later). The underlying JMBE algorithm numbers refer to
//! the IMBE/AMBE specification documentation.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::cell::RefCell;
use std::sync::Arc;

use crate::params::MbeParams;

/// FFT size used for unvoiced synthesis.
pub(crate) const FFT_SIZE: usize = 256;

/// Number of complex bins produced by the 256-point real FFT (N/2 + 1).
const FFT_BINS: usize = FFT_SIZE / 2 + 1;

/// Frame length in samples (8 kHz × 20 ms).
const FRAME_LEN: usize = 160;

/// Number of overlap samples carried between consecutive noise frames.
const NOISE_OVERLAP: usize = 96;

/// JMBE LCG multiplier coefficient.
const LCG_A: u32 = 171;
/// JMBE LCG additive coefficient.
const LCG_B: u32 = 11213;
/// JMBE LCG modulus (the period of the noise sequence).
const LCG_M: u32 = 53125;
/// JMBE default LCG seed used after cold-start sentinel.
const LCG_DEFAULT_SEED: f32 = 3147.0;

/// Algorithm #120 unvoiced amplitude scaling coefficient.
const UNVOICED_SCALE_COEFF: f32 = 146.176_96;

/// Precomputed `256 / (2π)` for converting harmonic frequencies (radians
/// per sample) into FFT bin indices.
#[expect(
    clippy::cast_precision_loss,
    reason = "FFT_SIZE is 256; representable exactly in f32"
)]
const BIN_FREQ_MULT: f32 = (FFT_SIZE as f32) / (2.0 * std::f32::consts::PI);

/// 211-element trapezoidal synthesis window, indexed -105..=+105
/// (stored offset by +105). Linear ramp up over indices -105..=-56,
/// flat 1.0 region over -55..=+55, linear ramp down over +56..=+105.
#[rustfmt::skip]
static SYNTHESIS_WINDOW: [f32; 211] = [
    // -105..=-56 (ramp up: 0.000 to 0.980 in steps of 0.020)
    0.000, 0.020, 0.040, 0.060, 0.080, 0.100, 0.120, 0.140, 0.160, 0.180,
    0.200, 0.220, 0.240, 0.260, 0.280, 0.300, 0.320, 0.340, 0.360, 0.380,
    0.400, 0.420, 0.440, 0.460, 0.480, 0.500, 0.520, 0.540, 0.560, 0.580,
    0.600, 0.620, 0.640, 0.660, 0.680, 0.700, 0.720, 0.740, 0.760, 0.780,
    0.800, 0.820, 0.840, 0.860, 0.880, 0.900, 0.920, 0.940, 0.960, 0.980,
    // -55..=+55 (flat 1.0, 111 values)
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000, 1.000,
    1.000,
    // +56..=+105 (ramp down: 0.980 to 0.000 in steps of 0.020)
    0.980, 0.960, 0.940, 0.920, 0.900, 0.880, 0.860, 0.840, 0.820, 0.800,
    0.780, 0.760, 0.740, 0.720, 0.700, 0.680, 0.660, 0.640, 0.620, 0.600,
    0.580, 0.560, 0.540, 0.520, 0.500, 0.480, 0.460, 0.440, 0.420, 0.400,
    0.380, 0.360, 0.340, 0.320, 0.300, 0.280, 0.260, 0.240, 0.220, 0.200,
    0.180, 0.160, 0.140, 0.120, 0.100, 0.080, 0.060, 0.040, 0.020, 0.000,
];

/// Returns the synthesis window value at integer offset `n` from the
/// frame center. Returns 0 outside `[-105, 105]`.
fn synthesis_window(n: i32) -> f32 {
    if !(-105..=105).contains(&n) {
        return 0.0;
    }
    #[expect(
        clippy::cast_sign_loss,
        reason = "n + 105 is in 0..=210 after the bounds check above"
    )]
    let idx = (n + 105) as usize;
    SYNTHESIS_WINDOW.get(idx).copied().unwrap_or(0.0)
}

/// Per-thread reusable FFT plan + scratch buffers + precomputed tables.
///
/// The plan has no per-stream state: only the realfft planners (which
/// are read-only after construction), scratch buffers, and precomputed
/// tables that depend only on FFT size and frame length. One plan can
/// be shared across all decoders running on the same thread, which is
/// why it lives in a thread-local rather than on `AmbeDecoder`.
struct FftPlan {
    forward: Arc<dyn RealToComplex<f32>>,
    inverse: Arc<dyn ComplexToReal<f32>>,
    /// Forward-FFT input scratch (windowed noise).
    fft_in: Vec<f32>,
    /// Forward-FFT output (and IFFT input) scratch.
    fft_spec: Vec<Complex<f32>>,
    /// Inverse-FFT output scratch.
    fft_out: Vec<f32>,
    /// Forward-FFT working memory.
    fwd_scratch: Vec<Complex<f32>>,
    /// Inverse-FFT working memory.
    inv_scratch: Vec<Complex<f32>>,
    /// Pre-built 256-element synthesis window aligned to the FFT input
    /// indexing convention (center at index 128).
    fft_window: [f32; FFT_SIZE],
    /// WOLA window for the previous frame at sample n: `w(n)`.
    wola_w_prev: [f32; FRAME_LEN],
    /// WOLA window for the current frame at sample n: `w(n - 160)`.
    wola_w_curr: [f32; FRAME_LEN],
    /// WOLA denominator: `w_prev² + w_curr²`.
    wola_denom: [f32; FRAME_LEN],
}

impl FftPlan {
    fn new() -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let forward = planner.plan_fft_forward(FFT_SIZE);
        let inverse = planner.plan_fft_inverse(FFT_SIZE);

        let fft_in = forward.make_input_vec();
        let fft_spec = forward.make_output_vec();
        let fft_out = inverse.make_output_vec();
        let fwd_scratch = forward.make_scratch_vec();
        let inv_scratch = inverse.make_scratch_vec();

        // Build the FFT-aligned synthesis window: center at index 128,
        // so FFT bin `i` gets weight `synthesis_window(i - 128)`.
        let mut fft_window = [0.0_f32; FFT_SIZE];
        for (i, slot) in fft_window.iter_mut().enumerate() {
            #[expect(
                clippy::cast_possible_wrap,
                clippy::cast_possible_truncation,
                reason = "i is at most FFT_SIZE-1=255; fits in i32"
            )]
            let win_idx = (i as i32) - 128;
            *slot = synthesis_window(win_idx);
        }

        // Precompute WOLA weights and denominator for each output sample.
        let mut wola_w_prev = [0.0_f32; FRAME_LEN];
        let mut wola_w_curr = [0.0_f32; FRAME_LEN];
        let mut wola_denom = [0.0_f32; FRAME_LEN];
        #[expect(
            clippy::cast_possible_wrap,
            clippy::cast_possible_truncation,
            reason = "FRAME_LEN is 160; fits in i32"
        )]
        let frame_len_i32 = FRAME_LEN as i32;
        #[expect(
            clippy::indexing_slicing,
            reason = "Filling three fixed-size WOLA buffers of exactly FRAME_LEN elements \
                      inside a bounded `0..FRAME_LEN` loop — indexing is always in-bounds \
                      by construction. Rewriting as a three-way zip would obscure the \
                      parallel per-sample write pattern that mirrors the WOLA reference \
                      algorithm."
        )]
        for n in 0..FRAME_LEN {
            #[expect(
                clippy::cast_possible_wrap,
                clippy::cast_possible_truncation,
                reason = "n is at most FRAME_LEN-1=159; fits in i32"
            )]
            let n_i32 = n as i32;
            let w_prev = synthesis_window(n_i32);
            let w_curr = synthesis_window(n_i32 - frame_len_i32);
            wola_w_prev[n] = w_prev;
            wola_w_curr[n] = w_curr;
            wola_denom[n] = w_prev.mul_add(w_prev, w_curr * w_curr);
        }

        Self {
            forward,
            inverse,
            fft_in,
            fft_spec,
            fft_out,
            fwd_scratch,
            inv_scratch,
            fft_window,
            wola_w_prev,
            wola_w_curr,
            wola_denom,
        }
    }
}

thread_local! {
    /// Per-thread FFT plan singleton. Lazily constructed on first use.
    static FFT_PLAN: RefCell<FftPlan> = RefCell::new(FftPlan::new());
}

/// Generates 256 samples of JMBE-compatible LCG noise into `buffer`,
/// reusing the previous frame's tail as overlap to maintain continuity.
///
/// The LCG state is encoded across two `MbeParams` fields:
/// - `noise_overlap[0..96]` holds samples 0..96 of the new buffer
///   (which were samples 160..256 of the previous frame's buffer)
/// - `noise_seed` holds the LCG state for generating samples 96..256
///
/// The cold-start sentinel (`noise_seed < 0.0`) emits an all-zero buffer
/// once and primes the state for normal operation on the next call.
fn generate_noise_with_overlap(buffer: &mut [f32; FFT_SIZE], params: &mut MbeParams) {
    if params.noise_seed < 0.0 {
        // Cold start: JMBE outputs an all-zero "current" buffer first.
        buffer.fill(0.0);
        params.noise_overlap.fill(0.0);
        params.noise_seed = LCG_DEFAULT_SEED;
        return;
    }

    // Prepend the previous tail, then generate the rest with the LCG.
    buffer[..NOISE_OVERLAP].copy_from_slice(&params.noise_overlap);

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "noise_seed is always in [0, 53124] after modulo by LCG_M"
    )]
    let mut state = (params.noise_seed as u32) % LCG_M;
    for sample in &mut buffer[NOISE_OVERLAP..] {
        #[expect(
            clippy::cast_precision_loss,
            reason = "state is at most 53124; no precision loss in f32"
        )]
        {
            *sample = state as f32;
        }
        state = (LCG_A.wrapping_mul(state).wrapping_add(LCG_B)) % LCG_M;
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "state is at most 53124; no precision loss in f32"
    )]
    {
        params.noise_seed = state as f32;
    }

    // Save the tail for next frame's overlap (samples 160..=255).
    params
        .noise_overlap
        .copy_from_slice(&buffer[FFT_SIZE - NOISE_OVERLAP..]);
}

/// FFT-based unvoiced synthesis for one frame.
///
/// Generates the unvoiced contribution for all unvoiced bands across the
/// previous→current frame transition and adds the result to `output`
/// (160 samples). Voiced bands' contributions must be added separately
/// by the per-band voiced synthesis.
///
/// `noise_buffer` is the pre-generated noise; passing it in (rather than
/// generating internally) lets the caller share the noise samples with
/// JMBE phase calculation (algorithm #140).
pub(crate) fn synthesize_unvoiced(
    output: &mut [f32; FRAME_LEN],
    cur: &mut MbeParams,
    prev: &MbeParams,
    noise_buffer: &[f32; FFT_SIZE],
) {
    FFT_PLAN.with_borrow_mut(|plan| {
        synthesize_unvoiced_with_plan(plan, output, cur, prev, noise_buffer);
    });
}

/// Generates the noise buffer for the current frame, returning a copy
/// that can be shared with the voiced phase calculation step.
pub(crate) fn make_noise_buffer(cur: &mut MbeParams) -> [f32; FFT_SIZE] {
    let mut buffer = [0.0_f32; FFT_SIZE];
    generate_noise_with_overlap(&mut buffer, cur);
    buffer
}

/// Computes the per-FFT-bin scaling factors that normalize each
/// unvoiced band's FFT magnitude to the band's commanded amplitude.
///
/// For unvoiced bands, the scaling factor is `UNVOICED_SCALE_COEFF * Ml`
/// divided by the RMS of the bins in the band's frequency range.
/// Voiced bands have scaling factor 0 (their bins are zeroed in the
/// caller's per-bin multiply step).
fn compute_unvoiced_band_scalors(
    bin_scalor: &mut [f32; FFT_BINS],
    fft_spec: &[Complex<f32>],
    cur: &MbeParams,
    big_l: usize,
    w0: f32,
) {
    let mult = BIN_FREQ_MULT * w0;
    for l in 1..=big_l.min(56) {
        if cur.vl.get(l).copied().unwrap_or(true) {
            continue; // Voiced band: stays zeroed.
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let l_f = l as f32;
        let a = ((l_f - 0.5) * mult).ceil();
        let b = ((l_f + 0.5) * mult).ceil();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a and b are clamped via min/max below"
        )]
        let a_min = (a.max(0.0) as usize).min(FFT_BINS - 1);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a and b are clamped via min/max below"
        )]
        let b_max = (b.max(0.0) as usize).min(FFT_BINS - 1);
        if b_max <= a_min {
            continue;
        }

        // Compute sum of |bin|² over the band's bin range.
        let mut numerator = 0.0_f32;
        for bin_idx in a_min..b_max {
            let bin = fft_spec.get(bin_idx).copied().unwrap_or_default();
            numerator = bin.re.mul_add(bin.re, numerator);
            numerator = bin.im.mul_add(bin.im, numerator);
        }

        if numerator <= 1e-10 {
            continue;
        }

        let bin_count = b_max - a_min;
        #[expect(
            clippy::cast_precision_loss,
            reason = "bin_count is at most 128; no precision loss in f32"
        )]
        let denom = bin_count as f32;
        let ml = cur.ml.get(l).copied().unwrap_or(0.0);
        let scalor = UNVOICED_SCALE_COEFF * ml / (numerator / denom).sqrt();

        for slot in bin_scalor.get_mut(a_min..b_max).into_iter().flatten() {
            *slot = scalor;
        }
    }
}

fn synthesize_unvoiced_with_plan(
    plan: &mut FftPlan,
    output: &mut [f32; FRAME_LEN],
    cur: &mut MbeParams,
    prev: &MbeParams,
    noise_buffer: &[f32; FFT_SIZE],
) {
    let big_l = cur.l;
    let w0 = cur.w0;

    // Apply synthesis window to noise buffer → FFT input.
    for (i, dst) in plan.fft_in.iter_mut().enumerate() {
        let src = noise_buffer.get(i).copied().unwrap_or(0.0);
        let win = plan.fft_window.get(i).copied().unwrap_or(0.0);
        *dst = src * win;
    }

    // Forward FFT: real input → 129 complex bins.
    let _ = plan.forward.process_with_scratch(
        &mut plan.fft_in,
        &mut plan.fft_spec,
        &mut plan.fwd_scratch,
    );

    // Compute per-band edge bins and per-band scale factors.
    // For unvoiced bands, scale = UNVOICED_SCALE_COEFF * Ml / sqrt(rms²).
    // For voiced bands, scale = 0 (zero out those bins).
    let mut bin_scalor = [0.0_f32; FFT_BINS];
    compute_unvoiced_band_scalors(&mut bin_scalor, &plan.fft_spec, cur, big_l, w0);

    // Apply per-bin scaling (voiced bands' bins are still 0 → zeroed).
    for (bin, &scale) in plan.fft_spec.iter_mut().zip(bin_scalor.iter()) {
        bin.re *= scale;
        bin.im *= scale;
    }

    // Inverse FFT.
    let _ = plan.inverse.process_with_scratch(
        &mut plan.fft_spec,
        &mut plan.fft_out,
        &mut plan.inv_scratch,
    );

    // Normalize: realfft's IFFT output is unscaled (IFFT(FFT(x)) = N·x).
    #[expect(
        clippy::cast_precision_loss,
        reason = "FFT_SIZE is 256; no precision loss in f32"
    )]
    let inv_n = 1.0_f32 / FFT_SIZE as f32;
    for sample in &mut plan.fft_out {
        *sample *= inv_n;
    }

    // WOLA combine: blend prev frame's stored output with current via
    // precomputed window weights. See mbe_wola_combine_fast for the
    // index arithmetic (prev offset +128, cur offset -32).
    for n in 0..FRAME_LEN {
        let mut prev_sample = 0.0_f32;
        let mut curr_sample = 0.0_f32;

        let prev_idx = n + 128;
        if prev_idx < FFT_SIZE {
            prev_sample = prev.previous_uw.get(prev_idx).copied().unwrap_or(0.0);
        }

        // curr_idx = n - 32 for FRAME_LEN=160; valid when n >= 32.
        if n >= 32 {
            let curr_idx = n - 32;
            if curr_idx < FFT_SIZE {
                curr_sample = plan.fft_out.get(curr_idx).copied().unwrap_or(0.0);
            }
        }

        let denom = plan.wola_denom.get(n).copied().unwrap_or(0.0);
        if denom > 1e-10 {
            let w_prev = plan.wola_w_prev.get(n).copied().unwrap_or(0.0);
            let w_curr = plan.wola_w_curr.get(n).copied().unwrap_or(0.0);
            let combined = w_prev.mul_add(prev_sample, w_curr * curr_sample) / denom;
            if let Some(slot) = output.get_mut(n) {
                *slot += combined;
            }
        }
    }

    // Save current IFFT output for next frame's WOLA.
    cur.previous_uw.copy_from_slice(&plan.fft_out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesis window is symmetric and bounded in [0, 1].
    #[test]
    fn synthesis_window_properties() {
        for n in -105..=105 {
            let w = synthesis_window(n);
            assert!((0.0..=1.0).contains(&w), "window({n}) = {w} out of range");
            // Symmetric around 0.
            let neg = synthesis_window(-n);
            assert!((w - neg).abs() < 1e-5, "asymmetry at n={n}: {w} vs {neg}");
        }
        // Out-of-range returns zero.
        assert!(synthesis_window(-106).abs() < 1e-9);
        assert!(synthesis_window(106).abs() < 1e-9);
        // Center is exactly 1.0.
        assert!((synthesis_window(0) - 1.0).abs() < 1e-6);
    }

    /// LCG noise generation cold-starts to all zeros, then produces
    /// non-trivial noise on subsequent calls.
    #[test]
    fn lcg_cold_start_then_active() {
        let mut params = MbeParams::new();
        assert!(
            (params.noise_seed - (-1.0)).abs() < 1e-9,
            "default seed should be cold-start"
        );

        // First call: all zeros, seed primed.
        let mut buf = [0.0_f32; FFT_SIZE];
        generate_noise_with_overlap(&mut buf, &mut params);
        assert!(
            buf.iter().all(|&x| x.abs() < 1e-9),
            "cold start should be silent"
        );
        assert!((params.noise_seed - LCG_DEFAULT_SEED).abs() < 1e-9);

        // Second call: actual noise.
        generate_noise_with_overlap(&mut buf, &mut params);
        let nonzero = buf.iter().filter(|&&x| x.abs() > 1e-9).count();
        assert!(nonzero > FFT_SIZE / 2, "expected mostly nonzero noise");
    }

    /// LCG output is deterministic given the same seed sequence.
    #[test]
    fn lcg_deterministic() {
        let mut p1 = MbeParams::new();
        let mut p2 = MbeParams::new();
        p1.noise_seed = 12345.0;
        p2.noise_seed = 12345.0;

        let mut b1 = [0.0_f32; FFT_SIZE];
        let mut b2 = [0.0_f32; FFT_SIZE];
        generate_noise_with_overlap(&mut b1, &mut p1);
        generate_noise_with_overlap(&mut b2, &mut p2);

        for (i, (&a, &b)) in b1.iter().zip(b2.iter()).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "sample {i}: {a} vs {b}");
        }
    }

    /// FFT plan constructor succeeds and produces buffers of the right size.
    #[test]
    fn fft_plan_construction() {
        FFT_PLAN.with_borrow(|plan| {
            assert_eq!(plan.fft_in.len(), FFT_SIZE);
            assert_eq!(plan.fft_spec.len(), FFT_BINS);
            assert_eq!(plan.fft_out.len(), FFT_SIZE);
        });
    }

    /// Synthesizing with all-voiced bands produces no output (all bins
    /// zeroed by the per-band loop).
    #[test]
    fn all_voiced_produces_no_unvoiced_contribution() {
        let mut cur = MbeParams::new();
        let prev = MbeParams::new();
        cur.l = 20;
        cur.w0 = 0.05;
        cur.noise_seed = 100.0; // skip cold start
        for l in 1..=cur.l {
            cur.vl[l] = true;
            cur.ml[l] = 1.0;
        }

        let noise = make_noise_buffer(&mut cur);
        let mut output = [0.5_f32; FRAME_LEN];
        let baseline = output;
        synthesize_unvoiced(&mut output, &mut cur, &prev, &noise);

        // Voiced bands → zeroed bins → IFFT produces ~zero → WOLA adds
        // ~zero to output. Output should be very close to baseline.
        for (i, (&new, &orig)) in output.iter().zip(baseline.iter()).enumerate() {
            let diff = (new - orig).abs();
            assert!(diff < 0.1, "sample {i}: diff {diff} should be small");
        }
    }
}
