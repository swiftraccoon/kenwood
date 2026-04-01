//! Comprehensive CAT command verification.
//! Tests every command's wire format against the live radio.
//! READ-ONLY except where explicitly marked.
//!
//! This is the definitive test that every CAT command works as expected.
//!
//! Run: cargo test --test cat_verification -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};
use std::io::Write as IoWrite;

async fn raw_exchange(
    transport: &mut SerialTransport,
    cmd: &str,
) -> (String, Option<String>) {
    let wire = format!("{cmd}\r");
    let _ = transport.write(wire.as_bytes()).await;
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

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

    let resp = match result {
        Ok(Some(s)) => Some(s),
        _ => None,
    };
    (cmd.to_string(), resp)
}

fn check(
    results: &mut Vec<(String, String, String)>,
    cmd: &str,
    resp: &Option<String>,
    expected_prefix: &str,
    description: &str,
) {
    let status = match resp {
        Some(r) if r == "?" => "REJECTED".to_string(),
        Some(r) if r == "N" => "NOT_AVAIL".to_string(),
        Some(r) if r.starts_with(expected_prefix) => "OK".to_string(),
        Some(r) => format!("UNEXPECTED({r})"),
        None => "TIMEOUT".to_string(),
    };
    let resp_str = resp.clone().unwrap_or_else(|| "TIMEOUT".to_string());
    println!(
        "  {:<8} {:<25} {:<12} {}",
        status, cmd, resp_str, description
    );
    results.push((cmd.to_string(), status, resp_str));
}

