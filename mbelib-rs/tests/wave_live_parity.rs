// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Parity: the live (causal) waveform-enhancement forward pass must
//! reproduce the training checkpoint's output on a recorded reference
//! vector, and the streaming path must reproduce the batch path
//! sample for sample.

use mbelib_rs::enhance_live::LiveWaveEnhancer;

// Compilation-unit dep acknowledgements (unused_crate_dependencies):
use proptest as _;
use realfft as _;
use wide as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VEC: &[u8] = include_bytes!("fixtures/wave_live_testvec.bin");

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap_or([0; 4])))
        .collect()
}

fn fixture() -> Result<(Vec<f32>, Vec<f32>), Box<dyn std::error::Error>> {
    let n = u32::from_le_bytes(VEC.get(..4).ok_or("short header")?.try_into()?) as usize;
    let body = VEC.get(4..).ok_or("short body")?;
    let all = floats(body);
    let input = all.get(..n).ok_or("short input")?.to_vec();
    let expected = all.get(n..2 * n).ok_or("short expected")?.to_vec();
    Ok((input, expected))
}

/// The batch path's output-side conversion, for building an `i16`
/// fixture input from the recorded float vector.
fn to_i16(v: f32) -> i16 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to i16 range before the cast"
    )]
    let s = (v * 32_768.0).clamp(-32_767.0, 32_767.0) as i16;
    s
}

#[test]
fn forward_pass_matches_training_checkpoint() -> TestResult {
    let (input, expected) = fixture()?;
    let enhancer = LiveWaveEnhancer::new()?;
    let output = enhancer.process_f32(&input);
    assert_eq!(output.len(), expected.len());

    let mut max_diff = 0.0_f32;
    for (a, b) in output.iter().zip(&expected) {
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
fn streaming_i16_frames_match_batch_process() -> TestResult {
    let (input, _) = fixture()?;
    let pcm: Vec<i16> = input.iter().copied().map(to_i16).collect();
    assert_eq!(pcm.len() % 160, 0, "fixture must divide into 20 ms frames");

    let enhancer = LiveWaveEnhancer::new()?;
    let batch = enhancer.process(&pcm);

    let mut stream = enhancer.stream();
    let mut streamed: Vec<i16> = Vec::new();
    for frame in pcm.chunks_exact(160) {
        let frame: &[i16; 160] = frame.try_into()?;
        streamed.extend(stream.push_frame(frame));
    }
    streamed.extend(stream.finish());

    assert_eq!(streamed.len(), pcm.len(), "stream must preserve length");
    let mut max_lsb = 0_i32;
    for (a, b) in streamed.iter().zip(&batch) {
        max_lsb = max_lsb.max((i32::from(*a) - i32::from(*b)).abs());
    }
    // The stream re-runs the batch arithmetic per spectral column in
    // the identical accumulation order, so it lands on the same f32
    // values before the shared i16 conversion; ≤ 2 LSB leaves margin
    // for float reassociation differences across platforms.
    assert!(
        max_lsb <= 2,
        "streamed output deviates from batch by {max_lsb} LSB"
    );
    Ok(())
}

#[test]
fn streaming_f32_matches_batch_and_resets_for_reuse() -> TestResult {
    let (input, _) = fixture()?;
    let enhancer = LiveWaveEnhancer::new()?;
    let batch = enhancer.process_f32(&input);

    let mut stream = enhancer.stream();
    for pass in 0..2_u8 {
        let mut streamed: Vec<f32> = Vec::new();
        // Deliberately hop-unaligned pushes to exercise buffering.
        for chunk in input.chunks(313) {
            streamed.extend(stream.push_samples_f32(chunk));
        }
        streamed.extend(stream.finish_f32());
        assert_eq!(streamed.len(), batch.len(), "pass {pass}: length");
        let mut max_diff = 0.0_f32;
        for (a, b) in streamed.iter().zip(&batch) {
            max_diff = max_diff.max((a - b).abs());
        }
        assert!(
            max_diff <= 1e-4,
            "pass {pass}: streamed f32 output deviates from batch by {max_diff}"
        );
    }
    Ok(())
}

#[test]
fn partial_final_hop_flushes_to_full_length() -> TestResult {
    let (input, _) = fixture()?;
    // 1000 samples: not a multiple of the 64-sample hop or of the
    // 160-sample frame, so the tail flush handles a partial column.
    let head = input.get(..1000).ok_or("fixture too short")?;
    let enhancer = LiveWaveEnhancer::new()?;
    let batch = enhancer.process_f32(head);

    let mut stream = enhancer.stream();
    let mut streamed: Vec<f32> = Vec::new();
    for chunk in head.chunks(160) {
        streamed.extend(stream.push_samples_f32(chunk));
    }
    streamed.extend(stream.finish_f32());

    assert_eq!(
        streamed.len(),
        head.len(),
        "tail flush must preserve length"
    );
    let mut max_diff = 0.0_f32;
    for (a, b) in streamed.iter().zip(&batch) {
        max_diff = max_diff.max((a - b).abs());
    }
    assert!(max_diff <= 1e-4, "tail-flush deviation {max_diff}");
    Ok(())
}

#[test]
fn short_i16_stream_passes_through_like_batch() -> TestResult {
    let enhancer = LiveWaveEnhancer::new()?;
    let pcm: Vec<i16> = (0..320_i16).map(|n| n.wrapping_mul(101)).collect();
    let batch = enhancer.process(&pcm);
    assert_eq!(batch, pcm, "batch passes short clips through");

    let mut stream = enhancer.stream();
    let mut streamed: Vec<i16> = Vec::new();
    for frame in pcm.chunks_exact(160) {
        let frame: &[i16; 160] = frame.try_into()?;
        streamed.extend(stream.push_frame(frame));
    }
    streamed.extend(stream.finish());
    assert_eq!(streamed, pcm, "short stream must pass through verbatim");
    Ok(())
}
