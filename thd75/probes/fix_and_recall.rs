//! Fix channel 2 name and recall RutherfordtonPD.
//!
//! This archival probe source is not registered as a Cargo target. Before a
//! hardware run, review it against `docs/audit/probe_queue.md`, promote the
//! reviewed copy to an explicit test target, and run that target serially.

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::{Band, RegularChannel};

fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::new(SerialTransport::open(&ports[0].port_name).unwrap())
}

#[tokio::test]
#[ignore]
async fn fix_channel_2_name_and_recall_rutherfordton() {
    // Step 1: Restore channel 2 name to RCOEMSTAC2
    println!("\n=== Step 1: Restoring channel 002 name to RCOEMSTAC2 ===");
    let mut radio = connect();

    let page = programming::McpPage::new(programming::CHANNEL_NAMES_START)
        .expect("channel-name page is inside the MCP image");
    let writable_page =
        programming::WritableMcpPage::from_page(page).expect("channel-name page is writable");
    let mut session = radio.enter_mcp().await.unwrap();
    let mut page_data = session.read_page(page).await.unwrap();

    // Restore: write "RCOEMSTAC2" + nulls at offset 32 (ch 2)
    let mut name_slot = [0u8; 16];
    name_slot[..10].copy_from_slice(b"RCOEMSTAC2");
    page_data[32..48].copy_from_slice(&name_slot);

    session.write_page(writable_page, &page_data).await.unwrap();
    session.exit().await.unwrap();
    println!("  Restored channel 002 name to RCOEMSTAC2");

    // Step 2: Find RutherfordtonPD channel number
    let names = radio.read_channel_names().await.unwrap();

    let mut target_ch: Option<u16> = None;
    for (i, name) in names.iter().enumerate() {
        if name.as_str().contains("Rutherfdtn")
            || name.as_str().contains("RutherfordtonPD")
            || name.as_str().contains("Rutherford")
        {
            println!("  Found: CH {:03} = {name}", i);
            if target_ch.is_none() {
                target_ch = Some(i as u16);
            }
        }
    }

    let ch = target_ch.expect("RutherfordtonPD not found in channel names");
    println!(
        "\n=== Step 2: Recalling channel {:03} (RutherfordtonPD) on Band A ===",
        ch
    );

    // Qualify the ME record, enter memory mode, and recall the exact channel.
    let channel = RegularChannel::new(ch).unwrap();
    radio.tune_channel(Band::A, channel).await.unwrap();

    println!("  Recalled channel {:03} on Band A", ch);

    // Verify by reading current frequency
    let freq = radio.get_frequency(Band::A).await.unwrap();
    println!("  Band A now: {} MHz", freq.as_mhz());

    let _ = radio.disconnect().await;
    println!("\n=== DONE ===");
}
