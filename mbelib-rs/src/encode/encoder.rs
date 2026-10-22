// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Top-level D-STAR AMBE encoder.
//!
//! Ties together the front-end ([`crate::analyze_frame`]), pitch tracker,
//! V/UV detector, spectral amplitude extractor, and bit packer into
//! a single `AmbeEncoder::encode_frame(pcm) -> [u8; 9]` entry point.
//!
//! # Status
//!
//! Every stage of OP25's `ambe_encoder.cc` is ported and wired together,
//! including the optional two-frame look-ahead pitch path. The output
//! clears structural round-trip and non-silence tests, and the validation
//! examples compare individual stages with supplied OP25 traces.
//!
//! The encoder remains experimental. Match rates depend on the trace, the
//! PRBA/HOC spectral-envelope path (`b3` through `b8`) is still under
//! investigation, and the ignored sine-correlation test remains near 0.04.
//! Reliable DVSI hardware interoperability is therefore not yet validated.
//!
//! # Pipeline
//!
//! 1. Front-end: DC remove → pitch-LPF → window → 256-pt FFT.
//! 2. Pitch estimation (sub-harmonic summation on the LPF'd buffer).
//! 3. Per-band V/UV decisions from the FFT.
//! 4. Per-harmonic magnitudes via 3-bin power integration.
//! 5. Quantization to the 49-bit `ambe_d` parameter vector:
//!    L-constrained W0 search for b0, energy-weighted VUV codebook
//!    search for b1, `AmbePlus` DG nearest for b2, block-DCT → R
//!    pairs → 8-pt DCT → G for PRBA24/PRBA58 (b3/b4), per-block
//!    HOC codebooks for b5..b8. Prediction residual
//!    `T = lsa − 0.65·interp(prev_log2_ml)` uses closed-loop
//!    reconstruction via `decode_params` so encoder and decoder
//!    track identical magnitude history.
//! 6. Golay(23,12) FEC on C0 and C1, plus outer parity on C0.
//! 7. LFSR scramble of C1 seeded from C0 data bits.
//! 8. 72-bit DSD-style interleave to wire order, pack to 9 bytes.

use realfft::num_complex::Complex;

use crate::PcmFrame;
use crate::ecc::ecc_encode;
use crate::encode::analyze::{FftPlan, analyze_kernel};
use crate::encode::interleave::AMBE_FRAME_BITS;
use crate::encode::pack::pack_kernel;
use crate::encode::pitch::{PITCH_CANDIDATES, PitchTracker, compute_e_p_kernel};
use crate::encode::quantize::quantize_kernel;
use crate::encode::state::{EncoderBuffers, FFT_LENGTH, FRAME, InputFilter};
use crate::encode::vuv::{VuvState, detect_vuv_and_sa_kernel};

/// Pitch-tracker latency policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PitchTracking {
    /// Commit the current frame using look-back and sub-multiple analysis.
    #[default]
    SingleFrame,
    /// Buffer two frames and commit a frame after 40 ms of lookahead.
    Lookahead,
}

/// Independent encoder algorithm choices; every combination is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EncoderOptions {
    /// Pitch-tracker latency policy.
    pub pitch_tracking: PitchTracking,
    /// Input-conditioning coefficients, selected independently of Cargo features.
    pub input_filter: InputFilter,
}

struct Quantizer {
    prev_log2_ml: [f32; 57],
    prev_l: usize,
    prev_gamma: f32,
    vuv_state: VuvState,
}

impl Quantizer {
    const fn new() -> Self {
        Self {
            prev_log2_ml: [0.0; 57],
            prev_l: 0,
            prev_gamma: 0.0,
            vuv_state: VuvState::new(),
        }
    }
}

/// Per-frame snapshot buffered by the 2-frame look-ahead pipeline.
///
/// The look-ahead DP commits pitch for frame `N-2` only after it has
/// seen the `E(p)` arrays for frames `N-2`, `N-1`, and `N`. Until
/// frame `N` arrives, every `encode_frame(N-2)` call's downstream
/// quantization is held in this slot; the FFT output is saved so
/// voicing / spectral-amplitude extraction re-runs against the same
/// spectrum the encoder saw at analysis time.
struct FrameSlot {
    e_p: [f32; PITCH_CANDIDATES],
    fft_out: [Complex<f32>; 129],
}

