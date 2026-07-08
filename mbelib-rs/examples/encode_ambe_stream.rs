// SPDX-License-Identifier: GPL-3.0-or-later

//! Thin CLI: read s16le 8 kHz mono PCM from argv[1] in 160-sample
//! frames, encode each with `AmbeEncoder`, write 9 AMBE bytes per
//! frame to argv[2].
//!
//! Used by the validation harness.

#![cfg(feature = "encoder")]
#![expect(
    clippy::print_stderr,
    reason = "CLI tool: uses stderr for usage/error messages — standard pattern for a \
              binary example, not a library."
)]

// Dev-dependencies pulled in by sibling tests/examples. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use proptest as _;
use realfft as _;
use wide as _;

use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let mut args = std::env::args();
    let prog = args
        .next()
        .unwrap_or_else(|| "encode_ambe_stream".to_owned());
    let (Some(in_path), Some(out_path)) = (args.next(), args.next()) else {
        eprintln!("usage: {prog} <in.s16> <out.ambe> [out.trace]");
        std::process::exit(2);
    };
    let trace_path = args.next();
    if args.next().is_some() {
        eprintln!("usage: {prog} <in.s16> <out.ambe> [out.trace]");
        std::process::exit(2);
    }
    let mut input = std::fs::File::open(&in_path)?;
    let mut output = std::fs::File::create(&out_path)?;
    let mut trace = match trace_path.as_deref() {
        Some("-") | None => None,
        Some(p) => Some(std::fs::File::create(p)?),
    };
    let mut enc = if std::env::var_os("MBELIB_LOOKAHEAD").is_some() {
        mbelib_rs::AmbeEncoder::new_with_lookahead()
    } else {
        mbelib_rs::AmbeEncoder::new()
    };

    let mut pcm_bytes = [0u8; 320];
    let mut frame_idx = 0_usize;
    loop {
        match input.read_exact(&mut pcm_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let mut pcm = [0_i16; 160];
        for (slot, chunk) in pcm.iter_mut().zip(pcm_bytes.chunks_exact(2)) {
            if let [lo, hi] = *chunk {
                *slot = i16::from_le_bytes([lo, hi]);
            }
        }
        let ambe = enc.encode_frame_i16(&pcm);
        output.write_all(&ambe)?;
        if let Some(t) = trace.as_mut() {
            writeln!(t, "FRAME {frame_idx}")?;
            write!(t, "  wire_bytes =")?;
            for b in &ambe {
                write!(t, " {b:02x}")?;
            }
            writeln!(t)?;
        }
        frame_idx += 1;
    }
    eprintln!("encoded {frame_idx} frames");
    Ok(())
}
