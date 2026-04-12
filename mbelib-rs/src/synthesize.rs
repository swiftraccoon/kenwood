//! Speech synthesis from decoded AMBE parameters.
//!
//! Converts the decoded (and enhanced) harmonic speech model parameters
//! back into a PCM audio waveform. This is the CPU-hot path of the
//! AMBE decoder — each frame requires evaluating cosines for every
//! harmonic band at every sample position (up to 56 bands x 160 samples).
//!
//! # Synthesis Model
//!
//! The AMBE codec models speech as a sum of harmonically related sinusoids:
//!
//! ```text
//! s(n) = sum_{l=1}^{L} M_l * cos(phi_l(n))
//! ```
//!
//! where `M_l` is the magnitude and `phi_l(n)` is the instantaneous phase
//! of harmonic band `l`. Each band is independently classified as either:
//!
//! - **Voiced**: a deterministic cosine oscillator with phase continuous
//!   from the previous frame, producing the periodic "buzz" component of
//!   speech.
//!
//! - **Unvoiced**: a multisine (sum of closely-spaced cosines with random
//!   phases), producing the noise-like "hiss" component (fricatives,
//!   breath sounds).
//!
//! # Cross-Frame Overlap
//!
//! To prevent discontinuities at frame boundaries, the synthesis uses the
//! `Ws` window (321 samples: 56 zeros, 49 ramp-up, 111 ones, 49 ramp-down,
//! 56 zeros). For each 160-sample frame:
//!
//! - `Ws[n + N]` (where N=160) windows the **previous** frame's
//!   contribution — it ramps down from 1 to 0 over the frame.
//! - `Ws[n]` windows the **current** frame's contribution — it ramps
//!   up from 0 to 1 over the frame.
//!
//! This overlap-add gives a smooth cross-fade between consecutive frames.
//!
//! # Phase Continuity
//!
//! Phase is updated per-band using the PSI/PHI system from the AMBE spec:
//!
//! ```text
//! PSI_l = PSI_l_prev + (w0_prev + w0_cur) * l * N / 2
//! PHI_l = PSI_l                          (for voiced, low bands l <= L/4)
//! PHI_l = PSI_l + random_offset          (for higher/unvoiced bands)
//! ```
//!
//! PSI provides the smooth phase prediction; PHI adds controlled randomness
//! for bands that are not strongly periodic. Both are updated in-place on
//! `cur` during synthesis and carry forward to the next frame.
//!
//! This is a direct port of `mbe_synthesizeSpeechf()` from the ISC-licensed
//! mbelib C library (<https://github.com/szechyjs/mbelib>).

use crate::params::MbeParams;
use crate::tables;

/// Number of PCM samples per AMBE frame (20 ms at 8000 Hz).
pub(crate) const FRAME_SAMPLES: usize = 160;

/// Number of unvoiced oscillators per band.
///
/// The C reference calls this `uvquality` and defaults to 3. More
/// oscillators produce smoother unvoiced noise at the cost of more
/// cosine evaluations per sample. 3 is the standard quality level
/// used by most D-STAR implementations.
const UV_QUALITY: usize = 3;

/// Unvoiced amplitude scaling factor: `1.359_140_9 * e`.
///
/// This empirical constant from the mbelib C source scales the unvoiced
/// multisine output to match the expected energy level of the voiced
/// signal. The factor `e` (~2.718) appears because the original codec
/// design uses a natural-log-based energy model.
const UV_SINE: f32 = 1.359_140_9 * std::f32::consts::E;

/// Unvoiced high-frequency noise injection amplitude.
///
/// When a band's frequency exceeds the [`UV_THRESHOLD`], additional random
/// noise is mixed in proportional to the excess frequency. This models
/// the increasing "breathiness" of high-frequency unvoiced speech sounds.
const UV_RAND: f32 = 2.0;

/// Frequency threshold above which extra noise is injected into unvoiced
/// bands. Corresponds to 2700 Hz mapped to radians: `2700 * pi / 4000`.
const UV_THRESHOLD: f32 = 2700.0 * std::f32::consts::PI / 4000.0;

