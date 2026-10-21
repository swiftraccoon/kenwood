//! Tone amplitudes and averaged windowed FFT bin powers.

use std::borrow::Borrow;
use std::sync::Arc;

use rustfft::{Fft, FftPlanner};

use crate::{Complex32, FftSize, Frequency, SampleRate};

/// Amplitude of the `freq_hz` component of `samples` (Goertzel).
///
/// Returns the amplitude a pure tone at `freq_hz` would need to
/// produce the observed correlation; for a clean unit tone the result
/// is ~1.0.
/// Frequencies alias modulo the sample rate. DC and Nyquist use one-sided
/// amplitude normalization; other frequencies use twice the correlation.
/// Finite blocks need not contain an integral number of cycles, so leakage
/// can affect the estimate. See the
/// [spectrum example](crate#measure-a-spectrum).
/// Empty input returns zero. Owned and borrowed sample iterators are accepted;
/// the normalization count accumulates at `f64` precision.
#[expect(
    clippy::cast_possible_truncation,
    reason = "Goertzel accumulates and normalizes in f64 before rounding the amplitude to f32"
)]
#[must_use]
pub fn goertzel<I>(samples: I, sample_rate: SampleRate, frequency: Frequency) -> f32
where
    I: IntoIterator,
    I::Item: Borrow<f32>,
{
    let mut count = 0.0_f64;
    let rate = f64::from(sample_rate.get());
    let remainder = frequency.get().rem_euclid(rate);
    let aliased = if remainder >= rate { 0.0 } else { remainder };
    if aliased == 0.0 {
        let mut sum = 0.0_f64;
        for sample in samples {
            sum += f64::from(*sample.borrow());
            count += 1.0;
        }
        return (sum.abs() / count.max(1.0)) as f32;
    }
    if aliased.to_bits() == (rate / 2.0).to_bits() {
        let mut sum = 0.0_f64;
        let mut sign = 1.0_f64;
        for sample in samples {
            sum += sign * f64::from(*sample.borrow());
            sign = -sign;
            count += 1.0;
        }
        return (sum.abs() / count.max(1.0)) as f32;
    }
    let omega = std::f64::consts::TAU * (aliased / rate);
    let coeff = 2.0 * omega.cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for sample in samples {
        let s0 = f64::from(*sample.borrow()) + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
        count += 1.0;
    }
    let power = (coeff * s1).mul_add(-s2, s1.mul_add(s1, s2 * s2));
    (2.0 * power.max(0.0).sqrt() / count.max(1.0)) as f32
}

