// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/v_uv_det.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Per-band voiced/unvoiced decisions + integrated spectral amplitude
//! extraction.
//!
//! Port of OP25's `imbe_vocoder::v_uv_det`. For each harmonic in the
//! `±f0/2` analysis window, fit a weighted sinusoid via the spectral
//! response window [`WR_SP`](crate::encode::wr_sp::WR_SP), then decide
//! voicing per band from the reconstruction-error-to-energy ratio
//! `Dk = D_num / D_den`. Bands group 3 adjacent harmonics except the
//! final band which takes the remainder.
//!
//! # Differences from a bare V/UV
//!
//! The OP25 algorithm couples V/UV with spectral amplitude
//! extraction: the same sinusoidal-fit pass that produces the
//! per-bin fitted amplitude feeds into both the `Dk` ratio (V/UV
//! output) and the per-harmonic SA output. Re-running extraction in
//! a separate pass using centre-bin integration (our old
//! [`extract_spectral_amplitudes`](crate::encode::extract_spectral_amplitudes))
//! produces different numerical SAs because the 3-bin-power
//! integration doesn't account for the Hamming spectral lobe the
//! analysis window imparts on each harmonic.
#![expect(
    clippy::indexing_slicing,
    reason = "V/UV detection: iterates per harmonic and per-bin over the fitted \
              sinusoid response; indices come from the harmonic count L (<= 56 by \
              IMBE spec) and the fixed WR_SP analysis window bounds. All array \
              accesses are bounded by the analysis-stage invariants — the FFT bins, \
              the fitted amplitudes, and the per-band window offsets are all \
              algorithmically defined. `.get()?` on every access would overwhelm the \
              reference-algorithm correspondence this file maintains with OP25 \
              `v_uv_det.cc`."
)]
//!
//! # State carried across frames
//!
//! [`VuvState`] holds the `v_uv_dsn` hysteresis array (previous
//! frame's decision per band) and the ``th_max`` sliding frame-quality
//! maximum. Both are part of OP25's steady-state behaviour:
//!
//! - Hysteresis: a band that was voiced last frame has a lower bar
//!   to stay voiced (threshold 0.5625 · `M_fcn` − …). An unvoiced band
//!   has a higher bar to become voiced (threshold 0.45 · `M_fcn` − …).
//!   Prevents per-frame V/UV ping-pong on borderline signals.
//!
//! - `th_max`: slow-update ceiling of total frame energy. When the
//!   current frame's total energy `th0 > th_max`, `th_max` jumps to
//!   the midpoint; otherwise it decays at 0.99 per frame. The
//!   V/UV threshold is scaled by `M_fcn = (th0 + 0.0025·th_max) /
//!   (th0 + 0.01·th_max)`, which is near 1 for loud frames and
//!   approaches 0.25 for very quiet ones. Quiet frames are thus
//!   pushed toward unvoiced (the simpler, noisier synthesis mode).

use realfft::num_complex::Complex;

use crate::encode::spectral::{MAX_HARMONICS, SpectralAmplitudes};
use crate::encode::wr_sp::{WR_SP, WR_SP_CENTER};

/// Maximum number of V/UV bands (12 per AMBE spec).
pub const MAX_BANDS: usize = 12;

/// Harmonics per band (OP25 `NUM_HARMS_PER_BAND`).
const HARMS_PER_BAND: usize = 3;

/// Per-stream V/UV state carried across frames.
#[derive(Debug, Clone, Copy)]
pub struct VuvState {
    /// Previous-frame V/UV decision per band. Used to bias the
    /// current-frame threshold: a band that was voiced is easier to
    /// keep voiced (hysteresis).
    prev_voiced: [bool; MAX_BANDS],
    /// Slow-update frame-energy ceiling used to compute `M_fcn`, the
    /// per-frame threshold multiplier. Approaches the peak frame
    /// energy seen so far; decays at 0.99 per frame when the current
    /// frame is below the peak.
    th_max: f32,
}

impl VuvState {
    /// Fresh state — no hysteresis, `th_max` starts at 0.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_voiced: [false; MAX_BANDS],
            th_max: 0.0,
        }
    }
}