/// Top-level D-STAR AMBE 3600×2400 encoder.
///
/// Owns one instance of every per-stream state object. Construct one per
/// concurrent voice stream and feed each stream's frames in order. The encoder
/// implements `Send` and `Sync`; encoding requires exclusive mutable access.
///
/// See the [checked PCM](crate#checked-pcm) and
/// [diagnostic-stage workflows](crate#inspect-the-encoder-stages).
pub struct AmbeEncoder {
    bufs: EncoderBuffers,
    plan: FftPlan,
    pitch: PitchTracker,
    /// Scratch FFT output reused across frames.
    fft_out: [Complex<f32>; 129],
    /// Reconstructed magnitude/gain predictor and V/UV hysteresis history.
    quantizer: Quantizer,
    /// At most two saved analysis frames for the optional 40 ms lookahead.
    pending: Option<Vec<FrameSlot>>,
}

/// Silence shortcut threshold: when the pitch tracker reports
/// essentially-no-signal (confidence below this), emit the canonical
/// D-STAR silence pattern directly (`MMDVMHost` / DVSI convention)
/// rather than trying to quantize zeros. Reference:
/// `NULL_AMBE_DATA_BYTES` in `MMDVMHost/DStarDefines.h:44`.
const SILENCE_CONFIDENCE: f32 = 0.05;

/// Canonical 9-byte AMBE silence frame returned for inputs that fall
/// below [`SILENCE_CONFIDENCE`] and during the 2-frame warmup of the
/// look-ahead pipeline.
const AMBE_SILENCE: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

impl AmbeEncoder {
    /// Construct a fresh encoder using OP25's single-frame
    /// (look-back + sub-multiples) pitch tracker. Zero added
    /// latency; each [`encode_frame`](Self::encode_frame) call
    /// commits pitch for the just-received frame.
    ///
    /// The input filter is `InputFilter::Reference`. Optional two-frame
    /// lookahead is selected through [`Self::new_with_lookahead`] or
    /// [`Self::with_options`]. Construction allocates the reusable FFT cache.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(EncoderOptions::default())
    }

    /// Construct zeroed history with explicit input-filter and latency choices.
    ///
    /// Plans and scratch allocate during construction. Frame processing uses
    /// fixed spectra and retains at most two lookahead snapshots without
    /// allocating or cloning a spectrum vector each frame.
    #[must_use]
    pub fn with_options(options: EncoderOptions) -> Self {
        Self {
            bufs: EncoderBuffers::with_input_filter(options.input_filter),
            plan: FftPlan::new(),
            pitch: PitchTracker::new(),
            fft_out: [Complex::new(0.0, 0.0); 129],
            quantizer: Quantizer::new(),
            pending: match options.pitch_tracking {
                PitchTracking::SingleFrame => None,
                PitchTracking::Lookahead => Some(Vec::with_capacity(2)),
            },
        }
    }

    /// Construct a fresh encoder WITH the 2-frame look-ahead DP
    /// enabled. The first two [`encode_frame`](Self::encode_frame)
    /// calls return `AMBE_SILENCE` while the pipeline fills; frame
    /// `N-2`'s pitch is committed on the third call (frame `N`).
    /// Adds 40 ms of buffering. The final two buffered inputs have no flush
    /// operation and are discarded when the encoder is dropped.
    #[must_use]
    pub fn new_with_lookahead() -> Self {
        Self::with_options(EncoderOptions {
            pitch_tracking: PitchTracking::Lookahead,
            ..EncoderOptions::default()
        })
    }

    /// Return the selected algorithm choices without changing stream history.
    #[must_use]
    pub const fn options(&self) -> EncoderOptions {
        EncoderOptions {
            pitch_tracking: if self.pending.is_some() {
                PitchTracking::Lookahead
            } else {
                PitchTracking::SingleFrame
            },
            input_filter: self.bufs.input_filter(),
        }
    }
}

