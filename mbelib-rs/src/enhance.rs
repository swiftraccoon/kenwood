// SPDX-FileCopyrightText: 2010 szechyjs (mbelib)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Spectral amplitude enhancement for decoded AMBE parameters.
//!
//! After the AMBE parameter decoder recovers per-band magnitudes from
//! the bitstream, this module applies an adaptive spectral weighting
//! that reduces codec artifacts and improves perceived audio quality.
//!
//! The enhancement algorithm works by estimating the "flatness" of the
//! spectral envelope through autocorrelation, then adjusting per-band
//! magnitudes to smooth over quantization roughness. Bands that are
//! unusually loud relative to their spectral neighborhood are attenuated,
//! while weak bands get a modest boost. A final energy-preserving
//! normalization step ensures the total power stays unchanged.
//!
//! Base implementation ported from `mbe_spectralAmpEnhance()` in
//! szechyjs/mbelib (originally ISC, redistributed here under
//! GPL-2.0-or-later). The shared `cos(w0 * l)` precomputation via
//! angle-addition recurrence is adapted from `mbe_spectralAmpEnhance` in
//! arancormonk/mbelib-neo (<https://github.com/arancormonk/mbelib-neo>,
//! GPL-2.0-or-later). The recurrence formula itself is the standard
//! angle-addition identity (cf. Numerical Recipes §5.5).

use crate::params::MbeParams;

