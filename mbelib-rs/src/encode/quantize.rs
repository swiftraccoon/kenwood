// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Parameter → 49-bit data vector quantization.
//!
//! Takes the analog outputs of the analysis pipeline (pitch, V/UV,
//! spectral amplitudes) and produces the 49-bit `ambe_d` parameter
//! vector consumed by the FEC encoder.
//!
//! # Fields
//!
//! - **`b0` (pitch, 7 bits):** L-constrained search against
//!   [`crate::tables::W0_TABLE`] — pick the index whose
//!   [`crate::tables::L_TABLE`] entry equals `amps.num_harmonics`
//!   and whose W0 value is closest to the target f0. Falls back
//!   to nearest-W0 only when no b0 matches the target L (rare,
//!   edge pitches only).
//! - **`b1` (V/UV, 4 bits):** per-band voicing pattern chosen by
//!   minimizing the energy-weighted disagreement with each of the
//!   16 [`crate::tables::VUV_TABLE`] rows across the active bands.
//! - **`b2` (gain, 6 bits):** nearest-neighbor on the 64-entry
//!   [`crate::tables::DG_TABLE`] against the mean of the per-
//!   harmonic log-spectral amplitudes.
//! - **`b3..b8` (spectral envelope):** block-DCT on the prediction
//!   residual `T = lsa - 0.65·interp(prev_log2_ml)`; assemble R
//!   pairs from block DC + first AC coefficients; inverse 8-pt DCT
//!   → G[8]; PRBA24 / PRBA58 codebook search (b3, b4); per-block
//!   HOC codebook search (b5..b8). `b8` uses stride-2 search —
//!   only even indices are representable in the 3-bits-with-
//!   forced-zero-LSB wire field (mbelib decoder convention).
//!
//! After emitting the 49-bit vector, the encoder runs a closed-loop
//! [`crate::decode::decode_params`] on its own output and carries
//! the reconstructed `log2_ml` forward as `prev_log2_ml` for the
//! next frame — so the prediction residual the decoder adds back
//! matches bit-for-bit what the encoder subtracted.
//!
//! Stage 5..8 is bit-exact against OP25's `ambe_encoder.cc` for
//! b3/b4/b5/b6/b7 when given identical `imbe_param` inputs
//! (validated by `examples/validate_quantize_vs_op25.rs`). The
//! remaining mismatch on b0 / b1 / b2 / b8 is documented inline
//! next to each codebook search.

use crate::ecc::AMBE_DATA_BITS;
use crate::encode::pitch::PitchEstimate;
use crate::encode::spectral::SpectralAmplitudes;
use crate::encode::vuv::VuvDecisions;
use crate::tables;

/// Number of PRBA blocks (matches decode side).
const PRBA_BLOCKS: usize = 8;
/// Number of IDCT blocks (matches decode side).
const IDCT_BLOCKS: usize = 4;
/// Max Cik coefficient index per block (k=1..=6).
const MAX_HOC_TERMS: usize = 6;

/// Quantize analysis outputs into a 49-bit `ambe_d` vector.
///
/// Uses the D-STAR AMBE 3600x2400 bit layout — every position below
/// is the exact mirror of what `crate::decode::decode_params` reads,
/// which in turn matches `mbe_decodeAmbe2400Parms` in
/// `ref/mbelib/ambe3600x2400.c`.
///
/// # Bit layout
///
/// Field widths and `ambe_d` positions:
///
/// | Field | Width | Positions (MSB → LSB) |
/// |-------|-------|-----------------------|
/// | `b0`  | 7     | `[0, 1, 2, 3, 4, 5, 48]` |
/// | `b1`  | 4     | `[38, 39, 40, 41]` |
/// | `b2`  | 6     | `[6, 7, 8, 9, 42, 43]` |
/// | `b3`  | 9     | `[10, 11, 12, 13, 14, 15, 16, 44, 45]` |
/// | `b4`  | 7     | `[17, 18, 19, 20, 21, 46, 47]` |
/// | `b5`  | 4     | `[22, 23, 25, 26]` ← bit 24 deliberately skipped |
/// | `b6`  | 4     | `[27, 28, 29, 30]` |
/// | `b7`  | 4     | `[31, 32, 33, 34]` |
/// | `b8`  | 3     | `[35, 36, 37]` (LSB always 0) |
///
/// `ambe_d[24]` is the one position that carries no parameter data
/// in D-STAR. mbelib's 2400 decoder never reads it.
/// Per-encoder state the spectral path needs to carry between frames.
///
/// Decoder-side, the reconstruction formula
/// `log_ml[l] = tl[l] + 0.65 * interp(prev_log2_ml)[l] - sum43 + big_gamma`
/// assumes the encoder subtracted a matching `0.65 * interp(prev_log2_ml)`
/// term at encode time. Without this struct the encoder would have
/// no `prev_log2_ml` to subtract, so the receiver would hear
/// `lsa + 0.65*prev_interp` instead of the intended `lsa`.
///
/// Both fields are owned and updated by `AmbeEncoder`; `quantize()`
/// reads the state, computes residuals, and returns an updated
/// [`QuantizeOutcome::prev_log2_ml`] the caller stores for the next
/// frame.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct PrevFrameState {
    /// Per-band log-magnitude from the previous frame, indexed by
    /// harmonic number (1-based; slot 0 mirrors slot 1 for the
    /// boundary condition the band-ratio interpolation uses).
    pub log2_ml: [f32; 57],
    /// Previous frame's harmonic count `L`. Drives the band-ratio
    /// mapping that projects `prev_log2_ml` onto the current frame's
    /// harmonic grid.
    pub l: usize,
}

/// Result of a single `quantize()` call: the 49-bit data vector plus
/// the updated per-frame state to carry into the next call.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct QuantizeOutcome {
    pub ambe_d: [u8; AMBE_DATA_BITS],
    pub prev_log2_ml: [f32; 57],
    pub prev_l: usize,
}

/// Enables env-gated diagnostic `eprintln!`s that emit each
/// intermediate-stage value inside `quantize()`. Set
/// `MBELIB_DUMP_QUANTIZE=1` at runtime to log one frame's full
/// pipeline — lsa, gain, T residuals, block DCT coefficients, R
/// pairs, 8-pt DCT G vector, and PRBA/HOC target vectors — to
/// stderr, line-prefixed so the output can be diffed frame-by-frame
/// against the matching dump from OP25's `ambe_encode_dump`.
///
/// This is the validation approach the April 2026 quantize-stage
/// investigation established: find the first stage where our values
/// diverge from OP25's reference, fix the divergence, repeat.
fn dump_enabled() -> bool {
    std::env::var_os("MBELIB_DUMP_QUANTIZE").is_some()
}

/// Dump LSA + gain + b2 choice for one frame to stderr. Hoisted out
/// of `quantize()` so the function-length lint stays happy.
fn dump_quantize_lsa(
    lsa: &[f32; 57],
    n: usize,
    pitch: &PitchEstimate,
    vuv: &VuvDecisions,
    amps: &SpectralAmplitudes,
    b2: u8,
) {
    eprint!("  OURS lsa[] =");
    for v in lsa.iter().take(n) {
        eprint!(" {v:.4}");
    }
    eprintln!();
    let gain = compute_gain_from_amps(pitch, vuv, amps);
    eprintln!("  OURS gain = {gain:.4}");
    eprintln!(
        "  OURS b2 chosen = {b2}  (DG_TABLE[{b2}] = {:.4})",
        tables::DG_TABLE[b2 as usize]
    );
}

/// Dump prediction residual T[] and prev state for one frame.
fn dump_quantize_t(t: &[f32; 57], n: usize, prev: &PrevFrameState) {
    eprint!("  OURS T[] =");
    for v in t.iter().take(n) {
        eprint!(" {v:.4}");
    }
    eprintln!();
    eprintln!("  OURS prev_L = {}", prev.l);
    eprint!("  OURS prev_log2_ml[0..15] =");
    for v in prev.log2_ml.iter().take(15) {
        eprint!(" {v:.4}");
    }
    eprintln!();
}

