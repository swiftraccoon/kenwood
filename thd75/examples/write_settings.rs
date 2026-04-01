//! Write radio settings via CAT and MCP.
//!
//! Demonstrates both direct CAT write commands (for settings the
//! firmware accepts) and MCP memory writes (for settings where CAT
//! writes are rejected, such as beep and Bluetooth).
//!
//! Usage:
//! ```text
//! cargo run --example write_settings
//! cargo run --example write_settings -- /dev/cu.usbmodem1234
//! ```
//!
//! **Warning:** The MCP write portion enters programming mode. The USB
//! connection resets when done. The radio display shows "PROG MCP"
//! during the transfer.

use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;
use kenwood_thd75::Radio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    // ------------------------------------------------------------------
    // Part 1: CAT writes (fast, no MCP needed)
    // ------------------------------------------------------------------
    println!("=== Part 1: CAT writes ===\n");
    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    let info = radio.identify().await?;
    println!("Connected to: {}\n", info.model);

    // Read current squelch, change it, then restore.
    let band = Band::A;
    let original_squelch = radio.get_squelch(band).await?;
    println!("Band A squelch: {original_squelch}");

    let test_squelch = if original_squelch >= 3 {
        original_squelch - 1
    } else {
        original_squelch + 1
    };
    println!("Setting squelch to {test_squelch}...");
    radio.set_squelch(band, test_squelch).await?;

    let readback = radio.get_squelch(band).await?;
    println!("Squelch readback: {readback}");

    println!("Restoring squelch to {original_squelch}...");
    radio.set_squelch(band, original_squelch).await?;
    println!("Restored.\n");

    // Read and display VOX state.
    let vox = radio.get_vox().await?;
    println!("VOX: {}", if vox { "ON" } else { "OFF" });

    radio.disconnect().await?;
    println!("Disconnected from CAT session.\n");

    // ------------------------------------------------------------------
    // Part 2: MCP writes (enters programming mode, USB resets after)
    // ------------------------------------------------------------------
    println!("=== Part 2: MCP writes ===\n");
    println!("Reconnecting to {port} for MCP operations...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    // Write a channel name via MCP (read-modify-write of one page).
    // This enters and exits programming mode.
    println!("Writing channel 0 name to 'EXAMPLE' via MCP...");
    println!("(Radio will show 'PROG MCP' briefly)\n");
    radio.write_channel_name(0, "EXAMPLE").await?;

    println!("Channel name written.");
    println!("USB connection has been reset by the radio.");
    println!("\nTo verify: reconnect and run `cargo run --example channel_dump -- --names`");

    Ok(())
}
