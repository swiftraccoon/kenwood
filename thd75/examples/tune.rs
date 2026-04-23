//! Tune the radio to a frequency or memory channel.
//!
//! Demonstrates the safe tuning API which automatically handles
//! VFO/Memory mode switching.
//!
//! Usage:
//! ```text
//! cargo run --example tune -- --band a --freq 145190000
//! cargo run --example tune -- --band b --channel 21
//! cargo run --example tune -- --band a --freq 446000000 /dev/cu.usbmodem5678
//! ```
//!
//! Pass a custom serial port as the last positional argument.

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, Frequency};

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
            eprintln!("Usage: tune --band <a|b> [--freq <hz> | --channel <num>] [port]");
            std::process::exit(1);
        }
    };

    // Parse --freq or --channel (one required).
    let freq_hz: Option<u64> = args
        .iter()
        .position(|a| a == "--freq")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());

    let channel_num: Option<u16> = args
        .iter()
        .position(|a| a == "--channel")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok());

    if freq_hz.is_none() && channel_num.is_none() {
        eprintln!("Specify --freq <hz> or --channel <num>");
        std::process::exit(1);
    }

    // Serial port is the last positional arg that starts with '/dev/' or 'COM'.
    let port = args
        .iter()
        .find(|a| a.starts_with("/dev/") || a.starts_with("COM"))
        .cloned()
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    let info = radio.identify().await?;
    println!("Connected to: {}", info.model);

    if let Some(hz) = freq_hz {
        // Frequency::new takes u32; truncate for safety.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "CLI example: `hz` is parsed from the user's argv as u64 for flexibility, \
                      but the D75 tunes below 1.3 GHz — well within u32. The cast cannot \
                      truncate for any on-band value."
        )]
        let freq = Frequency::new(hz as u32);
        println!("Tuning band {band} to {freq}...");
        radio.tune_frequency(band, freq).await?;
        println!("Done.");
    } else if let Some(ch) = channel_num {
        println!("Tuning band {band} to channel {ch}...");
        radio.tune_channel(band, ch).await?;
        println!("Done.");
    }

    // Read back to confirm.
    let readback = radio.get_frequency(band).await?;
    println!("Band {band} now on: {} Hz", readback.rx_frequency);

    radio.disconnect().await?;
    println!("Disconnected.");
    Ok(())
}
