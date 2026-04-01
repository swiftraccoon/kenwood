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

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;

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
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    let info = radio.identify().await?;
    println!("Connected to: {}\n", info.model);

    // Read channels via CAT (ME command).
    println!("Reading channels via CAT...\n");
    let mut populated = Vec::new();

    for ch in 0..1000_u16 {
        match radio.read_channel(ch).await {
            Ok(data) if data.rx_frequency.as_hz() > 0 => {
                println!(
                    "CH {:03}: {} {} shift={} step={:?}",
                    ch,
                    data.rx_frequency,
                    data.urcall.as_str(),
                    data.shift.as_u8(),
                    data.step_size,
                );
                populated.push(ch);
            }
            Ok(_) => {} // empty channel
            Err(e) => {
                eprintln!("CH {ch:03}: error: {e}");
            }
        }
    }

    println!("\n{} channels programmed.", populated.len());

    // Optionally read display names via MCP programming mode.
    if read_names {
        println!("\nEntering programming mode to read channel names...");
        println!("(USB connection will reset after this operation)\n");

        let names = radio.read_channel_names().await?;
        for (i, name) in names.iter().enumerate() {
            if !name.is_empty() {
                println!("CH {i:03}: {name}");
            }
        }
        println!("\nDone. USB connection has been reset by the radio.");
    } else {
        radio.disconnect().await?;
        println!("Disconnected.");
    }

    Ok(())
}
