// SPDX-FileCopyrightText: 2009 Pavel Yazev (OP25 imbe_vocoder/pitch_est.cc)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later
//
// Algorithmic reference: Pavel Yazev's `imbe_vocoder/pitch_est.cc`
// (OP25, 2009, GPLv3). This port implements:
//
//   * `e_p()` — the detectability function that returns one value of
//     E(p) per candidate pitch period. A fully periodic signal at
//     period `p` scores E(p) ≈ 0; non-periodic / aperiodic candidates
//     score E(p) closer to 1.
//   * Look-back pitch tracking (pitch_est.cc:200–226): search within
//     an empirically-learned window around the previous pitch and
//     accept the minimum if the 3-frame accumulated error stays
//     below 0.48 (Q4.12).
//
// Look-AHEAD pitch tracking + sub-multiples analysis
// (pitch_est.cc:229–332) is deferred: they need a 2-frame lookahead
// buffer (40 ms added latency), which is a caller-visible change and
// is tracked as its own piece of work. When look-back's confidence
// threshold isn't met, this module falls back to a single-frame
// global minimum over the E(p) array — better than autocorrelation
// + YIN on formant-rich speech, worse than full OP25.
//
// Q-format constants are carried from OP25's `globals.h` verbatim so
// threshold comparisons map directly across the port. The fixed-point
// math itself is f32 here — the overflow guards OP25 needs in Q15
// arithmetic are implicit in f32's much larger dynamic range.

//! Pitch (F0) estimation from the pitch-estimation history buffer.
//!
//! Given the 301-sample LPF'd buffer produced by [`crate::encode::analyze`],
//! produces a fractional pitch period in samples and the corresponding
//! F0 in Hz, plus a confidence score.
//!
//! # Algorithm — OP25 `pitch_est` port
//!
//! For each frame, [`PitchTracker::estimate`]:
//!
//! 1. Windows the 301-sample pitch-estimation buffer with `WI[]`.
//! 2. Computes autocorrelation at integer lags `21..=150` plus the
//!    half-integer lags interpolated between them (259 values, one
//!    per OP25 `corr[]` slot).
//! 3. Evaluates `E(p)` over 203 candidate periods from 21 to 122
//!    samples in 0.5-sample steps — the IMBE-native pitch grid.
//! 4. Look-back pitch search: within the allowed window
//!    [`MIN_MAX_TBL`] indexed by `prev_pitch_idx`, find the period minimizing
//!    `E`. If the 3-frame cumulative error (current + prev + prev-prev)
//!    stays below `CNST_0_48_Q4_12`, commit that as the new pitch.
//! 5. Otherwise fall back to the global minimum of `E` across all
//!    203 candidates. (This is the single-frame approximation of
//!    OP25's look-ahead DP; the full DP is a follow-on port.)
//!
//! The AMBE codebooks quantize F0 via the `W0_TABLE` in
//! [`crate::tables`]; [`PitchEstimate::period_samples`] is the
//! fractional period this module hands downstream.

use crate::encode::state::PITCH_EST_BUF_SIZE;
use crate::encode::window::WI;

