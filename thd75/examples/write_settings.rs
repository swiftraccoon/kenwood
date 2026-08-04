//! Write radio settings via CAT and MCP.
//!
//! Demonstrates a direct CAT setting write and an MCP channel-name
//! memory write. The CAT portion restores the original squelch value;
//! the MCP portion intentionally overwrites channel 0's display name.
//!
//! Usage:
//! ```text
//! cargo run --example write_settings
//! cargo run --example write_settings -- /dev/cu.usbmodem1234
//! ```
//!
//! **Warning:** The MCP portion permanently changes channel 0's display
//! name to `EXAMPLE`; restore it manually after running this example.
//! It enters programming mode, shows "PROG MCP", and resets USB when done.

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, ChannelDisplayName, RegularChannel};

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
    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::new(transport);

    let info = radio.identify().await?;
    println!("Connected to: {}\n", info.model);

    // Read current squelch, change it, then restore.
    let band = Band::A;
    let original_squelch = radio.get_squelch(band).await?;
    println!("Band A squelch: {}", original_squelch.as_raw());

    let test_val = if original_squelch.as_raw() >= 3 {
        original_squelch.as_raw() - 1
    } else {
        original_squelch.as_raw() + 1
    };
    let test_squelch = kenwood_thd75::types::SquelchLevel::new(test_val)?;
    println!("Setting squelch to {}...", test_squelch.as_raw());
    radio.set_squelch(band, test_squelch).await?;

    let readback = radio.get_squelch(band).await?;
    println!("Squelch readback: {}", readback.as_raw());

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
    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::new(transport);

    // Write a channel name via MCP (read-modify-write of one page).
    // This enters and exits programming mode.
    println!("WARNING: overwriting channel 0 name with 'EXAMPLE' via MCP.");
    println!("(Radio will show 'PROG MCP' briefly)\n");
    let name = ChannelDisplayName::new("EXAMPLE")?;
    radio
        .write_channel_name(RegularChannel::new(0)?, &name)
        .await?;

    println!("Channel name written.");
    println!("CAT was restored after the radio reset USB.");
    println!("Restore channel 0's original name manually when finished.");
    println!("\nTo verify: reconnect and run `cargo run --example channel_dump -- --names`");

    Ok(())
}
