//! Windowed-sinc FIR design and streaming filters.
//!
//! The streaming filters keep their history in a plain `Vec` rotated by
//! one slot per sample and compute the output as a `zip` multiply-
//! accumulate. That trades a small memmove per sample (cheap at these
//! tap counts and rates) for filter code with no slice indexing at all.

use crate::Complex32;

/// Design a linear-phase lowpass by the windowed-sinc method with a
/// Hamming window. `taps` must be odd; DC gain is normalized to
/// exactly 1.
///
/// # Panics
///
/// Panics if `taps` is even or zero.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "tap counts are small (well under 2^23) so usize -> f64 is \
              exact, and coefficients are O(1) so f64 -> f32 narrowing \
              loses only sub-epsilon precision"
)]
#[must_use]
pub fn design_lowpass(cutoff_hz: f32, sample_rate: f32, taps: usize) -> Vec<f32> {
    assert!(taps % 2 == 1, "taps must be odd, got {taps}");
    let fc = f64::from(cutoff_hz) / f64::from(sample_rate);
    let m = (taps - 1) as f64;
    let h: Vec<f64> = (0..taps)
        .map(|n| {
            let x = n as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                std::f64::consts::TAU * fc
            } else {
                (std::f64::consts::TAU * fc * x).sin() / x
            };
            let w = 0.46_f64.mul_add(-(std::f64::consts::TAU * n as f64 / m).cos(), 0.54);
            sinc * w
        })
        .collect();
    let sum: f64 = h.iter().sum();
    h.iter().map(|v| (v / sum) as f32).collect()
}

/// Shift a real lowpass prototype into a complex bandpass selecting
/// only the band `lo_hz..hi_hz`.
///
/// Passing a negative band (e.g. `-3000..-100`) selects the lower
/// sideband.
///
/// # Panics
///
/// Panics if `hi_hz <= lo_hz` or `taps` is even or zero.
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "tap counts are small so usize -> f64 is exact, and \
              modulated coefficients are O(1) so f64 -> f32 narrowing \
              loses only sub-epsilon precision"
)]
#[must_use]
pub fn design_complex_bandpass(
    lo_hz: f32,
    hi_hz: f32,
    sample_rate: f32,
    taps: usize,
) -> Vec<Complex32> {
    assert!(hi_hz > lo_hz, "band must be ordered: {lo_hz}..{hi_hz}");
    let proto = design_lowpass((hi_hz - lo_hz) / 2.0, sample_rate, taps);
    let fc = f64::from(lo_hz + hi_hz) / 2.0 / f64::from(sample_rate);
    let m = (taps - 1) as f64;
    proto
        .iter()
        .enumerate()
        .map(|(n, &h)| {
            let ph = std::f64::consts::TAU * fc * (n as f64 - m / 2.0);
            let (sin, cos) = ph.sin_cos();
            Complex32::new((f64::from(h) * cos) as f32, (f64::from(h) * sin) as f32)
        })
        .collect()
}

/// Streaming FIR with real taps over complex samples (the decimation
/// lowpass ahead of a rate change).
#[derive(Debug, Clone)]
pub struct FirRealTaps {
    taps: Vec<f32>,
    hist: Vec<Complex32>,
}

impl FirRealTaps {
    /// Build the filter from designed taps.
    #[must_use]
    pub fn new(taps: Vec<f32>) -> Self {
        let hist = vec![Complex32::new(0.0, 0.0); taps.len()];
        Self { taps, hist }
    }

    /// Push one sample, returning the filtered output.
    pub fn push(&mut self, x: Complex32) -> Complex32 {
        self.hist.rotate_right(1);
        if let Some(h) = self.hist.first_mut() {
            *h = x;
        }
        self.taps
            .iter()
            .zip(self.hist.iter())
            .map(|(&t, &h)| h * t)
            .sum()
    }
}

/// Streaming FIR with complex taps over complex samples (the mode
/// bandpass in SSB demodulation).
#[derive(Debug, Clone)]
pub struct FirComplexTaps {
    taps: Vec<Complex32>,
    hist: Vec<Complex32>,
}

impl FirComplexTaps {
    /// Build the filter from designed taps.
    #[must_use]
    pub fn new(taps: Vec<Complex32>) -> Self {
        let hist = vec![Complex32::new(0.0, 0.0); taps.len()];
        Self { taps, hist }
    }

