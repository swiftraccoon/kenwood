// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// See ../LICENSE for full attribution including upstream copyrights from
// szechyjs's mbelib and DSD projects (both originally ISC-licensed,
// redistributed here under GPL-2.0-or-later as permitted by ISC) and
// JMBE-compatible algorithm ports adapted from arancormonk/mbelib-neo
// (also GPL-2.0-or-later).

#![doc = include_str!("../README.md")]
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
//! // Create one decoder per voice stream; it carries inter-frame state
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
//! 1. **Bit unpacking**: 72-bit frame → 4 FEC codeword bitplanes
//! 2. **Error correction**: Golay(23,12) on C0 and C1 (3-error
//!    correction). AMBE 3600×2400 does not apply Hamming to C3; those
//!    14 bits are copied verbatim into the parameter vector.
//! 3. **Demodulation**: LFSR descrambling of C1 using C0 seed
//! 4. **Parameter extraction**: 49 decoded bits → fundamental frequency,
//!    harmonic count, voiced/unvoiced decisions, spectral magnitudes.
//!    Disposition then splits three ways: valid tone frames (b0 in
//!    126..=127) are synthesized directly by a dedicated tone oscillator;
//!    erasure frames (b0 in 120..=123) and out-of-range tone descriptors
//!    emit silence and fully re-initialize the decoder state; voice frames
//!    whose FEC required more than 3 corrected bits reuse the previous
//!    frame's parameters and increment the repeat counter.
//! 5. **Spectral enhancement**: adaptive amplitude weighting for clarity
//! 6. **Adaptive smoothing**: JMBE algorithms #111-116, gracefully
//!    damps spurious magnitudes/voicing decisions on noisy frames
//! 7. **Frame muting check**: comfort noise on excessive errors or
//!    sustained repeat frames (JMBE-compatible)
//! 8. **Synthesis**: voiced bands per-band cosine oscillators (with
//!    JMBE phase/amplitude interpolation for low harmonics) plus a
//!    single FFT-based unvoiced pass (JMBE algorithms #117-126)
//! 9. **Output conversion**: float PCM → i16 with SIMD-vectorized
//!    gain and clamping

// Dev-dependency `proptest` is used only inside the `encoder` feature module
// (`src/encode/pack.rs`). Acknowledge it at the lib level so
// `unused_crate_dependencies` stays silent in the default-features build.
#[cfg(all(test, not(feature = "encoder")))]
use proptest as _;

mod adaptive;
mod decode;
mod ecc;
#[cfg(feature = "encoder")]
mod encode;
mod enhance;
#[cfg(feature = "wave-enhance")]
pub mod enhance_live;
#[cfg(feature = "wave-enhance")]
pub mod enhance_wave;
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
/// This is deliberately stateless (each call constructs fresh
/// `MbeParams`), so the output depends only on the input bytes and
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
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "L_TABLE entries are small positive integers stored as f32 (harmonic count \
                  1..=56). The f32-to-usize cast is exact by construction; truncation and \
                  sign loss cannot occur within the defined table range."
    )]
    let big_l = *tables::L_TABLE.get(b0).unwrap_or(&0.0) as usize;
    (b, w0, big_l, ambe_d)
}

/// Classification of a decoded AMBE frame, derived from the b0 field.
///
/// Mirrors the decoder's internal frame disposition exactly: tone
/// frames are `(b0 & 0x7E) == 0x7E` (b0 ∈ {126, 127}), erasure
/// frames are `b0 ∈ 120..=123`, and everything else, including the
/// b0 ∈ {124, 125} silence encodings, decodes as voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Speech-model frame (includes explicit silence encodings).
    Voice,
    /// Encoder-signalled unrecoverable frame.
    Erasure,
    /// Tone-signalling frame (DVSI encoders emit these for pure tones).
    Tone,
}