/// Precomputed unvoiced synthesis parameters derived from [`UV_QUALITY`].
///
/// Bundled into a struct to avoid passing many individual arguments to
/// the per-sample unvoiced multisine helper.
struct UvParams {
    /// Per-oscillator quality scaling: `ln(uvquality) / uvquality`.
    qfactor: f32,
    /// Frequency step between oscillators: `1 / uvquality`.
    step: f32,
    /// Symmetric offset to center the oscillator spread.
    offset: f32,
}

impl UvParams {
    /// Computes unvoiced parameters from the compile-time quality setting.
    fn new() -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "UV_QUALITY is 3; no precision loss in f32"
        )]
        let q_f32 = UV_QUALITY as f32;
        let step = 1.0 / q_f32;
        #[expect(
            clippy::cast_precision_loss,
            reason = "UV_QUALITY is 3; no precision loss in f32"
        )]
        let offset = step * (UV_QUALITY - 1) as f32 / 2.0;
        Self {
            qfactor: q_f32.ln() / q_f32,
            step,
            offset,
        }
    }
}

/// Synthesizes one frame of PCM audio from enhanced model parameters.
///
/// Generates 160 samples (20 ms at 8 kHz) by summing windowed cosine
/// oscillators for each harmonic band, with cross-fade between the
/// previous and current frames.
///
/// # Arguments
///
/// * `pcm` - Output buffer, filled with 160 float PCM samples.
/// * `cur` - Current frame's enhanced parameters. Modified in-place:
///   `psi_l`, `phi_l`, `ml`, and `vl` are updated for phase continuity
///   and band extension.
/// * `prev_enh` - Previous frame's enhanced parameters. Modified in-place:
///   `ml` and `vl` are extended when the current frame has more bands.
///
/// # Four synthesis paths
///
/// For each band `1..=max(cur.l, prev_enh.l)`, one of four cases applies:
///
/// 1. **Prev voiced, cur unvoiced**: Cross-fade from voiced oscillator
///    (ramping down via `Ws[n+N]`) to unvoiced multisine (ramping up
///    via `Ws[n]`).
///
/// 2. **Prev unvoiced, cur voiced**: Cross-fade from unvoiced multisine
///    (ramping down) to voiced oscillator (ramping up).
///
/// 3. **Either voiced (both voiced, or mixed with one voiced)**: Smooth
///    overlap of two voiced oscillators using the window pair.
///
/// 4. **Both unvoiced**: Cross-fade between two independent unvoiced
///    multisines, one for the previous frame and one for the current.
pub(crate) fn synthesize_speech(
    pcm: &mut [f32; FRAME_SAMPLES],
    cur: &mut MbeParams,
    prev_enh: &mut MbeParams,
) {
    let uv = UvParams::new();

    // Count unvoiced bands in the current frame (used for phase
    // randomization in the PSI/PHI update below).
    let num_uv: usize = (1..=cur.l)
        .filter(|&l| !cur.vl.get(l).copied().unwrap_or(true))
        .count();

    let cw0 = cur.w0;
    let pw0 = prev_enh.w0;

    // Initialize output buffer to silence.
    *pcm = [0.0_f32; FRAME_SAMPLES];

    // Band extension and phase update.
    let maxl = extend_bands(cur, prev_enh);
    update_phases(cur, prev_enh, cw0, pw0, num_uv);

    // Main synthesis loop: for each band, choose one of four paths
    // based on the voiced/unvoiced classification of both frames.
    for l in 1..=maxl {
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56; no precision loss in f32"
        )]
        let l_f32 = l as f32;

        let band = BandCtx {
            index: l,
            l_f32,
            cw0l: cw0 * l_f32,
            pw0l: pw0 * l_f32,
            cw0,
            pw0,
            prev_ml: prev_enh.ml.get(l).copied().unwrap_or(0.0),
            cur_ml: cur.ml.get(l).copied().unwrap_or(0.0),
            prev_phi: prev_enh.phi_l.get(l).copied().unwrap_or(0.0),
            cur_phi: cur.phi_l.get(l).copied().unwrap_or(0.0),
        };

        let cur_voiced = cur.vl.get(l).copied().unwrap_or(true);
        let prev_voiced = prev_enh.vl.get(l).copied().unwrap_or(true);

        match (prev_voiced, cur_voiced) {
            (true, false) => synth_voiced_to_unvoiced(pcm, &band, &uv),
            (false, true) => synth_unvoiced_to_voiced(pcm, &band, &uv),
            (_, _) if cur_voiced || prev_voiced => synth_voiced_overlap(pcm, &band),
            (_, _) => synth_unvoiced_both(pcm, &band, &uv),
        }
    }
}

