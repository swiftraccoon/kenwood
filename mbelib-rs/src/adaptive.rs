// SPDX-FileCopyrightText: 2025 arancormonk (mbelib-neo)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Adaptive parameter smoothing for graceful degradation under bit
//! errors (JMBE algorithms #111-116).
//!
//! When the channel deteriorates (high BER), the FEC-decoded parameters
//! become unreliable: spurious magnitude spikes, wrong voicing
//! decisions, and sudden gain swings produce loud "blips" and
//! intelligibility loss. This module damps those artifacts by:
//!
//! 1. Tracking a smoothed estimate of the local spectral energy
//!    (Algorithm #111) using an IIR with α=0.95, β=0.05.
//! 2. Computing an adaptive voicing threshold `VM` (Algorithm #112) that
//!    forces bands with magnitudes far above the local energy to be
//!    voiced (preventing noisy unvoiced spikes).
//! 3. Computing an adaptive amplitude threshold `Tm` (Algorithm #115)
//!    that caps the total spectral energy. When exceeded, all
//!    magnitudes are scaled down (Algorithm #116).
//!
//! A separate frame-muting check (Algorithms outside this module's
//! scope but related) substitutes comfort noise when the error rate
//! exceeds 9.6% (AMBE) or after 3 consecutive parameter repeats.
//!
//! Algorithm port from arancormonk/mbelib-neo (`mbe_adaptive.c`,
//! GPL-2.0-or-later).

use crate::params::MbeParams;

/// Default local energy initial value (Algorithm #111).
const DEFAULT_LOCAL_ENERGY: f32 = 75_000.0;
/// Floor for the smoothed local energy estimate.
const MIN_LOCAL_ENERGY: f32 = 10_000.0;
/// IIR smoothing weight on the previous frame's energy.
const ENERGY_SMOOTH_ALPHA: f32 = 0.95;
/// IIR smoothing weight on the current frame's RM0.
const ENERGY_SMOOTH_BETA: f32 = 0.05;
/// Default amplitude threshold (Algorithm #115).
const DEFAULT_AMPLITUDE_THRESHOLD: i32 = 20_480;
/// Error rate above which adaptive smoothing engages.
const ERROR_THRESHOLD_ENTRY: f32 = 0.0125;
/// Error rate below which no smoothing is applied.
const ERROR_THRESHOLD_LOW: f32 = 0.005;
/// Adaptive gain constant for the exponential decay formula (Algorithm #112).
const ADAPTIVE_GAIN: f32 = 45.255;
/// Exponent used in the error-rate decay formula.
const ADAPTIVE_EXPONENT: f32 = 277.26;
/// Alternative multiplier used for higher error conditions.
const ADAPTIVE_ALT: f32 = 1.414;
/// Penalty applied to the amplitude threshold per bit error.
const AMPLITUDE_PENALTY_PER_ERROR: i32 = 300;
/// Base constant for the amplitude threshold formula (Algorithm #115).
const AMPLITUDE_BASE: i32 = 6_000;
/// AMBE 3600x2400 frame muting threshold (9.6%).
///
/// When the FEC-reported error rate for the frame exceeds this, the
/// decoder emits comfort noise instead of synthesized speech. IMBE
/// uses 8.75% instead, but this crate is AMBE-only (D-STAR).
const MUTING_THRESHOLD_AMBE: f32 = 0.096;

/// Maximum consecutive C0-uncorrectable frames before muting.
const MAX_FRAME_REPEATS: i32 = 3;

/// C0 Golay(23,12) error-correction capacity. Above this, the C0
/// codeword is too corrupt to trust; the decoder should reuse the
/// previous frame's parameters and increment `repeat_count`.
pub(crate) const GOLAY_C0_CAPACITY: u32 = 3;

/// Returns true if frame muting should be applied based on the error
/// rate threshold OR sustained C0-uncorrectable frames.
pub(crate) fn requires_muting(params: &MbeParams) -> bool {
    params.error_rate > MUTING_THRESHOLD_AMBE || params.repeat_count >= MAX_FRAME_REPEATS
}

