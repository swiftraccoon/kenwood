// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Inverse D-STAR bit-interleave table for AMBE frame packing.
//!
//! The decoder's interleave table in [`crate::unpack`] maps
//! `input_bit → ambe_fr_index`. For packing we need the inverse:
//! `ambe_fr_index → input_bit`. Both tables describe the same
//! permutation; the inverse is built at compile time from the forward
//! table so the two can never drift out of sync.
//!
//! # Reference
//!
//! The forward table is documented in `crate::unpack` and ultimately
//! derives from the DSD `dW[72]` / `dX[72]` tables
//! (<https://github.com/szechyjs/dsd>, ISC-licensed, copied under
//! GPL-2.0-or-later via the mbelib relicensing pathway).

#![expect(
    clippy::indexing_slicing,
    reason = "All indices are bounded by AMBE_FRAME_BITS=72 (enforced by the table \
              declarations themselves — FORWARD and INVERSE are `[u8; 72]`). The bit \
              permutation routines iterate over 0..72 and index into fixed-size tables; \
              bounds are statically provable but clippy can't prove them from the local \
              code shape."
)]

/// Number of bits in an AMBE frame (also the codeword array length).
pub(crate) const AMBE_FRAME_BITS: usize = 72;

/// Forward interleave: `FORWARD[input_bit] = ambe_fr_index`.
///
/// Identical to `crate::unpack::INTERLEAVE` — duplicated here as
/// `pub(crate)` so [`build_inverse`] can consume it at compile time
/// without exposing `unpack::INTERLEAVE` as a module-public item.
#[rustfmt::skip]
const FORWARD: [u8; AMBE_FRAME_BITS] = [
    10, 22, 69, 56, 34, 46, 11, 23, 32, 44,  9, 21,
    68, 55, 33, 45, 66, 53, 31, 43,  8, 20, 67, 54,
     6, 18, 65, 52, 30, 42,  7, 19, 28, 40,  5, 17,
    64, 51, 29, 41, 62, 49, 27, 39,  4, 16, 63, 50,
     2, 14, 61, 48, 26, 38,  3, 15, 24, 36,  1, 13,
    60, 47, 25, 37, 58, 70, 57, 35,  0, 12, 59, 71,
];

/// Invert the [`FORWARD`] permutation at compile time.
///
/// Given `FORWARD[input_bit] = ambe_fr_index`, produces
/// `INVERSE[ambe_fr_index] = input_bit`. Panics at compile time if
/// `FORWARD` is not a bijection on `0..AMBE_FRAME_BITS` (the initial
/// `u8::MAX` sentinel survives in any output slot that was never
/// assigned, and the final loop catches it).
const fn build_inverse() -> [u8; AMBE_FRAME_BITS] {
    let mut inverse = [u8::MAX; AMBE_FRAME_BITS];

    let mut input_bit = 0;
    while input_bit < AMBE_FRAME_BITS {
        let target = FORWARD[input_bit] as usize;
        // Reject collisions: two input bits claiming the same ambe_fr
        // slot would silently lose data at encode time.
        assert!(
            inverse[target] == u8::MAX,
            "FORWARD interleave has a collision — two input bits map to the same ambe_fr slot",
        );
        // Input-bit indices are 0..72 so the u8 narrowing is exact.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "input_bit counts 0..AMBE_FRAME_BITS = 0..72; fits trivially in u8."
        )]
        {
            inverse[target] = input_bit as u8;
        }
        input_bit += 1;
    }

    // Every ambe_fr slot must have been assigned. A sentinel here means
    // the FORWARD table doesn't cover the whole 0..72 range.
    let mut check = 0;
    while check < AMBE_FRAME_BITS {
        assert!(
            inverse[check] != u8::MAX,
            "FORWARD interleave has a gap — some ambe_fr slot has no source input bit",
        );
        check += 1;
    }

    inverse
}

/// Inverse interleave: `INVERSE[ambe_fr_index] = input_bit`.
///
/// Used by [`crate::encode::pack`] to read the 72 codeword bits back
/// out in transmission order (MSB-first within the 9-byte frame).
pub(crate) const INVERSE: [u8; AMBE_FRAME_BITS] = build_inverse();

#[cfg(test)]
mod tests {
    use super::{AMBE_FRAME_BITS, FORWARD, INVERSE};

    /// The inverse must undo the forward permutation.
    #[test]
    fn inverse_round_trips_forward() {
        for (input_bit, &target_u8) in FORWARD.iter().enumerate() {
            let target = target_u8 as usize;
            let expected = u8::try_from(input_bit).expect("input_bit < 72 fits in u8");
            assert_eq!(
                INVERSE[target], expected,
                "inverse mismatch at ambe_fr index {target} (input bit {input_bit})",
            );
        }
    }

    /// Every `ambe_fr` slot should map to a unique input bit in 0..72.
    #[test]
    fn inverse_is_a_permutation() {
        let mut seen = [false; AMBE_FRAME_BITS];
        for &input_bit in &INVERSE {
            let i = input_bit as usize;
            assert!(
                i < AMBE_FRAME_BITS,
                "inverse contains out-of-range value {i}",
            );
            assert!(!seen[i], "inverse maps two ambe_fr slots to input bit {i}",);
            seen[i] = true;
        }
        // Redundant with the loop above, but explicit: all 72 bits covered.
        assert!(seen.iter().all(|&b| b), "inverse missed some input bit");
    }
}
