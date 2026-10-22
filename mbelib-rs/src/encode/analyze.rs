// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/encode.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Front-end analysis: PCM frame → (pitch-buffers updated, FFT output).
//!
//! Port of the signal-conditioning and FFT section of
//! `imbe_vocoder::encode()` in OP25. Accepts one 160-sample PCM frame
//! at 8 kHz, normalized to inclusive `[-1, 1]`, and produces:
//!
//! 1. updated pitch-history buffers (DC-removed in `pitch_ref_buf`,
//!    LPF'd in `pitch_est_buf`), ready for downstream pitch estimation
//! 2. a 256-point complex FFT of the windowed signal, centered on
//!    `pitch_ref_buf[150]`, ready for pitch refinement and spectral
//!    amplitude extraction
//!
//! Pitch estimation consumes the updated history; voicing and magnitude
//! extraction consume the returned spectrum.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

use super::stage::{SPECTRUM_BINS, Spectrum};
use crate::PcmFrame;
use crate::encode::dc_rmv::dc_rmv;
use crate::encode::pe_lpf::pe_lpf;
use crate::encode::state::{EncoderBuffers, FFT_LENGTH, FRAME, InputFilter, PITCH_EST_BUF_SIZE};
use crate::encode::window::WR_HALF;

/// Per-stream FFT planning cache.
///
/// Each encoder retains its plans and reusable scratch across frames.
/// The cache implements `Send` and `Sync`; [`analyze_frame`] requires
/// exclusive access while updating it.
pub struct FftPlan {
    fft: std::sync::Arc<dyn realfft::RealToComplex<f32>>,
    /// Scratch allocated when the fixed-size plan is constructed.
    scratch: Vec<Complex<f32>>,
    input: [f32; FFT_LENGTH],
}

impl std::fmt::Debug for FftPlan {
    // Identify the planner type and report the reusable scratch length.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FftPlan")
            .field("fft_length", &FFT_LENGTH)
            .field("scratch_len", &self.scratch.len())
            .finish_non_exhaustive()
    }
}

impl FftPlan {
    /// Construct the fixed 256-point FFT plan and allocate its scratch once.
    #[must_use]
    pub fn new() -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_LENGTH);
        let scratch = fft.make_scratch_vec();
        Self {
            fft,
            scratch,
            input: [0.0; FFT_LENGTH],
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
/// - `snd` is one checked finite frame in inclusive `[-1,1]`.
/// - `bufs` is the per-stream working state; buffers are shifted and
///   updated in place.
/// - `plan` carries the realfft plan cache.
/// - The returned spectrum contains `FFT_LENGTH / 2 + 1`
///   = 129. Realfft returns only the non-redundant half of the
///   Hermitian-symmetric complex FFT; for the purposes of pitch
///   refinement and spectral amplitude extraction that's sufficient.
///
/// Processing reuses the plan's input/scratch storage and allocates no heap
/// buffer. See the [checked PCM workflow](crate#checked-pcm).
#[must_use]
pub fn analyze_frame(snd: &PcmFrame, bufs: &mut EncoderBuffers, plan: &mut FftPlan) -> Spectrum {
    let mut output = [Complex::new(0.0, 0.0); SPECTRUM_BINS];
    analyze_kernel(snd.samples(), bufs, plan, &mut output);
    Spectrum(output)
}

pub(crate) fn analyze_kernel(
    snd: &[f32; FRAME],
    bufs: &mut EncoderBuffers,
    plan: &mut FftPlan,
    fft_out: &mut [Complex<f32>; SPECTRUM_BINS],
) {
    // Step 1: slide both pitch-history buffers one frame left to make
    // room for the new samples at the tail.
    bufs.shift_pitch_history();

    // Input conditioning uses the explicitly selected coefficient path.
    let tail_start = PITCH_EST_BUF_SIZE - FRAME;
    let frame = snd;
    let (prefix, tail) = bufs.pitch_ref_buf.split_at_mut(tail_start);
    let _ = prefix; // keeping older history intact
    match bufs.input_filter {
        InputFilter::Reference => dc_rmv(frame, tail, &mut bufs.dc_rmv_mem),
        #[cfg(feature = "kenwood-tables")]
        InputFilter::Kenwood => crate::encode::kenwood::filter::biquad_df1_section(
            &crate::encode::kenwood::biquads::HPF_345HZ_COEFFS,
            frame,
            tail,
            &mut bufs.kenwood_hpf_mem,
        ),
    }

    // Step 3: apply the pitch-estimation LPF from pitch_ref_buf into
    // pitch_est_buf, borrowing the distinct history fields directly.
    let dc_removed_tail = bufs.pitch_ref_buf.get(tail_start..).unwrap_or_default();
    let (_, est_tail) = bufs.pitch_est_buf.split_at_mut(tail_start);
    pe_lpf(dc_removed_tail, est_tail, &mut bufs.pe_lpf_mem);

    // Step 4: build a 256-sample real-input buffer with Yazev's
    // scatter layout: windowed signal at indices [146..256] and
    // [0, 1..111], zeros at [111..146]. This places the center of the
    // 221-sample analysis window at index 0 of the FFT input (time
    // domain) so the spectrum comes out with zero group delay for the
    // center sample.
    plan.input.fill(0.0);
    let real_buf = &mut plan.input;
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

    // Step 5: forward FFT using the plan and scratch allocated at construction.
    // realfft returns an error on length mismatch; our lengths are
    // invariant so we handle it by zeroing output on any unexpected
    // failure rather than propagating.
    let result = plan
        .fft
        .process_with_scratch(real_buf, fft_out, &mut plan.scratch);
    if result.is_err() {
        for bin in fft_out.iter_mut() {
            *bin = Complex::new(0.0, 0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FFT_LENGTH, FRAME, FftPlan, analyze_kernel as analyze_frame};
    use crate::encode::state::EncoderBuffers;
    use realfft::num_complex::Complex;

    fn fresh_state() -> (EncoderBuffers, FftPlan, [Complex<f32>; 129]) {
        (
            EncoderBuffers::new(),
            FftPlan::new(),
            [Complex::new(0.0, 0.0); FFT_LENGTH / 2 + 1],
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
    /// stabilizes. The sine is fed for several frames to let both
    /// the DC-remover and the LPF settle.
    #[test]
    fn sine_input_peaks_near_expected_bin() {
        let (mut bufs, mut plan, mut fft_out) = fresh_state();
        let freq_hz = 500.0_f32;
        let sample_rate = 8000.0_f32;
        // Run 5 frames to let transients settle.
        for frame_idx in 0..5 {
            let snd: [f32; FRAME] = std::array::from_fn(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test tone generator: frame_idx * FRAME + i stays under \
                                  5 * 160 = 800, far below f32 mantissa precision."
                )]
                let t = (frame_idx * FRAME + i) as f32;
                (t * 2.0 * std::f32::consts::PI * freq_hz / sample_rate).sin()
            });
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
