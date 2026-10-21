//! Integer-factor rate conversion: FIR-protected decimation and
//! zero-stuffing interpolation.

use crate::fir::{FirReal, FirRealTaps, LowpassDesign};
use crate::{Complex32, DspError, RateFactor};

/// Anti-aliased decimator: lowpass then keep one sample in `factor`.
/// See the [rate conversion example](crate#convert-sample-rates).
#[derive(Debug, Clone)]
pub struct Decimator {
    fir: FirRealTaps,
    factor: usize,
    phase: usize,
}

impl Decimator {
    /// Build a decimator with a cutoff below the output Nyquist limit.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCutoff`] when the cutoff reaches
    /// `design.sample_rate() / (2 * factor)`, or [`DspError::InvalidTaps`]
    /// if designed coefficients are nonfinite.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    pub fn new(factor: RateFactor, design: LowpassDesign) -> Result<Self, DspError> {
        validate_cutoff(factor, design)?;
        Ok(Self {
            fir: FirRealTaps::new(design.to_taps())?,
            factor: factor.get(),
            phase: 0,
        })
    }

    /// Filter `input` and append every `factor`-th sample to `out`
    /// (which is cleared first). Phase carries across calls, so
    /// arbitrary chunk sizes are fine.
    ///
    /// The first input sample produces an output; later outputs are separated
    /// by `factor` inputs. Output growth may allocate; see
    /// [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Standard output-vector capacity limits can panic.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        out.clear();
        for &x in input {
            let y = self.fir.push(x);
            if self.phase == 0 {
                out.push(y);
            }
            self.phase += 1;
            if self.phase == self.factor {
                self.phase = 0;
            }
        }
    }
}

/// Zero-stuffing interpolator: insert `factor - 1` zeros per input
/// sample, lowpass at the output rate, gain-compensate by `factor`.
/// See the [rate conversion example](crate#convert-sample-rates).
#[derive(Debug, Clone)]
pub struct Interpolator {
    fir: FirReal,
    factor: usize,
    gain: f32,
}

