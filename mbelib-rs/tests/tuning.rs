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

/// Eight consecutive voiced frames sampled from a real REF030
/// transmission (mid-utterance, FEC-clean), plus the D-STAR silence
/// constant to exercise the unvoiced/noise path.
const REAL_FRAMES: [[u8; 9]; 9] = [
    [0x3F, 0xC2, 0x86, 0x42, 0x79, 0x72, 0x47, 0xB4, 0x9C],
    [0x2F, 0x21, 0x0E, 0x62, 0x6B, 0x3E, 0x6F, 0x07, 0x90],
    [0x7A, 0xC6, 0x88, 0x3A, 0x2F, 0x11, 0xDB, 0xA6, 0x42],
    [0x1E, 0x02, 0x0C, 0x6A, 0x8E, 0xC4, 0xCC, 0x31, 0x90],
    [0x1B, 0x44, 0x9C, 0x6B, 0x9F, 0x50, 0x5A, 0x22, 0x80],
    [0x53, 0x03, 0x90, 0x3B, 0xEE, 0x84, 0x55, 0x26, 0x90],
    [0xD4, 0xEE, 0x1E, 0x18, 0x25, 0x3F, 0x72, 0x68, 0xA8],
    [0x5E, 0xA4, 0x1A, 0x52, 0x36, 0x45, 0x3C, 0x06, 0x80],
    [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8],
];

fn decode_all(dec: &mut AmbeDecoder) -> Vec<i16> {
    let mut out = Vec::new();
    for frame in &REAL_FRAMES {
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
