// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// Provenance-anchored integrity tests for the extracted Kenwood AMBE
// DSP constants. These don't validate the encoder's *behaviour* with
// the tables — that requires hardware-in-the-loop captures from the
// radio. They only check that the specific numerical signatures the
// extraction calls out survived the copy into
// `mbelib-rs/src/encode/kenwood/` intact. If any of these break, the
// firmware dump was either miscopied or regenerated from a different
// image than the one originally extracted.

//! Provenance integrity tests for the extracted Kenwood AMBE DSP
//! reference data. See the file-header comment above for context.

#![cfg(feature = "kenwood-tables")]
#![expect(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "Provenance-integrity test file. Directly indexes into extracted Kenwood DSP \
              tables (known exact-size const arrays imported above) to check specific \
              signature values, and uses `.expect()` on optional fields that the extracted \
              firmware data guarantees present. Any bounds violation or missing field \
              would indicate the wrong firmware dump was copied in — the test correctly \
              panics in that case."
)]

// Dev-dependencies pulled in by sibling tests. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use mbelib_rs::kenwood::{
    SOURCE_VERSION,
    biquads::{BIQUAD_BANK_H_HPF, BIQUAD_BANK_I, BIQUAD_BANK_J, HPF_345HZ_COEFFS},
    inline_codebooks::{FN_11801E54, FN_11804A90, FN_11805B48},
    interleaver::BLOCK_INTERLEAVER,
    support::{ENVELOPE_WEIGHTS, HARMONIC_DECAY, MATH_CONSTANTS_LUT},
};

#[test]
fn source_version_matches_findings_doc() {
    assert_eq!(
        SOURCE_VERSION, "E5210  AMBE DSHP1.00.01",
        "firmware tag drifted from the one documented at extraction time"
    );
}

#[test]
fn hpf_coefficients_match_bristow_johnson_form() {
    // Derive the characteristic frequency f₀ from the filter's
    // numerator using the Bristow-Johnson HPF shape
    // `b0 = (1 + cos(ω₀))/2`. The resulting f₀ ≈ 345 Hz is the
    // filter's "design frequency" in the BJ parameterisation — NOT
    // its frequency response's cutoff (Kenwood tuned the poles very
    // close to the zeros, so the actual response is a narrow notch
    // at DC; see [`HPF_345HZ_COEFFS`]'s doc comment for the full
    // story). This test anchors the coefficient drift, not the
    // behaviour.
    let b0 = HPF_345HZ_COEFFS[0];
    let cos_w = b0.mul_add(2.0, -1.0);
    let w = cos_w.acos();
    let f0 = w * 8000.0 / (2.0 * std::f32::consts::PI);
    assert!(
        (f0 - 345.0).abs() < 5.0,
        "BJ-form f₀ drifted: got {f0:.1} Hz, expected ~345 Hz from b0={b0}"
    );
}

#[test]
fn hpf_345hz_is_prefix_of_bank_h_i_j() {
    // Banks H, I, J all start with the same 5-tap HPF preamble (per
    // the extraction notes §3.2). Verify the standalone HPF_345HZ_COEFFS
    // alias hasn't drifted from its source.
    for (bank_name, bank) in [
        ("H_HPF", &BIQUAD_BANK_H_HPF[..]),
        ("I", &BIQUAD_BANK_I[..]),
        ("J", &BIQUAD_BANK_J[..]),
    ] {
        assert_eq!(
            &bank[..5],
            &HPF_345HZ_COEFFS[..],
            "bank {bank_name} first 5 coefficients diverged from HPF_345HZ_COEFFS"
        );
    }
}

#[test]
fn harmonic_decay_is_reciprocal_of_1_plus_015k() {
    // HARMONIC_DECAY[k] should equal 1/(1 + 0.15·k) within f32 rounding.
    for (k, &v) in HARMONIC_DECAY.iter().enumerate() {
        #[expect(
            clippy::cast_precision_loss,
            reason = "HARMONIC_DECAY table index k is bounded by the table's small length; \
                      the usize-to-f32 cast is exact in that range."
        )]
        let expected = 1.0_f32 / 0.15_f32.mul_add(k as f32, 1.0);
        assert!(
            (v - expected).abs() < 1e-4,
            "HARMONIC_DECAY[{k}] = {v}, expected {expected}"
        );
    }
}

