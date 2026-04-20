// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder)
// SPDX-FileCopyrightText: 2016 Max H. Parke KA1RBI (OP25 ambe_encoder)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// Algorithmic port from Pavel Yazev's `imbe_vocoder` (OP25, 2009,
// GPLv3). All frame sizes and buffer layouts match that reference.

//! Encoder state buffers and canonical frame dimensions.
//!
//! The IMBE/AMBE encoder carries per-stream state across frames —
//! primarily the 301-sample pitch-estimation history (one past frame
//! plus current) and the 21-tap LPF memory. This module owns those
//! buffers plus the canonical size constants every downstream module
//! depends on.

/// One 20 ms frame at 8 kHz = 160 samples.
pub(crate) const FRAME: usize = 160;

/// Pitch-estimation history buffer length (samples).
///
/// Per Yazev (OP25 `imbe.h`): 301 = frame + past frame + 21-tap LPF
/// look-behind, sized so every pitch-period candidate from 20 to 123
/// samples fits entirely inside the buffer.
pub(crate) const PITCH_EST_BUF_SIZE: usize = 301;

/// Number of taps in the pitch-estimation lowpass FIR filter.
pub(crate) const PE_LPF_ORD: usize = 21;

/// FFT length (samples). The encoder does a single 256-point complex
/// FFT per frame for pitch refinement and spectral analysis.
pub(crate) const FFT_LENGTH: usize = 256;

/// Pitch-refinement half-window length (samples).
///
/// Indices 146..256 read the window ascending, and 1..111 read it
/// descending — total 220-sample overlap that sits centered on the
/// current 160-sample frame inside the 256-point FFT.
pub(crate) const WR_HALF_LEN: usize = 111;

/// Per-stream encoder working buffers.
///
/// One instance per concurrent voice stream. Holds the sliding
/// pitch-estimation history, the LPF delay line, and the DC-removal
/// high-pass filter state. All are zero-initialized; the first frame
/// processed through a fresh `EncoderBuffers` will be quieter than
/// steady-state output while the history fills, but intelligibility
/// is unaffected (the same transient behavior the reference encoder
/// exhibits).
#[derive(Debug)]
pub struct EncoderBuffers {
    /// Pitch estimation sample history (LPF output, wideband-removed).
    pub(crate) pitch_est_buf: [f32; PITCH_EST_BUF_SIZE],
    /// Pitch refinement / spectral analysis sample history
    /// (DC-removed, no LPF).
    pub(crate) pitch_ref_buf: [f32; PITCH_EST_BUF_SIZE],
    /// Pitch-estimation LPF delay line (one sample per tap).
    pub(crate) pe_lpf_mem: [f32; PE_LPF_ORD],
    /// OP25 `dc_rmv` single-pole HPF integrator — default path.
    #[cfg(not(feature = "kenwood-tables"))]
    pub(crate) dc_rmv_mem: f32,
    /// Kenwood 345 Hz biquad HPF delay line — replaces `dc_rmv_mem`
    /// as the active input-conditioning state when the
    /// `kenwood-tables` feature is enabled.
    #[cfg(feature = "kenwood-tables")]
    pub(crate) kenwood_hpf_mem: crate::encode::kenwood::filter::Biquad2State,
}

impl EncoderBuffers {
    /// Fresh state; all buffers zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pitch_est_buf: [0.0; PITCH_EST_BUF_SIZE],
            pitch_ref_buf: [0.0; PITCH_EST_BUF_SIZE],
            pe_lpf_mem: [0.0; PE_LPF_ORD],
            #[cfg(not(feature = "kenwood-tables"))]
            dc_rmv_mem: 0.0,
            #[cfg(feature = "kenwood-tables")]
            kenwood_hpf_mem: crate::encode::kenwood::filter::Biquad2State::new(),
        }
    }

    /// Slide both pitch buffers left by `FRAME` samples to make room
    /// for a new frame at the tail. Mirrors the first loop of Yazev's
    /// `imbe_vocoder::encode()`.
    pub fn shift_pitch_history(&mut self) {
        // Using copy_within to exactly replicate the in-place
        // `buf[i] = buf[i + FRAME]` loop without allocating.
        self.pitch_est_buf.copy_within(FRAME..PITCH_EST_BUF_SIZE, 0);
        self.pitch_ref_buf.copy_within(FRAME..PITCH_EST_BUF_SIZE, 0);
    }

    /// Read-only access to the pitch-estimation history buffer.
    ///
    /// Diagnostic-only: exposed so the `validate_analysis_vs_op25`
    /// example can feed the buffer into [`crate::PitchTracker::estimate`].
    #[must_use]
    #[doc(hidden)]
    pub const fn pitch_est_buf(&self) -> &[f32; PITCH_EST_BUF_SIZE] {
        &self.pitch_est_buf
    }
}

impl Default for EncoderBuffers {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{EncoderBuffers, FRAME, PE_LPF_ORD, PITCH_EST_BUF_SIZE};

    /// Bit-exact zero comparison on f32 is legitimate for freshly
    /// zero-initialized buffers — the values went through no
    /// arithmetic that could introduce rounding.
    fn is_zero(x: f32) -> bool {
        x.to_bits() == 0 || x.to_bits() == (1u32 << 31)
    }

    #[test]
    fn fresh_buffers_are_zero() {
        let b = EncoderBuffers::new();
        assert!(b.pitch_est_buf.iter().all(|&x| is_zero(x)));
        assert!(b.pitch_ref_buf.iter().all(|&x| is_zero(x)));
        assert!(b.pe_lpf_mem.iter().all(|&x| is_zero(x)));
        #[cfg(not(feature = "kenwood-tables"))]
        assert!(is_zero(b.dc_rmv_mem));
    }

    /// After one shift, content at position `p >= FRAME` moves to
    /// position `p - FRAME`. Verifies the buffer slides correctly.
    /// Note that `2 * FRAME > PITCH_EST_BUF_SIZE` so we can't verify
    /// "the last frame lands in the second-to-last frame position"
    /// directly — instead we check the sliding identity.
    #[test]
    fn shift_moves_content_by_frame() {
        let mut b = EncoderBuffers::new();
        // Fill with position-indexed values so we can identify where
        // each sample ends up after the shift.
        #[allow(clippy::cast_precision_loss)]
        for i in 0..PITCH_EST_BUF_SIZE {
            b.pitch_est_buf[i] = i as f32;
            b.pitch_ref_buf[i] = i as f32;
        }
        b.shift_pitch_history();
        // After shift: position `i` should now hold what was at
        // `i + FRAME` (for `i + FRAME < PITCH_EST_BUF_SIZE`).
        for i in 0..(PITCH_EST_BUF_SIZE - FRAME) {
            #[allow(clippy::cast_precision_loss)]
            let expected = (i + FRAME) as f32;
            let got = b.pitch_est_buf[i];
            assert!(
                (got - expected).abs() < f32::EPSILON,
                "pitch_est_buf[{i}] = {got}, expected {expected}",
            );
        }
    }

    #[test]
    fn canonical_sizes_match_reference() {
        // Yazev's `imbe.h` hard-codes these. Downstream DSP assumes
        // them; regressions here would silently corrupt the frame
        // pipeline.
        assert_eq!(FRAME, 160);
        assert_eq!(PITCH_EST_BUF_SIZE, 301);
        assert_eq!(PE_LPF_ORD, 21);
    }
}
