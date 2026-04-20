// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/encode.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Front-end analysis: PCM frame → (pitch-buffers updated, FFT output).
//!
//! Port of the signal-conditioning and FFT section of
//! `imbe_vocoder::encode()` in OP25. Accepts one 160-sample PCM frame
//! (16 kHz-equivalent energy — i.e. i16 inputs normalized to `[-1,
//! 1)`) and produces:
//!
//! 1. updated pitch-history buffers (DC-removed in `pitch_ref_buf`,
//!    LPF'd in `pitch_est_buf`), ready for downstream pitch estimation
//! 2. a 256-point complex FFT of the windowed signal, centered on
//!    `pitch_ref_buf[150]`, ready for pitch refinement and spectral
//!    amplitude extraction
//!
//! The pitch-estimation step (`pitch_est()`) itself runs in phase P3
//! and consumes `pitch_est_buf`. Phase P4 consumes the FFT output.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

#[cfg(not(feature = "kenwood-tables"))]
use crate::encode::dc_rmv::dc_rmv;
use crate::encode::pe_lpf::pe_lpf;
use crate::encode::state::{EncoderBuffers, FFT_LENGTH, FRAME, PITCH_EST_BUF_SIZE};
use crate::encode::window::WR_HALF;

/// Per-stream FFT planning cache.
///
/// Plans are thread-bound in `realfft` — we keep one plan per
/// encoder instance rather than re-planning every frame. Each call
/// to [`analyze_frame`] borrows the planner through a `&mut`.
pub struct FftPlan {
    planner: RealFftPlanner<f32>,
    /// Scratch buffer the realfft plan uses internally. Sized by the
    /// plan on first use.
    scratch: Vec<Complex<f32>>,
}

impl std::fmt::Debug for FftPlan {
    // `RealFftPlanner` does not implement `Debug` (upstream choice —
    // the FFTW-style plans hold runtime codegen state). We print the
    // cached scratch size instead so debug output remains useful.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FftPlan")
            .field("planner", &"RealFftPlanner<f32> { .. }")
            .field("scratch_len", &self.scratch.len())
            .finish()
    }
}

impl FftPlan {
    /// Construct a fresh FFT plan cache. Cheap; the actual plan is
    /// built lazily on the first [`analyze_frame`] call.
    #[must_use]
    pub fn new() -> Self {
        Self {
            planner: RealFftPlanner::<f32>::new(),
            scratch: Vec::new(),
        }
    }
}

impl Default for FftPlan {
    fn default() -> Self {
        Self::new()
    }
}

