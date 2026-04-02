//! Probe BL and DW exact wire formats.
//! Run: cargo test --test bl_dw_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw(transport: &mut SerialTransport, cmd: &[u8]) -> Option<String> {
    let _ = transport.write(cmd).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match transport.read(&mut buf).await {
                Ok(0) => return None,
                Ok(n) => {
                    codec.feed(&buf[..n]);
                    if let Some(frame) = codec.next_frame() {
                        return Some(String::from_utf8_lossy(&frame).to_string());
                    }
                }
                Err(_) => return None,
            }
        }
    })
    .await
    .unwrap_or(None)
}

#[tokio::test]
#[ignore]
async fn probe_bl_formats() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut t = SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== BL PROBE ===");
    // Read current value
    let r = raw(&mut t, b"BL\r").await;
    println!("  BL (read):     {:?}", r);

    // Try various write formats
    for cmd in [
        b"BL 5\r".as_slice(),   // 5 bytes - simple value
        b"BL 0,5\r".as_slice(), // 7 bytes - band,value
        b"BL 1,5\r".as_slice(), // 7 bytes - different first param
        b"BL 05\r".as_slice(),  // 6 bytes - zero-padded
    ] {
        let cmd_str = String::from_utf8_lossy(&cmd[..cmd.len() - 1]);
        let r = raw(&mut t, cmd).await;
        println!("  {:<15} {:?}", cmd_str, r);
    }

    let _ = t.close().await;
}

#[tokio::test]
#[ignore]
async fn probe_dw_formats() {
    let ports = SerialTransport::discover_usb().unwrap();
    let mut t = SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== DW PROBE ===");
    // Read current value
    let r = raw(&mut t, b"DW\r").await;
    println!("  DW (bare 3B):  {:?}", r);

    // DW handler: param_2==7 reads channel data
    // Try DW 0000 (7 bytes: DW + space + 4 digits)
    let r = raw(&mut t, b"DW 0000\r").await;
    println!("  DW 0000 (7B):  {:?}", r);

    let r = raw(&mut t, b"DW 0001\r").await;
    println!("  DW 0001 (7B):  {:?}", r);

    // DW handler: param_2==8, comma at [6]
    let r = raw(&mut t, b"DW 000,0\r").await;
    println!("  DW 000,0 (8B): {:?}", r);

    // Simple on/off (5 bytes) - should be rejected
    let r = raw(&mut t, b"DW 1\r").await;
    println!("  DW 1 (5B):     {:?}", r);

    let r = raw(&mut t, b"DW 0\r").await;
    println!("  DW 0 (5B):     {:?}", r);

    let _ = t.close().await;
}