/// Dump per-block J lengths and DCT coefficients.
fn dump_quantize_blocks(
    ji: &[usize; IDCT_BLOCKS + 1],
    cik: &[[f32; MAX_HOC_TERMS + 1]; IDCT_BLOCKS + 1],
) {
    eprint!("OURS J[] =");
    for v in ji.iter().skip(1).take(IDCT_BLOCKS) {
        eprint!(" {v}");
    }
    eprintln!();
    for (b_idx, block) in cik.iter().enumerate().skip(1).take(IDCT_BLOCKS) {
        eprint!("OURS C[{}][0..5] =", b_idx - 1);
        for v in block.iter().skip(1).take(6) {
            eprint!(" {v:.4}");
        }
        eprintln!();
    }
}

/// Dump R pairs, G vector, and PRBA target vectors.
fn dump_quantize_gm(ri: &[f32; PRBA_BLOCKS + 1], gm: &[f32; PRBA_BLOCKS + 1]) {
    eprint!("OURS R[] =");
    for v in ri.iter().skip(1).take(8) {
        eprint!(" {v:.4}");
    }
    eprintln!();
    eprint!("OURS G[] =");
    for v in gm.iter().skip(1).take(8) {
        eprint!(" {v:.4}");
    }
    eprintln!();
    eprintln!(
        "OURS PRBA24 target = {:.4} {:.4} {:.4}",
        gm[2], gm[3], gm[4]
    );
    eprintln!(
        "OURS PRBA58 target = {:.4} {:.4} {:.4} {:.4}",
        gm[5], gm[6], gm[7], gm[8]
    );
}

#[must_use]
#[doc(hidden)]
pub fn quantize(
    pitch: PitchEstimate,
    vuv: VuvDecisions,
    amps: &SpectralAmplitudes,
    prev: &PrevFrameState,
) -> QuantizeOutcome {
    let mut out = [0u8; AMBE_DATA_BITS];

    // -- b0 (7 bits): pitch index into W0_TABLE/L_TABLE --
    let b0 = quantize_pitch(pitch, amps.num_harmonics);
    write_bit(&mut out, 0, (b0 >> 6) & 1);
    write_bit(&mut out, 1, (b0 >> 5) & 1);
    write_bit(&mut out, 2, (b0 >> 4) & 1);
    write_bit(&mut out, 3, (b0 >> 3) & 1);
    write_bit(&mut out, 4, (b0 >> 2) & 1);
    write_bit(&mut out, 5, (b0 >> 1) & 1);
    write_bit(&mut out, 48, b0 & 1);

    // -- b1 (4 bits): V/UV summary index into 16-row VUV_TABLE --
    let b1 = quantize_vuv(&pitch, &vuv, amps);
    write_bit(&mut out, 38, (b1 >> 3) & 1);
    write_bit(&mut out, 39, (b1 >> 2) & 1);
    write_bit(&mut out, 40, (b1 >> 1) & 1);
    write_bit(&mut out, 41, b1 & 1);

    // -- b2 (6 bits): gain index into 64-entry DG_TABLE --
    let b2 = quantize_gain(&pitch, &vuv, amps);
    write_bit(&mut out, 6, (b2 >> 5) & 1);
    write_bit(&mut out, 7, (b2 >> 4) & 1);
    write_bit(&mut out, 8, (b2 >> 3) & 1);
    write_bit(&mut out, 9, (b2 >> 2) & 1);
    write_bit(&mut out, 42, (b2 >> 1) & 1);
    write_bit(&mut out, 43, b2 & 1);

    // -- b3..b8: spectral envelope + HOC --
    // Compute per-harmonic log-spectral-amplitudes with the D-STAR
    // voicing offset, then subtract the 0.65-weighted interpolated
    // previous-frame log-magnitudes to form the prediction residual
    // `T[i]`. That residual is what the PRBA24/PRBA58/HOC codebooks
    // expect at their input.
    let lsa = compute_lsa(b0, &vuv, amps);
    if dump_enabled() {
        dump_quantize_lsa(&lsa, amps.num_harmonics, &pitch, &vuv, amps, b2);
    }
    let t_residuals = compute_spectral_residuals(&lsa, amps.num_harmonics, prev);
    if dump_enabled() {
        dump_quantize_t(&t_residuals, amps.num_harmonics, prev);
    }
    let spectrum = quantize_spectrum(&t_residuals, amps.num_harmonics);
    let QuantizedSpectrum {
        b3,
        b4,
        b5,
        b6,
        b7,
        b8,
    } = spectrum;
    if dump_enabled() {
        eprintln!("  OURS b3..b8 = {b3} {b4} {b5} {b6} {b7} {b8}  b0={b0} b1={b1} b2={b2}");
    }

    // b3 (9 bits, u16 because it's 0..=511).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "low-bit mask; always 0 or 1"
    )]
    {
        write_bit(&mut out, 10, ((b3 >> 8) & 1) as u8);
        write_bit(&mut out, 11, ((b3 >> 7) & 1) as u8);
        write_bit(&mut out, 12, ((b3 >> 6) & 1) as u8);
        write_bit(&mut out, 13, ((b3 >> 5) & 1) as u8);
        write_bit(&mut out, 14, ((b3 >> 4) & 1) as u8);
        write_bit(&mut out, 15, ((b3 >> 3) & 1) as u8);
        write_bit(&mut out, 16, ((b3 >> 2) & 1) as u8);
        write_bit(&mut out, 44, ((b3 >> 1) & 1) as u8);
        write_bit(&mut out, 45, (b3 & 1) as u8);
    }

    // b4 (7 bits).
    write_bit(&mut out, 17, (b4 >> 6) & 1);
    write_bit(&mut out, 18, (b4 >> 5) & 1);
    write_bit(&mut out, 19, (b4 >> 4) & 1);
    write_bit(&mut out, 20, (b4 >> 3) & 1);
    write_bit(&mut out, 21, (b4 >> 2) & 1);
    write_bit(&mut out, 46, (b4 >> 1) & 1);
    write_bit(&mut out, 47, b4 & 1);

    // b5 (4 bits) — note the gap at position 24 (unused in D-STAR).
    write_bit(&mut out, 22, (b5 >> 3) & 1);
    write_bit(&mut out, 23, (b5 >> 2) & 1);
    write_bit(&mut out, 25, (b5 >> 1) & 1);
    write_bit(&mut out, 26, b5 & 1);

    // b6 (4 bits).
    write_bit(&mut out, 27, (b6 >> 3) & 1);
    write_bit(&mut out, 28, (b6 >> 2) & 1);
    write_bit(&mut out, 29, (b6 >> 1) & 1);
    write_bit(&mut out, 30, b6 & 1);

    // b7 (4 bits).
    write_bit(&mut out, 31, (b7 >> 3) & 1);
    write_bit(&mut out, 32, (b7 >> 2) & 1);
    write_bit(&mut out, 33, (b7 >> 1) & 1);
    write_bit(&mut out, 34, b7 & 1);

    // b8 (3 bits, stored in the top 3 positions of a notional 4-bit
    // field; the LSB is forced to 0 per the AMBE+ patent note in
    // mbelib 2400:437-440).
    write_bit(&mut out, 35, (b8 >> 3) & 1);
    write_bit(&mut out, 36, (b8 >> 2) & 1);
    write_bit(&mut out, 37, (b8 >> 1) & 1);

    // ambe_d[24] is intentionally left as 0 (unused in D-STAR 2400).

    // CLOSED-LOOP prev_log2_ml reconstruction.
    //
    // The on-air decoder sees our emitted 9-byte frame and computes
    // its own `log2_ml` by running PRBA/HOC codebook dequantization
    // plus inverse-DCT plus prediction using ITS previous frame's
    // state. Our encoder's `prev_log2_ml` must match the decoder's
    // result exactly — otherwise the prediction residual
    // `T = lsa − 0.65·interp(prev_log2_ml)` that we subtract at
    // encode time differs from the value the decoder adds back at
    // decode time, and the reconstructed magnitudes drift frame-
    // by-frame. Empirically (April 2026 OP25 diff): the drift
    // showed up as uniform lsa-valued prev on our side (≈11.4 across
    // all bins) vs OP25's varied decoder-shape (0.0 at boundary,
    // rising to 9–10 at vowel harmonics, decaying after) — the
    // difference produced unintelligible spectral envelopes despite
    // matching b0/b2 values.
    //
    // The reference implementation (OP25 `ambe_encoder.cc:486`)
    // handles this by running mbelib's `mbe_dequantizeAmbe2400Parms`
    // on its own emitted `b[]`. We mirror that here via our own
    // `decode::decode_params` — same inputs, same outputs, so the
    // encoder's `prev_log2_ml` is bit-exactly what the decoder will
    // have on the next frame.
    let mut cur_reconstructed = crate::params::MbeParams::new();
    let decoder_prev_params = {
        let mut p = crate::params::MbeParams::new();
        p.log2_ml = prev.log2_ml;
        p.l = prev.l;
        p
    };
    let _status = crate::decode::decode_params(&out, &mut cur_reconstructed, &decoder_prev_params);

    QuantizeOutcome {
        ambe_d: out,
        prev_log2_ml: cur_reconstructed.log2_ml,
        prev_l: cur_reconstructed.l,
    }
}

