//! Monitor S-meter and frequency in real-time.
//!
//! Polls the radio every 250ms for S-meter level, current frequency,
//! operating mode, and busy state on both Band A and Band B.
//!
//! Run: `cargo run --example monitor`
//!
//! Pass a custom serial port as the first argument:
//! `cargo run --example monitor -- /dev/cu.usbmodem1234`

use std::time::Duration;

use kenwood_thd75::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/cu.usbmodem1234".to_owned());

    println!("Connecting to {port}...");
    let transport = SerialTransport::open(&port, 115_200)?;
    let mut radio = Radio::connect(transport).await?;

    let info = radio.identify().await?;
    println!("Connected to: {}", info.model);
    println!("Press Ctrl+C to stop.\n");

    loop {
        for band in [Band::A, Band::B] {
            let freq = radio.get_frequency(band).await?;
            let smeter = radio.get_smeter(band).await?;
            let mode = radio.get_mode(band).await?;
            let busy = radio.get_busy(band).await?;

            println!(
                "Band {band}: {freq}  {mode}  S={smeter:02}  {busy}",
                freq = freq.rx_frequency,
                busy = if busy { "BUSY" } else { "    " },
            );
        }
        println!("---");

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
