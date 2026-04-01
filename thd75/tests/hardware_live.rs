//! Live hardware tests -- require a TH-D75 connected via USB.
//! Run with: cargo test --test hardware_live -- --ignored

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;

#[tokio::test]
#[ignore]
async fn live_identify() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(
        !ports.is_empty(),
        "No TH-D75 found -- connect radio via USB"
    );
    let transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();
    let mut radio = Radio::connect(transport).await.unwrap();
    let info = radio.identify().await.unwrap();
    assert!(info.model.contains("TH-D75"));
    println!("Radio identified: {}", info.model);
    radio.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_firmware_version() {
    let ports = SerialTransport::discover_usb().unwrap();
    let transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();
    let mut radio = Radio::connect(transport).await.unwrap();
    let version = radio.get_firmware_version().await.unwrap();
    assert!(!version.is_empty());
    println!("Firmware: {version}");
    radio.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_read_frequency() {
    let ports = SerialTransport::discover_usb().unwrap();
    let transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();
    let mut radio = Radio::connect(transport).await.unwrap();
    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    println!("Band A: {} MHz", ch.rx_frequency.as_mhz());
    radio.disconnect().await.unwrap();
}

/// Read channel display names via the `0M PROGRAM` binary protocol.
///
/// WARNING: This briefly puts the radio into programming mode.
/// The display will show "PROG MCP" during the operation.
/// Normal CAT commands are unavailable until the operation completes.
#[tokio::test]
#[ignore = "requires TH-D75 connected via USB"]
async fn live_read_channel_names() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(
        !ports.is_empty(),
        "No TH-D75 found -- connect radio via USB"
    );
    let transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();
    let mut radio = Radio::connect(transport).await.unwrap();

    let names = radio.read_channel_names().await.unwrap();

    println!("Read {} channel names", names.len());
    for (i, name) in names.iter().enumerate() {
        if !name.is_empty() {
            println!("  CH {i:03}: {name}");
        }
    }

    // Note: The USB connection does not survive the programming mode
    // transition. The radio's USB stack resets when exiting MCP mode.
    // A fresh connection is needed for subsequent CAT commands.
    let _ = radio.disconnect().await;
}
