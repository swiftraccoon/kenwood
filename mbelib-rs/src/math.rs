// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Lightweight numerical helpers for the AMBE decoder hot paths.
//!
//! These helpers exist to eliminate transcendental function calls (`cos`,
//! `sin`) from per-sample and per-band inner loops that otherwise dominate
//! decode time. They are not part of the public API.

/// Cosine oscillator using angle-addition recurrence for fast per-step
/// cosine evaluation.
///
/// Computes `cos(phi_0 + n * step)` for n = 0, 1, 2, ... by maintaining
/// `(c, s) = (cos(phi), sin(phi))` and advancing each call via the
/// angle-addition identity:
///
/// ```text
/// cos(phi + step) = cos(phi) * cos(step) - sin(phi) * sin(step)
/// sin(phi + step) = sin(phi) * cos(step) + cos(phi) * sin(step)
/// ```
///
/// This replaces a per-step `cos()` call (~20-30 cycles) with two fused
/// multiply-add ops plus two multiplies (~6-10 cycles total). For tight
/// inner loops in synthesis (160 samples per band) and the DCT/IDCT
/// (8×8 to 17×17 element passes), the speedup on the cosine work alone
/// is 3-8×.
///
/// Numerical drift accumulates as roughly `n * f32_epsilon`. For our
/// largest inner-loop count (160 samples in voiced synthesis) the
/// absolute error is ~2e-5, well below audio perception thresholds and
/// well within the 1e-4 tolerances used in the unit tests.
///
/// Pattern adapted from `mbe_fill_voiced_cos_block4` in
/// arancormonk/mbelib-neo (GPL-2.0-or-later); the recurrence formula
/// itself is the standard angle-addition identity (cf. Numerical
/// Recipes §5.5).
pub(crate) struct CosOscillator {
    /// Current `cos(phi)`.
    c: f32,
    /// Current `sin(phi)`.
    s: f32,
    /// `cos(step)`, precomputed once per oscillator.
    c_step: f32,
    /// `sin(step)`, precomputed once per oscillator.
    s_step: f32,
}

impl CosOscillator {
    /// Creates an oscillator at initial phase `phi_0` advancing by `step`
    /// radians per call to [`tick`](Self::tick).
    #[inline]
    pub(crate) fn new(phi_0: f32, step: f32) -> Self {
        let (s, c) = phi_0.sin_cos();
        let (s_step, c_step) = step.sin_cos();
        Self {
            c,
            s,
            c_step,
            s_step,
        }
    }

    /// Returns the current `cos(phi)` and advances the oscillator state
    /// by one step.
    #[inline]
    pub(crate) fn tick(&mut self) -> f32 {
        let c = self.c;
        let c_next = c.mul_add(self.c_step, -self.s * self.s_step);
        let s_next = self.s.mul_add(self.c_step, c * self.s_step);
        self.c = c_next;
        self.s = s_next;
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `CosOscillator` output should match `cos()` within float tolerance
    /// across a typical synthesis loop length.
    #[test]
    fn matches_cos_over_160_steps() {
        let phi_0 = 0.7_f32;
        let step = 0.05_f32;
        let mut osc = CosOscillator::new(phi_0, step);
        for n in 0..160 {
            #[expect(
                clippy::cast_precision_loss,
                reason = "n is at most 160; no precision loss"
            )]
            let expected = (n as f32).mul_add(step, phi_0).cos();
            let got = osc.tick();
            let err = (got - expected).abs();
            assert!(
                err < 1e-4,
                "step {n}: expected {expected}, got {got}, err {err}"
            );
        }
    }

    /// Step of zero should return constant cosine across all ticks.
    #[test]
    fn zero_step_constant_output() {
        let phi_0 = 1.234_f32;
        let mut osc = CosOscillator::new(phi_0, 0.0);
        let initial = osc.tick();
        for _ in 0..10 {
            let got = osc.tick();
            // Should match initial within float tolerance.
            assert!(
                (got - initial).abs() < 1e-5,
                "expected {initial}, got {got}"
            );
        }
    }

    /// Initial tick should return `cos(phi_0)` exactly (modulo `sin_cos`
    /// rounding, which is bit-deterministic).
    #[test]
    fn initial_tick_returns_cos_phi_0() {
        let phi_0 = 0.5_f32;
        let mut osc = CosOscillator::new(phi_0, 0.1);
        let got = osc.tick();
        let expected = phi_0.cos();
        assert!(
            (got - expected).abs() < 1e-7,
            "expected {expected}, got {got}"
        );
    }
}