/// Per-band synthesis context, bundling all parameters needed by the
/// four synthesis path helpers.
struct BandCtx {
    /// Band number (1-indexed).
    index: usize,
    /// Band number as f32 (precomputed to avoid repeated casts).
    l_f32: f32,
    /// Current fundamental frequency times band number: `cw0 * l`.
    cw0l: f32,
    /// Previous fundamental frequency times band number: `pw0 * l`.
    pw0l: f32,
    /// Current frame's fundamental radian frequency.
    cw0: f32,
    /// Previous frame's fundamental radian frequency.
    pw0: f32,
    /// Previous frame's magnitude for this band.
    prev_ml: f32,
    /// Current frame's magnitude for this band.
    cur_ml: f32,
    /// Previous frame's phase for this band.
    prev_phi: f32,
    /// Current frame's phase for this band.
    cur_phi: f32,
}

/// Extends the shorter band array with zero-magnitude voiced bands.
///
/// When the current and previous frames have different harmonic counts,
/// the shorter one is padded so the cross-fade can operate across all
/// bands (equations 128-129 from the AMBE spec).
fn extend_bands(cur: &mut MbeParams, prev_enh: &mut MbeParams) -> usize {
    if cur.l > prev_enh.l {
        for l in (prev_enh.l + 1)..=cur.l {
            if let Some(slot) = prev_enh.ml.get_mut(l) {
                *slot = 0.0;
            }
            if let Some(slot) = prev_enh.vl.get_mut(l) {
                *slot = true;
            }
        }
        cur.l
    } else {
        for l in (cur.l + 1)..=prev_enh.l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot = 0.0;
            }
            if let Some(slot) = cur.vl.get_mut(l) {
                *slot = true;
            }
        }
        prev_enh.l
    }
}

/// Updates PSI (smooth predicted phase) and PHI (randomized phase) for
/// all 56 bands.
///
/// PSI provides the smooth predicted phase assuming constant w0.
/// PHI adds controlled randomness for bands above `L/4`, which
/// prevents the "metallic" quality that would result from perfectly
/// deterministic phase across all bands. The random offset is
/// proportional to `num_uv / L`, so frames with more unvoiced bands
/// get more phase randomization.
fn update_phases(cur: &mut MbeParams, prev_enh: &MbeParams, cw0: f32, pw0: f32, num_uv: usize) {
    for l in 1..=56 {
        #[expect(
            clippy::cast_precision_loss,
            reason = "l is at most 56 and FRAME_SAMPLES is 160; product fits in f32"
        )]
        let half_phase = (pw0 + cw0) * (l as f32 * FRAME_SAMPLES as f32 / 2.0);

        let prev_psi = prev_enh.psi_l.get(l).copied().unwrap_or(0.0);
        let psi = prev_psi + half_phase;
        if let Some(slot) = cur.psi_l.get_mut(l) {
            *slot = psi;
        }

        // Low bands (l <= L/4) get deterministic phase tracking.
        // Higher bands get a random offset scaled by the fraction of
        // unvoiced bands, adding appropriate "breathiness".
        #[expect(
            clippy::cast_precision_loss,
            reason = "num_uv and cur.l are at most 56; no precision loss"
        )]
        let phi = if l <= cur.l / 4 {
            psi
        } else if cur.l > 0 {
            psi + (num_uv as f32 * deterministic_rand_phase(l) / cur.l as f32)
        } else {
            psi
        };
        if let Some(slot) = cur.phi_l.get_mut(l) {
            *slot = phi;
        }
    }
}

