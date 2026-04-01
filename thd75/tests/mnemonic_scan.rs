//! Brute-force scan of all 1,296 possible 2-char CAT mnemonics.
//! Finds undocumented commands by checking which ones don't return `?`.
//!
//! SAFE: Unrecognized mnemonics return `?`. Known dangerous commands
//! (SR, 0M, TX, RX, UP, DW, BE) are EXCLUDED from the scan.
//!
//! Run: cargo test --test mnemonic_scan -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};
use std::io::Write as IoWrite;

// Commands that are known writes/actions — SKIP these even if found
const DANGEROUS: &[&str] = &[
    "SR", // Reset
    "TX", // Transmit
    "RX", // Force receive
    "UP", // Freq up
    "DW", // Freq down (down button)
    "BE", // Beacon send (transmits!)
    "0M", // Programming mode
    "BC", // With bare send could change band — actually BC bare is a read. Keep it.
];

async fn raw_cmd(transport: &mut SerialTransport, cmd: &str) -> Option<String> {
    let wire = format!("{cmd}\r");
    if transport.write(wire.as_bytes()).await.is_err() {
        return None;
    }
    let mut codec = Codec::new();
    let mut buf = [0u8; 4096];
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
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
async fn scan_all_mnemonics() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(!ports.is_empty(), "No TH-D75 found");
    let mut transport = SerialTransport::open(
        &ports[0].port_name,
        SerialTransport::DEFAULT_BAUD,
    )
    .unwrap();

    let chars: Vec<char> = ('A'..='Z').chain('0'..='9').collect();
    let mut found: Vec<(String, String)> = Vec::new();
    let mut scanned = 0u32;
    let total = chars.len() * chars.len();

    println!("\n=== MNEMONIC BRUTE-FORCE SCAN ({total} combinations) ===\n");

    for &c1 in &chars {
        for &c2 in &chars {
            let mnemonic = format!("{c1}{c2}");
            scanned += 1;

            // Skip known dangerous commands
            if DANGEROUS.contains(&mnemonic.as_str()) {
                println!("  [{scanned:4}/{total}] {mnemonic} -> SKIPPED (dangerous)");
                continue;
            }

            match raw_cmd(&mut transport, &mnemonic).await {
                Some(resp) if resp == "?" => {
                    // Unknown command, skip silently
                }
                Some(resp) if resp == "N" => {
                    found.push((mnemonic.clone(), format!("N (not available in current mode)")));
                    println!("  [{scanned:4}/{total}] {mnemonic} -> N (not available)");
                }
                Some(resp) => {
                    found.push((mnemonic.clone(), resp.clone()));
                    println!("  [{scanned:4}/{total}] {mnemonic} -> {resp}");
                }
                None => {
                    println!("  [{scanned:4}/{total}] {mnemonic} -> TIMEOUT/DISCONNECT");
                    // Connection may be dead — try to continue
                }
            }

            // Progress indicator every 100
            if scanned % 100 == 0 {
                eprint!("\r  Progress: {scanned}/{total} ({} found)...", found.len());
            }
        }
    }

    println!("\n\n=== RESULTS ===");
    println!("Scanned: {scanned}/{total}");
    println!("Commands found: {}\n", found.len());

    for (mnemonic, resp) in &found {
        println!("  {mnemonic:<6} -> {resp}");
    }

    // Save to file
    let path = "tests/fixtures/mnemonic_scan_results.txt";
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "TH-D75 Mnemonic Brute-Force Scan Results").unwrap();
    writeln!(f, "Firmware: 1.03").unwrap();
    writeln!(f, "Scanned: {scanned}/{total}").unwrap();
    writeln!(f, "Commands found: {}\n", found.len()).unwrap();
    for (mnemonic, resp) in &found {
        writeln!(f, "  {mnemonic:<6} -> {resp}").unwrap();
    }
    println!("\nResults saved to {path}");

    let _ = transport.close().await;
}