#[test]
fn math_lut_contains_ln2_and_ln10() {
    // Per the extraction notes §3.5, slots 6 and 9 are ln(2) and ln(10).
    assert!(
        (MATH_CONSTANTS_LUT[6] - std::f32::consts::LN_2).abs() < 1e-6,
        "MATH_CONSTANTS_LUT[6] ≠ ln(2): got {}",
        MATH_CONSTANTS_LUT[6]
    );
    assert!(
        (MATH_CONSTANTS_LUT[9] - std::f32::consts::LN_10).abs() < 1e-5,
        "MATH_CONSTANTS_LUT[9] ≠ ln(10): got {}",
        MATH_CONSTANTS_LUT[9]
    );
}

#[test]
fn envelope_weights_peak_is_at_index_51() {
    // Shape per the extraction notes: 0.88 → 1.0 peak at index 51, decay to 0.83,
    // sharp drop at 100, zero tail. Assert the peak is exactly at 51.
    let (peak_idx, &peak_val) = ENVELOPE_WEIGHTS
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .expect("non-empty table");
    assert_eq!(
        peak_idx, 51,
        "envelope peak drifted from index 51 to {peak_idx}"
    );
    assert!(
        (peak_val - 1.0).abs() < 1e-4,
        "envelope peak at 51 should be ≈1.0, got {peak_val}"
    );
    assert!(
        ENVELOPE_WEIGHTS[102].abs() < 1e-6,
        "envelope tail at index 102 should be zero, got {}",
        ENVELOPE_WEIGHTS[102]
    );
}

#[test]
fn inline_codebook_sizes_match_extraction_report() {
    // huge_function_classification.txt lists each function's MVK
    // immediate count; the `FN_*` arrays in inline_codebooks.rs
    // should match those counts. If a re-run of scripts 16/22 lands
    // on different sizes, the extraction has drifted.
    assert_eq!(
        FN_11801E54.len(),
        223,
        "0x11801E54 signal-chain runner: expected 223 MVK immediates"
    );
    assert_eq!(
        FN_11804A90.len(),
        138,
        "0x11804A90 huge-fn-4: expected 138 MVK immediates"
    );
    assert_eq!(
        FN_11805B48.len(),
        97,
        "0x11805B48 biquad_bank_G caller: expected 97 MVK immediates"
    );
}

#[test]
fn inline_codebook_11804a90_opens_with_known_q15_values() {
    // FN_11804A90 starts with the Q15 sentinel pattern
    // [1, -8192, 8192, -8192, 3434, 1, 392, 392, ...] per the
    // inline_codebooks_per_function.txt dump. The -8192/+8192 bookends
    // are Q15 representations of -0.25/+0.25 — a common initial
    // state for a codebook search. Verify the first 8 entries to
    // catch any endian / sign extraction bug.
    let expected = [1_i16, -8192, 8192, -8192, 3434, 1, 392, 392];
    assert_eq!(
        &FN_11804A90[..expected.len()],
        &expected,
        "FN_11804A90 prefix diverged from the extraction dump"
    );
}

#[test]
fn block_interleaver_known_positions() {
    // The the extraction notes formula `val[k] = 24·(k%27) + ((k//27) || 24)`
    // matches the first row only; subsequent rows have 28 entries
    // (the 28th being the wrap `24·27 + row_tag`). Rather than
    // reverse-engineer the precise row-layout rule here, spot-check a
    // set of hand-verified positions from the extracted dump. If the
    // extraction script changes, these are the first values that
    // would drift.
    let fixtures = [
        (0_usize, 24_i16), // row 0, col 0
        (1, 48),           // row 0, col 1
        (26, 648),         // row 0, col 26
        (27, 1),           // row 1 starts with 1
        (28, 25),          // row 1, col 1
        (54, 649),         // row 1, col 27 (= 24·27 + 1)
        (55, 2),           // row 2 starts with 2
    ];
    for (k, expected) in fixtures {
        assert_eq!(
            BLOCK_INTERLEAVER[k], expected,
            "interleaver[{k}] drifted: got {}, expected {expected}",
            BLOCK_INTERLEAVER[k]
        );
    }
}