impl Default for VuvState {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-frame V/UV decision vector.
#[derive(Debug, Clone, Copy)]
pub struct VuvDecisions {
    /// `true` = voiced band (periodic content dominates), `false` =
    /// unvoiced (noise-like). Only the first `num_bands` entries
    /// are valid; the rest are padding zeros.
    pub voiced: [bool; MAX_BANDS],
    /// Number of active harmonic bands for this frame (derived from
    /// the pitch).
    pub num_bands: usize,
}

/// Stateless one-shot V/UV convenience wrapper.
///
/// Builds a fresh [`VuvState`] internally and calls
/// [`detect_vuv_and_sa`] with a neutral `e_p = 0.5`. Discards the
/// spectral amplitudes. Exposed for validators and tests that want a
/// simple "classify this spectrum" call; the encoder itself uses
/// [`detect_vuv_and_sa`] directly so that hysteresis + ``th_max`` carry
/// across frames.
#[must_use]
pub fn detect_vuv(fft_out: &[Complex<f32>], f0_bin: f32) -> VuvDecisions {
    let mut state = VuvState::new();
    let (vuv, _sa) = detect_vuv_and_sa(fft_out, f0_bin, &mut state, 0.5);
    vuv
}

/// Integrated V/UV + spectral amplitude extraction.
///
/// Ports OP25's `v_uv_det`. For each harmonic `k ∈ 1..=num_harms`,
/// extracts the analysis window `[k·f0 − f0/2, k·f0 + f0/2]` from
/// the FFT, fits a windowed sinusoid, and accumulates error and
/// energy. Every `HARMS_PER_BAND` harmonics, commits a band decision
/// based on `Dk = D_num / D_den < dsn_thr`.
///
/// `e_p` is the current-frame pitch-error metric (output of the
/// pitch tracker); passing a large value (`> 0.55`) disables voicing
/// in all but the first band — the pitch quality is too low to trust
/// the harmonic model.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::too_many_lines,
    reason = "Mirrors OP25's v_uv_det block so the port can be cross-referenced line-by-line. \
              DSP bin math: indices are bounded by the FFT length (256), harmonic count is \
              capped at MAX_HARMONICS (56), so the many f32/usize casts in the windowed \
              sinusoid fit are all within safe magnitudes. Splitting into smaller helpers \
              would break the line-by-line mapping with the reference."
)]
pub fn detect_vuv_and_sa(
    fft_out: &[Complex<f32>],
    f0_bin: f32,
    state: &mut VuvState,
    e_p: f32,
) -> (VuvDecisions, SpectralAmplitudes) {
    // Derive num_harms / num_bands from pitch. OP25:
    //   num_harms = min(max(int((pitch/2 + 0.5) · 0.9254), NUM_HARMS_MIN), NUM_HARMS_MAX)
    // In our bin-domain, pitch_period_samples = 256 / f0_bin.
    let period = if f0_bin > 0.5 { 256.0 / f0_bin } else { 256.0 };
    // OP25 computes `num_harms` via TWO successive integer
    // truncations (`v_uv_det.cc:117-118`):
    //
    //   tmp       = shr(add(shr(ref_pitch, 1), CNST_0_25_Q8_8), 8)
    //             = int_part(period / 2 + 0.25)
    //   num_harms = extract_h(tmp * CNST_0_9254_Q0_16)
    //             = int_part(tmp * 0.9254)
    //
    // The inner truncation matters: a pure
    // `floor((period/2 + 0.5) * 0.9254)` (our previous formula) is
    // off-by-one on many mid-range pitches — e.g. `period = 54.75`
    // gives `(27.375 + 0.5) * 0.9254 = 25.8 → 25`, while OP25's
    // pathway gives `floor(27.375 + 0.25) * 0.9254 = 27 * 0.9254 =
    // 24.98 → 24`. That 1-harmonic disagreement shifts
    // `L_TABLE`-indexed `b0` selection and cascades into b1/b2/b3
    // quantizer searches. Matched exactly here.
    let num_harms: usize = {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "OP25-parity num_harms inner truncation: period is in (0, 1024], so \
                      `period*0.5 + 0.25` is < 513; the float-to-usize truncation matches \
                      OP25's Q8.8 arithmetic exactly (see comment above)."
        )]
        let tmp = (period * 0.5 + 0.25) as usize;
        #[expect(
            clippy::cast_precision_loss,
            reason = "tmp <= 513 from the truncation above; usize-to-f32 cast is exact."
        )]
        let raw = tmp as f32 * 0.9254;
        let clamped = raw.max(9.0).min(MAX_HARMONICS as f32);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Clamped above to [9.0, MAX_HARMONICS (56)]; float-to-usize truncation \
                      is exact within this range."
        )]
        let n = clamped as usize;
        n
    };
    let num_bands = if num_harms <= 36 {
        ((num_harms + 2) / HARMS_PER_BAND).min(MAX_BANDS)
    } else {
        MAX_BANDS
    };

    // ── Frame-level quality metric `M_fcn` ──────────────────────
    //
    // `energy_low` = low-half-spectrum energy, `energy_high` =
    // high-half, `energy_total` = sum. Slow-update `th_max` so
    // `M_fcn` reflects "this frame vs recent loudest". OP25's
    // thresholds match int16 signal scale; we operate on normalized
    // f32 [-1, 1), so the numerical magnitudes are tiny (~1e-4 for
    // speech). `M_fcn`'s ratio form keeps it in [~0.25, ~1.0]
    // regardless.
    let bin_half = fft_out.len() / 2;
    let energy_low: f32 = fft_out
        .iter()
        .take(bin_half)
        .map(Complex::<f32>::norm_sqr)
        .sum();
    let energy_high: f32 = fft_out
        .iter()
        .skip(bin_half)
        .map(Complex::<f32>::norm_sqr)
        .sum();
    let energy_total = energy_low + energy_high;
    state.th_max = if energy_total > state.th_max {
        0.5 * (state.th_max + energy_total)
    } else {
        0.99_f32.mul_add(state.th_max, 0.01 * energy_total)
    };
    let mut m_fcn = {
        let num = 0.0025_f32.mul_add(state.th_max, energy_total);
        let den = 0.01_f32.mul_add(state.th_max, energy_total);
        if den < 1e-30 { 0.25 } else { num / den }
    };
    // If low-frequency energy is much smaller than high-frequency,
    // dampen `M_fcn` (the signal lacks the low-F structure typical of
    // speech, so the voicing decision needs more evidence).
    let hi5 = 5.0 * energy_high;
    if energy_low < hi5 && hi5 > 1e-30 {
        m_fcn *= (energy_low / hi5).sqrt();
    }

    // ── Per-harmonic sinusoidal fit ────────────────────────────
    let mut sa = [0.0_f32; MAX_HARMONICS];
    // Cache the sc_coef and window energy per harmonic so the final
    // SA calculation can reuse them after the band decision lands.
    let mut m_num = [0.0_f32; MAX_HARMONICS]; // observed window energy
    let mut sc_coef = [0.0_f32; MAX_HARMONICS]; // 1 / Σ wr_sp² for this harmonic
    let mut bin_counts = [0_usize; MAX_HARMONICS]; // bins used per harmonic

    let mut voiced = [false; MAX_BANDS];
    let mut d_num = 0.0_f32;
    let mut d_den = 0.0_f32;
    let mut band_cnt = 0_usize;
    let mut num_harms_cnt = 0_usize;
    let mut dsn_thr = 0.0_f32;
    // Cumulative band-center frequency (scaled to bin_half span). Used
    // to damp thresholds at high-frequency bands where harmonic
    // energy is typically lower.
    let mut band_center_norm = 0.0_f32;
    let band_center_step = f0_bin * HARMS_PER_BAND as f32 / (bin_half as f32);

    for k in 0..num_harms {
        let center = (k as f32 + 1.0) * f0_bin;
        let half_win = f0_bin * 0.5;
        let bin_lo = (center - half_win).ceil().max(0.0) as usize;
        let bin_hi = ((center + half_win).ceil() as usize).min(fft_out.len());
        if bin_lo >= bin_hi {
            continue;
        }
        bin_counts[k] = bin_hi - bin_lo;

        // Compute per-band threshold once per band.
        if num_harms_cnt == 0 {
            dsn_thr = if e_p > 0.55 && band_cnt >= 1 {
                0.0
            } else if state.prev_voiced[band_cnt] {
                (-0.1741_f32).mul_add(band_center_norm, 0.5625) * m_fcn
            } else {
                (-0.1393_f32).mul_add(band_center_norm, 0.45) * m_fcn
            };
            band_center_norm += band_center_step;
        }

        // Pass 1: windowed-sinusoid fit.
        let mut amp_re = 0.0_f32;
        let mut amp_im = 0.0_f32;
        let mut m_den_sum = 0.0_f32;
        for bin in bin_lo..bin_hi {
            let w = wr_sp_sample(bin, center);
            let c = fft_out
                .get(bin)
                .copied()
                .unwrap_or_else(|| Complex::new(0.0, 0.0));
            amp_re = c.re.mul_add(w, amp_re);
            amp_im = c.im.mul_add(w, amp_im);
            m_den_sum = w.mul_add(w, m_den_sum);
        }
        let sc = if m_den_sum > 1e-12 {
            1.0 / m_den_sum
        } else {
            0.0
        };
        sc_coef[k] = sc;
        let fit_re = amp_re * sc;
        let fit_im = amp_im * sc;

        // Pass 2: error + energy accumulation.
        let mut m_num_sum = 0.0_f32;
        for bin in bin_lo..bin_hi {
            let w = wr_sp_sample(bin, center);
            let rec_re = fit_re * w;
            let rec_im = fit_im * w;
            let c = fft_out
                .get(bin)
                .copied()
                .unwrap_or_else(|| Complex::new(0.0, 0.0));
            let err_re = c.re - rec_re;
            let err_im = c.im - rec_im;
            d_num += err_re.mul_add(err_re, err_im * err_im);
            m_num_sum += c.norm_sqr();
        }
        m_num[k] = m_num_sum;
        d_den += m_num_sum;

        // Commit band every HARMS_PER_BAND harmonics (except last).
        num_harms_cnt += 1;
        let last_harmonic = k + 1 == num_harms;
        let full_band = num_harms_cnt == HARMS_PER_BAND && band_cnt < num_bands - 1;
        let commit = full_band || last_harmonic;
        if commit {
            let dk = if d_den > 1e-12 { d_num / d_den } else { 1.0 };
            let band_voiced = dk < dsn_thr;
            voiced[band_cnt] = band_voiced;

            // Emit SA for the harmonics that make up this band.
            let first_k = (k + 1) - num_harms_cnt;
            for kk in first_k..=k {
                sa[kk] = if band_voiced {
                    voiced_sa_calc(m_num[kk], sc_coef[kk])
                } else {
                    unvoiced_sa_calc(m_num[kk], bin_counts[kk])
                };
            }

            d_num = 0.0;
            d_den = 0.0;
            num_harms_cnt = 0;
            band_cnt += 1;
        }
    }

    state.prev_voiced = voiced;
    let vuv = VuvDecisions { voiced, num_bands };
    let amps = SpectralAmplitudes {
        magnitudes: sa,
        num_harmonics: num_harms,
    };
    (vuv, amps)
}