/// Case 1: Previous was voiced, current is unvoiced.
///
/// Cross-fade from a voiced oscillator (ramping down via `Ws[n+N]`)
/// to an unvoiced multisine (ramping up via `Ws[n]`).
fn synth_voiced_to_unvoiced(pcm: &mut [f32; FRAME_SAMPLES], b: &BandCtx, uv: &UvParams) {
    let rphase = init_random_phases(b.index, 0);

    for n in 0..FRAME_SAMPLES {
        // Ws[n+N] ramps down the previous voiced oscillator (eq 131).
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "n is at most 159; no precision loss"
        )]
        let c1 = ws_prev * b.prev_ml * b.pw0l.mul_add(n as f32, b.prev_phi).cos();

        // Ws[n] ramps up the current unvoiced multisine.
        let c3 = unvoiced_multisine(n, b.cw0, b.l_f32, b.cw0l, b.cur_ml, &rphase, uv);
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);

        if let Some(sample) = pcm.get_mut(n) {
            *sample += (c3 * UV_SINE).mul_add(ws_cur, c1);
        }
    }
}

/// Case 2: Previous was unvoiced, current is voiced.
///
/// Cross-fade from unvoiced multisine (ramping down via `Ws[n+N]`)
/// to voiced oscillator (ramping up via `Ws[n]`).
fn synth_unvoiced_to_voiced(pcm: &mut [f32; FRAME_SAMPLES], b: &BandCtx, uv: &UvParams) {
    let rphase = init_random_phases(b.index, 1);

    for n in 0..FRAME_SAMPLES {
        // Ws[n] ramps up the current voiced oscillator (eq 132).
        // Phase offset by -N to align with the frame boundary.
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "n and FRAME_SAMPLES are at most 160; no precision loss"
        )]
        let c1 = ws_cur
            * b.cur_ml
            * b.cw0l
                .mul_add(n as f32 - FRAME_SAMPLES as f32, b.cur_phi)
                .cos();

        // Ws[n+N] ramps down the previous unvoiced multisine.
        let c3 = unvoiced_multisine(n, b.pw0, b.l_f32, b.pw0l, b.prev_ml, &rphase, uv);
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);

        if let Some(sample) = pcm.get_mut(n) {
            *sample += (c3 * UV_SINE).mul_add(ws_prev, c1);
        }
    }
}

/// Case 3: At least one frame is voiced (typically both).
///
/// Overlap two voiced oscillators using the synthesis window.
/// This is the most common path during normal speech.
fn synth_voiced_overlap(pcm: &mut [f32; FRAME_SAMPLES], b: &BandCtx) {
    for n in 0..FRAME_SAMPLES {
        // Ws[n+N] ramps down the previous voiced oscillator (eq 133-1).
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "n is at most 159; no precision loss"
        )]
        let c1 = ws_prev * b.prev_ml * b.pw0l.mul_add(n as f32, b.prev_phi).cos();

        // Ws[n] ramps up the current voiced oscillator (eq 133-2).
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);
        #[expect(
            clippy::cast_precision_loss,
            reason = "n and FRAME_SAMPLES are at most 160; no precision loss"
        )]
        let c2 = ws_cur
            * b.cur_ml
            * b.cw0l
                .mul_add(n as f32 - FRAME_SAMPLES as f32, b.cur_phi)
                .cos();

        if let Some(sample) = pcm.get_mut(n) {
            *sample += c1 + c2;
        }
    }
}

/// Case 4: Both frames are unvoiced.
///
/// Cross-fade between two independent unvoiced multisines.
fn synth_unvoiced_both(pcm: &mut [f32; FRAME_SAMPLES], b: &BandCtx, uv: &UvParams) {
    let rphase_prev = init_random_phases(b.index, 2);
    let rphase_cur = init_random_phases(b.index, 3);

    for n in 0..FRAME_SAMPLES {
        // Ws[n+N] ramps down the previous unvoiced multisine.
        let c3 = unvoiced_multisine(n, b.pw0, b.l_f32, b.pw0l, b.prev_ml, &rphase_prev, uv);
        let ws_prev = tables::WS.get(n + FRAME_SAMPLES).copied().unwrap_or(0.0);

        // Ws[n] ramps up the current unvoiced multisine.
        let c4 = unvoiced_multisine(n, b.cw0, b.l_f32, b.cw0l, b.cur_ml, &rphase_cur, uv);
        let ws_cur = tables::WS.get(n).copied().unwrap_or(0.0);

        if let Some(sample) = pcm.get_mut(n) {
            *sample += (c3 * UV_SINE).mul_add(ws_prev, c4 * UV_SINE * ws_cur);
        }
    }
}

