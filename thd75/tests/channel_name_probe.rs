//! Probe channel 018 (155.190 MHz = "ForestCityPD") to find where the name lives.
//! Run: cargo test --test channel_name_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw_cmd(transport: &mut SerialTransport, cmd: &[u8]) -> Option<Vec<u8>> {
    let _ = transport.write(cmd).await;
    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let n = transport.read(&mut buf).await.unwrap();
            codec.feed(&buf[..n]);
            if let Some(frame) = codec.next_frame() {
                return frame;
            }
        }
    })
    .await
    .ok()
}

#[tokio::test]
#[ignore]
async fn find_channel_name_field() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    // Channel 18 should be 155.190 MHz "ForestCityPD"
    println!("\n=== ME 018 (should be 155.190 ForestCityPD) ===");
    if let Some(frame) = raw_cmd(&mut transport, b"ME 018\r").await {
        let text = String::from_utf8_lossy(&frame);
        let fields: Vec<&str> = text.split(',').collect();
        println!("Total fields: {}", fields.len());
        for (i, f) in fields.iter().enumerate() {
            let hex: String = f
                .as_bytes()
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<Vec<_>>()
                .join(" ");
            println!("  [{i:2}] = {f:<20} (hex: {hex})");
        }
    }

    // Also try MR on the same channel
    println!("\n=== MR 018 ===");
    if let Some(frame) = raw_cmd(&mut transport, b"MR 018\r").await {
        let text = String::from_utf8_lossy(&frame);
        println!("  RAW: {text}");
    } else {
        println!("  No response or error");
    }

    // Try MR with band prefix
    println!("\n=== MR 0,018 ===");
    if let Some(frame) = raw_cmd(&mut transport, b"MR 0,018\r").await {
        let text = String::from_utf8_lossy(&frame);
        println!("  RAW: {text}");
    } else {
        println!("  No response or error");
    }

    // Also try FO on the current channel to compare
    println!("\n=== FO 0 (for comparison) ===");
    if let Some(frame) = raw_cmd(&mut transport, b"FO 0\r").await {
        let text = String::from_utf8_lossy(&frame);
        let fields: Vec<&str> = text.split(',').collect();
        println!("Total fields: {}", fields.len());
        for (i, f) in fields.iter().enumerate() {
            println!("  [{i:2}] = {f}");
        }
    }

    let _ = transport.close().await;
}