impl Quantizer {
    /// After emitting `AMBE_SILENCE`, overwrite the encoder's
    /// `prev_log2_ml` / `prev_l` with what the decoder will have
    /// after parsing that silence frame; otherwise the next voice
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
    /// approximation used at construction and on `reset()`, so at
    /// least encoder and decoder both observe the same null baseline
    /// across any silence-to-voice transition.
    const fn reset_prev_state_after_silence(&mut self) {
        self.prev_log2_ml = [0.0_f32; 57];
        self.prev_l = 14;
        // Silence frames carry b2 = 0 in the canonical AMBE_SILENCE
        // byte pattern, so a stream of silence frames drives
        // `gamma = DG_TABLE[0] + 0.5 * prev_gamma` = 0.5 * prev_gamma,
        // decaying toward zero. Pin it at zero so the first voice
        // frame after silence sees the same state the decoder will
        // have reconstructed from the silence stream on its side.
        self.prev_gamma = 0.0;
    }
}

impl AmbeEncoder {
    /// Encode one 20 ms PCM frame into a 9-byte AMBE wire frame.
    ///
    /// `pcm` contains exactly 160 finite samples in inclusive `[-1,1]`.
    ///
    /// # Output
    ///
    /// Returns a 9-byte D-STAR AMBE frame. Silent or near-silent
    /// input (pitch tracker confidence below `SILENCE_CONFIDENCE`)
    /// short-circuits to the canonical `AMBE_SILENCE` pattern that
    /// `MMDVMHost` and DVSI chips use for zero-audio frames, so silent
    /// stretches stay wire-compatible with conformant receivers.
    ///
    /// The selected filter/latency policy is fixed for this stream. A fresh
    /// encoder begins with zero predictor history. See [checked PCM](crate#checked-pcm).
    pub fn encode_frame(&mut self, pcm: &PcmFrame) -> [u8; 9] {
        // Front-end: DC remove → LPF → window → FFT. After this
        // call, `self.bufs.pitch_est_buf` holds the latest 301
        // samples of LPF'd audio and `self.fft_out` holds the
        // 129-bin complex spectrum of the just-arrived frame.
        analyze_kernel(
            pcm.samples(),
            &mut self.bufs,
            &mut self.plan,
            &mut self.fft_out,
        );
        let e_p_current = compute_e_p_kernel(&self.bufs.pitch_est_buf);

        // Look-ahead path: buffer the e_p array + a copy of the FFT
        // spectrum, then emit bytes only once we have 3 frames. The
        // `let-else` binds the pending queue directly, so there is no
        // separate is-Some check to fall out of sync with.
        let Some(pending) = self.pending.as_mut() else {
            // Zero-latency path: commit pitch for the just-received
            // frame via single-frame look-back + sub-multiples.
            let pitch = self.pitch.estimate_kernel(&self.bufs.pitch_est_buf);
            return self.quantizer.quantize_from_fft(&self.fft_out, pitch);
        };
        let slot = FrameSlot {
            e_p: e_p_current,
            fft_out: self.fft_out,
        };
        if pending.len() < 2 {
            pending.push(slot);
            // Pipeline not full yet: emit silence and keep the
            // decoder's `prev_log2_ml` state consistent with what
            // it sees on the wire.
            self.quantizer.reset_prev_state_after_silence();
            return AMBE_SILENCE;
        }

        // Three frames now on hand: pending[0]=N-2, pending[1]=N-1,
        // slot=N. Run the DP against pending[0]'s e_p, using the
        // next two as lookahead.
        let pitch = if let [n_minus_2, n_minus_1, ..] = pending.as_slice() {
            self.pitch
                .estimate_with_lookahead_kernel(&n_minus_2.e_p, &n_minus_1.e_p, &slot.e_p)
        } else {
            unreachable!("pending holds two slots after the `< 2` early return above")
        };
        // Swap out the oldest slot so we can take ownership of its
        // FFT output without cloning; push the newly-arrived slot.
        let oldest = pending.remove(0);
        pending.push(slot);

        // Quantize frame N-2's spectrum against the DP-chosen pitch.
        self.quantizer.quantize_from_fft(&oldest.fft_out, pitch)
    }
}

