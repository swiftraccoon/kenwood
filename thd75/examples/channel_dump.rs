//! Dump all programmed channels with names.
//!
//! Reads memory channels 0-999 via CAT protocol and prints any that have
//! a non-zero frequency. Optionally enters MCP programming mode to read
//! the user-assigned display names (requires USB, not Bluetooth).
//!
//! Run: `cargo run --example channel_dump`
//!
//! Pass `--names` to also read channel display names via MCP:
//! `cargo run --example channel_dump -- --names /dev/cu.usbmodem1234`
//!
//! **Note:** Reading names enters programming mode. The USB connection
//! resets afterward, so this should be the last operation.

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
use kenwood_thd75::types::RegularChannel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let read_names = args.iter().any(|a| a == "--names");
    let port = args
        .iter()
        .find(|a| a.starts_with("/dev/"))
        .cloned()
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port)?;
    let mut radio = Radio::new(transport);

    let info = radio.identify().await?;
    println!("Connected to: {}\n", info.model);

    // Read channels via CAT (ME command).
    println!("Reading channels via CAT...\n");
    let mut populated = Vec::new();

    for channel in RegularChannel::all() {
        match radio.get_regular_channel_record(channel).await {
            Ok(data) if data.channel.receive_frequency.as_hz() > 0 => {
                println!(
                    "CH {:03}: {} {} shift={} step={:?} transmit={} lockout={}",
                    channel,
                    data.channel.receive_frequency,
                    data.channel.ur_call.as_str(),
                    u8::from(data.channel.shift),
                    data.channel.receive_step,
                    data.transmit_value(),
                    data.scan_lockout,
                );
                populated.push(channel);
            }
            Ok(_) => {} // empty channel
            Err(e) => {
                eprintln!("CH {channel:03}: error: {e}");
            }
        }
    }

    println!("\n{} channels programmed.", populated.len());

    // Optionally read display names via MCP programming mode.
    if read_names {
        println!("\nEntering programming mode to read channel names...");
        println!("(USB will reset and the library will restore CAT before returning)\n");

        let names = radio.read_channel_names().await?;
        for (channel, name) in RegularChannel::all().zip(&names) {
            if !name.is_empty() {
                println!("CH {channel:03}: {name}");
            }
        }
        println!("\nDone. CAT was restored after the radio reset USB.");
    } else {
        radio.disconnect().await?;
        println!("Disconnected.");
    }

    Ok(())
}
