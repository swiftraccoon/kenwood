//! Tune the radio by recalling memory channels.
//!
//! This archival probe source is not registered as a Cargo target. Before a
//! hardware run, review it against `docs/audit/probe_queue.md`, promote the
//! reviewed copy to an explicit test target, and run that target serially.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::new(SerialTransport::open(&ports[0].port_name).unwrap())
}

#[tokio::test]
#[ignore]
async fn tune_bands() -> Result<(), Box<dyn std::error::Error>> {
    // We already know from earlier probes:
    // - 145.190 MHz channels: 021, 069 (from deep probe results)
    // - RutherfdtnPD: channel 019 at 159.255 MHz

    let mut radio = connect();

    // Each tune qualifies the ME record before changing mode or recalling it.
    println!("\n=== Recalling memory channels ===");
    println!("  Recalling CH 021 (145.190 MHz) on Band A...");
    radio
        .tune_channel(Band::A, RegularChannel::new(21)?)
        .await?;

    // Band B -> channel 019 (RutherfdtnPD, 159.255 MHz)
    println!("  Recalling CH 019 (RutherfdtnPD) on Band B...");
    radio
        .tune_channel(Band::B, RegularChannel::new(19)?)
        .await?;

    // Verify
    let freq_a = radio.get_frequency(Band::A).await?;
    let freq_b = radio.get_frequency(Band::B).await?;
    println!("\n=== Result ===");
    println!("  Band A: {} MHz (CH 021)", freq_a.as_mhz());
    println!("  Band B: {} MHz (CH 019 RutherfdtnPD)", freq_b.as_mhz());

    radio.disconnect().await?;
    Ok(())
}