#[tokio::test]
#[ignore]
async fn verify_all_cat_commands() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(!ports.is_empty(), "No TH-D75 found");
    let mut transport = SerialTransport::open(
        &ports[0].port_name,
        SerialTransport::DEFAULT_BAUD,
    )
    .unwrap();

    let mut results: Vec<(String, String, String)> = Vec::new();

    println!("\n========================================================================");
    println!("  CAT COMMAND VERIFICATION — every command tested against live radio");
    println!("========================================================================\n");
    println!(
        "  {:<8} {:<25} {:<12} {}",
        "STATUS", "COMMAND", "RESPONSE", "DESCRIPTION"
    );
    println!("  {}", "-".repeat(70));

    // ===== IDENTITY (read-only, no params) =====
    let (cmd, resp) = raw_exchange(&mut transport, "ID").await;
    check(&mut results, &cmd, &resp, "ID", "Radio model");

    let (cmd, resp) = raw_exchange(&mut transport, "FV").await;
    check(&mut results, &cmd, &resp, "FV", "Firmware version");

    let (cmd, resp) = raw_exchange(&mut transport, "AE").await;
    check(&mut results, &cmd, &resp, "AE", "Serial/model code");

    let (cmd, resp) = raw_exchange(&mut transport, "TY").await;
    check(&mut results, &cmd, &resp, "TY", "Region/type code");

    // ===== STATUS (read-only, no params) =====
    let (cmd, resp) = raw_exchange(&mut transport, "PS").await;
    check(&mut results, &cmd, &resp, "PS", "Power status");

    let (cmd, resp) = raw_exchange(&mut transport, "AI").await;
    check(&mut results, &cmd, &resp, "AI", "Auto-info status");

    let (cmd, resp) = raw_exchange(&mut transport, "BC").await;
    check(&mut results, &cmd, &resp, "BC", "Active band");

    let (cmd, resp) = raw_exchange(&mut transport, "BT").await;
    check(&mut results, &cmd, &resp, "BT", "Bluetooth status");

    let (cmd, resp) = raw_exchange(&mut transport, "SD").await;
    check(&mut results, &cmd, &resp, "SD", "SD card status");

    let (cmd, resp) = raw_exchange(&mut transport, "FR").await;
    check(&mut results, &cmd, &resp, "FR", "FM radio status");

    let (cmd, resp) = raw_exchange(&mut transport, "IO").await;
    check(&mut results, &cmd, &resp, "IO", "I/O port");

    let (cmd, resp) = raw_exchange(&mut transport, "BL").await;
    check(&mut results, &cmd, &resp, "BL", "Backlight level");

    // ===== FREQUENCY (band-indexed reads) =====
    for b in ["0", "1"] {
        let (cmd, resp) = raw_exchange(&mut transport, &format!("FO {b}")).await;
        check(&mut results, &cmd, &resp, "FO", &format!("Full freq band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("FQ {b}")).await;
        check(&mut results, &cmd, &resp, "FQ", &format!("Quick freq band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("SM {b}")).await;
        check(&mut results, &cmd, &resp, "SM", &format!("S-meter band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("SQ {b}")).await;
        check(&mut results, &cmd, &resp, "SQ", &format!("Squelch band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("MD {b}")).await;
        check(&mut results, &cmd, &resp, "MD", &format!("Mode band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("PC {b}")).await;
        check(&mut results, &cmd, &resp, "PC", &format!("Power level band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("RA {b}")).await;
        check(&mut results, &cmd, &resp, "RA", &format!("Attenuator band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("BY {b}")).await;
        check(&mut results, &cmd, &resp, "BY", &format!("Busy band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("VM {b}")).await;
        check(&mut results, &cmd, &resp, "VM", &format!("VFO/Mem mode band {b}"));
    }

    // ===== FREQUENCY (band-indexed per D75 RE) =====
    for b in ["0", "1"] {
        let (cmd, resp) = raw_exchange(&mut transport, &format!("TN {b}")).await;
        check(&mut results, &cmd, &resp, "TN", &format!("CTCSS tone band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("DC {b}")).await;
        check(&mut results, &cmd, &resp, "DC", &format!("DCS code band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("RT {b}")).await;
        check(&mut results, &cmd, &resp, "RT", &format!("Repeater tone band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("FS {b}")).await;
        check(&mut results, &cmd, &resp, "FS", &format!("Freq step band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("SF {b}")).await;
        check(&mut results, &cmd, &resp, "SF", &format!("Scan range band {b}"));

        let (cmd, resp) = raw_exchange(&mut transport, &format!("BS {b}")).await;
        check(&mut results, &cmd, &resp, "BS", &format!("Band scope band {b}"));
    }

    // ===== BARE READS (D75 RE format) =====
    let (cmd, resp) = raw_exchange(&mut transport, "AG").await;
    check(&mut results, &cmd, &resp, "AG", "AF gain (bare)");

    let (cmd, resp) = raw_exchange(&mut transport, "DL").await;
    check(&mut results, &cmd, &resp, "DL", "Dual band display");

    let (cmd, resp) = raw_exchange(&mut transport, "DW").await;
    check(&mut results, &cmd, &resp, "DW", "Dual watch");

    let (cmd, resp) = raw_exchange(&mut transport, "LC").await;
    check(&mut results, &cmd, &resp, "LC", "Lock control");

    let (cmd, resp) = raw_exchange(&mut transport, "VX").await;
    check(&mut results, &cmd, &resp, "VX", "VOX on/off");

    let (cmd, resp) = raw_exchange(&mut transport, "VG").await;
    check(&mut results, &cmd, &resp, "VG", "VOX gain");

    let (cmd, resp) = raw_exchange(&mut transport, "VD").await;
    check(&mut results, &cmd, &resp, "VD", "VOX delay");

    let (cmd, resp) = raw_exchange(&mut transport, "FT").await;
    check(&mut results, &cmd, &resp, "FT", "Function type");

    let (cmd, resp) = raw_exchange(&mut transport, "BE").await;
    check(&mut results, &cmd, &resp, "BE", "Beep");

    // ===== SH (mode-indexed per D74, bare per D75 RE?) =====
    let (cmd, resp) = raw_exchange(&mut transport, "SH").await;
    check(&mut results, &cmd, &resp, "SH", "Filter width (bare)");

    let (cmd, resp) = raw_exchange(&mut transport, "SH 0").await;
    check(&mut results, &cmd, &resp, "SH", "Filter width mode 0");

    // ===== AG (bare vs band-indexed) =====
    let (cmd, resp) = raw_exchange(&mut transport, "AG 0").await;
    check(&mut results, &cmd, &resp, "AG", "AF gain band 0");

    let (cmd, resp) = raw_exchange(&mut transport, "AG 1").await;
    check(&mut results, &cmd, &resp, "AG", "AF gain band 1");

    // ===== TN (bare vs band-indexed) =====
    let (cmd, resp) = raw_exchange(&mut transport, "TN").await;
    check(&mut results, &cmd, &resp, "TN", "TN bare");

    // ===== D-STAR =====
    let (cmd, resp) = raw_exchange(&mut transport, "DS").await;
    check(&mut results, &cmd, &resp, "DS", "D-STAR slot");

    let (cmd, resp) = raw_exchange(&mut transport, "CS").await;
    check(&mut results, &cmd, &resp, "CS", "Active callsign slot");

    let (cmd, resp) = raw_exchange(&mut transport, "GW").await;
    check(&mut results, &cmd, &resp, "GW", "Gateway");

    for slot in 1..=6u8 {
        let (cmd, resp) = raw_exchange(&mut transport, &format!("DC {slot}")).await;
        check(&mut results, &cmd, &resp, "DC", &format!("D-STAR callsign slot {slot}"));
    }

    // ===== GPS =====
    let (cmd, resp) = raw_exchange(&mut transport, "GP").await;
    check(&mut results, &cmd, &resp, "GP", "GPS config");

    let (cmd, resp) = raw_exchange(&mut transport, "GM").await;
    check(&mut results, &cmd, &resp, "GM", "GPS mode");

    let (cmd, resp) = raw_exchange(&mut transport, "GS").await;
    check(&mut results, &cmd, &resp, "GS", "GPS sentences");

    // ===== APRS =====
    let (cmd, resp) = raw_exchange(&mut transport, "AS").await;
    check(&mut results, &cmd, &resp, "AS", "TNC baud/APRS");

    let (cmd, resp) = raw_exchange(&mut transport, "PT").await;
    check(&mut results, &cmd, &resp, "PT", "Beacon type");

    let (cmd, resp) = raw_exchange(&mut transport, "MS").await;
    check(&mut results, &cmd, &resp, "MS", "Position source");

    // ===== CLOCK =====
    let (cmd, resp) = raw_exchange(&mut transport, "RT").await;
    check(&mut results, &cmd, &resp, "RT", "RT bare (clock or tone?)");

    // ===== SCAN =====
    let (cmd, resp) = raw_exchange(&mut transport, "SR").await;
    check(&mut results, &cmd, &resp, "SR", "Scan resume (CAUTION)");

    // ===== MEMORY =====
    let (cmd, resp) = raw_exchange(&mut transport, "ME 000").await;
    check(&mut results, &cmd, &resp, "ME", "Memory channel 000");

    let (cmd, resp) = raw_exchange(&mut transport, "MR 0").await;
    check(&mut results, &cmd, &resp, "MR", "Memory recall band 0");

    // ===== USER SETTINGS =====
    let (cmd, resp) = raw_exchange(&mut transport, "US").await;
    check(&mut results, &cmd, &resp, "US", "User settings");

    let _ = transport.close().await;

    // ===== SUMMARY =====
    println!("\n========================================================================");
    println!("  SUMMARY");
    println!("========================================================================\n");

    let ok_count = results.iter().filter(|(_, s, _)| s == "OK").count();
    let rejected = results.iter().filter(|(_, s, _)| s == "REJECTED").count();
    let not_avail = results.iter().filter(|(_, s, _)| s == "NOT_AVAIL").count();
    let unexpected = results
        .iter()
        .filter(|(_, s, _)| s.starts_with("UNEXPECTED"))
        .count();
    let timeout = results.iter().filter(|(_, s, _)| s == "TIMEOUT").count();

    println!("  OK:         {ok_count}");
    println!("  REJECTED:   {rejected} (radio returned ?)");
    println!("  NOT_AVAIL:  {not_avail} (radio returned N)");
    println!("  UNEXPECTED: {unexpected}");
    println!("  TIMEOUT:    {timeout}");
    println!("  TOTAL:      {}", results.len());

    if rejected > 0 {
        println!("\n  REJECTED commands (need format fix):");
        for (cmd, status, resp) in &results {
            if status == "REJECTED" {
                println!("    {cmd} -> {resp}");
            }
        }
    }
    if not_avail > 0 {
        println!("\n  NOT_AVAIL commands (mode-dependent):");
        for (cmd, status, resp) in &results {
            if status == "NOT_AVAIL" {
                println!("    {cmd} -> {resp}");
            }
        }
    }
    if unexpected > 0 {
        println!("\n  UNEXPECTED responses:");
        for (cmd, status, resp) in &results {
            if status.starts_with("UNEXPECTED") {
                println!("    {cmd} -> {resp}");
            }
        }
    }

    // Save results
    let path = "tests/fixtures/cat_verification_results.txt";
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "CAT Command Verification Results").unwrap();
    writeln!(f, "================================\n").unwrap();
    for (cmd, status, resp) in &results {
        writeln!(f, "{status:<12} {cmd:<25} {resp}").unwrap();
    }
    println!("\n  Saved to {path}");
}
