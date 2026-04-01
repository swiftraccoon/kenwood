//! Connect and control the radio via Bluetooth SPP.
//!
//! Demonstrates Bluetooth serial port connection. The radio must be
//! paired first via Menu 934 on the TH-D75.
//!
//! Usage:
//! ```text
//! cargo run --example bluetooth
//! cargo run --example bluetooth -- /dev/cu.TH-D75
//! ```
//!
//! On macOS the Bluetooth SPP port is typically `/dev/cu.TH-D75`.
//! On Linux it is `/dev/rfcomm0` (after `rfcomm bind`).
//! On Windows it is a COM port assigned during pairing.

use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;
use kenwood_thd75::Radio;

/// Default Bluetooth SPP port on macOS after pairing.
const DEFAULT_BT_PORT: &str = "/dev/cu.TH-D75";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_BT_PORT.to_owned());

    println!("Connecting via Bluetooth SPP on {port}...");
    println!("(Radio must be paired via Menu 934 first.)\n");

    let transport = SerialTransport::open(&port, SerialTransport::DEFAULT_BAUD)?;
    let mut radio = Radio::connect(transport).await?;

    // Identify.
    let info = radio.identify().await?;
    println!("Model:    {}", info.model);

    let fw = radio.get_firmware_version().await?;
    println!("Firmware: {fw}");

    // Read state from both bands.
    for band in [Band::A, Band::B] {
        let freq = radio.get_frequency(band).await?;
        let mode = radio.get_mode(band).await?;
        let smeter = radio.get_smeter(band).await?;
        println!(
            "Band {band}: {} {mode} S={smeter:02}",
            freq.rx_frequency,
        );
    }

    // Check Bluetooth state (should be on since we are connected via BT).
    let bt_on = radio.get_bluetooth().await?;
    println!("\nBluetooth: {}", if bt_on { "ON" } else { "OFF" });

    // Note: MCP programming mode is NOT available over Bluetooth.
    // Only CAT commands work over BT SPP.
    println!("\nNote: MCP programming requires USB. CAT commands work over BT.");

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
