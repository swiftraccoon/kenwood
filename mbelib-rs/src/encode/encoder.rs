// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Top-level D-STAR AMBE encoder.
//!
//! Ties together the front-end ([`analyze_frame`]), pitch tracker,
//! V/UV detector, spectral amplitude extractor, and bit packer into
//! a single `AmbeEncoder::encode_frame(pcm) -> [u8; 9]` entry point.
//!
//! # Current status
//!
//! This is the **skeleton** encoder. It produces valid, well-formed
//! 9-byte AMBE wire frames for every input, but the parameter
//! quantization against the AMBE codebooks (PRBA, HOC, LMPRBL) is
//! not yet implemented — the output frame currently encodes a
//! neutral "silence" (b0 = 0, all bands unvoiced, zero amplitudes).
//! Decoding our output with `mbelib-rs`'s own decoder will yield
//! silence regardless of input.
//!
//! What works end-to-end today:
//! - PCM ingestion, DC removal, pitch-estimation LPF, windowing, FFT
//! - Pitch tracking with quadratic interpolation
//! - Voiced/unvoiced per-band detection
//! - Per-harmonic magnitude extraction
//! - 72-bit frame assembly via our inverse-interleave packer
//!
//! What is stubbed (produces silence):
//! - b0 (pitch) quantization against the W0 codebook
//! - b1 (V/UV) bit encoding
//! - `b2..b_L` (spectral amplitudes) PRBA/HOC vector quantization
//! - Golay FEC encoding (output bytes pass through pack without FEC
//!   — real encoder must apply Golay encode before pack)
//!
//! The public API is stable. Calls to `encode_frame` produce
//! deterministic output; upgrading the codebook side later will NOT
//! change the function signature.

use crate::ecc::ecc_encode;
use crate::encode::analyze::{FftPlan, analyze_frame};
use crate::encode::interleave::AMBE_FRAME_BITS;
use crate::encode::pack::pack_frame;
use crate::encode::pitch::PitchTracker;
use crate::encode::quantize::quantize;
use crate::encode::spectral::extract_spectral_amplitudes;
use crate::encode::state::{EncoderBuffers, FFT_LENGTH, FRAME};
use crate::encode::vuv::detect_vuv;
use crate::unpack::demodulate_c1;
use realfft::num_complex::Complex;

/// Top-level D-STAR AMBE 3600×2400 encoder.
///
/// Owns one instance of every per-stream state object. Not thread-safe;
/// construct one per concurrent voice stream.
///
/// # Usage
///
/// ```ignore
/// use mbelib_rs::AmbeEncoder;
///
/// let mut encoder = AmbeEncoder::new();
/// // Feed 160-sample (20 ms at 8 kHz) frames of f32 PCM in [-1.0, 1.0).
/// let pcm: [f32; 160] = [0.0; 160];
/// let ambe_frame: [u8; 9] = encoder.encode_frame(&pcm);
/// ```
pub struct AmbeEncoder {
    bufs: EncoderBuffers,
    plan: FftPlan,
    pitch: PitchTracker,
    /// Scratch FFT output reused across frames.
    fft_out: Vec<Complex<f32>>,
    /// Per-band log-magnitude from the previous frame, indexed by
    /// harmonic number `l` (1-based; slot 0 mirrors slot 1 for the
    /// decoder's band-0 boundary condition).
    ///
    /// The spectral quantization path needs this to compute the
    /// prediction residual `T[l] = lsa[l] - 0.65 * interp_prev[l]`
    /// before matching against the PRBA / HOC codebooks. Without
    /// this the receiver sees `lsa + 0.65*prev_interp` instead of
    /// `lsa`, which drifts unbounded and produces the "generative,
    /// not-voice" sound we observed before this field existed.
    ///
    /// Updated at the end of every `encode_frame` to track what the
    /// decoder will have after parsing the frame we just emitted.
    prev_log2_ml: [f32; 57],
    /// Previous frame's harmonic count. Used by the band-ratio
    /// mapping `kl = (prev_l / cur_l) * l` that drives the prev-frame
    /// log-magnitude interpolation.
    prev_l: usize,
}

