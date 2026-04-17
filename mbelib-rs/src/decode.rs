// SPDX-FileCopyrightText: 2010 szechyjs (mbelib)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE 3600x2450 parameter decoding from error-corrected data bits.
//!
//! This module implements the core parameter extraction pipeline that converts
//! the 49-bit demodulated/error-corrected parameter vector into the harmonic
//! speech model parameters carried by [`MbeParams`].
//!
//! Ported from `mbe_decodeAmbe2450Parms()` in mbelib's `ambe3600x2450.c`
//! (<https://github.com/szechyjs/mbelib>), ISC license. Sub-field bit
//! positions, the b0..=b8 partitioning, the DCT-then-IDCT spectral
//! reconstruction sequence, and the equation-43 magnitude interpolation
//! all follow the upstream C reference exactly.
//!
//! # Decode Pipeline
//!
//! The 49 data bits are partitioned into 9 sub-fields (b0 through b8), each
//! controlling a different aspect of the speech model:
//!
//! 1. **b0 (7 bits)** -- fundamental frequency (w0) and harmonic count (L).
//!    Indexes into [`W0_TABLE`] and [`L_TABLE`]. Special codes signal
//!    erasure (120..=123), silence (124..=125), or tone frames (126..=127).
//!
//! 2. **b1 (5 bits)** -- voiced/unvoiced (V/UV) decisions per harmonic band.
//!    Indexes into [`VUV_TABLE`] which provides 8 V/UV decisions that are
//!    mapped across the L bands using frequency-proportional interpolation.
//!
//! 3. **b2 (5 bits)** -- gain delta (delta-gamma). Indexes into [`DG_TABLE`],
//!    then applies first-order smoothing: gamma = `delta_gamma` + 0.5 * `gamma_prev`.
//!
//! 4. **b3 (9 bits)** -- low-band PRBA (Prediction Residual Block Average)
//!    coefficients Gm\[2..4\] via [`PRBA24_TABLE`].
//!
//! 5. **b4 (7 bits)** -- high-band PRBA coefficients Gm\[5..8\] via
//!    [`PRBA58_TABLE`].
//!
//! 6. **b5 (5 bits), b6 (4 bits), b7 (4 bits), b8 (3 bits)** -- higher-order
//!    coefficients (HOC) for each IDCT block, via [`HOC_B5_TABLE`] through
//!    [`HOC_B8_TABLE`].
//!
//! After extracting these sub-fields, the decoder:
//!
//! - Performs a forward 8-point DCT on Gm\[1..8\] to produce Ri\[1..8\]
//! - Assembles Ci,k coefficient blocks from Ri pairs and HOC tables
//! - Runs an inverse DCT per block to produce Tl (per-band spectral offsets)
//! - Interpolates between previous and current log2 magnitudes using the
//!   band-ratio mapping (equations 40-43 from the AMBE specification)
//! - Computes `BigGamma` (global gain normalization) and final `log2_ml`
//! - Exponentiates to get linear magnitudes ml, with an unvoiced scaling factor
//!
//! # Status Codes
//!
//! The function returns a status code matching the C reference:
//! - `0` = valid voice frame, parameters fully populated
//! - `2` = erasure frame (b0 in 120..=123), unrecoverable
//! - `3` = tone signal detected (b0 in 126..=127)
//!
//! [`W0_TABLE`]: crate::tables::W0_TABLE
//! [`L_TABLE`]: crate::tables::L_TABLE
//! [`VUV_TABLE`]: crate::tables::VUV_TABLE
//! [`DG_TABLE`]: crate::tables::DG_TABLE
//! [`PRBA24_TABLE`]: crate::tables::PRBA24_TABLE
//! [`PRBA58_TABLE`]: crate::tables::PRBA58_TABLE
//! [`HOC_B5_TABLE`]: crate::tables::HOC_B5_TABLE
//! [`HOC_B8_TABLE`]: crate::tables::HOC_B8_TABLE
//! [`MbeParams`]: crate::params::MbeParams

use crate::ecc::AMBE_DATA_BITS;
use crate::math::CosOscillator;
use crate::params::{MAX_BANDS, MbeParams};
use crate::tables;