/// Compute per-harmonic log-spectral-amplitudes with the D-STAR
/// voicing-dependent offset. Mirrors `ambe_encoder.cc:229-248`.
fn compute_lsa(b0: u8, vuv: &VuvDecisions, amps: &SpectralAmplitudes) -> [f32; 57] {
    let mut lsa = [0.0_f32; 57];
    if amps.num_harmonics == 0 {
        return lsa;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "num_harmonics ≤ 56, well inside f32 mantissa"
    )]
    let num_harms_f = amps.num_harmonics as f32;
    let log_l_2 = 0.5 * num_harms_f.log2();
    // Use the b0-quantized f0 from W0_TABLE, NOT the raw pitch estimate.
    // OP25 `ambe_encoder.cc:234` does `log_l_w0 = 0.5 * log2(num_harms *
    // make_f0(b[0]) * 2π) + 2.289`, where `make_f0(b)` is the same
    // `W0_TABLE` lookup our decoder uses. Feeding raw pitch.f0_hz here
    // produces a systematic ~0.026 lsa bias — small but enough to push
    // the DC component of each block's DCT across a codebook boundary
    // occasionally, explaining the residual 30-45% b3/b4 disagreement
    // we saw before this change.
    let f0 = *tables::W0_TABLE.get(b0 as usize).unwrap_or(&0.0);
    let log_l_w0 = 0.5_f32.mul_add(
        (num_harms_f * f0 * 2.0 * std::f32::consts::PI).log2(),
        2.289,
    );
    for i in 0..amps.num_harmonics {
        let sa = amps.magnitudes.get(i).copied().unwrap_or(0.0);
        let sa_scaled = (sa * SA_SCALE).max(1.0);
        // Per-harmonic voicing via band-expansion.
        //
        // `vuv.voiced[band]` is per-band (12 bands); OP25's
        // `v_uv_dsn[i]` is per-harmonic. OP25 expands: harmonic `l`
        // (1-indexed) maps to band `kl = (l+2)/3` for `l <= 36`, else
        // band 12. In 0-indexed terms: harmonic `i` → band `i/3`
        // clamped to the number of bands. Without this mapping,
        // compute_lsa reads the wrong band's voicing for every
        // harmonic with `i >= num_bands`, producing a lsa[i] error of
        // `log_l_w0 - log_l_2` at those positions (≈0.44 for typical
        // voice pitch). That difference cascades into block-DCT
        // coefficients and shifts PRBA target vectors off by several
        // codebook entries, which was our 30% b3/b4 disagreement.
        let band = (i / 3).min(vuv.num_bands.saturating_sub(1));
        let voiced = vuv.voiced.get(band).copied().unwrap_or(false);
        let offset = if voiced { log_l_2 } else { log_l_w0 };
        let lsa_i = offset + sa_scaled.log2();
        if let Some(slot) = lsa.get_mut(i) {
            *slot = lsa_i;
        }
    }
    lsa
}

/// Compute the prediction residual `T[i] = lsa[i] - 0.65 * interp_prev[i]`
/// for each of the `n` harmonics in the current frame.
///
/// `interp_prev[i]` is the band-ratio-interpolated previous-frame
/// log-magnitude at the position this frame's harmonic `i+1` projects
/// onto. Mirrors `ambe_encoder.cc:275-291`.
fn compute_spectral_residuals(lsa: &[f32; 57], n: usize, prev: &PrevFrameState) -> [f32; 57] {
    let mut t = [0.0_f32; 57];
    if n == 0 {
        return t;
    }
    // Boundary mirror: OP25 `ambe_encoder.cc:277` does
    // `prev_mp->log2Ml[0] = prev_mp->log2Ml[1]` before interpolation.
    // Without this, the interpolation for the first harmonic (kl_floor=0)
    // reads log2Ml[0] as zero — producing a systematic `0.65 * prev[1]`
    // bias in `T[0]` that cascades into every block-0 DCT coefficient.
    // Empirically (2026-04 validation run) this bias was 0.6–0.7 per
    // voiced frame, which shifted block-0 mean `C[0][0]` enough to
    // pick a different PRBA24 codebook entry — hence the 56% b3 match
    // rate observed before this fix.
    let mut prev_log2_ml = prev.log2_ml;
    prev_log2_ml[0] = prev_log2_ml[1];
    #[allow(
        clippy::cast_precision_loss,
        reason = "L values bounded by 56 (MAX_BANDS)"
    )]
    let l_prev_l = if n == 0 {
        0.0
    } else {
        prev.l as f32 / n as f32
    };
    for i in 0..n {
        #[allow(clippy::cast_precision_loss, reason = "i bounded by 56")]
        let kl = l_prev_l * (i + 1) as f32;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "kl is positive; truncation is the intended floor"
        )]
        let kl_floor = kl as usize;
        #[allow(clippy::cast_precision_loss)]
        let kl_frac = kl - kl_floor as f32;
        let p0 = *prev_log2_ml.get(kl_floor.min(56)).unwrap_or(&0.0);
        let p1 = *prev_log2_ml.get((kl_floor + 1).min(56)).unwrap_or(&0.0);
        let interp = (1.0 - kl_frac).mul_add(p0, kl_frac * p1);
        let lsa_i = lsa.get(i).copied().unwrap_or(0.0);
        if let Some(slot) = t.get_mut(i) {
            *slot = 0.65_f32.mul_add(-interp, lsa_i);
        }
    }
    t
}

/// Quantize pitch to a 7-bit `b0` code using OP25's `b0_lookup` +
/// ±1 walk policy (`ambe_encoder.cc:158-192`).
///
/// Returns a value in `0..=119`. Codes 120–123 (erasure) and 126–127
/// (tone) are avoided; 124/125 (silence) are only emitted when the
/// pitch confidence is below the voice threshold.
///
/// See [`crate::encode::pitch_quant::pitch_index`] for the full
/// description of the walk. This wrapper adds:
///
/// - The silence short-circuit (low-confidence inputs emit `124`, the
///   D-STAR silence code).
/// - Conversion from our `PitchEstimate` (float period in samples) to
///   OP25's Q8.8 `ref_pitch` format.
fn quantize_pitch(pitch: PitchEstimate, target_l: usize) -> u8 {
    if pitch.confidence < 0.05 {
        // Near-silence — emit the silence code that mbelib 2400
        // treats specially (w0 = 2π/32, L = 14, all bands unvoiced).
        return 124;
    }
    // Convert period (f32 samples) → Q8.8 (samples · 256).
    //
    // OP25 expects `ref_pitch` in the range 19.875..123.125 samples
    // (`ambe_encoder.cc:163`). Values outside clamp into the table's
    // valid index range inside `pitch_index`, which is the
    // least-bad fallback — the caller should have silenced the
    // frame if the pitch were that far out.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "period ∈ (0, 256) for valid pitches; multiplying by 256 keeps it in u32 range"
    )]
    let ref_pitch_q8_8 = (pitch.period_samples * 256.0).round().max(0.0) as u32;
    crate::encode::pitch_quant::pitch_index(ref_pitch_q8_8, target_l, &tables::L_TABLE)
}

