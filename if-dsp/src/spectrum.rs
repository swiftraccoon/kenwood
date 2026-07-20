//! Spectrum estimation: Goertzel single-bin measurement and Welch
//! averaged periodograms.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner};

use crate::Complex32;

/// Amplitude of the `freq_hz` component of `samples` (Goertzel).
///
/// Returns the amplitude a pure tone at `freq_hz` would need to
/// produce the observed correlation — for a clean unit tone the result
/// is ~1.0.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "sample counts are far below 2^52 so usize -> f64 is exact \
              enough, and amplitudes are O(1) so f64 -> f32 narrowing is \
              lossless in practice"
)]
#[must_use]
pub fn goertzel(samples: &[f32], sample_rate: f32, freq_hz: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let omega = std::f64::consts::TAU * f64::from(freq_hz) / f64::from(sample_rate);
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &x in samples {
        let s0 = f64::from(x) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = (coeff * s1).mul_add(-s2, s1.mul_add(s1, s2 * s2));
    (2.0 * power.max(0.0).sqrt() / samples.len() as f64) as f32
}

/// Welch power spectral density estimator: Hann-windowed segments with
/// 50% overlap, averaged.
pub struct SpectrumEstimator {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    pending: Vec<f32>,
    scratch: Vec<Complex32>,
    acc: Vec<f32>,
    segments: usize,
    size: usize,
}

impl std::fmt::Debug for SpectrumEstimator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpectrumEstimator")
            .field("size", &self.size)
            .field("segments", &self.segments)
            .finish_non_exhaustive()
    }
}

impl SpectrumEstimator {
    /// Build an estimator with the given FFT size (power of two
    /// recommended).
    #[expect(
        clippy::cast_precision_loss,
        reason = "FFT sizes are small powers of two; usize -> f64 is exact"
    )]
    #[must_use]
    pub fn new(fft_size: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let window: Vec<f32> = (0..fft_size)
            .map(|n| {
                let x = std::f64::consts::TAU * n as f64 / fft_size as f64;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "window values are within [0, 1]"
                )]
                let w = 0.5_f64.mul_add(-x.cos(), 0.5) as f32;
                w
            })
            .collect();
        Self {
            fft,
            window,
            pending: Vec::new(),
            scratch: vec![Complex32::new(0.0, 0.0); fft_size],
            acc: vec![0.0; fft_size / 2 + 1],
            segments: 0,
            size: fft_size,
        }
    }

    /// Feed samples; whole segments (50% overlap) are consumed as they
    /// complete.
    pub fn feed(&mut self, samples: &[f32]) {
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= self.size {
            for (dst, (&s, &w)) in self
                .scratch
                .iter_mut()
                .zip(self.pending.iter().zip(self.window.iter()))
            {
                *dst = Complex32::new(s * w, 0.0);
            }
            self.fft.process(&mut self.scratch);
            for (a, x) in self.acc.iter_mut().zip(self.scratch.iter()) {
                *a += x.norm_sqr();
            }
            self.segments += 1;
            drop(self.pending.drain(..self.size / 2));
        }
    }

    /// Write the averaged one-sided PSD into `out` (cleared first).
    /// All zeros if nothing has been accumulated yet.
    #[expect(
        clippy::cast_precision_loss,
        reason = "segment counts are small; usize -> f32 is exact"
    )]
    pub fn write_psd(&self, out: &mut Vec<f32>) {
        out.clear();
        let norm = if self.segments == 0 {
            0.0
        } else {
            1.0 / (self.segments as f32)
        };
        out.extend(self.acc.iter().map(|&a| a * norm));
    }

    /// Frequency of the strongest bin, in hertz.
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin indices and FFT sizes are small; usize -> f32 is exact"
    )]
    #[must_use]
    pub fn peak_hz(&self, sample_rate: f32) -> f32 {
        let (idx, _) =
            self.acc.iter().enumerate().fold(
                (0, 0.0_f32),
                |(bi, bv), (i, &v)| if v > bv { (i, v) } else { (bi, bv) },
            );
        idx as f32 * sample_rate / self.size as f32
    }

    /// Discard all accumulated segments.
    pub fn reset(&mut self) {
        self.pending.clear();
        for a in &mut self.acc {
            *a = 0.0;
        }
        self.segments = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: f64, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| {
                #[expect(clippy::cast_precision_loss, reason = "small test lengths")]
                let ph = std::f64::consts::TAU * freq * n as f64 / rate;
                #[expect(clippy::cast_possible_truncation, reason = "unit tone samples")]
                let s = ph.sin() as f32;
                s
            })
            .collect()
    }

    #[test]
    fn goertzel_reads_unit_tone_amplitude() {
        let s = tone(1_000.0, 48_000.0, 48_000);
        let amp = goertzel(&s, 48_000.0, 1_000.0);
        assert!((amp - 1.0).abs() < 0.01, "amplitude {amp}");
        let off = goertzel(&s, 48_000.0, 2_000.0);
        assert!(off < 0.01, "off-frequency response {off}");
    }

    #[test]
    fn estimator_finds_the_peak() {
        let mut est = SpectrumEstimator::new(1_024);
        est.feed(&tone(3_000.0, 48_000.0, 48_000));
        let peak = est.peak_hz(48_000.0);
        assert!(
            (peak - 3_000.0).abs() < 100.0,
            "peak at {peak} Hz, want ~3000"
        );
        let mut psd = Vec::new();
        est.write_psd(&mut psd);
        assert_eq!(psd.len(), 513, "one-sided PSD length");
        est.reset();
        est.write_psd(&mut psd);
        assert!(
            psd.iter().all(|&v| v == 0.0),
            "reset must zero the accumulator"
        );
    }
}