/// Result of decoding a single AMBE parameter vector.
///
/// For D-STAR, only [`FrameStatus::Voice`] produces synthesizable
/// parameters; the other two variants signal that the caller should
/// fall back to error concealment (reuse previous frame's parameters
/// and increment the repeat counter).
///
/// D-STAR does not use codec-level tone signaling (DTMF and similar
/// run over slow-data, not via AMBE tones), so in this crate
/// [`FrameStatus::Tone`] is treated identically to erasure rather than
/// being synthesized as a tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameStatus {
    /// Valid speech frame with fully decoded parameters.
    Voice,
    /// Erasure frame (b0 in 120..=123). The encoder explicitly signals
    /// that this frame is unrecoverable.
    Erasure,
    /// Tone signaling frame (b0 in 126..=127). Unused by D-STAR.
    Tone,
}

/// Number of PRBA coefficient blocks used in the forward/inverse DCT.
const PRBA_BLOCKS: usize = 8;

/// Number of IDCT sub-blocks (the L bands are split into 4 blocks).
const IDCT_BLOCKS: usize = 4;

/// Maximum HOC index per IDCT block (coefficients at k > 6 are zero).
const MAX_HOC_TERMS: usize = 6;

/// ln(2) approximation: `exp(0.693 * log2_ml) ~= 2^log2_ml`.
const LN2_APPROX: f32 = 0.693;

/// Smoothing coefficient: `gamma_cur = delta_gamma + 0.5 * gamma_prev`.
const GAIN_SMOOTH: f32 = 0.5;

/// Inter-frame interpolation weight for previous log-magnitudes (eq. 43).
const INTERP_WEIGHT: f32 = 0.65;

/// Fundamental frequency for silence frames: `2*pi / 32`.
const SILENCE_W0: f32 = std::f32::consts::TAU / 32.0;

/// Harmonic count for silence frames.
const SILENCE_L: usize = 14;

/// Decodes the 49-bit AMBE parameter vector into harmonic speech model parameters.
///
/// See the [module-level documentation](self) for a full description of the
/// decode pipeline.
///
/// Returns [`FrameStatus::Voice`] for a normal speech frame. For erasure
/// or tone frames (which D-STAR treats identically — see [`FrameStatus`]
/// docs), returns early without modifying `cur`, so the caller can
/// safely reuse the previous frame's parameters.
pub(crate) fn decode_params(
    ambe_d: &[u8; AMBE_DATA_BITS],
    cur: &mut MbeParams,
    prev: &MbeParams,
) -> FrameStatus {
    // -- b0: fundamental frequency (7 bits) --
    let b0: usize = (bit(ambe_d, 0) << 6)
        | (bit(ambe_d, 1) << 5)
        | (bit(ambe_d, 2) << 4)
        | (bit(ambe_d, 3) << 3)
        | (bit(ambe_d, 37) << 2)
        | (bit(ambe_d, 38) << 1)
        | bit(ambe_d, 39);

    match b0 {
        120..=123 => return FrameStatus::Erasure,
        126..=127 => return FrameStatus::Tone,
        _ => {}
    }

    let silence = matches!(b0, 124 | 125);
    let f0 = decode_frequency(b0, silence, cur);
    let unvc: f32 = 0.2046 / cur.w0.sqrt();

    // -- b1: V/UV decisions (5 bits) --
    decode_vuv(ambe_d, f0, silence, cur);

    // -- b2: gain delta (5 bits) --
    let b2: usize = (bit(ambe_d, 8) << 4)
        | (bit(ambe_d, 9) << 3)
        | (bit(ambe_d, 10) << 2)
        | (bit(ambe_d, 11) << 1)
        | bit(ambe_d, 36);
    let delta_gamma = *tables::DG_TABLE.get(b2).unwrap_or(&0.0);
    cur.gamma = GAIN_SMOOTH.mul_add(prev.gamma, delta_gamma);

    // -- b3/b4: PRBA coefficients -> DCT -> Cik + HOC -> IDCT -> Tl --
    let tl = decode_spectral_offsets(ambe_d, cur.l);

    // -- Magnitude interpolation and final computation --
    compute_magnitudes(cur, prev, &tl, unvc);

    FrameStatus::Voice
}

