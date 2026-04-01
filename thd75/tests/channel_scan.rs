//! Scan memory channels to check for programmed data.
//! Run: cargo test --test channel_scan -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

#[tokio::test]
#[ignore]
async fn scan_channels_0_to_19() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== MEMORY CHANNEL SCAN (0-19) ===");
    println!(
        "{:<5} {:<15} {:<12} {:}",
        "CH", "FREQUENCY", "OFFSET", "NAME"
    );
    println!("{}", "-".repeat(50));

    for ch in 0..20u16 {
        let cmd = format!("ME {:03}\r", ch);
        let _ = transport.write(cmd.as_bytes()).await;

        let mut codec = Codec::new();
        let mut buf = [0u8; 4096];

        let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let n = transport.read(&mut buf).await.unwrap();
                codec.feed(&buf[..n]);
                if let Some(frame) = codec.next_frame() {
                    return frame;
                }
            }
        })
        .await;

        match result {
            Ok(frame) => {
                let text = String::from_utf8_lossy(&frame);
                let fields: Vec<&str> = text.split(',').collect();
                if fields.len() >= 20 {
                    let freq = fields[1];
                    let offset = fields[2];
                    let name = if fields.len() > 19 { fields[19] } else { "" };
                    println!("{:<5} {:<15} {:<12} {:}", ch, freq, offset, name);
                } else {
                    println!("{:<5} (response: {} fields)", ch, fields.len());
                }
            }
            Err(_) => println!("{:<5} TIMEOUT", ch),
        }
    }

    let _ = transport.close().await;
}
