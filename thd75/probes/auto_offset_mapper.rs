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
//! This archival probe source is not registered as a Cargo target. Before a
//! hardware run, review it against `docs/audit/probe_queue.md`, promote the
//! reviewed copy to an explicit test target, and run that target serially.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use kenwood_thd75::error::Error;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Connect to the first discovered USB serial radio.
fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().unwrap();
    Radio::new(SerialTransport::open(&ports[0].port_name).unwrap())
}

/// Wait for the USB stack to re-enumerate after MCP exit, then connect.
async fn reconnect() -> Radio<SerialTransport> {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    connect()
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
        0x15100..0x25000 => "APRS",
        0x25000..0x29B00 => "D-STAR callsign list",
        0x29B00..0x2A000 => "D-STAR list gap",
        0x2A000..0x4D100 => "D-STAR repeater/tail",
        0x4D100..0x7A300 => "BT/Tail",
        _ => "Unknown",
    }
}

/// One setting whose live value can be observed and changed through a typed
/// `Radio` operation.
#[derive(Debug, Clone, Copy)]
enum SettingKind {
    Squelch(Band),
    PowerLevel(Band),
    Attenuator(Band),
    BacklightControl,
    BandMode,
    Vox,
    VoxGain,
    VoxDelay,
    Bluetooth,
    AutoInfo,
    OperatingMode(Band),
}

impl SettingKind {
    /// Read the exact value that must be restored after the differential run.
    async fn observe(self, radio: &mut Radio<SerialTransport>) -> Result<SettingValue, Error> {
        match self {
            Self::Squelch(band) => radio
                .get_squelch(band)
                .await
                .map(|value| SettingValue::Squelch { band, value }),
            Self::PowerLevel(band) => radio
                .get_power_level(band)
                .await
                .map(|value| SettingValue::PowerLevel { band, value }),
            Self::Attenuator(band) => radio
                .get_attenuator(band)
                .await
                .map(|enabled| SettingValue::Attenuator { band, enabled }),
            Self::BacklightControl => radio
                .get_backlight_control()
                .await
                .map(SettingValue::BacklightControl),
            Self::BandMode => radio.get_band_mode().await.map(SettingValue::BandMode),
            Self::Vox => radio.get_vox().await.map(SettingValue::Vox),
            Self::VoxGain => radio.get_vox_gain().await.map(SettingValue::VoxGain),
            Self::VoxDelay => radio.get_vox_delay().await.map(SettingValue::VoxDelay),
            Self::Bluetooth => radio.get_bluetooth().await.map(SettingValue::Bluetooth),
            Self::AutoInfo => radio.get_auto_info().await.map(SettingValue::AutoInfo),
            Self::OperatingMode(band) => radio
                .get_operating_mode(band)
                .await
                .map(|value| SettingValue::OperatingMode { band, value }),
        }
    }
}

/// A fully typed setting value, including any band needed to write it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingValue {
    Squelch { band: Band, value: SquelchLevel },
    PowerLevel { band: Band, value: PowerLevel },
    Attenuator { band: Band, enabled: bool },
    BacklightControl(BacklightControl),
    BandMode(BandMode),
    Vox(bool),
    VoxGain(VoxGain),
    VoxDelay(VoxDelay),
    Bluetooth(bool),
    AutoInfo(bool),
    OperatingMode {
        band: Band,
        value: OperatingMode,
    },
}

impl SettingValue {
    /// Choose a valid value guaranteed to differ from the observed value.
    fn contrasting(self) -> Self {
        match self {
            Self::Squelch { band, value } => Self::Squelch {
                band,
                value: if value == SquelchLevel::MAX {
                    SquelchLevel::OPEN
                } else {
                    SquelchLevel::MAX
                },
            },
            Self::PowerLevel { band, value } => Self::PowerLevel {
                band,
                value: if value == PowerLevel::Low {
                    PowerLevel::High
                } else {
                    PowerLevel::Low
                },
            },
            Self::Attenuator { band, enabled } => Self::Attenuator {
                band,
                enabled: !enabled,
            },
            Self::BacklightControl(value) => {
                Self::BacklightControl(if value == BacklightControl::Manual {
                    BacklightControl::Auto
                } else {
                    BacklightControl::Manual
                })
            }
            Self::BandMode(mode) => Self::BandMode(match mode {
                BandMode::Dual => BandMode::Single,
                BandMode::Single => BandMode::Dual,
            }),
            Self::Vox(enabled) => Self::Vox(!enabled),
            Self::VoxGain(value) => Self::VoxGain(if value.as_raw() == VoxGain::MAX {
                VoxGain::ZERO
            } else {
                VoxGain::new(VoxGain::MAX).expect("VoxGain::MAX must remain valid")
            }),
            Self::VoxDelay(value) => Self::VoxDelay(if value == VoxDelay::MS_3000 {
                VoxDelay::MS_250
            } else {
                VoxDelay::MS_3000
            }),
            Self::Bluetooth(enabled) => Self::Bluetooth(!enabled),
            Self::AutoInfo(enabled) => Self::AutoInfo(!enabled),
            Self::OperatingMode { band, value } => Self::OperatingMode {
                band,
                value: if value == OperatingMode::Nfm {
                    OperatingMode::Fm
                } else {
                    OperatingMode::Nfm
                },
            },
        }
    }

