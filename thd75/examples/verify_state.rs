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
//! probe    <target> <port>                     attest the patched target
//! qualify  <target> <port>                     attest the patched target
//! dump     <target> <port> <offset> <len>      read and hexdump a region
//! discover ddr      <port> <offset> <len>      snapshot, wait, snapshot, report changes
//! scan     ddr      <port> <offset> <len>      same, coalesced into runs
//! hunt     ddr      <port> <offset> <len>      coarse sampling pass
//! ```
//!
//! `hunt` exists because dense scanning does not scale: 16 MiB is 65,536
//! requests per snapshot, and a diff needs two. It samples 16 bytes every
//! 4 KiB instead, a factor of sixteen fewer requests, which still cannot miss
//! anything framebuffer-sized because such a structure dirties tens of
//! consecutive kilobytes. Use it to narrow, then `scan` the candidate densely.
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
//! `qualify` is what to run first on a freshly modified radio, before trusting
//! any read. It is the encoded form of the post-flash checklist, and it exists
//! in code rather than in prose because one of its steps passes on failure: a
//! read past the accepted window must be refused, and a human working from a
//! checklist can easily record that backwards.
//!
//! ```text
//! cargo run -p kenwood-thd75 --example verify_state -- qualify low-nor /dev/cu.usbmodemXXXX
//! cargo run -p kenwood-thd75 --example verify_state -- qualify ddr /dev/cu.usbmodemXXXX
//! cargo run -p kenwood-thd75 --example verify_state -- dump low-nor /dev/cu.usbmodemXXXX 0 40
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
use std::path::Path;

use kenwood_thd75::Radio;
use kenwood_thd75::radio::memory_read::MemoryReader;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{MemoryReadOffset, MemoryReadTarget, ReadLen};
use kenwood_thd75::verify::{ByteChange, RuntimeOffsetMap};

type Failure = Box<dyn std::error::Error>;

/// Baud rate for normal CAT over USB.
const CAT_BAUD: u32 = 115_200;

/// Bytes shown per hexdump line.
const DUMP_WIDTH: usize = 16;

#[derive(Debug, Clone, Copy)]
enum Operation {
    Qualify,
    Dump {
        offset: MemoryReadOffset,
        raw_offset: u32,
        len: u32,
    },
    Discover {
        offset: MemoryReadOffset,
        len: u32,
        coalesced: bool,
    },
    Hunt {
        offset: MemoryReadOffset,
        len: u32,
    },
}

#[derive(Debug)]
struct Invocation {
    target: MemoryReadTarget,
    port: String,
    operation: Operation,
}

