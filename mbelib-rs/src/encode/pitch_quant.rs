// SPDX-FileCopyrightText: 2016 Max H. Parke (OP25 ambe_encoder/b0_lookup)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Pitch-index (`b0`) quantization — OP25-compatible port.
//!
//! OP25's `ambe_encoder.cc:158-192` picks `b[0]` by first mapping
//! `ref_pitch` (Q8.8) through a 827-entry lookup table, then
//! incrementing or decrementing the lookup-table index until
//! `AmbePlusLtable[b[0]] == imbe_param->num_harms`. This mirrors
//! OP25's exact decision policy, which differs from a pure
//! L-constrained nearest-`W0_TABLE` search:
//!
//! - OP25's lookup returns the canonical `b0` for a given pitch
//!   interval, walks ±1 slots in `b0_lookup[]` (NOT in `b0` itself)
//!   until the `L` constraint is met.
//! - The nearest-`W0` search picks whichever `b0` at the same `L`
//!   has the smallest `|W0_TABLE[b0] − target_W0|`.
//!
//! Both produce decoder-compatible output; OP25's walk is what the
//! reference encoder actually emits, so matching it is the only way
//! to achieve bit-exact `b0` against the OP25 traces.

#![expect(
    clippy::indexing_slicing,
    reason = "Pitch quantization: indices into B0_LOOKUP (827 entries, statically sized) \
              and AmbePlusLtable are bounded by the OP25 walk algorithm — the ±1 step \
              over `b0_lookup[]` stays within [0, 826] by construction. Rewriting with \
              `.get()?` would obscure the OP25 reference-implementation traceability \
              that this module exists to preserve."
)]