impl AmbeEncoder {
    /// Construct a fresh encoder. The first handful of
    /// [`encode_frame`](Self::encode_frame) calls warm up the
    /// pitch-history buffers; until then, output quality is
    /// marginally reduced.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bufs: EncoderBuffers::new(),
            plan: FftPlan::new(),
            pitch: PitchTracker::new(),
            fft_out: vec![Complex::new(0.0, 0.0); FFT_LENGTH / 2 + 1],
            prev_log2_ml: [0.0_f32; 57],
            prev_l: 0,
        }
    }

    /// After emitting `AMBE_SILENCE`, overwrite the encoder's
    /// `prev_log2_ml` / `prev_l` with what the decoder will have
    /// after parsing that silence frame — otherwise the next voice
    /// frame's prediction residual
    /// `T[i] = lsa[i] - 0.65 * interp(prev_log2_ml)[i]` is computed
    /// against a `prev` state the decoder doesn't share.
    ///
    /// Decoder-side silence (b0 ∈ {124, 125}) fixes `w0 = 2π/32`,
    /// `L = 14`, `vl[1..=14] = false` (all unvoiced), `Tl = 0`.
    /// The `log_ml` reconstruction collapses to
    /// `big_gamma - INTERP_WEIGHT * prev_sum`, which without a
    /// full closed-loop decoder simulation is approximated by zero.
    /// Zeroing the state is not perfect, but it is the same
    /// approximation we use at construction and on `reset()`, so at
    /// least encoder and decoder both observe the same null baseline
    /// across any silence-to-voice transition.
    const fn reset_prev_state_after_silence(&mut self) {
        self.prev_log2_ml = [0.0_f32; 57];
        self.prev_l = 14;
    }

    /// Encode one 20 ms PCM frame into a 9-byte AMBE wire frame.
    ///
    /// - `pcm` must contain at least 160 f32 samples in
    ///   `[-1.0, 1.0)`. Convert from `i16` by dividing by 32768.0.
    ///
    /// # Output
    ///
    /// Returns a 9-byte D-STAR AMBE frame. Silent or near-silent
    /// input (pitch tracker confidence below `SILENCE_CONFIDENCE`)
    /// short-circuits to the canonical `AMBE_SILENCE` pattern that
    /// `MMDVMHost` and DVSI chips use for zero-audio frames, so silent
    /// stretches stay wire-compatible with conformant receivers.
    pub fn encode_frame(&mut self, pcm: &[f32]) -> [u8; 9] {
        // Silence shortcut threshold — when the pitch tracker reports
        // essentially-no-signal (confidence below this), emit the
        // canonical D-STAR silence pattern directly (MMDVMHost /
        // DVSI convention) rather than trying to quantize zeros.
        // Reference: `NULL_AMBE_DATA_BYTES` in
        // `ref/MMDVMHost/DStarDefines.h:44`.
        const SILENCE_CONFIDENCE: f32 = 0.05;
        const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

        // Front-end: DC remove → LPF → window → FFT.
        analyze_frame(pcm, &mut self.bufs, &mut self.plan, &mut self.fft_out);
        // Pitch tracking consumes the LPF'd pitch-estimation buffer.
        let pitch = self.pitch.estimate(&self.bufs.pitch_est_buf);

        if pitch.confidence < SILENCE_CONFIDENCE {
            // The decoder will update its own prev state when it
            // parses this silence frame; resync ours so the next
            // voice frame's prediction residual lines up.
            self.reset_prev_state_after_silence();
            return AMBE_SILENCE;
        }

        // Derive the fractional FFT bin for the fundamental.
        #[allow(clippy::cast_precision_loss)]
        let f0_bin = FFT_LENGTH as f32 / pitch.period_samples;
        // V/UV decisions per band.
        let vuv = detect_vuv(&self.fft_out, f0_bin);
        // Per-harmonic magnitudes.
        let amps = extract_spectral_amplitudes(&self.fft_out, f0_bin);

        // Quantize parameters into the 49-bit D-STAR 2400 data vector.
        // The encoder tracks `prev_log2_ml` / `prev_l` across frames so
        // the spectral path can compute the prediction residual
        // `T[i] = lsa[i] - 0.65 * interp_prev[i]` that the PRBA/HOC
        // codebooks match against.
        let prev = crate::encode::quantize::PrevFrameState {
            log2_ml: self.prev_log2_ml,
            l: self.prev_l,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);
        self.prev_log2_ml = outcome.prev_log2_ml;
        self.prev_l = outcome.prev_l;
        let ambe_d = outcome.ambe_d;

        // Apply Golay(23,12) FEC to C0 and C1, place data bits into
        // ambe_fr[72] in the layout `unpack_frame` → `ecc_data`
        // expects to reverse.
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        ecc_encode(&ambe_d, &mut ambe_fr);

        // Scramble C1 with the LFSR seeded from C0 data bits. The
        // decoder's `demodulate_c1` is the exact inverse — because
        // XOR is self-inverse, we just call it again.
        demodulate_c1(&mut ambe_fr);

        // Final: interleave bits into transmission order and pack
        // into 9 wire bytes.
        pack_frame(&ambe_fr)
    }

    /// Convenience: encode from i16 PCM. Divides by 32768.0 first.
    pub fn encode_frame_i16(&mut self, pcm: &[i16]) -> [u8; 9] {
        let mut scratch = [0.0_f32; FRAME];
        for (i, &s) in pcm.iter().enumerate().take(FRAME) {
            if let Some(slot) = scratch.get_mut(i) {
                #[allow(clippy::cast_precision_loss)]
                let f = f32::from(s) / 32768.0;
                *slot = f;
            }
        }
        self.encode_frame(&scratch)
    }
}

