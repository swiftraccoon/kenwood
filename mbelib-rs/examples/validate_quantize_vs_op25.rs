// SPDX-License-Identifier: GPL-2.0-or-later
//
// Strict-validation harness for the encoder's `quantize` stage.
//
// Feeds OP25's reference `encode_ambe` intermediate values into our
// Rust `quantize()` function and verifies the emitted `b[0..8]`
// matches OP25's for every frame.  Any divergence pinpoints exactly
// which codebook search / DCT / interpolation step in our pipeline
// differs from the reference — without any confounding from stages
// 1–4 (PCM → spectral analysis), which use different algorithms.
//
// Usage:
//   validate_quantize_vs_op25 <op25_trace_file>
//
// Produces the OP25 trace with the `ambe_encode_dump` harness built
// against OP25: `ambe_encode_dump <in.s16> <out.ambe> <trace>`
//
// The trace must include `prev_log2Ml` / `prev_L` lines (added via
// `#define private public` access to `ambe_encoder::prev_mp`).

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::uninlined_format_args,
    clippy::collapsible_if,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::expect_used,
    clippy::indexing_slicing,
    dead_code,
    missing_docs,
    reason = "Stages 5-8 quantize A/B harness (OP25 trace -> Rust-port match rates). \
              Prints diagnostics; DSP precision casts are unavoidable in the PRBA/HOC \
              codebook search. `.expect()` is used on trace-parse results because this \
              is validation scratchwork — a malformed fixture should abort the example \
              with a specific message rather than propagating errors through a \
              library-shaped API. `clippy::indexing_slicing` fires on direct indexing \
              into the parsed-trace fixed-size arrays (`b[0..8]`, `prev_mp.log2_ml[0..56]`, \
              etc.) — bounds are IMBE-spec constants enforced at trace parse time."
)]

// Dev-dependencies pulled in by sibling tests/examples. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use mbelib_rs::validation::{PrevFrameState, quantize};
use mbelib_rs::{MAX_BANDS, MAX_HARMONICS, PitchEstimate, SpectralAmplitudes, VuvDecisions};
use std::io::{BufRead, BufReader};

/// One parsed frame from the OP25 reference trace.
#[derive(Debug, Clone)]
struct Op25Frame {
    index: usize,
    ref_pitch_q88: u16,
    num_harms: usize,
    sa: Vec<i32>,
    v_uv_dsn: Vec<bool>,
    b: [i32; 9],
    /// Post-encode state: log2Ml array that OP25 carries into the NEXT frame.
    prev_log2_ml: [f32; 57],
    prev_l: usize,
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
                ref_pitch_q88: 0,
                num_harms: 0,
                sa: Vec::new(),
                v_uv_dsn: Vec::new(),
                b: [0; 9],
                prev_log2_ml: [0.0; 57],
                prev_l: 0,
            });
        } else if let Some(f) = cur.as_mut() {
            if let Some(rest) = trimmed.strip_prefix("ref_pitch = ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                f.ref_pitch_q88 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                // "ref_pitch = N  num_harms = M"
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
                for (i, v) in parts.iter().enumerate() {
                    if i < 9 {
                        f.b[i] = *v;
                    }
                }
            } else if let Some(rest) = trimmed.strip_prefix("prev_L = ") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                f.prev_l = parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
            } else if let Some(rest) = trimmed.strip_prefix("prev_log2Ml = ") {
                let vals: Vec<f32> = rest
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .collect();
                for (i, v) in vals.iter().enumerate().take(57) {
                    f.prev_log2_ml[i] = *v;
                }
            }
        }
    }
    if let Some(f) = cur {
        frames.push(f);
    }
    frames
}

/// Build our [`PitchEstimate`] from OP25's `ref_pitch` (Q8.8 period).
fn pitch_from_op25(ref_pitch_q88: u16, confidence: f32) -> PitchEstimate {
    let period = f32::from(ref_pitch_q88) / 256.0;
    PitchEstimate {
        period_samples: period,
        f0_hz: 8000.0 / period,
        confidence,
    }
}

/// Convert OP25's per-harmonic `v_uv_dsn` to our per-band `VuvDecisions`.
///
/// IMBE groups harmonics into bands of 3: `band = (l + 2) / 3` for
/// l=1..=36, then single-harmonic bands. OP25's `v_uv_dsn` is already
/// expanded to per-harmonic (each group of 3 shares a value). We
/// reverse: pick `v_uv_dsn[band * 3]` as the band's voiced flag.
fn vuv_from_op25(v_uv_dsn: &[bool], num_harms: usize, num_bands: usize) -> VuvDecisions {
    let mut voiced = [false; MAX_BANDS];
    for (b_idx, slot) in voiced.iter_mut().enumerate().take(num_bands.min(MAX_BANDS)) {
        let harm_in_band = b_idx * 3;
        *slot = v_uv_dsn
            .get(harm_in_band.min(num_harms.saturating_sub(1)))
            .copied()
            .unwrap_or(false);
    }
    VuvDecisions {
        voiced,
        num_bands: num_bands.min(MAX_BANDS),
    }
}