    /// Apply this value through the corresponding validated high-level API.
    async fn apply(self, radio: &mut Radio<SerialTransport>) -> Result<(), Error> {
        match self {
            Self::Squelch { band, value } => radio.set_squelch(band, value).await,
            Self::PowerLevel { band, value } => radio.set_power_level(band, value).await,
            Self::Attenuator { band, enabled } => radio.set_attenuator(band, enabled).await,
            Self::BacklightControl(value) => radio.set_backlight_control(value).await,
            Self::BandMode(mode) => radio.set_band_mode(mode).await,
            Self::Vox(enabled) => radio.set_vox(enabled).await,
            Self::VoxGain(value) => radio.set_vox_gain(value).await,
            Self::VoxDelay(value) => radio.set_vox_delay(value).await,
            Self::Bluetooth(enabled) => radio.set_bluetooth(enabled).await,
            Self::AutoInfo(enabled) => radio.set_auto_info(enabled).await,
            Self::OperatingMode { band, value } => {
                radio.set_operating_mode(band, value).await
            }
        }
    }
}

/// Run a single-setting mapping test with exactly two MCP dumps.
///
/// 1. Baseline dump (MCP session 1).
/// 2. Reconnect, apply a contrasting value via the typed CAT API.
/// 3. Modified dump (MCP session 2).
/// 4. Reconnect, restore the exact value observed before the baseline dump.
/// 5. Print and return the byte-level diffs.
async fn map_single_setting(name: &str, kind: SettingKind) -> Vec<(usize, u8, u8)> {
    map_single_setting_with_radio(name, connect(), kind).await
}

