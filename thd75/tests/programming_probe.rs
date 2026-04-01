//! Raw probe of the 0M PROGRAM binary protocol.
//! Captures exact bytes for debugging.
//!
//! WARNING: This enters programming mode! Radio display will show "PROG MCP".
//! The test exits programming mode when done.
//!
//! Run: cargo test --test programming_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::transport::{SerialTransport, Transport};

async fn read_all_available(transport: &mut SerialTransport, timeout_ms: u64) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, transport.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                result.extend_from_slice(&buf[..n]);
                // Give a tiny window for more data
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Ok(Err(_)) => break,
            Err(_) => break, // timeout
        }
    }
    result
}

fn hex_dump(data: &[u8], label: &str) {
    println!("  {label} ({} bytes):", data.len());
    for (i, chunk) in data.chunks(16).enumerate() {
        let hex: String = chunk.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ");
        let ascii: String = chunk.iter().map(|&b| {
            if b.is_ascii_graphic() || b == b' ' { b as char } else { '.' }
        }).collect();
        println!("    {:04X}: {:<48} {}", i * 16, hex, ascii);
    }
}

#[tokio::test]
#[ignore]
async fn probe_programming_mode() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(!ports.is_empty(), "No TH-D75 found");
    let mut transport = SerialTransport::open(
        &ports[0].port_name,
        SerialTransport::DEFAULT_BAUD,
    ).unwrap();

    println!("\n=== 0M PROGRAM BINARY PROTOCOL PROBE ===\n");

    // Step 1: Enter programming mode
    println!("Step 1: Sending '0M PROGRAM\\r'...");
    let _ = transport.write(b"0M PROGRAM\r").await;

    // Wait and read response
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let resp = read_all_available(&mut transport, 3000).await;
    hex_dump(&resp, "Entry response");

    if resp.is_empty() {
        println!("  NO RESPONSE — radio may not support 0M PROGRAM");
        // Try to exit anyway
        let _ = transport.write(b"E").await;
        let _ = transport.close().await;
        return;
    }

    // Step 2: Try reading page 256 (first name page)
    // Wait a bit longer after entering programming mode
    println!("\nStep 1b: Waiting 1s for radio to settle in programming mode...");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Drain any pending data from mode switch
    let drain = read_all_available(&mut transport, 500).await;
    if !drain.is_empty() {
        hex_dump(&drain, "Drained after mode entry");
    }

    println!("\nStep 2: Sending R command for page 256 (0x0100)...");
    let read_cmd: [u8; 5] = [b'R', 0x01, 0x00, 0x00, 0x00];
    hex_dump(&read_cmd, "Read command sent");
    let _ = transport.write(&read_cmd).await;

    // Use longer timeout — radio may take a while to respond
    // Poll in a loop
    println!("  Waiting for response (up to 10s, polling every 100ms)...");
    let resp = read_all_available(&mut transport, 10000).await;
    hex_dump(&resp, "Read response");

    if !resp.is_empty() {
        // W response is 261 bytes: W + 4-byte address + 256-byte data.
        if resp[0] == b'W' && resp.len() >= 261 {
            let page = u16::from_be_bytes([resp[1], resp[2]]);
            println!("\n  W response: page={page:04X}, total_bytes={}",
                resp.len());

            // Data starts at byte 5.
            println!("\n  First 4 name entries:");
            for i in 0..4.min((resp.len() - 5) / 16) {
                let start = 5 + i * 16;
                let entry = &resp[start..start + 16];
                let name_end = entry.iter().position(|&b| b == 0).unwrap_or(16);
                let name = String::from_utf8_lossy(&entry[..name_end]);
                println!("    CH {i:03}: {name:?} (raw: {entry:02X?})");
            }
        } else if resp[0] == b'W' {
            println!("\n  W response incomplete: {} bytes (expected 261)", resp.len());
        }

        // Send ACK
        println!("\nStep 3: Sending ACK (0x06)...");
        let _ = transport.write(&[0x06]).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let resp = read_all_available(&mut transport, 1000).await;
        if !resp.is_empty() {
            hex_dump(&resp, "ACK response");
        }

        // Try reading page 257 (second name page)
        println!("\nStep 4: Reading page 257 (0x0101)...");
        let read_cmd2: [u8; 5] = [b'R', 0x01, 0x01, 0x00, 0x00];
        let _ = transport.write(&read_cmd2).await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let resp = read_all_available(&mut transport, 3000).await;
        hex_dump(&resp, "Page 257 response");

        if !resp.is_empty() {
            let _ = transport.write(&[0x06]).await;
        }
    }

    // Exit programming mode
    println!("\nStep 5: Exiting programming mode (sending 'E')...");
    let _ = transport.write(&[b'E']).await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let resp = read_all_available(&mut transport, 2000).await;
    if !resp.is_empty() {
        hex_dump(&resp, "Exit response");
    }

    // Verify CAT works again
    println!("\nStep 6: Verifying CAT works (sending 'ID\\r')...");
    let _ = transport.write(b"ID\r").await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let resp = read_all_available(&mut transport, 2000).await;
    hex_dump(&resp, "ID response after exit");

    let _ = transport.close().await;
    println!("\n=== PROBE COMPLETE ===");
}
