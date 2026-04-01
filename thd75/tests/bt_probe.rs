//! Probe Bluetooth CAT commands to find pairing mode control.
//! SAFE: Only sends reads and known BT parameter queries.
//!
//! Run: cargo test --test bt_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};

async fn raw_cmd(transport: &mut SerialTransport, cmd: &str) -> Option<String> {
    let wire = format!("{cmd}\r");
    if transport.write(wire.as_bytes()).await.is_err() {
        return None;
    }
    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
    .await;
    match result {
        Ok(Some(s)) => Some(s),
        _ => None,
    }
}

#[tokio::test]
#[ignore]
async fn probe_bluetooth_commands() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(!ports.is_empty(), "No TH-D75 found");
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    println!("\n=== BLUETOOTH COMMAND PROBE ===\n");

    // Current BT state
    println!("Current state:");
    let resp = raw_cmd(&mut transport, "BT").await;
    println!("  BT (bare): {:?}", resp);

    // Try BT with sub-indices (read only — single digit should be read)
    println!("\nBT sub-indices (reads):");
    for i in 0..=5u8 {
        let cmd = format!("BT {i}");
        let resp = raw_cmd(&mut transport, &cmd).await;
        match &resp {
            Some(r) if r == "?" || r == "N" => println!("  {cmd}: {r}"),
            Some(r) => println!("  {cmd}: {r}"),
            None => {
                println!("  {cmd}: TIMEOUT");
                break;
            }
        }
    }

    // Try BT with 2-digit codes (some Kenwood radios use BT 00, BT 01, etc.)
    println!("\nBT 2-digit codes:");
    for i in [0, 1, 2, 10, 11, 12, 20, 21] {
        let cmd = format!("BT {i:02}");
        let resp = raw_cmd(&mut transport, &cmd).await;
        match &resp {
            Some(r) if r == "?" || r == "N" => println!("  {cmd}: {r}"),
            Some(r) => println!("  {cmd}: {r}"),
            None => {
                println!("  {cmd}: TIMEOUT");
                break;
            }
        }
    }

    // Check the RE data — the BT handler at 0xC002ED7C might reveal pairing mode
    // The D74 docs show BT as on/off only. Pairing mode might be a menu-only function.
    // Let's also check if there's a separate pairing command

    // Try BP (Bluetooth Pairing?)
    println!("\nOther possible BT commands:");
    for cmd in ["BP", "BP 0", "BP 1", "BM", "BM 0", "BD", "BD 0"] {
        let resp = raw_cmd(&mut transport, cmd).await;
        match &resp {
            Some(r) if r == "?" => {} // unknown, skip
            Some(r) => println!("  {cmd}: {r}"),
            None => println!("  {cmd}: TIMEOUT"),
        }
    }

    let _ = transport.close().await;
    println!("\n=== PROBE COMPLETE ===");
}