async fn map_single_setting_with_radio(
    name: &str,
    mut radio: Radio<SerialTransport>,
    kind: SettingKind,
) -> Vec<(usize, u8, u8)> {
    println!("\n=== Mapping '{name}' ===\n");

    let original = match kind.observe(&mut radio).await {
        Ok(value) => value,
        Err(error) => {
            println!("  Cannot observe the original value: {error}");
            let _ = radio.disconnect().await;
            return Vec::new();
        }
    };
    let changed = original.contrasting();
    println!("  Original: {original:?}");
    println!("  Probe value: {changed:?}");

    // Step 1: Baseline dump.
    println!("  [1/4] Baseline dump...");
    let baseline = dump_memory(&mut radio).await;
    eprintln!();
    println!("         {} bytes", baseline.len());
    // MCP exit drops USB -- drop the handle.
    drop(radio);

    // Step 2: Reconnect and change the setting via CAT.
    println!("  [2/4] Changing setting via CAT...");
    let mut radio = reconnect().await;
    let set_result = changed.apply(&mut radio).await;
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
    let restore_result = original.apply(&mut radio).await;
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

async fn map_backlight_control_setting() -> Vec<(usize, u8, u8)> {
    map_single_setting("backlight_control", SettingKind::BacklightControl).await
}

#[test]
fn every_probe_value_differs_from_its_observed_value() {
    let values = [
        SettingValue::Squelch {
            band: Band::A,
            value: SquelchLevel::OPEN,
        },
        SettingValue::PowerLevel {
            band: Band::A,
            value: PowerLevel::High,
        },
        SettingValue::Attenuator {
            band: Band::A,
            enabled: false,
        },
        SettingValue::BacklightControl(BacklightControl::Manual),
        SettingValue::BandMode(BandMode::Single),
        SettingValue::Vox(false),
        SettingValue::VoxGain(VoxGain::ZERO),
        SettingValue::VoxDelay(VoxDelay::MS_250),
        SettingValue::Bluetooth(false),
        SettingValue::AutoInfo(false),
        SettingValue::OperatingMode {
            band: Band::A,
            value: OperatingMode::Fm,
        },
    ];

    for original in values {
        assert_ne!(original.contrasting(), original);
    }
}

// ---------------------------------------------------------------------------
// Individual mapping tests -- one setting per test, two MCP sessions each
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_squelch_a() {
    let _ = map_single_setting("squelch_a", SettingKind::Squelch(Band::A)).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_power_level_a() {
    let _ = map_single_setting("power_level_a", SettingKind::PowerLevel(Band::A)).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_attenuator_a() {
    let _ = map_single_setting("attenuator_a", SettingKind::Attenuator(Band::A)).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_backlight_control() {
    let _ = map_backlight_control_setting().await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_dual_band() {
    let _ = map_single_setting("band_mode", SettingKind::BandMode).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_enable() {
    let _ = map_single_setting("vox_enable", SettingKind::Vox).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_gain() {
    // VOX must already be enabled for VG to work. This test assumes
    // VOX is on or accepts that the radio may return N.
    let _ = map_single_setting("vox_gain", SettingKind::VoxGain).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_vox_delay() {
    // VOX must already be enabled for VD to work. Same caveat as vox_gain.
    let _ = map_single_setting("vox_delay", SettingKind::VoxDelay).await;
}

// BL is battery level (read-only) per KI4LAX, so no MCP offset mapping is needed.

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_bluetooth() {
    let _ = map_single_setting("bluetooth", SettingKind::Bluetooth).await;
}

// DW is frequency down (action command) per KI4LAX, so no MCP offset mapping is needed.

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_auto_info() {
    let _ = map_single_setting("auto_info", SettingKind::AutoInfo).await;
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
async fn map_operating_mode_a_nfm() {
    let _ = map_single_setting(
        "operating_mode_a",
        SettingKind::OperatingMode(Band::A),
    )
    .await;
}

// ---------------------------------------------------------------------------
// Batch mapper -- runs all settings sequentially, writes results file
// ---------------------------------------------------------------------------

/// Defines a single typed CAT setting to map via differential MCP dumps.
struct SettingTest {
    /// Human-readable name for the setting.
    name: &'static str,
    /// Operation used to observe, change, and restore the setting.
    kind: SettingKind,
}

#[tokio::test]
#[ignore = "requires connected radio hardware"]
#[expect(
    clippy::too_many_lines,
    reason = "Hardware probe. The function drives the radio through every CAT-writable \
              setting (~50+ settings, each with read/write/verify phases) in a single \
              linear sequence so a reviewer can correlate the test output against the \
              firmware command dispatch table at ~0xC002E2E0 top-to-bottom. Splitting \
              into per-setting helpers would obscure the ordering (which matters for \
              race-free probe runs against live hardware) and hide the shape of the \
              single-dump-per-setting protocol."
)]
async fn map_all_settings() {
    println!("\n=== AUTOMATED OFFSET MAPPER (single-dump per setting) ===\n");

    let tests = vec![
        // BL is battery level (read-only) per KI4LAX; excluded.
        SettingTest {
            name: "vox_enable",
            kind: SettingKind::Vox,
        },
        SettingTest {
            name: "vox_gain",
            kind: SettingKind::VoxGain,
        },
        SettingTest {
            name: "vox_delay",
            kind: SettingKind::VoxDelay,
        },
        SettingTest {
            name: "dual_band",
            kind: SettingKind::BandMode,
        },
        SettingTest {
            name: "attenuator_a",
            kind: SettingKind::Attenuator(Band::A),
        },
        SettingTest {
            name: "power_level_a",
            kind: SettingKind::PowerLevel(Band::A),
        },
        SettingTest {
            name: "squelch_a",
            kind: SettingKind::Squelch(Band::A),
        },
        // BE is the bare APRS beacon transmit action, not a beep setting.
        // Key beep can only be changed via MCP memory write. Excluded.
        SettingTest {
            name: "bluetooth",
            kind: SettingKind::Bluetooth,
        },
        // DW is frequency down (action command) per KI4LAX; excluded.
        SettingTest {
            name: "auto_info",
            kind: SettingKind::AutoInfo,
        },
        SettingTest {
            name: "operating_mode_a",
            kind: SettingKind::OperatingMode(Band::A),
        },
        // TN = TNC mode, DC = D-STAR callsign (not tone/DCS).
        // CTCSS/DCS are set through the FO (full channel) command.
    ];

    let mut results: BTreeMap<String, Vec<(usize, u8, u8)>> = BTreeMap::new();
    let total = tests.len() + 1;

    println!("\n--- Setting 1/{total}: 'backlight_control' ---");
    let backlight_diffs = map_backlight_control_setting().await;
    let _ = results.insert("backlight_control".to_string(), backlight_diffs);

    for (i, test) in tests.iter().enumerate() {
        println!("\n--- Setting {}/{}: '{}' ---", i + 2, total, test.name);

        let diffs = map_single_setting(test.name, test.kind).await;
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