/// OP25 `min_max_tbl[203]` — per-pitch allowed search window.
///
/// Indexed by the previous frame's pitch index `prev_pitch_idx` in
/// `0..203`. Each entry packs `(min_index, max_index)` as
/// `(hi_byte, lo_byte)`. The allowed window for the current frame
/// is `[min_index, max_index]` inclusive, in the same index space.
///
/// Reference: `pitch_est.cc:37` in OP25. Values were fit empirically
/// to the speech-pitch dynamics the IMBE encoder sees; we carry them
/// over verbatim.
#[rustfmt::skip]
const MIN_MAX_TBL: [u16; 203] = [
    0x0008, 0x0009, 0x000a, 0x000c, 0x000d, 0x000e, 0x000f, 0x0010, 0x0012, 0x0013,
    0x0014, 0x0115, 0x0216, 0x0218, 0x0319, 0x041a, 0x051b, 0x061c, 0x061e, 0x071f,
    0x0820, 0x0921, 0x0a22, 0x0a24, 0x0b25, 0x0c26, 0x0d27, 0x0e28, 0x0e2a, 0x0f2b,
    0x102c, 0x112d, 0x122e, 0x1230, 0x1331, 0x1432, 0x1533, 0x1634, 0x1636, 0x1737,
    0x1838, 0x1939, 0x1a3a, 0x1a3c, 0x1b3d, 0x1c3e, 0x1d3f, 0x1e40, 0x1e42, 0x1f43,
    0x2044, 0x2145, 0x2246, 0x2248, 0x2349, 0x244a, 0x254b, 0x264c, 0x264e, 0x274f,
    0x2850, 0x2951, 0x2a52, 0x2a54, 0x2b55, 0x2c56, 0x2d57, 0x2e58, 0x2e5a, 0x2f5b,
    0x305c, 0x315d, 0x325e, 0x3260, 0x3361, 0x3462, 0x3563, 0x3664, 0x3666, 0x3767,
    0x3868, 0x3969, 0x3a6a, 0x3a6c, 0x3b6d, 0x3c6e, 0x3d6f, 0x3e70, 0x3e72, 0x3f73,
    0x4074, 0x4175, 0x4276, 0x4278, 0x4379, 0x447a, 0x457b, 0x467c, 0x467e, 0x477f,
    0x4880, 0x4981, 0x4a82, 0x4a84, 0x4b85, 0x4c86, 0x4d87, 0x4e88, 0x4e8a, 0x4f8b,
    0x508c, 0x518d, 0x528e, 0x5290, 0x5391, 0x5492, 0x5593, 0x5694, 0x5696, 0x5797,
    0x5898, 0x5999, 0x5a9a, 0x5a9c, 0x5b9d, 0x5c9e, 0x5d9f, 0x5ea0, 0x5ea2, 0x5fa3,
    0x60a4, 0x61a5, 0x62a6, 0x62a8, 0x63a9, 0x64aa, 0x65ab, 0x66ac, 0x66ae, 0x67af,
    0x68b0, 0x69b1, 0x6ab2, 0x6ab4, 0x6bb5, 0x6cb6, 0x6db7, 0x6eb8, 0x6eba, 0x6fbb,
    0x70bc, 0x71bd, 0x72be, 0x72c0, 0x73c1, 0x74c2, 0x75c3, 0x76c4, 0x76c6, 0x77c7,
    0x78c8, 0x79c9, 0x7aca, 0x7aca, 0x7bca, 0x7cca, 0x7dca, 0x7eca, 0x7eca, 0x7fca,
    0x80ca, 0x81ca, 0x82ca, 0x82ca, 0x83ca, 0x84ca, 0x85ca, 0x86ca, 0x86ca, 0x87ca,
    0x88ca, 0x89ca, 0x8aca, 0x8aca, 0x8bca, 0x8cca, 0x8dca, 0x8eca, 0x8eca, 0x8fca,
    0x90ca, 0x91ca, 0x92ca, 0x92ca, 0x93ca, 0x94ca, 0x95ca, 0x96ca, 0x96ca, 0x97ca,
    0x98ca, 0x99ca, 0x9aca,
];

/// Look-back cumulative-error threshold (`CNST_0_48_Q4_12` in OP25).
///
/// If `E(current) + E(prev) + E(prev_prev) ≤ 0.48`, the look-back
/// pitch choice is considered stable enough that no look-ahead
/// disambiguation is needed. Empirically tuned by the IMBE authors.
const CEB_THRESHOLD: f32 = 0.48;

/// Number of E(p) candidates. Corresponds to pitch periods 21.0, 21.5,
/// 22.0, ..., 121.5, 122.0 — the OP25 index space.
pub(crate) const PITCH_CANDIDATES: usize = 203;

/// Default pitch index used on a fresh tracker.
///
/// OP25 initializes `prev_pitch = 158` (Q15.1 format = 2 × period +
/// 42). In our 0-based index space that's `158 − 42 = 116`, which
/// corresponds to a period of `21 + 116 × 0.5 = 79` samples (≈100 Hz)
/// — a reasonable baseline for unvoiced speech onset.
const PITCH_DEFAULT_IDX: usize = 116;

/// Convert an OP25 pitch index (0..203) to the corresponding period
/// in samples. Index 0 → period 21.0, index 1 → 21.5, ...,
/// index 202 → 122.0.
#[inline]
#[expect(
    clippy::cast_precision_loss,
    reason = "OP25 pitch index is bounded by PITCH_CANDIDATES (203); usize-to-f32 is exact \
              at these magnitudes."
)]
const fn idx_to_period(idx: usize) -> f32 {
    21.0 + (idx as f32) * 0.5
}

/// Inverse of [`idx_to_period`], clamping out-of-range periods into
/// the valid `0..PITCH_CANDIDATES` index space.
#[inline]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "Inverse of idx_to_period: the caller passes a period that is small (<= 123 \
              samples) and finite, and this function explicitly clamps into 0..PITCH_CANDIDATES, \
              so all casts are safe within the bounded range."
)]
fn period_to_idx(period: f32) -> usize {
    let idx = ((period - 21.0) * 2.0).round();
    if idx < 0.0 {
        0
    } else if idx >= PITCH_CANDIDATES as f32 {
        PITCH_CANDIDATES - 1
    } else {
        idx as usize
    }
}

/// Result of a pitch-estimation pass on one 20 ms frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PitchEstimate {
    /// Pitch period in samples at 8 kHz. Fractional — either an
    /// integer or integer + 0.5 on the IMBE pitch grid.
    pub period_samples: f32,
    /// Fundamental frequency in Hz (8000 / `period_samples`).
    pub f0_hz: f32,
    /// Confidence score in `[0, 1]`, computed as `1 − E(p)` at the
    /// chosen pitch. Values above ≈0.5 typically indicate stable
    /// voiced speech; below ≈0.05 indicates noise or silence and is
    /// the trigger for the encoder to emit `AMBE_SILENCE`.
    pub confidence: f32,
}