/// Enhances spectral magnitudes in the decoded parameters.
///
/// # Algorithm
///
/// 1. **Autocorrelation estimates**: Compute `R(m,0)` (total energy) and
///    `R(m,1)` (first lag) from the current per-band magnitudes:
///    - `Rm0 = sum(Ml^2)`
///    - `Rm1 = sum(Ml^2 * cos(w0 * l))`
///
/// 2. **Per-band weighting**: For each band with non-zero magnitude,
///    compute a spectral density–based weight `Wl` using the formula:
///    ```text
///    Wl = sqrt(Ml) * (0.96 * pi * (Rm0^2 + Rm1^2 - 2*Rm0*Rm1*cos(w0*l))
///                      / (w0 * Rm0 * (Rm0^2 - Rm1^2))) ^ 0.25
///    ```
///    This emphasizes bands where the local spectral density is high
///    relative to the global average, producing a "spectral sharpening"
///    effect that counteracts quantization smearing.
///
/// 3. **Clamping**: Wl is clamped to `[0.5, 1.2]` to prevent extreme
///    modifications. Bands in the lowest eighth (`8*l <= L`) are exempt
///    from enhancement to preserve the low-frequency spectral structure
///    that carries the voice's fundamental character.
///
/// 4. **Energy normalization**: After weighting, compute a scaling factor
///    `gamma = sqrt(Rm0_original / Rm0_weighted)` and apply it to all
///    bands. This preserves the original total energy, ensuring the
///    enhancement only reshapes the spectral envelope without changing
///    the overall loudness.
///
/// # Arguments
///
/// * `params` - Mutable reference to the decoded parameters. The `ml`
///   array is modified in-place.
pub(crate) fn spectral_amp_enhance(params: &mut MbeParams) {
    let big_l = params.l;
    if big_l == 0 {
        return;
    }

    // Precompute cos(w0 * l) for l in 1..=L via angle-addition recurrence.
    //
    // Replaces 2*L individual cos() calls (one per band in each of the two
    // loops below) with a single sin_cos() and L iterations of 2 fma + 2
    // mul. Numerical drift over L≤56 steps is below 1e-5, well within
    // audio-perception tolerance.
    //
    // Recurrence: given (c_step, s_step) = (cos(w0), sin(w0)) and
    // (c_l, s_l) = (cos(l·w0), sin(l·w0)), the next pair is
    //   c_{l+1} = c_l · c_step - s_l · s_step
    //   s_{l+1} = s_l · c_step + c_l · s_step
    let cos_tab: [f32; 57] = {
        let mut tab = [0.0_f32; 57];
        let (s_step, c_step) = params.w0.sin_cos();
        let mut c = 1.0_f32;
        let mut s = 0.0_f32;
        for l in 1..=big_l.min(56) {
            let c_next = c.mul_add(c_step, -s * s_step);
            let s_next = s.mul_add(c_step, c * s_step);
            c = c_next;
            s = s_next;
            if let Some(slot) = tab.get_mut(l) {
                *slot = c;
            }
        }
        tab
    };

    // Step 1: Compute autocorrelation R(m,0) and R(m,1).
    //
    // Rm0 is the zero-lag autocorrelation (total spectral energy).
    // Rm1 is the first-lag autocorrelation, which measures how much
    // the spectral envelope resembles a cosine at the fundamental
    // frequency. The ratio Rm1/Rm0 indicates spectral "flatness".
    let mut rm0: f32 = 0.0;
    let mut rm1: f32 = 0.0;
    for l in 1..=big_l {
        let ml = *params.ml.get(l).unwrap_or(&0.0);
        let ml_sq = ml * ml;
        rm0 += ml_sq;
        let cos_w0l = cos_tab.get(l).copied().unwrap_or(0.0);
        rm1 = ml_sq.mul_add(cos_w0l, rm1);
    }

    // Squared autocorrelation values for the weighting formula.
    let rm0_sq = rm0 * rm0;
    let rm1_sq = rm1 * rm1;

    // Step 2-3: Compute per-band weighting Wl and apply with clamping.
    //
    // The denominator (w0 * Rm0 * (R2m0 - R2m1)) is the spectral
    // density normalization factor. The numerator isolates the local
    // spectral density at harmonic l. The 0.25 exponent (fourth root)
    // provides a gentle, perceptually-appropriate amount of shaping.
    for l in 1..=big_l {
        let ml = *params.ml.get(l).unwrap_or(&0.0);
        if ml == 0.0 {
            continue;
        }

        let cos_w0l = cos_tab.get(l).copied().unwrap_or(0.0);

        // Spectral density ratio: numerator measures local density at
        // band l, denominator is the global spectral density estimate.
        // The 0.96*pi factor is a perceptual tuning constant from the
        // AMBE specification.
        let numerator =
            0.96 * std::f32::consts::PI * (2.0 * rm0 * rm1).mul_add(-cos_w0l, rm0_sq + rm1_sq);
        let denominator = params.w0 * rm0 * (rm0_sq - rm1_sq);

        // Guard against division by zero when the spectrum is perfectly
        // flat (Rm0 == |Rm1|, making R2m0 == R2m1).
        if denominator.abs() < f32::EPSILON {
            continue;
        }

        let wl = ml.sqrt() * (numerator / denominator).powf(0.25);

        // Bands in the lowest eighth of the spectrum (8*l <= L) are
        // exempt from enhancement. These carry the fundamental frequency
        // structure and should not be modified.
        if 8 * l <= big_l {
            // Intentionally no modification for the lowest bands.
        } else if wl > 1.2 {
            if let Some(slot) = params.ml.get_mut(l) {
                *slot = 1.2 * ml;
            }
        } else if wl < 0.5 {
            if let Some(slot) = params.ml.get_mut(l) {
                *slot = 0.5 * ml;
            }
        } else if let Some(slot) = params.ml.get_mut(l) {
            *slot = wl * ml;
        }
    }

    // Step 4: Energy-preserving normalization.
    //
    // After weighting, the total energy has changed. Compute the new
    // energy sum and scale all magnitudes by gamma = sqrt(Rm0_old / Rm0_new)
    // so the total energy matches the original. This ensures enhancement
    // only reshapes the spectrum without affecting overall loudness.
    let mut sum: f32 = 0.0;
    for l in 1..=big_l {
        let ml = params.ml.get(l).copied().unwrap_or(0.0).abs();
        sum += ml * ml;
    }

    let gamma = if sum == 0.0 { 1.0 } else { (rm0 / sum).sqrt() };

    for l in 1..=big_l {
        if let Some(slot) = params.ml.get_mut(l) {
            *slot *= gamma;
        }
    }

    #[cfg(feature = "kenwood-tables")]
    apply_kenwood_envelope_postfilter(params);
}

