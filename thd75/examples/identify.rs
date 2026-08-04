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
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::{FirmwareProfile, Radio};

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
    let transport = SerialTransport::open(&port)?;
    let mut radio = if recover_cat {
        println!("Sending the opt-in KISS/TNC-to-CAT recovery preamble...");
        Radio::connect_with_tnc_exit(transport).await?
    } else {
        Radio::new(transport)
    };

    let info = match radio.identify().await {
        Ok(info) => info,
        Err(error) => {
            let diagnosis = radio.probe_silent_link().await;
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
    let firmware_profile = FirmwareProfile::from_identity(&fw);

    let serial_information = radio.get_serial_information().await?;
    println!("Serial:   {}", serial_information.serial_number());
    println!("Model code: {}", serial_information.model_code());

    let radio_type = radio.get_radio_type().await?;
    println!(
        "Region:   {} (variant {})",
        radio_type.region(),
        radio_type.hardware_variant()
    );

    let power = radio.get_power_status().await?;
    println!("Power:    {}", if power { "ON" } else { "OFF" });

    let tnc = radio.get_tnc_mode().await?;
    println!("TNC:      {} ({})", tnc.mode, tnc.data_rate);

    let gps_settings = radio.get_gps_settings().await?;
    println!(
        "GPS:      {} (PC output {})",
        if gps_settings.enabled() { "ON" } else { "OFF" },
        if gps_settings.pc_output() {
            "ON"
        } else {
            "OFF"
        }
    );

    let beacon = radio.get_beacon_mode().await?;
    println!("Beacon:   {beacon}");

    let position = radio.get_my_position_selection().await?;
    println!("Position: {position}");

    let antenna_input = radio.get_antenna_input().await?;
    println!("MW/SW input: {antenna_input}");

    let backlight = radio.get_backlight_control().await?;
    println!("Backlight: {backlight:?}");

    if firmware_profile.supports_bare_gateway() {
        let gateway = radio.read_gateway().await?;
        println!("Gateway:  {gateway}");
    } else {
        println!("Gateway:  unavailable for {firmware_profile:?}");
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn automation_firmware_does_not_issue_colliding_stock_gateway_command() -> TestResult {
        let firmware = kenwood_thd75::FirmwareIdentity::new("1.03.AZM")?;
        assert!(!FirmwareProfile::from_identity(&firmware).supports_bare_gateway());
        Ok(())
    }

    #[test]
    fn stock_firmware_retains_gateway_command() -> TestResult {
        let firmware = kenwood_thd75::FirmwareIdentity::new("1.03")?;
        assert!(FirmwareProfile::from_identity(&firmware).supports_bare_gateway());
        Ok(())
    }
}