/// Per-frame FEC summary for archival and quality metadata.
///
/// Produced by [`frame_fec`]. The counts depend only on the input
/// wire bytes (no decoder state), so archived frames re-derive the
/// same numbers forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameFec {
    /// Golay(24,12) corrections applied to the C0 codeword.
    pub c0_errors: u32,
    /// Total corrected bits across all FEC-protected codewords
    /// (the analog of mbelib's `errs2`).
    pub total_errors: u32,
    /// Frame classification decoded from b0.
    pub kind: FrameKind,
}

/// Classify a decoded b0 value into a [`FrameKind`].
pub(crate) const fn classify_b0(b0: usize) -> FrameKind {
    if (b0 & 0x7E) == 0x7E {
        FrameKind::Tone
    } else if matches!(b0, 120..=123) {
        FrameKind::Erasure
    } else {
        FrameKind::Voice
    }
}

/// Compute the per-frame FEC error summary for one 9-byte AMBE frame.
///
/// Runs the same stateless unpack → ECC pipeline as [`decode_trace`]
/// but returns the correction counts that both [`decode_trace`] and
/// [`AmbeDecoder::decode_frame`] discard. Intended for recorders and
/// dataset tooling that need a signal-quality index per frame.
#[must_use]
pub fn frame_fec(ambe: &[u8; 9]) -> FrameFec {
    let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
    let mut ambe_d = [0u8; AMBE_DATA_BITS];
    unpack::unpack_frame(ambe, &mut ambe_fr);
    let c0_errors = ecc::ecc_c0(&mut ambe_fr);
    unpack::demodulate_c1(&mut ambe_fr);
    let data_errors = ecc::ecc_data(&ambe_fr, &mut ambe_d);

    let bit = |i: usize| usize::from(*ambe_d.get(i).unwrap_or(&0));
    let b0 = (bit(0) << 6)
        | (bit(1) << 5)
        | (bit(2) << 4)
        | (bit(3) << 3)
        | (bit(4) << 2)
        | (bit(5) << 1)
        | bit(48);

    FrameFec {
        c0_errors,
        total_errors: c0_errors + data_errors,
        kind: classify_b0(b0),
    }
}

/// Number of harmonic bands exposed per frame by [`FrameParams`].
///
/// The AMBE 3600×2400 codec produces 9..=56 bands depending on the
/// fundamental; slots past [`FrameParams::harmonics`] are padding.
pub const PARAM_BANDS: usize = 56;

/// Per-frame vocoder parameters, extracted without synthesis.
///
/// This is the harmonic speech model the codec transmits: the same
/// parameter family (fundamental, per-band voicing, spectral
/// amplitudes) that vocoder-parameter ASR consumes directly instead
/// of reconstructed audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameParams {
    /// Fundamental frequency in Hz (8 kHz sample-rate domain).
    /// Zero for erasure/tone frames.
    pub f0_hz: f32,
    /// Harmonic band count L (9..=56 for voice frames, else 0).
    pub harmonics: usize,
    /// Frame classification (voice / erasure / tone).
    pub kind: FrameKind,
    /// Voiced decision per band; index `i` is band `i + 1`, valid
    /// for `i < harmonics`, padding `false` beyond.
    pub voiced: [bool; PARAM_BANDS],
    /// Linear spectral magnitude per band, same indexing, padding 0.
    pub amplitudes: [f32; PARAM_BANDS],
    /// Total FEC-corrected bits in this frame.
    pub fec_errors: u32,
    /// True when the frame's parameters were repeated from the
    /// previous frame (untrustworthy FEC or concealment), mirroring
    /// the decoder's repeat disposition.
    pub repeated: bool,
}

impl FrameParams {
    const fn empty(kind: FrameKind, fec_errors: u32) -> Self {
        Self {
            f0_hz: 0.0,
            harmonics: 0,
            kind,
            voiced: [false; PARAM_BANDS],
            amplitudes: [0.0; PARAM_BANDS],
            fec_errors,
            repeated: false,
        }
    }
}

