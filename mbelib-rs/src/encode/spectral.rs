// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// Algorithmic reference: Pavel Yazev's `imbe_vocoder/sa_encode.cc`
// (OP25, 2009, GPLv3). This module handles the raw harmonic
// magnitude extraction from the FFT output; the log-magnitude
// predictor + PRBA/HOC vector quantization the reference chains
// onto it lives in [`crate::encode::quantize`] (fed by our output
// via [`crate::encode::encoder::AmbeEncoder::encode_frame`]).

//! Spectral amplitude extraction: FFT bins → per-harmonic magnitudes.

use realfft::num_complex::Complex;

/// Maximum number of harmonic amplitudes (matches IMBE/AMBE L).
pub const MAX_HARMONICS: usize = 56;

/// Per-frame spectral amplitudes.
#[derive(Debug, Clone, Copy)]
pub struct SpectralAmplitudes {
    /// Linear magnitudes per harmonic. Only `num_harmonics` are valid.
    pub magnitudes: [f32; MAX_HARMONICS],
    /// Number of harmonics actually computed for this frame.
    pub num_harmonics: usize,
}

/// Extract harmonic amplitudes from the FFT half-spectrum.
///
/// For each harmonic `k` from 1 to L, integrate the magnitude across
/// the three bins nearest to `k · f0_bin` (centre bin ± 1). Using a
/// 3-bin window rather than a single round-to-nearest bin recovers
/// energy for harmonics that fall between bin centres (fractional
/// `f0_bin` from pitch periods that aren't integer factors of the
/// FFT size). Without this, harmonics at bin offset ±0.5 drop ~3 dB
/// below their true amplitude — measurable on real voice captures
/// against the DVSI chip's own extraction, where the missing energy
/// produced flat Gm vectors that the PRBA codebook search always
/// resolved to near-origin entries (flat envelope → no formants).
///
/// We use sum-of-squares then `sqrt` so the window accumulates power
/// rather than raw magnitudes; this is the canonical way to pool
/// nearby bins without the magnitude-vs-phase ambiguity.
///
/// L is limited by both the FFT size and the AMBE codec:
/// `L = min(floor((N − 1) / f0_bin), MAX_HARMONICS)`.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "Spectral amplitude extraction: fft_out.len() is <= 129 (half of 256-pt \
              real FFT + 1), k is in 1..=MAX_HARMONICS (56), and f0_bin is a positive \
              spectrum-relative fundamental frequency. All bin-index casts between usize \
              and f32 fit well within f32 mantissa and stay non-negative by construction."
)]
pub fn extract_spectral_amplitudes(fft_out: &[Complex<f32>], f0_bin: f32) -> SpectralAmplitudes {
    let mut magnitudes = [0.0_f32; MAX_HARMONICS];
    if f0_bin < 0.5 {
        return SpectralAmplitudes {
            magnitudes,
            num_harmonics: 0,
        };
    }
    let max_k = ((fft_out.len() - 1) as f32 / f0_bin).floor() as usize;
    let num_harmonics = max_k.min(MAX_HARMONICS);
    for k in 1..=num_harmonics {
        let centre = (f0_bin * k as f32).round() as usize;
        // Integrate power over the 3-bin window [centre-1, centre+1].
        // `saturating_sub` handles the k=1 boundary; the `min` on the
        // upper end handles the Nyquist boundary.
        let lo = centre.saturating_sub(1);
        let hi = (centre + 1).min(fft_out.len() - 1);
        let mut power = 0.0_f32;
        for b in lo..=hi {
            if let Some(c) = fft_out.get(b) {
                power += c.norm_sqr();
            }
        }
        if let Some(slot) = magnitudes.get_mut(k - 1) {
            *slot = power.sqrt();
        }
    }
    SpectralAmplitudes {
        magnitudes,
        num_harmonics,
    }
}

#[cfg(test)]
mod tests {
    use super::extract_spectral_amplitudes;
    use realfft::num_complex::Complex;

    #[test]
    fn extracts_sparse_harmonic_peaks() {
        let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
        let f0_bin = 6.4_f32; // 200 Hz at 8 kHz
        // Place 3 harmonics with known magnitudes.
        for (k, mag) in [(1_usize, 1.0_f32), (2, 0.5), (3, 0.25)] {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "test bin painter: k in [1, 3], f0_bin = 6.4, so bin is bounded \
                          and non-negative — all casts are exact."
            )]
            let bin = (f0_bin * k as f32).round() as usize;
            if let Some(c) = fft_out.get_mut(bin) {
                *c = Complex::new(mag, 0.0);
            }
        }
        let amps = extract_spectral_amplitudes(&fft_out, f0_bin);
        assert!(amps.num_harmonics >= 3);
        assert!((amps.magnitudes[0] - 1.0).abs() < 1e-6);
        assert!((amps.magnitudes[1] - 0.5).abs() < 1e-6);
        assert!((amps.magnitudes[2] - 0.25).abs() < 1e-6);
    }

    #[test]
    fn zero_spectrum_gives_zero_magnitudes() {
        let fft_out = vec![Complex::new(0.0, 0.0); 129];
        let amps = extract_spectral_amplitudes(&fft_out, 6.4);
        assert!(amps.magnitudes.iter().all(|&m| m == 0.0));
    }

    #[test]
    fn num_harmonics_tracks_pitch() {
        let fft_out = vec![Complex::new(0.0, 0.0); 129];
        // Low pitch → more harmonics fit below Nyquist.
        let low = extract_spectral_amplitudes(&fft_out, 3.0);
        // High pitch → fewer harmonics.
        let high = extract_spectral_amplitudes(&fft_out, 20.0);
        assert!(
            low.num_harmonics > high.num_harmonics,
            "low={}, high={}",
            low.num_harmonics,
            high.num_harmonics,
        );
    }
}
