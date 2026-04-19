// SPDX-License-Identifier: GPL-2.0-or-later

//! Thin CLI: read a concatenated 9-byte-per-frame AMBE stream from
//! argv[1], decode each frame with `AmbeDecoder`, and write 160
//! s16le PCM samples per frame to argv[2].
//!
//! Used by the validation harness in `ref_tools/run_validation.sh`
//! to compare our decoder output against mbelib's for identical
//! AMBE input.

#![allow(clippy::print_stderr)]

use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("usage: {} <in.ambe> <out.s16> [out.trace]", args[0]);
        std::process::exit(2);
    }
    let trace_path = args.get(3).cloned();
    let mut input = std::fs::File::open(&args[1])?;
    let mut output = std::fs::File::create(&args[2])?;
    let mut trace = match trace_path.as_deref() {
        Some("-") | None => None,
        Some(p) => Some(std::fs::File::create(p)?),
    };
    let mut dec = mbelib_rs::AmbeDecoder::new();
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
        for (i, &s) in pcm.iter().enumerate() {
            let le = s.to_le_bytes();
            bytes[i * 2] = le[0];
            bytes[i * 2 + 1] = le[1];
        }
        output.write_all(&bytes)?;
        frame_idx += 1;
    }
    eprintln!("decoded {frame_idx} frames");
    Ok(())
}
