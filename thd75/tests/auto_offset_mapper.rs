//! Automated offset mapper -- uses CAT commands to change settings,
//! then compares MCP memory dumps to find the exact byte offset for each.
//!
//! # Strategy: single-dump differential
//!
//! The old approach entered MCP programming mode after every CAT setting
//! change: change via CAT, dump 500 KB, exit MCP (USB drops), reconnect,
//! restore, repeat. Each iteration took ~55 seconds and the USB
//! drop/reconnect was unreliable enough to crash the radio.
//!
//! The new approach uses exactly **two** MCP sessions per setting:
//!
//! 1. **Baseline dump** -- one MCP session (~50 s), USB drops.
//! 2. Reconnect, change ONE setting via CAT (fast, < 1 s).
//! 3. **Modified dump** -- one MCP session (~50 s), USB drops.
//! 4. Reconnect, restore the setting via CAT, disconnect cleanly.
//! 5. Diff baseline vs modified.
//!
//! Each setting gets its own `#[ignore]` test so it can be run individually
//! without risk of accumulating MCP sessions.
//!
//! Run one:   `cargo test --test auto_offset_mapper map_squelch_a -- --ignored --nocapture`
//! Run all:   `cargo test --test auto_offset_mapper -- --ignored --nocapture --test-threads=1`

use std::collections::BTreeMap;
use std::fmt::Write as _;

use kenwood_thd75::protocol::Command;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Connect to the first discovered USB serial radio.
async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::connect(
        SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD).unwrap(),
    )
    .await
    .unwrap()
}

/// Wait for the USB stack to re-enumerate after MCP exit, then connect.
async fn reconnect() -> Radio<SerialTransport> {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    connect().await
}

/// Read the full 500 KB memory image with progress output.
async fn dump_memory(radio: &mut Radio<SerialTransport>) -> Vec<u8> {
    radio
        .read_memory_image_with_progress(|cur, total| {
            if cur % 500 == 0 {
                eprint!("\r    dumping {cur}/{total}...");
            }
        })
        .await
        .unwrap()
}

/// Byte-level diff between two equally-sized images.
fn diff_bytes(a: &[u8], b: &[u8]) -> Vec<(usize, u8, u8)> {
    a.iter()
        .zip(b.iter())
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, (&x, &y))| (i, x, y))
        .collect()
}

/// Human-readable region name for a given byte offset.
const fn region_name(offset: usize) -> &'static str {
    match offset {
        0x00000..0x02000 => "Settings",
        0x02000..0x03300 => "Ch Flags",
        0x04000..0x10000 => "Ch Data",
        0x10000..0x14B00 => "Ch Names",
        0x15100..0x2A100 => "APRS",
        0x2A100..0x4D100 => "D-STAR",
        0x4D100..0x7A300 => "BT/Tail",
        _ => "Unknown",
    }
}

/// Run a single-setting mapping test with exactly two MCP dumps.
///
/// 1. Baseline dump (MCP session 1).
/// 2. Reconnect, apply `set_cmd` via CAT.
/// 3. Modified dump (MCP session 2).
/// 4. Reconnect, apply `restore_cmd` via CAT.
/// 5. Print and return the byte-level diffs.
async fn map_single_setting(
    name: &str,
    set_cmd: Command,
    restore_cmd: Command,
) -> Vec<(usize, u8, u8)> {
    println!("\n=== Mapping '{name}' ===\n");

    // Step 1: Baseline dump.
    println!("  [1/4] Baseline dump...");
    let mut radio = connect().await;
    let baseline = dump_memory(&mut radio).await;
    eprintln!();
    println!("         {} bytes", baseline.len());
    // MCP exit drops USB -- drop the handle.
    drop(radio);

    // Step 2: Reconnect and change the setting via CAT.
    println!("  [2/4] Changing setting via CAT...");
    let mut radio = reconnect().await;
    let set_result = radio.execute(set_cmd).await;
    match &set_result {
        Ok(_) => println!("         OK"),
        Err(e) => {
            println!("         FAILED: {e}");
            let _ = radio.disconnect().await;
            return Vec::new();
        }
    }

    // Step 3: Modified dump.
    println!("  [3/4] Modified dump...");
    let modified = dump_memory(&mut radio).await;
    eprintln!();
    println!("         {} bytes", modified.len());
    // MCP exit drops USB again.
    drop(radio);

    // Step 4: Reconnect and restore.
    println!("  [4/4] Restoring setting via CAT...");
    let mut radio = reconnect().await;
    let restore_result = radio.execute(restore_cmd).await;
    match &restore_result {
        Ok(_) => println!("         OK"),
        Err(e) => println!("         FAILED: {e}"),
    }
    let _ = radio.disconnect().await;

    // Step 5: Diff and report.
    let diffs = diff_bytes(&baseline, &modified);
    println!("\n  Changed {} bytes:", diffs.len());
    for &(offset, old, new) in &diffs {
        println!(
            "    0x{:05X} ({:<10}): 0x{:02X} -> 0x{:02X}",
            offset,
            region_name(offset),
            old,
            new
        );
    }

    diffs
}

