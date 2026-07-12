// SPDX-License-Identifier: GPL-2.0-or-later

//! Thin CLI: read a concatenated 9-byte-per-frame AMBE stream from
//! argv[1], decode each frame with `AmbeDecoder`, and write 160
//! s16le PCM samples per frame to argv[2].
//!
//! Used by the validation harness that compares our decoder output
//! against mbelib's for identical AMBE input, and by the synthesis
//! tuning sweep, which sets the `MBELIB_TUNING` environment variable
//! to a comma-separated `key=value` list (keys: `alpha`, `exp`, `lo`,
//! `hi`, `uv` — anything omitted stays at mbelib parity).

#![expect(
    clippy::print_stderr,
    reason = "CLI tool: uses stderr for usage/error messages — standard pattern for a \
              binary example, not a library."
)]

// Dev-dependencies pulled in by sibling test/example targets. Acknowledge them
// here so `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use std::io::{Read, Write};

/// Parse `MBELIB_TUNING="alpha=0.9,exp=0.3,lo=0.5,hi=1.4,uv=0.8"`;
/// unknown keys and malformed numbers abort loudly rather than run a
/// sweep with a silently-ignored knob.
fn tuning_from_env() -> mbelib_rs::SynthesisTuning {
    let mut tuning = mbelib_rs::SynthesisTuning::default();
    let Ok(spec) = std::env::var("MBELIB_TUNING") else {
        return tuning;
    };
    for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
        let Some((key, value)) = part.split_once('=') else {
            eprintln!("MBELIB_TUNING: expected key=value, got {part:?}");
            std::process::exit(2);
        };
        let Ok(value) = value.trim().parse::<f32>() else {
            eprintln!("MBELIB_TUNING: bad number in {part:?}");
            std::process::exit(2);
        };
        match key.trim() {
            "alpha" => tuning.enhance_alpha = value,
            "exp" => tuning.enhance_exponent = value,
            "lo" => tuning.enhance_clamp_lo = value,
            "hi" => tuning.enhance_clamp_hi = value,
            "uv" => tuning.unvoiced_gain = value,
            "jit" => tuning.phase_jitter = value,
            other => {
                eprintln!("MBELIB_TUNING: unknown key {other:?}");
                std::process::exit(2);
            }
        }
    }
    tuning
}

fn main() -> std::io::Result<()> {
    let mut args = std::env::args();
    let prog = args
        .next()
        .unwrap_or_else(|| "decode_ambe_stream".to_owned());
    let (Some(in_path), Some(out_path)) = (args.next(), args.next()) else {
        eprintln!("usage: {prog} <in.ambe> <out.s16> [out.trace]");
        std::process::exit(2);
    };
    let trace_path = args.next();
    if args.next().is_some() {
        eprintln!("usage: {prog} <in.ambe> <out.s16> [out.trace]");
        std::process::exit(2);
    }
    let mut input = std::fs::File::open(&in_path)?;
    let mut output = std::fs::File::create(&out_path)?;
    let mut trace = match trace_path.as_deref() {
        Some("-") | None => None,
        Some(p) => Some(std::fs::File::create(p)?),
    };
    let mut dec = mbelib_rs::AmbeDecoder::with_tuning(tuning_from_env());
    let mut frame = [0u8; 9];
    let mut frame_idx = 0_usize;
    loop {
        match input.read_exact(&mut frame) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        if let Some(t) = trace.as_mut() {
            // Run the bit-extraction path exposed via the test
            // helper so we can diff against mbelib's per-frame
            // ambe_d / b[] / w0 / L values.
            let (b, w0, big_l, ambe_d) = mbelib_rs::decode_trace(&frame);
            writeln!(t, "FRAME {frame_idx}")?;
            write!(t, "  wire_bytes =")?;
            for x in &frame {
                write!(t, " {x:02x}")?;
            }
            writeln!(t)?;
            write!(t, "  ambe_d =")?;
            for v in &ambe_d {
                write!(t, "{v}")?;
            }
            writeln!(t)?;
            write!(t, "  b0..b8 =")?;
            for v in &b {
                write!(t, " {v}")?;
            }
            writeln!(t, "  w0 = {w0:.6}  L = {big_l}")?;
        }
        let pcm = dec.decode_frame(&frame);
        let mut bytes = [0u8; 320];
        for (chunk, &s) in bytes.chunks_exact_mut(2).zip(pcm.iter()) {
            chunk.copy_from_slice(&s.to_le_bytes());
        }
        output.write_all(&bytes)?;
        frame_idx += 1;
    }
    eprintln!("decoded {frame_idx} frames");
    Ok(())
}
