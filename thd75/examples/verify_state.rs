//! Verify radio settings against live memory, and discover where they live.
//!
//! # Firmware requirement
//!
//! Every mode here needs firmware modified by the `thd75-fw` project. On a
//! stock radio the probe fails immediately and deliberately, because the
//! mnemonic this uses carries an unrelated function there.
//!
//! # Modes
//!
//! ```text
//! probe    <port>                     confirm the radio supports memory reads
//! dump     <port> <offset> <len>      read and hexdump a region
//! discover <port> <offset> <len>      snapshot, wait, snapshot, report changes
//! scan     <port> <offset> <len>      same, coalesced into runs, for large ranges
//! ```
//!
//! `discover` is the workhorse: start it, change one setting on the radio or
//! from another tool, press Enter, and it names the addresses that moved. That
//! is how the runtime offset map gets built without reverse engineering each
//! field.
//!
//! `scan` is the same idea aimed at a large window, and it coalesces changed
//! bytes into contiguous runs. A display redraw shows up as one large run,
//! which is how the framebuffer is meant to be located: read a wide window,
//! change what is on screen, and look for a run of the right size.
//!
//! ```text
//! cargo run -p kenwood-thd75 --example verify_state -- probe /dev/cu.usbmodemXXXX
//! cargo run -p kenwood-thd75 --example verify_state -- dump /dev/cu.usbmodemXXXX 17D1BC 40
//! ```

// Dependencies visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without weakening
// the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::io::{BufRead, Write};

use kenwood_thd75::error::TransportError;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::DdrOffset;
use kenwood_thd75::verify::{ByteChange, RuntimeOffsetMap};
use kenwood_thd75::{Error, Radio};

type Failure = Box<dyn std::error::Error>;

/// Baud rate for normal CAT over USB.
const CAT_BAUD: u32 = 115_200;

/// Bytes shown per hexdump line.
const DUMP_WIDTH: usize = 16;

fn usage() -> String {
    concat!(
        "usage:\n",
        "  verify_state probe    <port>\n",
        "  verify_state dump     <port> <offset-hex> <len-dec>\n",
        "  verify_state discover <port> <offset-hex> <len-dec>\n",
        "  verify_state scan     <port> <offset-hex> <len-dec>\n",
    )
    .to_owned()
}

/// Prints a classic offset/hex/ASCII dump.
fn hexdump(base: u32, data: &[u8]) {
    for (row, chunk) in data.chunks(DUMP_WIDTH).enumerate() {
        let addr = base as usize + row * DUMP_WIDTH;
        let hex: String = chunk
            .iter()
            .map(|b| format!("{b:02X} "))
            .collect::<Vec<_>>()
            .concat();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7F).contains(&b) {
                    char::from(b)
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {addr:06X}  {hex:<48} |{ascii}|");
    }
}

/// Blocks until the operator presses Enter.
///
/// Returns an [`std::io::Error`] rather than a boxed error so the caller can
/// map it straight into the radio error type without stringly-typed rewrapping.
fn wait_for_operator(prompt: &str) -> std::io::Result<()> {
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line)?;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "stdin closed before the operator confirmed",
        ));
    }
    Ok(())
}

/// Groups changed bytes into contiguous runs so a large redraw reads as one
/// finding rather than thousands.
fn coalesce(changes: &[ByteChange]) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for change in changes {
        let addr = change.offset.as_u32();
        match runs.last_mut() {
            Some(last) if addr == last.0 + last.1 => last.1 += 1,
            _ => runs.push((addr, 1)),
        }
    }
    runs
}

async fn run() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().ok_or_else(usage)?.as_str();
    let port = args.get(1).ok_or_else(usage)?;

    let transport = SerialTransport::open(port, CAT_BAUD)?;
    let mut radio = Radio::connect(transport).await?;

    // Every mode probes first. A read that is trusted without this is a read
    // that might be answering with something else entirely.
    println!("Probing memory-read capability ...");
    radio.probe_mem_read().await?;
    println!("  OK: the radio answered the known constant byte for byte.\n");

    if mode == "probe" {
        return Ok(());
    }

    let offset_arg = args.get(2).ok_or_else(usage)?;
    let len_arg = args.get(3).ok_or_else(usage)?;
    let raw_offset = u32::from_str_radix(offset_arg, 16)?;
    let offset = DdrOffset::new(raw_offset)?;
    let len: u32 = len_arg.parse()?;

    match mode {
        "dump" => {
            let bytes = radio.read_memory_range(offset, len).await?;
            println!("{len} bytes at {offset}:");
            hexdump(raw_offset, &bytes);
        }
        "discover" | "scan" => {
            let windows = [(offset, len)];
            println!("Capturing {len} bytes at {offset} ...");
            let changes = radio
                .discover_field(&windows, async |_| {
                    wait_for_operator(
                        "Change ONE setting now (radio keypad or another tool), \
                         then press Enter: ",
                    )
                    .map_err(|e| Error::Transport(TransportError::Read(e)))
                })
                .await?;

            if changes.is_empty() {
                println!(
                    "\nNo bytes changed. That is a result, not a failure: either the \
                     setting lives outside this window, or the change never reached \
                     the running radio. Widen the window, or verify the change took \
                     effect on the display."
                );
                return Ok(());
            }

            if mode == "scan" {
                let runs = coalesce(&changes);
                println!("\n{} changed bytes in {} runs:", changes.len(), runs.len());
                for (start, length) in &runs {
                    println!("  {start:06X}  {length} bytes");
                }
                println!(
                    "\nA run of roughly 129,600 bytes is the screen-capture image size \
                     for a 240 by 180 24-bit frame, which is what a framebuffer would \
                     look like."
                );
            } else {
                println!("\n{} changed bytes:", changes.len());
                for change in &changes {
                    println!(
                        "  {}  {:02X} -> {:02X}",
                        change.offset, change.before, change.after
                    );
                }

                let mut map = RuntimeOffsetMap::default();
                let offsets: Vec<DdrOffset> = changes.iter().map(|c| c.offset).collect();
                map.record("discovered", &offsets);
                println!(
                    "\nRecord this in a runtime offset map as:\n{}",
                    map.to_text()
                );
            }
        }
        other => return Err(format!("unknown mode {other:?}\n\n{}", usage()).into()),
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Failure> {
    run().await
}