/// Computes the unvoiced multisine contribution for a single sample.
///
/// Sums [`UV_QUALITY`] cosine oscillators spread around the band center
/// frequency, each with its own random phase offset. Above the
/// [`UV_THRESHOLD`] frequency, additional noise proportional to the
/// excess frequency is mixed in.
fn unvoiced_multisine(
    n: usize,
    w0: f32,
    l_f32: f32,
    w0l: f32,
    ml: f32,
    rphase: &[f32; UV_QUALITY],
    uv: &UvParams,
) -> f32 {
    let mut sum: f32 = 0.0;
    for i in 0..UV_QUALITY {
        let phase_offset = rphase.get(i).copied().unwrap_or(0.0);
        // Each oscillator is at a slightly different frequency around
        // the band center: w0 * n * (l + i*step - offset).
        // This frequency spreading creates a noise-like signal when
        // the phases are random.
        #[expect(
            clippy::cast_precision_loss,
            reason = "i is at most UV_QUALITY-1 (2) and n is at most 159"
        )]
        let freq = w0 * n as f32 * (i as f32).mul_add(uv.step, l_f32 - uv.offset);
        sum += (freq + phase_offset).cos();

        // High-frequency noise injection: above the threshold, unvoiced
        // bands sound increasingly "breathy" in natural speech. This
        // adds proportional noise to model that effect.
        if w0l > UV_THRESHOLD {
            sum += (w0l - UV_THRESHOLD) * UV_RAND * deterministic_rand(n, i);
        }
    }
    // Scale by magnitude and quality factor.
    sum * ml * uv.qfactor
}

/// Generates a deterministic pseudo-random float in `[0.0, 1.0)`.
///
/// Replaces the C library's `rand()` with a hash-based PRNG that
/// produces the same output for the same inputs, ensuring fully
/// deterministic decoding. Uses a simple multiplicative hash
/// (Robert Jenkins' mix-like) for speed.
fn deterministic_rand(sample: usize, oscillator: usize) -> f32 {
    // Combine inputs into a single seed value. The golden ratio
    // constant provides good bit mixing between the two inputs.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "intentional truncation to u32 for hash mixing; upper bits \
                  are not needed for pseudo-random quality"
    )]
    let mut x: u32 = (sample as u32)
        .wrapping_mul(2_654_435_761)
        .wrapping_add(oscillator as u32)
        .wrapping_mul(1_597_334_677);

    // Jenkins' 32-bit integer hash for avalanche mixing.
    x = x.wrapping_add(x << 10);
    x ^= x >> 6;
    x = x.wrapping_add(x << 3);
    x ^= x >> 11;
    x = x.wrapping_add(x << 15);

    // Map to [0.0, 1.0) by dividing by 2^32.
    // Loss of the lowest mantissa bits is acceptable for PRNG output.
    #[expect(
        clippy::cast_precision_loss,
        reason = "precision loss in u32->f32 is acceptable for pseudo-random output; \
                  we only need approximately uniform distribution, not bit-exact"
    )]
    {
        (x as f32) / 4_294_967_296.0
    }
}

/// Generates a deterministic pseudo-random phase in `[-pi, +pi)`.
///
/// Used to initialize unvoiced oscillator phases. Each band
/// gets a unique but deterministic phase, replacing the C library's
/// `mbe_rand_phase()` which used `rand()`.
fn deterministic_rand_phase(band: usize) -> f32 {
    let x = deterministic_rand(band, 42);
    x.mul_add(std::f32::consts::TAU, -std::f32::consts::PI)
}