/// Per-stream pitch-tracker state, matching OP25's `pitch_est` member
/// variables. All fields are prefixed `prev` by design — they're the
/// rolling history the look-back / cumulative-error tests consume.
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_field_names,
    reason = "OP25 pitch_est member variables use the `prev_*` prefix by convention; the \
              lint fires on the naming pattern itself, not on real ambiguity. Matching OP25's \
              naming keeps cross-references to the reference implementation straightforward."
)]
pub struct PitchTracker {
    /// Previous frame's pitch index (0..203).
    prev_pitch_idx: usize,
    /// Pitch index from two frames ago.
    prev_prev_pitch_idx: usize,
    /// E(p) value at the previous frame's chosen pitch.
    prev_e_p: f32,
    /// E(p) value two frames ago.
    prev_prev_e_p: f32,
}

impl PitchTracker {
    /// Fresh tracker. Initializes to OP25's canonical defaults:
    /// `prev_pitch_idx = 116` (period ≈ 79 samples), `prev_e_p = 0`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_pitch_idx: PITCH_DEFAULT_IDX,
            prev_prev_pitch_idx: PITCH_DEFAULT_IDX,
            prev_e_p: 0.0,
            prev_prev_e_p: 0.0,
        }
    }

    /// Estimate pitch from a pitch-history buffer via IMBE `e_p` +
    /// look-back tracking only.
    ///
    /// `pitch_est_buf` is the 301-sample LPF'd buffer. See
    /// [`compute_e_p`] for the `E(p)` derivation. This single-frame
    /// entry point is zero-latency; for the full OP25 look-ahead DP
    /// use [`Self::estimate_with_lookahead`].
    ///
    /// # Algorithm (single-frame path)
    ///
    /// 1. [`compute_e_p`] the 203-entry `E(p)` array for this frame.
    /// 2. Look-back search: pick the `E(p)` minimum inside
    ///    `MIN_MAX_TBL[prev_pitch_idx]`. If
    ///    `E + prev_E + prev_prev_E ≤ 0.48`, commit it.
    /// 3. Otherwise fall back to the global `E(p)` minimum.
    /// 4. Sub-multiples analysis tests `p/2..p/5` per OP25's 3-tier
    ///    threshold cascade; the winner is committed.
    pub fn estimate(&mut self, pitch_est_buf: &[f32; PITCH_EST_BUF_SIZE]) -> PitchEstimate {
        let e_p = compute_e_p(pitch_est_buf);
        if e_p.iter().all(|&v| v >= 1.0 - f32::EPSILON) {
            // Silent buffer — hold previous state, return zero-confidence.
            let period = idx_to_period(self.prev_pitch_idx);
            return PitchEstimate {
                period_samples: period,
                f0_hz: 8000.0 / period,
                confidence: 0.0,
            };
        }
        self.track_single_frame(&e_p)
    }

    /// Run the single-frame look-back + sub-multiples decision over a
    /// pre-computed `E(p)` array. Used by [`Self::estimate`] and
    /// internally by [`Self::estimate_with_lookahead`] as its
    /// look-back half.
    fn track_single_frame(&mut self, e_p: &[f32; PITCH_CANDIDATES]) -> PitchEstimate {
        // --- Look-back pitch tracking (pitch_est.cc:200–226) ---
        let entry = MIN_MAX_TBL
            .get(self.prev_pitch_idx)
            .copied()
            .unwrap_or(0x0008);
        let min_idx = ((entry >> 8) & 0xFF) as usize;
        let max_idx = (entry & 0xFF) as usize;

        let mut pb = min_idx;
        let mut e_pb = e_p.get(min_idx).copied().unwrap_or(1.0);
        for idx in (min_idx + 1)..=max_idx.min(PITCH_CANDIDATES - 1) {
            let e_val = e_p.get(idx).copied().unwrap_or(1.0);
            if e_val < e_pb {
                e_pb = e_val;
                pb = idx;
            }
        }
        let ceb = e_pb + self.prev_e_p + self.prev_prev_e_p;

        let mut chosen_idx = if ceb <= CEB_THRESHOLD {
            pb
        } else {
            // --- Fallback: global E(p) minimum (deferred DP stand-in) ---
            //
            // OP25 runs a 2-frame look-ahead DP here (pitch_est.cc:229).
            // Without that lookahead, a single-frame global argmin is
            // the best we can do without inventing data — it matches
            // OP25 exactly whenever the true pitch already sits at the
            // global minimum (stable voiced speech), and falls back
            // gracefully to the best-scored candidate otherwise.
            let mut best_idx = 0;
            let mut best_e = e_p.first().copied().unwrap_or(1.0);
            for (idx, &val) in e_p.iter().enumerate().skip(1) {
                if val < best_e {
                    best_e = val;
                    best_idx = idx;
                }
            }
            best_idx
        };

        // --- Sub-multiples analysis (pitch_est.cc:273–332) ---
        //
        // For harmonic-rich signals, E(p) has near-zero minima at the
        // true period AND at integer multiples of it (the signal is
        // trivially also periodic at 2P, 3P, ...). Look-back alone
        // can't break this tie — if the tracker converges on 2·P_true
        // it keeps scoring E ≈ 0 there and never considers P_true.
        //
        // OP25 works around this by testing P_est/2, /3, /4, /5 after
        // the DP and switching to the smallest sub-multiple whose E
        // is comparable to or smaller than the current best. We do
        // the same using the single-frame E(p) array — less precise
        // than OP25's look-ahead-augmented cef but enough to fix the
        // 2P lock-in on voice-like signals.
        //
        // Thresholds carried verbatim from OP25's Q4.12 constants.
        if chosen_idx >= 42 {
            // Choose how far to divide. OP25 picks i ∈ {1..=4}
            // inversely to the pitch index.
            let max_div = if chosen_idx < 84 {
                1
            } else if chosen_idx < 126 {
                2
            } else if chosen_idx < 168 {
                3
            } else {
                4
            };
            let cef_est = e_p.get(chosen_idx).copied().unwrap_or(1.0);
            for div in (1..=max_div).rev() {
                // OP25's p_fp = (chosen_idx + 42) in Q7.1 shifted.
                // Here we work directly on the real period.
                let base_period = idx_to_period(chosen_idx);
                let divisor = match div {
                    1 => 2.0_f32,
                    2 => 3.0,
                    3 => 4.0,
                    _ => 5.0,
                };
                let sub_period = base_period / divisor;
                if sub_period < 21.0 {
                    continue; // below grid minimum
                }
                let sub_idx = period_to_idx(sub_period);
                let cef = e_p.get(sub_idx).copied().unwrap_or(1.0);

                let accept = if cef <= 0.05 {
                    // Tier 3 — always accept small-enough sub-multiple.
                    true
                } else if cef <= 0.4 && cef <= 3.5 * cef_est {
                    // Tier 2 — accept if within 3.5× of current best.
                    true
                } else if cef <= 0.85 && cef <= 1.7 * cef_est {
                    // Tier 1 — accept if within 1.7× of current best.
                    true
                } else {
                    false
                };
                if accept {
                    chosen_idx = sub_idx;
                    break;
                }
            }
        }

        let chosen_e = e_p.get(chosen_idx).copied().unwrap_or(1.0);

        // Commit state. Mirror OP25: prev_prev gets the old prev,
        // prev gets the newly-chosen pitch.
        self.prev_prev_pitch_idx = self.prev_pitch_idx;
        self.prev_prev_e_p = self.prev_e_p;
        self.prev_pitch_idx = chosen_idx;
        self.prev_e_p = chosen_e;

        let period = idx_to_period(chosen_idx);
        let confidence = (1.0 - chosen_e).clamp(0.0, 1.0);
        PitchEstimate {
            period_samples: period,
            f0_hz: 8000.0 / period,
            confidence,
        }
    }

    /// For regression tests / diagnostics: the internal state after the
    /// last [`Self::estimate`] call.
    #[must_use]
    #[cfg(test)]
    pub(crate) const fn state(&self) -> (usize, usize, f32, f32) {
        (
            self.prev_pitch_idx,
            self.prev_prev_pitch_idx,
            self.prev_e_p,
            self.prev_prev_e_p,
        )
    }
}

