// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// See ../LICENSE for full attribution including upstream copyrights from
// szechyjs's mbelib and DSD projects (both originally ISC-licensed,
// redistributed here under GPL-2.0-or-later as permitted by ISC) and
// JMBE-compatible algorithm ports adapted from arancormonk/mbelib-neo
// (also GPL-2.0-or-later).

//! Pure Rust AMBE 3600×2400 voice codec decoder for D-STAR digital radio.
//!
//! The AMBE (Advanced Multi-Band Excitation) 3600×2400 codec compresses
//! speech at 3600 bits/second with 2400 bits of voice data and 1200 bits
//! of forward error correction (FEC). It is the mandatory voice codec
//! for the JARL D-STAR digital radio standard, used in all D-STAR
//! transceivers and reflectors worldwide.
//!
//! Each voice frame is 9 bytes (72 bits), transmitted at 50 frames per
//! second (20 ms per frame). The codec models speech as a sum of
//! harmonically related sinusoids, with each band independently
//! classified as voiced or unvoiced.
//!
//! # Usage
//!
//! ```
//! use mbelib_rs::AmbeDecoder;
//!
//! // Create one decoder per voice stream — it carries inter-frame state
//! // needed for delta decoding and phase-continuous synthesis.
//! let mut decoder = AmbeDecoder::new();
//!
//! // Feed 9-byte AMBE frames from D-STAR VoiceFrame.ambe field.
//! let ambe_frame: [u8; 9] = [0; 9];
//! let pcm: [i16; 160] = decoder.decode_frame(&ambe_frame);
//!
//! // Output: 160 samples at 8 kHz, 16-bit signed PCM (20 ms of audio).
//! assert_eq!(pcm.len(), 160);
//! ```
//!
//! # Decode Pipeline
//!
//! Each frame passes through these stages:
//!
//! 1. **Bit unpacking** — 72-bit frame → 4 FEC codeword bitplanes
//! 2. **Error correction** — Golay(23,12) on C0 and C1 (3-error
//!    correction). AMBE 3600×2400 does not apply Hamming to C3; those
//!    14 bits are copied verbatim into the parameter vector.
//! 3. **Demodulation** — LFSR descrambling of C1 using C0 seed
//! 4. **Parameter extraction** — 49 decoded bits → fundamental frequency,
//!    harmonic count, voiced/unvoiced decisions, spectral magnitudes.
//!    Frames with b0 in the erasure range (120..=123) or tone range
//!    (126..=127) trigger the same repeat/conceal path as Golay-C0
//!    failures, since D-STAR does not use codec-level tone signaling.
//! 5. **Spectral enhancement** — adaptive amplitude weighting for clarity
//! 6. **Adaptive smoothing** — JMBE algorithms #111-116, gracefully
//!    damps spurious magnitudes/voicing decisions on noisy frames
//! 7. **Frame muting check** — comfort noise on excessive errors or
//!    sustained repeat frames (JMBE-compatible)
//! 8. **Synthesis** — voiced bands per-band cosine oscillators (with
//!    JMBE phase/amplitude interpolation for low harmonics) plus a
//!    single FFT-based unvoiced pass (JMBE algorithms #117-126)
//! 9. **Output conversion** — float PCM → i16 with SIMD-vectorized
//!    gain and clamping

mod adaptive;
mod decode;
mod ecc;
#[cfg(feature = "encoder")]
mod encode;
mod enhance;
mod error;
mod math;
mod params;
mod synthesize;
mod tables;
mod unpack;
mod unvoiced_fft;

pub use error::DecodeError;

