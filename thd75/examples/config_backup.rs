//! Backup full radio configuration via MCP programming mode.
//!
//! Reads the entire 500KB radio memory and writes it to a binary file.
//! This captures all channels, settings, APRS configuration, D-STAR
//! callsigns, and GPS waypoints.
//!
//! Run: `cargo run --example config_backup`
//!
//! Pass a custom output path and serial port:
//! `cargo run --example config_backup -- backup.bin /dev/cu.usbmodem1234`
//!
//! **Note:** This enters MCP programming mode at 9600 baud. The radio
//! display shows "PROG MCP" during the transfer (~55 seconds for a full
//! dump). The USB connection resets when done.

use std::path::PathBuf;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;

/// Baud rate for MCP programming mode entry (CAT commands use this rate
/// for the initial handshake, then the entire session stays at 9600).
const PROGRAMMING_ENTRY_BAUD: u32 = 9600;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let output = args
        .get(1)
        .map_or_else(|| PathBuf::from("thd75_backup.bin"), PathBuf::from);
    let port = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port} at {PROGRAMMING_ENTRY_BAUD} baud...");
    let transport = SerialTransport::open(&port, PROGRAMMING_ENTRY_BAUD)?;
    let mut radio = Radio::connect(transport).await?;

    println!("Reading full memory image (this takes ~55 seconds)...\n");

    let image = radio
        .read_memory_image_with_progress(|page, total| {
            if page % 100 == 0 || page == total - 1 {
                let pct = (page + 1) * 100 / total;
                eprint!("\r  Page {}/{} ({pct}%)", page + 1, total);
            }
        })
        .await?;

    eprintln!();
    println!(
        "\nRead {} bytes ({} pages).",
        image.len(),
        image.len() / 256
    );

    std::fs::write(&output, &image)?;
    println!("Saved to: {}", output.display());

    println!("\nUSB connection has been reset by the radio.");
    println!("Reconnect for further CAT commands.");

    Ok(())
}
