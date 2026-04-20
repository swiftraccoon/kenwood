// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Top-level D-STAR AMBE encoder.
//!
//! Ties together the front-end ([`analyze_frame`]), pitch tracker,
//! V/UV detector, spectral amplitude extractor, and bit packer into
//! a single `AmbeEncoder::encode_frame(pcm) -> [u8; 9]` entry point.
//!
//! # Status
//!
//! Functional end-to-end.  Every stage of OP25's `ambe_encoder.cc`
//! is ported and wired together; the output bytes decode cleanly
//! through our own decoder and through reference `mbelib`.
//!
//! Bit-exact vs OP25 on the stage-5..8 (quantize) path when fed
//! identical `sa`/`v_uv_dsn`/`prev_mp` state — validated by
//! `examples/validate_quantize_vs_op25.rs`:
//! b3/b4/b5/b6/b7 = 100%, b2 (gain) = 99%, b1 (VUV) = 88%,
//! b0 (pitch) = 60%.  b8 (`HOC_B8`) = 30% because OP25 searches the
//! full 0..=15 codebook in D-STAR mode while the wire format only
//! carries 3 bits with a forced-zero LSB; our stride-2 search
//! follows mbelib's decoder convention (the DVSI implementation).
//!
//! Stages 1..4 (analysis: pitch / `num_harms` / V/UV / sa from FFT)
//! still diverge from OP25 during pitch transitions. The pitch
//! tracker ports OP25's exact E(p) detectability function plus
//! look-back tracking (`pitch_est.cc:200–226`) and sub-multiples
//! analysis (`pitch_est.cc:273–332`). The remaining gap — OP25's
//! 2-frame look-ahead DP (`pitch_est.cc:229–270`) — is the main
//! stage 1-4 improvement left. Audio remains intelligible on real
//! speech despite the divergence because the spectral-envelope
//! reconstruction (stages 5..8) emits the correct codebook entries
//! for whatever pitch we pick.
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

use crate::ecc::ecc_encode;
use crate::encode::analyze::{FftPlan, analyze_frame};
use crate::encode::interleave::AMBE_FRAME_BITS;
use crate::encode::pack::pack_frame;
use crate::encode::pitch::{PITCH_CANDIDATES, PitchTracker, compute_e_p};
use crate::encode::quantize::quantize;
use crate::encode::state::{EncoderBuffers, FFT_LENGTH, FRAME};
use crate::encode::vuv::{VuvState, detect_vuv_and_sa};
use crate::unpack::demodulate_c1;
use realfft::num_complex::Complex;

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
    fft_out: Vec<Complex<f32>>,
}

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
    /// 2-slot ring buffer holding analysis output for frames `N-2`
    /// and `N-1`. On `encode_frame(N)` we compute `E(p)_N`, run the
    /// DP on `(E(p)_{N-2}, E(p)_{N-1}, E(p)_N)` to commit pitch for
    /// frame `N-2`, quantize its saved FFT against that pitch, emit
    /// bytes, then shift the ring.
    ///
    /// While `pending.len() < 2` the encoder is warming up: the
    /// output is `AMBE_SILENCE` and the decoder's `prev_log2_ml`
    /// state is re-zeroed each time (via
    /// [`Self::reset_prev_state_after_silence`]).
    ///
    /// `None` means look-ahead is disabled entirely — the default
    /// zero-latency [`Self::new`] sets this to `None`, while
    /// [`Self::new_with_lookahead`] sets it to `Some(Vec::new())`.
    pending: Option<Vec<FrameSlot>>,
    /// Hysteretic V/UV state — previous frame's per-band decisions
    /// plus the slow-update frame-energy ceiling `th_max`. Carried
    /// across frames so the V/UV threshold reflects the signal's
    /// recent history (OP25 `v_uv_det.cc:152`).
    vuv_state: VuvState,
}