/// Ingest one 160-sample frame and run the front-end analysis.
///
/// - `snd` is the new audio frame as f32 samples in nominal `[-1, 1)`.
///   Callers converting from i16 should divide by 32768.0.
/// - `bufs` is the per-stream working state; buffers are shifted and
///   updated in place.
/// - `plan` carries the realfft plan cache.
/// - `fft_out` receives a complex spectrum of length `FFT_LENGTH / 2 + 1`
///   = 129. Realfft returns only the non-redundant half of the
///   Hermitian-symmetric complex FFT; for the purposes of pitch
///   refinement and spectral amplitude extraction that's sufficient.
///
/// # Panics
///
/// Panics if `snd.len() < FRAME` or `fft_out.len() != FFT_LENGTH / 2 + 1`.
/// Both are caller contracts; the downstream encoder controls both.
pub fn analyze_frame(
    snd: &[f32],
    bufs: &mut EncoderBuffers,
    plan: &mut FftPlan,
    fft_out: &mut [Complex<f32>],
) {
    assert!(
        snd.len() >= FRAME,
        "analyze_frame: snd must contain at least FRAME samples",
    );
    assert_eq!(
        fft_out.len(),
        FFT_LENGTH / 2 + 1,
        "analyze_frame: fft_out must be FFT_LENGTH/2 + 1 complex bins",
    );

    // Step 1: slide both pitch-history buffers one frame left to make
    // room for the new samples at the tail.
    bufs.shift_pitch_history();

    // Step 2: input conditioning — remove DC (plus, when Kenwood
    // tables are enabled, everything below ≈345 Hz) and write the
    // result into `pitch_ref_buf`'s tail. The default path uses
    // OP25's 13 Hz first-order HPF; the `kenwood-tables` path uses
    // the radio's own 345 Hz biquad HPF (zeros on the unit circle
    // at DC, corner ≈345 Hz) which rejects rumble entirely before
    // pitch analysis sees it. Either way, the tail slice of
    // `pitch_ref_buf` is what downstream pitch refinement + FFT
    // stages read — swapping the filter doesn't change the data
    // flow, only the frequency response of the input.
    let tail_start = PITCH_EST_BUF_SIZE - FRAME;
    let (prefix, tail) = bufs.pitch_ref_buf.split_at_mut(tail_start);
    let _ = prefix; // keeping older history intact
    #[cfg(not(feature = "kenwood-tables"))]
    dc_rmv(&snd[..FRAME], tail, &mut bufs.dc_rmv_mem);
    #[cfg(feature = "kenwood-tables")]
    crate::encode::kenwood::filter::biquad_df1_section(
        &crate::encode::kenwood::biquads::HPF_345HZ_COEFFS,
        &snd[..FRAME],
        tail,
        &mut bufs.kenwood_hpf_mem,
    );

    // Step 3: apply the pitch-estimation LPF from pitch_ref_buf into
    // pitch_est_buf (both in the same tail slot). We can't borrow
    // `bufs.pitch_ref_buf` and `bufs.pitch_est_buf` mutably together,
    // so copy the DC-removed samples into a small scratch vec first.
    let dc_removed_tail: Vec<f32> = bufs
        .pitch_ref_buf
        .get(tail_start..)
        .map(<[f32]>::to_vec)
        .unwrap_or_default();
    let est_tail = bufs
        .pitch_est_buf
        .get_mut(tail_start..)
        .expect("tail slice in range");
    pe_lpf(&dc_removed_tail, est_tail, &mut bufs.pe_lpf_mem);

    // Step 4: build a 256-sample real-input buffer with Yazev's
    // scatter layout — windowed signal at indices [146..256] and
    // [0, 1..111], zeros at [111..146]. This places the center of the
    // 221-sample analysis window at index 0 of the FFT input (time
    // domain) so the spectrum comes out with zero group delay for the
    // center sample.
    let mut real_buf = vec![0.0_f32; FFT_LENGTH];
    // [146..256] ← pitch_ref_buf[40..150] × WR_HALF[0..110]
    for i in 0..110 {
        let sig = *bufs.pitch_ref_buf.get(40 + i).unwrap_or(&0.0);
        let w = *WR_HALF.get(i).unwrap_or(&0.0);
        if let Some(slot) = real_buf.get_mut(146 + i) {
            *slot = sig * w;
        }
    }
    // [0] ← pitch_ref_buf[150] (center sample, unwindowed)
    if let Some(slot) = real_buf.get_mut(0) {
        *slot = *bufs.pitch_ref_buf.get(150).unwrap_or(&0.0);
    }
    // [1..111] ← pitch_ref_buf[151..261] × WR_HALF[109..=0] (descending)
    for i in 0..110 {
        let sig = *bufs.pitch_ref_buf.get(151 + i).unwrap_or(&0.0);
        let w = *WR_HALF.get(109 - i).unwrap_or(&0.0);
        if let Some(slot) = real_buf.get_mut(1 + i) {
            *slot = sig * w;
        }
    }
    // [111..146] already zero.

    // Step 5: forward FFT. RealFftPlanner reuses a plan per length;
    // the first call per-stream is O(FFT_LENGTH log FFT_LENGTH)
    // planning cost, subsequent calls reuse.
    let fft = plan.planner.plan_fft_forward(FFT_LENGTH);
    let scratch_len = fft.get_scratch_len();
    if plan.scratch.len() < scratch_len {
        plan.scratch.resize(scratch_len, Complex::new(0.0, 0.0));
    }
    // realfft returns an error on length mismatch; our lengths are
    // invariant so we handle it by zeroing output on any unexpected
    // failure rather than propagating.
    let result = fft.process_with_scratch(&mut real_buf, fft_out, &mut plan.scratch);
    if result.is_err() {
        for bin in fft_out.iter_mut() {
            *bin = Complex::new(0.0, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FFT_LENGTH, FRAME, FftPlan, analyze_frame};
    use crate::encode::state::EncoderBuffers;
    use realfft::num_complex::Complex;

    fn fresh_state() -> (EncoderBuffers, FftPlan, Vec<Complex<f32>>) {
        (
            EncoderBuffers::new(),
            FftPlan::new(),
            vec![Complex::new(0.0, 0.0); FFT_LENGTH / 2 + 1],
        )
    }

    /// A zero input produces a zero spectrum.
    #[test]
    fn silent_input_produces_zero_spectrum() {
        let (mut bufs, mut plan, mut fft_out) = fresh_state();
        let snd = [0.0_f32; FRAME];
        analyze_frame(&snd, &mut bufs, &mut plan, &mut fft_out);
        for bin in &fft_out {
            assert!(bin.norm() < 1e-9, "nonzero bin {bin} for silent input",);
        }
    }

    /// A pure sine at 500 Hz sampled at 8 kHz (bin 16 in a 256-pt FFT
    /// at 8 kHz SR) should show a peak near bin 16 after the front-end
    /// stabilizes. We feed the sine for several frames to let both
    /// the DC-remover and the LPF settle.
    #[test]
    fn sine_input_peaks_near_expected_bin() {
        let (mut bufs, mut plan, mut fft_out) = fresh_state();
        let freq_hz = 500.0_f32;
        let sample_rate = 8000.0_f32;
        // Run 5 frames to let transients settle.
        for frame_idx in 0..5 {
            let snd: Vec<f32> = (0..FRAME)
                .map(|i| {
                    #[allow(clippy::cast_precision_loss)]
                    let t = (frame_idx * FRAME + i) as f32;
                    (t * 2.0 * std::f32::consts::PI * freq_hz / sample_rate).sin()
                })
                .collect();
            analyze_frame(&snd, &mut bufs, &mut plan, &mut fft_out);
        }

        // Bin index for 500 Hz at 8 kHz / 256-pt FFT: k = f * N / SR =
        // 500 * 256 / 8000 = 16.
        // The window spreads energy across neighboring bins, so we
        // accept a small cluster around bin 16.
        let target = 16;
        let mut max_bin = 0;
        let mut max_mag = 0.0_f32;
        for (i, bin) in fft_out.iter().enumerate() {
            let m = bin.norm();
            if m > max_mag {
                max_mag = m;
                max_bin = i;
            }
        }
        let delta = (i64::try_from(max_bin).unwrap_or(0) - target).abs();
        assert!(
            delta <= 2,
            "peak at bin {max_bin}, expected near {target} (mag {max_mag})",
        );
    }
}
