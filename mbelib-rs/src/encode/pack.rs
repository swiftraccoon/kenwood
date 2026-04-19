// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE frame packing — the exact inverse of [`crate::unpack`].
//!
//! Given a 72-bit codeword array `ambe_fr` (C0 at 0..24, C1 at 24..47,
//! C2 at 47..58, C3 at 58..72 — the same layout the decoder consumes
//! after error correction and C1 demodulation), this module:
//!
//! 1. **Modulates C1** by XOR-ing its bits with an LFSR sequence
//!    seeded from the C0 data bits. The XOR operation is self-inverse,
//!    so we call [`crate::unpack::demodulate_c1`] directly — "modulate"
//!    and "demodulate" are the same byte-level op; only the pipeline
//!    direction differs. Applied after the caller has already encoded
//!    FEC into C0 and C1 (or, for P1 round-trip testing, the caller
//!    supplies already-encoded codewords and this step restores them
//!    to wire form).
//! 2. **Packs** the 72 bits back into 9 bytes via the inverse interleave
//!    table, MSB-first within each byte (bit 7 of byte 0 is input bit 0).
//!
//! The output is a valid 9-byte AMBE wire frame suitable for the
//! DSVT voice-data slot in the D-STAR frame.

use crate::encode::interleave::{AMBE_FRAME_BITS, INVERSE};
use crate::unpack::demodulate_c1;

/// Pack a 72-bit FEC-codeword array into a 9-byte AMBE wire frame.
///
/// This is the inverse of the decoder's `unpack_frame` composed with
/// `demodulate_c1`: given an `ambe_fr` array that contains
/// already-FEC-encoded codewords (C0 Golay-encoded, C1 Golay-encoded
/// but *not* yet XOR-scrambled, C2 / C3 as-is), produce the on-wire
/// byte sequence that a conformant AMBE decoder would recover back
/// to the same `ambe_fr`.
///
/// # Arguments
///
/// - `ambe_fr`: a 72-element array where each byte holds a single bit
///   (0 or 1), laid out in FEC-codeword order (C0 at 0..24, C1 at
///   24..47, C2 at 47..58, C3 at 58..72). This is the *same* layout
///   `unpack_frame` writes into.
///
/// # Returns
///
/// 9 packed bytes, MSB-first. `result[0]` bit 7 is the first bit that
/// goes on the wire.
///
/// # Round-trip invariant
///
/// For any `ambe_fr` produced by the decoder's `unpack_frame` +
/// `demodulate_c1`, the following holds (see `tests` below):
///
/// ```text
/// let mut fr = [0u8; 72];
/// unpack_frame(&wire, &mut fr);
/// demodulate_c1(&mut fr);
/// let wire_round_trip = pack_frame(&fr);
/// assert_eq!(wire, wire_round_trip);
/// ```
#[must_use]
pub fn pack_frame(ambe_fr: &[u8; AMBE_FRAME_BITS]) -> [u8; 9] {
    // Step 1: modulate C1 back to its on-wire scrambled form. The
    // decoder calls `demodulate_c1` after Golay-correcting C0; the
    // encoder must undo that scrambling before packing. XOR is its own
    // inverse so the same routine serves both directions.
    let mut working = *ambe_fr;
    demodulate_c1(&mut working);

    // Step 2: pack 72 codeword bits back into 9 bytes, MSB-first.
    // Iterate over every `ambe_fr` position paired with its
    // wire-order input-bit index from `INVERSE`. That bit lands at
    // byte `input_bit / 8`, bit position `7 - (input_bit % 8)`
    // (MSB-first within each byte).
    let mut out = [0u8; 9];
    for (&bit_val, &input_bit_u8) in working.iter().zip(INVERSE.iter()) {
        if bit_val == 0 {
            continue;
        }
        let input_bit = input_bit_u8 as usize;
        let byte_idx = input_bit / 8;
        let bit_pos = 7 - (input_bit % 8);
        if let Some(b) = out.get_mut(byte_idx) {
            *b |= 1 << bit_pos;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{AMBE_FRAME_BITS, pack_frame};
    use crate::unpack::{demodulate_c1, unpack_frame};
    use proptest::prelude::*;

    /// Hand-round-trip a zero frame. Trivial but catches "did anything
    /// compile at all" regressions.
    #[test]
    fn zero_frame_round_trips() {
        let wire = [0u8; 9];
        let mut fr = [0u8; AMBE_FRAME_BITS];
        unpack_frame(&wire, &mut fr);
        demodulate_c1(&mut fr);
        let wire_rt = pack_frame(&fr);
        assert_eq!(wire, wire_rt);
    }

    /// Hand-round-trip a frame where every bit is 1. Exercises the
    /// packing when every output position must be set.
    #[test]
    fn all_ones_frame_round_trips() {
        let wire = [0xFFu8; 9];
        let mut fr = [0u8; AMBE_FRAME_BITS];
        unpack_frame(&wire, &mut fr);
        demodulate_c1(&mut fr);
        let wire_rt = pack_frame(&fr);
        assert_eq!(wire, wire_rt);
    }

    proptest! {
        /// For ANY 9-byte input, unpack → demodulate → pack must
        /// return the original 9 bytes. This is the key correctness
        /// invariant for the packing layer: together, unpack+pack
        /// form a bijection on the wire-frame byte space.
        #[test]
        fn wire_frame_round_trip(wire in prop::array::uniform9(0u8..=255)) {
            let mut fr = [0u8; AMBE_FRAME_BITS];
            unpack_frame(&wire, &mut fr);
            demodulate_c1(&mut fr);
            let wire_rt = pack_frame(&fr);
            prop_assert_eq!(wire, wire_rt);
        }
    }
}