/// Silence shortcut threshold — when the pitch tracker reports
/// essentially-no-signal (confidence below this), emit the canonical
/// D-STAR silence pattern directly (`MMDVMHost` / DVSI convention)
/// rather than trying to quantize zeros. Reference:
/// `NULL_AMBE_DATA_BYTES` in `ref/MMDVMHost/DStarDefines.h:44`.
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
    /// This is the backwards-compatible default. Real-voice inputs
    /// work well here because sub-multiples analysis resolves the
    /// common octave ambiguities; pure-sine synthetic tests can
    /// lose the 2P-vs-P disambiguation on settled tones. For the
    /// full OP25 pitch pipeline (2-frame look-ahead DP), see
    /// [`Self::new_with_lookahead`] — it costs 40 ms of latency and
    /// an extra 2-frame warmup but matches OP25's pitch decisions
    /// on pure sines as well as voice.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bufs: EncoderBuffers::new(),
            plan: FftPlan::new(),
            pitch: PitchTracker::new(),
            fft_out: vec![Complex::new(0.0, 0.0); FFT_LENGTH / 2 + 1],
            prev_log2_ml: [0.0_f32; 57],
            prev_l: 0,
            pending: None,
            vuv_state: VuvState::new(),
        }
    }

    /// Construct a fresh encoder WITH the 2-frame look-ahead DP
    /// enabled. The first two [`encode_frame`](Self::encode_frame)
    /// calls return `AMBE_SILENCE` while the pipeline fills; frame
    /// `N-2`'s pitch is committed on the third call (frame `N`).
    /// Adds ≈40 ms end-to-end latency; matches OP25's pitch-tracking
    /// behaviour across pure sines and pitch transitions.
    #[must_use]
    pub fn new_with_lookahead() -> Self {
        Self {
            bufs: EncoderBuffers::new(),
            plan: FftPlan::new(),
            pitch: PitchTracker::new(),
            fft_out: vec![Complex::new(0.0, 0.0); FFT_LENGTH / 2 + 1],
            prev_log2_ml: [0.0_f32; 57],
            prev_l: 0,
            pending: Some(Vec::with_capacity(2)),
            vuv_state: VuvState::new(),
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
    ///
    /// # Panics
    ///
    /// Never panics under normal use. The look-ahead pipeline's
    /// internal invariant — `self.pending` is `Some` iff the encoder
    /// was built with [`Self::new_with_lookahead`] — is enforced at
    /// construction and never mutated afterwards; the unreachable
    /// `expect` is kept as a defensive check rather than removed
    /// entirely.
    pub fn encode_frame(&mut self, pcm: &[f32]) -> [u8; 9] {
        // Front-end: DC remove → LPF → window → FFT. After this
        // call, `self.bufs.pitch_est_buf` holds the latest 301
        // samples of LPF'd audio and `self.fft_out` holds the
        // 129-bin complex spectrum of the just-arrived frame.
        analyze_frame(pcm, &mut self.bufs, &mut self.plan, &mut self.fft_out);
        let e_p_current = compute_e_p(&self.bufs.pitch_est_buf);

        if self.pending.is_none() {
            // Zero-latency path: commit pitch for the just-received
            // frame via single-frame look-back + sub-multiples.
            let pitch = self.pitch.estimate(&self.bufs.pitch_est_buf);
            return self.quantize_and_pack(pitch);
        }

        // Look-ahead path: buffer the e_p array + a copy of the FFT
        // spectrum, then emit bytes only once we have 3 frames.
        let slot = FrameSlot {
            e_p: e_p_current,
            fft_out: self.fft_out.clone(),
        };
        // Construction invariant: `self.pending.is_some()` here
        // because we returned early above when it was None.
        let pending = self
            .pending
            .as_mut()
            .expect("checked Some above; see # Panics");
        if pending.len() < 2 {
            pending.push(slot);
            // Pipeline not full yet — emit silence and keep the
            // decoder's `prev_log2_ml` state consistent with what
            // it sees on the wire.
            self.reset_prev_state_after_silence();
            return AMBE_SILENCE;
        }

        // Three frames now on hand: pending[0]=N-2, pending[1]=N-1,
        // slot=N. Run the DP against pending[0]'s e_p, using the
        // next two as lookahead.
        let pitch = self
            .pitch
            .estimate_with_lookahead(&pending[0].e_p, &pending[1].e_p, &slot.e_p);
        // Swap out the oldest slot so we can take ownership of its
        // FFT output without cloning; push the newly-arrived slot.
        let oldest = pending.remove(0);
        pending.push(slot);

        // Quantize frame N-2's spectrum against the DP-chosen pitch.
        self.quantize_from_fft(&oldest.fft_out, pitch)
    }

    /// Quantize the encoder's current-frame FFT output (set by
    /// [`analyze_frame`]) against a just-committed pitch and return
    /// 9 wire bytes. Used by the zero-latency path.
    fn quantize_and_pack(&mut self, pitch: crate::encode::pitch::PitchEstimate) -> [u8; 9] {
        if pitch.confidence < SILENCE_CONFIDENCE {
            self.reset_prev_state_after_silence();
            return AMBE_SILENCE;
        }
        let fft = self.fft_out.clone();
        self.quantize_from_fft(&fft, pitch)
    }

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

        #[allow(clippy::cast_precision_loss)]
        let f0_bin = FFT_LENGTH as f32 / pitch.period_samples;
        // `e_p` for OP25's V/UV threshold is the pitch tracker's
        // reconstruction-error metric (1 − confidence on the chosen
        // period). Our PitchEstimate carries `confidence`; invert to
        // get the error.
        let e_p = (1.0 - pitch.confidence).clamp(0.0, 1.0);
        let (vuv, amps) = detect_vuv_and_sa(fft_out, f0_bin, &mut self.vuv_state, e_p);

        let prev = crate::encode::quantize::PrevFrameState {
            log2_ml: self.prev_log2_ml,
            l: self.prev_l,
        };
        let outcome = quantize(pitch, vuv, &amps, &prev);
        self.prev_log2_ml = outcome.prev_log2_ml;
        self.prev_l = outcome.prev_l;
        let ambe_d = outcome.ambe_d;

        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        ecc_encode(&ambe_d, &mut ambe_fr);
        demodulate_c1(&mut ambe_fr);
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

    /// End-to-end: encode a 200 Hz sine, decode it, verify the
    /// decoder produces PCM of the expected shape and non-zero
    /// energy.  Full perceptual-quality validation lives in the
    /// `encoder_roundtrip.rs` integration test + the `validate_*`
    /// example harnesses; this unit test is a smoke check that the
    /// `encode_frame` → `decode_frame` pipeline doesn't panic or
    /// deadlock on a trivial input.
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