/// OP25 `b0_lookup[]` table — 827 entries (`ambe_encoder.cc:41-146`).
///
/// Indexed by `(ref_pitch >> 5) - 159` where `ref_pitch` is the
/// pitch period in Q8.8 format. Each entry is a 7-bit `b0` index in
/// `0..=119`.
#[rustfmt::skip]
pub(crate) const B0_LOOKUP: [u8; 827] = [
      0,   0,   0,   1,   1,   2,   2,   2,   3,   3,   4,   4,   4,   5,   5,   5,
      6,   6,   7,   7,   7,   8,   8,   8,   9,   9,   9,  10,  10,  11,  11,  11,
     12,  12,  12,  13,  13,  13,  14,  14,  14,  15,  15,  15,  16,  16,  16,  17,
     17,  17,  17,  18,  18,  18,  19,  19,  19,  20,  20,  20,  21,  21,  21,  21,
     22,  22,  22,  23,  23,  23,  24,  24,  24,  24,  25,  25,  25,  25,  26,  26,
     26,  27,  27,  27,  27,  28,  28,  28,  29,  29,  29,  29,  30,  30,  30,  30,
     31,  31,  31,  31,  31,  32,  32,  32,  32,  33,  33,  33,  33,  34,  34,  34,
     34,  35,  35,  35,  35,  36,  36,  36,  36,  37,  37,  37,  37,  38,  38,  38,
     38,  38,  39,  39,  39,  39,  40,  40,  40,  40,  40,  41,  41,  41,  41,  42,
     42,  42,  42,  42,  43,  43,  43,  43,  43,  44,  44,  44,  44,  45,  45,  45,
     45,  45,  46,  46,  46,  46,  46,  47,  47,  47,  47,  47,  48,  48,  48,  48,
     48,  49,  49,  49,  49,  49,  49,  50,  50,  50,  50,  50,  51,  51,  51,  51,
     51,  52,  52,  52,  52,  52,  52,  53,  53,  53,  53,  53,  54,  54,  54,  54,
     54,  54,  55,  55,  55,  55,  55,  56,  56,  56,  56,  56,  56,  57,  57,  57,
     57,  57,  57,  58,  58,  58,  58,  58,  58,  59,  59,  59,  59,  59,  59,  60,
     60,  60,  60,  60,  60,  61,  61,  61,  61,  61,  61,  62,  62,  62,  62,  62,
     62,  63,  63,  63,  63,  63,  63,  63,  64,  64,  64,  64,  64,  64,  65,  65,
     65,  65,  65,  65,  65,  66,  66,  66,  66,  66,  66,  67,  67,  67,  67,  67,
     67,  67,  68,  68,  68,  68,  68,  68,  68,  69,  69,  69,  69,  69,  69,  69,
     70,  70,  70,  70,  70,  70,  70,  71,  71,  71,  71,  71,  71,  71,  72,  72,
     72,  72,  72,  72,  72,  73,  73,  73,  73,  73,  73,  73,  73,  74,  74,  74,
     74,  74,  74,  74,  75,  75,  75,  75,  75,  75,  75,  75,  76,  76,  76,  76,
     76,  76,  76,  76,  77,  77,  77,  77,  77,  77,  77,  77,  77,  78,  78,  78,
     78,  78,  78,  78,  78,  79,  79,  79,  79,  79,  79,  79,  79,  80,  80,  80,
     80,  80,  80,  80,  80,  81,  81,  81,  81,  81,  81,  81,  81,  81,  82,  82,
     82,  82,  82,  82,  82,  82,  83,  83,  83,  83,  83,  83,  83,  83,  83,  84,
     84,  84,  84,  84,  84,  84,  84,  84,  85,  85,  85,  85,  85,  85,  85,  85,
     85,  86,  86,  86,  86,  86,  86,  86,  86,  86,  87,  87,  87,  87,  87,  87,
     87,  87,  87,  88,  88,  88,  88,  88,  88,  88,  88,  88,  89,  89,  89,  89,
     89,  89,  89,  89,  89,  89,  90,  90,  90,  90,  90,  90,  90,  90,  90,  90,
     91,  91,  91,  91,  91,  91,  91,  91,  91,  92,  92,  92,  92,  92,  92,  92,
     92,  92,  92,  93,  93,  93,  93,  93,  93,  93,  93,  93,  93,  94,  94,  94,
     94,  94,  94,  94,  94,  94,  94,  94,  95,  95,  95,  95,  95,  95,  95,  95,
     95,  95,  96,  96,  96,  96,  96,  96,  96,  96,  96,  96,  96,  97,  97,  97,
     97,  97,  97,  97,  97,  97,  97,  98,  98,  98,  98,  98,  98,  98,  98,  98,
     98,  98,  99,  99,  99,  99,  99,  99,  99,  99,  99,  99,  99,  99, 100, 100,
    100, 100, 100, 100, 100, 100, 100, 100, 100, 101, 101, 101, 101, 101, 101, 101,
    101, 101, 101, 101, 102, 102, 102, 102, 102, 102, 102, 102, 102, 102, 102, 102,
    103, 103, 103, 103, 103, 103, 103, 103, 103, 103, 103, 103, 104, 104, 104, 104,
    104, 104, 104, 104, 104, 104, 104, 104, 105, 105, 105, 105, 105, 105, 105, 105,
    105, 105, 105, 105, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106, 106,
    107, 107, 107, 107, 107, 107, 107, 107, 107, 107, 107, 107, 107, 108, 108, 108,
    108, 108, 108, 108, 108, 108, 108, 108, 108, 109, 109, 109, 109, 109, 109, 109,
    109, 109, 109, 109, 109, 109, 110, 110, 110, 110, 110, 110, 110, 110, 110, 110,
    110, 110, 110, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111, 111,
    112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 112, 113, 113,
    113, 113, 113, 113, 113, 113, 113, 113, 113, 113, 113, 113, 114, 114, 114, 114,
    114, 114, 114, 114, 114, 114, 114, 114, 114, 115, 115, 115, 115, 115, 115, 115,
    115, 115, 115, 115, 115, 115, 115, 116, 116, 116, 116, 116, 116, 116, 116, 116,
    116, 116, 116, 116, 116, 116, 117, 117, 117, 117, 117, 117, 117, 117, 117, 117,
    117, 117, 117, 117, 118, 118, 118, 118, 118, 118, 118, 118, 118, 118, 118, 118,
    118, 118, 118, 119, 119, 119, 119, 119, 119, 119, 119,
];

