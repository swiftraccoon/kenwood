// SPDX-License-Identifier: GPL-2.0-or-later
//
// Per-field `b0..b8` diff of our encoder vs OP25's `ambe_encode_dump`
// trace. Reverse-derives each b_N from our `ambe_d[0..49]` using the
// D-STAR bit layout documented in `src/encode/quantize.rs` (mbelib
// AmbePlus convention). Matches what OP25's `ambe_encode_dump` does
// — it also reverse-derives b[0..8] from its own 72-bit interleaved
// output via `decode_dstar`, rather than exposing the quantizer's
// internal values directly.
//
// Bit layout reference: `ref/mbelib/ambe3600x2400_const.h` and the
// TIA-102.BABA-1 § Annex tables for widths. The field positions
// themselves (which `ambe_d[]` index each bit lives at) are the
// mbelib/DSD D-STAR convention, documented in-repo at
// `src/encode/quantize.rs:64-81`.
//
// Usage:
//   validate_bvec_vs_op25 <pcm_file> <op25_trace_file>

#![expect(
    clippy::print_stdout,
    clippy::uninlined_format_args,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_docs_in_private_items,
    clippy::cast_precision_loss,
    missing_docs,
    unused_results,
    reason = "Debugging example binary that parses OP25 trace files and compares b-vector \
              quantization to the Rust port. Uses stdout for diagnostics, allows panics \
              on malformed input (`.expect()` on parse, direct indexing into \
              deterministic-length byte arrays). This is validation scratchwork, not \
              library code. Skips docs since the tool is internal. DSP casts are \
              unavoidable here."
)]

// Dev-dependencies pulled in by sibling tests/examples. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use wide as _;

use mbelib_rs::{
    AmbeEncoder, EncoderBuffers, FftPlan, analyze_frame, compute_e_p, detect_vuv_and_sa,
    validation::{PrevFrameState, quantize},
};
use realfft::num_complex::Complex;
use std::io::{BufRead, BufReader, Read};

/// D-STAR `ambe_d` bit positions for each `b_N` field, MSB-first.
/// Verbatim from `src/encode/quantize.rs` layout table.
#[rustfmt::skip]
const B_POS: [&[usize]; 9] = [
    &[0, 1, 2, 3, 4, 5, 48],                  // b0: 7 bits
    &[38, 39, 40, 41],                         // b1: 4 bits
    &[6, 7, 8, 9, 42, 43],                     // b2: 6 bits
    &[10, 11, 12, 13, 14, 15, 16, 44, 45],    // b3: 9 bits
    &[17, 18, 19, 20, 21, 46, 47],             // b4: 7 bits
    &[22, 23, 25, 26],                         // b5: 4 bits (skips 24)
    &[27, 28, 29, 30],                         // b6: 4 bits
    &[31, 32, 33, 34],                         // b7: 4 bits
    &[35, 36, 37],                             // b8: 3 bits (LSB forced 0 on wire)
];

/// Reassemble the 9-int `b_vec` from a 49-bit `ambe_d` slice by
/// reading each field's positions MSB-first. Mirror of the bit
/// scatter in `src/encode/quantize.rs:write_bit` + the field layout
/// documented there. b8 is returned as the 3-bit value (not shifted
/// to the 4-bit half-rate form); the shift is applied at decode time.
fn reassemble_b_vec(ambe_d: &[u8]) -> [u16; 9] {
    let mut b = [0_u16; 9];
    for (field_idx, positions) in B_POS.iter().enumerate() {
        let mut v = 0_u16;
        for &p in *positions {
            v = (v << 1) | u16::from(ambe_d[p] & 1);
        }
        b[field_idx] = v;
    }
    b
}

#[derive(Debug, Clone, Default)]
struct Op25Frame {
    #[expect(dead_code, reason = "kept for future diagnostics")]
    index: usize,
    b: [i32; 9],
}

/// Per-frame analysis snapshot used by `--dp` mode. The 2-frame
/// lookahead pipeline needs to buffer each frame's `E(p)` array and
/// the FFT output it was derived from: pitch is committed for frame
/// N-2 only after frame N's analysis has produced `E(p)_N`.
struct DpSlot {
    e_p: [f32; 203],
    fft_out: Vec<Complex<f32>>,
}

fn parse_trace(path: &str) -> Vec<Op25Frame> {
    let file = std::fs::File::open(path).expect("open trace");
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
        } else if let Some(f) = cur.as_mut()
            && let Some(rest) = trimmed.strip_prefix("b0..b8 = ")
        {
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
        }
    }
    if let Some(f) = cur {
        frames.push(f);
    }
    frames
}

