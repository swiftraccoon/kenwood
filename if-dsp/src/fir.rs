//! Windowed-sinc FIR design and streaming filters.
//!
//! The streaming filters keep their history in a plain `Vec` rotated by
//! one slot per sample and compute the output as a `zip` multiply-
//! accumulate without slice indexing.

use std::ops::Range;

use crate::{Bandwidth, Complex32, DspError, Frequency, SampleRate, TapCount};

/// Checked lowpass design parameters.
///
/// See the [filter design example](crate#design-and-stream-a-filter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowpassDesign {
    cutoff: Bandwidth,
    sample_rate: SampleRate,
    taps: TapCount,
}

impl LowpassDesign {
    /// Select a positive cutoff strictly below half the sample rate.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCutoff`] at or above Nyquist.
    pub fn new(
        cutoff: Bandwidth,
        sample_rate: SampleRate,
        taps: TapCount,
    ) -> Result<Self, DspError> {
        let maximum_hz = f64::from(sample_rate.get()) / 2.0;
        if f64::from(cutoff.get()) >= maximum_hz {
            return Err(DspError::InvalidCutoff {
                cutoff_hz: f64::from(cutoff.get()),
                maximum_hz,
            });
        }
        Ok(Self {
            cutoff,
            sample_rate,
            taps,
        })
    }

    /// Positive cutoff in hertz.
    #[must_use]
    pub const fn cutoff(self) -> Bandwidth {
        self.cutoff
    }

    /// Input sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// Odd coefficient count.
    #[must_use]
    pub const fn taps(self) -> TapCount {
        self.taps
    }
}

/// Checked complex bandpass design parameters.
///
/// Negative bands select the lower sideband. See the
/// [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandpassDesign {
    low: Frequency,
    high: Frequency,
    sample_rate: SampleRate,
    taps: TapCount,
}

impl BandpassDesign {
    /// Select an ordered band strictly inside both Nyquist boundaries.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidPassband`] for reversed or equal edges, or
    /// an edge at or outside half the positive or negative sample rate.
    /// Also rejects a normalized prototype cutoff that rounds to zero or Nyquist.
    pub fn new(
        band: Range<Frequency>,
        sample_rate: SampleRate,
        taps: TapCount,
    ) -> Result<Self, DspError> {
        let low_hz = band.start.get();
        let high_hz = band.end.get();
        let nyquist = f64::from(sample_rate.get()) / 2.0;
        let prototype_cutoff = ((high_hz - low_hz) / f64::from(sample_rate.get())) / 2.0;
        if low_hz >= high_hz
            || low_hz <= -nyquist
            || high_hz >= nyquist
            || prototype_cutoff <= 0.0
            || prototype_cutoff >= 0.5
        {
            return Err(DspError::InvalidPassband {
                low_hz,
                high_hz,
                sample_rate_hz: sample_rate.get(),
            });
        }
        Ok(Self {
            low: band.start,
            high: band.end,
            sample_rate,
            taps,
        })
    }

    /// Lower design edge in hertz.
    #[must_use]
    pub const fn low(self) -> Frequency {
        self.low
    }

    /// Upper edge in hertz.
    #[must_use]
    pub const fn high(self) -> Frequency {
        self.high
    }

    /// Complex sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> SampleRate {
        self.sample_rate
    }

    /// Odd coefficient count.
    #[must_use]
    pub const fn taps(self) -> TapCount {
        self.taps
    }
}

impl LowpassDesign {
    /// Design a linear-phase lowpass with a Hamming window and unity DC gain
    /// to floating-point precision.
    /// See the [filter design example](crate#design-and-stream-a-filter).
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    #[must_use]
    pub fn to_taps(self) -> Vec<f32> {
        lowpass_coefficients(
            f64::from(self.cutoff.get()) / f64::from(self.sample_rate.get()),
            self.taps.get(),
        )
    }
}

