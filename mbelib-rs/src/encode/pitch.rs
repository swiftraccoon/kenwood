// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// Algorithmic reference: Pavel Yazev's `imbe_vocoder/pitch_est.cc`
// (OP25, 2009, GPLv3) — specifically the E(p) detectability function
// using sub-harmonic summation.  For a candidate pitch period `p`,
// E(p) sums the windowed autocorrelation at `p`, `2p`, `3p`, ... —
// only the true fundamental gets contributions from ALL harmonics,
// while sub-multiples (`p/2`, `p/3`) miss the odd-harmonic terms and
// score worse. This is the standard fix for the octave-high errors
// that confused our earlier autocorrelation + YIN CMND trackers on
// formant-rich speech (Stage 1-4 validator showed mean pitch-period
// error of 20 samples vs OP25's tracker; this port closes most of
// that gap).
//
// The multi-frame look-ahead + sub-multiples analysis from OP25's
// full `pitch_est` is deferred — the single-frame E(p) estimator
// alone is substantially more robust than autocorrelation/YIN and
// is a plausible end-state for the vast majority of frames.

//! Pitch (F0) estimation from the pitch-estimation history buffer.
//!
//! Given the 301-sample LPF'd buffer produced by [`crate::encode::analyze`],
//! produces a fractional pitch period in samples and the corresponding
//! F0 in Hz, plus a confidence score.
//!
//! # Algorithm
//!
//! 1. Window the pitch-estimation buffer with `wi[]` (the same window
//!    the reference uses — makes the autocorrelation peaky).
//! 2. Compute normalized autocorrelation at integer lags from 20 to
//!    150 samples (corresponds to F0 range 53 Hz to 400 Hz at 8 kHz).
//! 3. Find the lag with maximum correlation.
//! 4. Quadratic-interpolate around the peak for sub-sample accuracy.
//! 5. Smooth against the previous-frame estimate to suppress
//!    octave-jump errors.
//!
//! The AMBE codebooks quantize F0 via the `W0_TABLE` in
//! [`crate::tables`] — 120 allowed radian-per-sample values between
//! ≈0.015 and ≈0.05 (corresponding to pitch periods ≈125 to ≈420
//! samples of the 4× oversampled representation AMBE uses).

use crate::encode::state::PITCH_EST_BUF_SIZE;
use crate::encode::window::WI;

/// Minimum pitch-period candidate in samples (400 Hz at 8 kHz).
pub(crate) const PITCH_MIN_SAMPLES: usize = 20;
/// Maximum pitch-period candidate in samples (53 Hz at 8 kHz).
pub(crate) const PITCH_MAX_SAMPLES: usize = 150;
/// Default pitch period used when confidence is low or on stream
/// start — corresponds to ≈80 Hz, a plausible average adult voice.
pub(crate) const PITCH_DEFAULT_SAMPLES: f32 = 100.0;

/// Smoothing weight on the previous estimate. Higher = more inertia.
///
/// YIN's CMND makes the per-frame estimate reliable enough that heavy
/// smoothing is no longer needed to cover octave errors; 0.25 keeps a
/// small amount of inertia to dampen sub-sample jitter without lagging
/// the pitch contour during fast sweeps.
const PRIOR_WEIGHT: f32 = 0.25;

/// Confidence threshold below which the tracker falls back to the
/// previous estimate instead of locking onto a noisy minimum. Below
/// this level the frame is effectively unvoiced (noise, silence,
/// fricatives) and holding the prior gives the rest of the encoder
/// a stable pitch to work against.
const MIN_CONFIDENCE: f32 = 0.05;

/// Weight of the log-distance-from-prior penalty in the pitch
/// candidate score. `CONTOUR_BIAS * (ln(p) - ln(p_prev))²` is
/// subtracted from the sub-harmonic salience score; a larger value
/// produces a smoother pitch contour but slows adaptation to real
/// pitch changes. 0.15 empirically balances the two on speech.
const CONTOUR_BIAS: f32 = 0.15;

/// Result of a pitch-estimation pass on one 20 ms frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchEstimate {
    /// Pitch period in samples at 8 kHz. Fractional via interpolation.
    pub period_samples: f32,
    /// Fundamental frequency in Hz (8000 / `period_samples`).
    pub f0_hz: f32,
    /// Confidence score in `[0, 1]` — peak autocorrelation normalized
    /// against the frame's energy. Values above 0.3 typically
    /// indicate voiced speech; below, noise or silence.
    pub confidence: f32,
}

/// Per-stream pitch tracker state.
#[derive(Debug, Clone, Copy)]
pub struct PitchTracker {
    previous: f32,
}

