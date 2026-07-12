// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Synthesis tuning seam: the default configuration is bit-exact with
//! the untuned decoder (mbelib/JMBE parity), and each knob actually
//! reaches its synthesis stage.

use mbelib_rs::{AmbeDecoder, SynthesisTuning};

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use proptest as _;
use realfft as _;
use wide as _;

/// Eight consecutive frames sampled from a real REF030 transmission
/// straddling a voiced/unvoiced transition (mixed-voicing frames are
/// required for the unvoiced-gain and phase-jitter knobs to have any
/// effect), plus the D-STAR silence constant.
const REAL_FRAMES: [[u8; 9]; 9] = [
    [0xF8, 0x98, 0x96, 0x1A, 0x62, 0x7B, 0xC0, 0x3C, 0x90],
    [0xF8, 0x5C, 0x1F, 0xE5, 0x7B, 0x81, 0xF1, 0xB0, 0x85],
    [0xDD, 0xBD, 0x55, 0xFD, 0x7F, 0x33, 0x72, 0x87, 0x8B],
    [0xF1, 0x7A, 0xD3, 0xDF, 0xFB, 0x7A, 0xC8, 0x71, 0xB6],
    [0xE8, 0x5D, 0xCF, 0xE5, 0x48, 0x73, 0xA3, 0x55, 0xBB],
    [0xC8, 0x5E, 0x57, 0xAC, 0xD8, 0xA4, 0x9A, 0x2D, 0x42],
    [0xE4, 0x1C, 0xC5, 0xD5, 0x18, 0xFE, 0xB3, 0x17, 0xB0],
    [0xA5, 0x5B, 0xC3, 0x76, 0x78, 0xF5, 0xB3, 0x12, 0x21],
    [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8],
];

fn decode_all(dec: &mut AmbeDecoder) -> Vec<i16> {
    // Two passes so the mixed/unvoiced silence frame sits mid-stream:
    // phase-jitter effects only manifest in voiced frames that FOLLOW
    // a frame with unvoiced bands.
    let mut out = Vec::new();
    for frame in REAL_FRAMES.iter().chain(REAL_FRAMES.iter()) {
        out.extend_from_slice(&dec.decode_frame(frame));
    }
    out
}

#[test]
fn default_tuning_is_bit_exact_with_untuned_decoder() {
    let mut plain = AmbeDecoder::new();
    let mut tuned = AmbeDecoder::with_tuning(SynthesisTuning::default());
    assert_eq!(
        decode_all(&mut plain),
        decode_all(&mut tuned),
        "SynthesisTuning::default() must reproduce parity output exactly"
    );
}

#[test]
fn parity_constant_is_the_default() {
    assert_eq!(SynthesisTuning::PARITY, SynthesisTuning::default());
}

#[test]
fn unvoiced_gain_reaches_the_excitation() {
    let mut plain = AmbeDecoder::new();
    let mut tuned = AmbeDecoder::with_tuning(SynthesisTuning {
        unvoiced_gain: 0.5,
        ..SynthesisTuning::default()
    });
    assert_ne!(
        decode_all(&mut plain),
        decode_all(&mut tuned),
        "halving unvoiced_gain must change synthesized PCM"
    );
}

#[test]
fn enhancement_knobs_reach_the_spectral_weighting() {
    let plain = {
        let mut d = AmbeDecoder::new();
        decode_all(&mut d)
    };
    for tuning in [
        SynthesisTuning {
            enhance_alpha: 1.5,
            ..SynthesisTuning::default()
        },
        SynthesisTuning {
            enhance_exponent: 0.4,
            ..SynthesisTuning::default()
        },
        SynthesisTuning {
            enhance_clamp_hi: 2.0,
            ..SynthesisTuning::default()
        },
        SynthesisTuning {
            phase_jitter: 0.0,
            ..SynthesisTuning::default()
        },
    ] {
        let mut d = AmbeDecoder::with_tuning(tuning);
        assert_ne!(
            plain,
            decode_all(&mut d),
            "tuning {tuning:?} must change synthesized PCM"
        );
    }
}

#[test]
fn tuned_decoding_stays_deterministic() {
    let tuning = SynthesisTuning {
        enhance_alpha: 1.1,
        unvoiced_gain: 0.8,
        ..SynthesisTuning::default()
    };
    let mut a = AmbeDecoder::with_tuning(tuning);
    let mut b = AmbeDecoder::with_tuning(tuning);
    assert_eq!(decode_all(&mut a), decode_all(&mut b));
}
