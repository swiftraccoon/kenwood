// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Parity: the Rust waveform-enhancement forward pass must reproduce
//! the training checkpoint's output on a recorded reference vector.

use mbelib_rs::enhance_wave::WaveEnhancer;

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use proptest as _;
use realfft as _;
use wide as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VEC: &[u8] = include_bytes!("fixtures/wave_testvec.bin");

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
        .collect()
}

#[test]
fn forward_pass_matches_training_checkpoint() -> TestResult {
    let n = u32::from_le_bytes(VEC.get(..4).ok_or("short header")?.try_into()?) as usize;
    let body = VEC.get(4..).ok_or("short body")?;
    let all = floats(body);
    let input = all.get(..n).ok_or("short input")?;
    let expected = all.get(n..2 * n).ok_or("short expected")?;

    let enhancer = WaveEnhancer::new()?;
    let output = enhancer.process_f32(input);
    assert_eq!(output.len(), expected.len());

    let mut max_diff = 0.0_f32;
    for (a, b) in output.iter().zip(expected) {
        max_diff = max_diff.max((a - b).abs());
    }
    // FP accumulation across two FFT implementations plus the erf
    // approximation; 1e-3 on unit-scale audio is −60 dBFS.
    assert!(
        max_diff < 1e-3,
        "max deviation {max_diff} exceeds parity tolerance"
    );
    Ok(())
}

#[test]
fn i16_path_round_trips_length_and_stays_finite() -> TestResult {
    let enhancer = WaveEnhancer::new()?;
    let pcm: Vec<i16> = (0..4000_i32)
        .map(|n| {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "sine amplitude 8000 fits i16"
            )]
            let s = ((f64::from(n) * 0.22).sin() * 8000.0) as i16;
            s
        })
        .collect();
    let out = enhancer.process(&pcm);
    assert_eq!(out.len(), pcm.len());
    Ok(())
}