fn usage() -> String {
    concat!(
        "usage:\n",
        "  verify_state probe    <ddr|low-nor> <port>\n",
        "  verify_state qualify  <ddr|low-nor> <port>\n",
        "  verify_state dump     <ddr|low-nor> <port> <offset-hex> <len-dec>\n",
        "  verify_state discover ddr           <port> <offset-hex> <len-dec>\n",
        "  verify_state scan     ddr           <port> <offset-hex> <len-dec>\n",
        "  verify_state hunt     ddr           <port> <offset-hex> <len-dec>\n",
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

/// Number of sample points a coarse pass will take.
const fn total_points(len: u32, stride: u32) -> u32 {
    len.div_ceil(stride)
}

/// Groups changed *sample points* into runs of consecutive points.
///
/// Distinct from [`coalesce`], which merges byte-adjacent changes. In a coarse
/// pass the changed bytes sit `stride` apart, so byte adjacency never holds and
/// merging has to happen at sample-point granularity instead. Returns each run
/// as a start offset and a count of points.
fn coalesce_samples(changes: &[ByteChange], stride: u32) -> Vec<(u32, u32)> {
    let mut points: Vec<u32> = changes
        .iter()
        .map(|c| (c.offset.as_u32() / stride) * stride)
        .collect();
    points.sort_unstable();
    points.dedup();

    let mut runs: Vec<(u32, u32)> = Vec::new();
    for point in points {
        match runs.last_mut() {
            Some(last) if point == last.0 + last.1 * stride => last.1 += 1,
            _ => runs.push((point, 1)),
        }
    }
    runs
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

fn parse_invocation(args: &[String]) -> Result<Invocation, Failure> {
    let mode = args.first().ok_or_else(usage)?.as_str();
    let target = match args.get(1).ok_or_else(usage)?.as_str() {
        "ddr" => MemoryReadTarget::DdrV103,
        "low-nor" => MemoryReadTarget::LowNorV103,
        other => return Err(format!("unknown target {other:?}\n\n{}", usage()).into()),
    };
    let port = args.get(2).ok_or_else(usage)?.clone();
    if target == MemoryReadTarget::LowNorV103 && matches!(mode, "discover" | "scan" | "hunt") {
        return Err("discover, scan, and hunt describe mutable DDR; use target ddr".into());
    }

    let operation = match mode {
        "probe" | "qualify" if args.len() == 3 => Operation::Qualify,
        "dump" | "discover" | "scan" | "hunt" if args.len() == 5 => {
            let raw_offset = u32::from_str_radix(args.get(3).ok_or_else(usage)?, 16)?;
            let offset = MemoryReadOffset::new(raw_offset)?;
            let len: u32 = args.get(4).ok_or_else(usage)?.parse()?;
            let _chunks =
                kenwood_thd75::protocol::memread::plan_read_for_target(target, offset, len)?;
            match mode {
                "dump" => Operation::Dump {
                    offset,
                    raw_offset,
                    len,
                },
                "discover" => Operation::Discover {
                    offset,
                    len,
                    coalesced: false,
                },
                "scan" => Operation::Discover {
                    offset,
                    len,
                    coalesced: true,
                },
                "hunt" => Operation::Hunt { offset, len },
                _ => return Err(usage().into()),
            }
        }
        "probe" | "qualify" | "dump" | "discover" | "scan" | "hunt" => {
            return Err(usage().into());
        }
        other => return Err(format!("unknown mode {other:?}\n\n{}", usage()).into()),
    };

    Ok(Invocation {
        target,
        port,
        operation,
    })
}

fn validate_usb_port(port: &str) -> Result<(), Failure> {
    if !Path::new(port).is_absolute() || SerialTransport::is_bluetooth_port(port) {
        return Err("the GM verifier requires an absolute USB CDC path, not Bluetooth".into());
    }
    let matches = SerialTransport::discover_usb()?
        .into_iter()
        .filter(|candidate| candidate.port_name == port)
        .count();
    if matches == 1 {
        Ok(())
    } else {
        Err(
            format!("the exact port must enumerate once as TH-D75 USB VID:PID 2166:9023: {port}")
                .into(),
        )
    }
}

async fn run() -> Result<(), Failure> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = parse_invocation(&args)?;
    validate_usb_port(&invocation.port)?;

    let transport = SerialTransport::open(&invocation.port, CAT_BAUD)?;
    let mut radio = Radio::connect(transport).await?;

    println!(
        "Attesting {} memory-read target ...",
        invocation.target.as_str()
    );
    let mut reader = radio.qualify_mem_read_for(invocation.target).await?;
    println!("  PASS: exact V1.03 identity, patch, data, and bounds checks.\n");

    match invocation.operation {
        Operation::Qualify => Ok(()),
        Operation::Dump {
            offset,
            raw_offset,
            len,
        } => mode_dump(&mut reader, offset, raw_offset, len).await,
        Operation::Discover {
            offset,
            len,
            coalesced,
        } => mode_discover(&mut reader, offset, len, coalesced).await,
        Operation::Hunt { offset, len } => mode_hunt(&mut reader, offset, len).await,
    }
}

/// Reads a region and hexdumps it.
async fn mode_dump(
    reader: &mut MemoryReader<'_, SerialTransport>,
    offset: MemoryReadOffset,
    raw_offset: u32,
    len: u32,
) -> Result<(), Failure> {
    let bytes = reader.read_memory_range(offset, len).await?;
    println!("{len} bytes at {offset}:");
    hexdump(raw_offset, &bytes);
    Ok(())
}

/// Snapshots a window, waits for the operator to change something, snapshots
/// again, and reports what moved.
async fn mode_discover(
    reader: &mut MemoryReader<'_, SerialTransport>,
    offset: MemoryReadOffset,
    len: u32,
    coalesced: bool,
) -> Result<(), Failure> {
    let windows = [(offset, len)];
    println!("Capturing {len} bytes at {offset} ...");
    let before = reader.capture_snapshot(&windows).await?;
    wait_for_operator("Change ONE setting on the radio, then press Enter: ")?;
    let after = reader.capture_snapshot(&windows).await?;
    let changes = before.diff(&after)?;

    if changes.is_empty() {
        println!(
            "\nNo bytes changed. That is a result, not a failure: either the \
             setting lives outside this window, or the change never reached the \
             running radio. Widen the window, or verify the change took effect \
             on the display."
        );
        return Ok(());
    }

    if coalesced {
        let runs = coalesce(&changes);
        println!("\n{} changed bytes in {} runs:", changes.len(), runs.len());
        for (start, length) in &runs {
            println!("  {start:06X}  {length} bytes");
        }
        println!(
            "\nA run of roughly 129,600 bytes is the screen-capture image size for \
             a 240 by 180 24-bit frame, which is what a framebuffer would look like."
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
        let offsets: Vec<MemoryReadOffset> = changes.iter().map(|c| c.offset).collect();
        map.record("discovered", &offsets);
        println!(
            "\nRecord this in a runtime offset map as:\n{}",
            map.to_text()
        );
    }
    Ok(())
}

/// Coarse-to-fine search for a large structure such as a framebuffer.
///
/// Dense scanning does not scale to the whole window: 16 MiB is 65,536 requests
/// per snapshot and a diff needs two. Sampling 16 bytes every 4 KiB is 4,096,
/// and anything framebuffer-sized dirties tens of consecutive kilobytes, so it
/// cannot hide between sample points. Something small can, which is the trade.
async fn mode_hunt(
    reader: &mut MemoryReader<'_, SerialTransport>,
    offset: MemoryReadOffset,
    len: u32,
) -> Result<(), Failure> {
    const STRIDE: u32 = 4096;

    let sample = ReadLen::new(16)?;
    let points = total_points(len, STRIDE);
    println!(
        "Coarse pass: {points} samples of 16 bytes every {STRIDE} bytes across \
         {len} bytes from {offset}."
    );

    let before = reader.sample_range(offset, len, STRIDE, sample).await?;
    wait_for_operator(
        "Now change the DISPLAY substantially (switch menus, change band), then press Enter: ",
    )?;
    let after = reader.sample_range(offset, len, STRIDE, sample).await?;

    let changes = before.diff(&after)?;
    if changes.is_empty() {
        println!(
            "\nNothing changed at any sample point. Either the display did not \
             change, or whatever changed is smaller than the sampling can see. \
             Re-run over a narrower range, or scan it densely."
        );
        return Ok(());
    }

    let runs = coalesce_samples(&changes, STRIDE);
    println!(
        "\n{} bytes moved across {} runs of samples:",
        changes.len(),
        runs.len()
    );
    for (start, count) in &runs {
        println!("  {start:06X}  spans about {} bytes", count * STRIDE);
    }
    println!(
        "\nA run spanning roughly 129,600 bytes is the size of a 240 by 180 24-bit \
         frame. Scan the most promising run densely to confirm:\n  \
         verify_state scan ddr <port> <run-start-hex> <run-length-dec>"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MemoryReadTarget, Operation, parse_invocation};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn complete_invocation_is_parsed_before_hardware_access() -> TestResult {
        let qualify = parse_invocation(&arguments(&["qualify", "low-nor", "/dev/cu.usbmodem101"]))?;
        assert_eq!(qualify.target, MemoryReadTarget::LowNorV103);
        assert!(matches!(qualify.operation, Operation::Qualify));

        let dump = parse_invocation(&arguments(&[
            "dump",
            "low-nor",
            "/dev/cu.usbmodem101",
            "1FFFFF",
            "1",
        ]))?;
        assert!(matches!(
            dump.operation,
            Operation::Dump {
                raw_offset: 0x1F_FFFF,
                len: 1,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn invalid_operation_fails_during_parse() {
        for invalid in [
            arguments(&["unknown", "low-nor", "/dev/cu.usbmodem101"]),
            arguments(&["dump", "low-nor", "/dev/cu.usbmodem101"]),
            arguments(&["discover", "low-nor", "/dev/cu.usbmodem101", "0", "16"]),
            arguments(&["dump", "low-nor", "/dev/cu.usbmodem101", "200000", "1"]),
        ] {
            assert!(
                parse_invocation(&invalid).is_err(),
                "invalid invocation reached the hardware phase: {invalid:?}"
            );
        }
    }
}

#[tokio::main]
async fn main() {
    // Print the Display form rather than returning Err from main, which would
    // print the Debug form. The most common failure here is a refusal from
    // unmodified firmware, and that message is written to be read.
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
