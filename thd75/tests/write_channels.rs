//! Write channel data to the radio.
//! Run: cargo test --test write_channels -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::connect(
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap(),
    )
    .await
    .unwrap()
}

async fn reconnect() -> Radio<SerialTransport> {
    // USB needs time to re-enumerate after MCP exit
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    connect().await
}

#[tokio::test]
#[ignore]
async fn set_channel_1_freq() {
    let mut radio = connect().await;

    println!("\n=== Setting channel 001 frequency to 145.190 MHz ===");

    let mut ch1 = radio.read_channel(1).await.unwrap();
    println!("  Before: {} Hz", ch1.rx_frequency.as_hz());

    ch1.rx_frequency = Frequency::new(145_190_000);
    radio.write_channel(1, &ch1).await.unwrap();

    let ch1_verify = radio.read_channel(1).await.unwrap();
    println!("  After:  {} Hz", ch1_verify.rx_frequency.as_hz());
    assert_eq!(ch1_verify.rx_frequency.as_hz(), 145_190_000);
    println!("  PASS");

    let _ = radio.disconnect().await;
}

#[tokio::test]
#[ignore]
async fn set_channel_2_name() {
    // Step 1: Read name page, modify, write back in ONE MCP session
    let mut radio = connect().await;
    println!("\n=== Setting channel 002 name to RutherfordtonPD ===");

    // Use read_page + write_page which each do their own MCP session.
    // We need to do read + modify + write in one session.
    // The current API enters/exits per call. We need to read the page
    // first, then reconnect and write it.

    // Read the name page
    let page_data = radio
        .read_memory_pages(programming::CHANNEL_NAMES_START, 1)
        .await
        .unwrap();

    println!("  Read name page, ch002 before:");
    let old_name_bytes = &page_data[32..48]; // channel 2 at offset 2*16
    let old_end = old_name_bytes
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(16);
    println!("    {:?}", String::from_utf8_lossy(&old_name_bytes[..old_end]));

    // Modify channel 2's name
    let mut page = [0u8; 256];
    page.copy_from_slice(&page_data[..256]);
    let name_bytes = b"RutherfordtonPD\0";
    page[32..48].copy_from_slice(name_bytes);

    // Reconnect (USB drops after MCP exit)
    drop(radio);
    let mut radio2 = reconnect().await;

    // Write the modified page
    radio2
        .write_memory_pages(programming::CHANNEL_NAMES_START, &page)
        .await
        .unwrap();
    println!("  Name page written");

    // Reconnect again to verify
    drop(radio2);
    let mut radio3 = reconnect().await;

    let names = radio3.read_channel_names().await.unwrap();
    let ch2_name = names.get(2).map(|s| s.as_str()).unwrap_or("");
    println!("  After: ch002 = {ch2_name:?}");
    assert_eq!(ch2_name, "RutherfordtonPD");
    println!("  PASS");
}
