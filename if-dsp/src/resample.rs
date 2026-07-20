//! Integer-factor rate conversion: FIR-protected decimation and
//! zero-stuffing interpolation.

use crate::Complex32;
use crate::fir::{FirReal, FirRealTaps, design_lowpass};

/// Anti-aliased decimator: lowpass then keep one sample in `factor`.
#[derive(Debug, Clone)]
pub struct Decimator {
    fir: FirRealTaps,
    factor: usize,
    phase: usize,
}

impl Decimator {
    /// Build a decimator. `cutoff_hz` is the anti-alias corner at the
    /// input rate (must be below `input_rate / (2 * factor)`).
    #[must_use]
    pub fn new(factor: usize, cutoff_hz: f32, input_rate: f32, taps: usize) -> Self {
        Self {
            fir: FirRealTaps::new(design_lowpass(cutoff_hz, input_rate, taps)),
            factor,
            phase: 0,
        }
    }

    /// Filter `input` and append every `factor`-th sample to `out`
    /// (which is cleared first). Phase carries across calls, so
    /// arbitrary chunk sizes are fine.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        out.clear();
        for &x in input {
            let y = self.fir.push(x);
            if self.phase == 0 {
                out.push(y);
            }
            self.phase = (self.phase + 1) % self.factor;
        }
    }
}

/// Zero-stuffing interpolator: insert `factor - 1` zeros per input
/// sample, lowpass at the output rate, gain-compensate by `factor`.
#[derive(Debug, Clone)]
pub struct Interpolator {
    fir: FirReal,
    factor: usize,
    gain: f32,
}

impl Interpolator {
    /// Build an interpolator. `cutoff_hz` is the image-reject corner at
    /// the output rate (must be below `output_rate / (2 * factor)`).
    #[expect(
        clippy::cast_precision_loss,
        reason = "interpolation factors are tiny integers; usize -> f32 is exact"
    )]
    #[must_use]
    pub fn new(factor: usize, cutoff_hz: f32, output_rate: f32, taps: usize) -> Self {
        Self {
            fir: FirReal::new(design_lowpass(cutoff_hz, output_rate, taps)),
            factor,
            gain: factor as f32,
        }
    }

    /// Interpolate `input` into `out` (cleared first); output length is
    /// `input.len() * factor`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrum_test_support::tone_amplitude;

    #[test]
    fn decimate_then_interpolate_preserves_a_tone() {
        let mut dec = Decimator::new(4, 5_000.0, 48_000.0, 63);
        let mut int = Interpolator::new(4, 5_000.0, 48_000.0, 63);
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
        let steady = out.get(4_800..).unwrap_or(&[]);
        let amp = tone_amplitude(steady, 48_000.0, 1_000.0);
        assert!(
            (amp - 1.0).abs() < 0.05,
            "round-trip amplitude {amp}, expected ~1.0"
        );
    }

    #[test]
    fn decimator_rejects_out_of_band_energy() {
        let mut dec = Decimator::new(4, 5_000.0, 48_000.0, 63);
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
        let steady = real.get(1_200..).unwrap_or(&[]);
        let leaked = tone_amplitude(steady, 12_000.0, 2_000.0);
        assert!(leaked < 0.02, "aliased leakage {leaked} (want < -34 dB)");
    }
}
