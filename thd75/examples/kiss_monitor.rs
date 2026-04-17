//! Monitor APRS packets via KISS TNC.
//!
//! Demonstrates KISS frame decoding, AX.25 packet parsing, and APRS
//! position report extraction. Reads raw bytes from the serial port
//! and decodes any KISS-framed packets received.
//!
//! Before running, enable KISS mode on the radio:
//! Press `[F]`, then `[LIST]`, cycle to "KISS mode ON".
//!
//! Usage:
//! ```text
//! cargo run --example kiss_monitor
//! cargo run --example kiss_monitor -- /dev/cu.usbmodem1234
//! ```
//!
//! Press Ctrl+C to stop.

use kenwood_thd75::Transport;
use kenwood_thd75::transport::SerialTransport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Opening {port} for KISS TNC monitoring...");
    println!("(Enable KISS mode on the radio first: [F] > [LIST] > KISS mode ON)");
    println!("Press Ctrl+C to stop.\n");

    let mut transport = SerialTransport::open(&port, 9600)?;

    let mut buf = [0u8; 2048];
    let mut accumulator = Vec::new();
    let mut packet_count: u64 = 0;

    loop {
        let n = transport.read(&mut buf).await?;
        if n == 0 {
            eprintln!("Connection closed.");
            break;
        }

        accumulator.extend_from_slice(&buf[..n]);

        // Scan for complete KISS frames (FEND-delimited).
        while let Some(frame_data) = extract_kiss_frame(&mut accumulator) {
            if frame_data.is_empty() {
                continue;
            }

            packet_count += 1;
            print!("[{packet_count:04}] ");

            // Decode the KISS frame.
            match kiss_tnc::decode_kiss_frame(&frame_data) {
                Ok(kiss_frame) => {
                    if kiss_frame.command != kiss_tnc::CMD_DATA {
                        println!(
                            "KISS cmd=0x{:02X} ({} bytes)",
                            kiss_frame.command,
                            kiss_frame.data.len()
                        );
                        continue;
                    }

                    // Parse the AX.25 payload.
                    if let Ok(ax25) = ax25_codec::parse_ax25(&kiss_frame.data) {
                        print!("{}>{}", ax25.source, ax25.destination,);
                        for digi in &ax25.digipeaters {
                            print!(",{digi}");
                        }
                        print!(": ");

                        // Try to parse as APRS position report.
                        if let Ok(pos) = aprs::parse_aprs_position(&ax25.info) {
                            println!(
                                "APRS {:.4},{:.4} ({}{}) \"{}\"",
                                pos.latitude,
                                pos.longitude,
                                pos.symbol_table,
                                pos.symbol_code,
                                pos.comment,
                            );
                        } else {
                            // Not a position report -- show raw info.
                            let info_str = String::from_utf8_lossy(&ax25.info);
                            println!("{info_str}");
                        }
                    } else {
                        println!("AX.25 parse error ({} bytes)", kiss_frame.data.len());
                    }
                }
                Err(e) => {
                    println!("KISS decode error: {e:?} ({} bytes)", frame_data.len());
                }
            }
        }
    }

    Ok(())
}

/// Extract the first complete KISS frame from the accumulator.
///
/// Returns `Some(frame_bytes)` if a FEND-delimited frame is found,
/// removing it (and the delimiters) from the accumulator. Returns
/// `None` if no complete frame is available yet.
fn extract_kiss_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    // Find first FEND.
    let start = buf.iter().position(|&b| b == kiss_tnc::FEND)?;

    // Find next FEND after start.
    let end = buf[start + 1..]
        .iter()
        .position(|&b| b == kiss_tnc::FEND)
        .map(|i| start + 1 + i)?;

    // Extract the frame (between the two FENDs).
    let frame: Vec<u8> = buf[start..=end].to_vec();

    // Remove processed bytes from accumulator.
    drop(buf.drain(..=end));

    Some(frame)
}
