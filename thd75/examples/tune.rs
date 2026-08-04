//! Recall a regular memory channel.
//!
//! Memory recall automatically handles VFO/Memory mode switching. Arbitrary
//! frequency writes are intentionally absent because no complete FO writer
//! has been qualified.
//!
//! Usage:
//! ```text
//! cargo run --example tune -- --band b --channel 21
//! ```
//!
//! Pass a custom serial port as the last positional argument.

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
use kenwood_thd75::types::{Band, RegularChannel};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Parse --band (required).
    let band_str = args
        .iter()
        .position(|a| a == "--band")
        .and_then(|i| args.get(i + 1));
    let band = match band_str.map(|s| s.to_lowercase()) {
        Some(ref s) if s == "a" || s == "0" => Band::A,
        Some(ref s) if s == "b" || s == "1" => Band::B,
        Some(ref other) => {
            eprintln!("Unknown band: {other} (use 'a' or 'b')");
            std::process::exit(1);
        }
        None => {
            eprintln!("Usage: tune --band <a|b> --channel <num> [port]");
            std::process::exit(1);
        }
    };

    let channel: RegularChannel = args
        .iter()
        .position(|a| a == "--channel")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.parse::<u16>())
        .transpose()?
        .map(RegularChannel::new)
        .transpose()?
        .ok_or("missing --channel <num>")?;

    // Serial port is the last positional arg that starts with '/dev/' or 'COM'.
    let port = args
        .iter()
        .find(|a| a.starts_with("/dev/") || a.starts_with("COM"))
        .cloned()
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::new(transport);

    let info = radio.identify().await?;
    println!("Connected to: {}", info.model);

    println!("Tuning band {band} to channel {channel}...");
    radio.tune_channel(band, channel).await?;
    println!("Done.");

    // Read back to confirm.
    let readback = radio.get_frequency(band).await?;
    println!("Band {band} now on: {} Hz", readback.as_hz());

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
