//! Bit unpacking and LFSR demodulation for AMBE 3600x2450 voice frames.
//!
//! The AMBE codec transmits 72 bits (9 bytes) per voice frame, but the
//! bits are not stored in codeword order on the wire. Instead, they are
//! **interleaved** across the four FEC codewords (C0, C1, C2, C3) so
//! that burst errors (consecutive corrupted bits) are spread across
//! multiple codewords rather than concentrating in one. This allows the
//! per-codeword FEC (Golay, Hamming) to correct errors that would
//! otherwise exceed its capacity on a single codeword.
//!
//! After unpacking and C0 error correction, the C1 codeword must be
//! **demodulated** by XOR-ing with a pseudo-random sequence (LFSR)
//! seeded from the corrected C0 data bits. This scrambling prevents
//! correlated errors in C0 from systematically biasing C1 decoding.
//!
//! # Interleave Table
//!
//! The D-STAR interleave pattern is defined by the `dW` and `dX` tables
//! from DSD (Digital Speech Decoder, ISC license). For each of the 72
//! input bits, `dW[i]` selects the codeword (0=C0, 1=C1, 2=C2, 3=C3)
//! and `dX[i]` selects the bit position within that codeword. This
//! module pre-computes the flat-array target index for each input bit.
//!
//! # Flat Frame Layout
//!
//! The `ambe_fr` array uses one byte per bit (0 or 1), laid out as:
//!
//! | Codeword | Indices  | Bits | Protection       |
//! |----------|----------|------|------------------|
//! | C0       | 0..24    | 24   | Golay(23,12) + 1 parity |
//! | C1       | 24..47   | 23   | Golay(23,12)     |
//! | C2       | 47..58   | 11   | None (data only) |
//! | C3       | 58..72   | 14   | None (data only) |
//!
//! # Reference
//!
//! - Interleave tables: DSD `dstar_const.h`
//!   (<https://github.com/szechyjs/dsd>), ISC license.
//! - Demodulation: mbelib `ambe3600x2450.c`
//!   `mbe_demodulateAmbe3600x2450Data()`
//!   (<https://github.com/szechyjs/mbelib>), ISC license.

/// Number of bits in the FEC codeword array.
///
/// The 72-bit AMBE frame is unpacked into a flat array where each byte
/// holds a single bit (0 or 1). The four FEC codewords are laid out
/// contiguously:
///
/// | Codeword | Indices  | Length |
/// |----------|----------|--------|
/// | C0       | 0..24    | 24     |
/// | C1       | 24..47   | 23     |
/// | C2       | 47..58   | 11     |
/// | C3       | 58..72   | 14     |
pub(crate) const AMBE_FRAME_BITS: usize = 72;

/// Offset of C0 codeword in the flat `ambe_fr` array.
const C0_OFFSET: usize = 0;

/// Offset of C1 codeword in the flat `ambe_fr` array.
const C1_OFFSET: usize = 24;

/// D-STAR bit interleave table: input bit index to flat `ambe_fr` index.
///
/// Derived from the DSD `dW[72]` and `dX[72]` tables (`dstar_const.h`).
/// For each input bit `i` (0..72, MSB-first from the 9-byte frame),
/// `INTERLEAVE[i]` gives the flat `ambe_fr` index where that bit
/// belongs.
///
/// The DSD tables use `ambe_fr[dW[i]][dX[i]]` with a `char[4][24]`
/// layout. We convert to flat indices using the codeword offsets:
/// - `dW=0` (C0): flat index = `dX[i]`
/// - `dW=1` (C1): flat index = `24 + dX[i]`
/// - `dW=2` (C2): flat index = `47 + dX[i]`
/// - `dW=3` (C3): flat index = `58 + dX[i]`
#[rustfmt::skip]
const INTERLEAVE: [u8; AMBE_FRAME_BITS] = [
    // Input bits 0..11: spread across C0, C1, C2, C3
    10, 22, 69, 56, 34, 46, 11, 23, 32, 44,  9, 21,
    // Input bits 12..23
    68, 55, 33, 45, 66, 53, 31, 43,  8, 20, 67, 54,
    // Input bits 24..35
     6, 18, 65, 52, 30, 42,  7, 19, 28, 40,  5, 17,
    // Input bits 36..47
    64, 51, 29, 41, 62, 49, 27, 39,  4, 16, 63, 50,
    // Input bits 48..59
     2, 14, 61, 48, 26, 38,  3, 15, 24, 36,  1, 13,
    // Input bits 60..71
    60, 47, 25, 37, 58, 70, 57, 35,  0, 12, 59, 71,
];