/// Find the 4-bit `VUV_TABLE` index whose voicing pattern best matches
/// the per-band decisions.
///
/// D-STAR's VUV codebook is 16 rows × 8 slots (`VUV_TABLE` in
/// `tables.rs`). Each row is a pattern of 8 V/UV decisions applied
/// across harmonic bands via the `jl = floor(l * 16 * f0)` slot
/// mapping the decoder uses.
///
/// Following OP25's `ambe_encoder.cc:200-227`, we minimize the
/// energy of disagreements between the candidate row and each
/// harmonic's own voicing decision:
///
/// ```text
///   En(row) = Σ_{l=1..L} [voiced(l) ≠ row[jl]] · m[l]²
///   b1 = argmin En(row)
/// ```
///
/// where `jl = floor(l * 16 * f0)` picks which of the 8 row slots
/// applies to harmonic `l`. Strong (high-energy) harmonics dominate
/// the decision — a bright voiced harmonic drags the best-match row
/// toward one that covers its band, even if weaker harmonics in
/// other bands would individually prefer the opposite voicing.
///
/// The prior revision used a naive Hamming-distance on the total
/// voiced count, which destroyed position information: for a pure
/// tone at the fundamental (only band 0 voiced) it would pick rows
/// like `{0,0,0,0,0,0,1,1}` that mark a HIGH band as voiced. The
/// decoder would then synthesize harmonic 1 as noise (producing
/// "shaped noise at 300 Hz" when the input was a pure 150 Hz tone).
fn quantize_vuv(pitch: &PitchEstimate, vuv: &VuvDecisions, amps: &SpectralAmplitudes) -> u8 {
    if amps.num_harmonics == 0 {
        return 0; // all-unvoiced row
    }
    let w0 = pitch.f0_hz / 8000.0;
    let mut best_idx: u8 = 0;
    let mut best_en = f32::INFINITY;
    for (idx, row) in tables::VUV_TABLE.iter().enumerate() {
        let mut en = 0.0_f32;
        for l in 1..=amps.num_harmonics {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "l ≤ 56, 16*w0*l small-positive, floor-to-usize is intentional"
            )]
            let jl = ((l as f32) * 16.0 * w0) as usize;
            let row_slot = row.get(jl.min(7)).copied().unwrap_or(0) == 1;
            // Which band does harmonic l belong to?  The encoder
            // stores per-band decisions in `vuv.voiced[]`; map
            // harmonic l to its band index.  If num_bands == 1 all
            // harmonics share the same decision; otherwise we use
            // roughly 3 harmonics per band.
            let band = ((l - 1) * vuv.num_bands / amps.num_harmonics).min(vuv.num_bands - 1);
            let obs = vuv.voiced.get(band).copied().unwrap_or(false);
            if obs != row_slot {
                let m = amps.magnitudes.get(l - 1).copied().unwrap_or(0.0);
                en += m * m;
            }
        }
        if en < best_en {
            best_en = en;
            #[allow(clippy::cast_possible_truncation, reason = "VUV_TABLE has 16 rows")]
            {
                best_idx = idx as u8;
            }
        }
    }
    best_idx
}

/// Helper: set `dst[idx]` to the low bit of `value`.
fn write_bit(dst: &mut [u8; AMBE_DATA_BITS], idx: usize, value: u8) {
    if let Some(slot) = dst.get_mut(idx) {
        *slot = value & 1;
    }
}

/// Quantize the frame gain against the 6-bit (64-entry) D-STAR
/// `DG_TABLE` (`AmbePlusDg` in mbelib).
///
/// Follows the OP25 `dstar` branch of `ambe_encoder.cc:229-272`:
///
/// 1. For each harmonic `i`, compute `log_sa[i]`:
///    - voiced:   `log_sa[i] = 0.5 * log2(L) + log2(sa[i])`
///    - unvoiced: `log_sa[i] = 0.5 * log2(L * w0 * 2π) + 2.289 + log2(sa[i])`
///
///    The voiced branch uses a per-frame `L`-only offset; the unvoiced
///    branch adds a `w0`-dependent offset plus the magic constant
///    `2.289` (from OP25). Both place `log2(sa)` in the same scale
///    the 64-entry gain table operates in.
///
/// 2. `gain = mean(log_sa[0..L])`
///
/// 3. For D-STAR, `diff_gain = gain` directly (no prev-frame smoothing
///    subtraction, unlike DMR/AMBE+2).
///
/// 4. Return the index `i` minimizing `|diff_gain - DG_TABLE[i]|`.
///
/// The first entry `DG_TABLE[0] = 0.0` is the "completely silent"
/// value — encoders emit it when the harmonic magnitudes are
/// effectively zero. The table is monotonically non-decreasing all
/// the way to `5.352783` at index 63.
/// Scale factor applied to the encoder's normalised float harmonic
/// magnitudes before taking `log2()` to get LSA values.
///
/// OP25's `sa_encode.cc` operates on `imbe_param->sa[]` values scaled
/// to match int16 signal magnitude (typical voiced-speech peaks
/// 2000–5000, max ~10000, corresponding to log2 values 11–13). Our
/// FFT bin magnitudes come from normalised f32 input in `[-1, 1]`,
/// so the peak values are 32768× smaller than OP25's.
///
/// `SA_SCALE = 32768.0` maps our float magnitude domain into the same
/// log2 scale OP25 works in. `log2(sa * 32768) = log2(sa) + 15`, so
/// a normalised peak of ~0.1 (voiced harmonic at a typical mic level)
/// lands at log2 ~ 11.7 — squarely in OP25's working range. Matches
/// the exact behaviour of `ambe_encoder.cc:241-248` after the implicit
/// int16 → float cast that OP25's `encode_ambe()` does at
/// `(float)imbe_param->sa[i1]`.
///
/// An earlier revision used `SA_SCALE = 12.5` after a synthetic
/// sine-sweep calibration; that value gave decent fundamental-
/// harmonic dominance on synthetic inputs but placed `lsa` 3–4 orders
/// of magnitude below OP25's scale, so the PRBA/HOC codebook search
/// always landed on the all-zero corner of the codebooks. On real
/// voice that produced the flat, "generative noise" sound — the
/// 2025-04 user-verified regression that motivated this re-derivation
/// against instrumented OP25 dumps.
const SA_SCALE: f32 = 32768.0;

fn quantize_gain(pitch: &PitchEstimate, vuv: &VuvDecisions, amps: &SpectralAmplitudes) -> u8 {
    if amps.num_harmonics == 0 {
        // No harmonics → silence. Index 0 (= 0.0 gain delta).
        return 0;
    }
    let gain = compute_gain_from_amps(pitch, vuv, amps);

    // Nearest-neighbor search on the 64-entry DG_TABLE.
    let mut best_idx: u8 = 0;
    let mut best_err = f32::INFINITY;
    for (idx, &v) in tables::DG_TABLE.iter().enumerate() {
        let err = (v - gain).abs();
        if err < best_err {
            best_err = err;
            #[allow(clippy::cast_possible_truncation, reason = "DG_TABLE has 64 entries")]
            {
                best_idx = idx as u8;
            }
        }
    }
    best_idx
}

/// Mean log-spectral-amplitude across all harmonics — `gain` in the
/// OP25 sense. Shared between `quantize_gain` and `compute_lsa` to
/// keep the scale factor / voicing-offset math in one place.
fn compute_gain_from_amps(
    pitch: &PitchEstimate,
    vuv: &VuvDecisions,
    amps: &SpectralAmplitudes,
) -> f32 {
    if amps.num_harmonics == 0 {
        return 0.0;
    }
    #[allow(
        clippy::cast_precision_loss,
        reason = "num_harmonics ≤ 56, well inside f32 mantissa"
    )]
    let num_harms_f = amps.num_harmonics as f32;
    let log_l_2 = 0.5 * num_harms_f.log2();
    let w0 = pitch.f0_hz / 8000.0;
    let log_l_w0 = 0.5_f32.mul_add(
        (num_harms_f * w0 * 2.0 * std::f32::consts::PI).log2(),
        2.289,
    );

    let mut lsa_sum = 0.0_f32;
    for i in 0..amps.num_harmonics {
        let sa = amps.magnitudes.get(i).copied().unwrap_or(0.0);
        let sa_scaled = (sa * SA_SCALE).max(1.0);
        let voiced = vuv.voiced.get(i).copied().unwrap_or(false);
        let offset = if voiced { log_l_2 } else { log_l_w0 };
        lsa_sum += offset + sa_scaled.log2();
    }
    lsa_sum / num_harms_f
}

