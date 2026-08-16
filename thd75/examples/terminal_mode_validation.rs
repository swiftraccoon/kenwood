//! Live validation of the Reflector Terminal Mode lifecycle.
//!
//! Two directions, run separately:
//!
//! - `--off-via-bluetooth`: with the radio in terminal mode bound to USB,
//!   connect over native Bluetooth (the port that still answers CAT), write
//!   Menu 650 off through [`Radio::set_dv_gateway_mode_detached`], then poll
//!   the USB port until CAT identity answers again.
//! - `--enter-via-usb`: with the radio in normal CAT mode, run
//!   [`Radio::enter_reflector_terminal_mode`] over USB and report the MMDVM
//!   proof or the transition timeout.
//!
//! Usage:
//! ```text
//! cargo run -p kenwood-thd75 --example terminal_mode_validation -- --off-via-bluetooth
//! cargo run -p kenwood-thd75 --example terminal_mode_validation -- --enter-via-usb /dev/cu.usbmodem101
//! ```

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

#[cfg(target_os = "macos")]
use std::time::Duration;

use kenwood_thd75::transport::SerialTransport;
#[cfg(target_os = "macos")]
use kenwood_thd75::types::DvGatewayMode;
use kenwood_thd75::{Radio, TerminalModeTransition};

/// How long to wait for CAT to return after the Menu 650 off reboot.
#[cfg(target_os = "macos")]
const CAT_RETURN_WINDOW: Duration = Duration::from_secs(60);

/// Delay between CAT reconnect attempts during the reboot wait.
#[cfg(target_os = "macos")]
const CAT_RETURN_POLL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().unwrap_or_default();
    match mode.as_str() {
        "--off-via-bluetooth" => {
            let usb_port = arguments.next();
            off_via_bluetooth(usb_port.as_deref()).await
        }
        "--enter-via-usb" => {
            let port = arguments
                .next()
                .ok_or("usage: --enter-via-usb <serial port>")?;
            enter_via_usb(&port).await
        }
        other => Err(
            format!("unknown mode {other:?}; use --off-via-bluetooth or --enter-via-usb").into(),
        ),
    }
}

/// Write Menu 650 off over Bluetooth, then wait for USB CAT to return.
#[cfg(target_os = "macos")]
async fn off_via_bluetooth(usb_port: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    use kenwood_thd75::transport::BluetoothTransport;

    println!("[1/4] Opening native Bluetooth (the port that still answers CAT)...");
    let transport = BluetoothTransport::open(None)?;
    let mut radio = Radio::connect_with_tnc_exit(transport).await?;

    let info = radio.identify().await?;
    let firmware = radio.get_firmware_version().await?;
    println!(
        "[2/4] Bluetooth CAT is alive: {} firmware {firmware}",
        info.model
    );

    println!("[3/4] Writing Menu 650 (DV Gateway) off via detached MCP update...");
    let update = radio
        .set_dv_gateway_mode_detached(DvGatewayMode::Off)
        .await?;
    println!("      detached update result: {update:?}");
    drop(radio.disconnect().await);

    let Some(usb_port) = usb_port else {
        println!("[4/4] No USB port given; skipping the CAT-return wait.");
        return Ok(());
    };
    println!("[4/4] Waiting for USB CAT identity on {usb_port} (radio rebooting)...");
    let deadline = tokio::time::Instant::now() + CAT_RETURN_WINDOW;
    loop {
        tokio::time::sleep(CAT_RETURN_POLL).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "USB CAT did not return within {CAT_RETURN_WINDOW:?}; \
                 check the radio's screen"
            )
            .into());
        }
        let Ok(transport) = SerialTransport::open(usb_port) else {
            println!("      port not ready yet...");
            continue;
        };
        let Ok(mut radio) = Radio::connect_with_tnc_exit(transport).await else {
            println!("      preamble not accepted yet...");
            continue;
        };
        match radio.identify().await {
            Ok(info) => {
                println!("PASS: USB CAT restored; radio identifies as {}", info.model);
                drop(radio.disconnect().await);
                return Ok(());
            }
            Err(error) => {
                println!("      CAT not answering yet ({error}); retrying...");
                drop(radio.disconnect().await);
            }
        }
    }
}

/// Stub so the example still compiles on platforms without `IOBluetooth`.
#[cfg(not(target_os = "macos"))]
fn off_via_bluetooth(
    _usb_port: Option<&str>,
) -> std::future::Ready<Result<(), Box<dyn std::error::Error>>> {
    std::future::ready(Err(
        "--off-via-bluetooth requires the native macOS Bluetooth transport".into(),
    ))
}

/// Enter Reflector Terminal Mode over USB and report the MMDVM proof.
async fn enter_via_usb(port: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("[1/3] Connecting over USB {port}...");
    let transport = SerialTransport::open(port)?;
    let mut radio = Radio::connect_with_tnc_exit(transport).await?;
    let info = radio.identify().await?;
    let firmware = radio.get_firmware_version().await?;
    println!("[2/3] CAT alive: {} firmware {firmware}", info.model);

    println!(
        "[3/3] Entering Reflector Terminal Mode (window {:?}, poll {:?})...",
        TerminalModeTransition::RECOMMENDED.window(),
        TerminalModeTransition::RECOMMENDED.poll_interval(),
    );
    match radio
        .enter_reflector_terminal_mode(TerminalModeTransition::RECOMMENDED)
        .await
    {
        Ok(radio) => {
            println!("PASS: the link is positively proved to speak MMDVM.");
            println!("      The radio's screen should show TERM.");
            drop(radio);
            Ok(())
        }
        Err((returned, error)) => {
            println!("FAIL: {error}");
            if let Some(radio) = returned {
                drop(radio.disconnect().await);
            }
            Err(error.into())
        }
    }
}
