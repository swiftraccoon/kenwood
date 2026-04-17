// SPDX-FileCopyrightText: 2010 szechyjs (mbelib)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Error correction coding for AMBE 3600×2450 voice frames.
//!
//! AMBE 3600×2450 uses **Golay(23,12)** — a 3-error-correcting code —
//! on the C0 and C1 codewords. C0 carries the fundamental frequency
//! index (the most perceptually critical parameter), so it receives
//! the strongest protection. C1 carries the PRBA spectral coefficients.
//!
//! C2 (11 bits) and C3 (14 bits) are unprotected: they carry data bits
//! directly with no ECC. The codec accepts this reduced protection
//! because C3 carries higher-order coefficients and gain LSBs that are
//! less perceptually critical than the C0 pitch index.
//!
//! (Some related codec variants apply Hamming(15,11) to a C4 codeword,
//! but AMBE 3600×2450 has no C4; this crate does not implement Hamming.)
//!
//! After error correction, `ecc_data` packs the corrected data bits
//! from all four codewords (C0–C3) into a 49-element parameter vector
//! (`ambe_d`) that feeds the parameter decoder.
//!
//! # Flat Frame Layout
//!
//! The `ambe_fr` array uses one byte per bit (0 or 1), laid out as:
//!
//! | Codeword | Indices  | Bits | Protection  |
//! |----------|----------|------|-------------|
//! | C0       | 0..24    | 24   | Golay(23,12) + 1 parity |
//! | C1       | 24..47   | 23   | Golay(23,12) |
//! | C2       | 47..58   | 11   | None (data only) |
//! | C3       | 58..72   | 14   | None (copied verbatim) |
//!
//! # Reference
//!
//! Ported from mbelib `ecc.c` and `ambe3600x2450.c`
//! (<https://github.com/szechyjs/mbelib>), ISC license.

use crate::tables;
use crate::unpack::AMBE_FRAME_BITS;

/// Number of data bits extracted after ECC decoding.
///
/// The 49-bit parameter vector carries all decoded speech model
/// parameters: 12 bits from C0 (fundamental frequency, partial VUV),
/// 12 bits from C1 (PRBA coefficients), 11 bits from C2 (higher-order
/// coefficients), and 14 bits from C3 (remaining HOC + gain LSBs).
pub(crate) const AMBE_DATA_BITS: usize = 49;

/// Offset of C0 codeword in the flat `ambe_fr` array.
const C0_OFFSET: usize = 0;
/// Number of bits in C0 (24: 1 parity + 23 Golay).
const C0_LEN: usize = 24;

/// Offset of C1 codeword in the flat `ambe_fr` array.
const C1_OFFSET: usize = C0_OFFSET + C0_LEN; // 24
/// Number of bits in C1 (23 Golay-protected).
const C1_LEN: usize = 23;

/// Offset of C2 codeword in the flat `ambe_fr` array.
const C2_OFFSET: usize = C1_OFFSET + C1_LEN; // 47
/// Number of bits in C2 (11 unprotected data bits).
const C2_LEN: usize = 11;

/// Offset of C3 codeword in the flat `ambe_fr` array.
const C3_OFFSET: usize = C2_OFFSET + C2_LEN; // 58
/// Number of bits in C3 (14 unprotected data bits).
const C3_LEN: usize = 14;