/// Look up `WR_SP[round(WR_SP_CENTER + (bin − harmonic_center) · 64)]`
/// with bounds-check, returning `0.0` outside the table. Centralises
/// the f32→index arithmetic + sign/precision-loss justification for
/// every site that samples the spectral-response window.
#[inline]
fn wr_sp_sample(bin: usize, harmonic_center: f32) -> f32 {
    // `bin` < 129 for a 256-pt real FFT, so `bin as f32` is lossless.
    // `harmonic_center` ∈ (0, 128) for any valid pitch. The `.round()`
    // output is therefore in `[WR_SP_CENTER − 64·128, WR_SP_CENTER +
    // 64·128] ⊂ i32`. Bounds check against `WR_SP_LEN` happens
    // before indexing; out-of-range returns zero.
    #[expect(
        clippy::cast_precision_loss,
        reason = "wr_sp lookup: bin is bounded by 128 (256-pt real FFT) and WR_SP_CENTER is \
                  160; both tiny magnitudes fit exactly in f32 mantissa precision."
    )]
    let offset_raw = 64.0_f32.mul_add(bin as f32 - harmonic_center, WR_SP_CENTER as f32);
    let offset = offset_raw.round();
    if !offset.is_finite() || offset < 0.0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "offset is verified finite and non-negative above; `.round()` produces an \
                  integer value from a bounded calculation, so the f32-to-usize cast is exact."
    )]
    let idx = offset as usize;
    WR_SP.get(idx).copied().unwrap_or(0.0)
}