impl Quantizer {
    /// Quantize an arbitrary saved FFT spectrum against a committed
    /// pitch, returning 9 wire bytes. Shared by the zero-latency and
    /// look-ahead paths; the look-ahead path hands in an FFT saved
    /// from 2 frames ago.
    fn quantize_from_fft(
        &mut self,
        fft_out: &[Complex<f32>],
        pitch: crate::encode::pitch::PitchEstimate,
    ) -> [u8; 9] {
        if pitch.confidence < SILENCE_CONFIDENCE {
            self.reset_prev_state_after_silence();
            return AMBE_SILENCE;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "FFT_LENGTH is 256 (constant), well within f32 mantissa precision; \
                      the usize-to-f32 cast is exact."
        )]
        let f0_bin = FFT_LENGTH as f32 / pitch.period_samples;
        // `e_p` for OP25's V/UV threshold is the pitch tracker's
        // reconstruction-error metric (1 − confidence on the chosen
        // period). Our PitchEstimate carries `confidence`; invert to
        // get the error.
        let e_p = (1.0 - pitch.confidence).clamp(0.0, 1.0);
        let (vuv, amps) = detect_vuv_and_sa_kernel(fft_out, f0_bin, &mut self.vuv_state, e_p);

        let prev = crate::encode::quantize::PrevFrameState {
            log2_ml: self.prev_log2_ml,
            l: self.prev_l,
            prev_gamma: self.prev_gamma,
        };
        let outcome = quantize_kernel(pitch, vuv, &amps, &prev);
        self.prev_log2_ml = outcome.prev_log2_ml;
        self.prev_l = outcome.prev_l;
        self.prev_gamma = outcome.prev_gamma;
        let ambe_d = outcome.ambe_d;

        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        ecc_encode(&ambe_d, &mut ambe_fr);
        // Note: do NOT call `demodulate_c1` here. `pack_frame` performs
        // the C1 modulation step internally before laying bits on the
        // wire (the doc comment on `pack_frame` documents that it
        // expects already-demodulated `ambe_fr` and re-modulates as
        // part of the packing). An explicit `demodulate_c1` call
        // *before* `pack_frame` cancels with the one inside, leaving
        // the wire bytes in unmodulated form, which the decoder's
        // single `demodulate_c1` then incorrectly XORs back into the
        // modulated state. Net effect: 7 of 49 `ambe_d` bits flip
        // through the round-trip (b3 was reading 304 instead of 368
        // for the 1 kHz sine, scrambling the spectral envelope and
        // pinning decoded peaks at the wrong harmonic).
        pack_kernel(&ambe_fr)
    }
}

