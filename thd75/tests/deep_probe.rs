//! Deep probe — map the full read-only data surface of the TH-D75.
//!
//! STRICTLY READ-ONLY. Every command sent here has been verified against
//! D74 development notes and Hamlib source.
//!
//! ## Commands EXCLUDED (will change radio state):
//! - SR (RESET — factory resets radio)
//! - 0M (enters programming mode — radio stops responding)
//! - TX (keys transmitter — transmits on air)
//! - RX (forces receive mode)
//! - UP (steps frequency up)
//! - DW (steps frequency DOWN — NOT dual watch!)
//! - BE (sends APRS beacon — transmits on air!)
//! - BC with parameter (changes active band)
//! - MR with 2 params (switches active channel)
//! - VM with 2 params (changes VFO/Memory mode)
//! - GM with parameter (reboots radio into GPS mode!)
//! - AI with parameter (changes auto-info setting)
//! - Any command with write parameters
//!
//! ## Safe read patterns used:
//! - Bare mnemonic: `XX\r` — for commands with no index
//! - Band-indexed: `XX 0\r` / `XX 1\r` — for per-band queries
//! - Channel-indexed: `ME ccc\r` — reads memory channel data
//! - Slot-indexed: `DC 1\r`-`DC 6\r`, `CS 0\r`-`CS 10\r`
//!
//! Run: cargo test --test deep_probe -- --ignored --nocapture --test-threads=1

use kenwood_thd75::protocol::Codec;
use kenwood_thd75::transport::{SerialTransport, Transport};
use std::io::Write as IoWrite;

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

fn log(output: &mut Vec<String>, cmd: &str, resp: &Option<String>) {
    match resp {
        Some(r) => {
            let fields: Vec<&str> = r.split(',').collect();
            if fields.len() > 2 {
                output.push(format!("  {cmd:<25} -> ({} fields) {r}", fields.len()));
            } else {
                output.push(format!("  {cmd:<25} -> {r}"));
            }
        }
        None => output.push(format!("  {cmd:<25} -> TIMEOUT/DISCONNECT")),
    }
}

/// Send a bare read command (no parameters).
async fn bare_read(
    transport: &mut SerialTransport,
    mnemonic: &str,
    output: &mut Vec<String>,
) -> bool {
    let resp = raw_cmd(transport, mnemonic).await;
    log(output, mnemonic, &resp);
    resp.is_some()
}

/// Send a band-indexed read (XX 0, XX 1).
async fn band_read(
    transport: &mut SerialTransport,
    mnemonic: &str,
    output: &mut Vec<String>,
) -> bool {
    for band in 0..=1u8 {
        let cmd = format!("{mnemonic} {band}");
        let resp = raw_cmd(transport, &cmd).await;
        log(output, &cmd, &resp);
        if resp.is_none() {
            return false;
        }
    }
    true
}