// ---------------------------------------------------------------------------
// Individual mapping tests -- one setting per test, two MCP sessions each
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_squelch_a() {
    let _ = map_single_setting(
        "squelch_a",
        Command::SetSquelch {
            band: Band::A,
            level: 5,
        },
        Command::SetSquelch {
            band: Band::A,
            level: 2,
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_power_level_a() {
    let _ = map_single_setting(
        "power_level_a",
        Command::SetPowerLevel {
            band: Band::A,
            level: PowerLevel::Low,
        },
        Command::SetPowerLevel {
            band: Band::A,
            level: PowerLevel::High,
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_attenuator_a() {
    let _ = map_single_setting(
        "attenuator_a",
        Command::SetAttenuator {
            band: Band::A,
            enabled: true,
        },
        Command::SetAttenuator {
            band: Band::A,
            enabled: false,
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_lock() {
    let _ = map_single_setting(
        "lock",
        Command::SetLock { locked: false },
        Command::SetLock { locked: true },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_dual_band() {
    let _ = map_single_setting(
        "dual_band",
        Command::SetDualBand { enabled: true },
        Command::SetDualBand { enabled: false },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_enable() {
    let _ = map_single_setting(
        "vox_enable",
        Command::SetVox { enabled: true },
        Command::SetVox { enabled: false },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_gain() {
    // VOX must already be enabled for VG to work. This test assumes
    // VOX is on or accepts that the radio may return N.
    let _ = map_single_setting(
        "vox_gain",
        Command::SetVoxGain { gain: 9 },
        Command::SetVoxGain { gain: 4 },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_delay() {
    // VOX must already be enabled for VD to work. Same caveat as vox_gain.
    let _ = map_single_setting(
        "vox_delay",
        Command::SetVoxDelay { delay: 9 },
        Command::SetVoxDelay { delay: 1 },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_backlight() {
    let _ = map_single_setting(
        "backlight_level",
        Command::SetBacklight { level: 1 },
        Command::SetBacklight { level: 4 },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_bluetooth() {
    let _ = map_single_setting(
        "bluetooth_off",
        Command::SetBluetooth { enabled: false },
        Command::SetBluetooth { enabled: true },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_dual_watch() {
    let _ = map_single_setting(
        "dual_watch",
        Command::SetDualWatch { enabled: true },
        Command::SetDualWatch { enabled: false },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_auto_info() {
    let _ = map_single_setting(
        "auto_info",
        Command::SetAutoInfo { enabled: true },
        Command::SetAutoInfo { enabled: false },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_mode_a_nfm() {
    let _ = map_single_setting(
        "mode_a_nfm",
        Command::SetMode {
            band: Band::A,
            mode: Mode::Nfm,
        },
        Command::SetMode {
            band: Band::A,
            mode: Mode::Fm,
        },
    )
    .await;
}

// ---------------------------------------------------------------------------
// Batch mapper -- runs all settings sequentially, writes results file
// ---------------------------------------------------------------------------

/// Defines a single CAT setting to map via differential MCP dumps.
struct SettingTest {
    /// Human-readable name for the setting.
    name: &'static str,
    /// CAT command to change the setting to a known non-default value.
    set_cmd: Command,
    /// CAT command to restore the setting to its original value.
    restore_cmd: Command,
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
#[allow(clippy::too_many_lines)]
async fn map_all_settings() {
    println!("\n=== AUTOMATED OFFSET MAPPER (single-dump per setting) ===\n");

    let tests = vec![
        SettingTest {
            name: "backlight_level",
            set_cmd: Command::SetBacklight { level: 1 },
            restore_cmd: Command::SetBacklight { level: 4 },
        },
        SettingTest {
            name: "vox_enable",
            set_cmd: Command::SetVox { enabled: true },
            restore_cmd: Command::SetVox { enabled: false },
        },
        SettingTest {
            name: "vox_gain",
            set_cmd: Command::SetVoxGain { gain: 9 },
            restore_cmd: Command::SetVoxGain { gain: 4 },
        },
        SettingTest {
            name: "vox_delay",
            set_cmd: Command::SetVoxDelay { delay: 9 },
            restore_cmd: Command::SetVoxDelay { delay: 1 },
        },
        SettingTest {
            name: "lock",
            set_cmd: Command::SetLock { locked: false },
            restore_cmd: Command::SetLock { locked: true },
        },
        SettingTest {
            name: "dual_band",
            set_cmd: Command::SetDualBand { enabled: true },
            restore_cmd: Command::SetDualBand { enabled: false },
        },
        SettingTest {
            name: "attenuator_a",
            set_cmd: Command::SetAttenuator {
                band: Band::A,
                enabled: true,
            },
            restore_cmd: Command::SetAttenuator {
                band: Band::A,
                enabled: false,
            },
        },
        SettingTest {
            name: "power_level_a",
            set_cmd: Command::SetPowerLevel {
                band: Band::A,
                level: PowerLevel::Low,
            },
            restore_cmd: Command::SetPowerLevel {
                band: Band::A,
                level: PowerLevel::High,
            },
        },
        SettingTest {
            name: "squelch_a",
            set_cmd: Command::SetSquelch {
                band: Band::A,
                level: 5,
            },
            restore_cmd: Command::SetSquelch {
                band: Band::A,
                level: 2,
            },
        },
        // BE (beep) write is a D75 firmware stub -- always returns `?`.
        // Beep can only be changed via MCP memory write. Excluded.
        SettingTest {
            name: "bluetooth_off",
            set_cmd: Command::SetBluetooth { enabled: false },
            restore_cmd: Command::SetBluetooth { enabled: true },
        },
        SettingTest {
            name: "dual_watch",
            set_cmd: Command::SetDualWatch { enabled: true },
            restore_cmd: Command::SetDualWatch { enabled: false },
        },
        SettingTest {
            name: "auto_info",
            set_cmd: Command::SetAutoInfo { enabled: true },
            restore_cmd: Command::SetAutoInfo { enabled: false },
        },
        SettingTest {
            name: "mode_a_nfm",
            set_cmd: Command::SetMode {
                band: Band::A,
                mode: Mode::Nfm,
            },
            restore_cmd: Command::SetMode {
                band: Band::A,
                mode: Mode::Fm,
            },
        },
        // TN = TNC mode, DC = D-STAR callsign (not tone/DCS).
        // CTCSS/DCS are set through the FO (full channel) command.
    ];

    let mut results: BTreeMap<String, Vec<(usize, u8, u8)>> = BTreeMap::new();

    for (i, test) in tests.iter().enumerate() {
        println!(
            "\n--- Setting {}/{}: '{}' ---",
            i + 1,
            tests.len(),
            test.name
        );

        let diffs = map_single_setting(
            test.name,
            test.set_cmd.clone(),
            test.restore_cmd.clone(),
        )
        .await;
        let _ = results.insert(test.name.to_string(), diffs);
    }

    // Summary table.
    println!("\n\n=== OFFSET MAP RESULTS ===\n");
    println!("| Setting              | Offset  | Region     | Old  | New  |");
    println!("|----------------------|---------|------------|------|------|");
    for (name, diffs) in &results {
        if diffs.is_empty() {
            println!("| {name:<20} | (no change detected) |            |      |      |");
        }
        for &(offset, old, new) in diffs {
            println!(
                "| {:<20} | 0x{:05X} | {:<10} | 0x{:02X} | 0x{:02X} |",
                name,
                offset,
                region_name(offset),
                old,
                new
            );
        }
    }

    // Append new results to the existing verified offsets file.
    let mut output = String::from("# Verified MCP Offset Map\n\n");
    output.push_str("| Setting | Offset | Old | New |\n");
    output.push_str("|---------|--------|-----|-----|\n");
    for (name, diffs) in &results {
        for &(offset, old, new) in diffs {
            let _ = writeln!(
                output,
                "| {name} | 0x{offset:05X} | 0x{old:02X} | 0x{new:02X} |",
            );
        }
    }
    std::fs::write("tests/fixtures/verified_offsets.md", &output).unwrap();
    println!("\nSaved to tests/fixtures/verified_offsets.md");
}
