//! Automatic gain control.

use crate::parameters::positive;
use crate::{DspError, DspParameter, SampleRate};

/// Finite positive AGC tuning parameters.
///
/// [`Self::default`] selects a 0.25 target, 5 ms attack, 250 ms decay and
/// 1,000 gain ceiling. See the [AGC example](crate#use-standalone-stages).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AgcConfig {
    /// Target peak amplitude of the output.
    target: f32,
    /// Attack time constant in milliseconds (gain reduction on loud
    /// input).
    attack_ms: f32,
    /// Decay time constant in milliseconds (gain recovery on quiet
    /// input).
    decay_ms: f32,
    /// Upper bound on applied gain (limits noise pumping on silence).
    max_gain: f32,
}

impl AgcConfig {
    /// Default listening parameters, also returned by [`Self::default`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            target: 0.25,
            attack_ms: 5.0,
            decay_ms: 250.0,
            max_gain: 1_000.0,
        }
    }

    /// Target peak amplitude.
    #[must_use]
    pub const fn target(self) -> f32 {
        self.target
    }

    /// Attack time constant in milliseconds.
    #[must_use]
    pub const fn attack_ms(self) -> f32 {
        self.attack_ms
    }

    /// Decay time constant in milliseconds.
    #[must_use]
    pub const fn decay_ms(self) -> f32 {
        self.decay_ms
    }

    /// Maximum applied gain.
    #[must_use]
    pub const fn max_gain(self) -> f32 {
        self.max_gain
    }

    /// Select a finite positive target amplitude.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub fn with_target(mut self, target: f32) -> Result<Self, DspError> {
        self.target = positive(target, DspParameter::Target)?;
        Ok(self)
    }

    /// Select a finite positive attack time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub fn with_attack_ms(mut self, milliseconds: f32) -> Result<Self, DspError> {
        self.attack_ms = positive(milliseconds, DspParameter::AttackTime)?;
        Ok(self)
    }

    /// Select a finite positive decay time in milliseconds.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub fn with_decay_ms(mut self, milliseconds: f32) -> Result<Self, DspError> {
        self.decay_ms = positive(milliseconds, DspParameter::DecayTime)?;
        Ok(self)
    }

    /// Select a finite positive gain ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidParameter`] for nonfinite or nonpositive input.
    pub fn with_max_gain(mut self, gain: f32) -> Result<Self, DspError> {
        self.max_gain = positive(gain, DspParameter::MaximumGain)?;
        Ok(self)
    }
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Envelope-tracking AGC applied in place.
#[derive(Debug, Clone)]
pub struct Agc {
    env: f32,
    attack: f32,
    decay: f32,
    target: f32,
    max_gain: f32,
}

impl Agc {
    /// Build an AGC at the configured sample rate.
    ///
    /// See the [standalone stages example](crate#use-standalone-stages).
    ///
    /// # Errors
    ///
    /// Returns [`DspError::InvalidAgcTiming`] if a rate/time combination rounds
    /// its tracking coefficient to zero at `f32` precision.
    pub fn new(config: AgcConfig, sample_rate: SampleRate) -> Result<Self, DspError> {
        let coeff = |ms: f32| {
            // Dividing by the larger factor first avoids an overflowing
            // rate*time product and premature overflow for tiny rates.
            let rate = sample_rate.get();
            let coefficient = -((-1_000.0 / rate.max(ms)) / rate.min(ms)).exp_m1();
            if coefficient > 0.0 && coefficient.is_finite() {
                Ok(coefficient)
            } else {
                Err(DspError::InvalidAgcTiming {
                    sample_rate_hz: rate,
                    milliseconds: ms,
                })
            }
        };
        Ok(Self {
            env: 0.0,
            attack: coeff(config.attack_ms)?,
            decay: coeff(config.decay_ms)?,
            target: config.target,
            max_gain: config.max_gain,
        })
    }

    /// Scale samples in place, preserving the envelope across calls.
    ///
    /// No allocation occurs. Nonfinite samples and arithmetic overflow follow
    /// [the PCM contract](crate#allocation-and-samples).
    pub fn process(&mut self, samples: &mut [f32]) {
        for s in samples {
            let a = s.abs();
            let coeff = if a > self.env {
                self.attack
            } else {
                self.decay
            };
            self.env += coeff * (a - self.env);
            let gain = (self.target / self.env.max(1e-6)).min(self.max_gain);
            *s *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn tone(amp: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|n| {
                #[expect(clippy::cast_precision_loss, reason = "small test lengths")]
                let ph = std::f64::consts::TAU * 1_000.0 * n as f64 / 12_000.0;
                #[expect(clippy::cast_possible_truncation, reason = "bounded tone samples")]
                let s = (f64::from(amp) * ph.sin()) as f32;
                s
            })
            .collect()
    }

    #[test]
    fn quiet_input_is_lifted_to_target() -> TestResult {
        let mut agc = Agc::new(AgcConfig::default(), SampleRate::new(12_000.0)?)?;
        let mut samples = tone(0.01, 24_000);
        agc.process(&mut samples);
        let tail = samples.get(18_000..).ok_or("quiet steady-state samples")?;
        let peak = tail.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(
            (peak - 0.25).abs() < 0.05,
            "converged peak {peak}, want ~0.25"
        );
        Ok(())
    }

    #[test]
    fn loud_step_is_pulled_down_quickly() -> TestResult {
        let mut agc = Agc::new(AgcConfig::default(), SampleRate::new(12_000.0)?)?;
        // Converge on a quiet tone first.
        let mut quiet = tone(0.01, 12_000);
        agc.process(&mut quiet);
        // Then a loud step: within 50 ms the output must be back under
        // twice the target.
        let mut loud = tone(1.0, 1_200);
        agc.process(&mut loud);
        let tail = loud.get(600..).ok_or("post-attack samples")?;
        let peak = tail.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(peak < 0.5, "post-attack peak {peak}, want < 0.5");
        Ok(())
    }

    #[test]
    fn parameter_and_combined_timing_boundaries_are_checked() -> TestResult {
        for value in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            let config = AgcConfig::default();
            assert!(config.with_target(value).is_err(), "target {value}");
            assert!(config.with_attack_ms(value).is_err(), "attack {value}");
            assert!(config.with_decay_ms(value).is_err(), "decay {value}");
            assert!(config.with_max_gain(value).is_err(), "gain {value}");
        }
        let slow = AgcConfig::new().with_attack_ms(f32::MAX)?;
        assert!(
            matches!(
                Agc::new(slow, SampleRate::new(f32::MAX)?),
                Err(DspError::InvalidAgcTiming { .. })
            ),
            "unrepresentable coefficient must not freeze tracking"
        );
        let mut tiny_rate = Agc::new(AgcConfig::default(), SampleRate::new(f32::from_bits(1))?)?;
        let mut sample = [0.5];
        tiny_rate.process(&mut sample);
        assert_eq!(
            sample.map(f32::to_bits),
            [0.25_f32.to_bits()],
            "a tiny rate converges in one sample"
        );
        let finite = Agc::new(
            AgcConfig::new().with_attack_ms(f32::MAX)?,
            SampleRate::new(1.0)?,
        )?;
        assert!(
            finite.attack > 0.0,
            "exp_m1 must preserve a small coefficient"
        );
        Ok(())
    }
}