/// Decodes fundamental frequency and harmonic count from b0.
///
/// Populates `cur.w0` and `cur.l`. Returns `f0` (cycles per sample).
fn decode_frequency(b0: usize, silence: bool, cur: &mut MbeParams) -> f32 {
    if silence {
        cur.w0 = SILENCE_W0;
        cur.l = SILENCE_L;
        1.0 / 32.0
    } else {
        let f0 = *tables::W0_TABLE.get(b0).unwrap_or(&0.0);
        cur.w0 = f0 * std::f32::consts::TAU;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "L_TABLE values are whole numbers 9..=56"
        )]
        {
            cur.l = *tables::L_TABLE.get(b0).unwrap_or(&0.0) as usize;
        }
        f0
    }
}

/// Decodes V/UV decisions from b1 and maps them across L harmonic bands.
///
/// Each band is assigned to one of 8 VUV decision slots using the
/// frequency-proportional index: `jl = floor(l * 16 * f0)`.
fn decode_vuv(ambe_d: &[u8; AMBE_DATA_BITS], f0: f32, silence: bool, cur: &mut MbeParams) {
    let b1: usize = (bit(ambe_d, 4) << 4)
        | (bit(ambe_d, 5) << 3)
        | (bit(ambe_d, 6) << 2)
        | (bit(ambe_d, 7) << 1)
        | bit(ambe_d, 35);

    let vuv_row = tables::VUV_TABLE.get(b1);
    for l in 1..=cur.l {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "l is at most 56 (fits in f32 mantissa); product is small and positive; \
                      truncation to usize is the intended floor operation"
        )]
        let jl = (l as f32 * 16.0 * f0) as usize;

        if let Some(slot) = cur.vl.get_mut(l) {
            if silence {
                *slot = false;
            } else {
                let voiced = vuv_row.and_then(|row| row.get(jl)).copied().unwrap_or(0);
                *slot = voiced == 1;
            }
        }
    }
}