/// Result of spectral quantization: the 6 codebook indices that
/// collectively encode the frame's harmonic envelope + detail.
struct QuantizedSpectrum {
    /// 9-bit PRBA24 index (low-band Gm\[2..=4\]).
    b3: u16,
    /// 7-bit PRBA58 index (high-band Gm\[5..=8\]).
    b4: u8,
    /// 5-bit HOC index for block 1.
    b5: u8,
    /// 4-bit HOC index for block 2.
    b6: u8,
    /// 4-bit HOC index for block 3.
    b7: u8,
    /// 3-bit HOC index for block 4.
    b8: u8,
}

/// Quantize the spectral envelope into all 6 spectral-detail indices.
///
/// Pipeline:
/// 1. Per-harmonic log-magnitudes (ε-floored).
/// 2. Split into 4 blocks by the `LMPRBL_TABLE` `Ji[1..=4]` allocation
///    (each block gets a pitch-dependent number of harmonics).
/// 3. Forward DCT per block: Cik\[blk\]\[k\] for k=1..=Ji.
/// 4. Block means (Cik\[blk\]\[1\]) + tilts (Cik\[blk\]\[2\]) form the
///    Ri pairs per the decoder's reconstruction formulas.
/// 5. Inverse 8-point DCT of Ri → Gm\[1..=8\] for PRBA codebooks.
/// 6. Remaining Cik\[blk\]\[3..\] coefficients → HOC codebooks.
#[allow(
    clippy::cast_precision_loss,
    reason = "all indices bounded by Ji (max 17) or PRBA_BLOCKS (8); safe for f32"
)]
#[allow(
    clippy::too_many_lines,
    reason = "Linear top-to-bottom pipeline; splitting obscures the data flow."
)]
fn quantize_spectrum(t_residuals: &[f32; 57], n: usize) -> QuantizedSpectrum {
    let zero = QuantizedSpectrum {
        b3: 0,
        b4: 0,
        b5: 0,
        b6: 0,
        b7: 0,
        b8: 0,
    };
    if n == 0 {
        return zero;
    }

    // Copy the first `n` residuals into a dense `log_m`-style array
    // for the block partitioning. The residuals already have the
    // 0.65-weighted prev-interp subtracted, so this is the input the
    // PRBA/HOC codebooks expect.
    let mut log_m = [0.0_f32; 56];
    for (i, &v) in t_residuals.iter().enumerate().take(n.min(56)) {
        log_m[i] = v;
    }

    // Step 2: block partitioning via LMPRBL[L]. Row big_l = L.
    let ji = lookup_ji(n);

    // Step 3: forward DCT per block → Cik[blk][1..=Ji].
    // Cik layout: [blk_idx 1..=4][k 0..=MAX_HOC_TERMS].
    let mut cik = [[0.0_f32; MAX_HOC_TERMS + 1]; IDCT_BLOCKS + 1];
    let mut base: usize = 0;
    for blk in 1..=IDCT_BLOCKS {
        let ji_val = *ji.get(blk).unwrap_or(&0);
        if ji_val == 0 {
            continue;
        }
        let block_end = (base + ji_val).min(n);
        let block_len = block_end.saturating_sub(base);
        if block_len == 0 {
            continue;
        }
        // Forward DCT on this block: Cik[blk][k] = Σ_j log_m[base+j]·cos(π·(k−1)·(j+0.5)/ji_val)
        // Matches decode's inverse_dct_blocks formula with j=k and ji_val=N.
        for k in 1..=ji_val.min(MAX_HOC_TERMS) {
            let mut sum = 0.0_f32;
            let step = std::f32::consts::PI * (k as f32 - 1.0) / ji_val as f32;
            for j in 0..block_len {
                // Decode uses (j - 0.5) for j=1..=ji_val; our offset
                // with j=0..ji_val uses (j + 0.5).
                let angle = step * (j as f32 + 0.5);
                sum += log_m[base + j] * angle.cos();
            }
            // Normalization: the decoder's inverse_dct_blocks
            // applies `ak` = 1 for k=1, 2 for k≥2. Our forward pass
            // reverses that by dividing all bins equally by N; the
            // decoder's factor-of-2 for AC bins restores the original
            // amplitude. Both the DC and AC branches here use the
            // same 1/N scale — the decoder handles the asymmetry.
            if let Some(block) = cik.get_mut(blk)
                && let Some(slot) = block.get_mut(k)
            {
                *slot = sum / ji_val as f32;
            }
        }
        base = block_end;
    }

    if dump_enabled() {
        dump_quantize_blocks(&ji, &cik);
    }

    // Step 4: Ri pairs.
    // Decode: Cik[blk][1] = 0.5·(r_odd + r_even); Cik[blk][2] = (1/(2√2))·(r_odd − r_even).
    // Encode inverse: r_odd = Cik[blk][1] + √2·Cik[blk][2];
    //                 r_even = Cik[blk][1] − √2·Cik[blk][2].
    let sqrt2 = std::f32::consts::SQRT_2;
    let mut ri = [0.0_f32; PRBA_BLOCKS + 1];
    for blk in 1..=IDCT_BLOCKS {
        let c1 = *cik.get(blk).and_then(|b| b.get(1)).unwrap_or(&0.0);
        let c2 = *cik.get(blk).and_then(|b| b.get(2)).unwrap_or(&0.0);
        let r_odd = sqrt2.mul_add(c2, c1);
        let r_even = sqrt2.mul_add(-c2, c1);
        ri[2 * blk - 1] = r_odd;
        ri[2 * blk] = r_even;
    }

    // Step 5: inverse 8-point DCT → Gm → PRBA codebooks.
    let gm = inverse_dct_8(&ri);
    if dump_enabled() {
        dump_quantize_gm(&ri, &gm);
    }
    let prba24_target = [gm[2], gm[3], gm[4]];
    let prba58_target = [gm[5], gm[6], gm[7], gm[8]];
    let b3 = nearest_prba24(&prba24_target);
    let b4 = nearest_prba58(&prba58_target);

    // Step 6: HOC codebook searches for blocks 1..=4.
    //
    // Only `min(Ji - 2, 4)` HOC dimensions per block are real data:
    // positions 1-2 (DC, first-AC) went to PRBA, leaving Ji-2 AC
    // coefficients available for HOC — capped at 4 because each HOC
    // codebook row is only 4-D.  Blocks with Ji ≤ 2 have no HOC
    // information at all and use codebook index 0 (per OP25
    // `ambe_encoder.cc:393-394`).
    //
    // Prior revision always compared 4 dimensions, padding with
    // zeros when the block was short. For a real block with
    // `Ji = 3` only the first target coordinate is real; the rest
    // are zero. The full-4D nearest-neighbor search then found the
    // codebook row whose LAST three coordinates were closest to
    // zero rather than the row whose FIRST coordinate matched the
    // real target — a completely different HOC vector. That's the
    // "envelope warped; 2f0 louder than f0" symptom in end-to-end
    // decoded audio: block-wise envelope terms reconstructed with
    // the wrong HOC signs/magnitudes.
    let hoc_target = |blk: usize| -> [f32; 4] {
        [
            *cik.get(blk).and_then(|b| b.get(3)).unwrap_or(&0.0),
            *cik.get(blk).and_then(|b| b.get(4)).unwrap_or(&0.0),
            *cik.get(blk).and_then(|b| b.get(5)).unwrap_or(&0.0),
            *cik.get(blk).and_then(|b| b.get(6)).unwrap_or(&0.0),
        ]
    };
    let hoc_dims = |blk: usize| -> usize {
        let ji_val = *ji.get(blk).unwrap_or(&0);
        ji_val.saturating_sub(2).min(4)
    };
    let b5 = if hoc_dims(1) == 0 {
        0
    } else {
        nearest_hoc(&tables::HOC_B5_TABLE, &hoc_target(1), hoc_dims(1), 1)
    };
    let b6 = if hoc_dims(2) == 0 {
        0
    } else {
        nearest_hoc(&tables::HOC_B6_TABLE, &hoc_target(2), hoc_dims(2), 1)
    };
    let b7 = if hoc_dims(3) == 0 {
        0
    } else {
        nearest_hoc(&tables::HOC_B7_TABLE, &hoc_target(3), hoc_dims(3), 1)
    };
    // b8: stride-2 search over the 16-row HOCb8 codebook.
    //
    // OP25's `ambe_encoder.cc:495` uses `max_8 = (dstar) ? 16 : 8`,
    // searching all 16 rows and storing whichever has minimum SSE.
    // OP25's trace dumper (`ambe_encode_dump`) reads the wire back
    // through `decode_dstar`, whose `load_reg` reconstructs the LOW
    // 3 bits of OP25's internal `b[8]` (`p25p2_vf.cc:39,45`).
    //
    // Our wire format is mbelib's: `ambe_d[35..=37]` hold bits 3, 2,
    // 1 of the 4-bit index, with bit 0 forced to 0
    // (`ref/mbelib/ambe3600x2400.c:436-440`). Reconstruction uses
    // the MIDDLE 3 bits, not the LOW 3.
    //
    // Consequence: if we'd done the stride=1 full-row search, an
    // odd-indexed best pick (say row k+1) would pack-collapse to
    // even row k on our wire — and row k may have HIGHER SSE than
    // row k+2 would. Stride=2 evaluates `{k, k+2}` directly and
    // picks whichever has lower SSE. That's strictly ≤ what
    // stride=1 achieves under mbelib wire packing. Empirically
    // (chirp fixture), stride=1 and stride=2 produce identical
    // wire bytes AND identical validator b8 match rate (22%) — the
    // remaining gap is structural wire-format difference with
    // OP25's D-STAR path, not search policy.
    let b8 = if hoc_dims(4) == 0 {
        0
    } else {
        nearest_hoc(&tables::HOC_B8_TABLE, &hoc_target(4), hoc_dims(4), 2)
    };

    QuantizedSpectrum {
        b3,
        b4,
        b5,
        b6,
        b7,
        b8,
    }
}

