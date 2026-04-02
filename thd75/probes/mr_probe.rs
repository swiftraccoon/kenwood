//! Probe MR command with correct band,channel format.
//! Run: cargo test --test mr_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw_cmd(transport: &mut SerialTransport, cmd: &str) -> Option<Vec<u8>> {
    let wire = format!("{cmd}\r");
    let _ = transport.write(wire.as_bytes()).await;
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
async fn probe_mr_formats() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== MR COMMAND FORMAT PROBING ===\n");

    // Try various MR formats for channel 018 (ForestCityPD)
    let formats = [
        "MR 0,018", // band 0, channel 018
        "MR 0,18",  // band 0, channel 18 (no leading zero)
        "MR 1,018", // band 1, channel 018
    ];

    for fmt in &formats {
        print!("  {fmt:<20} -> ");
        match raw_cmd(&mut transport, fmt).await {
            Some(frame) => {
                let text = String::from_utf8_lossy(&frame);
                let fields: Vec<&str> = text.split(',').collect();
                println!("({} fields) {text}", fields.len());
                if fields.len() > 2 {
                    for (i, f) in fields.iter().enumerate() {
                        println!("      [{i:2}] = {f}");
                    }
                }
            }
            None => println!("TIMEOUT"),
        }
    }

    // Scan channels 0-9 with MR 0,ccc to find named ones
    println!("\n=== MR CHANNEL SCAN (band 0, ch 0-19) ===\n");
    for ch in 0..20u16 {
        let cmd = format!("MR 0,{ch:03}");
        match raw_cmd(&mut transport, &cmd).await {
            Some(frame) => {
                let text = String::from_utf8_lossy(&frame);
                println!("  ch {ch:03}: {text}");
            }
            None => println!("  ch {ch:03}: TIMEOUT"),
        }
    }

    let _ = transport.close().await;
}