/// Pick `b0` for a given pitch and target harmonic count via OP25's
/// lookup + ±1 walk policy.
///
/// `ref_pitch_q8_8` is the pitch period in Q8.8 format (samples × 256).
/// `target_l` is the desired `AmbePlusLtable[b0]` value — usually
/// `num_harms` from the V/UV + SA stage.
/// `ltable` is the L-table we look up against. For D-STAR / AMBE+
/// this is `AmbePlusLtable` (126 entries — codes 120–127 are
/// reserved for silence / tone / erasure and are not visited by the
/// walk). Only `ltable[0..120]` matters here.
///
/// Behaviour:
/// 1. `b0_i = (ref_pitch_q8_8 >> 5) - 159`, clamped to
///    `0..B0_LOOKUP.len()`.
/// 2. `b0 = B0_LOOKUP[b0_i]`, then `L = ltable[b0]`.
/// 3. Walk `b0_i` up or down by 1 until `L == target_l` OR the
///    lookup table bounds are hit (in which case the nearest
///    in-range `b0` is returned).
///
/// Mirrors `ambe_encoder.cc:158-192` exactly.
#[must_use]
pub(crate) fn pitch_index(ref_pitch_q8_8: u32, target_l: usize, ltable: &[f32]) -> u8 {
    // Compile-time const; B0_LOOKUP is a fixed-size array whose
    // length is 827 (see table definition). Asserting the literal
    // here instead of casting the const `B0_LOOKUP.len() as i32`
    // avoids the usize→i32 cast lints without a per-site `#[allow]`.
    const LOOKUP_LEN: i32 = 827;
    const LOOKUP_MAX: i32 = LOOKUP_LEN - 1;
    debug_assert_eq!(
        B0_LOOKUP.len(),
        LOOKUP_LEN as usize,
        "B0_LOOKUP length must equal LOOKUP_LEN=827 per OP25 ambe_encoder.cc:41-146 — the \
         walk algorithm's bounds rely on this invariant"
    );
    #[expect(
        clippy::cast_possible_wrap,
        reason = "Initial b0_lookup index: ref_pitch_q8_8 is Q8.8 with period < 256, so the \
                  value is < 2^16; `>> 5` yields <= 2047, which fits safely in i32 — the \
                  u32->i32 cast cannot wrap at these magnitudes."
    )]
    let initial = (ref_pitch_q8_8 >> 5) as i32 - 159;
    let mut b0_i: i32 = initial.clamp(0, LOOKUP_MAX);

    // Walk until AmbePlusLtable[b0] matches target_l, or we bump
    // into a boundary. OP25 treats boundary hits as silent aborts
    // (it returns without emitting a frame); we return whatever the
    // clamped b0 is — the caller has already validated the pitch
    // range, so boundary hits here indicate a pitch the codec can't
    // represent, and returning the closest b0 is the least-bad
    // fallback.
    #[expect(
        clippy::cast_sign_loss,
        reason = "b0_i is clamped to [0, LOOKUP_MAX] on line 120, so the i32-to-usize cast \
                  cannot lose a sign bit here."
    )]
    let mut b0 = B0_LOOKUP[b0_i as usize];
    let current_l = |b0: u8| -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "L_TABLE entries are whole positive integers in 9..=56 stored as f32; \
                      the f32-to-usize cast is exact within this range."
        )]
        let l = ltable.get(b0 as usize).copied().unwrap_or(0.0) as usize;
        l
    };

    // Bounded walk: never more than `B0_LOOKUP.len()` steps.
    for _ in 0..B0_LOOKUP.len() {
        let l = current_l(b0);
        if l == target_l {
            break;
        }
        if l < target_l {
            b0_i += 1;
        } else {
            b0_i -= 1;
        }
        if b0_i < 0 {
            b0_i = 0;
            #[expect(
                clippy::cast_sign_loss,
                reason = "b0_i was just set to 0 on the previous line, so the i32-to-usize \
                          cast is trivially safe."
            )]
            {
                b0 = B0_LOOKUP[b0_i as usize];
            }
            break;
        }
        if b0_i >= LOOKUP_LEN {
            b0_i = LOOKUP_MAX;
            #[expect(
                clippy::cast_sign_loss,
                reason = "b0_i was just set to LOOKUP_MAX (826), a non-negative integer, so \
                          the i32-to-usize cast is trivially safe."
            )]
            {
                b0 = B0_LOOKUP[b0_i as usize];
            }
            break;
        }
        #[expect(
            clippy::cast_sign_loss,
            reason = "b0_i has been verified non-negative (the `< 0` branch above) and \
                      below LOOKUP_LEN (the `>= LOOKUP_LEN` branch above); cast is safe."
        )]
        {
            b0 = B0_LOOKUP[b0_i as usize];
        }
    }
    b0
}