/// Stateful vocoder-parameter extractor: the decoder's parameter
/// track without audio synthesis.
///
/// AMBE delta-codes gain and predicts magnitudes from the previous
/// frame, so extraction is sequential: feed frames in stream order,
/// one extractor per stream, exactly like [`AmbeDecoder`]. The
/// disposition rules mirror the decoder: untrustworthy frames
/// (more than 3 corrected bits) repeat the previous parameters,
/// sustained repeats reset the model, and erasure/tone frames reset
/// it immediately.
#[derive(Debug, Clone)]
pub struct AmbeParamExtractor {
    cur: MbeParams,
    prev: MbeParams,
}

impl Default for AmbeParamExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl AmbeParamExtractor {
    /// Creates an extractor with zeroed initial state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            cur: MbeParams::new(),
            prev: MbeParams::new(),
        }
    }

    /// Extracts one frame's parameters from 9 AMBE wire bytes.
    pub fn extract(&mut self, ambe: &[u8; 9]) -> FrameParams {
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        let mut ambe_d = [0u8; AMBE_DATA_BITS];
        unpack::unpack_frame(ambe, &mut ambe_fr);
        let c0_errors = ecc::ecc_c0(&mut ambe_fr);
        unpack::demodulate_c1(&mut ambe_fr);
        let other_errors = ecc::ecc_data(&ambe_fr, &mut ambe_d);
        let errs2 = c0_errors + other_errors;

        let status = decode::decode_params(&ambe_d, &mut self.cur, &self.prev);
        match status {
            decode::FrameStatus::Voice => {}
            decode::FrameStatus::Tone { .. } => {
                *self = Self::new();
                return FrameParams::empty(FrameKind::Tone, errs2);
            }
            decode::FrameStatus::Erasure => {
                *self = Self::new();
                return FrameParams::empty(FrameKind::Erasure, errs2);
            }
        }

        let repeated = if errs2 > adaptive::MAX_CORRECTED_BITS {
            let prev_repeat = self.prev.repeat_count;
            self.cur.copy_from(&self.prev);
            self.cur.repeat_count = prev_repeat + 1;
            true
        } else {
            self.cur.repeat_count = 0;
            false
        };
        if self.cur.repeat_count > adaptive::MAX_FRAME_REPEATS {
            *self = Self::new();
            let mut out = FrameParams::empty(FrameKind::Voice, errs2);
            out.repeated = true;
            return out;
        }

        let out = self.snapshot(errs2, repeated);
        self.prev.copy_from(&self.cur);
        out
    }

    /// Emits parameters for a frame known to be missing (sequence
    /// gap): the previous frame's parameters, marked `repeated`,
    /// with the decoder's sustained-loss reset semantics.
    pub fn conceal(&mut self) -> FrameParams {
        let prev_repeat = self.prev.repeat_count;
        self.cur.copy_from(&self.prev);
        self.cur.repeat_count = prev_repeat + 1;
        if self.cur.repeat_count > adaptive::MAX_FRAME_REPEATS {
            *self = Self::new();
            let mut out = FrameParams::empty(FrameKind::Voice, 0);
            out.repeated = true;
            return out;
        }
        let out = self.snapshot(0, true);
        self.prev.copy_from(&self.cur);
        out
    }

    fn snapshot(&self, fec_errors: u32, repeated: bool) -> FrameParams {
        let mut out = FrameParams::empty(FrameKind::Voice, fec_errors);
        out.repeated = repeated;
        out.f0_hz = self.cur.w0 / std::f32::consts::TAU * 8000.0;
        out.harmonics = self.cur.l.min(PARAM_BANDS);
        for band in 1..=out.harmonics {
            if let (Some(v), Some(a)) = (
                out.voiced.get_mut(band - 1),
                out.amplitudes.get_mut(band - 1),
            ) {
                *v = self.cur.vl.get(band).copied().unwrap_or(false);
                *a = self.cur.ml.get(band).copied().unwrap_or(0.0);
            }
        }
        out
    }
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
/// The encoder pipeline does NOT consume these by default; the
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
/// - **`cur`**: parameters decoded from the current frame
/// - **`prev`**: previous frame's parameters (before enhancement),
///   used as the prediction reference for delta decoding
/// - **`prev_enhanced`**: previous frame's parameters (after spectral
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
    /// Running oscillator phase for tone-frame synthesis, in radians.
    /// Carried across consecutive tone frames so the sine is
    /// continuous at frame boundaries.
    tone_phase: f32,
    /// Synthesis tuning; [`SynthesisTuning::PARITY`] by default.
    tuning: SynthesisTuning,
}

