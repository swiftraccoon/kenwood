//! Live hardware tests -- require a TH-D75 connected via USB.
//! Run with: cargo test --test hardware_live -- --ignored

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, RadioModel};

#[tokio::test]
#[ignore]
async fn live_identify() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(
        !ports.is_empty(),
        "No TH-D75 found -- connect radio via USB"
    );
    let transport =
        SerialTransport::open(&ports[0].port_name).unwrap();
    let mut radio = Radio::new(transport);
    let info = radio.identify().await.unwrap();
    assert_eq!(info.model, RadioModel::ThD75);
    println!("Radio identified: {}", info.model);
    radio.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_firmware_version() {
    let ports = SerialTransport::discover_usb().unwrap();
    let transport =
        SerialTransport::open(&ports[0].port_name).unwrap();
    let mut radio = Radio::new(transport);
    let version = radio.get_firmware_version().await.unwrap();
    println!("Firmware: {version}");
    radio.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore]
async fn live_read_frequency() {
    let ports = SerialTransport::discover_usb().unwrap();
    let transport =
        SerialTransport::open(&ports[0].port_name).unwrap();
    let mut radio = Radio::new(transport);
    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    println!("Band A: {} MHz", ch.receive_frequency.as_mhz());
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
        SerialTransport::open(&ports[0].port_name).unwrap();
    let mut radio = Radio::new(transport);

    let names = radio.read_channel_names().await.unwrap();

    println!("Read {} channel names", names.len());
    for (i, name) in names.iter().enumerate() {
        if !name.is_empty() {
            println!("  CH {i:03}: {name}");
        }
    }

    // The helper waited for the USB reset, reopened the transport, and proved
    // CAT identity before returning, so this controller remains usable.
    let _ = radio.disconnect().await;
}