/// Decodes PRBA coefficients, performs forward DCT, assembles Cik with
/// HOC, and runs inverse DCT to produce per-band spectral offsets Tl.
fn decode_spectral_offsets(ambe_d: &[u8; AMBE_DATA_BITS], big_l: usize) -> [f32; MAX_BANDS] {
    // -- b3: low-band PRBA (9 bits) --
    let b3: usize = (bit(ambe_d, 12) << 8)
        | (bit(ambe_d, 13) << 7)
        | (bit(ambe_d, 14) << 6)
        | (bit(ambe_d, 15) << 5)
        | (bit(ambe_d, 16) << 4)
        | (bit(ambe_d, 17) << 3)
        | (bit(ambe_d, 18) << 2)
        | (bit(ambe_d, 19) << 1)
        | bit(ambe_d, 40);

    // -- b4: high-band PRBA (7 bits) --
    let b4: usize = (bit(ambe_d, 20) << 6)
        | (bit(ambe_d, 21) << 5)
        | (bit(ambe_d, 22) << 4)
        | (bit(ambe_d, 23) << 3)
        | (bit(ambe_d, 41) << 2)
        | (bit(ambe_d, 42) << 1)
        | bit(ambe_d, 43);

    // Assemble Gm[1..8]: Gm[1]=0, Gm[2..4] from PRBA24, Gm[5..8] from PRBA58.
    let gm = assemble_gm(b3, b4);

    // Forward DCT: Gm[1..8] -> Ri[1..8]
    let ri = forward_dct_8(&gm);

    // Cik coefficient assembly from Ri pairs
    let rconst: f32 = 1.0 / (2.0 * std::f32::consts::SQRT_2);
    let mut cik = [[0.0_f32; MAX_HOC_TERMS + 1]; IDCT_BLOCKS + 1];
    for blk in 1..=IDCT_BLOCKS {
        let r_odd = *ri.get(2 * blk - 1).unwrap_or(&0.0);
        let r_even = *ri.get(2 * blk).unwrap_or(&0.0);
        if let Some(block) = cik.get_mut(blk) {
            if let Some(c) = block.get_mut(1) {
                *c = 0.5 * (r_odd + r_even);
            }
            if let Some(c) = block.get_mut(2) {
                *c = rconst * (r_odd - r_even);
            }
        }
    }

    // -- b5-b8: HOC coefficients --
    let b5: usize = (bit(ambe_d, 24) << 4)
        | (bit(ambe_d, 25) << 3)
        | (bit(ambe_d, 26) << 2)
        | (bit(ambe_d, 27) << 1)
        | bit(ambe_d, 44);
    let b6: usize =
        (bit(ambe_d, 28) << 3) | (bit(ambe_d, 29) << 2) | (bit(ambe_d, 30) << 1) | bit(ambe_d, 45);
    let b7: usize =
        (bit(ambe_d, 31) << 3) | (bit(ambe_d, 32) << 2) | (bit(ambe_d, 33) << 1) | bit(ambe_d, 46);
    let b8: usize = (bit(ambe_d, 34) << 2) | (bit(ambe_d, 47) << 1) | bit(ambe_d, 48);

    // Look up IDCT block lengths Ji[1..4] from LMPRBL table.
    let ji = lookup_ji(big_l);

    // Fill HOC coefficients into Cik blocks.
    let hoc_tables: [&[[f32; 4]]; 4] = [
        &tables::HOC_B5_TABLE,
        &tables::HOC_B6_TABLE,
        &tables::HOC_B7_TABLE,
        &tables::HOC_B8_TABLE,
    ];
    let hoc_indices = [b5, b6, b7, b8];
    for blk in 0..IDCT_BLOCKS {
        let ji_val = *ji.get(blk + 1).unwrap_or(&0);
        let hoc_row = hoc_tables
            .get(blk)
            .and_then(|table| table.get(*hoc_indices.get(blk).unwrap_or(&0)));
        if let Some(block) = cik.get_mut(blk + 1) {
            for k in 3..=ji_val {
                if let Some(c) = block.get_mut(k) {
                    *c = if k > MAX_HOC_TERMS {
                        0.0
                    } else {
                        hoc_row
                            .and_then(|row| row.get(k - 3))
                            .copied()
                            .unwrap_or(0.0)
                    };
                }
            }
        }
    }

    // Inverse DCT: Cik -> Tl (per-band spectral offsets)
    inverse_dct_blocks(&cik, &ji)
}

/// Assembles the Gm[1..8] PRBA coefficient vector from table lookups.
fn assemble_gm(b3: usize, b4: usize) -> [f32; PRBA_BLOCKS + 1] {
    let mut gm = [0.0_f32; PRBA_BLOCKS + 1];
    let prba24 = tables::PRBA24_TABLE.get(b3);
    let prba58 = tables::PRBA58_TABLE.get(b4);

    // Gm[2..4] from PRBA24 table
    for (dst_idx, src_idx) in [(2, 0), (3, 1), (4, 2)] {
        if let Some(slot) = gm.get_mut(dst_idx) {
            *slot = prba24
                .and_then(|row| row.get(src_idx))
                .copied()
                .unwrap_or(0.0);
        }
    }
    // Gm[5..8] from PRBA58 table
    for (dst_idx, src_idx) in [(5, 0), (6, 1), (7, 2), (8, 3)] {
        if let Some(slot) = gm.get_mut(dst_idx) {
            *slot = prba58
                .and_then(|row| row.get(src_idx))
                .copied()
                .unwrap_or(0.0);
        }
    }
    gm
}