/// Runtime tuning for the synthesis-side stages of the decoder.
///
/// Every field's default is the exact constant used by mbelib/JMBE, so
/// [`SynthesisTuning::default()`] (== [`SynthesisTuning::PARITY`])
/// reproduces the untuned decoder bit for bit. Non-default values move
/// the synthesis away from mbelib parity toward configurations scored
/// against reference-decoded audio of the same transmissions; they
/// never change bitstream interpretation, FEC handling, or frame
/// disposition; they change only how already-decoded parameters are
/// rendered.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SynthesisTuning {
    /// Spectral-enhancement sharpening factor (mbelib parity: `0.96`).
    /// Larger values weight loud harmonics harder relative to the
    /// spectral floor.
    pub enhance_alpha: f32,
    /// Spectral-enhancement weight exponent (mbelib parity: `0.25`).
    /// Controls how aggressively the density ratio reshapes bands.
    pub enhance_exponent: f32,
    /// Lower clamp on the per-band enhancement weight (parity: `0.5`).
    pub enhance_clamp_lo: f32,
    /// Upper clamp on the per-band enhancement weight (parity: `1.2`).
    pub enhance_clamp_hi: f32,
    /// Multiplier on the unvoiced excitation level (parity: `1.0`).
    pub unvoiced_gain: f32,
    /// Multiplier on the voiced phase jitter (parity: `1.0`).
    /// `0.0` accumulates fully deterministic harmonic phases:
    /// cleaner, at the risk of a more mechanical timbre.
    pub phase_jitter: f32,
}

impl SynthesisTuning {
    /// The exact mbelib/JMBE constants. Decoding with this tuning is
    /// bit-identical to [`AmbeDecoder::new`].
    pub const PARITY: Self = Self {
        enhance_alpha: 0.96,
        enhance_exponent: 0.25,
        enhance_clamp_lo: 0.5,
        enhance_clamp_hi: 1.2,
        unvoiced_gain: 1.0,
        phase_jitter: 1.0,
    };
}

impl Default for SynthesisTuning {
    fn default() -> Self {
        Self::PARITY
    }
}

impl AmbeDecoder {
    /// Creates a new decoder with zeroed initial state.
    ///
    /// The first decoded frame will use silence as its prediction
    /// reference, which may produce a brief transient. This matches
    /// the behavior of hardware DVSI vocoders.
    #[must_use]
    pub const fn new() -> Self {
        Self::with_tuning(SynthesisTuning::PARITY)
    }