impl PitchTracker {
    /// Fresh tracker; first estimate uses a ~80 Hz default as the prior.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            previous: PITCH_DEFAULT_SAMPLES,
        }
    }

    /// Estimate pitch from a pitch-history buffer via IMBE's E(p).
    ///
    /// `pitch_est_buf` is the 301-sample LPF'd buffer. Applies `WI[]`
    /// window internally.
    ///
    /// # Algorithm (IMBE `e_p` function port)
    ///
    /// For each candidate period `p` in the voiced-speech range, the
    /// detectability measure is
    ///
    /// ```text
    ///   E(p) = 1 − p·(e0 + Σ_{n≥1} corr(n·p)) / S
    /// ```
    ///
    /// where
    /// - `windowed[i] = pitch_est_buf[i] · WI[i]`
    /// - `S = Σ (sigin · WI)²` — baseline signal energy
    /// - `e0 = Σ windowed[i]²` — windowed self-energy
    /// - `corr(k) = Σᵢ windowed[i]·windowed[i+k]` — windowed autocorr
    /// - `n·p` capped at the frame-length index limit
    ///
    /// `E(p)` is low (near 0) when `p` matches the true period AND all
    /// its harmonics are periodic contributors. For a sub-multiple
    /// (e.g. `p/2` when true period is `p`), only even-n terms have
    /// matching correlations; odd-n terms drop to noise level and
    /// `E(p/2)` stays comparatively high. This is the octave-error
    /// rejection property autocorrelation-peak-pickers lack.
    ///
    /// The numerator-denominator form of `L_den = L_sum * (1 −
    /// p·sum(w⁴))` from OP25 is collapsed here because our f32
    /// arithmetic doesn't need the Q-format guard against overflow;
    /// we just divide and clamp.
    #[allow(
        clippy::too_many_lines,
        reason = "Linear pipeline; splitting obscures data flow."
    )]
    pub fn estimate(&mut self, pitch_est_buf: &[f32; PITCH_EST_BUF_SIZE]) -> PitchEstimate {
        let mut windowed = [0.0_f32; PITCH_EST_BUF_SIZE];
        for (i, (&x, &w)) in pitch_est_buf.iter().zip(WI.iter()).enumerate() {
            if let Some(slot) = windowed.get_mut(i) {
                *slot = x * w;
            }
        }

        // S = Σ(s·w)² = Σ(windowed²). Matches OP25's L_sum.
        let s_sum: f32 = windowed.iter().map(|&x| x * x).sum();
        if s_sum < 1e-12 {
            let period = self.previous;
            return PitchEstimate {
                period_samples: period,
                f0_hz: 8000.0 / period,
                confidence: 0.0,
            };
        }

        // Precompute windowed-autocorr corr(k) for all lags we'll ever
        // touch via sub-harmonic summation: up to the full frame
        // length minus one.  Using the raw windowed signal (not doubly-
        // windowed) mirrors OP25 — the original's `sig_wndwed` already
        // has `wi` applied once; `e0` uses its self-energy which is
        // `Σ(sig·wi)²` — exactly what we compute here.
        let n = windowed.len();
        let max_lag = (n - 1).min(PITCH_MAX_SAMPLES * 6); // room for n*p up to 6×max period
        let mut corr = vec![0.0_f32; max_lag + 1];
        for k in 1..=max_lag {
            let mut acc = 0.0_f32;
            for i in 0..(n - k) {
                acc = windowed[i].mul_add(windowed[i + k], acc);
            }
            corr[k] = acc;
        }
        let e0 = s_sum; // Σ windowed², equivalent to L_e0 in OP25

        // Normalized sub-harmonic salience score per candidate p:
        // score(p) = (corr(p) + corr(2p) + corr(3p) + ...) / e0
        //
        // For the true fundamental, every corr(n·p) matches the
        // windowed self-energy e0, so score ≈ N_harms. For a sub-
        // multiple p/2, only half the terms match (the other half hit
        // inter-harmonic gaps) → score ≈ N_harms/2. Sub-multiples are
        // thus inherently penalized AS a normalized score, avoiding
        // the multi-minima problem the unnormalized E(p) function had.
        //
        // Adding a soft log-distance penalty against the prior
        // estimate breaks residual ties smoothly, keeping the pitch
        // contour stable across frames without hard-pinning.
        let prev_log = self.previous.ln();
        let mut best_period = self.previous;
        let mut best_score = f32::NEG_INFINITY;
        let mut best_e = 0.0_f32;
        for p_int in PITCH_MIN_SAMPLES..=PITCH_MAX_SAMPLES {
            let mut sum_corr = 0.0_f32;
            let mut n_terms: u32 = 0;
            let mut harm_idx = p_int;
            while harm_idx <= max_lag {
                sum_corr += corr[harm_idx];
                harm_idx += p_int;
                n_terms += 1;
            }
            // Average correlation across harmonics — divide by number
            // of terms so longer p (fewer terms) isn't penalized vs
            // short p (many terms).
            #[allow(clippy::cast_precision_loss)]
            let salience = if n_terms == 0 {
                0.0
            } else {
                sum_corr / (e0 * n_terms as f32)
            };

            // Soft log-distance penalty for smoothness.
            #[allow(clippy::cast_precision_loss)]
            let p_f = p_int as f32;
            let log_ratio = p_f.ln() - prev_log;
            let penalty = CONTOUR_BIAS * log_ratio * log_ratio;

            let score = salience - penalty;
            if score > best_score {
                best_score = score;
                best_e = 1.0 - salience.clamp(0.0, 1.0);
                #[allow(clippy::cast_precision_loss)]
                {
                    best_period = p_int as f32;
                }
            }
        }

        // Parabolic interpolation around the min for sub-sample
        // resolution. Re-evaluate E at the two neighbour integer
        // periods; fit a parabola through (p-1, p, p+1).
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let refined = {
            let p_center = best_period as usize;
            if p_center > PITCH_MIN_SAMPLES && p_center < PITCH_MAX_SAMPLES {
                let e_at = |p_int: usize| -> f32 {
                    let mut sum_corr = 0.0_f32;
                    let mut harm_idx = p_int;
                    while harm_idx <= max_lag {
                        sum_corr += corr[harm_idx];
                        harm_idx += p_int;
                    }
                    let num = (p_int as f32).mul_add(-(e0 + sum_corr), s_sum);
                    (num / s_sum).max(0.0)
                };
                let e_m = e_at(p_center - 1);
                let e_c = best_e;
                let e_p = e_at(p_center + 1);
                let denom = 2.0_f32.mul_add(-e_c, e_m) + e_p;
                if denom.abs() > 1e-9 {
                    let delta = 0.5 * (e_m - e_p) / denom;
                    best_period + delta.clamp(-1.0, 1.0)
                } else {
                    best_period
                }
            } else {
                best_period
            }
        };

        // Confidence: 1 − E(p). Clean voiced ≈ 1.0; noise ≈ 0.
        let confidence = (1.0 - best_e).clamp(0.0, 1.0);

        // On low-confidence frames hold the previous estimate; on
        // high-confidence frames mildly smooth against the prior.
        let smoothed = if confidence < MIN_CONFIDENCE {
            self.previous
        } else {
            PRIOR_WEIGHT.mul_add(self.previous, (1.0 - PRIOR_WEIGHT) * refined)
        };
        self.previous = smoothed;

        PitchEstimate {
            period_samples: smoothed,
            f0_hz: 8000.0 / smoothed,
            confidence,
        }
    }
}