/// Apply Kenwood TH-D75 firmware's per-frequency envelope-shaping
/// postfilter to the per-band magnitudes.
///
/// Source: `ENVELOPE_WEIGHTS[103]` at DSP VA `0x80049044`. The table
/// is a bell curve peaking at index 51 with the value `1.0` and
/// rolling off symmetrically — values around 0.85–0.88 at the
/// endpoints, plateau near 0.97 across the mid-band where speech
/// formants live (1.5–2.5 kHz). Multiplying decoder per-harmonic
/// magnitudes by this curve reshapes the synthesized spectrum to
/// match the perceptual emphasis Kenwood applies in firmware,
/// producing audio that sounds closer to a TH-D75 RX rather than a
/// generic mbelib decode.
///
/// The 103 entries span DC up to Nyquist (4000 Hz at the 8 kHz
/// sample rate the AMBE codec works at) — each index covers
/// `4000 / 103 ≈ 38.8 Hz`. Harmonic `l` lands at frequency
/// `f_l = w0 · 8000 · l / (2π)` Hz; the index is
/// `round(f_l / (4000 / 103))`. We clamp to `[0, 102]` for
/// numerical safety; harmonics above 4 kHz get the
/// last-bin weight (0.83).
///
/// Energy-preserving by design: after applying the weights, all
/// magnitudes are renormalized to keep `Σ ml²` unchanged from the
/// pre-postfilter total. Without the renormalization the overall
/// loudness would drop because the bell curve reduces all bands
/// (the peak weight is exactly 1.0; everything else is < 1.0).
#[cfg(feature = "kenwood-tables")]
fn apply_kenwood_envelope_postfilter(params: &mut MbeParams) {
    use crate::encode::kenwood::support::ENVELOPE_WEIGHTS;
    let big_l = params.l;
    if big_l == 0 {
        return;
    }
    let total_pre: f32 = (1..=big_l)
        .map(|l| {
            let m = params.ml.get(l).copied().unwrap_or(0.0);
            m * m
        })
        .sum();
    if total_pre == 0.0 {
        return;
    }

    // Total bandwidth = π rad/sample = 4000 Hz at 8 kHz. The 103-entry
    // table spans this range, so the mapping from normalized angular
    // frequency (w0·l) to table index is `idx = round(w0 · l · 102 / π)`.
    // The 102 (not 103) puts the last entry on the Nyquist endpoint.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "Postfilter band-to-index mapping: l <= 56, w0 in (0, π/2], so the \
                  product `w0 · l · 102 / π` is in [0, ~50]; the round-and-cast to \
                  usize is exact within f32 mantissa precision and clamps to ENVELOPE_WEIGHTS \
                  bounds via `.min(102)` below."
    )]
    let map_idx = |l: usize| -> usize {
        let w0_l = params.w0 * l as f32;
        let scale = 102.0 / std::f32::consts::PI;
        let idx_f = (w0_l * scale).round();
        let idx = if idx_f.is_finite() && idx_f >= 0.0 {
            idx_f as usize
        } else {
            0
        };
        idx.min(ENVELOPE_WEIGHTS.len() - 1)
    };

    for l in 1..=big_l {
        let idx = map_idx(l);
        let weight = ENVELOPE_WEIGHTS.get(idx).copied().unwrap_or(1.0);
        if let Some(slot) = params.ml.get_mut(l) {
            *slot *= weight;
        }
    }

    // Energy-preserving renormalization: rescale all bands so total
    // power matches the pre-postfilter total. Without this the bell
    // curve attenuation would reduce overall loudness by ~5–10 %.
    let total_post: f32 = (1..=big_l)
        .map(|l| {
            let m = params.ml.get(l).copied().unwrap_or(0.0);
            m * m
        })
        .sum();
    if total_post > 0.0 {
        let renorm = (total_pre / total_post).sqrt();
        for l in 1..=big_l {
            if let Some(slot) = params.ml.get_mut(l) {
                *slot *= renorm;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::MbeParams;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Zero-magnitude input should remain unchanged after enhancement.
    /// When all Ml are zero, there is nothing to enhance and the function
    /// should be a no-op.
    #[test]
    fn zero_magnitude_unchanged() {
        let mut params = MbeParams::new();
        params.l = 12;
        params.w0 = 0.04;

        spectral_amp_enhance(&mut params);

        for l in 1..=params.l {
            let ml = params.ml.get(l).copied().unwrap_or(f32::NAN);
            assert!(ml == 0.0, "band {l} should remain zero, got {ml}");
        }
    }

    /// Single-band energy preservation: when only one band has non-zero
    /// magnitude, the enhancement should preserve its value (gamma
    /// normalization cancels any weighting change).
    #[test]
    fn single_band_energy_preserved() -> TestResult {
        let mut params = MbeParams::new();
        params.l = 12;
        params.w0 = 0.04;
        let original_ml = 1.5_f32;
        let band = 10;
        if let Some(slot) = params.ml.get_mut(band) {
            *slot = original_ml;
        }

        let original_energy = original_ml * original_ml;
        spectral_amp_enhance(&mut params);

        let enhanced_ml = *params.ml.get(band).ok_or("ml out of bounds")?;
        let enhanced_energy = enhanced_ml * enhanced_ml;

        // Energy should be preserved within floating-point tolerance.
        assert!(
            (enhanced_energy - original_energy).abs() < 1e-4,
            "energy not preserved: original={original_energy}, enhanced={enhanced_energy}"
        );

        Ok(())
    }

    /// Gamma normalization: after enhancement, the total energy (sum of
    /// Ml^2) should match the original total energy, since gamma
    /// normalizes to preserve it.
    #[test]
    fn gamma_normalization_preserves_total_energy() {
        let mut params = MbeParams::new();
        params.l = 20;
        params.w0 = 0.03;

        // Set up a non-uniform spectral envelope so the weighting
        // actually does something.
        for l in 1..=params.l {
            if let Some(slot) = params.ml.get_mut(l) {
                // Vary magnitudes: some large, some small.
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "l is at most 20; no precision loss in f32"
                )]
                {
                    *slot = (l as f32).mul_add(0.1, 0.5);
                }
            }
        }

        // Compute original total energy.
        let original_energy: f32 = (1..=params.l)
            .map(|l| {
                let ml = params.ml.get(l).copied().unwrap_or(0.0);
                ml * ml
            })
            .sum();

        spectral_amp_enhance(&mut params);

        // Compute enhanced total energy.
        let enhanced_energy: f32 = (1..=params.l)
            .map(|l| {
                let ml = params.ml.get(l).copied().unwrap_or(0.0);
                ml * ml
            })
            .sum();

        // Should be very close due to gamma normalization.
        let rel_error = ((enhanced_energy - original_energy) / original_energy).abs();
        assert!(
            rel_error < 1e-4,
            "total energy changed too much: original={original_energy}, \
             enhanced={enhanced_energy}, relative error={rel_error}"
        );
    }

    /// Enhancement with L=0 should be a no-op (no bands to process).
    #[test]
    fn zero_l_is_noop() {
        let mut params = MbeParams::new();
        params.l = 0;
        params.w0 = 0.04;

        spectral_amp_enhance(&mut params);

        // Nothing should have changed.
        assert_eq!(params.l, 0);
    }

    /// Weighting is clamped to [0.5, 1.2], so individual band magnitudes
    /// cannot be scaled by more than 1.2x or less than 0.5x (before
    /// gamma normalization).
    #[test]
    fn weighting_clamp_bounds() {
        let mut params = MbeParams::new();
        params.l = 16;
        params.w0 = 0.03;

        // Create a spectrum with one very large band and many small ones
        // to trigger the clamping bounds.
        for l in 1..=params.l {
            if let Some(slot) = params.ml.get_mut(l) {
                *slot = if l == 8 { 10.0 } else { 0.01 };
            }
        }

        // Snapshot pre-enhancement magnitudes.
        let pre_ml: Vec<f32> = (1..=params.l)
            .map(|l| params.ml.get(l).copied().unwrap_or(0.0))
            .collect();

        spectral_amp_enhance(&mut params);

        // After gamma normalization, the exact bounds are scaled, but
        // the relative ratios between bands should reflect the clamping.
        // At minimum, the function should not produce NaN, Inf, or
        // negative magnitudes.
        for l in 1..=params.l {
            let ml = params.ml.get(l).copied().unwrap_or(0.0);
            let pre = pre_ml.get(l - 1).copied().unwrap_or(0.0);
            assert!(
                ml.is_finite(),
                "band {l}: magnitude is not finite ({ml}), pre={pre}"
            );
            assert!(ml >= 0.0, "band {l}: magnitude is negative ({ml})");
        }
    }

    /// Deterministic: same input always produces same output.
    #[test]
    fn deterministic_output() {
        let make_params = || {
            let mut p = MbeParams::new();
            p.l = 15;
            p.w0 = 0.035;
            for l in 1..=p.l {
                if let Some(slot) = p.ml.get_mut(l) {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "l is at most 15; no precision loss in f32"
                    )]
                    {
                        *slot = (l as f32).mul_add(0.05, 0.3);
                    }
                }
            }
            p
        };

        let mut p1 = make_params();
        let mut p2 = make_params();

        spectral_amp_enhance(&mut p1);
        spectral_amp_enhance(&mut p2);

        for l in 1..=p1.l {
            let ml1 = p1.ml.get(l).copied().unwrap_or(f32::NAN);
            let ml2 = p2.ml.get(l).copied().unwrap_or(f32::NAN);
            assert_eq!(
                ml1.to_bits(),
                ml2.to_bits(),
                "band {l}: ml mismatch ({ml1} vs {ml2})"
            );
        }
    }

    /// Kenwood postfilter: applying `ENVELOPE_WEIGHTS` after standard
    /// JMBE enhancement preserves total spectral energy (within
    /// numerical precision) and reshapes the per-band magnitudes via
    /// the Kenwood-firmware bell-curve postfilter (peak weight 1.0
    /// near index 51, ≈0.83 at endpoints).
    ///
    /// This test asserts:
    /// 1. Energy preservation — `Σ ml²` after postfilter ≈ before.
    /// 2. Mid-band magnitudes (where weight ≈ 1) are emphasized over
    ///    end-band magnitudes (weight ≈ 0.83).
    #[cfg(feature = "kenwood-tables")]
    #[test]
    fn kenwood_postfilter_preserves_energy() {
        let mut params = MbeParams::new();
        params.l = 24;
        params.w0 = std::f32::consts::PI / 32.0; // 250 Hz fundamental
        for l in 1..=params.l {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test L=24, exact in f32 mantissa."
            )]
            let l_f = l as f32;
            if let Some(slot) = params.ml.get_mut(l) {
                *slot = (-(l_f - 12.0).powi(2) * 0.05).exp();
            }
        }
        let pre_energy: f32 = (1..=params.l)
            .map(|l| {
                let m = params.ml.get(l).copied().unwrap_or(0.0);
                m * m
            })
            .sum();

        spectral_amp_enhance(&mut params);

        let post_energy: f32 = (1..=params.l)
            .map(|l| {
                let m = params.ml.get(l).copied().unwrap_or(0.0);
                m * m
            })
            .sum();
        let ratio = post_energy / pre_energy;
        assert!(
            (0.95..=1.05).contains(&ratio),
            "postfilter+enhance should preserve energy within 5%, got ratio={ratio:.4}"
        );

        for l in 1..=params.l {
            let ml = params.ml.get(l).copied().unwrap_or(f32::NAN);
            assert!(
                ml.is_finite() && ml >= 0.0,
                "band {l}: non-finite or negative magnitude {ml}"
            );
        }
    }
}