/// Look up `Ji[1..=4]` (block harmonic counts) for the current `L`.
fn lookup_ji(big_l: usize) -> [usize; IDCT_BLOCKS + 1] {
    let mut ji = [0_usize; IDCT_BLOCKS + 1];
    let row = tables::LMPRBL_TABLE.get(big_l);
    for idx in 0..IDCT_BLOCKS {
        if let Some(slot) = ji.get_mut(idx + 1) {
            #[allow(
                clippy::cast_sign_loss,
                reason = "LMPRBL_TABLE values are always 2..=17"
            )]
            {
                *slot = row.and_then(|r| r.get(idx)).copied().unwrap_or(0) as usize;
            }
        }
    }
    ji
}

/// Generic nearest-neighbor search against a 4-D HOC codebook with a
/// configurable index stride.
///
/// `stride` must be 1 or 2. Stride-2 searches only even-indexed rows
/// of the table and is required for `HOC_B8_TABLE`: the D-STAR 2400
/// wire format allocates only 3 bits for `b8` with the LSB forced
/// to 0 per the AMBE+ patent, so only even indices are physically
/// representable on the wire. Scanning all 16 rows and picking an
/// odd index would silently remap to the adjacent even entry at the
/// decoder, producing the wrong block-4 HOC ~50% of the time.
///
/// The other HOC tables (`B5`/`B6`/`B7`) are 16 rows × 4 bits — the
/// full index range is addressable, so they use `stride == 1`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "HOC tables have at most 16 entries; idx fits in u8"
)]
fn nearest_hoc(table: &[[f32; 4]], target: &[f32; 4], dims: usize, stride: usize) -> u8 {
    debug_assert!(stride == 1 || stride == 2, "stride must be 1 or 2");
    debug_assert!((1..=4).contains(&dims), "dims must be 1..=4");
    let mut best_idx: u8 = 0;
    let mut best_err = f32::INFINITY;
    for (idx, row) in table.iter().enumerate().step_by(stride) {
        let mut err = 0.0_f32;
        for k in 0..dims {
            let d = row[k] - target[k];
            err += d * d;
        }
        if err < best_err {
            best_err = err;
            best_idx = idx as u8;
        }
    }
    best_idx
}

/// Inverse 8-point DCT — the exact undo of `decode::forward_dct_8`.
///
/// The decoder's forward DCT is:
/// `Ri[i] = Σ am·Gm[m]·cos(π·(m−1)·(i−0.5)/8)` with am=1 for m=1, 2 for m≥2.
///
/// Its inverse — derived via basis orthogonality — divides by `N=8`
/// for **both DC and AC** terms:
///
/// ```text
/// Gm[m] = (1/8) · Σ_{i=1..8} Ri[i] · cos(π·(m−1)·(i−0.5)/8)
/// ```
///
/// Derivation: multiply the decoder's forward equation by
/// `cos(π·(m'−1)·(i−0.5)/8)` and sum over `i ∈ 1..=8`. Orthogonality
/// of the cosine basis gives:
/// - For `m'=1`: `Σ_i Ri[i] = 1·Gm[1]·N + 0` → `Gm[1] = Σ/N = Σ/8`.
/// - For `m'>1`: `Σ_i Ri[i]·cos(...) = 2·Gm[m']·(N/2)` → `Gm[m'] = Σ·cos(...)/N = Σ/8`.
///
/// The `a_{m'}=2` weight on the decoder side exactly cancels the
/// `N/2` norm factor of the AC cosines, leaving the same `1/N` scale
/// as the DC term. Matches the mbelib-2400 / OP25 inverse-PRBA DCT,
/// both of which divide by 8 unconditionally.
///
/// An earlier version of this function used `1/N` for DC and `2/N`
/// for AC, which double-counted the `am=2` factor: encoder-side
/// `Gm[m>1]` came out 2× too large, the decoder's `forward_dct_8`
/// doubled it again, and the reconstructed `Ri` AC terms arrived at
/// the synthesis with a 4× boost — audibly a "loud but not-voice"
/// spectrum envelope.
#[allow(
    clippy::cast_precision_loss,
    reason = "all indices bounded by PRBA_BLOCKS=8; well within f32 mantissa"
)]
fn inverse_dct_8(ri: &[f32; PRBA_BLOCKS + 1]) -> [f32; PRBA_BLOCKS + 1] {
    let mut gm = [0.0_f32; PRBA_BLOCKS + 1];
    let n = PRBA_BLOCKS as f32;
    for (m, gm_slot) in gm.iter_mut().enumerate().take(PRBA_BLOCKS + 1).skip(1) {
        let mut sum = 0.0_f32;
        for (i, &ri_val) in ri.iter().enumerate().take(PRBA_BLOCKS + 1).skip(1) {
            let angle = std::f32::consts::PI * (m as f32 - 1.0) * (i as f32 - 0.5) / n;
            sum += ri_val * angle.cos();
        }
        *gm_slot = sum / n;
    }
    gm
}

/// Nearest-neighbor search against `PRBA24_TABLE` (3-D, 512 entries).
#[allow(
    clippy::cast_possible_truncation,
    reason = "PRBA24_TABLE has 512 entries; idx fits in u16"
)]
fn nearest_prba24(target: &[f32; 3]) -> u16 {
    let mut best_idx: u16 = 0;
    let mut best_err = f32::INFINITY;
    for (idx, row) in tables::PRBA24_TABLE.iter().enumerate() {
        let mut err = 0.0_f32;
        for k in 0..3 {
            let d = row[k] - target[k];
            err += d * d;
        }
        if err < best_err {
            best_err = err;
            best_idx = idx as u16;
        }
    }
    best_idx
}

