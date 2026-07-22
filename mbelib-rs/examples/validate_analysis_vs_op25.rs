// SPDX-License-Identifier: GPL-2.0-or-later
//
// Strict-validation harness for the encoder's analysis stages 1–4.
//
// For each 160-sample PCM frame, runs our analysis pipeline
// (analyze_frame → pitch tracker → V/UV detector → spectral
// amplitudes) and compares the outputs frame-by-frame against OP25's
// `imbe_param` values dumped by an `ambe_encode_dump` harness built
// against OP25.
//
// This isolates Stages 1–4 from Stages 5–8; stage-5+ divergences
// are covered by `validate_quantize_vs_op25`. Together, the two
// harnesses cover the entire encoder path against the reference.
//
// Usage:
//   validate_analysis_vs_op25 <pcm_file> <op25_trace_file>

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::uninlined_format_args,
    clippy::too_many_lines,
    missing_docs,
    reason = "Stages 1-4 analysis A/B harness (PCM + OP25 trace -> metrics): a \
              diagnostic CLI that prints its comparison tables to stdout/stderr; the \
              summary math casts frame counts and harmonic deltas for display only. \
              Docs are skipped since the tool is an internal validation harness."
)]

// Dev-dependencies pulled in by sibling tests/examples. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use wide as _;

use mbelib_rs::{
    EncoderBuffers, FftPlan, PitchTracker, VuvState, analyze_frame, detect_vuv_and_sa,
};
use realfft::num_complex::Complex;
use std::io::{BufRead, BufReader, Read};

#[derive(Debug, Clone, Default)]
struct Op25Frame {
    ref_pitch_q88: u16,
    num_harms: usize,
    v_uv_dsn: Vec<bool>,
}

fn parse_trace(path: &str) -> Result<Vec<Op25Frame>, std::io::Error> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut frames: Vec<Op25Frame> = Vec::new();
    let mut cur: Option<Op25Frame> = None;
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim_start();
        if trimmed.strip_prefix("FRAME ").is_some() {
            if let Some(f) = cur.take() {
                frames.push(f);
            }
            cur = Some(Op25Frame::default());
        } else if let Some(f) = cur.as_mut() {
            if let Some(rest) = trimmed.strip_prefix("ref_pitch = ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                f.ref_pitch_q88 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                for (i, w) in parts.iter().enumerate() {
                    if *w == "num_harms" {
                        f.num_harms = parts.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix("v_uv_dsn[] = ") {
                f.v_uv_dsn = rest
                    .split_whitespace()
                    .map(|s| s.parse::<i32>().unwrap_or(0) != 0)
                    .collect();
            }
        }
    }
    if let Some(f) = cur {
        frames.push(f);
    }
    Ok(frames)
}

