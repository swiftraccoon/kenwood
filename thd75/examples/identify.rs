//! Identify the connected radio.
//!
//! Connects over USB serial and prints the radio model ID, firmware version,
//! region code, and current power status.
//!
//! Run: `cargo run --example identify`
//!
//! Pass a custom serial port as the first argument:
//! `cargo run --example identify -- /dev/cu.usbmodem1234`

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

    let info = radio.identify().await?;
    println!("Model:    {}", info.model);

    let fw = radio.get_firmware_version().await?;
    println!("Firmware: {fw}");

    let (region, variant) = radio.get_radio_type().await?;
    println!("Region:   {region} (variant {variant})");

    let power = radio.get_power_status().await?;
    println!("Power:    {}", if power { "ON" } else { "OFF" });

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