/// Forward 8-point DCT: Gm[1..8] -> Ri[1..8].
///
/// `Ri[i] = sum(am * Gm[m] * cos(pi*(m-1)*(i-0.5)/8))` where am=1 for
/// the DC term (m=1) and am=2 for all others.
///
/// For each fixed `i`, the angle is a linear function of `m`:
/// `angle = step_i * (m - 1)` with `step_i = pi * (i - 0.5) / 8`. The
/// inner cosines are evaluated via [`CosOscillator`] recurrence, which
/// replaces 8 `cos()` calls per outer iteration with 1 `sin_cos` plus
/// 8 cheap recurrence ticks.
fn forward_dct_8(gm: &[f32; PRBA_BLOCKS + 1]) -> [f32; PRBA_BLOCKS + 1] {
    let mut ri = [0.0_f32; PRBA_BLOCKS + 1];
    for i in 1..=PRBA_BLOCKS {
        #[expect(
            clippy::cast_precision_loss,
            reason = "i is at most 8; no precision loss"
        )]
        let step = std::f32::consts::PI * (i as f32 - 0.5) / PRBA_BLOCKS as f32;
        let mut osc = CosOscillator::new(0.0, step);
        let mut sum = 0.0_f32;
        for m in 1..=PRBA_BLOCKS {
            let am: f32 = if m == 1 { 1.0 } else { 2.0 };
            let gm_val = *gm.get(m).unwrap_or(&0.0);
            // osc.tick() returns cos(step * (m-1)) for m=1,2,...,8.
            sum = (am * gm_val).mul_add(osc.tick(), sum);
        }
        if let Some(slot) = ri.get_mut(i) {
            *slot = sum;
        }
    }
    ri
}

/// Looks up IDCT block lengths Ji[1..4] from the LMPRBL table.
fn lookup_ji(big_l: usize) -> [usize; IDCT_BLOCKS + 1] {
    let mut ji = [0_usize; IDCT_BLOCKS + 1];
    let lmprbl_row = tables::LMPRBL_TABLE.get(big_l);
    for idx in 0..IDCT_BLOCKS {
        #[expect(
            clippy::cast_sign_loss,
            reason = "LMPRBL_TABLE values are always non-negative (2..17 range)"
        )]
        if let Some(slot) = ji.get_mut(idx + 1) {
            *slot = lmprbl_row
                .and_then(|row| row.get(idx))
                .copied()
                .unwrap_or(0) as usize;
        }
    }
    ji
}

/// Inverse DCT per block: Cik -> Tl (per-band spectral offsets).
///
/// Each of 4 blocks produces Ji[i] values; concatenated they give Tl[1..L].
///
/// For each fixed `(i, j)` the inner cosines are linear in `k`:
/// `angle = step_j * (k - 1)` with `step_j = pi * (j - 0.5) / ji_val`.
/// The inner cosine evaluations use [`CosOscillator`] recurrence,
/// replacing up to 17 `cos()` calls per `j` with one `sin_cos` plus 17
/// cheap recurrence ticks. With L=56 split into 4 blocks of ~14 each,
/// this saves ~600 cosines per frame.
fn inverse_dct_blocks(
    cik: &[[f32; MAX_HOC_TERMS + 1]; IDCT_BLOCKS + 1],
    ji: &[usize; IDCT_BLOCKS + 1],
) -> [f32; MAX_BANDS] {
    let mut tl = [0.0_f32; MAX_BANDS];
    let mut l_idx = 1_usize;

    for i in 1..=IDCT_BLOCKS {
        let ji_val = *ji.get(i).unwrap_or(&0);
        if ji_val == 0 {
            continue;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "ji_val is at most 17; no precision loss"
        )]
        let inv_ji = 1.0 / ji_val as f32;
        for j in 1..=ji_val {
            #[expect(
                clippy::cast_precision_loss,
                reason = "j is at most 17; no precision loss"
            )]
            let step = std::f32::consts::PI * (j as f32 - 0.5) * inv_ji;
            let mut osc = CosOscillator::new(0.0, step);
            let mut sum = 0.0_f32;
            for k in 1..=ji_val {
                let ak: f32 = if k == 1 { 1.0 } else { 2.0 };
                let cik_val = cik
                    .get(i)
                    .and_then(|block| block.get(k))
                    .copied()
                    .unwrap_or(0.0);
                // osc.tick() returns cos(step * (k-1)) for k=1,2,...,ji_val.
                sum = (ak * cik_val).mul_add(osc.tick(), sum);
            }
            if let Some(slot) = tl.get_mut(l_idx) {
                *slot = sum;
            }
            l_idx += 1;
        }
    }
    tl
}