    /// Creates a decoder with explicit synthesis tuning.
    ///
    /// [`SynthesisTuning::PARITY`] reproduces [`AmbeDecoder::new`]
    /// exactly; other values reshape enhancement and excitation only.
    #[must_use]
    pub const fn with_tuning(tuning: SynthesisTuning) -> Self {
        Self {
            cur: MbeParams::new(),
            prev: MbeParams::new(),
            prev_enhanced: MbeParams::new(),
            comfort_noise_state: adaptive::COMFORT_NOISE_INIT_SEED,
            tone_phase: 0.0,
            tuning,
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

        // Frame disposition, porting mbelib's `mbe_processAmbe2400Dataf`
        // (`mbelib/ambe3600x2400.c:655-713`) exactly:
        //
        // 1. Erasure (b0 in 120..=123), invalid single-tone index, or
        //    unsupported dual tone: output silence and RE-INITIALIZE the
        //    decoder state. Valid single-tone descriptors are synthesized
        //    directly in the match below.
        // 2. More than 3 total FEC-corrected bits (`errs2 > 3`): the
        //    corrections nominally succeeded, but Golay mis-corrects
        //    silently at these error densities, so the decoded
        //    parameters can't be trusted. Reuse the previous frame's
        //    RAW parameters (`mbe_useLastMbeParms` copies `prev_mp`,
        //    not the enhanced snapshot) and increment the repeat
        //    counter. Real-world reflector uplinks sit in this zone
        //    for a large fraction of frames; decoding them fresh is
        //    what turned noisy-but-intelligible speech into garble.
        // 3. More than 3 consecutive repeats: mute (comfort noise
        //    downstream) and re-initialize, per the reference's
        //    `cur_mp->repeat <= 3` gate.
        let errs2 = c0_errors + other_errors;
        let status = decode::decode_params(&ambe_d, &mut self.cur, &self.prev);
        match status {
            decode::FrameStatus::Voice => {}
            decode::FrameStatus::Tone { index, volume } if (5..=122).contains(&index) => {
                // Single tone at index × 31.25 Hz. No open reference
                // implements tone synthesis (mbelib decodes the
                // descriptor for diagnostics and outputs silence), but
                // real DVSI decoders render the tone, and hardware
                // encoders emit tone frames for any pure-tone input,
                // so muting them fails legitimate captures. The
                // amplitude mapping (`volume × 32`) is empirical:
                // volume ≈ 172 (a TH-D75 fed a strong test tone)
                // lands at speech-typical peaks (~5500).
                return self.synthesize_tone(index, volume);
            }
            _ => {
                // Erasure, invalid tone index, or dual tone (the
                // D-STAR dual-tone table is undocumented): silence +
                // full state reset, mirroring the reference's tone/
                // erasure disposition.
                *self = Self::new();
                return [0_i16; FRAME_SAMPLES];
            }
        }
        if errs2 > adaptive::MAX_CORRECTED_BITS {
            let prev_repeat = self.prev.repeat_count;
            self.cur.copy_from(&self.prev);
            self.cur.repeat_count = prev_repeat + 1;
        } else {
            self.cur.repeat_count = 0;
        }
        if self.cur.repeat_count > adaptive::MAX_FRAME_REPEATS {
            // Sustained repeats: comfort noise + full state reset so
            // the next good frame starts from a clean prediction.
            let mut pcm_f = [0.0_f32; FRAME_SAMPLES];
            adaptive::synthesize_comfort_noise(&mut pcm_f, &mut self.comfort_noise_state);
            let noise_state = self.comfort_noise_state;
            *self = Self::new();
            self.comfort_noise_state = noise_state;
            return float_to_i16(&pcm_f);
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

        self.synthesize_current()
    }

    /// Synthesizes one 20 ms concealment frame for a frame known to
    /// be missing (e.g. a UDP sequence gap on a network transport).
    ///
    /// Runs the same repeat path as an uncorrectable or wire-erasure
    /// frame: the previous frame's enhanced parameters are reused and
    /// the repeat counter increments, so sustained loss degrades to
    /// comfort noise exactly like sustained RF errors (JMBE-
    /// compatible). Call once per missing frame, in stream order,
    /// interleaved with [`Self::decode_frame`] calls for the frames
    /// that did arrive.
    #[must_use]
    pub fn conceal_frame(&mut self) -> [i16; FRAME_SAMPLES] {
        // Same repeat semantics as an untrustworthy wire frame: copy
        // the previous frame's RAW parameters (the reference's
        // `mbe_useLastMbeParms` copies `prev_mp`, not the enhanced
        // snapshot; repeats must not re-enhance already-enhanced
        // spectra) and increment the repeat counter.
        let prev_repeat = self.prev.repeat_count;
        self.cur.copy_from(&self.prev);
        self.cur.repeat_count = prev_repeat + 1;
        if self.cur.repeat_count > adaptive::MAX_FRAME_REPEATS {
            // Sustained loss: comfort noise + full state reset, the
            // same degradation as sustained wire errors.
            let mut pcm_f = [0.0_f32; FRAME_SAMPLES];
            adaptive::synthesize_comfort_noise(&mut pcm_f, &mut self.comfort_noise_state);
            let noise_state = self.comfort_noise_state;
            *self = Self::new();
            self.comfort_noise_state = noise_state;
            return float_to_i16(&pcm_f);
        }
        // No bits arrived, so there are no FEC error statistics.
        self.cur.error_count_total = 0;
        self.cur.error_rate = 0.0;
        self.synthesize_current()
    }

    /// Synthesizes one 20 ms single-tone frame: a phase-continuous
    /// sine at `index × 31.25 Hz`, amplitude `volume × 32`.
    ///
    /// The speech-model state resets afterwards (the next voice frame
    /// predicts from defaults, as after any non-voice frame) while the
    /// oscillator phase persists so back-to-back tone frames join
    /// without a discontinuity.
    fn synthesize_tone(&mut self, index: u8, volume: u8) -> [i16; FRAME_SAMPLES] {
        let step = core::f32::consts::TAU * f32::from(index) * 31.25 / 8000.0;
        let amp = f32::from(volume) * 32.0;
        let mut pcm = [0_i16; FRAME_SAMPLES];
        for slot in &mut pcm {
            self.tone_phase = (self.tone_phase + step) % core::f32::consts::TAU;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "amp ≤ 255 × 32 = 8160, well inside i16 range"
            )]
            {
                *slot = (self.tone_phase.sin() * amp) as i16;
            }
        }
        let phase = self.tone_phase;
        let noise_state = self.comfort_noise_state;
        *self = Self::new();
        self.tone_phase = phase;
        self.comfort_noise_state = noise_state;
        pcm
    }