/// Inspection helper for golden-vector validation.
///
/// Runs just the unpack → ECC → parameter-extract pipeline for a
/// single frame and returns `(b[0..9], w0, L, ambe_d)` as the decoder
/// sees them.  Used by the validation harness in
/// `examples/decode_ambe_stream.rs` to diff against mbelib's decoded
/// `(b, w0, L, ambe_d)` for identical wire bytes.
///
/// The full `ambe_d` vector (49 bits, one byte per bit, 0 or 1) is
/// returned alongside the extracted parameter fields so downstream
/// tooling can localize a divergence to "ECC disagrees" (`ambe_d`
/// bits differ) vs "parameter extraction disagrees" (`ambe_d` bits
/// match but `b[]` differs).
///
/// This is deliberately stateless — each call constructs fresh
/// `MbeParams` — so the output depends only on the input bytes and
/// can be compared frame-for-frame against another implementation.
#[must_use]
pub fn decode_trace(ambe: &[u8; 9]) -> ([usize; 9], f32, usize, [u8; 49]) {
    let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
    let mut ambe_d = [0u8; AMBE_DATA_BITS];
    unpack::unpack_frame(ambe, &mut ambe_fr);
    let _ = ecc::ecc_c0(&mut ambe_fr);
    unpack::demodulate_c1(&mut ambe_fr);
    let _ = ecc::ecc_data(&ambe_fr, &mut ambe_d);

    let bit = |i: usize| usize::from(*ambe_d.get(i).unwrap_or(&0));
    let b0 = (bit(0) << 6)
        | (bit(1) << 5)
        | (bit(2) << 4)
        | (bit(3) << 3)
        | (bit(4) << 2)
        | (bit(5) << 1)
        | bit(48);
    let b1 = (bit(38) << 3) | (bit(39) << 2) | (bit(40) << 1) | bit(41);
    let b2 =
        (bit(6) << 5) | (bit(7) << 4) | (bit(8) << 3) | (bit(9) << 2) | (bit(42) << 1) | bit(43);
    let b3 = (bit(10) << 8)
        | (bit(11) << 7)
        | (bit(12) << 6)
        | (bit(13) << 5)
        | (bit(14) << 4)
        | (bit(15) << 3)
        | (bit(16) << 2)
        | (bit(44) << 1)
        | bit(45);
    let b4 = (bit(17) << 6)
        | (bit(18) << 5)
        | (bit(19) << 4)
        | (bit(20) << 3)
        | (bit(21) << 2)
        | (bit(46) << 1)
        | bit(47);
    let b5 = (bit(22) << 3) | (bit(23) << 2) | (bit(25) << 1) | bit(26);
    let b6 = (bit(27) << 3) | (bit(28) << 2) | (bit(29) << 1) | bit(30);
    let b7 = (bit(31) << 3) | (bit(32) << 2) | (bit(33) << 1) | bit(34);
    let b8 = (bit(35) << 3) | (bit(36) << 2) | (bit(37) << 1);

    let b = [b0, b1, b2, b3, b4, b5, b6, b7, b8];
    let f0 = *tables::W0_TABLE.get(b0).unwrap_or(&0.0);
    let w0 = f0 * std::f32::consts::TAU;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let big_l = *tables::L_TABLE.get(b0).unwrap_or(&0.0) as usize;
    (b, w0, big_l, ambe_d)
}

#[cfg(feature = "encoder")]
pub use encode::{
    AmbeEncoder, EncoderBuffers, FftPlan, MAX_BANDS, MAX_HARMONICS, PitchEstimate, PitchTracker,
    SpectralAmplitudes, VuvDecisions, VuvState, analyze_frame, compute_e_p, detect_vuv,
    detect_vuv_and_sa, extract_spectral_amplitudes, pack_frame, validation,
};

/// Kenwood-specific constants for A/B testing the encoder, gated
/// behind the `kenwood-tables` feature.
///
/// The encoder pipeline does NOT consume these by default — the
/// module is a catalogue, not a swap. Swap points are introduced
/// deliberately, one at a time, with each change measurable against
/// hardware-in-the-loop captures.
#[cfg(feature = "kenwood-tables")]
pub use encode::kenwood;

use ecc::AMBE_DATA_BITS;
use params::MbeParams;
use synthesize::FRAME_SAMPLES;
use unpack::AMBE_FRAME_BITS;
use wide::{f32x4, i32x4};

/// Output audio gain applied during float-to-i16 conversion.
const GAIN: f32 = 7.0;

/// Maximum absolute sample value after gain (clamp threshold). Matches
/// mbelib-neo's JMBE-parity soft-clip at 95% of i16 max.
const CLAMP_MAX: f32 = 32_767.0 * 0.95;

/// Total bits per AMBE 3600x2400 frame (used to compute error rate).
const FRAME_BITS: f32 = 72.0;