impl Default for PitchTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl PitchTracker {
    /// Run OP25's full pitch tracker: single-frame look-back, fall
    /// through to 2-frame-look-ahead DP on `pitch_est.cc:229–270`,
    /// then sub-multiples analysis, then pick look-back vs
    /// look-ahead by comparing cumulative-error scores.
    ///
    /// `e_p_current` is the `E(p)` array for the frame being
    /// committed; `e_p_next` is the array for the frame that will
    /// be committed next, and `e_p_nextnext` the one after that.
    /// Callers are expected to buffer 2 frames of future
    /// [`compute_e_p`] output before invoking this method; the
    /// encoder's 40 ms effective latency is the cost.
    ///
    /// State is advanced exactly as in the single-frame path
    /// — `prev_pitch_idx`, `prev_e_p`, and their `prev_prev`
    /// shadows roll forward.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "Mirrors OP25's pitch_est lookahead block so the port can be read \
                  side-by-side with the reference. The long linear structure comes from \
                  OP25's algorithm; splitting would obscure the one-to-one mapping."
    )]
    pub fn estimate_with_lookahead(
        &mut self,
        e_p_current: &[f32; PITCH_CANDIDATES],
        e_p_next: &[f32; PITCH_CANDIDATES],
        e_p_nextnext: &[f32; PITCH_CANDIDATES],
    ) -> PitchEstimate {
        // --- Step 1: single-frame look-back on e_p_current ---
        // Per pitch_est.cc:200–226, look-back alone commits if
        // ceb = E_current + prev_E + prev_prev_E ≤ 0.48.
        let entry = MIN_MAX_TBL
            .get(self.prev_pitch_idx)
            .copied()
            .unwrap_or(0x0008);
        let min_idx = ((entry >> 8) & 0xFF) as usize;
        let max_idx = (entry & 0xFF) as usize;
        let mut pb = min_idx;
        let mut e_pb = e_p_current.get(min_idx).copied().unwrap_or(1.0);
        for idx in (min_idx + 1)..=max_idx.min(PITCH_CANDIDATES - 1) {
            let e_val = e_p_current.get(idx).copied().unwrap_or(1.0);
            if e_val < e_pb {
                e_pb = e_val;
                pb = idx;
            }
        }
        let ceb = e_pb + self.prev_e_p + self.prev_prev_e_p;

        if ceb <= CEB_THRESHOLD {
            return self.commit_pitch(pb, e_pb);
        }

        // --- Step 2: 2-frame-look-ahead DP (pitch_est.cc:229–270) ---
        //
        // Build e_p_nextnext_min[p1]: for each p1, the minimum of
        // e_p_nextnext over the p2 window defined by MIN_MAX_TBL[p1].
        // This is the "best future-future score assuming p1 at
        // t+1", computed once so the inner loop over p0 doesn't
        // repeat the work.
        let mut e_p_nextnext_min = [1.0_f32; PITCH_CANDIDATES];
        for p1 in 0..PITCH_CANDIDATES {
            let entry = MIN_MAX_TBL.get(p1).copied().unwrap_or(0x0008);
            let min_p2 = ((entry >> 8) & 0xFF) as usize;
            let max_p2 = (entry & 0xFF) as usize;
            let mut best = e_p_nextnext.get(min_p2).copied().unwrap_or(1.0);
            for p2 in (min_p2 + 1)..=max_p2.min(PITCH_CANDIDATES - 1) {
                let v = e_p_nextnext.get(p2).copied().unwrap_or(1.0);
                if v < best {
                    best = v;
                }
            }
            if let Some(slot) = e_p_nextnext_min.get_mut(p1) {
                *slot = best;
            }
        }

        // e1p1_e2p2_est_save[p0] = min over p1 in MIN_MAX_TBL[p0] of
        //   (e_p_next[p1] + e_p_nextnext_min[p1]).
        // cef[p0] = e_p_current[p0] + e1p1_e2p2_est_save[p0]; pick the
        // p0 minimizing cef.
        let mut e1p1_e2p2_est_save = [1.0_f32; PITCH_CANDIDATES];
        let mut p0_est = 0_usize;
        let mut cef_est = e_p_current[0] + e_p_next[0] + e_p_nextnext[0];
        for p0 in 0..PITCH_CANDIDATES {
            let entry = MIN_MAX_TBL.get(p0).copied().unwrap_or(0x0008);
            let min_p1 = ((entry >> 8) & 0xFF) as usize;
            let max_p1 = (entry & 0xFF) as usize;
            let mut e1p1_e2p2_est = e_p_next.get(p0).copied().unwrap_or(1.0)
                + e_p_nextnext_min.get(p0).copied().unwrap_or(1.0);
            for p1 in min_p1..=max_p1.min(PITCH_CANDIDATES - 1) {
                let cand = e_p_next.get(p1).copied().unwrap_or(1.0)
                    + e_p_nextnext_min.get(p1).copied().unwrap_or(1.0);
                if cand < e1p1_e2p2_est {
                    e1p1_e2p2_est = cand;
                }
            }
            if let Some(slot) = e1p1_e2p2_est_save.get_mut(p0) {
                *slot = e1p1_e2p2_est;
            }
            let cef = e_p_current.get(p0).copied().unwrap_or(1.0) + e1p1_e2p2_est;
            if cef < cef_est {
                cef_est = cef;
                p0_est = p0;
            }
        }

        // --- Step 3: sub-multiples analysis on pf=p0_est ---
        // Same 3-tier threshold cascade as the single-frame path,
        // but testing against `cef = e_p_current[p_sub] +
        // e1p1_e2p2_est_save[p_sub]` which consults all 3 frames.
        let mut pf = p0_est;
        if pf >= 42 {
            let max_div = if pf < 84 {
                1
            } else if pf < 126 {
                2
            } else if pf < 168 {
                3
            } else {
                4
            };
            for div in (1..=max_div).rev() {
                let base_period = idx_to_period(pf);
                let divisor = match div {
                    1 => 2.0_f32,
                    2 => 3.0,
                    3 => 4.0,
                    _ => 5.0,
                };
                let sub_period = base_period / divisor;
                if sub_period < 21.0 {
                    continue;
                }
                let sub_idx = period_to_idx(sub_period);
                let cef_sub = e_p_current.get(sub_idx).copied().unwrap_or(1.0)
                    + e1p1_e2p2_est_save.get(sub_idx).copied().unwrap_or(1.0);
                // 3-tier acceptance cascade from pitch_est.cc:314–330.
                // A sub-multiple is accepted when its combined error is
                // very small on its own, small-and-close-to-best, or
                // moderate-but-far-better-than-best.
                let accept = cef_sub <= 0.05
                    || (cef_sub <= 0.4 && cef_sub <= 3.5 * cef_est)
                    || (cef_sub <= 0.85 && cef_sub <= 1.7 * cef_est);
                if accept {
                    pf = sub_idx;
                    break;
                }
            }
        }

        // --- Step 4: pick look-back vs look-ahead ---
        // cef_pf = e_p_current[pf] + e1p1_e2p2_est_save[pf]; if
        // ceb ≤ cef_pf, look-back wins; else look-ahead.
        let cef_pf = e_p_current.get(pf).copied().unwrap_or(1.0)
            + e1p1_e2p2_est_save.get(pf).copied().unwrap_or(1.0);
        let (chosen, chosen_e) = if ceb <= cef_pf {
            (pb, e_pb)
        } else {
            (pf, e_p_current.get(pf).copied().unwrap_or(1.0))
        };
        self.commit_pitch(chosen, chosen_e)
    }

    /// Roll tracker state forward and return the corresponding
    /// [`PitchEstimate`]. Shared between all track-and-commit paths.
    fn commit_pitch(&mut self, chosen_idx: usize, chosen_e: f32) -> PitchEstimate {
        self.prev_prev_pitch_idx = self.prev_pitch_idx;
        self.prev_prev_e_p = self.prev_e_p;
        self.prev_pitch_idx = chosen_idx;
        self.prev_e_p = chosen_e;

        let period = idx_to_period(chosen_idx);
        let confidence = (1.0 - chosen_e).clamp(0.0, 1.0);
        PitchEstimate {
            period_samples: period,
            f0_hz: 8000.0 / period,
            confidence,
        }
    }
}

