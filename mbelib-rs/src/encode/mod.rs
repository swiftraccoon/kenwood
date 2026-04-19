// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE 3600×2400 D-STAR encoder (feature-gated).
//!
//! Scope: D-STAR only. This encoder produces 72-bit AMBE frames
//! compatible with the AMBE chips inside real D-STAR radios (DVSI
//! AMBE-2020 / AMBE-3000 / AMBE-3003). The algorithmic structure is a
//! Rust port of Max H. Parke's (KA1RBI) `ambe_encoder.cc` from OP25,
//! which chains Pavel Yazev's IMBE analyzer (OP25 `imbe_vocoder`, 2009,
//! GPLv3) with AMBE-specific parameter requantization against the
//! szechyjs mbelib codebooks we already ship in [`crate::tables`].
//!
//! # What this module contains (P1 scope)
//!
//! P1 is only the *packing* layer — given a 72-bit FEC-codeword array
//! `ambe_fr` (same layout [`crate::unpack`] produces after error
//! correction), produce the 9-byte wire frame. It is the exact
//! algorithmic inverse of [`crate::unpack::unpack_frame`] plus
//! [`crate::unpack::demodulate_c1`].
//!
//! The encode pipeline layers on top of this in later phases:
//!
//! | Phase | Adds | Status |
//! |------:|------|--------|
//! | P1    | bit pack + interleave + C1 XOR (this module) | present |
//! | P2    | FFT front-end, windowing | TODO |
//! | P3    | pitch estimation (`pitch_est.cc` / `pitch_ref.cc`) | TODO |
//! | P4    | V/UV + spectral amplitudes + quantization | TODO |
//! | P5    | MBE→AMBE parameter remap + `AmbeEncoder` wrapper | TODO |
//! | P6    | chip interop tuning | TODO |
//!
//! # IP status
//!
//! All patents on the D-STAR AMBE 3600×2400 variant expired in 2017.
//! The later AMBE+2 "half-rate" variant (DMR / YSF / NXDN) remains
//! covered by US 8,359,197 B2 (DVSI, active until 2028-05-20). This
//! module deliberately does **not** implement AMBE+2; a `set_49bit_mode`
//! equivalent is explicitly out of scope.

// P2 front-end (DC removal, LPF, window, FFT) + P1 packer. Public
// items are exposed so operators can experiment with the in-progress
// pipeline; they WILL be wrapped inside a stable `AmbeEncoder` in P5.
// Until then, `mbelib_rs::EncoderBuffers` + `mbelib_rs::FftPlan` +
// `mbelib_rs::analyze_frame` + `mbelib_rs::pack_frame` form the
// piecemeal API.

mod analyze;
mod dc_rmv;
mod encoder;
mod interleave;
mod pack;
mod pe_lpf;
mod pitch;
mod quantize;
mod spectral;
mod state;
mod vuv;
mod window;

pub use analyze::{FftPlan, analyze_frame};
pub use encoder::AmbeEncoder;
pub use pack::pack_frame;
pub use pitch::{PitchEstimate, PitchTracker};
pub use spectral::{MAX_HARMONICS, SpectralAmplitudes, extract_spectral_amplitudes};
pub use state::EncoderBuffers;
pub use vuv::{MAX_BANDS, VuvDecisions, detect_vuv};

/// Validation-only exposure of the internal quantize pipeline.
///
/// External diagnostic tools can feed known-good OP25 reference
/// inputs into [`validation::quantize`] and compare the resulting
/// `b[0..8]` bit assignments against OP25's dumped values. This is
/// how the April 2026 stage-by-stage investigation against the
/// reference implementation was performed.
pub mod validation {
    pub use super::quantize::{PrevFrameState, QuantizeOutcome, quantize};
}