#[tokio::test]
#[ignore]
async fn deep_probe_all_reads() {
    let ports = SerialTransport::discover_usb().unwrap();
    assert!(!ports.is_empty(), "No TH-D75 found");
    let mut transport =
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap();

    let mut output: Vec<String> = Vec::new();
    let mut alive = true;

    output.push("TH-D75 Deep Probe — CONFIRMED SAFE READS ONLY".into());
    output.push("Date: 2026-03-25".into());
    output.push(format!("Port: {}", ports[0].port_name));
    output.push("Sources: D74 development notes, Hamlib".into());

    // ================================================================
    // IDENTITY (confirmed read-only, no params)
    // ================================================================
    output.push("\n===== IDENTITY =====".into());
    alive = alive && bare_read(&mut transport, "ID", &mut output).await;
    alive = alive && bare_read(&mut transport, "FV", &mut output).await;
    alive = alive && bare_read(&mut transport, "AE", &mut output).await;

    // ================================================================
    // STATUS (confirmed read-only, no params)
    // ================================================================
    output.push("\n===== STATUS =====".into());
    alive = alive && bare_read(&mut transport, "PS", &mut output).await;
    alive = alive && bare_read(&mut transport, "AI", &mut output).await;
    alive = alive && bare_read(&mut transport, "BC", &mut output).await;
    alive = alive && bare_read(&mut transport, "SD", &mut output).await;
    alive = alive && bare_read(&mut transport, "FR", &mut output).await;
    alive = alive && bare_read(&mut transport, "IO", &mut output).await;
    alive = alive && bare_read(&mut transport, "BL", &mut output).await;

    if !alive {
        output.push("\n*** CONNECTION LOST ***".into());
    }

    // ================================================================
    // FREQUENCY — per band (confirmed: FO band, FQ band)
    // ================================================================
    if alive {
        output.push("\n===== FREQUENCY (per band) =====".into());
        alive = band_read(&mut transport, "FO", &mut output).await;
        if alive {
            alive = band_read(&mut transport, "FQ", &mut output).await;
        }
    }

    // ================================================================
    // VFO SETTINGS — per band
    // Confirmed safe: SQ band, SM band, MD band, PC band, RA band
    // Confirmed safe bare: AG, FS, FT
    // SH uses mode index not band: SH 0, SH 1, SH 2
    // ================================================================
    if alive {
        output.push("\n===== VFO SETTINGS =====".into());
        alive = bare_read(&mut transport, "AG", &mut output).await;
        if alive {
            alive = band_read(&mut transport, "SQ", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "SM", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "MD", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "PC", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "RA", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "FS", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "FT", &mut output).await;
        }
        if alive {
            // SH mode indices 0, 1, 2
            for mode_idx in 0..=2u8 {
                let cmd = format!("SH {mode_idx}");
                let resp = raw_cmd(&mut transport, &cmd).await;
                log(&mut output, &cmd, &resp);
                if resp.is_none() {
                    alive = false;
                    break;
                }
            }
        }
    }

    // ================================================================
    // CONTROL — bare reads
    // ================================================================
    if alive {
        output.push("\n===== CONTROL SETTINGS =====".into());
        alive = bare_read(&mut transport, "DL", &mut output).await;
        if alive {
            alive = bare_read(&mut transport, "LC", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "VX", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "VG", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "VD", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "BY", &mut output).await;
        }
        if alive {
            alive = band_read(&mut transport, "VM", &mut output).await;
        }
    }

    // ================================================================
    // TONE — per band for TN, bare for RT
    // ================================================================
    if alive {
        output.push("\n===== TONE SETTINGS =====".into());
        alive = bare_read(&mut transport, "TN", &mut output).await;
        if alive {
            alive = bare_read(&mut transport, "RT", &mut output).await;
        }
    }

    // ================================================================
    // D-STAR — bare DS, indexed DC 1-6, indexed CS 0-10
    // ================================================================
    if alive {
        output.push("\n===== D-STAR =====".into());
        alive = bare_read(&mut transport, "DS", &mut output).await;
        // DC 1-6: D-Star callsign message slots
        if alive {
            for slot in 1..=6u8 {
                let cmd = format!("DC {slot}");
                let resp = raw_cmd(&mut transport, &cmd).await;
                log(&mut output, &cmd, &resp);
                if resp.is_none() {
                    alive = false;
                    break;
                }
            }
        }
        // CS bare: active callsign slot number
        // NOTE: CS N (indexed) is a WRITE that selects the slot — do NOT send
        if alive {
            alive = bare_read(&mut transport, "CS", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "GW", &mut output).await;
        }
    }

    // ================================================================
    // GPS — bare reads only (GM bare is safe, GM with params is NOT)
    // ================================================================
    if alive {
        output.push("\n===== GPS =====".into());
        alive = bare_read(&mut transport, "GP", &mut output).await;
        if alive {
            alive = bare_read(&mut transport, "GM", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "GS", &mut output).await;
        }
    }

    // ================================================================
    // BLUETOOTH
    // ================================================================
    if alive {
        output.push("\n===== BLUETOOTH =====".into());
        alive = bare_read(&mut transport, "BT", &mut output).await;
    }

    // ================================================================
    // APRS — bare reads for AS, PT, MS
    // AE is serial number (bare read, already captured above)
    // ================================================================
    if alive {
        output.push("\n===== APRS =====".into());
        alive = bare_read(&mut transport, "AS", &mut output).await;
        if alive {
            alive = bare_read(&mut transport, "PT", &mut output).await;
        }
        if alive {
            alive = bare_read(&mut transport, "MS", &mut output).await;
        }
    }

    // ================================================================
    // USER SETTINGS — bare read
    // ================================================================
    if alive {
        output.push("\n===== USER SETTINGS =====".into());
        alive = bare_read(&mut transport, "US", &mut output).await;
    }

    // ================================================================
    // SCAN — SF per band, BS per band
    // ================================================================
    if alive {
        output.push("\n===== SCAN =====".into());
        alive = band_read(&mut transport, "SF", &mut output).await;
        if alive {
            alive = band_read(&mut transport, "BS", &mut output).await;
        }
    }

    // ================================================================
    // MEMORY CHANNELS 0-999
    // ME ccc reads channel data without changing active channel
    // ================================================================
    if alive {
        output.push("\n===== MEMORY CHANNELS (0-999) =====".into());
        let mut consecutive_empty = 0u16;
        let mut found = 0u16;
        for ch in 0..=999u16 {
            let cmd = format!("ME {:03}", ch);
            let resp = raw_cmd(&mut transport, &cmd).await;
            match &resp {
                Some(r) if r == "?" || r == "N" => {
                    consecutive_empty += 1;
                    if consecutive_empty >= 100 {
                        output.push(format!(
                            "  ME {:03}+: 100 consecutive empty, stopping (found {found} total)",
                            ch
                        ));
                        break;
                    }
                }
                Some(r) => {
                    consecutive_empty = 0;
                    found += 1;
                    let fields: Vec<&str> = r.split(',').collect();
                    let freq = fields.get(1).unwrap_or(&"?");
                    let urcall = fields.get(19).unwrap_or(&"?");
                    output.push(format!(
                        "  ME {:03}  freq={:<12} urcall={:<10} ({} fields)",
                        ch,
                        freq,
                        urcall,
                        fields.len()
                    ));
                }
                None => {
                    output.push(format!("  ME {:03}  -> TIMEOUT", ch));
                    alive = false;
                    break;
                }
            }
        }
        if alive {
            output.push(format!("  Total populated channels: {found}"));
        }
    }

    let _ = transport.close().await;

    // ================================================================
    // PRINT & SAVE
    // ================================================================
    if !alive {
        output.push("\n*** CONNECTION LOST DURING PROBE ***".into());
    }

    println!("\n{}", "=".repeat(72));
    println!("  TH-D75 DEEP PROBE — CONFIRMED SAFE READS ONLY");
    println!("{}\n", "=".repeat(72));
    for line in &output {
        println!("{line}");
    }

    let path = "tests/fixtures/deep_probe_results.txt";
    let mut f = std::fs::File::create(path).unwrap();
    for line in &output {
        writeln!(f, "{line}").unwrap();
    }
    println!("\nResults saved to {path}");
}