    /// Runs the post-parameter half of the decode pipeline on
    /// `self.cur`: prediction-reference snapshot, spectral
    /// enhancement, adaptive smoothing, the muting decision, speech
    /// (or comfort-noise) synthesis, and the enhanced-parameter
    /// snapshot for the next frame's cross-fade.
    fn synthesize_current(&mut self) -> [i16; FRAME_SAMPLES] {
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
        enhance::spectral_amp_enhance(&mut self.cur, &self.tuning);

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
            synthesize::synthesize_speech(
                &mut pcm_f,
                &mut self.cur,
                &mut self.prev_enhanced,
                &self.tuning,
            );
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

        for (n, (&got, &inp)) in simd_out.iter().zip(input.iter()).enumerate() {
            let expected = (inp * GAIN).clamp(-CLAMP_MAX, CLAMP_MAX).round();
            #[expect(
                clippy::cast_possible_truncation,
                reason = "expected is in i16 range due to clamp"
            )]
            let expected_i16 = expected as i16;
            let diff = (i32::from(got) - i32::from(expected_i16)).abs();
            assert!(
                diff <= 1,
                "sample {n}: got {got}, expected {expected_i16} (input={inp})"
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

#[cfg(test)]
mod param_extractor_tests {
    use super::*;

    const SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

    #[test]
    fn silence_frame_yields_valid_voice_params() {
        let mut ex = AmbeParamExtractor::new();
        let p = ex.extract(&SILENCE);
        assert_eq!(p.kind, FrameKind::Voice);
        assert!(!p.repeated);
        assert_eq!(p.fec_errors, 0);
        assert!(
            (9..=PARAM_BANDS).contains(&p.harmonics),
            "harmonics {} out of codec range",
            p.harmonics
        );
        assert!(
            p.f0_hz > 0.0 && p.f0_hz < 500.0,
            "f0 {} implausible",
            p.f0_hz
        );
        // Padding beyond L stays inert.
        for i in p.harmonics..PARAM_BANDS {
            assert_eq!(p.voiced.get(i), Some(&false));
            assert_eq!(p.amplitudes.get(i), Some(&0.0));
        }
    }

    #[test]
    fn extraction_is_deterministic_per_stream() {
        let mut a = AmbeParamExtractor::new();
        let mut b = AmbeParamExtractor::new();
        for _ in 0..3 {
            assert_eq!(a.extract(&SILENCE), b.extract(&SILENCE));
        }
    }

    #[test]
    fn conceal_repeats_previous_parameters() {
        let mut ex = AmbeParamExtractor::new();
        let voice = ex.extract(&SILENCE);
        let gap = ex.conceal();
        assert!(gap.repeated);
        assert_eq!(gap.kind, FrameKind::Voice);
        assert!((gap.f0_hz - voice.f0_hz).abs() < f32::EPSILON);
        assert_eq!(gap.harmonics, voice.harmonics);
    }

    #[test]
    fn sustained_concealment_resets_like_the_decoder() {
        let mut ex = AmbeParamExtractor::new();
        let _voice = ex.extract(&SILENCE);
        // MAX_FRAME_REPEATS is 3: the 4th consecutive conceal resets.
        for _ in 0..3 {
            let p = ex.conceal();
            assert!(p.repeated);
            assert!(p.harmonics > 0, "repeats keep the model alive");
        }
        let reset = ex.conceal();
        assert!(reset.repeated);
        assert_eq!(reset.harmonics, 0, "sustained loss resets the model");
    }

    #[test]
    fn kind_agrees_with_frame_fec() {
        for frame in [[0u8; 9], SILENCE, [0xFF; 9], [0xA5; 9]] {
            let mut ex = AmbeParamExtractor::new();
            assert_eq!(
                ex.extract(&frame).kind,
                frame_fec(&frame).kind,
                "{frame:02X?}"
            );
        }
    }
}

#[cfg(test)]
mod frame_fec_tests {
    use super::*;

    /// The D-STAR AMBE silence frame (same bytes as
    /// `DSTAR_AMBE_NULL` in g4klx/MMDVMHost `DStarDefines.h`).
    const SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

    /// An all-zero frame is NOT FEC-clean end to end: C0 is the valid
    /// zero Golay codeword (0 corrections), but the C1 bits are then
    /// LFSR-descrambled with the C0-seeded PRN, and that descrambled
    /// pattern is not a codeword; the data-path Golay reports 2
    /// corrections. Pinned here as a regression guard on the ECC
    /// accounting; [`SILENCE`] is the true zero-error baseline.
    #[test]
    fn zero_frame_c0_is_clean_but_descrambled_c1_is_not() {
        let fec = frame_fec(&[0u8; 9]);
        assert_eq!(fec.c0_errors, 0, "all-zero C0 is the valid zero codeword");
        assert_eq!(
            fec.total_errors, 2,
            "descrambled all-zero C1 needs corrections"
        );
        assert_eq!(fec.kind, FrameKind::Voice);
    }

    /// Flipping a single wire bit inside the FEC-protected region of
    /// a clean frame registers exactly one correction. The protected
    /// wire positions depend on the interleave table, so the test
    /// locates one empirically instead of hardcoding an index.
    #[test]
    fn single_flipped_protected_bit_is_corrected() {
        let mut found = false;
        for bit_index in 0..72u32 {
            let mut frame = SILENCE;
            let byte = (bit_index / 8) as usize;
            if let Some(b) = frame.get_mut(byte) {
                *b ^= 1u8 << (bit_index % 8);
            }
            let fec = frame_fec(&frame);
            if fec.c0_errors == 1 {
                assert_eq!(
                    fec.total_errors, 1,
                    "a lone C0 bit error must be the only correction (bit {bit_index})"
                );
                found = true;
                break;
            }
        }
        assert!(
            found,
            "no wire bit flip landed in C0 (interleave regression?)"
        );
    }

    /// The repeat disposition trusts a frame at exactly three
    /// corrected bits and repeats the previous parameters at four,
    /// which is the `errs2 > 3` boundary mbelib uses (Golay mis-corrects
    /// silently above three). Real reflector uplinks live around this
    /// threshold, and decoding untrusted frames fresh is the past
    /// regression that turned noisy-but-intelligible speech into
    /// garble. Wire bit positions are located empirically so the test
    /// is independent of the interleave table.
    #[test]
    fn repeat_threshold_trusts_three_corrections_and_repeats_at_four()
    -> Result<(), Box<dyn std::error::Error>> {
        let flip = |frame: &mut [u8; 9], bit_index: u32| {
            let byte = (bit_index / 8) as usize;
            if let Some(b) = frame.get_mut(byte) {
                *b ^= 1u8 << (bit_index % 8);
            }
        };

        // Locate two C0-protected and two data-protected wire bits.
        let mut c0_bits: Vec<u32> = Vec::new();
        let mut data_bits: Vec<u32> = Vec::new();
        for bit_index in 0..72u32 {
            let mut frame = SILENCE;
            flip(&mut frame, bit_index);
            let fec = frame_fec(&frame);
            if fec.c0_errors == 1 && fec.total_errors == 1 {
                c0_bits.push(bit_index);
            } else if fec.c0_errors == 0 && fec.total_errors == 1 {
                data_bits.push(bit_index);
            }
        }
        let &[c0_a, c0_b, ..] = c0_bits.as_slice() else {
            return Err("need two C0-protected wire bits".into());
        };
        let &[d_a, d_b, ..] = data_bits.as_slice() else {
            return Err("need two data-protected wire bits".into());
        };

        let mut extractor = AmbeParamExtractor::new();
        let baseline = extractor.extract(&SILENCE);
        assert!(!baseline.repeated, "clean frame is never repeated");

        // Three corrections (2 in C0 + 1 in the data chain, each
        // Golay block within its correction capacity): still trusted,
        // and the corrected payload is byte-identical to SILENCE.
        let mut frame3 = SILENCE;
        flip(&mut frame3, c0_a);
        flip(&mut frame3, c0_b);
        flip(&mut frame3, d_a);
        let p3 = extractor.extract(&frame3);
        assert_eq!(p3.fec_errors, 3, "crafted frame must show 3 corrections");
        assert!(
            !p3.repeated,
            "exactly three corrected bits is still a trusted frame"
        );
        assert_eq!(
            p3.harmonics, baseline.harmonics,
            "fully corrected frame decodes to the clean payload"
        );

        // Four corrections, one past the trust threshold: the frame
        // must be discarded and the previous parameters repeated.
        let mut frame4 = SILENCE;
        flip(&mut frame4, c0_a);
        flip(&mut frame4, c0_b);
        flip(&mut frame4, d_a);
        flip(&mut frame4, d_b);
        let p4 = extractor.extract(&frame4);
        assert_eq!(p4.fec_errors, 4, "crafted frame must show 4 corrections");
        assert!(
            p4.repeated,
            "four corrected bits must repeat the previous parameters"
        );
        assert_eq!(p4.kind, FrameKind::Voice, "repeat keeps the voice kind");
        assert_eq!(
            p4.harmonics, p3.harmonics,
            "repeated frame carries the previous frame's parameters"
        );
        Ok(())
    }

    #[test]
    fn silence_frame_is_clean() {
        let fec = frame_fec(&SILENCE);
        assert_eq!(fec.total_errors, 0, "valid DVSI frame needs no corrections");
        assert_eq!(
            fec.kind,
            FrameKind::Voice,
            "silence b0 (124/125) is a voice-status frame"
        );
    }

    /// `frame_fec`'s kind must agree with the classification rule
    /// applied to the b0 that `decode_trace` extracts from the same
    /// wire bytes; the two paths share the unpack→ECC pipeline.
    #[test]
    fn kind_agrees_with_decode_trace_b0() {
        for frame in [[0u8; 9], SILENCE, [0xFF; 9], [0xA5; 9]] {
            let (b, _, _, _) = decode_trace(&frame);
            assert_eq!(
                frame_fec(&frame).kind,
                classify_b0(b[0]),
                "frame {frame:02X?}"
            );
        }
    }

    #[test]
    fn classify_b0_ranges() {
        assert_eq!(classify_b0(0), FrameKind::Voice);
        assert_eq!(classify_b0(119), FrameKind::Voice);
        for b0 in 120..=123 {
            assert_eq!(classify_b0(b0), FrameKind::Erasure, "b0={b0}");
        }
        assert_eq!(
            classify_b0(124),
            FrameKind::Voice,
            "silence is voice-status"
        );
        assert_eq!(
            classify_b0(125),
            FrameKind::Voice,
            "silence is voice-status"
        );
        assert_eq!(classify_b0(126), FrameKind::Tone);
        assert_eq!(classify_b0(127), FrameKind::Tone);
    }
}