/// Nearest-neighbor search against `PRBA58_TABLE` (4-D, 128 entries).
#[allow(
    clippy::cast_possible_truncation,
    reason = "PRBA58_TABLE has 128 entries; idx fits in u8"
)]
fn nearest_prba58(target: &[f32; 4]) -> u8 {
    let mut best_idx: u8 = 0;
    let mut best_err = f32::INFINITY;
    for (idx, row) in tables::PRBA58_TABLE.iter().enumerate() {
        let mut err = 0.0_f32;
        for k in 0..4 {
            let d = row[k] - target[k];
            err += d * d;
        }
        if err < best_err {
            best_err = err;
            best_idx = idx as u8;
        }
    }
    best_idx
}

#[cfg(test)]
mod tests {
    use super::{PrevFrameState, quantize, quantize_pitch};
    use crate::encode::pitch::PitchEstimate;
    use crate::encode::vuv::{MAX_BANDS, VuvDecisions};

    #[test]
    fn pitch_200hz_quantizes_to_valid_index() {
        let est = PitchEstimate {
            period_samples: 40.0,
            f0_hz: 200.0,
            confidence: 0.9,
        };
        // 200 Hz at L=30 — plausible voiced mid-range parameters.
        let idx = quantize_pitch(est, 30);
        // 200 Hz / 8000 = 0.025 cycles/sample, which lives somewhere
        // in the middle of the W0_TABLE (indices ~60..70).
        assert!(idx < 120, "idx should be in voice range, got {idx}");
    }

    #[test]
    fn silence_quantizes_to_silence_code() {
        let est = PitchEstimate {
            period_samples: 100.0,
            f0_hz: 80.0,
            confidence: 0.0,
        };
        // target_l ignored because confidence < 0.05 triggers the
        // silence-code early return.
        let idx = quantize_pitch(est, 14);
        assert!(matches!(idx, 124 | 125), "expected silence code, got {idx}");
    }

    #[test]
    fn quantize_produces_49_bits() {
        use crate::encode::spectral::{MAX_HARMONICS, SpectralAmplitudes};
        let pitch = PitchEstimate {
            period_samples: 40.0,
            f0_hz: 200.0,
            confidence: 0.9,
        };
        let vuv = VuvDecisions {
            voiced: [true; MAX_BANDS],
            num_bands: 8,
        };
        let amps = SpectralAmplitudes {
            magnitudes: [0.0; MAX_HARMONICS],
            num_harmonics: 0,
        };
        let prev = PrevFrameState {
            log2_ml: [0.0_f32; 57],
            l: 0,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);
        assert_eq!(outcome.ambe_d.len(), 49);
        assert!(outcome.ambe_d.iter().all(|&b| b <= 1));
    }

    /// Per-block forward DCT + decoder-equivalent inverse DCT must
    /// recover the original `log_m` vector within numerical noise.
    ///
    /// Decoder's per-block inverse is unscaled:
    /// `Tl[l] = Σ_k ak · Cik[blk][k] · cos(π·(k−1)·(l−0.5)/N)` with
    /// `ak = 1 for k=1, 2 for k≥2`. Encoder's forward divides by N
    /// for all k. This pair should round-trip via the same
    /// orthogonality argument as `inverse_dct_8_recovers_gm`.
    #[test]
    fn block_dct_round_trip_preserves_log_m() {
        let n = 12_usize;
        let log_m_in: [f32; 12] = [
            1.5, 1.2, 0.9, 0.7, 0.5, 0.3, 0.1, -0.1, -0.3, -0.5, -0.7, -0.9,
        ];

        // Encoder forward: Cik[k] = (1/N) · Σ_j log_m[j] · cos(π·(k−1)·(j+0.5)/N).
        // NOTE: encoder uses (j+0.5) with j=0..N-1 which is equivalent to the
        // decoder's (j-0.5) with j=1..=N.
        let mut cik = [0.0_f32; 12];
        for (k_idx, cik_slot) in cik.iter_mut().enumerate().take(n) {
            #[allow(clippy::cast_precision_loss)]
            let step = std::f32::consts::PI * k_idx as f32 / n as f32;
            let mut sum = 0.0_f32;
            for (j_idx, &v) in log_m_in.iter().enumerate().take(n) {
                #[allow(clippy::cast_precision_loss)]
                let angle = step * (j_idx as f32 + 0.5);
                sum += v * angle.cos();
            }
            #[allow(clippy::cast_precision_loss)]
            {
                *cik_slot = sum / n as f32;
            }
        }

        // Decoder inverse: Tl[l] = Σ_k ak · Cik[k] · cos(π·(k−1)·(l−0.5)/N).
        let mut tl = [0.0_f32; 12];
        for (l_minus_1, tl_slot) in tl.iter_mut().enumerate().take(n) {
            #[allow(clippy::cast_precision_loss)]
            let step = std::f32::consts::PI * (l_minus_1 as f32 + 0.5) / n as f32;
            let mut sum = 0.0_f32;
            for (k_idx, &c) in cik.iter().enumerate().take(n) {
                let ak: f32 = if k_idx == 0 { 1.0 } else { 2.0 };
                #[allow(clippy::cast_precision_loss)]
                let angle = step * k_idx as f32;
                sum += ak * c * angle.cos();
            }
            *tl_slot = sum;
        }

        for (i, (&input, &output)) in log_m_in.iter().zip(tl.iter()).enumerate().take(n) {
            let err = (output - input).abs();
            assert!(
                err < 1e-4,
                "log_m[{i}] roundtrip error {err}: in={input} out={output}"
            );
        }
    }

    /// DCT orthogonality check: `inverse_dct_8(forward_dct_8(gm)) == gm`.
    ///
    /// The decoder's `forward_dct_8` uses `am = 1 for m=1, 2 for m≥2`
    /// weights. The encoder's inverse must divide by `N = 8` for
    /// both DC and AC components — the `am=2` factor on the decoder
    /// side exactly cancels the `N/2` cosine-norm factor, leaving
    /// the same `1/N` scale as DC. A prior revision divided AC by
    /// `N/2` which produced a 2× AC-gain, and the decoder's forward
    /// DCT doubled it again to give a 4× AC boost on reconstructed
    /// `Ri` — heard as "intelligibly-loud but spectrally-wrong
    /// voice envelope" in sextant↔sextant tests.
    #[test]
    fn inverse_dct_8_recovers_gm() {
        // A non-degenerate Gm vector with both DC and AC energy.
        // Index 0 is a sentinel (decoder convention is 1-based).
        let gm_in: [f32; 9] = [0.0, 1.5, -0.7, 0.3, -0.1, 0.4, -0.25, 0.15, -0.05];

        // Feed through the decoder's forward_dct_8 equivalent.
        // Reimplemented here because the real function is private
        // to the decode module.
        let mut ri = [0.0_f32; 9];
        for (i, ri_slot) in ri.iter_mut().enumerate().take(9).skip(1) {
            #[allow(clippy::cast_precision_loss)]
            let step = std::f32::consts::PI * (i as f32 - 0.5) / 8.0;
            let mut sum = 0.0_f32;
            for (m, &gm_val) in gm_in.iter().enumerate().take(9).skip(1) {
                #[allow(clippy::cast_precision_loss)]
                let angle = step * (m as f32 - 1.0);
                let am: f32 = if m == 1 { 1.0 } else { 2.0 };
                sum += am * gm_val * angle.cos();
            }
            *ri_slot = sum;
        }

        // Round-trip through our inverse_dct_8.
        let gm_out = super::inverse_dct_8(&ri);
        for (m, (&input, &output)) in gm_in.iter().zip(gm_out.iter()).enumerate().take(9).skip(1) {
            let err = (output - input).abs();
            assert!(
                err < 1e-5,
                "gm[{m}] recovery error {err}: in={input} out={output}"
            );
        }
    }

    // -------------------------------------------------------------------
    // Regression tests — each pins a specific bug we fixed during the
    // stage-by-stage OP25 validation pass. A failure on any of these
    // indicates a regression in the quantize pipeline.
    // -------------------------------------------------------------------