/// Computes final log2 and linear magnitudes per band (equations 40-43).
///
/// Interpolates between the IDCT spectral offsets (Tl) and the previous
/// frame's log-magnitudes, applies `BigGamma` normalization, and
/// exponentiates to get linear magnitudes with unvoiced scaling.
fn compute_magnitudes(cur: &mut MbeParams, prev: &MbeParams, tl: &[f32; MAX_BANDS], unvc: f32) {
    let big_l = cur.l;

    // Local copies of previous frame's arrays for extension fixup.
    let mut prev_log2_ml = prev.log2_ml;
    let mut prev_ml = prev.ml;

    // Extend previous arrays when current frame has more harmonics.
    if big_l > prev.l && prev.l > 0 {
        let last_log2 = *prev_log2_ml.get(prev.l).unwrap_or(&0.0);
        let last_ml = *prev_ml.get(prev.l).unwrap_or(&0.0);
        for l in (prev.l + 1)..=big_l {
            if let Some(s) = prev_log2_ml.get_mut(l) {
                *s = last_log2;
            }
            if let Some(s) = prev_ml.get_mut(l) {
                *s = last_ml;
            }
        }
    }

    // Copy band 1 to band 0 for interpolation boundary.
    let log2_1 = *prev_log2_ml.get(1).unwrap_or(&0.0);
    if let Some(s) = prev_log2_ml.get_mut(0) {
        *s = log2_1;
    }
    let ml_1 = *prev_ml.get(1).unwrap_or(&0.0);
    if let Some(s) = prev_ml.get_mut(0) {
        *s = ml_1;
    }

    // Band-ratio mapping and Sum43 accumulation.
    #[expect(
        clippy::cast_precision_loss,
        reason = "prev.l and big_l are at most 56; no precision loss in f32"
    )]
    let prev_l_f32 = prev.l as f32;
    #[expect(
        clippy::cast_precision_loss,
        reason = "big_l is at most 56; no precision loss in f32"
    )]
    let cur_l_f32 = big_l as f32;

    let mut intkl = [0_usize; MAX_BANDS];
    let mut deltal = [0.0_f32; MAX_BANDS];
    let mut sum43 = 0.0_f32;

    for l in 1..=big_l {
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let flo = (prev_l_f32 / cur_l_f32) * l as f32;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "flo is small and positive; truncation is the intended floor"
        )]
        let int_k = flo as usize;
        #[expect(
            clippy::cast_precision_loss,
            reason = "int_k is at most 56; no precision loss in f32"
        )]
        let delta = flo - int_k as f32;

        if let Some(s) = intkl.get_mut(l) {
            *s = int_k;
        }
        if let Some(s) = deltal.get_mut(l) {
            *s = delta;
        }

        let pk = *prev_log2_ml.get(int_k).unwrap_or(&0.0);
        let pk1 = *prev_log2_ml.get(int_k + 1).unwrap_or(&0.0);
        sum43 += (1.0 - delta).mul_add(pk, delta * pk1);
    }
    sum43 *= INTERP_WEIGHT / cur_l_f32;

    // Mean of Tl and BigGamma normalization.
    let mut sum42 = 0.0_f32;
    for l in 1..=big_l {
        sum42 += *tl.get(l).unwrap_or(&0.0);
    }
    sum42 /= cur_l_f32;
    let big_gamma = 0.5_f32.mul_add(-cur_l_f32.log2(), cur.gamma) - sum42;

    // Final per-band magnitudes.
    for l in 1..=big_l {
        let int_k = *intkl.get(l).unwrap_or(&0);
        let delta = *deltal.get(l).unwrap_or(&0.0);
        let tl_val = *tl.get(l).unwrap_or(&0.0);

        let pk = *prev_log2_ml.get(int_k).unwrap_or(&0.0);
        let pk1 = *prev_log2_ml.get(int_k + 1).unwrap_or(&0.0);
        let c1 = INTERP_WEIGHT * (1.0 - delta) * pk;
        let c2 = INTERP_WEIGHT * delta * pk1;

        let log2_val = tl_val + c1 + c2 - sum43 + big_gamma;
        if let Some(s) = cur.log2_ml.get_mut(l) {
            *s = log2_val;
        }

        let linear_base = (LN2_APPROX * log2_val).exp();
        let is_voiced = cur.vl.get(l).copied().unwrap_or(false);
        if let Some(s) = cur.ml.get_mut(l) {
            *s = if is_voiced {
                linear_base
            } else {
                unvc * linear_base
            };
        }
    }
}