/// Stateful AMBE 3600×2400 voice frame decoder.
///
/// The AMBE codec uses inter-frame prediction: each frame's gain and
/// spectral magnitudes are delta-coded against the previous frame.
/// This decoder maintains three parameter snapshots to support that:
///
/// - **`cur`** — parameters decoded from the current frame
/// - **`prev`** — previous frame's parameters (before enhancement),
///   used as the prediction reference for delta decoding
/// - **`prev_enhanced`** — previous frame's parameters (after spectral
///   enhancement), used as the cross-fade source during synthesis
///
/// # Invariants
///
/// - Create one `AmbeDecoder` per voice stream (per D-STAR `StreamId`).
/// - Feed frames sequentially in receive order.
/// - Discard the decoder when the stream ends (`VoiceEnd` event).
/// - The decoder is deterministic: same input sequence always produces
///   the same output.
#[derive(Debug, Clone)]
pub struct AmbeDecoder {
    /// Parameters decoded from the current frame.
    cur: MbeParams,
    /// Previous frame's raw parameters (prediction reference for delta
    /// decoding of gain and spectral magnitudes).
    prev: MbeParams,
    /// Previous frame's enhanced parameters (cross-fade source during
    /// harmonic synthesis, ensuring smooth transitions between frames).
    prev_enhanced: MbeParams,
    /// Per-stream RNG state for comfort noise output during muting.
    comfort_noise_state: u64,
}

impl AmbeDecoder {
    /// Creates a new decoder with zeroed initial state.
    ///
    /// The first decoded frame will use silence as its prediction
    /// reference, which may produce a brief transient. This matches
    /// the behavior of hardware DVSI vocoders.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cur: MbeParams::new(),
            prev: MbeParams::new(),
            prev_enhanced: MbeParams::new(),
            comfort_noise_state: adaptive::COMFORT_NOISE_INIT_SEED,
        }
    }

    /// Decodes a single 9-byte AMBE frame into 160 PCM samples.
    ///
    /// Returns 160 signed 16-bit samples at 8000 Hz (20 ms of audio).
    /// A gain factor of 7.0 is applied and samples are clamped to
    /// `±32767 × 0.95` to match JMBE soft-clipping semantics.
    ///
    /// If the frame contains excessive bit errors (more than the FEC
    /// can correct) or the decoder has hit the maximum repeat count,
    /// comfort noise is output instead of synthesized speech.
    #[must_use]
    pub fn decode_frame(&mut self, ambe: &[u8; 9]) -> [i16; FRAME_SAMPLES] {
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        let mut ambe_d = [0u8; AMBE_DATA_BITS];

        // Unpack + ECC + demod pipeline.
        unpack::unpack_frame(ambe, &mut ambe_fr);
        let c0_errors = ecc::ecc_c0(&mut ambe_fr);
        unpack::demodulate_c1(&mut ambe_fr);
        let other_errors = ecc::ecc_data(&ambe_fr, &mut ambe_d);

        // Error concealment: three conditions trigger "reuse previous
        // frame's parameters + increment repeat_count" for the current
        // frame. `repeat_count` accumulates across consecutive bad
        // frames; sustained failures (≥3) trigger muting downstream.
        //
        // 1. C0-uncorrectable (Golay(23,12) exceeded its 3-error
        //    correction capacity). b0 and the other C0 data bits are
        //    untrustworthy, so decode_params shouldn't even run.
        // 2. decode_params returned `Erasure`: b0 in 120..=123 is the
        //    AMBE codec's explicit "unrecoverable frame" signal.
        // 3. decode_params returned `Tone`: b0 in 126..=127 signals a
        //    codec-level tone. D-STAR doesn't use codec tones (DTMF
        //    goes over slow-data), so we treat this as erasure too.
        let mut reuse_prev = c0_errors > adaptive::GOLAY_C0_CAPACITY;
        if !reuse_prev {
            reuse_prev = decode::decode_params(&ambe_d, &mut self.cur, &self.prev)
                != decode::FrameStatus::Voice;
        }

        if reuse_prev {
            let prev_repeat = self.prev_enhanced.repeat_count;
            self.cur.copy_from(&self.prev_enhanced);
            self.cur.repeat_count = prev_repeat + 1;
        } else {
            self.cur.repeat_count = 0;
        }

        // Update error tracking for adaptive smoothing and muting.
        // AMBE 3600x2400 has 72 raw bits.
        #[expect(
            clippy::cast_possible_wrap,
            reason = "error counts are at most a few dozen; fit in i32"
        )]
        {
            self.cur.error_count_total = (c0_errors + other_errors) as i32;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "error counts are at most a few dozen; no precision loss in f32"
        )]
        {
            self.cur.error_rate = self.cur.error_count_total as f32 / FRAME_BITS;
        }

        // Snapshot raw parameters as prediction reference for next frame.
        self.prev.copy_from(&self.cur);

        // Compute pre-enhancement RM0 (algorithm #111 input).
        let pre_enhance_rm0 = (1..=self.cur.l)
            .map(|l| {
                let m = self.cur.ml.get(l).copied().unwrap_or(0.0);
                m * m
            })
            .sum::<f32>();

        // Spectral amplitude enhancement.
        enhance::spectral_amp_enhance(&mut self.cur);

        // Adaptive smoothing (JMBE algorithms #111-116).
        adaptive::apply_adaptive_smoothing(
            &mut self.cur,
            &self.prev_enhanced,
            Some(pre_enhance_rm0),
        );

        // Muting: output comfort noise instead of synthesized speech
        // when the FEC-reported error rate exceeds the AMBE threshold.
        // Preserves model state for next-frame recovery.
        let muted = adaptive::requires_muting(&self.cur);

        let mut pcm_f = [0.0_f32; FRAME_SAMPLES];
        if muted {
            adaptive::synthesize_comfort_noise(&mut pcm_f, &mut self.comfort_noise_state);
        } else {
            synthesize::synthesize_speech(&mut pcm_f, &mut self.cur, &mut self.prev_enhanced);
        }

        // Save enhanced parameters as cross-fade source for next frame.
        self.prev_enhanced.copy_from(&self.cur);

        float_to_i16(&pcm_f)
    }
}

