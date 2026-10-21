//! Numerically controlled oscillator.

use crate::{Complex32, Frequency, SampleRate};

/// Complex oscillator with an `f64` phase accumulator.
///
/// The accumulator stays in `f64` so phase error does not build up
/// over long runs, and phase is wrapped to `[-PI, PI]` every sample so
/// the trigonometric argument never grows. Samples are emitted as
/// unit-magnitude [`Complex32`] values.
/// See the [standalone stages example](crate#use-standalone-stages).
#[derive(Debug, Clone)]
pub struct Nco {
    phase: f64,
    step: f64,
}

impl Nco {
    /// Create a zero-phase oscillator with a finite signed frequency.
    ///
    /// Frequencies are reduced modulo the sample rate before division,
    /// including frequencies beyond Nyquist. Negative frequencies rotate
    /// clockwise, as used to mix an IF down to baseband.
    #[must_use]
    pub fn new(frequency: Frequency, sample_rate: SampleRate) -> Self {
        let rate = f64::from(sample_rate.get());
        let mut step = std::f64::consts::TAU * (frequency.get().rem_euclid(rate) / rate);
        if step > std::f64::consts::PI {
            step -= std::f64::consts::TAU;
        }
        Self { phase: 0.0, step }
    }

    /// Return the current unit-magnitude sample, then advance the phase.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "sin/cos of a wrapped phase are within [-1, 1]; narrowing \
                  f64 -> f32 loses only sub-epsilon precision"
    )]
    pub fn next_sample(&mut self) -> Complex32 {
        let (sin, cos) = self.phase.sin_cos();
        self.phase += self.step;
        if self.phase > std::f64::consts::PI {
            self.phase -= std::f64::consts::TAU;
        } else if self.phase < -std::f64::consts::PI {
            self.phase += std::f64::consts::TAU;
        }
        Complex32::new(cos as f32, sin as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn to_test_err<E: std::fmt::Debug>(error: E) -> TestCaseError {
        TestCaseError::fail(format!("{error:?}"))
    }

    #[test]
    fn matches_reference_cosine_over_long_run() -> TestResult {
        let mut nco = Nco::new(Frequency::new(1_000.0)?, SampleRate::new(48_000.0)?);
        for n in 0_u32..100_000 {
            let s = nco.next_sample();
            let ref_phase = std::f64::consts::TAU * 1_000.0 * f64::from(n) / 48_000.0;
            let (rs, rc) = ref_phase.sin_cos();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "reference values are within [-1, 1]"
            )]
            let (rs, rc) = (rs as f32, rc as f32);
            assert!(
                (s.re - rc).abs() < 1e-4 && (s.im - rs).abs() < 1e-4,
                "sample {n} diverged: got {s:?}, expected ({rc}, {rs})"
            );
        }
        Ok(())
    }

    proptest! {
        #[test]
        fn unit_magnitude_and_constant_phase_step(
            freq in -20_000.0_f64..20_000.0,
            steps in 1_usize..2_000,
        ) {
            let mut nco = Nco::new(Frequency::new(freq).map_err(to_test_err)?, SampleRate::new(48_000.0).map_err(to_test_err)?);
            let expected_step = std::f64::consts::TAU * freq / 48_000.0;
            let mut prev = nco.next_sample();
            for _ in 0..steps {
                let cur = nco.next_sample();
                let mag = f64::from(cur.norm());
                prop_assert!((mag - 1.0).abs() < 1e-5, "magnitude {}", mag);
                // Phase advance between consecutive samples equals the
                // configured step (angle of cur * conj(prev)).
                let delta = f64::from((cur * prev.conj()).arg());
                // Normalize the error into (-PI, PI] without float
                // loop conditions.
                let err = (delta - expected_step + std::f64::consts::PI)
                    .rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
                prop_assert!(err.abs() < 1e-3, "phase step error {}", err);
                prev = cur;
            }
        }
    }

    #[test]
    fn extreme_finite_aliases_keep_a_bounded_unit_oscillator() -> TestResult {
        for frequency in [f64::MAX, -f64::MAX, 0.0] {
            let mut nco = Nco::new(
                Frequency::new(frequency)?,
                SampleRate::new(f32::from_bits(1))?,
            );
            for _ in 0..100 {
                let sample = nco.next_sample();
                assert!(
                    sample.re.is_finite() && sample.im.is_finite(),
                    "frequency {frequency}"
                );
                assert!((sample.norm() - 1.0).abs() < 1e-6, "unit magnitude");
                assert!(nco.phase.abs() <= std::f64::consts::PI, "bounded phase");
            }
        }
        Ok(())
    }
}
