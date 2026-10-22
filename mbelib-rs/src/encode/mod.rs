// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE 3600×2400 D-STAR encoder (feature-gated).
//!
//! Scope: D-STAR only. This experimental encoder produces 72-bit AMBE
//! frames intended for the AMBE chips inside real D-STAR radios (DVSI
//! AMBE-2020 / AMBE-3000 / AMBE-3003), but reliable hardware
//! interoperability is not yet validated. The algorithmic structure is a
//! Rust port of Max H. Parke's (KA1RBI) `ambe_encoder.cc` from OP25,
//! which chains Pavel Yazev's IMBE analyzer (OP25 `imbe_vocoder`, 2009,
//! GPLv3) with AMBE-specific parameter requantization against the
//! szechyjs mbelib codebooks shipped in [`crate::tables`].
//!
//! The top-level entry point is [`AmbeEncoder::encode_frame`]. The
//! individual stage functions (`analyze_frame`, `PitchTracker`,
//! `detect_vuv`, `extract_spectral_amplitudes`) remain public so
//! diagnostic tooling (`examples/validate_*.rs`) can feed known-good
//! inputs at each stage boundary. Constructors check sample, spectrum,
//! pitch, count, padding and predictor domains before numerical work.

pub use analyze::{FftPlan, analyze_frame};
pub use encoder::{AmbeEncoder, EncoderOptions, PitchTracking};
pub use pack::pack_frame;
pub use pitch::{PitchEstimate, PitchTracker, compute_e_p};
pub use spectral::{MAX_HARMONICS, SpectralAmplitudes, extract_spectral_amplitudes};
pub use stage::{
    CodewordBits, FundamentalBin, PITCH_ERROR_CANDIDATES, PITCH_HISTORY_SAMPLES, PitchErrors,
    PitchHistory, PitchMetric, SPECTRUM_BINS, Spectrum, StageError, StageField,
};
pub use state::{EncoderBuffers, InputFilter};
pub use vuv::{MAX_BANDS, VuvDecisions, VuvState, detect_vuv, detect_vuv_and_sa};

mod analyze;
mod dc_rmv;
mod encoder;
mod interleave;
mod pack;
mod pe_lpf;
mod pitch;
mod pitch_quant;
mod quantize;
mod spectral;
mod stage;
mod state;
mod vuv;
mod window;
mod wr_sp;

#[cfg(feature = "kenwood-tables")]
pub mod kenwood;

/// Checked quantizer inputs and inspection results for diagnostic tools.
///
/// External diagnostic tools can feed known-good OP25 reference
/// inputs into [`validation::quantize`] and compare the resulting
/// `b[0..8]` bit assignments against OP25's dumped values. The candidate
/// constructors and fallible stage enforce the same documented domains as
/// the other public encoder stages.
pub mod validation {
    pub use super::quantize::{PrevFrameState, QuantizeOutcome, quantize};
}