#[cfg(test)]
mod tests {
    use super::{B0_LOOKUP, pitch_index};
    use crate::tables::L_TABLE;

    #[test]
    fn table_size_matches_op25() {
        // OP25 source has 827 entries; `ambe_encoder.cc` computes
        // `b0_lmax = sizeof(b0_lookup) / sizeof(b0_lookup[0])` and
        // treats `b0_i > b0_lmax` as an error.
        assert_eq!(B0_LOOKUP.len(), 827);
    }

    #[test]
    fn table_is_monotonic_non_decreasing() {
        // Longer pitch → larger b0_lookup entry (lower f0 → later
        // b0). A regression in the dump ordering would show up as a
        // monotonicity break.
        for w in B0_LOOKUP.windows(2) {
            assert!(w[0] <= w[1], "non-monotonic entry: {w:?}");
        }
    }

    #[test]
    fn table_covers_full_b0_range() {
        assert_eq!(B0_LOOKUP[0], 0);
        assert_eq!(B0_LOOKUP[B0_LOOKUP.len() - 1], 119);
    }

    /// On an input whose `(ref_pitch >> 5) − 159` points at a slot
    /// Convert an `L_TABLE` float entry to the `usize` the walk
    /// compares against. Factored to centralise the one numeric cast.
    fn l_of(b0: u8) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "L_TABLE entries are whole positive integers in 9..=56 stored as f32; \
                      the f32-to-usize cast is exact within this range."
        )]
        let v = L_TABLE[b0 as usize] as usize;
        v
    }

    /// where `L_TABLE[b0] == target_l`, the walk exits immediately
    /// and returns the lookup's value.
    #[test]
    fn no_walk_needed_when_initial_b0_matches_target_l() {
        // Pick ref_pitch such that b0_lookup entry happens to match.
        // For ref_pitch = 0x1800 (period = 24.0 samples), b0_i = 0x1800>>5 - 159 = 192 - 159 = 33.
        // B0_LOOKUP[33] = 12. L_TABLE[12] = ? — whatever mbelib says.
        let ref_pitch_q8_8 = 0x1800_u32;
        let b0_i = (ref_pitch_q8_8 >> 5) as usize - 159;
        let start_b0 = B0_LOOKUP[b0_i];
        let start_l = l_of(start_b0);
        let chosen = pitch_index(ref_pitch_q8_8, start_l, &L_TABLE);
        assert_eq!(
            chosen, start_b0,
            "walk should be a no-op when L already matches"
        );
    }

    /// If the target L exceeds the initial slot's L, the walk must
    /// increment (not decrement) — corresponds to a longer-period
    /// pitch needing more harmonics.
    #[test]
    fn walk_increments_when_target_l_is_larger() {
        let ref_pitch_q8_8 = 0x1800_u32;
        let b0_i = (ref_pitch_q8_8 >> 5) as usize - 159;
        let start_b0 = B0_LOOKUP[b0_i];
        let start_l = l_of(start_b0);
        let chosen = pitch_index(ref_pitch_q8_8, start_l + 1, &L_TABLE);
        assert!(
            chosen > start_b0,
            "walk-up should produce larger b0; start={start_b0}, chosen={chosen}"
        );
    }
}
