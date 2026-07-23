//! Identify the connected radio.
//!
//! Connects over USB serial and prints the radio model ID, firmware version,
//! region code, and current power status.
//!
//! Run: `cargo run --example identify`
//!
//! Pass a custom serial port as the first argument:
//! `cargo run --example identify -- /dev/cu.usbmodem1234`

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
use kenwood_thd75::transport::SerialTransport;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    let info = match radio.identify().await {
        Ok(info) => info,
        Err(error) => {
            let diagnosis = radio.diagnose_link().await;
            println!("CAT identification failed: {error}");
            println!("{}", diagnosis.guidance());
            drop(radio.disconnect().await);
            let boxed: Box<dyn std::error::Error> = Box::new(error);
            return Err(boxed);
        }
    };
    println!("Model:    {}", info.model);

    let fw = radio.get_firmware_version().await?;
    println!("Firmware: {fw}");

    let (region, variant) = radio.get_radio_type().await?;
    println!("Region:   {region} (variant {variant})");

    let power = radio.get_power_status().await?;
    println!("Power:    {}", if power { "ON" } else { "OFF" });

    let (tnc_mode, tnc_baud) = radio.get_tnc_mode().await?;
    println!("TNC:      {tnc_mode} ({tnc_baud})");

    let (gps_enabled, gps_pc_output) = radio.get_gps_config().await?;
    println!(
        "GPS:      {} (PC output {})",
        if gps_enabled { "ON" } else { "OFF" },
        if gps_pc_output { "ON" } else { "OFF" }
    );

    let gateway = radio.get_gateway().await?;
    println!("Gateway:  {gateway}");

    let bluetooth = radio.get_bluetooth().await?;
    println!("Bluetooth: {}", if bluetooth { "ON" } else { "OFF" });

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