#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "windowed-sinc design uses f64 index arithmetic and rounds its coefficients to f32"
)]
fn lowpass_coefficients(fc: f64, taps: usize) -> Vec<f32> {
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

impl BandpassDesign {
    /// Shift a real lowpass prototype into the selected complex bandpass.
    ///
    /// Negative bands select the lower sideband. See the
    /// [filter design example](crate#design-and-stream-a-filter).
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "modulation evaluates coefficient indices in f64 and rounds complex coefficients to f32"
    )]
    #[must_use]
    pub fn to_taps(self) -> Vec<Complex32> {
        let lo_hz = self.low.get();
        let hi_hz = self.high.get();
        let sample_rate = f64::from(self.sample_rate.get());
        let taps = self.taps.get();
        let proto = lowpass_coefficients(((hi_hz - lo_hz) / sample_rate) / 2.0, taps);
        let fc = lo_hz.midpoint(hi_hz) / sample_rate;
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
}

/// Streaming FIR with real taps over complex samples (the decimation
/// lowpass ahead of a rate change).
#[derive(Debug, Clone)]
pub struct FirRealTaps {
    taps: Vec<f32>,
    hist: Vec<Complex32>,
}

impl FirRealTaps {
    /// Retain finite real coefficients and allocate zeroed complex history.
    ///
    /// See the [filter design example](crate#design-and-stream-a-filter).
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] for empty or nonfinite coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(taps: Vec<f32>) -> Result<Self, DspError> {
        validate_taps(taps.iter().copied())?;
        let hist = vec![Complex32::new(0.0, 0.0); taps.len()];
        Ok(Self { taps, hist })
    }

    /// Advance zero-initialized history and return its convolution with the taps.
    ///
    /// No allocation occurs. Sample arithmetic follows
    /// [the PCM contract](crate#allocation-and-samples).
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
    /// Retain finite complex coefficients and allocate zeroed history.
    ///
    /// See the [standalone stages example](crate#use-standalone-stages).
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] for empty coefficients or a nonfinite
    /// component.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(taps: Vec<Complex32>) -> Result<Self, DspError> {
        if taps.is_empty() {
            return Err(DspError::InvalidTaps { index: None });
        }
        if let Some(index) = taps
            .iter()
            .position(|tap| !tap.re.is_finite() || !tap.im.is_finite())
        {
            return Err(DspError::InvalidTaps { index: Some(index) });
        }
        let hist = vec![Complex32::new(0.0, 0.0); taps.len()];
        Ok(Self { taps, hist })
    }

    /// Advance zero-initialized history and return its convolution with the taps.
    ///
    /// No allocation occurs. Sample arithmetic follows
    /// [the PCM contract](crate#allocation-and-samples).
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
    /// Retain finite real coefficients and allocate zeroed real history.
    ///
    /// See the [filter design example](crate#design-and-stream-a-filter).
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidTaps`] for empty or nonfinite coefficients.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(taps: Vec<f32>) -> Result<Self, DspError> {
        validate_taps(taps.iter().copied())?;
        let hist = vec![0.0; taps.len()];
        Ok(Self { taps, hist })
    }

    /// Advance zero-initialized history and return its convolution with the taps.
    ///
    /// No allocation occurs. Sample arithmetic follows
    /// [the PCM contract](crate#allocation-and-samples).
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

fn validate_taps(mut taps: impl Iterator<Item = f32>) -> Result<(), DspError> {
    let Some(first) = taps.next() else {
        return Err(DspError::InvalidTaps { index: None });
    };
    if !first.is_finite() {
        return Err(DspError::InvalidTaps { index: Some(0) });
    }
    if let Some(index) = taps.position(|tap| !tap.is_finite()) {
        return Err(DspError::InvalidTaps {
            index: Some(index + 1),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Evaluate the frequency response magnitude of real taps at `f`
    /// (analytic DTFT: deterministic, no filtering needed).
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
    fn lowpass_dc_gain_is_unity_and_stopband_is_deep() -> TestResult {
        let taps = LowpassDesign::new(
            Bandwidth::new(5_000.0)?,
            SampleRate::new(48_000.0)?,
            TapCount::new(63)?,
        )?
        .to_taps();
        let dc = response_real(&taps, 0.0, 48_000.0);
        assert!((dc - 1.0).abs() < 1e-6, "DC gain {dc}");
        let pass = response_real(&taps, 2_000.0, 48_000.0);
        assert!(pass > 0.98, "passband gain {pass}");
        let stop = response_real(&taps, 10_000.0, 48_000.0);
        assert!(stop < 0.01, "stopband leakage {stop} (want < -40 dB)");
        Ok(())
    }

    #[test]
    fn complex_bandpass_selects_only_the_positive_band() -> TestResult {
        let taps = BandpassDesign::new(
            Frequency::new(100.0)?..Frequency::new(3_000.0)?,
            SampleRate::new(12_000.0)?,
            TapCount::new(127)?,
        )?
        .to_taps();
        let inband = response_complex(&taps, 1_000.0, 12_000.0);
        assert!(inband > 0.95, "in-band gain {inband}");
        let mirror = response_complex(&taps, -1_000.0, 12_000.0);
        assert!(mirror < 0.01, "mirror leakage {mirror} (want < -40 dB)");
        let outer = response_complex(&taps, 5_000.0, 12_000.0);
        assert!(outer < 0.01, "out-of-band leakage {outer}");
        Ok(())
    }

    #[test]
    fn streaming_filter_matches_direct_convolution() -> TestResult {
        let taps = LowpassDesign::new(
            Bandwidth::new(5_000.0)?,
            SampleRate::new(48_000.0)?,
            TapCount::new(15)?,
        )?
        .to_taps();
        let mut fir = FirReal::new(taps.clone())?;
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
        Ok(())
    }

    #[test]
    fn filter_boundaries_reject_aliasing_and_nonfinite_coefficients() -> TestResult {
        let rate = SampleRate::new(12_000.0)?;
        let taps = TapCount::new(3)?;
        for cutoff in [6_000.0, 6_001.0] {
            assert!(
                matches!(
                    LowpassDesign::new(Bandwidth::new(cutoff)?, rate, taps),
                    Err(DspError::InvalidCutoff { .. })
                ),
                "cutoff {cutoff}"
            );
        }
        for (low, high) in [(-6_000.0, 0.0), (0.0, 6_000.0), (1.0, 1.0), (2.0, 1.0)] {
            assert!(
                matches!(
                    BandpassDesign::new(Frequency::new(low)?..Frequency::new(high)?, rate, taps),
                    Err(DspError::InvalidPassband { .. })
                ),
                "band {low}..{high}"
            );
        }
        for coefficients in [vec![], vec![f32::NAN], vec![1.0, f32::INFINITY]] {
            assert!(FirReal::new(coefficients.clone()).is_err(), "real taps");
            assert!(
                FirRealTaps::new(coefficients).is_err(),
                "real complex-input taps"
            );
        }
        assert!(FirComplexTaps::new(vec![]).is_err(), "empty complex taps");
        assert!(
            FirComplexTaps::new(vec![Complex32::new(0.0, f32::NAN)]).is_err(),
            "nonfinite imaginary coefficient"
        );
        let mut identity = FirReal::new(vec![1.0])?;
        assert_eq!(
            identity.push(0.25).to_bits(),
            0.25_f32.to_bits(),
            "a custom one-tap FIR remains valid"
        );
        let smallest = LowpassDesign::new(Bandwidth::new(1_000.0)?, rate, taps)?.to_taps();
        assert!(
            smallest.iter().all(|value| value.is_finite()),
            "three-tap window must be finite"
        );
        let tiny = LowpassDesign::new(
            Bandwidth::new(f32::from_bits(1))?,
            SampleRate::new(f32::MAX)?,
            taps,
        )?
        .to_taps();
        assert!(
            tiny.iter().all(|value| value.is_finite()),
            "a tiny positive normalized cutoff stays usable"
        );
        let unrepresentable = BandpassDesign::new(
            Frequency::new(0.0)?..Frequency::new(f64::from_bits(1))?,
            SampleRate::new(f32::MAX)?,
            taps,
        );
        assert!(
            matches!(unrepresentable, Err(DspError::InvalidPassband { .. })),
            "a prototype cutoff rounded to zero must be rejected"
        );
        Ok(())
    }
}