/// Extracts a single bit from the parameter vector as a `usize`.
///
/// Returns 0 or 1 at the given index, or 0 if out of bounds.
fn bit(ambe_d: &[u8; AMBE_DATA_BITS], index: usize) -> usize {
    ambe_d.get(index).copied().unwrap_or(0) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::MbeParams;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// All-zero input should produce a valid voice frame with b0=0
    /// parameters from the lookup tables.
    #[test]
    fn zero_input_produces_valid_params() -> TestResult {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();

        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(
            status,
            FrameStatus::Voice,
            "all-zero input should be valid voice"
        );

        let table_f0 = *tables::W0_TABLE.first().ok_or("W0_TABLE[0] missing")?;
        let expected_w0 = table_f0 * std::f32::consts::TAU;
        assert!(
            (cur.w0 - expected_w0).abs() < 1e-6,
            "w0 mismatch: got {}, expected {expected_w0}",
            cur.w0,
        );

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "L_TABLE values are whole numbers 9..=56"
        )]
        let expected_l = *tables::L_TABLE.first().ok_or("L_TABLE[0] missing")? as usize;
        assert_eq!(cur.l, expected_l, "L mismatch for b0=0");

        let expected_gamma = *tables::DG_TABLE.first().ok_or("DG_TABLE[0] missing")?;
        assert!(
            (cur.gamma - expected_gamma).abs() < 1e-6,
            "gamma mismatch: got {}, expected {expected_gamma}",
            cur.gamma,
        );

        Ok(())
    }

    /// b0=0 should give `W0_TABLE`[0] for f0 and `L_TABLE`[0] for L.
    #[test]
    fn b0_zero_correct_w0_and_l() -> TestResult {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();

        let _status = decode_params(&ambe_d, &mut cur, &prev);

        let table_f0 = *tables::W0_TABLE.first().ok_or("W0_TABLE[0] missing")?;
        let expected_w0 = table_f0 * std::f32::consts::TAU;
        assert!(
            (cur.w0 - expected_w0).abs() < 1e-6,
            "w0 for b0=0: got {}, expected {expected_w0}",
            cur.w0,
        );
        assert_eq!(cur.l, 9, "L for b0=0 should be 9");

        Ok(())
    }

    /// b1 V/UV mapping: `VUV_TABLE`[0] is all-voiced, so every band
    /// should be voiced with b0=0 (L=9).
    #[test]
    fn b1_vuv_mapping() -> TestResult {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();

        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(status, FrameStatus::Voice);

        for l in 1..=cur.l {
            assert!(
                *cur.vl.get(l).ok_or("vl out of bounds")?,
                "band {l} should be voiced for b1=0"
            );
        }

        Ok(())
    }

    /// Erasure frames (b0 = 120..=123) should return status 2.
    #[test]
    fn erasure_frame_detection() {
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();
        let mut ambe_d = [0u8; AMBE_DATA_BITS];

        // b0 = 120 = 0b1111000
        for idx in [0, 1, 2, 3] {
            if let Some(b) = ambe_d.get_mut(idx) {
                *b = 1;
            }
        }

        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(status, FrameStatus::Erasure, "b0=120 should be erasure");
    }

    /// Tone frames (b0 = 126..=127) should return status 3.
    #[test]
    fn tone_frame_detection() {
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();
        let mut ambe_d = [0u8; AMBE_DATA_BITS];

        // b0 = 127 = 0b1111111
        for idx in [0, 1, 2, 3, 37, 38, 39] {
            if let Some(b) = ambe_d.get_mut(idx) {
                *b = 1;
            }
        }

        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(status, FrameStatus::Tone, "b0=127 should be tone");
    }

    /// Silence frames (b0 = 124 or 125) should produce valid voice
    /// status with fixed w0 and L=14, all unvoiced.
    #[test]
    fn silence_frame_parameters() -> TestResult {
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();
        let mut ambe_d = [0u8; AMBE_DATA_BITS];

        // b0 = 124 = 0b1111100
        for idx in [0, 1, 2, 3, 37] {
            if let Some(b) = ambe_d.get_mut(idx) {
                *b = 1;
            }
        }

        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(
            status,
            FrameStatus::Voice,
            "silence frame should return status 0"
        );
        assert_eq!(cur.l, SILENCE_L, "silence L should be 14");
        assert!(
            (cur.w0 - SILENCE_W0).abs() < 1e-6,
            "silence w0 should be 2*pi/32"
        );

        for l in 1..=cur.l {
            assert!(
                !*cur.vl.get(l).ok_or("vl out of bounds")?,
                "band {l} should be unvoiced in silence frame"
            );
        }

        Ok(())
    }

    /// Determinism: same input always produces bit-exact same output.
    #[test]
    fn deterministic_output() {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let prev = MbeParams::new();

        let mut cur1 = MbeParams::new();
        let mut cur2 = MbeParams::new();

        let s1 = decode_params(&ambe_d, &mut cur1, &prev);
        let s2 = decode_params(&ambe_d, &mut cur2, &prev);

        assert_eq!(s1, s2, "status codes must match");
        assert_eq!(cur1.w0.to_bits(), cur2.w0.to_bits(), "w0 must match");
        assert_eq!(cur1.l, cur2.l, "l must match");
        assert_eq!(
            cur1.gamma.to_bits(),
            cur2.gamma.to_bits(),
            "gamma must match"
        );
        assert_eq!(cur1.vl, cur2.vl, "vl must match");

        for l in 0..MAX_BANDS {
            let ml1 = cur1.ml.get(l).copied().unwrap_or(0.0);
            let ml2 = cur2.ml.get(l).copied().unwrap_or(0.0);
            assert_eq!(ml1.to_bits(), ml2.to_bits(), "ml[{l}] must match");

            let log1 = cur1.log2_ml.get(l).copied().unwrap_or(0.0);
            let log2_val = cur2.log2_ml.get(l).copied().unwrap_or(0.0);
            assert_eq!(
                log1.to_bits(),
                log2_val.to_bits(),
                "log2_ml[{l}] must match"
            );
        }
    }

    /// Gain delta with non-zero previous gamma applies first-order smoothing.
    #[test]
    fn gain_smoothing_with_nonzero_prev() -> TestResult {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let mut prev = MbeParams::new();
        prev.gamma = 4.0;
        prev.l = 9;

        let mut cur = MbeParams::new();
        let status = decode_params(&ambe_d, &mut cur, &prev);
        assert_eq!(status, FrameStatus::Voice);

        let dg = *tables::DG_TABLE.first().ok_or("DG_TABLE[0] missing")?;
        let expected = 0.5_f32.mul_add(4.0, dg);
        assert!(
            (cur.gamma - expected).abs() < 1e-6,
            "gamma: got {}, expected {expected}",
            cur.gamma,
        );

        Ok(())
    }

    /// The `bit()` helper returns 0 for out-of-bounds indices.
    #[test]
    fn bit_helper_oob_returns_zero() {
        let ambe_d = [1u8; AMBE_DATA_BITS];
        assert_eq!(bit(&ambe_d, 0), 1);
        assert_eq!(bit(&ambe_d, 48), 1);
        assert_eq!(bit(&ambe_d, 49), 0, "out-of-bounds should return 0");
        assert_eq!(bit(&ambe_d, 100), 0, "way out-of-bounds should return 0");
    }

    /// PRBA table lookups produce non-zero magnitudes for b3=0, b4=0.
    #[test]
    fn prba_coefficients_b3_b4_zero() -> TestResult {
        let ambe_d = [0u8; AMBE_DATA_BITS];
        let prev = MbeParams::new();
        let mut cur = MbeParams::new();

        let _status = decode_params(&ambe_d, &mut cur, &prev);

        let prba24 = tables::PRBA24_TABLE.first().ok_or("PRBA24_TABLE[0]")?;
        let prba58 = tables::PRBA58_TABLE.first().ok_or("PRBA58_TABLE[0]")?;

        let has_nonzero_ml = (1..=cur.l).any(|l| cur.ml.get(l).copied().unwrap_or(0.0) != 0.0);
        assert!(
            has_nonzero_ml,
            "at least one ml band should be non-zero; PRBA24[0]={prba24:?}, PRBA58[0]={prba58:?}"
        );

        Ok(())
    }
}
