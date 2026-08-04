//! Write channel-name data through the MCP memory-page interface.
//! Run: cargo test --test write_channels -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;

fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::new(SerialTransport::open(&ports[0].port_name).unwrap())
}

#[tokio::test]
#[ignore]
async fn set_channel_2_name() {
    let mut radio = connect();
    println!("\n=== Setting channel 002 name to RutherfordtonPD ===");

    let page = programming::McpPage::new(programming::CHANNEL_NAMES_START)
        .expect("channel-name page is inside the MCP image");
    let writable_page =
        programming::WritableMcpPage::from_page(page).expect("channel-name page is writable");
    let mut session = radio.enter_mcp().await.unwrap();
    let mut page_data = session.read_page(page).await.unwrap();

    println!("  Read name page, ch002 before:");
    let old_name_bytes = &page_data[32..48]; // channel 2 at offset 2*16
    let old_end = old_name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
    println!(
        "    {:?}",
        String::from_utf8_lossy(&old_name_bytes[..old_end])
    );

    // Modify channel 2's name
    let name_bytes = b"RutherfordtonPD\0";
    page_data[32..48].copy_from_slice(name_bytes);

    session.write_page(writable_page, &page_data).await.unwrap();
    session.exit().await.unwrap();
    println!("  Name page written");

    let names = radio.read_channel_names().await.unwrap();
    let ch2_name = names.get(2).map(|s| s.as_str()).unwrap_or("");
    println!("  After: ch002 = {ch2_name:?}");
    assert_eq!(ch2_name, "RutherfordtonPD");
    println!("  PASS");
}