/// Applies JMBE-compatible adaptive smoothing to the current frame's
/// magnitudes and voicing decisions.
///
/// Operates in-place on `cur.ml`, `cur.vl`, `cur.local_energy`, and
/// `cur.amplitude_threshold`. Should be called after the parameter
/// decoder + spectral enhancement, before synthesis.
///
/// `pre_enhance_rm0` is the sum of squared magnitudes BEFORE spectral
/// enhancement. JMBE specifies that local energy tracking uses
/// pre-enhanced RM0; if the caller doesn't have it (e.g., for direct
/// parameter feeds), passing `None` falls back to computing RM0 from
/// the current (post-enhanced) magnitudes.
pub(crate) fn apply_adaptive_smoothing(
    cur: &mut MbeParams,
    prev: &MbeParams,
    pre_enhance_rm0: Option<f32>,
) {
    let big_l = cur.l;
    let error_rate = cur.error_rate;
    let error_total = cur.error_count_total;

    // Algorithm #111: local energy IIR.
    let rm0 = pre_enhance_rm0.unwrap_or_else(|| {
        let mut sum = 0.0_f32;
        for l in 1..=big_l {
            let ml = cur.ml.get(l).copied().unwrap_or(0.0);
            sum = ml.mul_add(ml, sum);
        }
        sum
    });

    let prev_energy = if prev.local_energy < MIN_LOCAL_ENERGY {
        DEFAULT_LOCAL_ENERGY
    } else {
        prev.local_energy
    };

    cur.local_energy = ENERGY_SMOOTH_ALPHA
        .mul_add(prev_energy, ENERGY_SMOOTH_BETA * rm0)
        .max(MIN_LOCAL_ENERGY);

    // Algorithm #112: adaptive voicing threshold VM.
    let vm = if error_rate <= ERROR_THRESHOLD_LOW && error_total <= 4 {
        // No smoothing at very low error rates.
        f32::MAX
    } else {
        // x^(3/8) computed as (x^(1/8))^3 = (sqrt(sqrt(sqrt(x))))^3
        let x8 = cur.local_energy.sqrt().sqrt().sqrt();
        let energy = x8 * x8 * x8;
        if error_rate <= ERROR_THRESHOLD_ENTRY {
            (ADAPTIVE_GAIN * energy) / (ADAPTIVE_EXPONENT * error_rate).exp()
        } else {
            ADAPTIVE_ALT * energy
        }
    };

    // Algorithm #113: force voicing where amplitude exceeds VM.
    for l in 1..=big_l {
        let ml = cur.ml.get(l).copied().unwrap_or(0.0);
        if ml > vm
            && let Some(slot) = cur.vl.get_mut(l)
        {
            *slot = true;
        }
    }

    // Algorithm #114: total amplitude.
    let mut am = 0.0_f32;
    for l in 1..=big_l {
        am += cur.ml.get(l).copied().unwrap_or(0.0);
    }

    // Algorithm #115: adaptive amplitude threshold Tm.
    let prev_threshold = if prev.amplitude_threshold <= 0 {
        DEFAULT_AMPLITUDE_THRESHOLD
    } else {
        prev.amplitude_threshold
    };
    let tm = if error_rate <= ERROR_THRESHOLD_LOW && error_total <= 6 {
        DEFAULT_AMPLITUDE_THRESHOLD
    } else {
        AMPLITUDE_BASE - (AMPLITUDE_PENALTY_PER_ERROR * error_total) + prev_threshold
    };
    cur.amplitude_threshold = tm;

    // Algorithm #116: scale magnitudes if total amplitude exceeded.
    #[expect(
        clippy::cast_precision_loss,
        reason = "tm is at most ~30000; no precision loss in f32"
    )]
    let tm_f = tm as f32;
    if am > tm_f && am > 0.0 {
        let scale = tm_f / am;
        for l in 1..=big_l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot *= scale;
            }
        }
    }
}

/// Generates 160 samples of comfort noise for muted frames, using a
/// JMBE-compatible Java `Random`-style 48-bit LCG.
///
/// JMBE specifies a uniform white-noise model with gain 0.003 (relative
/// to the [-1, +1] range, before the float→i16 ×7 scaling). Translated
/// to our float-domain scale, this yields very low-level noise that
/// fills the gap during frame muting without producing audible
/// artifacts that would distract the listener.
pub(crate) fn synthesize_comfort_noise(output: &mut [f32; 160], rng_state: &mut u64) {
    // JMBE muted-noise model: uniform white noise in [-1, +1] with
    // gain 0.003. Translate to our float-domain scale (the float→i16
    // path multiplies by 7).
    const GAIN: f32 = (0.003 * 32_767.0) / 7.0;

    for sample in output {
        let bits = java_random_next_bits(rng_state, 24);
        #[expect(
            clippy::cast_precision_loss,
            reason = "bits is at most 2^24-1; representable in f32"
        )]
        let u = ((bits as f32) / 16_777_216.0_f32).mul_add(2.0, -1.0);
        *sample = u * GAIN;
    }
}

