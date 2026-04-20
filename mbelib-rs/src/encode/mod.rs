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
//! # Phase status
//!
//! | Phase | Adds | Status |
//! |------:|------|--------|
//! | P1    | bit pack + interleave + C1 XOR ([`pack_frame`]) | done |
//! | P2    | DC removal, LPF, window, FFT ([`analyze_frame`]) | done |
//! | P3    | pitch estimation (sub-harmonic summation port of `pitch_est.cc`) | done (single-frame; multi-frame DP deferred) |
//! | P4    | V/UV + spectral amplitudes + gain quantization ([`detect_vuv`], [`extract_spectral_amplitudes`], `quantize`) | done |
//! | P5    | PRBA/HOC codebook search, FEC, `AmbeEncoder` wrapper | done (bit-exact vs OP25 on b3..b7) |
//! | P6    | chip-interop tuning | partial (see [`encoder`]'s status block) |
//!
//! The top-level entry point is [`AmbeEncoder::encode_frame`]. The
//! individual stage functions (`analyze_frame`, `PitchTracker`,
//! `detect_vuv`, `extract_spectral_amplitudes`) remain public so
//! diagnostic tooling (`examples/validate_*.rs`) can feed known-good
//! inputs at each stage boundary.
//!
//! # IP status
//!
//! All patents on the D-STAR AMBE 3600×2400 variant expired in 2017.
//! The later AMBE+2 "half-rate" variant (DMR / YSF / NXDN) remains
//! covered by US 8,359,197 B2 (DVSI, active until 2028-05-20). This
//! module deliberately does **not** implement AMBE+2; a `set_49bit_mode`
//! equivalent is explicitly out of scope.

mod analyze;
#[cfg(not(feature = "kenwood-tables"))]
mod dc_rmv;
mod encoder;
mod interleave;
mod pack;
mod pe_lpf;
mod pitch;
mod pitch_quant;
mod quantize;
mod spectral;
mod state;
mod vuv;
mod window;
mod wr_sp;

#[cfg(feature = "kenwood-tables")]
pub mod kenwood;

pub use analyze::{FftPlan, analyze_frame};
pub use encoder::AmbeEncoder;
pub use pack::pack_frame;
pub use pitch::{PitchEstimate, PitchTracker, compute_e_p};
pub use spectral::{MAX_HARMONICS, SpectralAmplitudes, extract_spectral_amplitudes};
pub use state::EncoderBuffers;
pub use vuv::{MAX_BANDS, VuvDecisions, VuvState, detect_vuv, detect_vuv_and_sa};

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