/// Decodes a Golay(23,12) codeword, correcting up to 3 bit errors.
///
/// The Golay(23,12) code encodes 12 data bits with 11 parity bits,
/// yielding a 23-bit codeword. It can detect and correct any pattern
/// of up to 3 bit errors in the 23-bit block.
///
/// # Algorithm
///
/// 1. Recompute the expected 11-bit parity from the received 12 data
///    bits using the generator polynomial coefficients.
/// 2. XOR the expected parity with the received parity to form the
///    11-bit syndrome. A zero syndrome means no detectable errors.
/// 3. Look up the syndrome in the 2048-entry correction matrix to get
///    a 12-bit error pattern for the data bits.
/// 4. XOR the error pattern with the received data bits to correct them.
///
/// # Arguments
///
/// - `in_bits`: 23-element array of single-bit bytes (indices 0..23),
///   ordered LSB-first (bit 0 at index 0). This matches the mbelib C
///   convention where `in[0]` is the least-significant bit.
///
/// # Returns
///
/// A tuple of:
/// - `out_bits`: 23-element corrected bit array (same ordering as input).
///   Indices 11..23 contain the corrected 12 data bits; indices 0..11
///   retain the original parity bits (uncorrected, as Golay correction
///   targets the data bits only).
/// - Error count: the number of data bits that were corrected.
fn golay_decode(in_bits: &[u8; 23]) -> ([u8; 23], u32) {
    // Step 1: Pack the 23 input bits into a single integer.
    // The C code packs LSB-first: in[0] is bit 0, in[22] is bit 22.
    let mut block: i64 = 0;
    let mut i: usize = 22;
    loop {
        block <<= 1;
        // Safe indexing: i is always 0..=22 which is within bounds of a 23-element array.
        block += i64::from(*in_bits.get(i).unwrap_or(&0));
        if i == 0 {
            break;
        }
        i -= 1;
    }

    // Step 2: Compute the expected parity from the 12 data bits (bits 11..22).
    // Each generator entry corresponds to one data bit position; if that
    // bit is set, XOR its generator value into the accumulated parity.
    let mut mask: i64 = 0x0040_0000; // bit 22
    let mut ecc_expected: i32 = 0;
    let mut gi: usize = 0;
    while gi < 12 {
        if (block & mask) != 0 {
            ecc_expected ^= *tables::GOLAY_GENERATOR.get(gi).unwrap_or(&0);
        }
        mask >>= 1;
        gi += 1;
    }

    // Step 3: Extract the received 11-bit parity (bits 0..10) and compute
    // the syndrome. A non-zero syndrome indicates detectable errors.
    // Safe cast: masking with 0x7FF limits the value to 11 bits, which
    // fits in i32 without truncation.
    let ecc_bits = (block & 0x7FF) as i32;
    let syndrome = ecc_expected ^ ecc_bits;

    // Step 4: Look up the correction pattern for the 12 data bits.
    // The matrix is indexed by the 11-bit syndrome value (0..2048).
    #[expect(
        clippy::cast_sign_loss,
        reason = "syndrome is the XOR of two 11-bit values, always non-negative in practice"
    )]
    let correction = *tables::GOLAY_MATRIX.get(syndrome as usize).unwrap_or(&0);

    // Step 5: Extract the 12 data bits (bits 11..22) and apply the
    // correction pattern via XOR.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "right-shifting by 11 and original value is 23 bits, result fits in i32"
    )]
    let databits_raw = (block >> 11) as i32;
    let databits = databits_raw ^ correction;

    // Step 6: Unpack the corrected data bits back into the output array.
    // Indices 11..22 get the corrected data bits (MSB first from the
    // packed integer); indices 0..10 retain the original parity bits.
    let mut out_bits = [0u8; 23];
    let mut unpack = databits;
    let mut oi: usize = 22;
    loop {
        if oi < 11 {
            break;
        }
        if let Some(slot) = out_bits.get_mut(oi) {
            #[expect(
                clippy::cast_sign_loss,
                reason = "masking with 0x800 then shifting right by 11 always yields 0 or 1"
            )]
            {
                *slot = ((unpack & 2048) >> 11) as u8;
            }
        }
        unpack <<= 1;
        if oi == 11 {
            break;
        }
        oi -= 1;
    }

    // Indices 0..10 keep the original (uncorrected) parity bits, matching
    // the C reference behavior.
    let mut pi: usize = 0;
    while pi <= 10 {
        if let Some(slot) = out_bits.get_mut(pi) {
            *slot = *in_bits.get(pi).unwrap_or(&0);
        }
        pi += 1;
    }

    // Step 7: Count how many data bits were corrected by comparing the
    // output data bits (indices 11..22) against the input.
    let mut errs: u32 = 0;
    let mut ci: usize = 22;
    loop {
        if ci < 11 {
            break;
        }
        let out_val = *out_bits.get(ci).unwrap_or(&0);
        let in_val = *in_bits.get(ci).unwrap_or(&0);
        if out_val != in_val {
            errs += 1;
        }
        if ci == 11 {
            break;
        }
        ci -= 1;
    }

    (out_bits, errs)
}