fn load_pcm(path: &str) -> Vec<f32> {
    let mut file = std::fs::File::open(path).expect("open pcm");
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("read");
    buf.chunks_exact(2)
        .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
        .collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "A/B harness with inline option parsing + pipeline + summary output; \
              splitting adds indirection without clarifying what each section does."
)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Parse args: [--dp] <pcm> <trace>
    let mut use_dp = false;
    let mut positional = Vec::new();
    for a in args.iter().skip(1) {
        if a == "--dp" {
            use_dp = true;
        } else {
            positional.push(a);
        }
    }
    let (Some(pcm_path), Some(trace_path)) = (positional.first(), positional.get(1)) else {
        eprintln!(
            "usage: {} [--dp] <pcm_file> <op25_trace_file>",
            args.first().map_or("validate", String::as_str)
        );
        eprintln!("  --dp: use 2-frame lookahead DP pitch tracker (mirrors OP25 pitch_est)");
        std::process::exit(2);
    };
    let pcm = load_pcm(pcm_path);
    let op25 = parse_trace(trace_path);
    eprintln!(
        "PCM: {} samples ({:.2}s), OP25 trace: {} frames, pitch path: {}",
        pcm.len(),
        pcm.len() as f64 / 8000.0,
        op25.len(),
        if use_dp {
            "DP (lookahead)"
        } else {
            "single-frame"
        }
    );

    // We drive the whole encoder pipeline (analyze → pitch →
    // detect_vuv_and_sa → quantize) here rather than calling
    // `AmbeEncoder::encode_frame`, because we want the pre-ECC
    // `ambe_d` not the scrambled/interleaved wire bytes.
    let mut bufs = EncoderBuffers::new();
    let mut plan = FftPlan::new();
    let mut fft_out: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); 129];
    let mut tracker_state = mbelib_rs::PitchTracker::new();
    let mut vuv_state = mbelib_rs::VuvState::new();
    let mut prev = PrevFrameState {
        log2_ml: [0.0_f32; 57],
        l: 0,
    };
    // Ignore AmbeEncoder here: it would short-circuit to silence on
    // low-confidence pitch, which hides the b_vec values for silence.
    // Construct-and-drop (kept as a smoke test that `::new()` doesn't panic).
    drop(AmbeEncoder::new());

    // DP mode buffers: 3 e_p arrays + 3 FFT snapshots for the
    // pipeline; emission is delayed by 2 frames so frame N-2 commits
    // when frame N arrives.
    let mut dp_slots: Vec<DpSlot> = Vec::with_capacity(2);

    let mut total = 0_usize;
    let mut per_field_match = [0_usize; 9];
    let mut first_mismatch_by_field: [Option<usize>; 9] = [None; 9];
    let mut exact_frame_match = 0_usize;

    for (frame_idx, chunk) in pcm.chunks(160).take(op25.len()).enumerate() {
        if chunk.len() < 160 {
            break;
        }
        let mut samples = [0.0_f32; 160];
        samples.copy_from_slice(chunk);
        analyze_frame(&samples, &mut bufs, &mut plan, &mut fft_out);
        let e_p_current = compute_e_p(bufs.pitch_est_buf());

        // Pick pitch path + FFT to quantize against.
        let (pitch, target_fft, emit_idx) = if use_dp {
            let slot = DpSlot {
                e_p: e_p_current,
                fft_out: fft_out.clone(),
            };
            if dp_slots.len() < 2 {
                dp_slots.push(slot);
                continue;
            }
            let p = tracker_state.estimate_with_lookahead(
                &dp_slots[0].e_p,
                &dp_slots[1].e_p,
                &slot.e_p,
            );
            let oldest = dp_slots.remove(0);
            let target = oldest.fft_out;
            dp_slots.push(slot);
            // Frame N-2 corresponds to trace index frame_idx - 2.
            (p, target, frame_idx - 2)
        } else {
            let p = tracker_state.estimate(bufs.pitch_est_buf());
            (p, fft_out.clone(), frame_idx)
        };

        let f0_bin = 256.0_f32 / pitch.period_samples;
        let e_p_for_vuv = (1.0 - pitch.confidence).clamp(0.0, 1.0);
        let (vuv, amps) = detect_vuv_and_sa(&target_fft, f0_bin, &mut vuv_state, e_p_for_vuv);

        let outcome = quantize(pitch, vuv, &amps, &prev);
        prev = PrevFrameState {
            log2_ml: outcome.prev_log2_ml,
            l: outcome.prev_l,
        };
        let ours = reassemble_b_vec(&outcome.ambe_d);
        let op25_idx = emit_idx;

        let Some(op25_f) = op25.get(op25_idx) else {
            continue;
        };
        total += 1;
        let mut frame_ok = true;
        for (i, (&op25_v, &our_v)) in op25_f.b.iter().zip(ours.iter()).enumerate() {
            let our_as_i32 = i32::from(our_v);
            if op25_v == our_as_i32 {
                per_field_match[i] += 1;
            } else {
                frame_ok = false;
                if first_mismatch_by_field[i].is_none() {
                    first_mismatch_by_field[i] = Some(op25_idx);
                }
            }
        }
        if frame_ok {
            exact_frame_match += 1;
        }
        if op25_idx < 10 {
            let op = &op25_f.b;
            println!(
                "F{:3}: OP25 b=[{:3} {:3} {:3} {:4} {:4} {:3} {:3} {:3} {:2}]",
                op25_idx, op[0], op[1], op[2], op[3], op[4], op[5], op[6], op[7], op[8]
            );
            println!(
                "      OURS b=[{:3} {:3} {:3} {:4} {:4} {:3} {:3} {:3} {:2}]{}",
                ours[0],
                ours[1],
                ours[2],
                ours[3],
                ours[4],
                ours[5],
                ours[6],
                ours[7],
                ours[8],
                if frame_ok { "  MATCH" } else { "  DIFF" },
            );
        }
    }

    println!();
    println!("=== SUMMARY ({} frames) ===", total);
    println!(
        "exact 9-field match: {}/{} ({:.1}%)",
        exact_frame_match,
        total,
        100.0 * exact_frame_match as f64 / total.max(1) as f64
    );
    for (i, count) in per_field_match.iter().enumerate() {
        let first = first_mismatch_by_field[i]
            .map_or_else(|| "—".to_string(), |f| format!("first diverge at F{f}"));
        println!(
            "  b{i}: {:3}/{} ({:5.1}%)  [{}]",
            count,
            total,
            100.0 * *count as f64 / total.max(1) as f64,
            first
        );
    }
}
