//! Monitor APRS packets via KISS TNC.
//!
//! Demonstrates KISS frame decoding, AX.25 packet parsing, and APRS
//! position report extraction. Reads raw bytes from the serial port
//! and decodes any KISS-framed packets received.
//!
//! Before running, enable KISS on the interface selected by Menu 983.
//! This reader opens that existing binary connection without issuing CAT.
//!
//! Usage:
//! ```text
//! cargo run --example kiss_monitor
//! cargo run --example kiss_monitor -- /dev/cu.usbmodem1234
//! ```
//!
//! Press Ctrl+C to stop.

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs_is as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kenwood_schema as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::num::NonZeroU32;

use kenwood_thd75::transport::SerialTransport;
use kenwood_transport::Transport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Opening {port} for KISS TNC monitoring...");
    println!("Enable KISS on the interface selected by Menu 983 before opening it.");
    println!("Press Ctrl+C to stop.\n");

    let mut transport =
        SerialTransport::open_with_baud(&port, NonZeroU32::new(9600).ok_or("nonzero baud")?)?;

    let mut buf = [0u8; 2048];
    let mut decoder = kiss_tnc::KissDecoder::new();
    let mut packet_count: u64 = 0;

    loop {
        let n = transport.read(&mut buf).await?;
        if n == 0 {
            eprintln!("Connection closed.");
            break;
        }

        decoder.push(
            buf.get(..n)
                .ok_or("transport returned more bytes than the buffer holds")?,
        );

        loop {
            match decoder.next_frame() {
                Ok(None) => break,
                Ok(Some(kiss_frame)) => {
                    packet_count += 1;
                    print!("[{packet_count:04}] ");
                    if kiss_frame.command != kiss_tnc::KissCommand::Data {
                        println!(
                            "KISS cmd=0x{:02X} ({} bytes)",
                            kiss_frame.command.as_byte(),
                            kiss_frame.data.len()
                        );
                        continue;
                    }

                    // Parse the AX.25 payload.
                    if let Ok(ax25) = ax25_codec::Ax25Packet::from_bytes(&kiss_frame.data) {
                        print!("{}>{}", ax25.header.source, ax25.header.destination);
                        for digi in &ax25.header.digipeaters {
                            print!(",{digi}");
                        }
                        print!(": ");

                        // Try to parse as APRS position report.
                        if let Ok(pos) = aprs::AprsPosition::from_bytes(ax25.information()) {
                            println!(
                                "APRS {:.4},{:.4} ({}{}) \"{}\"",
                                pos.latitude(),
                                pos.longitude(),
                                pos.symbol().table_char(),
                                pos.symbol().code_char(),
                                pos.comment(),
                            );
                        } else {
                            // Not a position report -- show raw info.
                            let info_str = String::from_utf8_lossy(ax25.information());
                            println!("{info_str}");
                        }
                    } else {
                        println!("AX.25 parse error ({} bytes)", kiss_frame.data.len());
                    }
                }
                Err(e) => {
                    println!("KISS decode error: {e}");
                }
            }
        }
    }

    Ok(())
}
