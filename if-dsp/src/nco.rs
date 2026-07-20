//! Numerically controlled oscillator.

use crate::Complex32;

/// Complex oscillator with an `f64` phase accumulator.
///
/// The accumulator stays in `f64` so phase error does not build up
/// over long runs, and phase is wrapped to `(-PI, PI]` every sample so
/// the trigonometric argument never grows. Samples are emitted as
/// unit-magnitude [`Complex32`] values.
#[derive(Debug, Clone)]
pub struct Nco {
    phase: f64,
    step: f64,
}

impl Nco {
    /// Create an oscillator emitting `freq_hz` at `sample_rate` samples
    /// per second. Negative frequencies rotate clockwise (used for
    /// down-mixing an IF to baseband).
    #[must_use]
    pub const fn new(freq_hz: f64, sample_rate: f64) -> Self {
        Self {
            phase: 0.0,
            step: std::f64::consts::TAU * freq_hz / sample_rate,
        }
    }

    /// Next unit-magnitude sample.
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

    #[test]
    fn matches_reference_cosine_over_long_run() {
        let mut nco = Nco::new(1_000.0, 48_000.0);
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
    }

    proptest! {
        #[test]
        fn unit_magnitude_and_constant_phase_step(
            freq in -20_000.0_f64..20_000.0,
            steps in 1_usize..2_000,
        ) {
            let mut nco = Nco::new(freq, 48_000.0);
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
}
