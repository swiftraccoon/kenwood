//! Connect and control the radio via Bluetooth SPP.
//!
//! Uses native `IOBluetooth` RFCOMM on macOS and a serial RFCOMM port
//! on Linux or Windows. The radio must be paired first via Menu 934.
//!
//! Usage:
//! ```text
//! # macOS: optionally pass a paired Bluetooth device name
//! cargo run -p kenwood-thd75 --example bluetooth
//! cargo run -p kenwood-thd75 --example bluetooth -- TH-D75
//!
//! # Linux/Windows: pass the RFCOMM serial port
//! cargo run -p kenwood-thd75 --example bluetooth -- /dev/rfcomm0
//! cargo run -p kenwood-thd75 --example bluetooth -- COM7
//! ```
//!
//! Do not use `/dev/cu.TH-D75` on macOS: Apple's Bluetooth serial
//! driver drops data for this radio. The native transport bypasses it.

// Deps visible to every kenwood-thd75 example target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
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

use kenwood_thd75::Radio;
use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::Band;

#[cfg(target_os = "linux")]
const DEFAULT_BT_PORT: Option<&str> = Some("/dev/rfcomm0");

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DEFAULT_BT_PORT: Option<&str> = None;

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kenwood_thd75::transport::BluetoothTransport;

    let device_name = std::env::args().nth(1);
    println!(
        "Connecting via native Bluetooth RFCOMM to {}...",
        device_name.as_deref().unwrap_or("TH-D75")
    );
    println!("(Radio must be paired via Menu 934 first.)\n");

    let transport = BluetoothTransport::open(device_name.as_deref())?;
    inspect_radio(transport).await
}

#[cfg(not(target_os = "macos"))]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kenwood_thd75::transport::SerialTransport;

    let port = std::env::args()
        .nth(1)
        .or_else(|| DEFAULT_BT_PORT.map(str::to_owned))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "pass the Bluetooth serial port, for example COM7",
            )
        })?;

    println!("Connecting via Bluetooth SPP on {port}...");
    println!("(Radio must be paired via Menu 934 first.)\n");

    let transport = SerialTransport::open(&port, SerialTransport::DEFAULT_BAUD)?;
    inspect_radio(transport).await
}

async fn inspect_radio<T: Transport>(transport: T) -> Result<(), Box<dyn std::error::Error>> {
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
        println!("Band {band}: {} {mode} S={smeter:02}", freq.rx_frequency,);
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
