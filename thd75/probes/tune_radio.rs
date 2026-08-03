//! Tune the radio by recalling memory channels.
//! Run: cargo test --test tune_radio -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::{self, Codec, Command};
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::{SerialTransport, Transport};
use kenwood_thd75::types::*;

async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::connect(
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore]
async fn tune_bands() {
    // We already know from earlier probes:
    // - 145.190 MHz channels: 021, 069 (from deep probe results)
    // - RutherfdtnPD: channel 019 at 159.255 MHz

    let mut radio = connect().await;

    // Ensure memory mode on both bands
    println!("\n=== Setting memory mode ===");
    let _ = radio
        .execute(Command::SetVfoMemoryMode {
            band: Band::A,
            mode: 1,
        })
        .await;
    let _ = radio
        .execute(Command::SetVfoMemoryMode {
            band: Band::B,
            mode: 1,
        })
        .await;

    // Band A -> channel 021 (145.190 MHz)
    println!("  Recalling CH 021 (145.190 MHz) on Band A...");
    let _ = radio
        .execute(Command::RecallMemoryChannel {
            band: Band::A,
            selector: MemorySelector::try_from(21_u16).unwrap(),
        })
        .await;

    // Band B -> channel 019 (RutherfdtnPD, 159.255 MHz)
    println!("  Recalling CH 019 (RutherfdtnPD) on Band B...");
    let _ = radio
        .execute(Command::RecallMemoryChannel {
            band: Band::B,
            selector: MemorySelector::try_from(19_u16).unwrap(),
        })
        .await;

    // Verify
    let freq_a = radio.get_frequency(Band::A).await.unwrap();
    let freq_b = radio.get_frequency(Band::B).await.unwrap();
    println!("\n=== Result ===");
    println!("  Band A: {} MHz (CH 021)", freq_a.as_mhz());
    println!("  Band B: {} MHz (CH 019 RutherfdtnPD)", freq_b.as_mhz());

    let _ = radio.disconnect().await;
}