impl Default for AmbeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Converts 160 float PCM samples to 16-bit signed integers using
/// SIMD-vectorized gain + clamp + round.
///
/// Processes 4 samples per loop iteration via `wide::f32x4`. The
/// `round_int` step uses round-to-nearest-even (vs the C reference's
/// truncation), which produces marginally better fidelity at the cost
/// of being one ulp different on samples exactly on a half-integer.
fn float_to_i16(input: &[f32; FRAME_SAMPLES]) -> [i16; FRAME_SAMPLES] {
    let mut output = [0_i16; FRAME_SAMPLES];

    let gain_v = f32x4::splat(GAIN);
    let max_v = f32x4::splat(CLAMP_MAX);
    let min_v = f32x4::splat(-CLAMP_MAX);

    // FRAME_SAMPLES (160) is divisible by 4, no scalar tail needed.
    let mut i = 0;
    while i + 4 <= FRAME_SAMPLES {
        let chunk = f32x4::new([
            input.get(i).copied().unwrap_or(0.0),
            input.get(i + 1).copied().unwrap_or(0.0),
            input.get(i + 2).copied().unwrap_or(0.0),
            input.get(i + 3).copied().unwrap_or(0.0),
        ]);
        let scaled = chunk * gain_v;
        let clamped = scaled.fast_min(max_v).fast_max(min_v);
        let rounded: i32x4 = clamped.round_int();
        let arr: [i32; 4] = rounded.into();
        for (j, &v) in arr.iter().enumerate() {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "v is in i16 range due to clamp above"
            )]
            if let Some(slot) = output.get_mut(i + j) {
                *slot = v as i16;
            }
        }
        i += 4;
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Float→i16 produces results bit-identical (or within 1 ULP) to
    /// the scalar reference implementation.
    #[test]
    fn float_to_i16_matches_scalar() {
        let mut input = [0.0_f32; FRAME_SAMPLES];
        for (i, slot) in input.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "i is at most 159; no precision loss"
            )]
            {
                *slot = ((i as f32 / 80.0) - 1.0) * 5000.0;
            }
        }

        let simd_out = float_to_i16(&input);

        for (n, &got) in simd_out.iter().enumerate() {
            let expected = (input[n] * GAIN).clamp(-CLAMP_MAX, CLAMP_MAX).round();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "expected is in i16 range due to clamp"
            )]
            let expected_i16 = expected as i16;
            let diff = (i32::from(got) - i32::from(expected_i16)).abs();
            assert!(
                diff <= 1,
                "sample {n}: got {got}, expected {expected_i16} (input={})",
                input[n]
            );
        }
    }

    /// Float→i16 properly clamps values outside the valid range.
    #[test]
    fn float_to_i16_clamps_extremes() {
        let mut input = [0.0_f32; FRAME_SAMPLES];
        input[0] = 1_000_000.0;
        input[1] = -1_000_000.0;
        input[2] = 0.0;
        input[3] = -0.0;

        let out = float_to_i16(&input);
        // CLAMP_MAX is 31128.65, so clamped × 7 then round → ≤ 31129.
        assert!(
            (31_125..=31_130).contains(&out[0]),
            "max should clamp near 31128, got {}",
            out[0]
        );
        assert!(
            (-31_130..=-31_125).contains(&out[1]),
            "min should clamp near -31128, got {}",
            out[1]
        );
        assert_eq!(out[2], 0);
        assert_eq!(out[3], 0);
    }
}
