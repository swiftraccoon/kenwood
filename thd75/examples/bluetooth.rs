//! Connect and control the radio via Bluetooth SPP.
//!
//! Uses native `IOBluetooth` RFCOMM on macOS and a serial RFCOMM port
//! on Linux or Windows. The radio must be paired first via Menu 934.
//!
//! Usage:
//! ```text
//! # macOS: optionally pass an exact paired-device name or address
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
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::transport::Transport;
use kenwood_thd75::types::Band;
use kenwood_thd75::{FirmwareProfile, Radio};

#[cfg(target_os = "linux")]
const DEFAULT_BT_PORT: Option<&str> = Some("/dev/rfcomm0");

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const DEFAULT_BT_PORT: Option<&str> = None;

#[cfg(target_os = "macos")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use kenwood_thd75::transport::BluetoothTransport;

    let device_identifier = std::env::args().nth(1);
    println!(
        "Connecting via native Bluetooth RFCOMM to {}...",
        device_identifier.as_deref().unwrap_or("TH-D75")
    );
    println!("(Radio must be paired via Menu 934 first.)\n");

    let transport = BluetoothTransport::open(device_identifier.as_deref())?;
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

    let transport = SerialTransport::open(&port)?;
    inspect_radio(transport).await
}

async fn inspect_radio<T: Transport>(transport: T) -> Result<(), Box<dyn std::error::Error>> {
    let mut radio = Radio::new(transport);

    // Identify.
    let info = radio.identify().await?;
    println!("Model:    {}", info.model);

    let fw = radio.get_firmware_version().await?;
    println!("Firmware: {fw}");
    let firmware_profile = FirmwareProfile::from_identity(&fw);

    let serial_information = radio.get_serial_information().await?;
    println!("Serial:   {}", serial_information.serial_number());
    println!("Model code: {}", serial_information.model_code());

    let tnc = radio.get_tnc_mode().await?;
    println!("TNC mode: {} ({})", tnc.mode, tnc.data_rate);

    if firmware_profile.supports_bare_gateway() {
        let gateway = radio.read_gateway().await?;
        println!("DV Gateway: {gateway}");
    } else {
        println!("DV Gateway: unavailable (bare GW is not qualified for this firmware)");
    }

    let gps_settings = radio.get_gps_settings().await?;
    println!(
        "GPS: {} (PC output {})",
        if gps_settings.enabled() { "ON" } else { "OFF" },
        if gps_settings.pc_output() {
            "ON"
        } else {
            "OFF"
        }
    );

    // Read state from both bands.
    for band in [Band::A, Band::B] {
        let freq = radio.get_frequency(band).await?;
        let mode = radio.get_operating_mode(band).await?;
        let smeter = radio.get_smeter(band).await?;
        println!("Band {band}: {freq} {mode} S={smeter:02}");
    }

    // Check Bluetooth state (should be on since we are connected via BT).
    let bt_on = radio.get_bluetooth().await?;
    println!("\nBluetooth: {}", if bt_on { "ON" } else { "OFF" });

    println!("\nNote: this example stays in normal CAT mode.");
    println!("MCP reads are explicit programming operations, not passive inspection.");

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