/// Applies Golay(23,12) error correction to the C0 codeword in the
/// AMBE frame.
///
/// C0 carries the fundamental frequency index (b0), which is the most
/// perceptually critical parameter — even a 1-bit error can shift the
/// decoded pitch drastically. The 24-bit C0 consists of 1 overall
/// parity bit at index 0, followed by 23 Golay-encoded bits at
/// indices 1..24. This function corrects the 23 Golay bits in place.
///
/// The overall parity bit (`ambe_fr[0]`) is not checked by the
/// reference implementation (noted as a TODO in mbelib).
///
/// # Returns
///
/// The number of data bits that were corrected (0 to 3 for valid
/// Golay corrections).
pub(crate) fn ecc_c0(ambe_fr: &mut [u8; AMBE_FRAME_BITS]) -> u32 {
    // Copy C0 bits [1..24] into a 23-element buffer for Golay decoding.
    // Index 0 of C0 is the overall parity bit, which is skipped here
    // (matching the C reference's `ambe_fr[0][j + 1]` offset).
    let mut golay_in = [0u8; 23];
    let mut j: usize = 0;
    while j < 23 {
        if let Some(val) = ambe_fr.get(C0_OFFSET + j + 1)
            && let Some(slot) = golay_in.get_mut(j)
        {
            *slot = *val;
        }
        j += 1;
    }

    let (golay_out, errs) = golay_decode(&golay_in);

    // Write the corrected bits back into the frame.
    let mut j2: usize = 0;
    while j2 < 23 {
        if let Some(val) = golay_out.get(j2)
            && let Some(slot) = ambe_fr.get_mut(C0_OFFSET + j2 + 1)
        {
            *slot = *val;
        }
        j2 += 1;
    }

    errs
}