/// LFSR multiplier for the C1 pseudo-random demodulation sequence.
///
/// The linear congruential generator uses:
/// `pr[i] = (173 * pr[i-1] + 13849) mod 65536`
///
/// This is a standard LCG with full period 65536 (since 173 is coprime
/// to 65536 and the increment 13849 is odd). The modular reduction is
/// implicit through `u16` wrapping arithmetic.
const LFSR_MULTIPLIER: u16 = 173;

/// LFSR increment for the C1 pseudo-random demodulation sequence.
const LFSR_INCREMENT: u16 = 13849;

/// Seed scaling factor applied to the packed C0 data bits before
/// starting the LFSR sequence.
///
/// The seed is `16 * packed_c0_data`, effectively left-shifting the
/// 12-bit data value by 4 positions within the 16-bit LFSR state.
const LFSR_SEED_SCALE: u16 = 16;

/// Number of LFSR pseudo-random values generated for C1 demodulation.
///
/// One value per C1 bit (23 bits), plus the seed at index 0.
const LFSR_PR_LEN: usize = 24;

/// Threshold for converting LFSR state to a single-bit pseudo-random
/// value. Values >= 32768 map to 1; values < 32768 map to 0. This is
/// equivalent to extracting the MSB of the 16-bit LFSR state.
const LFSR_THRESHOLD: u16 = 32768;

/// Unpacks a 9-byte AMBE frame into the bit-field array used by FEC.
///
/// Each of the 72 input bits (extracted MSB-first from the 9-byte
/// frame) is placed into its corresponding FEC codeword position
/// according to the D-STAR interleave pattern. The interleave spreads
/// bits from each codec parameter across multiple codewords so that
/// burst errors on the RF channel do not concentrate in one parameter.
///
/// # Algorithm
///
/// 1. Extract bits MSB-first from the 9-byte input (bit 7 of byte 0
///    is input bit 0, bit 0 of byte 0 is input bit 7, etc.).
/// 2. For each input bit `i`, write it to `ambe_fr[INTERLEAVE[i]]`.
///
/// The output array is zeroed by the caller; only `1` bits are written.
///
/// # Arguments
///
/// - `ambe`: the 9-byte packed AMBE frame from the D-STAR voice data.
/// - `ambe_fr`: output array of 72 single-bit bytes, laid out in FEC
///   codeword order (C0 at 0..24, C1 at 24..47, C2 at 47..58, C3 at
///   58..72).
pub(crate) fn unpack_frame(ambe: &[u8; 9], ambe_fr: &mut [u8; AMBE_FRAME_BITS]) {
    // Process each of the 72 input bits. The bits are packed MSB-first
    // in the 9-byte input: bit 7 of byte 0 is the first bit transmitted,
    // bit 0 of byte 8 is the last.
    let mut input_bit: usize = 0;
    let mut byte_idx: usize = 0;

    while byte_idx < 9 {
        let byte_val = match ambe.get(byte_idx) {
            Some(&b) => b,
            None => 0,
        };

        // Extract 8 bits from this byte, MSB first (bit 7 down to bit 0).
        let mut bit_pos: u8 = 7;
        loop {
            // Extract the single bit at position `bit_pos`.
            let bit = (byte_val >> bit_pos) & 1;

            // Look up the flat interleave target for this input bit.
            if let Some(&target_idx) = INTERLEAVE.get(input_bit)
                && let Some(slot) = ambe_fr.get_mut(target_idx as usize)
            {
                *slot = bit;
            }

            input_bit += 1;

            if bit_pos == 0 {
                break;
            }
            bit_pos -= 1;
        }

        byte_idx += 1;
    }
}