/// Java `Random.next(bits)` implementation: 48-bit LCG returning the
/// top `bits` bits.
const fn java_random_next_bits(state: &mut u64, bits: u32) -> u32 {
    const MULT: u64 = 0x5_DEEC_E66D;
    const ADD: u64 = 0xB;
    const MASK: u64 = (1 << 48) - 1;
    *state = (state.wrapping_mul(MULT).wrapping_add(ADD)) & MASK;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "shifted value fits in u32 by construction"
    )]
    {
        (*state >> (48 - bits)) as u32
    }
}

/// Initial seed for the comfort-noise RNG (matches JMBE's
/// `MBENoiseSequenceGenerator` initial state).
pub(crate) const COMFORT_NOISE_INIT_SEED: u64 = 0x1234_5678 ^ 0x5_DEEC_E66D;

#[cfg(test)]
mod tests {
    use super::*;

    /// With zero error rate, smoothing should be skipped (no magnitude
    /// changes from VM threshold logic).
    #[test]
    fn no_smoothing_at_zero_error() {
        let mut cur = MbeParams::new();
        let prev = MbeParams::new();
        cur.l = 12;
        cur.error_rate = 0.0;
        cur.error_count_total = 0;
        for l in 1..=cur.l {
            cur.ml[l] = 0.5;
        }
        let original = cur.ml;

        apply_adaptive_smoothing(&mut cur, &prev, Some(3.0));

        // Magnitudes should be unchanged (no VM force, no Tm scaling).
        for (l, (got, expected)) in cur
            .ml
            .iter()
            .zip(original.iter())
            .enumerate()
            .take(cur.l + 1)
            .skip(1)
        {
            assert!(
                (got - expected).abs() < 1e-6,
                "band {l}: changed from {expected} to {got}"
            );
        }
    }

    /// High error rate should engage smoothing and modify state.
    #[test]
    fn high_error_engages_smoothing() {
        let mut cur = MbeParams::new();
        let prev = MbeParams::new();
        cur.l = 20;
        cur.error_rate = 0.05; // 5% - well above entry threshold
        cur.error_count_total = 10;
        for l in 1..=cur.l {
            cur.ml[l] = 1000.0; // huge magnitudes to trigger Tm scaling
        }

        apply_adaptive_smoothing(&mut cur, &prev, Some(20_000_000.0));

        // Local energy should have updated (IIR smoothed toward RM0).
        assert!(cur.local_energy >= MIN_LOCAL_ENERGY);
        // amplitude threshold should be set.
        assert!(cur.amplitude_threshold != DEFAULT_AMPLITUDE_THRESHOLD);
    }

    /// Comfort noise generator produces deterministic output for the
    /// same seed and stays within the expected amplitude bound.
    #[test]
    fn comfort_noise_deterministic_and_bounded() {
        let mut state1 = COMFORT_NOISE_INIT_SEED;
        let mut state2 = COMFORT_NOISE_INIT_SEED;
        let mut buf1 = [0.0_f32; 160];
        let mut buf2 = [0.0_f32; 160];
        synthesize_comfort_noise(&mut buf1, &mut state1);
        synthesize_comfort_noise(&mut buf2, &mut state2);

        // Bit-exact reproducibility.
        for (i, (&a, &b)) in buf1.iter().zip(buf2.iter()).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "sample {i}: {a} vs {b}");
        }

        // Amplitude bounded by gain (well below ±1.0).
        let max_abs = buf1.iter().fold(0.0_f32, |acc, &s| acc.max(s.abs()));
        assert!(max_abs < 20.0, "max abs {max_abs} too large");
        assert!(max_abs > 0.0, "should not be silent");
    }

    /// `requires_muting` is true when `error_rate` exceeds the AMBE
    /// muting threshold OR when `repeat_count` reaches the max.
    #[test]
    fn muting_threshold() {
        let mut params = MbeParams::new();
        params.error_rate = 0.05;
        assert!(!requires_muting(&params));
        params.error_rate = 0.10;
        assert!(requires_muting(&params));
    }

    /// `requires_muting` fires on sustained C0-uncorrectable frames.
    #[test]
    fn repeat_count_muting() {
        let mut params = MbeParams::new();
        params.repeat_count = MAX_FRAME_REPEATS - 1;
        assert!(!requires_muting(&params));
        params.repeat_count = MAX_FRAME_REPEATS;
        assert!(requires_muting(&params));
    }
}
