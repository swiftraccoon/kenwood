//! Bluetooth SPP transport test: verifies CAT commands work over BT.
//!
//! Requires TH-D75 paired via Bluetooth (Menu 934).
//! Run: cargo test --test bluetooth_test -- --ignored --nocapture --test-threads=1

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, RadioModel};

const BT_PORT: &str = "/dev/cu.TH-D75";

#[tokio::test]
#[ignore]
async fn bt_identify() {
    println!("\n=== BLUETOOTH TRANSPORT TEST ===\n");

    println!("Opening BT SPP at {BT_PORT}...");
    let transport = SerialTransport::open_with_baud(BT_PORT, 9600).unwrap();

    let mut radio = Radio::new(transport);

    println!("Sending ID command over Bluetooth...");
    let info = radio.identify().await.unwrap();
    println!("  Radio identified: {}", info.model);
    assert_eq!(info.model, RadioModel::ThD75);

    println!("Sending FV command over Bluetooth...");
    let version = radio.get_firmware_version().await.unwrap();
    println!("  Firmware: {version}");

    println!("Reading frequency over Bluetooth...");
    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    println!("  Band A: {} MHz", ch.receive_frequency.as_mhz());

    println!("Reading S-meter over Bluetooth...");
    let sm = radio.get_smeter(Band::A).await.unwrap();
    println!("  S-meter: {sm}");

    println!("Reading squelch over Bluetooth...");
    let sq = radio.get_squelch(Band::A).await.unwrap();
    println!("  Squelch: {sq}");

    println!("Reading power level over Bluetooth...");
    let pc = radio.get_power_level(Band::A).await.unwrap();
    println!("  Power: {pc:?}");

    println!("Reading mode over Bluetooth...");
    let md = radio.get_operating_mode(Band::A).await.unwrap();
    println!("  Mode: {md:?}");

    println!("Reading BT status over Bluetooth (meta!)...");
    let bt = radio.get_bluetooth().await.unwrap();
    println!("  Bluetooth enabled: {bt}");

    let _ = radio.disconnect().await;
    println!("\n  ALL BLUETOOTH TESTS PASSED");
}