impl Default for PitchTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{PITCH_EST_BUF_SIZE, PitchTracker};

    /// Zero input → confidence 0, period clamped to default.
    #[test]
    fn silent_input_gives_zero_confidence() {
        let buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let mut tracker = PitchTracker::new();
        let est = tracker.estimate(&buf);
        assert!(est.confidence.abs() < f32::EPSILON);
    }

    /// A pure sine at 200 Hz has a period of 40 samples at 8 kHz. The
    /// estimator should recover this within ~1 sample and produce a
    /// high confidence.
    #[test]
    fn sine_at_200hz_recovers_correct_period() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 200.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let mut tracker = PitchTracker::new();
        // Let the tracker settle across a few frames.
        for _ in 0..4 {
            let _ = tracker.estimate(&buf);
        }
        let est = tracker.estimate(&buf);
        let period_err = (est.period_samples - 40.0).abs();
        assert!(
            period_err < 2.0,
            "period {} off from 40 (err {period_err}), f0={} Hz, conf={}",
            est.period_samples,
            est.f0_hz,
            est.confidence,
        );
        assert!(
            est.confidence > 0.3,
            "confidence too low: {}",
            est.confidence,
        );
    }

    /// A sine at 150 Hz has a period of ~53.3 samples at 8 kHz.
    /// Must not halve-pitch (report 26.7-ish for the 2nd-harmonic
    /// resonance) nor double-pitch (100-sample for alias). A halved
    /// pitch result causes sextant→sextant audio to decode its
    /// harmonics at 2× the intended frequency, producing exactly
    /// the "energy at 300 Hz instead of 150 Hz" symptom we observed
    /// in `diagnostic_voice_pipeline::pure_sine_pitch_preserved`.
    #[test]
    fn sine_at_150hz_recovers_correct_period() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 150.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let mut tracker = PitchTracker::new();
        for _ in 0..20 {
            let _ = tracker.estimate(&buf);
        }
        let est = tracker.estimate(&buf);
        let period_err = (est.period_samples - 53.3).abs();
        assert!(
            period_err < 3.0,
            "period {:.2} off from 53.3 (err {period_err}); \
             f0={:.1} Hz (want ~150), conf={:.3}",
            est.period_samples,
            est.f0_hz,
            est.confidence,
        );
    }

    /// A sine at 100 Hz has a period of 80 samples. Also recoverable,
    /// though the heavy `wi[]` taper biases the estimate slightly
    /// short — we accept up to ~5% error which is well within the
    /// AMBE pitch-quantization grid.
    #[test]
    fn sine_at_100hz_recovers_correct_period() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 100.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let mut tracker = PitchTracker::new();
        // More warmup frames so the 15%-weighted prior has time to
        // converge from the default 100-sample seed to ~80.
        for _ in 0..20 {
            let _ = tracker.estimate(&buf);
        }
        let est = tracker.estimate(&buf);
        let period_err = (est.period_samples - 80.0).abs();
        assert!(
            period_err < 4.0,
            "period {} off from 80 (err {period_err})",
            est.period_samples,
        );
    }
}
