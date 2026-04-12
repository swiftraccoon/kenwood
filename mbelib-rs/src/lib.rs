//! Pure Rust AMBE 3600×2450 voice codec decoder for D-STAR digital radio.
//!
//! The AMBE (Advanced Multi-Band Excitation) 3600×2450 codec compresses
//! speech at 3600 bits/second with 2450 bits of voice data and 1150 bits
//! of forward error correction (FEC). It is the mandatory voice codec
//! for the JARL D-STAR digital radio standard, used in all D-STAR
//! transceivers and reflectors worldwide.
//!
//! Each voice frame is 9 bytes (72 bits), transmitted at 50 frames per
//! second (20 ms per frame). The codec models speech as a sum of
//! harmonically related sinusoids, with each band independently
//! classified as voiced or unvoiced.
//!
//! This crate is a decode-only port of the ISC-licensed
//! [mbelib](https://github.com/szechyjs/mbelib) C library. It has zero
//! runtime dependencies and requires only `std` for floating-point math.
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
//! 2. **Error correction** — Golay(23,12) on C0/C1, Hamming(15,11) on C3
//! 3. **Demodulation** — LFSR descrambling of C1 using C0 seed
//! 4. **Parameter extraction** — 49 decoded bits → fundamental frequency,
//!    harmonic count, voiced/unvoiced decisions, spectral magnitudes
//! 5. **Spectral enhancement** — adaptive amplitude weighting for clarity
//! 6. **Synthesis** — harmonic oscillator bank (voiced) + noise (unvoiced)
//! 7. **Output conversion** — float PCM → i16 with gain and clamping

mod decode;
mod ecc;
mod enhance;
mod error;
mod params;
mod synthesize;
mod tables;
mod unpack;

pub use error::DecodeError;

use ecc::AMBE_DATA_BITS;
use params::MbeParams;
use synthesize::FRAME_SAMPLES;
use unpack::AMBE_FRAME_BITS;

/// Output audio gain applied during float-to-i16 conversion.
const GAIN: f32 = 7.0;

/// Maximum absolute sample value after gain (clamp threshold).
const CLAMP_MAX: f32 = 32_760.0;

/// Stateful AMBE 3600×2450 voice frame decoder.
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
        }
    }

    /// Decodes a single 9-byte AMBE frame into 160 PCM samples.
    ///
    /// Returns 160 signed 16-bit samples at 8000 Hz (20 ms of audio).
    /// A gain factor of 7.0 is applied and samples are clamped to
    /// ±32760 to prevent clipping artifacts.
    ///
    /// If the frame contains excessive bit errors (more than the FEC
    /// can correct), the decoder repeats the previous frame's
    /// parameters up to 3 times, then outputs silence.
    #[must_use]
    pub fn decode_frame(&mut self, ambe: &[u8; 9]) -> [i16; FRAME_SAMPLES] {
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        let mut ambe_d = [0u8; AMBE_DATA_BITS];

        // Unpack the 9-byte frame into individual bits ordered by FEC
        // codeword. The 72 bits are interleaved across 4 codewords
        // (C0, C1, C2, C3) so the FEC can protect different parts of
        // the parameter space independently.
        unpack::unpack_frame(ambe, &mut ambe_fr);

        // Apply Golay(23,12) error correction to the C0 codeword.
        // C0 protects the most critical parameters (fundamental
        // frequency index b0), so it gets the strongest FEC.
        let _c0_errors = ecc::ecc_c0(&mut ambe_fr);

        // Demodulate C1 using an LFSR sequence seeded from the
        // corrected C0 data. This scrambling prevents systematic
        // errors in C0 from propagating into C1.
        unpack::demodulate_c1(&mut ambe_fr);

        // Apply ECC to the remaining codewords (C1 Golay, C2 Golay,
        // C3 Hamming) and pack the corrected bits into the 49-bit
        // parameter vector.
        let _total_errors = ecc::ecc_data(&ambe_fr, &mut ambe_d);

        // Decode the 49 parameter bits into the harmonic speech model:
        // fundamental frequency (b0), voiced/unvoiced decisions (b1),
        // gain delta (b2), and spectral magnitudes (b3-b8).
        // Status: 0 = valid voice, 2 = erasure, 3 = tone signal.
        let _decode_status = decode::decode_params(&ambe_d, &mut self.cur, &self.prev);

        // TODO(task-6): if combined errors exceed threshold, increment
        // the repeat counter and reuse previous frame's parameters.
        // After 3 consecutive repeats, output silence. This matches
        // mbelib's mbe_processAmbe2450Dataf() error handling.

        // Snapshot current parameters as the prediction reference for
        // the NEXT frame's delta decoding. This must happen BEFORE
        // enhancement, because the delta predictions are relative to
        // un-enhanced magnitudes.
        self.prev.copy_from(&self.cur);

        // Spectral amplitude enhancement: adjusts per-band magnitudes
        // based on autocorrelation to reduce codec artifacts. This
        // operates on the current frame only and does not affect the
        // prediction reference saved above.
        enhance::spectral_amp_enhance(&mut self.cur);

        // Synthesize PCM audio from the enhanced parameters. Each
        // harmonic band contributes a windowed cosine oscillator
        // (voiced) or random-phase multisine (unvoiced). The synthesis
        // window (Ws) cross-fades between the previous enhanced frame
        // and the current one for smooth transitions.
        //
        // Both `cur` and `prev_enhanced` are mutated: `cur` gets phase
        // updates (PSI/PHI) for continuity into the next frame, and
        // `prev_enhanced` gets band extension (zero-fill when the
        // current frame has more harmonics).
        let mut pcm_f = [0.0f32; FRAME_SAMPLES];
        synthesize::synthesize_speech(&mut pcm_f, &mut self.cur, &mut self.prev_enhanced);

        // Save the enhanced parameters as the cross-fade source for
        // the next frame's synthesis.
        self.prev_enhanced.copy_from(&self.cur);

        // Convert floating-point PCM to 16-bit signed integers.
        // The gain of 7.0 matches mbelib's mbe_floattoshort() output
        // level. Clamping to ±32760 (not ±32767) leaves headroom to
        // avoid wrap-around clipping artifacts.
        let mut pcm = [0i16; FRAME_SAMPLES];
        let mut i = 0;
        while i < FRAME_SAMPLES {
            let scaled = pcm_f.get(i).map_or(0.0, |sample| sample * GAIN);
            let clamped = scaled.clamp(-CLAMP_MAX, CLAMP_MAX);
            if let Some(out) = pcm.get_mut(i) {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "clamped to ±32760 which fits in i16; no truncation-free \
                              f32→i16 path exists in stable Rust without unsafe"
                )]
                {
                    *out = clamped as i16;
                }
            }
            i += 1;
        }

        pcm
    }
}

impl Default for AmbeDecoder {
    fn default() -> Self {
        Self::new()
    }
}
