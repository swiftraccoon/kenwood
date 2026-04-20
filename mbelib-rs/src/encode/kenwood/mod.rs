// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Reference data extracted from a lawfully-owned TH-D75 radio's
// firmware under DMCA §1201(f) (interoperability). Every symbol in
// this module is a numerical constant — no compiled Kenwood code or
// algorithm logic is imported. The clean-room JMBE/mbelib-derived
// algorithms in the rest of this crate are the code; these constants
// are referenced only at the specific A/B-test swap points gated by
// the `kenwood-tables` cargo feature.
//
// Source: TH-D75 firmware image (`TH-D75_V103_e.exe` as distributed
// by Kenwood), specifically the `DATA_00E0` DSP section carrying the
// AMBE module tagged `E5210  AMBE DSHP1.00.01`. Extraction is done
// in a separate (unpublished) tree; the tables here are the
// generated output.

//! Kenwood TH-D75 AMBE DSP reference tables.
//!
//! Every `pub const` in this module was lifted from the TH-D75
//! firmware image with its source DSP virtual address preserved in
//! a doc comment. The tables are here so the encoder pipeline can
//! optionally swap its clean-room constants for Kenwood's when
//! measuring against radio-captured AMBE frames.
//!
//! Gated behind `feature = "kenwood-tables"`; disabled by default.
//!
//! # Catalogue
//!
//! | Symbol                       | Source VA   | Role                                 |
//! | ---------------------------- | ----------- | ------------------------------------ |
//! | [`biquads::BIQUAD_BANK_A`]–`K` | 0x1183B70C+ | Pitch-analysis + anti-alias filters |
//! | [`biquads::HPF_345HZ_COEFFS`] | 0x1183C670  | 345 Hz DC-blocker / rumble removal   |
//! | [`support::SYMMETRIC_FIR_13TAP`] | 0x1183C958 | Unity-gain symmetric FIR           |
//! | [`support::STRUCTURED_COSINE_TABLE`] | 0x1183CE00 | Table containing `cos(π/16)`  |
//! | [`support::RISING_COSINE_CURVE`] | 0x1183D244 | Bandwidth-expansion gain curve    |
//! | [`support::BIQUAD_PATTERN_TWICE`] | 0x1183D2C4 | 7-tap IIR biquad, duplicated     |
//! | [`support::HARMONIC_DECAY`]  | 0x1183D458  | `1/(1+0.15k)` harmonic weights       |
//! | [`support::MATH_CONSTANTS_LUT`] | 0x1183EC6C | ln(2), ln(10), 0.5, 1.0, 1/6, ...  |
//! | [`support::ENVELOPE_WEIGHTS`] | 0x80049044  | 103-entry per-freq postfilter weights|
//! | [`interleaver::BLOCK_INTERLEAVER`] | 0x1183A95A | 24×27 permutation              |
//! | [`inline_codebooks`] `FN_*` | 0x11800000+ | 15 Q-format MVK-immediate sequences  |
//!
//! The inline codebook sequences come from scanning DSP sec-1
//! disassembly for runs of `MVK`+`MVKLH` instruction pairs inside
//! each function. Each `FN_xxxxxxxx` slice is the ordered list of
//! 16-bit immediates loaded into registers during one function's
//! execution; for code paths that walk a compile-time-constant
//! quantizer codebook, these immediates ARE that codebook. Which
//! function holds which AMBE codebook (W0/L table, PRBA24/58, HOC
//! banks) still needs per-function decompilation to resolve.
//!
//! # Provenance
//!
//! The full extraction audit — DSP section map, pointer-target
//! analysis, biquad bank labeling, inline-codebook disassembly — is
//! kept in a separate working tree outside this repo. The tables
//! here are the committed output; the raw firmware dump and
//! Python-based extraction scripts that produced them are not
//! redistributed.

#![allow(
    clippy::excessive_precision,
    reason = "f32 literals carry the full 32-bit pattern from the firmware image; \
              truncating them would silently break the bit-for-bit match."
)]
#![allow(
    clippy::approx_constant,
    reason = "some values in MATH_CONSTANTS_LUT are approximations of π, ln(2), ln(10); \
              they match what the DSP uses, not the mathematical exact values."
)]
#![allow(
    clippy::unreadable_literal,
    reason = "tables are machine-generated from the firmware image; adding digit \
              separators by hand risks transcription errors and would diverge from \
              the canonical dump produced by scripts/15_gen_rust_tables.py."
)]

pub mod biquads;
pub(crate) mod filter;
pub mod inline_codebooks;
pub mod interleaver;
pub mod support;

/// Firmware version string embedded in the DSP blob.
pub const SOURCE_VERSION: &str = "E5210  AMBE DSHP1.00.01";

/// DSP L2 SRAM base address (TMS320C6748 on OMAP-L138). Used as the
/// reference for every source-VA annotation in this module.
pub const DSP_L2_BASE: u32 = 0x1180_0000;