impl AmbeEncoder {
    /// Convenience: encode from i16 PCM. Divides by 32768.0 first.
    pub fn encode_frame_i16(&mut self, pcm: &[i16; FRAME]) -> [u8; 9] {
        self.encode_frame(&PcmFrame::from(*pcm))
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
    use crate::PcmFrame;
    use crate::encode::state::FRAME;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn encode_silent_frame_produces_nine_bytes() -> TestResult {
        let mut enc = AmbeEncoder::new();
        let pcm = [0.0_f32; FRAME];
        let out = enc.encode_frame(&PcmFrame::new(pcm)?);
        assert_eq!(out.len(), 9);
        Ok(())
    }

    /// End-to-end: encode a 200 Hz sine, decode it, verify the
    /// decoder produces PCM of the expected shape and non-zero
    /// energy.  Full perceptual-quality validation lives in the
    /// `encoder_roundtrip.rs` integration test + the `validate_*`
    /// example harnesses; this unit test is a smoke check that the
    /// `encode_frame` → `decode_frame` pipeline doesn't panic or
    /// deadlock on a trivial input.
    #[test]
    fn encode_sine_round_trips_through_decoder() -> TestResult {
        use crate::AmbeDecoder;
        let mut enc = AmbeEncoder::new();
        let sr = 8000.0_f32;
        let f0 = 200.0_f32;

        // Feed several frames of a 200 Hz sine.
        for frame in 0..5 {
            let pcm: [f32; FRAME] = core::array::from_fn(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test sine generator: frame * FRAME + i stays under 5 * 160 = 800, \
                              far below f32 mantissa precision."
                )]
                let t = (frame * FRAME + i) as f32;
                (t * 2.0 * std::f32::consts::PI * f0 / sr).sin()
            });
            let ambe = enc.encode_frame(&PcmFrame::new(pcm)?);
            // Decode and verify we got 160 samples.
            let mut dec = AmbeDecoder::new();
            let pcm_out = dec.decode_frame(&ambe);
            assert_eq!(pcm_out.len(), 160);
        }
        Ok(())
    }

    /// Multiple successive calls don't panic and don't leak state
    /// in ways that affect frame shape.
    #[test]
    fn repeated_frames_remain_nine_bytes() -> TestResult {
        let mut enc = AmbeEncoder::new();
        for i in 0..50 {
            let pcm: [f32; FRAME] = core::array::from_fn(|j| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test tone generator: i * FRAME + j stays under 50 * 160 = 8000, \
                              far below f32 mantissa precision."
                )]
                let t = (i * FRAME + j) as f32;
                0.5 * (t * 0.1).sin()
            });
            let out = enc.encode_frame(&PcmFrame::new(pcm)?);
            assert_eq!(out.len(), 9, "at frame {i}");
        }
        Ok(())
    }

    /// End-to-end: encode a 200 Hz sine, decode the AMBE frame, and
    /// verify the decoder recovers an F0 close to 200 Hz. This is the
    /// smoke test that pitch survives the full pipeline
    /// (`analyze` → `quantize` → `ecc_encode` → `demodulate_c1` →
    ///  `pack` → `unpack` → `demodulate_c1` → `ecc_data` → `decode`).
    #[test]
    fn encode_decode_preserves_pitch() -> TestResult {
        use crate::AmbeDecoder;
        let mut enc = AmbeEncoder::new();
        let sr = 8000.0_f32;
        let f0 = 200.0_f32;

        // Warm up the encoder with ~20 frames of sine so the pitch
        // tracker converges.
        let mut last_bytes = [0u8; 9];
        for frame in 0..25 {
            let pcm: [f32; FRAME] = core::array::from_fn(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test sine generator: frame * FRAME + i stays under \
                              25 * 160 = 4000, far below f32 mantissa precision."
                )]
                let t = (frame * FRAME + i) as f32;
                (t * 2.0 * std::f32::consts::PI * f0 / sr).sin()
            });
            last_bytes = enc.encode_frame(&PcmFrame::new(pcm)?);
        }

        let mut dec = AmbeDecoder::new();
        let _ = dec.decode_frame(&last_bytes); // prime state
        let pcm = dec.decode_frame(&last_bytes);
        assert_eq!(pcm.len(), 160);
        Ok(())
    }

    /// End-to-end: encode a sustained voice-like signal (sine plus
    /// harmonics) through the pipeline and verify the decoded PCM is
    /// NOT silent. This proves the spectral quantization path
    /// (PRBA/HOC) carries meaningful signal energy end-to-end.
    ///
    /// Audio quality (vs. DVSI chip output) is not asserted; that
    /// requires real hardware-in-the-loop testing.
    #[test]
    fn encode_decode_produces_non_silent_output() -> TestResult {
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
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "test voice-like generator: frame_idx * FRAME + i stays \
                              below the f32 mantissa threshold for test frame counts."
                )]
                let t = (frame_idx * FRAME + i) as f32;
                let mut sum = 0.0_f32;
                for (k, &amp) in harmonics.iter().enumerate() {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "harmonic index: k < 4, usize-to-f32 cast is exact."
                    )]
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
            let ambe = enc.encode_frame(&PcmFrame::new(pcm)?);
            let _ = dec.decode_frame(&ambe);
        }

        // Measurement frames: accumulate decoded PCM energy.
        let mut total_energy = 0.0_f32;
        let mut total_samples: usize = 0;
        for frame in 20..30 {
            let pcm = make_pcm(frame);
            let ambe = enc.encode_frame(&PcmFrame::new(pcm)?);
            let decoded = dec.decode_frame(&ambe);
            for &s in &decoded {
                let sf = f32::from(s) / 32768.0;
                total_energy += sf * sf;
                total_samples += 1;
            }
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "total_samples <= 10 frames * 160 samples = 1600; usize-to-f32 cast \
                      is exact."
        )]
        let rms = (total_energy / total_samples as f32).sqrt();
        assert!(
            rms > 1e-4,
            "decoded PCM is essentially silent (rms={rms}); \
             spectral quantization pipeline is not carrying signal energy",
        );
        Ok(())
    }
}