/// Voiced spectral amplitude: square root of observed energy
/// normalized by the window-self-energy. Float port of OP25's
/// `voiced_sa_calc(M_num, M_den)` where `M_den` is `sc_coef =
/// 1 / Σ wr_sp²`.
#[inline]
fn voiced_sa_calc(m_num: f32, sc: f32) -> f32 {
    // OP25 comment: `2 * 256 * sqrt(2 * num / den)` — but den there
    // is `sc_coef = 1/M_den_sum`, so `2*num/den = 2*num*M_den_sum`.
    // In float that's `sqrt(2 · m_num · sc⁻¹) · 512`. For unit
    // harmonic amplitude (the fitted-sinusoid case) this reduces
    // algebraically to `fitted_amplitude · √(m_den_sum) · 512`.
    // We just compute `sqrt(m_num * sc⁻¹)` directly — the downstream
    // quantizer's SA_SCALE handles absolute scaling.
    if sc < 1e-12 {
        return 0.0;
    }
    (m_num / sc).sqrt()
}

/// Unvoiced spectral amplitude: RMS per-bin observed energy scaled
/// by 0.1454 (compensates for unvoiced-synthesis overshoot). Float
/// port of OP25's `unvoiced_sa_calc(M_num, bin_count)`.
#[inline]
fn unvoiced_sa_calc(m_num: f32, bin_count: usize) -> f32 {
    if bin_count == 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "bin_count is the number of FFT bins in a harmonic window, bounded by 128 \
                  (half of 256-pt real FFT); usize-to-f32 cast is exact."
    )]
    let n = bin_count as f32;
    0.1454 * (m_num / n).sqrt() * (2.0_f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::{MAX_BANDS, VuvState, detect_vuv, detect_vuv_and_sa};
    use realfft::num_complex::Complex;

    /// A silent spectrum is classified as entirely unvoiced (all
    /// bands false).
    #[test]
    fn silent_spectrum_is_unvoiced() {
        let fft_out = vec![Complex::new(0.0, 0.0); 129];
        let decisions = detect_vuv(&fft_out, 6.4);
        assert!(decisions.voiced.iter().all(|&v| !v));
    }

    /// A spectrum shaped like the analysis window's sinusoidal
    /// response — `wr_sp` centered at each `k · f0_bin` — should be
    /// classified as voiced. Single-bin-impulse inputs don't work
    /// for the integrated detector: its sinusoidal fit expects the
    /// tapered `wr_sp` spectral lobe the real encoder's analysis
    /// window produces, so impulses leave `D_num` large and the ratio
    /// test fails even though the spectrum "looks" harmonic.
    #[test]
    fn harmonic_spectrum_is_voiced() {
        use crate::encode::wr_sp::{WR_SP, WR_SP_CENTER};
        let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
        let f0_bin = 6.4_f32;
        // Paint the wr_sp lobe at each harmonic. The lobe spans ≈2.5
        // bins either side of center (160/64 ≈ 2.5), tapered to zero
        // at the edges.
        for k in 1..=10 {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test harmonic index: k in [1, 10], usize-to-f32 is exact."
            )]
            let center = f0_bin * k as f32;
            for (wr_idx, &w) in WR_SP.iter().enumerate() {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "WR_SP table index: wr_idx < 321, exact in f32."
                )]
                let offset_bins = (wr_idx as f32 - WR_SP_CENTER as f32) / 64.0;
                // `bin` is bounded: center ∈ [6.4, 64], offset_bins
                // ∈ [−2.5, 2.5], so `bin ∈ [3, 67]` — well within
                // usize and within the 129-bin FFT.
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "Bin index from the wr_sp lobe painting: the comment above \
                              establishes bin is in [3, 67], non-negative and bounded, so \
                              the float-to-usize truncation of `.round()` is exact."
                )]
                let bin = (center + offset_bins).round() as usize;
                if let Some(c) = fft_out.get_mut(bin) {
                    c.re += w;
                }
            }
        }
        let decisions = detect_vuv(&fft_out, f0_bin);
        let any_voiced = decisions.voiced.iter().any(|&v| v);
        assert!(
            any_voiced,
            "expected at least one voiced band; got {:?}",
            decisions.voiced
        );
    }

    /// A flat spectrum without clear harmonic peaks should not be
    /// called voiced.
    #[test]
    fn flat_spectrum_is_not_voiced() {
        let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
        for c in &mut fft_out {
            *c = Complex::new(0.1, 0.0);
        }
        let decisions = detect_vuv(&fft_out, 6.4);
        let voiced_count = decisions.voiced.iter().filter(|&&v| v).count();
        // A flat spectrum's sinusoidal fit has residual error
        // comparable to the signal itself → Dk is near 1 → not voiced.
        assert!(
            voiced_count < decisions.num_bands,
            "all bands voiced for flat spectrum: {voiced_count}/{}",
            decisions.num_bands
        );
    }

    /// `num_bands` ≈ `num_harms / 3`, capped at `MAX_BANDS`.
    #[test]
    fn band_count_tracks_pitch() {
        let fft_out = vec![Complex::new(0.0, 0.0); 129];
        // f0_bin=2 ⇒ period=128 ⇒ num_harms≈floor((64+0.5)·0.9254)=59→56 → num_bands=12
        let d_low = detect_vuv(&fft_out, 2.0);
        assert_eq!(d_low.num_bands, MAX_BANDS);
        // f0_bin=20 ⇒ period=12.8 ⇒ num_harms≈floor(6.4·0.9254)=5→9 (clamped) → num_bands=3
        let d_high = detect_vuv(&fft_out, 20.0);
        assert!(d_high.num_bands >= 1 && d_high.num_bands <= 4);
    }

    /// Hysteresis: a band that was voiced is easier to keep voiced
    /// on the next frame.
    #[test]
    fn hysteresis_biases_voicing_decision() {
        // Build a spectrum that's a borderline voicing case — the
        // same ratio produces different decisions depending on
        // `state.prev_voiced`.
        let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
        let f0_bin = 6.4_f32;
        for k in 1..=10 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "test harmonic bin painter: k in [1, 10] and f0_bin is 6.4, so \
                          bin = round(6.4 * k) is in [6, 64], non-negative and within 129."
            )]
            let bin = (f0_bin * k as f32).round() as usize;
            if let Some(c) = fft_out.get_mut(bin) {
                *c = Complex::new(0.6, 0.0);
            }
        }
        // Add some background noise.
        for c in &mut fft_out {
            c.re += 0.15;
        }

        // Fresh state: all bands unvoiced → harder threshold.
        let mut fresh = VuvState::new();
        let (d1, _) = detect_vuv_and_sa(&fft_out, f0_bin, &mut fresh, 0.0);

        // Biased state: band 0 was voiced → easier threshold.
        let mut biased = VuvState::new();
        biased.prev_voiced[0] = true;
        let (d2, _) = detect_vuv_and_sa(&fft_out, f0_bin, &mut biased, 0.0);

        // In the biased case, at least as many bands voiced as fresh.
        let count_fresh = d1.voiced.iter().filter(|&&v| v).count();
        let count_biased = d2.voiced.iter().filter(|&&v| v).count();
        assert!(
            count_biased >= count_fresh,
            "hysteresis failed to bias voicing: fresh={count_fresh}, biased={count_biased}"
        );
    }

    /// Integrated SA path should produce non-zero magnitudes when the
    /// spectrum has visible harmonic content.
    #[test]
    fn integrated_sa_nonzero_for_harmonic_input() {
        let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
        let f0_bin = 6.4_f32;
        for k in 1..=10 {
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "test harmonic bin painter: k in [1, 10] and f0_bin is 6.4, so \
                          bin = round(6.4 * k) is in [6, 64], non-negative and within 129."
            )]
            let bin = (f0_bin * k as f32).round() as usize;
            if let Some(c) = fft_out.get_mut(bin) {
                *c = Complex::new(1.0, 0.0);
            }
        }
        let mut state = VuvState::new();
        let (_vuv, amps) = detect_vuv_and_sa(&fft_out, f0_bin, &mut state, 0.0);
        let total: f32 = amps.magnitudes.iter().sum();
        assert!(
            total > 0.0,
            "integrated SA all zero on harmonic input ({amps:?})"
        );
    }
}