/// Decodes all data codewords and assembles the 49-bit AMBE parameter
/// vector.
///
/// This function performs three operations:
///
/// 1. **C0 data extraction** — copies the 12 data bits from the
///    already-corrected C0 codeword (bits 12..23, MSB first).
/// 2. **C1 Golay correction** — applies Golay(23,12) ECC to C1 and
///    extracts the 12 corrected data bits (MSB first).
/// 3. **C2/C3 passthrough** — copies C2 (11 bits) and C3 (14 bits)
///    verbatim, as the reference implementation does not apply any
///    FEC to these shorter codewords.
///
/// The output `ambe_d` is the 49-bit parameter vector consumed by
/// `decode_params()`:
///
/// | `ambe_d` indices | Source     | Bits |
/// |------------------|------------|------|
/// | 0..12            | C0[23..12] | 12   |
/// | 12..24           | C1[22..11] | 12   |
/// | 24..35           | C2[10..0]  | 11   |
/// | 35..49           | C3[13..0]  | 14   |
///
/// # Returns
///
/// The total number of bit errors detected and corrected across all
/// codewords (only C1 contributes, since C0 was corrected earlier and
/// C2/C3 have no FEC).
pub(crate) fn ecc_data(ambe_fr: &[u8; AMBE_FRAME_BITS], ambe_d: &mut [u8; AMBE_DATA_BITS]) -> u32 {
    let mut d_idx: usize = 0;

    // --- C0: copy data bits (already Golay-corrected by ecc_c0) ---
    // C0 data bits are at frame indices C0_OFFSET+12 through C0_OFFSET+23
    // (the upper 12 of the 23 Golay bits, plus index 0 is the parity bit,
    // so data starts at frame index 12+1=13). In the flat layout with the
    // +1 parity offset, data bits are at ambe_fr[C0_OFFSET + 12 + 1]
    // through ambe_fr[C0_OFFSET + 23 + 1 - 1], i.e., indices 13..24.
    //
    // The C code iterates `for (j = 23; j > 11; j--)` on ambe_fr[0][j],
    // which maps to flat indices C0_OFFSET + 23 down to C0_OFFSET + 12.
    // But remember the C0 has a +1 offset in the flat layout: the Golay
    // bits are at flat[1..24], so data bits 12..23 of the Golay word
    // correspond to flat indices 13..24.
    //
    // Matching the C: j goes from 23 down to 12 (exclusive of 11).
    let mut j: usize = C0_LEN - 1; // 23
    loop {
        if j <= 11 {
            break;
        }
        // ambe_fr[0][j] in C maps to ambe_fr[C0_OFFSET + j] in flat layout
        if let Some(slot) = ambe_d.get_mut(d_idx) {
            *slot = *ambe_fr.get(C0_OFFSET + j).unwrap_or(&0);
        }
        d_idx += 1;
        j -= 1;
    }

    // --- C1: Golay ECC and extract data bits ---
    // C1 occupies ambe_fr[C1_OFFSET .. C1_OFFSET + 23].
    let mut golay_in = [0u8; 23];
    let mut gi: usize = 0;
    while gi < C1_LEN {
        if let Some(slot) = golay_in.get_mut(gi) {
            *slot = *ambe_fr.get(C1_OFFSET + gi).unwrap_or(&0);
        }
        gi += 1;
    }

    let (golay_out, errs) = golay_decode(&golay_in);

    // Extract the 12 data bits from the corrected C1 codeword.
    // The C code iterates `for (j = 22; j > 10; j--)` on gout[j].
    let mut cj: usize = 22;
    loop {
        if cj <= 10 {
            break;
        }
        if let Some(slot) = ambe_d.get_mut(d_idx) {
            *slot = *golay_out.get(cj).unwrap_or(&0);
        }
        d_idx += 1;
        cj -= 1;
    }

    // --- C2: copy 11 bits verbatim (no FEC) ---
    // The C code iterates `for (j = 10; j >= 0; j--)` on ambe_fr[2][j].
    let mut c2j: usize = C2_LEN; // start at 11, will decrement before use
    loop {
        if c2j == 0 {
            break;
        }
        c2j -= 1;
        if let Some(slot) = ambe_d.get_mut(d_idx) {
            *slot = *ambe_fr.get(C2_OFFSET + c2j).unwrap_or(&0);
        }
        d_idx += 1;
    }

    // --- C3: copy 14 bits verbatim (no FEC) ---
    // The C code iterates `for (j = 13; j >= 0; j--)` on ambe_fr[3][j].
    let mut c3j: usize = C3_LEN; // start at 14, will decrement before use
    loop {
        if c3j == 0 {
            break;
        }
        c3j -= 1;
        if let Some(slot) = ambe_d.get_mut(d_idx) {
            *slot = *ambe_fr.get(C3_OFFSET + c3j).unwrap_or(&0);
        }
        d_idx += 1;
    }

    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Extracts a single bit from an i32 at the given position as a u8.
    ///
    /// Returns 0 or 1. Used throughout tests to unpack codewords into
    /// bit arrays without triggering `cast_sign_loss` on every line.
    #[expect(
        clippy::cast_sign_loss,
        reason = "masking with 1 after right-shift always yields 0 or 1, never negative"
    )]
    fn bit_at(value: i32, pos: usize) -> u8 {
        ((value >> pos) & 1) as u8
    }

    /// Verifies that a zero-error Golay codeword passes through unchanged.
    ///
    /// Constructs a valid Golay(23,12) codeword by encoding a known
    /// 12-bit data pattern and computing the correct 11-bit parity,
    /// then confirms the decoder returns 0 errors and identical data bits.
    #[test]
    fn golay_zero_error_passthrough() {
        // Encode data bits = 0b0000_0000_0001 (just bit 11 set).
        // Compute parity: only generator[0] contributes (data bit 11).
        let data: i32 = 0x001; // 12-bit data value
        let mut parity: i32 = 0;
        let mut i = 0;
        while i < 12 {
            if (data & (1 << (11 - i))) != 0 {
                parity ^= tables::GOLAY_GENERATOR[i];
            }
            i += 1;
        }

        // Pack into 23-bit codeword: [22..11] = data, [10..0] = parity.
        let codeword: i32 = (data << 11) | parity;

        // Unpack to bit array (LSB-first).
        let mut in_bits = [0u8; 23];
        let mut bi = 0;
        while bi < 23 {
            in_bits[bi] = bit_at(codeword, bi);
            bi += 1;
        }

        let (out_bits, errs) = golay_decode(&in_bits);

        assert_eq!(errs, 0, "expected zero errors for a valid codeword");

        // Data bits (indices 11..23) should be unchanged.
        let mut di = 11;
        while di < 23 {
            assert_eq!(
                out_bits[di], in_bits[di],
                "data bit {di} should be unchanged"
            );
            di += 1;
        }
    }

    /// Verifies that the Golay decoder corrects a single-bit error in
    /// a data bit position.
    #[test]
    fn golay_single_error_correction() {
        // Encode data = 0xABC (12 bits: 1010_1011_1100).
        let data: i32 = 0xABC;
        let mut parity: i32 = 0;
        let mut i = 0;
        while i < 12 {
            if (data & (1 << (11 - i))) != 0 {
                parity ^= tables::GOLAY_GENERATOR[i];
            }
            i += 1;
        }

        let codeword: i32 = (data << 11) | parity;

        // Unpack to bit array.
        let mut in_bits = [0u8; 23];
        let mut bi = 0;
        while bi < 23 {
            in_bits[bi] = bit_at(codeword, bi);
            bi += 1;
        }

        // Flip a data bit (bit 15, which is data bit 4).
        in_bits[15] ^= 1;

        let (out_bits, errs) = golay_decode(&in_bits);

        assert_eq!(errs, 1, "expected exactly 1 corrected error");

        // Reconstruct the corrected data from out_bits.
        let mut corrected_data: i32 = 0;
        let mut di = 22;
        loop {
            if di < 11 {
                break;
            }
            corrected_data <<= 1;
            corrected_data |= i32::from(out_bits[di]);
            if di == 11 {
                break;
            }
            di -= 1;
        }

        assert_eq!(corrected_data, data, "corrected data should match original");
    }

    /// Verifies that `ecc_c0` leaves a zero frame unchanged and reports
    /// zero errors.
    #[test]
    fn ecc_c0_zero_frame() {
        // An all-zero frame is a valid Golay codeword (zero data, zero
        // parity), so the decoder should report zero errors.
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        let errs = ecc_c0(&mut ambe_fr);
        assert_eq!(errs, 0, "all-zero C0 should have zero errors");
    }

    /// Verifies that `ecc_data` produces the correct output length and
    /// zero errors for an all-zero frame.
    #[test]
    fn ecc_data_zero_frame() {
        let ambe_fr = [0u8; AMBE_FRAME_BITS];
        let mut ambe_d = [0u8; AMBE_DATA_BITS];
        let errs = ecc_data(&ambe_fr, &mut ambe_d);
        assert_eq!(errs, 0, "all-zero frame should have zero C1 errors");

        // All output bits should be zero for an all-zero input.
        for (i, &bit) in ambe_d.iter().enumerate() {
            assert_eq!(bit, 0, "ambe_d[{i}] should be 0 for zero input");
        }
    }

    /// Verifies that `ecc_data` correctly extracts data bits from each
    /// codeword region and places them in the right `ambe_d` positions.
    #[test]
    fn ecc_data_bit_placement() {
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];

        // Set a recognizable bit in each codeword region.
        // C0: set bit at index 23 (highest data bit) -> should appear at ambe_d[0].
        ambe_fr[C0_OFFSET + 23] = 1;

        // C1: set bit at index 22 -> after Golay (with all others zero,
        // a single-bit error will be corrected). For simplicity, construct
        // a valid C1 codeword with bit 22 set.
        // Bit 22 set in C1 means data = 0x800, compute parity.
        let c1_data: i32 = 0x800;
        let mut c1_parity: i32 = 0;
        let mut i = 0;
        while i < 12 {
            if (c1_data & (1 << (11 - i))) != 0 {
                c1_parity ^= tables::GOLAY_GENERATOR[i];
            }
            i += 1;
        }
        let c1_codeword = (c1_data << 11) | c1_parity;
        let mut bi = 0;
        while bi < 23 {
            ambe_fr[C1_OFFSET + bi] = bit_at(c1_codeword, bi);
            bi += 1;
        }

        let mut ambe_d = [0u8; AMBE_DATA_BITS];
        let errs = ecc_data(&ambe_fr, &mut ambe_d);
        assert_eq!(errs, 0, "valid C1 codeword should have zero errors");

        // C0 bit at index 23 should map to ambe_d[0].
        assert_eq!(ambe_d[0], 1, "C0 MSB should map to ambe_d[0]");

        // C1 bit 22 (data MSB) should map to ambe_d[12].
        assert_eq!(ambe_d[12], 1, "C1 data MSB should map to ambe_d[12]");
    }

    /// Verifies Golay decoder handles a 3-bit error pattern correctly.
    #[test]
    fn golay_three_bit_error_correction() {
        // Encode data = 0x000 (all zeros) with correct parity.
        // A zero data word has zero parity, so all 23 bits are zero.
        let mut in_bits = [0u8; 23];

        // Inject 3 errors in data bit positions 11, 15, and 20.
        in_bits[11] = 1;
        in_bits[15] = 1;
        in_bits[20] = 1;

        let (out_bits, errs) = golay_decode(&in_bits);

        assert_eq!(errs, 3, "expected 3 corrected errors");

        // All data bits (indices 11..23) should be corrected back to 0.
        let mut di = 11;
        while di < 23 {
            assert_eq!(out_bits[di], 0, "data bit {di} should be corrected to 0");
            di += 1;
        }
    }
}