    /// Fix #1 (`SA_SCALE`): without `SA_SCALE = 32768` mapping our
    /// [-1, 1] f32 magnitudes into OP25's int16 `imbe_param->sa[]`
    /// domain, `log2(sa)` sits 15 bits below IMBE's scale and every
    /// PRBA/HOC codebook search collapses to the all-zero corner.
    ///
    /// This test feeds a moderate-amplitude input and checks that
    /// the gain index `b2` lands somewhere meaningful — neither at
    /// the codebook floor (0) nor clamped at the ceiling (63) for a
    /// mid-level signal. A regression like `SA_SCALE = 1` would pin
    /// b2 to 0; `SA_SCALE = 2^30` would pin it to 63.
    #[test]
    fn sa_scale_maps_amps_into_gain_codebook_range() {
        use crate::encode::spectral::{MAX_HARMONICS, SpectralAmplitudes};
        let pitch = PitchEstimate {
            period_samples: 50.0, // 160 Hz
            f0_hz: 160.0,
            confidence: 0.9,
        };
        let vuv = VuvDecisions {
            voiced: [true; MAX_BANDS],
            num_bands: 10,
        };
        let mut magnitudes = [0.0_f32; MAX_HARMONICS];
        // Voiced harmonic magnitudes in a typical mic-level range.
        // Peak ~0.15 (−16 dBFS) across 20 harmonics.
        for slot in magnitudes.iter_mut().take(20) {
            *slot = 0.15;
        }
        let amps = SpectralAmplitudes {
            magnitudes,
            num_harmonics: 20,
        };
        let prev = PrevFrameState {
            log2_ml: [0.0_f32; 57],
            l: 0,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);
        // Extract b2 from the emitted ambe_d.
        let a = &outcome.ambe_d;
        let bit = |k: usize| u8::from(a[k] != 0);
        let b2 = (bit(6) << 5)
            | (bit(7) << 4)
            | (bit(8) << 3)
            | (bit(9) << 2)
            | (bit(42) << 1)
            | bit(43);
        assert!(
            (5..=63).contains(&b2),
            "b2 for a voiced moderate-amplitude input should sit in \
             the upper half of DG_TABLE, got {b2} — likely SA_SCALE \
             regression"
        );
    }

    /// Fix #2 (closed-loop prev): encoder must emit `prev_log2_ml`
    /// that a fresh decoder would reconstruct from the same
    /// emitted bytes. If we stored raw `lsa` instead of running
    /// `decode_params` internally, the prediction residual drifts
    /// every frame.
    ///
    /// This test encodes a voiced frame, then independently runs
    /// our decoder on the emitted `ambe_d` and asserts the decoder's
    /// `log2_ml` matches what `quantize()` returned as
    /// `prev_log2_ml`.
    #[test]
    fn prev_log2_ml_matches_decoder_reconstruction() {
        use crate::decode;
        use crate::encode::spectral::{MAX_HARMONICS, SpectralAmplitudes};
        use crate::params::MbeParams;
        let pitch = PitchEstimate {
            period_samples: 50.0,
            f0_hz: 160.0,
            confidence: 0.9,
        };
        let vuv = VuvDecisions {
            voiced: [true; MAX_BANDS],
            num_bands: 10,
        };
        let mut magnitudes = [0.0_f32; MAX_HARMONICS];
        for slot in magnitudes.iter_mut().take(25) {
            *slot = 0.10;
        }
        let amps = SpectralAmplitudes {
            magnitudes,
            num_harmonics: 25,
        };
        let prev = PrevFrameState {
            log2_ml: [0.0_f32; 57],
            l: 0,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);

        // Independently decode the emitted ambe_d.
        let mut cur = MbeParams::new();
        let decoder_prev = {
            let mut p = MbeParams::new();
            p.log2_ml = prev.log2_ml;
            p.l = prev.l;
            p
        };
        let _status = decode::decode_params(&outcome.ambe_d, &mut cur, &decoder_prev);

        // Encoder's carry-forward prev_log2_ml must match the
        // decoder's reconstructed log2_ml bit-for-bit.
        for i in 0..=cur.l {
            let encoder_side = outcome.prev_log2_ml.get(i).copied().unwrap_or(f32::NAN);
            let decoder_side = cur.log2_ml.get(i).copied().unwrap_or(f32::NAN);
            assert!(
                (encoder_side - decoder_side).abs() < 1e-6,
                "prev_log2_ml[{i}] diverges from decoder: encoder={encoder_side} \
                 decoder={decoder_side}"
            );
        }
        assert_eq!(
            outcome.prev_l, cur.l,
            "prev_l must match decoder's reconstructed L"
        );
    }

    /// Fix #5 (per-harmonic voicing via `i/3` band expansion):
    /// `compute_lsa` must map harmonic index `i` to band `i/3` when
    /// reading `vuv.voiced`. If it reads `vuv.voiced[i]` directly,
    /// every harmonic past `num_bands` (i.e. past the 12th) picks
    /// the wrong voicing offset, shifting `lsa[i]` by the
    /// `log_l_w0 - log_l_2` gap (~0.44 for typical voice pitch).
    ///
    /// This test forces a scenario where the bug would be visible:
    /// all bands voiced, 30 harmonics. If the bug is back,
    /// harmonics 12..=29 would read `vuv.voiced[12..=29]` which is
    /// the `false` zero-padding and treat those as unvoiced.
    #[test]
    fn per_harmonic_voicing_uses_band_expansion() {
        use crate::encode::spectral::{MAX_HARMONICS, SpectralAmplitudes};
        let pitch = PitchEstimate {
            period_samples: 40.0,
            f0_hz: 200.0,
            confidence: 0.9,
        };
        // 10 bands all voiced; voiced[10..] would be the default false.
        let mut voiced = [false; MAX_BANDS];
        voiced.iter_mut().take(10).for_each(|v| *v = true);
        let vuv = VuvDecisions {
            voiced,
            num_bands: 10,
        };
        let mut magnitudes = [0.0_f32; MAX_HARMONICS];
        for slot in magnitudes.iter_mut().take(30) {
            *slot = 0.05;
        }
        let amps = SpectralAmplitudes {
            magnitudes,
            num_harmonics: 30,
        };
        let prev = PrevFrameState {
            log2_ml: [0.0_f32; 57],
            l: 0,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);

        // b1 (VUV index) is 4 bits at positions [38, 39, 40, 41]. For
        // all 10 bands voiced with the band-expansion bug absent, the
        // V/UV codebook search should pick row 15 (all-voiced) — the
        // minimum-energy pattern when every band matches. With the
        // bug (voiced[i] read per-harmonic), harmonics 10..=29 report
        // unvoiced, pulling the search toward a mid-voiced row.
        let a = &outcome.ambe_d;
        let bit = |k: usize| u8::from(a[k] != 0);
        let b1 = (bit(38) << 3) | (bit(39) << 2) | (bit(40) << 1) | bit(41);
        assert_eq!(
            b1, 15,
            "b1 should pick the all-voiced row (15) when every band is \
             voiced; got {b1} — likely per-harmonic band-expansion \
             regression"
        );
    }

    /// Fix #6 (L-constrained b0 search): `quantize_pitch` must pick
    /// a `b0` where `L_TABLE[b0] == target_l`, not purely the
    /// nearest-W0. Multiple `b0` values map to the same L; nearest-W0
    /// alone can land on a b0 whose L differs from the one the
    /// spectral analysis computed harmonics for, desync'ing the
    /// downstream quantizer.
    #[test]
    fn quantize_pitch_respects_target_l() {
        use crate::tables::L_TABLE;
        let est = PitchEstimate {
            period_samples: 40.0, // 200 Hz
            f0_hz: 200.0,
            confidence: 0.9,
        };
        // For a mid-range target_l, the chosen b0 must satisfy
        // L_TABLE[b0] == target_l — this is the whole point of the
        // L-constrained search.
        for target_l in [18_usize, 24, 30, 40] {
            let b0 = quantize_pitch(est, target_l);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let got_l = L_TABLE[b0 as usize] as usize;
            assert_eq!(
                got_l, target_l,
                "L_TABLE[b0={b0}]={got_l} does not match target_l={target_l} \
                 — nearest-W0 fallback fired when a matching entry exists"
            );
        }
    }
}