/// Demodulates the C1 codeword bits in place using an LFSR
/// pseudo-random sequence seeded from the corrected C0 data.
///
/// After C0 error correction, its 12 data bits (indices 12..23 within
/// C0, corresponding to flat indices 12..23) are packed into a 12-bit
/// integer, multiplied by 16, and used to seed a linear congruential
/// generator (LCG). The LCG produces 23 pseudo-random bits that are
/// XOR-ed with the C1 codeword bits to reverse the modulation applied
/// by the AMBE encoder.
///
/// This scrambling ensures that systematic patterns in C0 (which
/// carries the fundamental frequency -- the most critical parameter)
/// do not create correlated artifacts in C1 (which carries the PRBA
/// spectral coefficients).
///
/// # LFSR Algorithm
///
/// 1. Pack C0 data bits `ambe_fr[12..24]` MSB-first into a 12-bit
///    value `seed` (bit at index 23 is MSB, index 12 is LSB).
/// 2. `pr[0] = 16 * seed` (fits in u16 since seed < 4096).
/// 3. For `i` in 1..24: `pr[i] = (173 * pr[i-1] + 13849) mod 65536`.
/// 4. Convert each `pr[i]` to a single bit: `pr[i] / 32768` (0 or 1).
/// 5. XOR C1 bits: for `j` from 22 down to 0, `C1[j] ^= pr[k]`
///    where `k` goes from 1 to 23.
///
/// # Arguments
///
/// - `ambe_fr`: the flat 72-bit frame array. C0 must already be
///   Golay-corrected (by `ecc_c0`). C1 bits at indices 24..47 are
///   modified in place.
pub(crate) fn demodulate_c1(ambe_fr: &mut [u8; AMBE_FRAME_BITS]) {
    // Step 1: Pack the 12 C0 data bits (flat indices 12..23) into a
    // 12-bit integer. The C code iterates from index 23 down to 12,
    // shifting left each time, so index 23 becomes the MSB.
    let mut seed_bits: u16 = 0;
    let mut i: usize = 23;
    loop {
        seed_bits <<= 1;
        // C0 data bits are at flat indices C0_OFFSET + 12 through
        // C0_OFFSET + 23. Since C0_OFFSET is 0, these are indices 12..23.
        seed_bits |= u16::from(*ambe_fr.get(C0_OFFSET + i).unwrap_or(&0));
        if i == 12 {
            break;
        }
        i -= 1;
    }

    // Step 2: Generate the LFSR pseudo-random sequence.
    // pr[0] is the seed (16 * seed_bits), then each subsequent value is
    // computed via the LCG recurrence. The u16 type provides the
    // mod-65536 reduction automatically through wrapping arithmetic.
    let mut pr = [0u16; LFSR_PR_LEN];
    if let Some(slot) = pr.get_mut(0) {
        *slot = LFSR_SEED_SCALE.wrapping_mul(seed_bits);
    }

    let mut pi: usize = 1;
    while pi < LFSR_PR_LEN {
        let prev = *pr.get(pi - 1).unwrap_or(&0);
        if let Some(slot) = pr.get_mut(pi) {
            *slot = LFSR_MULTIPLIER
                .wrapping_mul(prev)
                .wrapping_add(LFSR_INCREMENT);
        }
        pi += 1;
    }

    // Step 3: Convert each LFSR state to a single pseudo-random bit.
    // Values >= 32768 (MSB set) become 1; values < 32768 become 0.
    // This is equivalent to integer division by 32768.
    let mut pr_bits = [0u8; LFSR_PR_LEN];
    let mut bi: usize = 1;
    while bi < LFSR_PR_LEN {
        if let Some(slot) = pr_bits.get_mut(bi) {
            let val = *pr.get(bi).unwrap_or(&0);
            *slot = u8::from(val >= LFSR_THRESHOLD);
        }
        bi += 1;
    }

    // Step 4: XOR C1 bits with the pseudo-random sequence.
    // The C code iterates j from 22 down to 0 with k from 1 to 23:
    //   ambe_fr[1][j] ^= pr[k]
    // In flat layout: C1[j] is at index C1_OFFSET + j.
    let mut k: usize = 1;
    let mut j: usize = 22;
    loop {
        let pr_bit = *pr_bits.get(k).unwrap_or(&0);
        if let Some(c1_bit) = ambe_fr.get_mut(C1_OFFSET + j) {
            *c1_bit ^= pr_bit;
        }

        k += 1;

        if j == 0 {
            break;
        }
        j -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that unpacking an all-zero frame produces all-zero bits.
    ///
    /// An all-zero 9-byte input has no set bits, so every position in
    /// the output array should remain zero regardless of the interleave
    /// pattern.
    #[test]
    fn unpack_all_zeros() {
        let ambe = [0u8; 9];
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        unpack_frame(&ambe, &mut ambe_fr);

        for (i, &bit) in ambe_fr.iter().enumerate() {
            assert_eq!(bit, 0, "ambe_fr[{i}] should be 0 for all-zero input");
        }
    }

    /// Verifies that unpacking an all-ones frame sets all 72 bit positions.
    ///
    /// When every input bit is 1, every position in the interleave table
    /// should receive a 1, resulting in all 72 output positions set.
    #[test]
    fn unpack_all_ones() {
        let ambe = [0xFFu8; 9];
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        unpack_frame(&ambe, &mut ambe_fr);

        for (i, &bit) in ambe_fr.iter().enumerate() {
            assert_eq!(bit, 1, "ambe_fr[{i}] should be 1 for all-ones input");
        }
    }

    /// Verifies that each input bit lands at exactly the position
    /// specified by the interleave table.
    ///
    /// Sets each of the 72 input bits one at a time and checks that
    /// only the expected output position is set. This confirms the
    /// interleave table is correctly applied and that no positions
    /// overlap or are missed.
    #[test]
    fn unpack_single_bit_positions() {
        for input_bit in 0..72_usize {
            let byte_idx = input_bit / 8;
            let bit_pos = 7 - (input_bit % 8);

            // Build a 9-byte frame with only this one bit set.
            let mut ambe = [0u8; 9];
            if let Some(b) = ambe.get_mut(byte_idx) {
                *b = 1 << bit_pos;
            }

            let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
            unpack_frame(&ambe, &mut ambe_fr);

            // The expected target from the interleave table.
            let expected_target = match INTERLEAVE.get(input_bit) {
                Some(&t) => t as usize,
                None => continue,
            };

            // Verify exactly one bit is set, at the expected position.
            for (out_idx, &bit) in ambe_fr.iter().enumerate() {
                if out_idx == expected_target {
                    assert_eq!(
                        bit, 1,
                        "input bit {input_bit} should set ambe_fr[{expected_target}]"
                    );
                } else {
                    assert_eq!(
                        bit, 0,
                        "input bit {input_bit}: ambe_fr[{out_idx}] should be 0 \
                         (only [{expected_target}] should be set)"
                    );
                }
            }
        }
    }

    /// Verifies that unpacking fills each codeword region with the
    /// correct number of bits.
    ///
    /// The interleave table assigns exactly 24 bits to C0, 23 to C1,
    /// 11 to C2, and 14 to C3. This test confirms those counts by
    /// counting how many interleave entries target each codeword range.
    #[test]
    fn interleave_table_codeword_counts() {
        let mut c0_count: usize = 0;
        let mut c1_count: usize = 0;
        let mut c2_count: usize = 0;
        let mut c3_count: usize = 0;

        for &target in &INTERLEAVE {
            let t = target as usize;
            if t < 24 {
                c0_count += 1;
            } else if t < 47 {
                c1_count += 1;
            } else if t < 58 {
                c2_count += 1;
            } else {
                c3_count += 1;
            }
        }

        assert_eq!(c0_count, 24, "C0 should have 24 bits");
        assert_eq!(c1_count, 23, "C1 should have 23 bits");
        assert_eq!(c2_count, 11, "C2 should have 11 bits");
        assert_eq!(c3_count, 14, "C3 should have 14 bits");
    }

    /// Verifies that the interleave table covers all 72 output positions
    /// exactly once (no duplicates, no gaps).
    #[test]
    fn interleave_table_is_permutation() {
        let mut seen = [false; AMBE_FRAME_BITS];

        for (i, &target) in INTERLEAVE.iter().enumerate() {
            let t = target as usize;
            assert!(
                t < AMBE_FRAME_BITS,
                "INTERLEAVE[{i}] = {t} is out of range [0, {AMBE_FRAME_BITS})"
            );
            assert!(
                !*seen.get(t).unwrap_or(&true),
                "INTERLEAVE[{i}] = {t} is a duplicate"
            );
            if let Some(slot) = seen.get_mut(t) {
                *slot = true;
            }
        }

        for (i, &was_seen) in seen.iter().enumerate() {
            assert!(
                was_seen,
                "output position {i} was never targeted by INTERLEAVE"
            );
        }
    }

    /// Verifies LFSR demodulation with all-zero C0 data bits.
    ///
    /// When C0 data bits are all zero, the LFSR seed is 0 and the
    /// pseudo-random sequence is deterministic. This test verifies the
    /// exact sequence by computing it independently and comparing against
    /// the demodulation output.
    #[test]
    fn demodulate_c1_zero_seed() {
        // Build a frame with all-zero C0 and all-zero C1.
        // After demodulation, C1 should equal the LFSR sequence
        // (since 0 XOR pr = pr).
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        demodulate_c1(&mut ambe_fr);

        // Compute expected LFSR sequence independently.
        let mut pr = [0u16; LFSR_PR_LEN];
        // pr[0] = 16 * 0 = 0
        let mut pi: usize = 1;
        while pi < LFSR_PR_LEN {
            let prev = *pr.get(pi - 1).unwrap_or(&0);
            if let Some(slot) = pr.get_mut(pi) {
                *slot = LFSR_MULTIPLIER
                    .wrapping_mul(prev)
                    .wrapping_add(LFSR_INCREMENT);
            }
            pi += 1;
        }

        // Verify C1 bits match the LFSR output.
        // C1[22] = pr[1]/32768, C1[21] = pr[2]/32768, ..., C1[0] = pr[23]/32768
        let mut k: usize = 1;
        let mut j: usize = 22;
        loop {
            let expected = u8::from(*pr.get(k).unwrap_or(&0) >= LFSR_THRESHOLD);
            let actual = *ambe_fr.get(C1_OFFSET + j).unwrap_or(&0);
            assert_eq!(
                actual, expected,
                "C1[{j}] should be {expected} from LFSR pr[{k}], got {actual}"
            );

            k += 1;
            if j == 0 {
                break;
            }
            j -= 1;
        }
    }

    /// Verifies LFSR demodulation with all-one C0 data bits.
    ///
    /// When C0 data bits are all 1 (0xFFF), the LFSR seed is
    /// `16 * 4095 = 65520`. This test confirms the LFSR produces the
    /// correct sequence for this non-trivial seed.
    #[test]
    fn demodulate_c1_all_ones_seed() {
        // Set C0 data bits (indices 12..23) all to 1, C1 all to 0.
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        let mut idx: usize = 12;
        while idx <= 23 {
            if let Some(slot) = ambe_fr.get_mut(C0_OFFSET + idx) {
                *slot = 1;
            }
            idx += 1;
        }
        demodulate_c1(&mut ambe_fr);

        // Compute expected LFSR with seed = 16 * 0xFFF = 65520.
        let mut pr = [0u16; LFSR_PR_LEN];
        if let Some(slot) = pr.get_mut(0) {
            *slot = LFSR_SEED_SCALE.wrapping_mul(0x0FFF);
        }
        let mut pi: usize = 1;
        while pi < LFSR_PR_LEN {
            let prev = *pr.get(pi - 1).unwrap_or(&0);
            if let Some(slot) = pr.get_mut(pi) {
                *slot = LFSR_MULTIPLIER
                    .wrapping_mul(prev)
                    .wrapping_add(LFSR_INCREMENT);
            }
            pi += 1;
        }

        // Verify C1 bits.
        let mut k: usize = 1;
        let mut j: usize = 22;
        loop {
            let expected = u8::from(*pr.get(k).unwrap_or(&0) >= LFSR_THRESHOLD);
            let actual = *ambe_fr.get(C1_OFFSET + j).unwrap_or(&0);
            assert_eq!(
                actual, expected,
                "C1[{j}] should be {expected} from LFSR pr[{k}], got {actual}"
            );

            k += 1;
            if j == 0 {
                break;
            }
            j -= 1;
        }
    }

    /// Verifies that demodulation is self-inverse: applying it twice
    /// with the same C0 data restores the original C1 bits.
    ///
    /// Since demodulation XORs with a deterministic sequence derived
    /// from C0, applying it twice cancels out (XOR is its own inverse).
    #[test]
    fn demodulate_c1_is_involution() {
        // Set up a frame with known C0 data and non-zero C1 bits.
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];

        // Set some C0 data bits to create a non-trivial seed.
        if let Some(slot) = ambe_fr.get_mut(C0_OFFSET + 15) {
            *slot = 1;
        }
        if let Some(slot) = ambe_fr.get_mut(C0_OFFSET + 20) {
            *slot = 1;
        }

        // Set some C1 bits to non-zero values.
        if let Some(slot) = ambe_fr.get_mut(C1_OFFSET + 5) {
            *slot = 1;
        }
        if let Some(slot) = ambe_fr.get_mut(C1_OFFSET + 10) {
            *slot = 1;
        }
        if let Some(slot) = ambe_fr.get_mut(C1_OFFSET + 18) {
            *slot = 1;
        }

        // Save original C1 bits.
        let mut original_c1 = [0u8; 23];
        let mut ci: usize = 0;
        while ci < 23 {
            if let Some(&val) = ambe_fr.get(C1_OFFSET + ci)
                && let Some(slot) = original_c1.get_mut(ci)
            {
                *slot = val;
            }
            ci += 1;
        }

        // Demodulate twice.
        demodulate_c1(&mut ambe_fr);
        demodulate_c1(&mut ambe_fr);

        // C1 should be restored to original.
        let mut vi: usize = 0;
        while vi < 23 {
            let restored = *ambe_fr.get(C1_OFFSET + vi).unwrap_or(&0);
            let original = *original_c1.get(vi).unwrap_or(&0);
            assert_eq!(
                restored, original,
                "C1[{vi}] should be restored after double demodulation"
            );
            vi += 1;
        }
    }

    /// Verifies that demodulation does not modify C0, C2, or C3 bits.
    ///
    /// The LFSR demodulation should only touch C1 (indices 24..47).
    /// All other regions must remain unchanged.
    #[test]
    fn demodulate_c1_does_not_touch_other_codewords() {
        // Fill the entire frame with ones to detect any unintended writes.
        let mut ambe_fr = [1u8; AMBE_FRAME_BITS];

        // Save non-C1 regions.
        let mut saved = [0u8; AMBE_FRAME_BITS];
        let mut si: usize = 0;
        while si < AMBE_FRAME_BITS {
            if let Some(&val) = ambe_fr.get(si)
                && let Some(slot) = saved.get_mut(si)
            {
                *slot = val;
            }
            si += 1;
        }

        demodulate_c1(&mut ambe_fr);

        // Verify C0 (0..24) unchanged.
        let mut c0i: usize = 0;
        while c0i < 24 {
            assert_eq!(
                *ambe_fr.get(c0i).unwrap_or(&0),
                *saved.get(c0i).unwrap_or(&0),
                "C0[{c0i}] should not be modified by demodulation"
            );
            c0i += 1;
        }

        // Verify C2 (47..58) unchanged.
        let mut c2i: usize = 47;
        while c2i < 58 {
            assert_eq!(
                *ambe_fr.get(c2i).unwrap_or(&0),
                *saved.get(c2i).unwrap_or(&0),
                "C2 at index {c2i} should not be modified by demodulation"
            );
            c2i += 1;
        }

        // Verify C3 (58..72) unchanged.
        let mut c3i: usize = 58;
        while c3i < 72 {
            assert_eq!(
                *ambe_fr.get(c3i).unwrap_or(&0),
                *saved.get(c3i).unwrap_or(&0),
                "C3 at index {c3i} should not be modified by demodulation"
            );
            c3i += 1;
        }
    }

    /// Verifies a known byte pattern produces the expected flat frame layout.
    ///
    /// Uses a recognizable input pattern (0xAA = alternating 1/0 bits)
    /// and checks that the interleaved output matches the table lookup.
    #[test]
    fn unpack_known_pattern() {
        // 0xAA = 10101010 in binary, so bits 7,5,3,1 are set for each byte.
        let ambe = [0xAAu8; 9];
        let mut ambe_fr = [0u8; AMBE_FRAME_BITS];
        unpack_frame(&ambe, &mut ambe_fr);

        // Verify each input bit. For 0xAA, even-indexed input bits
        // (0, 2, 4, ...) are 1, odd-indexed (1, 3, 5, ...) are 0.
        for input_bit in 0..72_usize {
            let is_set = (input_bit % 2) == 0; // MSB of each byte pair
            let expected = u8::from(is_set);

            let target = match INTERLEAVE.get(input_bit) {
                Some(&t) => t as usize,
                None => continue,
            };
            let actual = *ambe_fr.get(target).unwrap_or(&0);

            assert_eq!(
                actual, expected,
                "input bit {input_bit} -> ambe_fr[{target}]: expected {expected}, got {actual}"
            );
        }
    }
}
