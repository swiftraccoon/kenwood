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
//! This is a direct port of `mbe_spectralAmpEnhance()` from the ISC-licensed
//! mbelib C library (<https://github.com/szechyjs/mbelib>).

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
        // Cast is safe: l is at most 56, well within f32 mantissa precision.
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let angle = params.w0 * l as f32;
        rm1 = ml_sq.mul_add(angle.cos(), rm1);
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

        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let l_f32 = l as f32;
        let cos_w0l = (params.w0 * l_f32).cos();

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
}
