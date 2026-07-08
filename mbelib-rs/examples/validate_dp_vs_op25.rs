// SPDX-License-Identifier: GPL-2.0-or-later
//
// A/B: single-frame PitchTracker::estimate vs DP estimate_with_lookahead.
// Feeds both against a PCM fixture + OP25 trace, reports pitch match.

#![expect(
    clippy::print_stdout,
    clippy::cast_precision_loss,
    missing_docs,
    reason = "Single-frame vs DP pitch-tracker A/B harness: a diagnostic CLI that \
              prints its comparison table to stdout; the percentage math casts \
              frame counts to f64 for display only."
)]

// Dev-dependencies pulled in by sibling tests/examples. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use wide as _;

use mbelib_rs::{EncoderBuffers, FftPlan, PitchTracker, analyze_frame, compute_e_p};
use realfft::num_complex::Complex;
use std::io::{BufRead, BufReader, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pcm_path = std::env::args()
        .nth(1)
        .ok_or("usage: validate_dp <pcm> <trace>")?;
    let trace_path = std::env::args()
        .nth(2)
        .ok_or("usage: validate_dp <pcm> <trace>")?;

    let pcm: Vec<f32> = {
        let mut f = std::fs::File::open(&pcm_path)?;
        let mut buf = Vec::new();
        let _len = f.read_to_end(&mut buf)?;
        buf.chunks_exact(2)
            .filter_map(|b| match *b {
                [lo, hi] => Some(f32::from(i16::from_le_bytes([lo, hi])) / 32768.0),
                _ => None,
            })
            .collect()
    };

    // Parse OP25 trace for ref_pitch per frame.
    let op25_pitch: Vec<f32> = {
        let f = std::fs::File::open(&trace_path)?;
        let r = BufReader::new(f);
        let mut out = Vec::new();
        for line in r.lines().map_while(Result::ok) {
            if let Some(rest) = line.trim_start().strip_prefix("ref_pitch = ") {
                let Some(token) = rest.split_whitespace().next() else {
                    continue;
                };
                let q88: u16 = token.parse()?;
                out.push(f32::from(q88) / 256.0);
            }
        }
        out
    };

    let mut bufs = EncoderBuffers::new();
    let mut plan = FftPlan::new();
    let mut fft_out = vec![Complex::new(0.0, 0.0); 129];
    let mut ring: Vec<[f32; 203]> = Vec::with_capacity(3);
    let mut single = PitchTracker::new();
    let mut dp = PitchTracker::new();

    let mut single_periods = Vec::new();
    let mut dp_periods = Vec::new();

    for frame in pcm.chunks(160).take(op25_pitch.len()) {
        if frame.len() < 160 {
            break;
        }
        let mut samples = [0.0_f32; 160];
        samples.copy_from_slice(frame);
        analyze_frame(&samples, &mut bufs, &mut plan, &mut fft_out);
        let e_p = compute_e_p(bufs.pitch_est_buf());

        // Single-frame on same buf.
        let s_est = single.estimate(bufs.pitch_est_buf());
        single_periods.push(s_est.period_samples);

        // DP: buffer 3 e_p arrays.
        ring.push(e_p);
        if let [r0, r1, r2] = ring.as_slice() {
            let est = dp.estimate_with_lookahead(r0, r1, r2);
            dp_periods.push(est.period_samples);
            let _oldest = ring.remove(0);
        } else {
            dp_periods.push(0.0); // warmup
        }
    }

    // Compute match rates.
    let within = |diffs: &[f32]| diffs.iter().filter(|&&d| d.abs() < 5.0).count();
    let single_diffs: Vec<f32> = single_periods
        .iter()
        .zip(&op25_pitch)
        .map(|(a, b)| a - b)
        .collect();
    // DP is delayed by 2 frames: DP[i] = pitch for frame i-2. So compare
    // dp_periods[2..] to op25_pitch[..len-2].
    let dp_diffs: Vec<f32> = dp_periods
        .iter()
        .skip(2)
        .zip(op25_pitch.iter())
        .map(|(a, b)| a - b)
        .collect();

    println!("frames: {}", op25_pitch.len());
    println!(
        "single-frame within ±5 samples: {}/{}  ({:.1}%)",
        within(&single_diffs),
        single_diffs.len(),
        100.0 * within(&single_diffs) as f64 / single_diffs.len() as f64
    );
    println!(
        "DP (lookahead) within ±5 samples: {}/{}  ({:.1}%)",
        within(&dp_diffs),
        dp_diffs.len(),
        100.0 * within(&dp_diffs) as f64 / dp_diffs.len() as f64
    );
    Ok(())
}
