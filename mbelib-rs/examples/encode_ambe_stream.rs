// SPDX-License-Identifier: GPL-3.0-or-later

//! Thin CLI: read s16le 8 kHz mono PCM from argv[1] in 160-sample
//! frames, encode each with `AmbeEncoder`, write 9 AMBE bytes per
//! frame to argv[2].
//!
//! Used by the validation harness.

#![cfg(feature = "encoder")]
#![allow(clippy::print_stderr)]

use std::io::{Read, Write};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 || args.len() > 4 {
        eprintln!("usage: {} <in.s16> <out.ambe> [out.trace]", args[0]);
        std::process::exit(2);
    }
    let trace_path = args.get(3).cloned();
    let mut input = std::fs::File::open(&args[1])?;
    let mut output = std::fs::File::create(&args[2])?;
    let mut trace = match trace_path.as_deref() {
        Some("-") | None => None,
        Some(p) => Some(std::fs::File::create(p)?),
    };
    let mut enc = mbelib_rs::AmbeEncoder::new();

    let mut pcm_bytes = [0u8; 320];
    let mut frame_idx = 0_usize;
    loop {
        match input.read_exact(&mut pcm_bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let mut pcm = [0_i16; 160];
        for (i, slot) in pcm.iter_mut().enumerate() {
            *slot = i16::from_le_bytes([pcm_bytes[i * 2], pcm_bytes[i * 2 + 1]]);
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