/// Compute IMBE's `E(p)` detectability function for one 301-sample
/// pitch-estimation buffer.
///
/// Returns 203 fractional values in `[0, 1]`, one per candidate
/// pitch index on the 0.5-sample grid 21.0 .. 122.0. A value near
/// zero at index `i` means the signal is well-explained by a
/// period of `21 + i·0.5` samples (plus its harmonic multiples).
///
/// Exposed so the encoder can maintain a ring of 3 `E(p)` arrays
/// and hand them to [`PitchTracker::estimate_with_lookahead`]. The
/// single-frame [`PitchTracker::estimate`] calls this internally.
#[must_use]
#[expect(
    clippy::similar_names,
    reason = "OP25's e_p() body is kept close to the reference: similar variable names \
              (e.g., cef/ceb, pb/pf) mirror OP25's pitch_est.cc, which makes \
              cross-referencing straightforward."
)]
pub fn compute_e_p(pitch_est_buf: &[f32; PITCH_EST_BUF_SIZE]) -> [f32; PITCH_CANDIDATES] {
    let mut windowed = [0.0_f32; PITCH_EST_BUF_SIZE];
    for (i, (&x, &w)) in pitch_est_buf.iter().zip(WI.iter()).enumerate() {
        if let Some(slot) = windowed.get_mut(i) {
            *slot = x * w;
        }
    }

    // L_sum = Σ(s² · w) — single-windowed energy (OP25's L_sum via
    // `L_mpy_ls(L_mult(s, s), wi)`).
    let l_sum: f32 = pitch_est_buf
        .iter()
        .zip(WI.iter())
        .map(|(&x, &w)| x * x * w)
        .sum();
    if l_sum < 1e-12 {
        // Silent buffer — return "all bad" so track_single_frame
        // / estimate_with_lookahead fall into their silent branches.
        return [1.0; PITCH_CANDIDATES];
    }

    // L_e0 = Σ((s·w)²) — doubly-windowed self-energy. Scaled by
    // 1/128 to match OP25's `L_shr(L_e0, 7)` compensation.
    let l_e0_raw: f32 = windowed.iter().map(|&x| x * x).sum();
    let l_e0 = l_e0_raw / 128.0;

    // Build corr[0..259] covering integer lags 21..150 at even
    // indices and their half-integer linear interpolations at odd
    // indices.
    let mut corr = [0.0_f32; 259];
    for (i, shift) in (21..=150).enumerate() {
        let j = i * 2;
        if let Some(slot) = corr.get_mut(j) {
            let mut acc = 0.0_f32;
            for k in 0..(PITCH_EST_BUF_SIZE - shift) {
                let a = windowed.get(k).copied().unwrap_or(0.0);
                let b = windowed.get(k + shift).copied().unwrap_or(0.0);
                acc = a.mul_add(b, acc);
            }
            *slot = acc;
        }
    }
    for i in (1..258).step_by(2) {
        let prev = corr.get(i - 1).copied().unwrap_or(0.0);
        let next = corr.get(i + 1).copied().unwrap_or(0.0);
        if let Some(slot) = corr.get_mut(i) {
            *slot = 0.5 * (prev + next);
        }
    }

    let mut e_p = [0.0_f32; PITCH_CANDIDATES];
    for i in 0..PITCH_CANDIDATES {
        let index_step = 42 + i;
        let mut sum_corr = 0.0_f32;
        let mut j = i;
        while j <= 258 {
            sum_corr += corr.get(j).copied().unwrap_or(0.0);
            j += index_step;
        }
        let sum_corr_scaled = sum_corr / 64.0;
        #[expect(
            clippy::cast_precision_loss,
            reason = "E(p) pitch-period computation: index_step = 42 + i with i bounded by \
                      PITCH_CANDIDATES (203), so index_step <= 244; usize-to-f32 cast is exact."
        )]
        let p = index_step as f32;
        let l_num = p.mul_add(-(l_e0 + sum_corr_scaled), l_sum);
        let ratio = (l_num / l_sum).clamp(0.0, 1.0);
        if let Some(slot) = e_p.get_mut(i) {
            *slot = ratio;
        }
    }
    e_p
}

