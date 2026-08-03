//! Identify the connected radio.
//!
//! Connects over USB serial and prints the radio model ID, firmware version,
//! region code, and current power status.
//!
//! Run: `cargo run --example identify`
//!
//! Pass a custom serial port as the first argument:
//! `cargo run --example identify -- /dev/cu.usbmodem1234`
//!
//! If a previous KISS/TNC session left the port unable to answer CAT, opt in
//! to the library's documented recovery preamble:
//! `cargo run --example identify -- /dev/cu.usbmodem1234 --recover-cat`

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

const AUTOMATION_FIRMWARE: &str = "1.03.AZM";

fn supports_stock_gateway_command(firmware: &str) -> bool {
    firmware != AUTOMATION_FIRMWARE
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = None;
    let mut recover_cat = false;
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--recover-cat" if !recover_cat => recover_cat = true,
            _ if !argument.starts_with('-') && port.is_none() => port = Some(argument),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("unknown or duplicate argument: {argument}"),
                )
                .into());
            }
        }
    }
    let port = port.unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = if recover_cat {
        println!("Sending the opt-in KISS/TNC-to-CAT recovery preamble...");
        Radio::connect_safe(transport).await?
    } else {
        Radio::connect(transport).await?
    };

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

    let beacon = radio.get_beacon_type().await?;
    println!("Beacon:   {beacon}");

    let position = radio.get_my_position_selection().await?;
    println!("Position: {position}");

    let bar_antenna = radio.get_bar_antenna().await?;
    println!(
        "Bar ant.:  {}",
        if bar_antenna { "ENABLED" } else { "DISABLED" }
    );

    let backlight = radio.get_backlight_control().await?;
    println!("Backlight: {backlight:?}");

    if supports_stock_gateway_command(&fw) {
        let gateway = radio.get_gateway().await?;
        println!("Gateway:  {gateway}");
    } else {
        println!("Gateway:  unavailable (GW is reserved by {AUTOMATION_FIRMWARE})");
    }

    let bluetooth = radio.get_bluetooth().await?;
    println!("Bluetooth: {}", if bluetooth { "ON" } else { "OFF" });

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automation_firmware_does_not_issue_colliding_stock_gateway_command() {
        assert!(!supports_stock_gateway_command(AUTOMATION_FIRMWARE));
    }

    #[test]
    fn stock_firmware_retains_gateway_command() {
        assert!(supports_stock_gateway_command("1.03"));
    }
}
