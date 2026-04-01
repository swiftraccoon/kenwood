//! Fix channel 2 name and recall RutherfordtonPD.
//! Run: cargo test --test fix_and_recall -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::{self, programming, Command};
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::Band;

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
async fn fix_channel_2_name_and_recall_rutherfordton() {
    // Step 1: Restore channel 2 name to RCOEMSTAC2
    println!("\n=== Step 1: Restoring channel 002 name to RCOEMSTAC2 ===");
    let mut radio = connect().await;

    let page_data = radio
        .read_memory_pages(programming::CHANNEL_NAMES_START, 1)
        .await
        .unwrap();

    let mut page = [0u8; 256];
    page.copy_from_slice(&page_data[..256]);

    // Restore: write "RCOEMSTAC2" + nulls at offset 32 (ch 2)
    let mut name_slot = [0u8; 16];
    name_slot[..10].copy_from_slice(b"RCOEMSTAC2");
    page[32..48].copy_from_slice(&name_slot);

    drop(radio);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let mut radio2 = connect().await;
    radio2
        .write_memory_pages(programming::CHANNEL_NAMES_START, &page)
        .await
        .unwrap();
    println!("  Restored channel 002 name to RCOEMSTAC2");

    // Step 2: Find RutherfordtonPD channel number
    drop(radio2);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let mut radio3 = connect().await;
    let names = radio3.read_channel_names().await.unwrap();

    let mut target_ch: Option<u16> = None;
    for (i, name) in names.iter().enumerate() {
        if name.contains("Rutherfdtn") || name.contains("RutherfordtonPD") || name.contains("Rutherford") {
            println!("  Found: CH {:03} = {name}", i);
            if target_ch.is_none() {
                target_ch = Some(i as u16);
            }
        }
    }

    let ch = target_ch.expect("RutherfordtonPD not found in channel names");
    println!("\n=== Step 2: Recalling channel {:03} (RutherfordtonPD) on Band A ===", ch);

    // Step 3: Recall the channel using MR command
    // Need to reconnect since read_channel_names drops connection
    drop(radio3);
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let mut radio4 = connect().await;

    // First make sure we're in memory mode (VM 0,1 = band A, memory mode)
    let _ = radio4
        .execute(Command::SetVfoMemoryMode {
            band: Band::A,
            mode: 1,
        })
        .await;

    // Recall the channel
    let _ = radio4
        .execute(Command::RecallMemoryChannel {
            band: Band::A,
            channel: ch,
        })
        .await;

    println!("  Recalled channel {:03} on Band A", ch);

    // Verify by reading current frequency
    let freq = radio4.get_frequency(Band::A).await.unwrap();
    println!("  Band A now: {} MHz", freq.rx_frequency.as_mhz());

    let _ = radio4.disconnect().await;
    println!("\n=== DONE ===");
}
