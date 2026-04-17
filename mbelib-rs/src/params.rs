// SPDX-FileCopyrightText: 2010 szechyjs (mbelib)
// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! AMBE harmonic speech model parameters.
//!
//! The AMBE 3600×2450 codec models speech as a sum of harmonically
//! related sinusoids at integer multiples of a fundamental frequency.
//! Each harmonic band is independently classified as voiced (periodic
//! oscillator) or unvoiced (noise-like). The spectral envelope is
//! described by per-band magnitudes.
//!
//! This module defines the [`MbeParams`] struct that carries these
//! parameters across consecutive frames. The codec uses inter-frame
//! delta coding for gain and magnitudes, so three parameter snapshots
//! are needed: current, previous (prediction reference), and
//! previous-enhanced (synthesis cross-fade source).
//!
//! Field layout, naming, and the 1-based-indexing convention all mirror
//! the `mbe_parms` struct from mbelib's `mbelib.h`
//! (<https://github.com/szechyjs/mbelib>), ISC license. Index 0 is
//! unused padding to allow direct indexing by harmonic band number
//! (band 1 through band L) matching the AMBE specification notation.

/// Maximum number of harmonic bands the codec supports.
///
/// The AMBE 3600×2450 codec produces 9 to 56 harmonic bands depending
/// on the fundamental frequency (lower pitch = more bands). Arrays are
/// dimensioned to 57 entries (indices 0..=56) to allow direct 1-based
/// indexing matching the codec specification.
pub(crate) const MAX_BANDS: usize = 57;

/// Decoded parameters from a single AMBE voice frame.
///
/// These parameters describe the harmonic speech model used by the
/// AMBE 3600×2450 codec. They are populated by the parameter decoder
/// (`decode.rs`), refined by spectral enhancement (`enhance.rs`), and
/// consumed by the speech synthesizer (`synthesize.rs`).
///
/// # Array Indexing Convention
///
/// All per-band arrays (`vl`, `ml`, `log2_ml`, `phi_l`, `psi_l`) use
/// 1-based indexing: valid bands are `1..=l` where `l` is the harmonic
/// count. Index 0 is unused padding. This matches the AMBE
/// specification notation and the C reference implementation.
///
/// # Inter-Frame State
///
/// The codec uses delta coding: gain (`gamma`) is decoded as a delta
/// from the previous frame, and spectral magnitudes are interpolated
/// between frames. The `copy_from` method supports the snapshot
/// mechanism needed for this prediction chain.
#[derive(Debug, Clone)]
pub(crate) struct MbeParams {
    /// Fundamental radian frequency ω₀ = 2π·f₀.
    ///
    /// Determines the spacing between harmonic bands. Lower values
    /// correspond to higher-pitched voices (fewer harmonics needed to
    /// span the 4 kHz audio bandwidth). Decoded from the b0 parameter
    /// via the W0 lookup table.
    pub(crate) w0: f32,

    /// Number of harmonic bands (L), range 9..=56.
    ///
    /// Derived from the fundamental frequency: L = floor(π / ω₀).
    /// Lower-pitched voices produce more harmonics. Decoded from b0
    /// via the L lookup table.
    pub(crate) l: usize,

    /// Voiced/unvoiced decision per harmonic band.
    ///
    /// `vl[band] == true` means band `band` is voiced (synthesized as
    /// a deterministic cosine oscillator). `false` means unvoiced
    /// (synthesized as random-phase noise). Decoded from the b1
    /// parameter via the VUV decision table.
    pub(crate) vl: [bool; MAX_BANDS],

    /// Spectral magnitude per harmonic band (linear scale).
    ///
    /// `ml[band]` is the amplitude of the sinusoid at frequency
    /// `band * ω₀`. Computed from `log2_ml` via exponentiation after
    /// the full decode pipeline (PRBA dequantization + IDCT +
    /// interpolation + gamma scaling).
    pub(crate) ml: [f32; MAX_BANDS],

    /// Log₂ of spectral magnitude per harmonic band.
    ///
    /// The codec operates on log-magnitudes internally because the
    /// PRBA (Predictive Residual Block Average) quantizer and the
    /// inter-frame gain delta both work in the log domain. Linear
    /// magnitudes `ml` are derived from these via `2^(log2_ml)`.
    pub(crate) log2_ml: [f32; MAX_BANDS],

    /// Absolute phase per harmonic band (radians).
    ///
    /// Updated each frame using the phase continuity equation:
    /// `φ[l] = φ_prev[l] + ω₀·l·N` where N=160 is the frame length.
    /// For unvoiced bands, a random phase offset is injected.
    pub(crate) phi_l: [f32; MAX_BANDS],

    /// Smoothed (predicted) phase per harmonic band (radians).
    ///
    /// Used during synthesis for the cross-fade between the previous
    /// and current frames. The synthesis window (Ws) interpolates
    /// between `psi_l` from the previous enhanced frame and `phi_l`
    /// from the current frame.
    pub(crate) psi_l: [f32; MAX_BANDS],