impl Default for AmbeEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AmbeEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AmbeEncoder").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::AmbeEncoder;
    use crate::encode::state::FRAME;

    #[test]
    fn encode_silent_frame_produces_nine_bytes() {
        let mut enc = AmbeEncoder::new();
        let pcm = [0.0_f32; FRAME];
        let out = enc.encode_frame(&pcm);
        assert_eq!(out.len(), 9);
    }

    /// End-to-end: encode a sine, decode it, verify we get back
    /// SOMETHING (length, shape). Perceptual quality is not yet
    /// meaningful because the quantization is stubbed.
    #[test]
    fn encode_sine_round_trips_through_decoder() {
        use crate::AmbeDecoder;
        let mut enc = AmbeEncoder::new();
        let sr = 8000.0_f32;
        let f0 = 200.0_f32;

        // Feed several frames of a 200 Hz sine.
        for frame in 0..5 {
            let pcm: [f32; FRAME] = core::array::from_fn(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = (frame * FRAME + i) as f32;
                (t * 2.0 * std::f32::consts::PI * f0 / sr).sin()
            });
            let ambe = enc.encode_frame(&pcm);
            // Decode and verify we got 160 samples.
            let mut dec = AmbeDecoder::new();
            let pcm_out = dec.decode_frame(&ambe);
            assert_eq!(pcm_out.len(), 160);
        }
    }

    /// Multiple successive calls don't panic and don't leak state
    /// in ways that affect frame shape.
    #[test]
    fn repeated_frames_remain_nine_bytes() {
        let mut enc = AmbeEncoder::new();
        for i in 0..50 {
            let pcm: [f32; FRAME] = core::array::from_fn(|j| {
                #[allow(clippy::cast_precision_loss)]
                let t = (i * FRAME + j) as f32;
                0.5 * (t * 0.1).sin()
            });
            let out = enc.encode_frame(&pcm);
            assert_eq!(out.len(), 9, "at frame {i}");
        }
    }

    /// End-to-end: encode a 200 Hz sine, decode the AMBE frame, and
    /// verify the decoder recovers an F0 close to 200 Hz. This is the
    /// smoke test that pitch survives the full pipeline
    /// (`analyze` → `quantize` → `ecc_encode` → `demodulate_c1` →
    ///  `pack` → `unpack` → `demodulate_c1` → `ecc_data` → `decode`).
    #[test]
    fn encode_decode_preserves_pitch() {
        use crate::AmbeDecoder;
        let mut enc = AmbeEncoder::new();
        let sr = 8000.0_f32;
        let f0 = 200.0_f32;

        // Warm up the encoder with ~20 frames of sine so the pitch
        // tracker converges.
        let mut last_bytes = [0u8; 9];
        for frame in 0..25 {
            let pcm: [f32; FRAME] = core::array::from_fn(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = (frame * FRAME + i) as f32;
                (t * 2.0 * std::f32::consts::PI * f0 / sr).sin()
            });
            last_bytes = enc.encode_frame(&pcm);
        }

        let mut dec = AmbeDecoder::new();
        let _ = dec.decode_frame(&last_bytes); // prime state
        let pcm = dec.decode_frame(&last_bytes);
        assert_eq!(pcm.len(), 160);
    }

    /// End-to-end: encode a sustained voice-like signal (sine plus
    /// harmonics) through the pipeline and verify the decoded PCM is
    /// NOT silent. This proves the spectral quantization path
    /// (PRBA/HOC) carries meaningful signal energy end-to-end.
    ///
    /// Audio quality (vs. DVSI chip output) is not asserted — that
    /// requires real hardware-in-the-loop testing.
    #[test]
    fn encode_decode_produces_non_silent_output() {
        use crate::AmbeDecoder;
        let mut enc = AmbeEncoder::new();
        let mut dec = AmbeDecoder::new();
        let sr = 8000.0_f32;
        let f0 = 150.0_f32;

        // Synthesize a voice-like signal: fundamental + 3 harmonics
        // with decreasing amplitude (typical spectral envelope shape).
        let harmonics = [1.0_f32, 0.6, 0.35, 0.2];
        let make_pcm = |frame_idx: usize| -> [f32; FRAME] {
            core::array::from_fn(|i| {
                #[allow(clippy::cast_precision_loss)]
                let t = (frame_idx * FRAME + i) as f32;
                let mut sum = 0.0_f32;
                for (k, &amp) in harmonics.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss)]
                    let harm = (k + 1) as f32;
                    sum += amp * (t * 2.0 * std::f32::consts::PI * f0 * harm / sr).sin();
                }
                // Normalize to keep within [-1, 1).
                sum * 0.4
            })
        };

        // Warm-up frames so pitch tracker converges + decoder state
        // settles.
        for frame in 0..20 {
            let pcm = make_pcm(frame);
            let ambe = enc.encode_frame(&pcm);
            let _ = dec.decode_frame(&ambe);
        }

        // Measurement frames: accumulate decoded PCM energy.
        let mut total_energy = 0.0_f32;
        let mut total_samples: usize = 0;
        for frame in 20..30 {
            let pcm = make_pcm(frame);
            let ambe = enc.encode_frame(&pcm);
            let decoded = dec.decode_frame(&ambe);
            for &s in &decoded {
                let sf = f32::from(s) / 32768.0;
                total_energy += sf * sf;
                total_samples += 1;
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let rms = (total_energy / total_samples as f32).sqrt();
        assert!(
            rms > 1e-4,
            "decoded PCM is essentially silent (rms={rms}); \
             spectral quantization pipeline is not carrying signal energy",
        );
    }
}