/// Initializes [`UV_QUALITY`] random phase offsets for a band's unvoiced
/// oscillators.
///
/// Each oscillator in the multisine gets its own random phase so they
/// don't constructively interfere (which would produce a tone instead
/// of noise). The `variant` parameter differentiates between the
/// different synthesis paths (prev-voiced/cur-unvoiced = 0,
/// prev-unvoiced/cur-voiced = 1, both-unvoiced prev = 2, cur = 3).
fn init_random_phases(band: usize, variant: usize) -> [f32; UV_QUALITY] {
    let mut phases = [0.0_f32; UV_QUALITY];
    for i in 0..UV_QUALITY {
        // Combine band, variant, and oscillator index to get a unique
        // deterministic phase for each (band, case, oscillator) triple.
        let seed = band
            .wrapping_mul(67)
            .wrapping_add(variant.wrapping_mul(997))
            .wrapping_add(i.wrapping_mul(31));
        let r = deterministic_rand(seed, i.wrapping_add(variant));
        if let Some(slot) = phases.get_mut(i) {
            *slot = r.mul_add(std::f32::consts::TAU, -std::f32::consts::PI);
        }
    }
    phases
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::MbeParams;

    /// Silence parameters (all zero magnitudes) should produce silence output.
    #[test]
    fn silence_params_produce_silence() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];
        let mut cur = MbeParams::new();
        let mut prev = MbeParams::new();

        // Set up valid but silent parameters.
        cur.l = 12;
        cur.w0 = 0.04;
        prev.l = 12;
        prev.w0 = 0.04;

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        for (n, sample) in pcm.iter().enumerate() {
            assert!(
                *sample == 0.0,
                "sample {n} should be zero for silence, got {sample}"
            );
        }
    }

    /// Deterministic: same input parameters always produce bit-exact
    /// same output samples.
    #[test]
    fn deterministic_output() {
        let make_params = || {
            let mut p = MbeParams::new();
            p.l = 12;
            p.w0 = 0.04;
            for l in 1..=p.l {
                if let Some(slot) = p.ml.get_mut(l) {
                    *slot = 1.0;
                }
                if let Some(slot) = p.vl.get_mut(l) {
                    *slot = l <= 8;
                }
            }
            p
        };

        let mut pcm1 = [0.0_f32; FRAME_SAMPLES];
        let mut cur1 = make_params();
        let mut prev1 = make_params();
        synthesize_speech(&mut pcm1, &mut cur1, &mut prev1);

        let mut pcm2 = [0.0_f32; FRAME_SAMPLES];
        let mut cur2 = make_params();
        let mut prev2 = make_params();
        synthesize_speech(&mut pcm2, &mut cur2, &mut prev2);

        for n in 0..FRAME_SAMPLES {
            let s1 = pcm1.get(n).copied().unwrap_or(f32::NAN);
            let s2 = pcm2.get(n).copied().unwrap_or(f32::NAN);
            assert_eq!(s1.to_bits(), s2.to_bits(), "sample {n}: {s1} vs {s2}");
        }
    }

    /// A single voiced band should produce a cosine-like waveform.
    /// The output should have the right periodicity matching the
    /// fundamental frequency.
    #[test]
    fn single_voiced_band_produces_cosine() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 1;
        cur.w0 = 0.04;
        if let Some(slot) = cur.ml.get_mut(1) {
            *slot = 1.0;
        }
        if let Some(slot) = cur.vl.get_mut(1) {
            *slot = true;
        }

        let mut prev = MbeParams::new();
        prev.l = 1;
        prev.w0 = 0.04;
        if let Some(slot) = prev.ml.get_mut(1) {
            *slot = 1.0;
        }
        if let Some(slot) = prev.vl.get_mut(1) {
            *slot = true;
        }

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        // The output should be non-silent (has energy).
        let energy: f32 = pcm.iter().map(|s| s * s).sum();
        assert!(
            energy > 0.1,
            "single voiced band should produce non-trivial output, energy={energy}"
        );

        // The output should be oscillatory: it should have both positive
        // and negative samples.
        let has_positive = pcm.iter().any(|s| *s > 0.01);
        let has_negative = pcm.iter().any(|s| *s < -0.01);
        assert!(
            has_positive && has_negative,
            "voiced output should oscillate \
             (has_positive={has_positive}, has_negative={has_negative})"
        );
    }

    /// Unvoiced bands should produce noise-like output (non-zero
    /// but without strong periodicity).
    #[test]
    fn unvoiced_band_produces_noise() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 10;
        cur.w0 = 0.03;
        for l in 1..=cur.l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot = 1.0;
            }
            if let Some(slot) = cur.vl.get_mut(l) {
                *slot = false; // All unvoiced.
            }
        }

        let mut prev = MbeParams::new();
        prev.l = 10;
        prev.w0 = 0.03;
        for l in 1..=prev.l {
            if let Some(slot) = prev.ml.get_mut(l) {
                *slot = 1.0;
            }
            if let Some(slot) = prev.vl.get_mut(l) {
                *slot = false;
            }
        }

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        let energy: f32 = pcm.iter().map(|s| s * s).sum();
        assert!(
            energy > 0.1,
            "unvoiced output should have non-trivial energy, got {energy}"
        );
    }

    /// Phase is updated during synthesis (PSI and PHI fields).
    #[test]
    fn phase_updated_during_synthesis() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 5;
        cur.w0 = 0.04;
        for l in 1..=cur.l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot = 1.0;
            }
            if let Some(slot) = cur.vl.get_mut(l) {
                *slot = true;
            }
        }

        let mut prev = MbeParams::new();
        prev.l = 5;
        prev.w0 = 0.04;

        // Phases start at zero.
        let phi_before = cur.phi_l.get(1).copied().unwrap_or(0.0);
        assert!(phi_before == 0.0, "phi should start at zero");

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        // After synthesis, PSI should be updated (non-zero for non-zero w0).
        let psi_after = cur.psi_l.get(1).copied().unwrap_or(0.0);
        assert!(
            psi_after != 0.0,
            "PSI should be updated after synthesis (got {psi_after})"
        );
    }

    /// Band extension: when cur.l > prev.l, prev is extended with
    /// zero magnitudes.
    #[test]
    fn band_extension_cur_longer() {
        let mut pcm = [0.0_f32; FRAME_SAMPLES];

        let mut cur = MbeParams::new();
        cur.l = 15;
        cur.w0 = 0.03;
        for l in 1..=cur.l {
            if let Some(slot) = cur.ml.get_mut(l) {
                *slot = 0.5;
            }
            if let Some(slot) = cur.vl.get_mut(l) {
                *slot = true;
            }
        }

        let mut prev = MbeParams::new();
        prev.l = 10;
        prev.w0 = 0.04;
        for l in 1..=prev.l {
            if let Some(slot) = prev.ml.get_mut(l) {
                *slot = 0.5;
            }
            if let Some(slot) = prev.vl.get_mut(l) {
                *slot = true;
            }
        }

        synthesize_speech(&mut pcm, &mut cur, &mut prev);

        // After synthesis, prev_enh bands 11-15 should be zero (extended).
        for l in 11..=15 {
            let ml = prev.ml.get(l).copied().unwrap_or(f32::NAN);
            assert!(
                ml == 0.0,
                "extended band {l} in prev should be 0.0, got {ml}"
            );
        }
    }

    /// The deterministic PRNG produces values in `[0.0, 1.0)`.
    #[test]
    fn prng_range() {
        for sample in 0..200 {
            for osc in 0..10 {
                let r = deterministic_rand(sample, osc);
                assert!(
                    (0.0..1.0).contains(&r),
                    "rand({sample}, {osc}) = {r} out of [0, 1)"
                );
            }
        }
    }

    /// The deterministic phase PRNG produces values in `[-pi, +pi)`.
    #[test]
    fn rand_phase_range() {
        let pi = std::f32::consts::PI;
        for band in 0..100 {
            let p = deterministic_rand_phase(band);
            assert!(
                (-pi..pi).contains(&p),
                "rand_phase({band}) = {p} out of [-pi, pi)"
            );
        }
    }
}