    /// Push one sample, returning the filtered output.
    pub fn push(&mut self, x: Complex32) -> Complex32 {
        self.hist.rotate_right(1);
        if let Some(h) = self.hist.first_mut() {
            *h = x;
        }
        self.taps
            .iter()
            .zip(self.hist.iter())
            .map(|(&t, &h)| h * t)
            .sum()
    }
}

/// Streaming FIR with real taps over real samples (the interpolation
/// lowpass on demodulated audio).
#[derive(Debug, Clone)]
pub struct FirReal {
    taps: Vec<f32>,
    hist: Vec<f32>,
}

impl FirReal {
    /// Build the filter from designed taps.
    #[must_use]
    pub fn new(taps: Vec<f32>) -> Self {
        let hist = vec![0.0; taps.len()];
        Self { taps, hist }
    }

    /// Push one sample, returning the filtered output.
    pub fn push(&mut self, x: f32) -> f32 {
        self.hist.rotate_right(1);
        if let Some(h) = self.hist.first_mut() {
            *h = x;
        }
        self.taps
            .iter()
            .zip(self.hist.iter())
            .map(|(&t, &h)| h * t)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluate the frequency response magnitude of real taps at `f`
    /// (analytic DTFT — deterministic, no filtering needed).
    fn response_real(taps: &[f32], f_hz: f64, rate: f64) -> f64 {
        let omega = std::f64::consts::TAU * f_hz / rate;
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        for (n, &t) in taps.iter().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "small tap indices")]
            let ph = omega * n as f64;
            re += f64::from(t) * ph.cos();
            im -= f64::from(t) * ph.sin();
        }
        re.hypot(im)
    }

    /// Same for complex taps.
    fn response_complex(taps: &[Complex32], f_hz: f64, rate: f64) -> f64 {
        let omega = std::f64::consts::TAU * f_hz / rate;
        let (mut re, mut im) = (0.0_f64, 0.0_f64);
        for (n, t) in taps.iter().enumerate() {
            #[expect(clippy::cast_precision_loss, reason = "small tap indices")]
            let ph = -omega * n as f64;
            let (tr, ti) = (f64::from(t.re), f64::from(t.im));
            re += tr.mul_add(ph.cos(), -(ti * ph.sin()));
            im += tr.mul_add(ph.sin(), ti * ph.cos());
        }
        re.hypot(im)
    }

    #[test]
    fn lowpass_dc_gain_is_unity_and_stopband_is_deep() {
        let taps = design_lowpass(5_000.0, 48_000.0, 63);
        let dc = response_real(&taps, 0.0, 48_000.0);
        assert!((dc - 1.0).abs() < 1e-6, "DC gain {dc}");
        let pass = response_real(&taps, 2_000.0, 48_000.0);
        assert!(pass > 0.98, "passband gain {pass}");
        let stop = response_real(&taps, 10_000.0, 48_000.0);
        assert!(stop < 0.01, "stopband leakage {stop} (want < -40 dB)");
    }

    #[test]
    fn complex_bandpass_selects_only_the_positive_band() {
        let taps = design_complex_bandpass(100.0, 3_000.0, 12_000.0, 127);
        let inband = response_complex(&taps, 1_000.0, 12_000.0);
        assert!(inband > 0.95, "in-band gain {inband}");
        let mirror = response_complex(&taps, -1_000.0, 12_000.0);
        assert!(mirror < 0.01, "mirror leakage {mirror} (want < -40 dB)");
        let outer = response_complex(&taps, 5_000.0, 12_000.0);
        assert!(outer < 0.01, "out-of-band leakage {outer}");
    }

    #[test]
    fn streaming_filter_matches_direct_convolution() {
        let taps = design_lowpass(5_000.0, 48_000.0, 15);
        let mut fir = FirReal::new(taps.clone());
        let input: Vec<f32> = (0..40).map(|n| if n == 0 { 1.0 } else { 0.0 }).collect();
        let outputs: Vec<f32> = input.iter().map(|&x| fir.push(x)).collect();
        // Impulse response of the streaming filter equals the taps.
        for (n, &t) in taps.iter().enumerate() {
            let y = outputs.get(n).copied().unwrap_or(f32::NAN);
            assert!(
                (y - t).abs() < 1e-6,
                "tap {n}: streamed {y} vs designed {t}"
            );
        }
    }
}
