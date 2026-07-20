//! Automatic gain control.

/// AGC tuning parameters.
#[derive(Debug, Clone, Copy)]
pub struct AgcConfig {
    /// Target peak amplitude of the output.
    pub target: f32,
    /// Attack time constant in milliseconds (gain reduction on loud
    /// input).
    pub attack_ms: f32,
    /// Decay time constant in milliseconds (gain recovery on quiet
    /// input).
    pub decay_ms: f32,
    /// Upper bound on applied gain (limits noise pumping on silence).
    pub max_gain: f32,
}

impl Default for AgcConfig {
    fn default() -> Self {
        Self {
            target: 0.25,
            attack_ms: 5.0,
            decay_ms: 250.0,
            max_gain: 1_000.0,
        }
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
    /// Build an AGC for a stream at `sample_rate`.
    #[must_use]
    pub fn new(config: AgcConfig, sample_rate: f32) -> Self {
        let coeff = |ms: f32| 1.0 - (-1_000.0 / (sample_rate * ms)).exp();
        Self {
            env: 0.0,
            attack: coeff(config.attack_ms),
            decay: coeff(config.decay_ms),
            target: config.target,
            max_gain: config.max_gain,
        }
    }

    /// Scale `samples` in place toward the target amplitude.
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
    fn quiet_input_is_lifted_to_target() {
        let mut agc = Agc::new(AgcConfig::default(), 12_000.0);
        let mut samples = tone(0.01, 24_000);
        agc.process(&mut samples);
        let tail = samples.get(18_000..).unwrap_or(&[]);
        let peak = tail.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(
            (peak - 0.25).abs() < 0.05,
            "converged peak {peak}, want ~0.25"
        );
    }

    #[test]
    fn loud_step_is_pulled_down_quickly() {
        let mut agc = Agc::new(AgcConfig::default(), 12_000.0);
        // Converge on a quiet tone first.
        let mut quiet = tone(0.01, 12_000);
        agc.process(&mut quiet);
        // Then a loud step: within 50 ms the output must be back under
        // twice the target.
        let mut loud = tone(1.0, 1_200);
        agc.process(&mut loud);
        let tail = loud.get(600..).unwrap_or(&[]);
        let peak = tail.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
        assert!(peak < 0.5, "post-attack peak {peak}, want < 0.5");
    }
}