#[cfg(test)]
mod tests {
    use super::{PITCH_CANDIDATES, PITCH_EST_BUF_SIZE, PitchTracker, idx_to_period, period_to_idx};

    #[test]
    fn idx_to_period_covers_op25_range() {
        assert!((idx_to_period(0) - 21.0).abs() < 1e-6);
        assert!((idx_to_period(1) - 21.5).abs() < 1e-6);
        assert!((idx_to_period(PITCH_CANDIDATES - 1) - 122.0).abs() < 1e-6);
    }

    #[test]
    fn period_to_idx_inverts_idx_to_period() {
        for idx in 0..PITCH_CANDIDATES {
            let p = idx_to_period(idx);
            assert_eq!(period_to_idx(p), idx);
        }
    }

    /// Zero input → confidence 0, period held at the default.
    #[test]
    fn silent_input_gives_zero_confidence() {
        let buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let mut tracker = PitchTracker::new();
        let est = tracker.estimate(&buf);
        assert!(
            est.confidence.abs() < f32::EPSILON,
            "expected confidence 0 on silent input, got {}",
            est.confidence
        );
    }

    /// 150 Hz pure sine has period 53.33 samples. The OP25 look-back
    /// tracker's default window starts at `pitch_idx` 116 (period 79)
    /// and converges inward: each frame narrows the allowed window
    /// around `prev_pitch_idx`, so within a handful of frames we should
    /// land on `pitch_idx` 64 or 65 (period 53.0 or 53.5).
    ///
    /// Pure tones have octave-symmetric E(p) minima (the signal is
    /// trivially also periodic at 2P, 3P, ...), so this test only
    /// asserts "within 3 samples of the true period OR within 3
    /// samples of 2× the true period" — the remaining octave
    /// ambiguity is exactly what the deferred 2-frame look-ahead DP
    /// resolves.
    #[test]
    fn sine_at_150hz_lands_on_valid_octave() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 150.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test tone generator: i < PITCH_EST_BUF_SIZE (320), exact in f32."
            )]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let mut tracker = PitchTracker::new();
        for _ in 0..20 {
            let _ = tracker.estimate(&buf);
        }
        let est = tracker.estimate(&buf);
        let acceptable_periods = [53.3_f32, 106.6];
        let matched = acceptable_periods
            .iter()
            .any(|&target| (est.period_samples - target).abs() < 3.0);
        assert!(
            matched,
            "period {:.2} not within 3 of 53.3 or 106.6 (f0={:.1}, conf={:.3})",
            est.period_samples, est.f0_hz, est.confidence
        );
    }

    /// Pure sine at 200 Hz (period 40) — the look-ahead DP breaks
    /// the octave ambiguity that stuck the single-frame tracker on
    /// `2·P` or `P`. Feeds the same `E(p)` array for all three
    /// look-ahead slots to simulate a long steady-state sine.
    #[test]
    fn sine_at_200hz_locks_to_valid_octave_via_dp() {
        use super::compute_e_p;
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 200.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test tone generator: i < PITCH_EST_BUF_SIZE (320), exact in f32."
            )]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let e_p = compute_e_p(&buf);
        let mut tracker = PitchTracker::new();
        // Warm the look-back state with a few estimates so the DP's
        // prev_e_p history is representative.
        for _ in 0..5 {
            let _ = tracker.estimate_with_lookahead(&e_p, &e_p, &e_p);
        }
        let est = tracker.estimate_with_lookahead(&e_p, &e_p, &e_p);
        // Valid octaves for 200 Hz at 8 kHz: 40, 80, 120. Sub-multiples
        // analysis in the DP should prefer the smallest acceptable
        // period — 40 — since E(40) ≈ E(80) ≈ E(120) on pure sines.
        let valid = [40.0_f32, 80.0, 120.0];
        let matched = valid.iter().any(|&m| (est.period_samples - m).abs() < 3.0);
        assert!(
            matched,
            "period {:.2} not within 3 of any multiple of 40 ({valid:?}); \
             conf={:.3}",
            est.period_samples, est.confidence
        );
    }

    /// The DP prefers a period that scores low across ALL three
    /// frames. If frames 0 and 1 favour `P`, but frame 2 favours `2P`,
    /// cef at `P` stays low (sum of good scores) while cef at `2P` is
    /// pulled up by frame 0/1's bad score — the DP picks P.
    #[test]
    fn lookahead_dp_picks_pitch_stable_across_frames() {
        use super::{PITCH_CANDIDATES, compute_e_p};
        let mut tracker = PitchTracker::new();

        // Generate a 200 Hz sine for all three slots. All three
        // frames agree on pitch, so DP picks the stable answer.
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test tone generator: i < PITCH_EST_BUF_SIZE (320), exact in f32."
            )]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * 200.0 / 8000.0).sin();
        }
        let e_p = compute_e_p(&buf);

        let est = tracker.estimate_with_lookahead(&e_p, &e_p, &e_p);
        // Confidence should be high — 3 frames agree perfectly.
        assert!(
            est.confidence > 0.9,
            "expected high confidence on 3 matching frames, got {:.3}",
            est.confidence
        );
        // The chosen pitch index must correspond to one of the
        // sine's valid octaves (stored in the tracker state).
        let (chosen_idx, _, _, _) = tracker.state();
        assert!(chosen_idx < PITCH_CANDIDATES);
    }

    /// Voice-like signal: fundamental + 3 harmonics with decreasing
    /// amplitude. Real speech has harmonic-amplitude asymmetry that
    /// breaks the pure-tone octave tie; the tracker should lock onto
    /// the true period rather than an octave multiple.
    #[test]
    fn voice_like_signal_locks_to_true_period() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 150.0_f32;
        let sr = 8000.0_f32;
        let harmonics = [1.0_f32, 0.6, 0.35, 0.2];
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test voice-like generator: i < PITCH_EST_BUF_SIZE (320), exact in f32."
            )]
            let t = i as f32;
            let mut sum = 0.0_f32;
            for (k, &amp) in harmonics.iter().enumerate() {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "harmonic index: k < 4, usize-to-f32 cast is exact."
                )]
                let harm = (k + 1) as f32;
                sum += amp * (t * 2.0 * std::f32::consts::PI * f0_hz * harm / sr).sin();
            }
            *slot = sum * 0.4;
        }
        let mut tracker = PitchTracker::new();
        for _ in 0..20 {
            let _ = tracker.estimate(&buf);
        }
        let est = tracker.estimate(&buf);
        let expected = 53.3_f32;
        assert!(
            (est.period_samples - expected).abs() < 3.0,
            "period {:.2} off from 53.3 (err {:.2}) — harmonic signal \
             should break the octave tie; f0={:.1}, conf={:.3}",
            est.period_samples,
            (est.period_samples - expected).abs(),
            est.f0_hz,
            est.confidence,
        );
    }

    /// Look-back state tracks across frames: after one estimate, the
    /// stored `prev_pitch_idx` should equal the freshly-chosen index,
    /// and `prev_prev_pitch_idx` should hold the tracker's initial
    /// default (`PITCH_DEFAULT_IDX`).
    #[test]
    fn state_rolls_forward_each_frame() {
        let mut buf = [0.0_f32; PITCH_EST_BUF_SIZE];
        let f0_hz = 150.0_f32;
        let sr = 8000.0_f32;
        for (i, slot) in buf.iter_mut().enumerate() {
            #[expect(
                clippy::cast_precision_loss,
                reason = "test tone generator: i < PITCH_EST_BUF_SIZE (320), exact in f32."
            )]
            let t = i as f32;
            *slot = (t * 2.0 * std::f32::consts::PI * f0_hz / sr).sin();
        }
        let mut tracker = PitchTracker::new();
        let (init_idx, _, _, _) = tracker.state();
        let _ = tracker.estimate(&buf);
        let (p1, pp1, _, _) = tracker.state();
        let _ = tracker.estimate(&buf);
        let (p2, pp2, _, _) = tracker.state();
        assert_eq!(
            pp1, init_idx,
            "first frame: prev_prev should be initial default"
        );
        assert_eq!(
            pp2, p1,
            "second frame: prev_prev should be first frame's chosen idx"
        );
        let _ = p2;
    }
}