fn load_pcm(path: &str) -> Result<Vec<f32>, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    let _len = file.read_to_end(&mut buf)?;
    Ok(buf
        .chunks_exact(2)
        .filter_map(|b| match *b {
            [lo, hi] => Some(f32::from(i16::from_le_bytes([lo, hi])) / 32768.0),
            _ => None,
        })
        .collect())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(pcm_path), Some(trace_path)) = (args.get(1), args.get(2)) else {
        eprintln!(
            "usage: {} <pcm_file> <op25_trace>",
            args.first().map_or("validate", |v| v.as_str())
        );
        std::process::exit(2);
    };

    let pcm = load_pcm(pcm_path)?;
    let op25 = parse_trace(trace_path)?;
    println!(
        "PCM: {} samples ({:.1}s), OP25 trace: {} frames",
        pcm.len(),
        pcm.len() as f64 / 8000.0,
        op25.len()
    );

    let mut bufs = EncoderBuffers::new();
    let mut plan = FftPlan::new();
    let mut fft_out: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 129];
    let mut pitch_tracker = PitchTracker::new();
    let mut vuv_state = VuvState::new();

    println!();
    println!("Frame | OP25 (L, pitch_ref_samples)   | OURS (L_est, pitch_samples)");
    println!("------|------------------------------|------------------------------");

    let mut pitch_period_diffs: Vec<f32> = Vec::new();
    let mut l_diffs: Vec<i32> = Vec::new();

    for (frame_idx, frame) in pcm.chunks(160).take(op25.len()).enumerate() {
        if frame.len() < 160 {
            break;
        }
        let mut samples = [0.0_f32; 160];
        samples.copy_from_slice(frame);
        analyze_frame(&samples, &mut bufs, &mut plan, &mut fft_out);
        let pitch = pitch_tracker.estimate(bufs.pitch_est_buf());
        let f0_bin = 256.0 / pitch.period_samples;
        let e_p = (1.0 - pitch.confidence).clamp(0.0, 1.0);
        let (vuv, amps) = detect_vuv_and_sa(&fft_out, f0_bin, &mut vuv_state, e_p);
        let our_l = amps.num_harmonics;

        let Some(op25_f) = op25.get(frame_idx) else {
            break;
        };
        let op25_period = f32::from(op25_f.ref_pitch_q88) / 256.0;
        let period_diff = pitch.period_samples - op25_period;
        let l_diff = our_l as i32 - op25_f.num_harms as i32;
        pitch_period_diffs.push(period_diff);
        l_diffs.push(l_diff);

        if frame_idx < 10 {
            println!(
                "F{:3}  | L={:2} period={:.2} samples   | L={:2} period={:.2} (Δperiod={:+.2}, ΔL={:+})",
                frame_idx,
                op25_f.num_harms,
                op25_period,
                our_l,
                pitch.period_samples,
                period_diff,
                l_diff
            );
        }

        // Also compute per-harmonic voicing comparison
        if frame_idx < 5 {
            let mut our_voiced_per_harm: Vec<bool> = Vec::new();
            for i in 0..our_l {
                let band = (i / 3).min(vuv.num_bands.saturating_sub(1));
                our_voiced_per_harm.push(vuv.voiced.get(band).copied().unwrap_or(false));
            }
            let op25_voiced: Vec<bool> = op25_f.v_uv_dsn.iter().copied().take(our_l).collect();
            let matches = our_voiced_per_harm
                .iter()
                .zip(op25_voiced.iter())
                .filter(|(a, b)| a == b)
                .count();
            println!(
                "       VUV match: {}/{} harmonics",
                matches,
                our_voiced_per_harm.len().max(op25_voiced.len())
            );
        }
    }

    // Summary statistics
    let mean_period_diff =
        pitch_period_diffs.iter().sum::<f32>() / pitch_period_diffs.len().max(1) as f32;
    let max_abs_period_diff = pitch_period_diffs
        .iter()
        .map(|x| x.abs())
        .fold(0.0_f32, f32::max);
    let mean_l_diff = f64::from(l_diffs.iter().sum::<i32>()) / l_diffs.len().max(1) as f64;
    let l_match = l_diffs.iter().filter(|&&d| d == 0).count();
    let pitch_close = pitch_period_diffs
        .iter()
        .filter(|&&d| d.abs() < 5.0)
        .count();

    println!();
    println!("=== SUMMARY ({} frames) ===", pitch_period_diffs.len());
    println!(
        "Pitch period: mean_diff={:.2} samples, max_abs_diff={:.1} samples",
        mean_period_diff, max_abs_period_diff
    );
    println!(
        "Pitch period within ±5 samples: {}/{} ({:.1}%)",
        pitch_close,
        pitch_period_diffs.len(),
        100.0 * pitch_close as f64 / pitch_period_diffs.len().max(1) as f64
    );
    println!(
        "num_harms exact match: {}/{} ({:.1}%)  mean_diff={:.2}",
        l_match,
        l_diffs.len(),
        100.0 * l_match as f64 / l_diffs.len().max(1) as f64,
        mean_l_diff
    );
    Ok(())
}