impl Interpolator {
    /// Build an interpolator with a cutoff below the input Nyquist limit.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidCutoff`] when the cutoff reaches
    /// `design.sample_rate() / (2 * factor)`, or [`DspError::InvalidTaps`]
    /// if designed coefficients are nonfinite.
    ///
    /// # Panics
    ///
    /// Standard vector capacity limits can panic; see
    /// [allocation and samples](crate#allocation-and-samples).
    #[expect(
        clippy::cast_precision_loss,
        reason = "the gain is the integer factor rounded to f32 precision; large factors can lose low bits"
    )]
    pub fn new(factor: RateFactor, design: LowpassDesign) -> Result<Self, DspError> {
        validate_cutoff(factor, design)?;
        Ok(Self {
            fir: FirReal::new(design.to_taps())?,
            factor: factor.get(),
            gain: factor.get() as f32,
        })
    }

    /// Interpolate `input` into `out` (cleared first); output length is
    /// `input.len() * factor`.
    ///
    /// Filter state carries across calls. Output growth may allocate; see
    /// [allocation and samples](crate#allocation-and-samples).
    ///
    /// # Panics
    ///
    /// Panics if the expanded length exceeds the output vector's capacity limit.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        for &x in input {
            out.push(self.fir.push(x * self.gain));
            for _ in 1..self.factor {
                out.push(self.fir.push(0.0));
            }
        }
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "integer rate factors are evaluated at f64 precision for the Nyquist comparison"
)]
fn validate_cutoff(factor: RateFactor, design: LowpassDesign) -> Result<(), DspError> {
    let maximum_hz = f64::from(design.sample_rate().get()) / 2.0 / factor.get() as f64;
    let cutoff_hz = f64::from(design.cutoff().get());
    if cutoff_hz >= maximum_hz {
        Err(DspError::InvalidCutoff {
            cutoff_hz,
            maximum_hz,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum_test_support::tone_amplitude;
    use crate::{Bandwidth, SampleRate, TapCount};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn design() -> Result<LowpassDesign, DspError> {
        LowpassDesign::new(
            Bandwidth::new(5_000.0)?,
            SampleRate::new(48_000.0)?,
            TapCount::new(63)?,
        )
    }

    #[test]
    fn decimate_then_interpolate_preserves_a_tone() -> TestResult {
        let mut dec = Decimator::new(RateFactor::new(4)?, design()?)?;
        let mut int = Interpolator::new(RateFactor::new(4)?, design()?)?;
        // 1 kHz complex tone at 48 kHz.
        let input: Vec<Complex32> = (0..48_000_u32)
            .map(|n| {
                let ph = std::f64::consts::TAU * 1_000.0 * f64::from(n) / 48_000.0;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "unit-magnitude tone samples"
                )]
                Complex32::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let mut bb = Vec::new();
        dec.process(&input, &mut bb);
        assert_eq!(bb.len(), 12_000, "decimated length");
        // Take the real part (a 1 kHz cosine at 12 kHz rate) back up.
        let real: Vec<f32> = bb.iter().map(|c| c.re).collect();
        let mut out = Vec::new();
        int.process(&real, &mut out);
        assert_eq!(out.len(), 48_000, "interpolated length");
        // Steady-state amplitude of the 1 kHz tone survives the round
        // trip (skip the filters' transient head).
        let steady = out
            .get(4_800..)
            .ok_or("interpolated steady-state samples")?;
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0)?;
        assert!(
            (amp - 1.0).abs() < 0.05,
            "round-trip amplitude {amp}, expected ~1.0"
        );
        Ok(())
    }

    #[test]
    fn decimator_rejects_out_of_band_energy() -> TestResult {
        let mut dec = Decimator::new(RateFactor::new(4)?, design()?)?;
        // 10 kHz tone: above the 5 kHz corner, must be crushed before
        // the rate change would alias it to 2 kHz.
        let input: Vec<Complex32> = (0..48_000_u32)
            .map(|n| {
                let ph = std::f64::consts::TAU * 10_000.0 * f64::from(n) / 48_000.0;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "unit-magnitude tone samples"
                )]
                Complex32::new(ph.cos() as f32, ph.sin() as f32)
            })
            .collect();
        let mut bb = Vec::new();
        dec.process(&input, &mut bb);
        let real: Vec<f32> = bb.iter().map(|c| c.re).collect();
        let steady = real.get(1_200..).ok_or("decimated steady-state samples")?;
        let leaked = tone_amplitude(steady, 12_000.0, 2_000.0)?;
        assert!(leaked < 0.02, "aliased leakage {leaked} (want < -34 dB)");
        Ok(())
    }

    #[test]
    fn resampling_preserves_chunk_phase_and_rejects_aliasing_cutoffs() -> TestResult {
        let factor = RateFactor::new(4)?;
        let invalid = LowpassDesign::new(
            Bandwidth::new(6_000.0)?,
            SampleRate::new(48_000.0)?,
            TapCount::new(3)?,
        )?;
        assert!(
            Decimator::new(factor, invalid).is_err(),
            "decimator Nyquist boundary"
        );
        assert!(
            Interpolator::new(factor, invalid).is_err(),
            "interpolator Nyquist boundary"
        );
        let input = vec![Complex32::new(1.0, 0.0); 37];
        let mut whole = Decimator::new(factor, design()?)?;
        let mut chunked = whole.clone();
        let mut expected = Vec::new();
        whole.process(&input, &mut expected);
        let mut actual = Vec::new();
        let mut block = Vec::new();
        for chunk in input.chunks(3) {
            chunked.process(chunk, &mut block);
            actual.extend_from_slice(&block);
        }
        assert_eq!(
            actual, expected,
            "decimator phase must survive nonmultiple chunks"
        );
        let real = vec![0.25; 13];
        let mut whole = Interpolator::new(factor, design()?)?;
        let mut chunked = whole.clone();
        let mut expected = Vec::new();
        whole.process(&real, &mut expected);
        let mut actual = Vec::new();
        let mut block = Vec::new();
        for chunk in real.chunks(3) {
            chunked.process(chunk, &mut block);
            actual.extend_from_slice(&block);
        }
        assert_eq!(actual, expected, "interpolation must retain filter history");
        Ok(())
    }
}
