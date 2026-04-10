//! Probe MMDVM gateway mode over Bluetooth.
//!
//! Assumes the radio is ALREADY in Reflector Terminal Mode (TERM shown
//! on display). Connects via native BT, sends MMDVM GET_VERSION, and
//! reports the response.
//!
//! Run: cargo run --bin mmdvm_gateway_probe
//!
//! This is a standalone binary, not a test, because IOBluetooth
//! requires the main thread.

#![allow(clippy::too_many_lines, clippy::doc_markdown, missing_docs)]

use kenwood_thd75::transport::{EitherTransport, Transport};
use std::time::Duration;

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(run());
}

async fn run() {
    println!("=== MMDVM Gateway Probe ===");
    println!("Radio MUST be in Reflector Terminal Mode (TERM on display).");
    println!();

    // Connect via native BT.
    println!("Connecting via Bluetooth...");
    let transport: EitherTransport = match kenwood_thd75::BluetoothTransport::open(None) {
        Ok(bt) => {
            println!("Bluetooth connected.");
            EitherTransport::Bluetooth(bt)
        }
        Err(e) => {
            println!("BT connect failed: {e}");
            println!("Retrying in 3 seconds...");
            tokio::time::sleep(Duration::from_secs(3)).await;
            match kenwood_thd75::BluetoothTransport::open(None) {
                Ok(bt) => {
                    println!("Bluetooth connected on retry.");
                    EitherTransport::Bluetooth(bt)
                }
                Err(e2) => {
                    println!("BT connect failed again: {e2}");
                    return;
                }
            }
        }
    };

    // First: try reading raw bytes to see what the radio sends on connect.
    println!();
    println!("--- Phase 1: Read any unsolicited data (2 seconds) ---");
    let mut transport = transport;
    let mut buf = [0u8; 512];
    match tokio::time::timeout(Duration::from_secs(2), transport.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            println!("Received {n} bytes on connect:");
            print_hex(&buf[..n]);
        }
        Ok(Ok(_)) => println!("No data received (0 bytes)."),
        Ok(Err(e)) => println!("Read error: {e}"),
        Err(_) => println!("No unsolicited data within 2 seconds."),
    }

    // Phase 2: Send MMDVM GET_VERSION (E0 03 00).
    println!();
    println!("--- Phase 2: Send MMDVM GET_VERSION (E0 03 00) ---");
    let get_version = [0xE0, 0x03, 0x00];
    if let Err(e) = transport.write(&get_version).await {
        println!("Write failed: {e}");
        return;
    }
    println!("Sent 3 bytes. Waiting for response...");

    match tokio::time::timeout(Duration::from_secs(5), transport.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            println!("Response: {n} bytes:");
            print_hex(&buf[..n]);
            parse_mmdvm_response(&buf[..n]);
        }
        Ok(Ok(_)) => println!("No response (0 bytes)."),
        Ok(Err(e)) => println!("Read error: {e}"),
        Err(_) => println!("No response within 5 seconds (timeout)."),
    }

    // Phase 3: Try sending CAT ID command to see if radio is in CAT mode.
    println!();
    println!("--- Phase 3: Send CAT 'ID\\r' to check if still in CAT mode ---");
    if let Err(e) = transport.write(b"ID\r").await {
        println!("Write failed: {e}");
        return;
    }
    println!("Sent 'ID\\r'. Waiting for response...");

    match tokio::time::timeout(Duration::from_secs(3), transport.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            println!("Response: {n} bytes:");
            print_hex(&buf[..n]);
            let text = String::from_utf8_lossy(&buf[..n]);
            if text.contains("ID") {
                println!("=> Radio is in CAT mode (not MMDVM).");
            } else {
                println!("=> Got binary data, radio may be in MMDVM mode.");
            }
        }
        Ok(Ok(_)) => println!("No response (0 bytes)."),
        Ok(Err(e)) => println!("Read error: {e}"),
        Err(_) => println!("No response within 3 seconds."),
    }

    // Phase 4: Try MMDVM GET_STATUS.
    println!();
    println!("--- Phase 4: Send MMDVM GET_STATUS (E0 03 01) ---");
    let get_status = [0xE0, 0x03, 0x01];
    if let Err(e) = transport.write(&get_status).await {
        println!("Write failed: {e}");
        return;
    }
    println!("Sent 3 bytes. Waiting for response...");

    match tokio::time::timeout(Duration::from_secs(5), transport.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => {
            println!("Response: {n} bytes:");
            print_hex(&buf[..n]);
            parse_mmdvm_response(&buf[..n]);
        }
        Ok(Ok(_)) => println!("No response (0 bytes)."),
        Ok(Err(e)) => println!("Read error: {e}"),
        Err(_) => println!("No response within 5 seconds (timeout)."),
    }

    println!();
    println!("=== Probe complete ===");
    let _ = transport.close().await;
}

fn print_hex(data: &[u8]) {
    for chunk in data.chunks(16) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02X}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (32..127).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("  {} | {}", hex.join(" "), ascii);
    }
}

fn parse_mmdvm_response(data: &[u8]) {
    if data.is_empty() {
        return;
    }
    if data[0] == 0xE0 && data.len() >= 3 {
        let len = data[1] as usize;
        let cmd = data[2];
        println!("  MMDVM frame: start=0xE0 len={len} cmd=0x{cmd:02X}");
        match cmd {
            0x00 => {
                if data.len() >= 4 {
                    let protocol = data[3];
                    let desc = String::from_utf8_lossy(&data[4..len.min(data.len())]);
                    println!("  => GET_VERSION response: protocol v{protocol}, desc: {desc}");
                }
            }
            0x01 => {
                if data.len() >= 7 {
                    println!(
                        "  => GET_STATUS: modes=0x{:02X} state=0x{:02X} tx={} dstar_buf={}",
                        data[3],
                        data[4],
                        data[5] & 1,
                        data[6]
                    );
                }
            }
            0x70 => println!(
                "  => ACK for command 0x{:02X}",
                if data.len() > 3 { data[3] } else { 0 }
            ),
            0x7F => println!(
                "  => NAK for command 0x{:02X}",
                if data.len() > 3 { data[3] } else { 0 }
            ),
            _ => println!("  => Unknown command 0x{cmd:02X}"),
        }
    } else {
        println!("  Not an MMDVM frame (first byte: 0x{:02X})", data[0]);
    }
}