/// Build [`SpectralAmplitudes`] from OP25's int16-scaled `sa[]`.
///
/// Our `quantize` multiplies each magnitude by `SA_SCALE = 32768.0`
/// before `log2` to match OP25's lsa scale, so we divide by 32768 here
/// to reverse the scaling and end up with the same value OP25 uses.
fn amps_from_op25(sa: &[i32], num_harms: usize) -> SpectralAmplitudes {
    let mut magnitudes = [0.0_f32; MAX_HARMONICS];
    for (i, &v) in sa.iter().enumerate().take(num_harms).take(MAX_HARMONICS) {
        // Pass OP25's sa verbatim as the "raw" magnitude. Our quantize
        // then multiplies by SA_SCALE=32768 then `log2` — but OP25 is
        // already at the int16 scale (log2 ready), so divide first so
        // the round-trip through `sa * SA_SCALE` lands at OP25's value.
        #[expect(
            clippy::cast_precision_loss,
            reason = "OP25 sa values are int16-scaled (~0..32767); i32-to-f32 cast is \
                      exact within int16 range where real values land."
        )]
        let f = v as f32 / 32768.0;
        magnitudes[i] = f;
    }
    SpectralAmplitudes {
        magnitudes,
        num_harmonics: num_harms.min(MAX_HARMONICS),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!(
            "usage: {} <op25_trace_file>",
            args.first().map_or("validate", |v| v.as_str())
        );
        std::process::exit(2);
    };
    let frames = parse_trace(path);
    eprintln!("parsed {} frames", frames.len());

    let mut exact_b_match = 0;
    let mut b0_match = 0;
    let mut b_total = 0;
    let mut b_pos_matches = [0_usize; 9];

    for i in 0..frames.len() {
        let f = &frames[i];
        if f.num_harms == 0 {
            continue; // silence-ish frame in OP25 — skip
        }
        // Previous-frame state: for frame 0, zero prev. For frame N,
        // use frame N-1's post-encode dump.
        let prev = if i == 0 {
            PrevFrameState {
                log2_ml: [0.0_f32; 57],
                l: 0,
            }
        } else {
            PrevFrameState {
                log2_ml: frames[i - 1].prev_log2_ml,
                l: frames[i - 1].prev_l,
            }
        };
        let pitch = pitch_from_op25(f.ref_pitch_q88, 0.5);
        let amps = amps_from_op25(&f.sa, f.num_harms);
        // OP25 picks num_bands based on the pitch-dependent band
        // layout; we approximate by using our own VUV bands count.
        // Using num_harms/3 rounded up matches IMBE's grouping.
        let num_bands = f.num_harms.div_ceil(3).min(MAX_BANDS);
        let vuv = vuv_from_op25(&f.v_uv_dsn, f.num_harms, num_bands);

        let outcome = quantize(pitch, vuv, &amps, &prev);
        // Extract our b[0..8] from the returned ambe_d.
        let a = &outcome.ambe_d;
        let bit = |k: usize| i32::from(a[k]);
        let our_b0 = (bit(0) << 6)
            | (bit(1) << 5)
            | (bit(2) << 4)
            | (bit(3) << 3)
            | (bit(4) << 2)
            | (bit(5) << 1)
            | bit(48);
        let our_b1 = (bit(38) << 3) | (bit(39) << 2) | (bit(40) << 1) | bit(41);
        let our_b2 = (bit(6) << 5)
            | (bit(7) << 4)
            | (bit(8) << 3)
            | (bit(9) << 2)
            | (bit(42) << 1)
            | bit(43);
        let our_b3 = (bit(10) << 8)
            | (bit(11) << 7)
            | (bit(12) << 6)
            | (bit(13) << 5)
            | (bit(14) << 4)
            | (bit(15) << 3)
            | (bit(16) << 2)
            | (bit(44) << 1)
            | bit(45);
        let our_b4 = (bit(17) << 6)
            | (bit(18) << 5)
            | (bit(19) << 4)
            | (bit(20) << 3)
            | (bit(21) << 2)
            | (bit(46) << 1)
            | bit(47);
        let our_b5 = (bit(22) << 3) | (bit(23) << 2) | (bit(25) << 1) | bit(26);
        let our_b6 = (bit(27) << 3) | (bit(28) << 2) | (bit(29) << 1) | bit(30);
        let our_b7 = (bit(31) << 3) | (bit(32) << 2) | (bit(33) << 1) | bit(34);
        let our_b8 = (bit(35) << 3) | (bit(36) << 2) | (bit(37) << 1);
        let ours = [
            our_b0, our_b1, our_b2, our_b3, our_b4, our_b5, our_b6, our_b7, our_b8,
        ];

        b_total += 1;
        if ours == f.b {
            exact_b_match += 1;
        }
        if ours[0] == f.b[0] {
            b0_match += 1;
        }
        for k in 0..9 {
            if ours[k] == f.b[k] {
                b_pos_matches[k] += 1;
            }
        }
        if i < 20 || ours != f.b {
            if i < 20 {
                println!(
                    "F{i:3}: OP25 b={:?}  OURS b={:?}  {}",
                    f.b,
                    ours,
                    if ours == f.b { "MATCH" } else { "DIFF" }
                );
            }
        }
    }

    println!();
    println!("=== SUMMARY ({b_total} frames) ===");
    println!(
        "exact b[0..8] match: {}/{} ({:.1}%)",
        exact_b_match,
        b_total,
        100.0 * exact_b_match as f64 / b_total.max(1) as f64
    );
    println!("b0 match: {}/{}", b0_match, b_total);
    for (k, count) in b_pos_matches.iter().enumerate() {
        println!(
            "  b{k} match: {}/{} ({:.1}%)",
            count,
            b_total,
            100.0 * *count as f64 / b_total.max(1) as f64
        );
    }
}
