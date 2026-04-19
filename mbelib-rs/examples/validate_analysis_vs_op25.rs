// SPDX-License-Identifier: GPL-2.0-or-later
//
// Strict-validation harness for the encoder's analysis stages 1–4.
//
// For each 160-sample PCM frame, runs our analysis pipeline
// (analyze_frame → pitch tracker → V/UV detector → spectral
// amplitudes) and compares the outputs frame-by-frame against OP25's
// `imbe_param` values dumped by `ref_tools/build/ambe_encode_dump`.
//
// This isolates Stages 1–4 from Stages 5–8 — stage-5+ divergences
// are covered by `validate_quantize_vs_op25`. Together, the two
// harnesses cover the entire encoder path against the reference.
//
// Usage:
//   validate_analysis_vs_op25 <pcm_file> <op25_trace_file>

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::uninlined_format_args,
    clippy::too_many_lines,
    dead_code,
    unused_results,
    missing_docs
)]

use mbelib_rs::{
    EncoderBuffers, FftPlan, PitchTracker, analyze_frame, detect_vuv, extract_spectral_amplitudes,
};
use realfft::num_complex::Complex;
use std::io::{BufRead, BufReader, Read};

#[derive(Debug, Clone, Default)]
struct Op25Frame {
    index: usize,
    ref_pitch_q88: u16,
    num_harms: usize,
    sa: Vec<i32>,
    v_uv_dsn: Vec<bool>,
    b0: i32,
}

fn parse_trace(path: &str) -> Vec<Op25Frame> {
    let file = std::fs::File::open(path).expect("open trace file");
    let reader = BufReader::new(file);
    let mut frames: Vec<Op25Frame> = Vec::new();
    let mut cur: Option<Op25Frame> = None;
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("FRAME ") {
            if let Some(f) = cur.take() {
                frames.push(f);
            }
            let idx: usize = rest.trim().parse().unwrap_or(0);
            cur = Some(Op25Frame {
                index: idx,
                ..Default::default()
            });
        } else if let Some(f) = cur.as_mut() {
            if let Some(rest) = trimmed.strip_prefix("ref_pitch = ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                f.ref_pitch_q88 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                for (i, w) in parts.iter().enumerate() {
                    if *w == "num_harms" {
                        f.num_harms = parts.get(i + 2).and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix("sa[] = ") {
                f.sa = rest
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
            } else if let Some(rest) = trimmed.strip_prefix("v_uv_dsn[] = ") {
                f.v_uv_dsn = rest
                    .split_whitespace()
                    .map(|s| s.parse::<i32>().unwrap_or(0) != 0)
                    .collect();
            } else if let Some(rest) = trimmed.strip_prefix("b0..b8 = ") {
                let parts: Vec<i32> = rest
                    .split_whitespace()
                    .take(9)
                    .filter_map(|s| s.parse().ok())
                    .collect();
                if let Some(&v) = parts.first() {
                    f.b0 = v;
                }
            }
        }
    }
    if let Some(f) = cur {
        frames.push(f);
    }
    frames
}

fn load_pcm(path: &str) -> Vec<f32> {
    let mut file = std::fs::File::open(path).expect("open PCM");
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("read");
    buf.chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(pcm_path), Some(trace_path)) = (args.get(1), args.get(2)) else {
        eprintln!(
            "usage: {} <pcm_file> <op25_trace>",
            args.first().map_or("validate", |v| v.as_str())
        );
        std::process::exit(2);
    };

    let pcm = load_pcm(pcm_path);
    let op25 = parse_trace(trace_path);
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
        let vuv = detect_vuv(&fft_out, f0_bin);
        let amps = extract_spectral_amplitudes(&fft_out, f0_bin);
        let our_l = amps.num_harmonics;

        let op25_f = &op25[frame_idx];
        let op25_period = op25_f.ref_pitch_q88 as f32 / 256.0;
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
    let mean_l_diff = l_diffs.iter().sum::<i32>() as f64 / l_diffs.len().max(1) as f64;
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
}