/// Average squared FFT magnitudes of periodically Hann-windowed real segments.
///
/// The retained bins are `0..=floor(size / 2)`. No one-sided doubling,
/// window-energy normalization, sample-rate division or dB conversion occurs.
/// See the [spectrum example](crate#measure-a-spectrum).
/// Cloning shares the immutable FFT plan and independently copies stream state.
#[derive(Clone)]
pub struct SpectrumEstimator {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    pending: Vec<f32>,
    scratch: Vec<Complex32>,
    fft_scratch: Vec<Complex32>,
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
    /// Allocate a plan, periodic Hann window, segment and FFT scratch buffers.
    ///
    /// The hop is `size / 2`; it is exactly 50% overlap for even sizes.
    /// Odd sizes retain one additional sample in the overlapping half.
    /// Power-of-two sizes are usually efficient; every checked size is accepted.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    #[expect(
        clippy::cast_precision_loss,
        reason = "window coordinates are evaluated in f64; unusually large FFT sizes can round at f64 precision"
    )]
    #[must_use]
    pub fn new(size: FftSize) -> Self {
        let fft_size = size.get();
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let fft_scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
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
            pending: Vec::with_capacity(fft_size),
            scratch: vec![Complex32::new(0.0, 0.0); fft_size],
            fft_scratch,
            acc: vec![0.0; fft_size / 2 + 1],
            segments: 0,
            size: fft_size,
        }
    }

    /// Consume a block, processing every completed overlapping segment.
    ///
    /// Internal buffers retain their construction-time capacities regardless
    /// of block size. This method does not allocate. Partial segments survive
    /// calls. Power sums use `f32` arithmetic and can overflow; the segment
    /// counter saturates at `usize::MAX`. Reset when starting a new average.
    /// Nonfinite samples follow [the PCM contract](crate#allocation-and-samples).
    pub fn feed(&mut self, mut samples: &[f32]) {
        while !samples.is_empty() {
            let take = (self.size - self.pending.len()).min(samples.len());
            let (prefix, remaining) = samples.split_at(take);
            self.pending.extend_from_slice(prefix);
            samples = remaining;
            if self.pending.len() < self.size {
                continue;
            }
            for (dst, (&s, &w)) in self
                .scratch
                .iter_mut()
                .zip(self.pending.iter().zip(self.window.iter()))
            {
                *dst = Complex32::new(s * w, 0.0);
            }
            self.fft
                .process_with_scratch(&mut self.scratch, &mut self.fft_scratch);
            for (a, x) in self.acc.iter_mut().zip(self.scratch.iter()) {
                *a += x.norm_sqr();
            }
            self.segments = self.segments.saturating_add(1);
            drop(self.pending.drain(..self.size / 2));
        }
    }

    /// Write averaged unnormalized FFT bin powers into `out`, clearing it first.
    ///
    /// Produces `size / 2 + 1` bins and all zeros before the first segment.
    /// Values are squared magnitudes of the windowed DFT sums, before any
    /// one-sided, window-gain or spectral-density calibration.
    /// Caller-owned output storage can grow; see
    /// [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Standard output-vector capacity limits can panic.
    #[expect(
        clippy::cast_precision_loss,
        reason = "the segment-count normalization is rounded to f32, matching the accumulated bin powers"
    )]
    pub fn write_bin_power(&self, out: &mut Vec<f32>) {
        out.clear();
        let norm = if self.segments == 0 {
            0.0
        } else {
            1.0 / (self.segments as f32)
        };
        out.extend(self.acc.iter().map(|&a| a * norm));
    }

    /// Sum of the actual window coefficients for coherent-amplitude calibration.
    ///
    /// For a bin-centered real tone away from DC and Nyquist, divide a bin's
    /// square-root power by this sum and multiply by two to estimate its peak
    /// amplitude. DC and even-size Nyquist bins omit that factor of two.
    /// See the [spectrum example](crate#measure-a-spectrum).
    #[must_use]
    pub fn window_sum(&self) -> f32 {
        self.window.iter().sum()
    }

    /// Frequency of the strongest bin, in hertz.
    ///
    /// Returns zero before any segment, or when all powers are zero. Ties
    /// select the lowest index. Frequencies follow the supplied sample rate;
    /// no additional transform is computed.
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin coordinates are rounded to f32; dividing before scaling avoids intermediate overflow"
    )]
    #[must_use]
    pub fn peak_hz(&self, sample_rate: SampleRate) -> f32 {
        let (idx, _) =
            self.acc.iter().enumerate().fold(
                (0, 0.0_f32),
                |(bi, bv), (i, &v)| if v > bv { (i, v) } else { (bi, bv) },
            );
        (idx as f32 / self.size as f32) * sample_rate.get()
    }

    /// Discard partial input, powers and segment count, retaining all allocations.
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    fn goertzel_reads_unit_tone_amplitude() -> TestResult {
        let s = tone(1_000.0, 48_000.0, 48_000);
        let amp = goertzel(&s, SampleRate::new(48_000.0)?, Frequency::new(1_000.0)?);
        assert!((amp - 1.0).abs() < 0.01, "amplitude {amp}");
        let off = goertzel(&s, SampleRate::new(48_000.0)?, Frequency::new(2_000.0)?);
        assert!(off < 0.01, "off-frequency response {off}");
        Ok(())
    }

    #[test]
    fn estimator_finds_the_peak() -> TestResult {
        let mut est = SpectrumEstimator::new(FftSize::new(1_024)?);
        est.feed(&tone(3_000.0, 48_000.0, 48_000));
        let peak = est.peak_hz(SampleRate::new(48_000.0)?);
        assert!(
            (peak - 3_000.0).abs() < 100.0,
            "peak at {peak} Hz, want ~3000"
        );
        let mut psd = Vec::new();
        est.write_bin_power(&mut psd);
        assert_eq!(psd.len(), 513, "one-sided PSD length");
        assert!(
            (est.window_sum() - 512.0).abs() < 1e-3,
            "periodic Hann coherent gain"
        );
        est.reset();
        est.write_bin_power(&mut psd);
        assert!(
            psd.iter().all(|&v| v == 0.0),
            "reset must zero the accumulator"
        );
        Ok(())
    }

    #[test]
    fn odd_and_small_fft_sizes_advance_and_chunks_preserve_bin_powers() -> TestResult {
        let input = tone(1_000.0, 48_000.0, 4_099);
        for size in [2, 3, 5, 1_024] {
            let mut whole = SpectrumEstimator::new(FftSize::new(size)?);
            let mut chunked = whole.clone();
            let capacities = (
                whole.pending.capacity(),
                whole.scratch.capacity(),
                whole.fft_scratch.capacity(),
            );
            whole.feed(&input);
            let expected_segments = 1 + (input.len() - size) / (size / 2);
            assert_eq!(
                whole.segments, expected_segments,
                "segment count must follow the documented half-size hop"
            );
            for chunk in input.chunks(7) {
                chunked.feed(chunk);
            }
            let mut expected = Vec::new();
            let mut actual = Vec::new();
            whole.write_bin_power(&mut expected);
            chunked.write_bin_power(&mut actual);
            assert_eq!(
                actual, expected,
                "FFT size {size} must be independent of feed chunks"
            );
            assert_eq!(actual.len(), size / 2 + 1, "positive-frequency bin count");
            assert!(
                whole.pending.len() < size,
                "retained partial segment is bounded"
            );
            assert_eq!(
                (
                    whole.pending.capacity(),
                    whole.scratch.capacity(),
                    whole.fft_scratch.capacity()
                ),
                capacities,
                "large blocks must not grow scratch"
            );
        }
        Ok(())
    }

    #[test]
    fn goertzel_normalizes_dc_nyquist_and_frequency_aliases() -> TestResult {
        let rate = SampleRate::new(48_000.0)?;
        assert_eq!(
            goertzel([1.0; 16], rate, Frequency::new(0.0)?).to_bits(),
            1.0_f32.to_bits(),
            "DC amplitude"
        );
        assert_eq!(
            goertzel([1.0; 16], rate, Frequency::new(-f64::from_bits(1))?).to_bits(),
            1.0_f32.to_bits(),
            "a remainder rounded to the rate aliases to DC"
        );
        let nyquist: Vec<_> = (0..16)
            .map(|index| if index % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        assert_eq!(
            goertzel(&nyquist, rate, Frequency::new(24_000.0)?).to_bits(),
            1.0_f32.to_bits(),
            "Nyquist amplitude"
        );
        let samples = tone(1_000.0, 48_000.0, 48_000);
        let expected = goertzel(&samples, rate, Frequency::new(1_000.0)?);
        assert_eq!(
            goertzel(&samples, rate, Frequency::new(49_000.0)?).to_bits(),
            expected.to_bits(),
            "frequency alias"
        );
        assert_eq!(
            goertzel(std::iter::empty::<f32>(), rate, Frequency::new(0.0)?).to_bits(),
            0.0_f32.to_bits(),
            "empty input"
        );
        assert_eq!(
            goertzel(std::iter::repeat_n(1.0_f32, 16), rate, Frequency::new(0.0)?).to_bits(),
            1.0_f32.to_bits(),
            "owned streamed samples"
        );
        Ok(())
    }
}