    /// Adaptive smoothing gain factor (γ).
    ///
    /// Controls the inter-frame gain trajectory. Decoded as a delta:
    /// `γ_cur = Δγ(b2) + 0.5 · γ_prev`. The 0.5 coefficient provides
    /// first-order smoothing that prevents abrupt level changes.
    pub(crate) gamma: f32,

    // === JMBE adaptive smoothing state (algorithms #111-116) ===
    /// IIR-smoothed local energy estimate (Algorithm #111).
    ///
    /// Tracks the per-frame total spectral energy with α=0.95/β=0.05
    /// smoothing. Used as the input to the adaptive voicing threshold
    /// `VM` in Algorithm #112. Initialized to 75000.0 to match JMBE.
    pub(crate) local_energy: f32,

    /// Adaptive amplitude threshold `Tm` (Algorithm #115).
    ///
    /// The maximum total amplitude (`Σ Ml`) allowed for the current
    /// frame. When exceeded, all magnitudes are scaled down (Algorithm
    /// #116). Default 20480 matches JMBE.
    pub(crate) amplitude_threshold: i32,

    /// Channel error rate from the FEC decoder, in [0, 1].
    ///
    /// Used to gate adaptive smoothing entry (>1.25%) and frame muting
    /// (>9.6% for AMBE, >8.75% for IMBE). Updated by the AMBE decoder's
    /// Golay/Hamming syndrome counts.
    pub(crate) error_rate: f32,

    /// Total bit errors detected by FEC across all four codewords.
    pub(crate) error_count_total: i32,

    /// Count of consecutive frames where C0 was uncorrectable (C0 Golay
    /// errors > 3) and the decoder reused the previous frame's
    /// parameters. Reset to 0 on any successful decode.
    ///
    /// When this reaches 3, the decoder emits comfort noise instead of
    /// synthesized speech to avoid sustained artifacts from stale state.
    pub(crate) repeat_count: i32,

    // === JMBE FFT-based unvoiced synthesis state (algorithms #117-126) ===
    /// Previous frame's 256-sample IFFT output, retained for WOLA combine.
    pub(crate) previous_uw: [f32; 256],

    /// LCG state for JMBE-compatible noise generation.
    ///
    /// `x' = (171 · x + 11213) mod 53125`. The negative sentinel
    /// `-1.0` indicates cold start: the next noise frame returns all
    /// zeros and primes the LCG with `MBE_LCG_DEFAULT_SEED` (3147.0).
    pub(crate) noise_seed: f32,

    /// First 96 samples of the previous noise buffer, prepended to the
    /// current frame's noise to maintain continuity across frame
    /// boundaries (avoids audible discontinuities).
    pub(crate) noise_overlap: [f32; 96],
}

impl MbeParams {
    /// Creates a zeroed parameter set representing silence.
    ///
    /// All frequencies, magnitudes, and phases are zero. This is the
    /// starting state for a new voice stream — the first frame's delta
    /// decoding will use these zeros as the prediction reference.
    pub(crate) const fn new() -> Self {
        Self {
            w0: 0.0,
            l: 0,
            vl: [false; MAX_BANDS],
            ml: [0.0; MAX_BANDS],
            log2_ml: [0.0; MAX_BANDS],
            phi_l: [0.0; MAX_BANDS],
            psi_l: [0.0; MAX_BANDS],
            gamma: 0.0,
            // JMBE adaptive smoothing defaults (mbe_initMbeParms).
            local_energy: 75000.0,
            amplitude_threshold: 20480,
            error_rate: 0.0,
            error_count_total: 0,
            repeat_count: 0,
            // JMBE FFT-unvoiced state.
            previous_uw: [0.0; 256],
            noise_seed: -1.0, // cold-start sentinel
            noise_overlap: [0.0; 96],
        }
    }

    /// Copies all parameter fields from `src` into `self`.
    ///
    /// Used for the three-snapshot mechanism in the decode pipeline:
    /// `cur → prev` (prediction reference) and `cur → prev_enhanced`
    /// (synthesis cross-fade source) at different points in the
    /// pipeline.
    pub(crate) const fn copy_from(&mut self, src: &Self) {
        self.w0 = src.w0;
        self.l = src.l;
        self.vl = src.vl;
        self.ml = src.ml;
        self.log2_ml = src.log2_ml;
        self.phi_l = src.phi_l;
        self.psi_l = src.psi_l;
        self.gamma = src.gamma;
        self.local_energy = src.local_energy;
        self.amplitude_threshold = src.amplitude_threshold;
        self.error_rate = src.error_rate;
        self.error_count_total = src.error_count_total;
        self.repeat_count = src.repeat_count;
        self.previous_uw = src.previous_uw;
        self.noise_seed = src.noise_seed;
        self.noise_overlap = src.noise_overlap;
    }
}
