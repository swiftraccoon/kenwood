//! Audit tests that compare our implementation against the KI4LAX CAT
//! command reference (`docs/ki4lax_cat_spec.json`). The spec is transcribed
//! directly from the PDF and must NOT be edited based on our code.
//!
//! Any test failure means our code disagrees with the external reference.
//! Investigate which is correct (spec, code, or hardware) before fixing.

use kenwood_thd75::protocol::{self, Command, Response, command_name};
use kenwood_thd75::types::*;

/// Load the KI4LAX spec and return the parsed JSON.
fn load_spec() -> serde_json::Value {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/ki4lax_cat_spec.json");
    let data = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read spec at {}: {e}", path.display()));
    serde_json::from_str(&data).expect("invalid JSON in spec")
}

/// Collect every unique mnemonic our code maps from `command_name()`.
fn our_mnemonics() -> Vec<&'static str> {
    vec![
        command_name(&Command::GetFrequency { band: Band::A }), // FQ
        command_name(&Command::GetFrequencyFull { band: Band::A }), // FO
        command_name(&Command::GetFirmwareVersion),             // FV
        command_name(&Command::GetPowerStatus),                 // PS
        command_name(&Command::GetRadioId),                     // ID
        command_name(&Command::GetBeep),                        // BE
        command_name(&Command::GetPowerLevel { band: Band::A }), // PC
        command_name(&Command::GetBand),                        // BC
        command_name(&Command::GetVfoMemoryMode { band: Band::A }), // VM
        command_name(&Command::GetFmRadio),                     // FR
        command_name(&Command::GetAfGain),                      // AG
        command_name(&Command::GetSquelch { band: Band::A }),   // SQ
        command_name(&Command::GetSmeter { band: Band::A }),    // SM
        command_name(&Command::GetMode { band: Band::A }),      // MD
        command_name(&Command::GetFineStep),                    // FS
        command_name(&Command::GetFunctionType),                // FT
        command_name(&Command::GetFilterWidth {
            mode: FilterMode::Ssb,
        }), // SH
        command_name(&Command::FrequencyUp { band: Band::A }),  // UP
        command_name(&Command::FrequencyDown { band: Band::A }), // DW
        command_name(&Command::GetAttenuator { band: Band::A }), // RA
        command_name(&Command::SetAutoInfo { enabled: true }),  // AI
        command_name(&Command::GetBusy { band: Band::A }),      // BY
        command_name(&Command::GetDualBand),                    // DL
        command_name(&Command::Receive { band: Band::A }),      // RX
        command_name(&Command::Transmit { band: Band::A }),     // TX
        command_name(&Command::GetLock),                        // LC
        command_name(&Command::GetIoPort),                      // IO
        command_name(&Command::GetBatteryLevel),                // BL
        command_name(&Command::GetVoxDelay),                    // VD
        command_name(&Command::GetVoxGain),                     // VG
        command_name(&Command::GetVox),                         // VX
        command_name(&Command::GetCurrentChannel { band: Band::A }), // MR
        command_name(&Command::GetMemoryChannel { channel: 0 }), // ME
        command_name(&Command::EnterProgrammingMode),           // 0M
        command_name(&Command::GetTncMode),                     // TN
        command_name(&Command::GetDstarCallsign {
            slot: DstarSlot::new(1).unwrap(),
        }), // DC
        command_name(&Command::GetRealTimeClock),               // RT
        command_name(&Command::SetScanResume {
            mode: ScanResumeMethod::TimeOperated,
        }), // SR
        command_name(&Command::GetStepSize { band: Band::A }),  // SF
        command_name(&Command::GetBandScope { band: Band::A }), // BS
        command_name(&Command::GetTncBaud),                     // AS
        command_name(&Command::GetSerialInfo),                  // AE
        command_name(&Command::GetBeaconType),                  // PT
        command_name(&Command::GetPositionSource),              // MS
        command_name(&Command::GetDstarSlot),                   // DS
        command_name(&Command::GetActiveCallsignSlot),          // CS
        command_name(&Command::GetGateway),                     // GW
        command_name(&Command::GetGpsConfig),                   // GP
        command_name(&Command::GetGpsMode),                     // GM
        command_name(&Command::GetGpsSentences),                // GS
        command_name(&Command::GetBluetooth),                   // BT
        command_name(&Command::GetSdCard),                      // SD
        command_name(&Command::GetUserSettings),                // US
        command_name(&Command::GetRadioType),                   // TY
        command_name(&Command::GetMcpStatus),                   // 0E
    ]
}

// ============================================================================
// Mnemonic coverage: every PDF command should exist in our code
// ============================================================================

#[test]
fn all_spec_mnemonics_implemented() {
    let spec = load_spec();
    let commands = spec["commands"].as_object().unwrap();
    let ours = our_mnemonics();

    let mut missing = Vec::new();
    for mnemonic in commands.keys() {
        if !ours.contains(&mnemonic.as_str()) {
            missing.push(mnemonic.clone());
        }
    }

    // FS/SF mnemonic discrepancy resolved via firmware RE:
    // SF = Step Size (band-indexed, 0-11), FS = Fine Step (bare read, 0-3).
    // Our code now matches the firmware dispatch table exactly.
    let documented_conflicts: &[&str] = &[];
    let real_missing: Vec<&String> = missing
        .iter()
        .filter(|m| !documented_conflicts.contains(&m.as_str()))
        .collect();

    assert!(
        real_missing.is_empty(),
        "KI4LAX spec commands missing from our code: {real_missing:?}\n\
         Our mnemonics: {ours:?}"
    );
}

#[test]
fn all_our_mnemonics_are_valid() {
    // Verify every entry in our_mnemonics() is a real 2-char string
    for m in our_mnemonics() {
        assert!(
            m.len() == 2 || m == "0M" || m == "0E",
            "Invalid mnemonic: {m:?}"
        );
    }
}

// ============================================================================
// Commands our code has that the PDF does NOT document
// ============================================================================

#[test]
fn document_commands_beyond_spec() {
    let spec = load_spec();
    let commands = spec["commands"].as_object().unwrap();
    let ours = our_mnemonics();

    let extra: Vec<&&str> = ours
        .iter()
        .filter(|m| !commands.contains_key(**m))
        .collect();

    // These are commands we implement from firmware RE that KI4LAX
    // didn't document. This list must be kept up to date — any NEW
    // undocumented command added should be reviewed.
    let expected_extra = vec![
        "PS", // Power status
        "BE", // Beep (firmware stub)
        "VD", // VOX delay
        "VG", // VOX gain
        "VX", // VOX enable
        "0M", // MCP programming mode
        "DC", // D-STAR callsign
        "BS", // Band scope
        "PT", // Beacon type
        "DS", // D-STAR slot
        "GM", // GPS mode
        "GS", // GPS sentences
        "SD", // SD card
        "US", // User settings
        "0E", // MCP status
        "LC", // Lock (PDF has it but we also add SetLockFull)
    ];

    for m in &extra {
        assert!(
            expected_extra.contains(m),
            "New undocumented command {m} — add to expected_extra or document in spec"
        );
    }
}

// ============================================================================
// Mode table: TABLE D values must match our Mode enum
// ============================================================================

#[test]
fn mode_table_matches_spec() {
    let spec = load_spec();
    let table_d = spec["tables"]["TABLE_D"]["entries"].as_object().unwrap();

    for (index_str, name_val) in table_d {
        let index: u8 = index_str.parse().unwrap();
        let expected_name = name_val.as_str().unwrap();
        let mode = Mode::try_from(index)
            .unwrap_or_else(|_| panic!("Mode index {index} from spec is invalid in our code"));
        assert_eq!(
            mode.to_string(),
            expected_name,
            "Mode {index}: spec says {expected_name}, we say {mode}"
        );
    }

    // Modes 8 (WFM) and 9 (CW-R) are NOT in the KI4LAX spec but
    // confirmed via ARFC-D75 decompilation. Mode 10 should be invalid.
    assert!(
        Mode::try_from(8).is_ok(),
        "Mode 8 (WFM) confirmed by ARFC RE"
    );
    assert!(
        Mode::try_from(9).is_ok(),
        "Mode 9 (CW-R) confirmed by ARFC RE"
    );
    assert!(Mode::try_from(10).is_err(), "Mode 10 should be invalid");
}

// ============================================================================
// Step size table: TABLE C values must match our StepSize enum
// ============================================================================

#[test]
fn step_size_table_matches_spec() {
    let spec = load_spec();
    let table_c = spec["tables"]["TABLE_C"]["entries"].as_object().unwrap();

    for (hex_str, _value) in table_c {
        let index = u8::from_str_radix(hex_str, 16).unwrap();
        assert!(
            StepSize::try_from(index).is_ok(),
            "Step size index 0x{hex_str} ({index}) from spec is invalid in our code"
        );
    }

    // Index 12 (0xC) should be invalid — spec defines 0-B only
    assert!(
        StepSize::try_from(12).is_err(),
        "Step size 12 should be invalid — spec only defines 0-B"
    );
}

// ============================================================================
// Tone table: TABLE A entries must all be valid ToneCode values
// ============================================================================

#[test]
fn tone_table_matches_spec() {
    let spec = load_spec();
    let table_a = spec["tables"]["TABLE_A"]["entries"].as_object().unwrap();

    assert_eq!(table_a.len(), 50, "TABLE A should have 50 entries (0-49)");

    for (index_str, _freq) in table_a {
        let index: u8 = index_str.parse().unwrap();
        assert!(
            ToneCode::new(index).is_ok(),
            "Tone code {index} from spec is invalid in our code"
        );
    }

    // Index 50 = 1750 Hz tone burst (not in KI4LAX spec, confirmed by ARFC RE)
    assert!(
        ToneCode::new(50).is_ok(),
        "Tone code 50 (1750 Hz burst) confirmed by ARFC RE"
    );
    // Index 51 should be invalid
    assert!(ToneCode::new(51).is_err(), "Tone code 51 should be invalid");
}

// ============================================================================
// BL (Battery Level): spec says read-only, values 0-3
// ============================================================================

#[test]
fn bl_is_read_only_per_spec() {
    let spec = load_spec();
    let bl = &spec["commands"]["BL"];
    assert!(bl["write"].is_null(), "BL should be read-only per spec");
}

#[test]
fn bl_values_include_spec_range() {
    // Spec documents 0-3. We additionally support 4 (charging, hardware).
    for raw in 0..=3u8 {
        let frame = format!("BL {raw}");
        let response = protocol::parse(frame.as_bytes()).unwrap();
        let expected = BatteryLevel::try_from(raw).unwrap();
        assert!(
            matches!(response, Response::BatteryLevel { level } if level == expected),
            "BL {raw} should parse as BatteryLevel"
        );
    }
    // Level 4 (charging) is NOT in the spec but observed on hardware
    let response = protocol::parse(b"BL 4").unwrap();
    assert!(
        matches!(response, Response::BatteryLevel { level } if level == BatteryLevel::Charging),
        "BL 4 (charging) should parse — hardware extension beyond spec"
    );
}

// ============================================================================
// DW (Down): spec says write-only, no parameters
// ============================================================================

#[test]
fn dw_is_write_only_per_spec() {
    let spec = load_spec();
    let dw = &spec["commands"]["DW"];
    assert!(dw["read"].is_null(), "DW should be write-only per spec");
}

// ============================================================================
// SQ range: spec says 0-6
// ============================================================================

#[test]
fn sq_range_matches_spec() {
    for raw in 0..=6u8 {
        let frame = format!("SQ 0,{raw}");
        let response = protocol::parse(frame.as_bytes()).unwrap();
        let expected = SquelchLevel::new(raw).unwrap();
        assert!(
            matches!(response, Response::Squelch { band: Band::A, level } if level == expected),
            "SQ 0,{raw} should parse successfully"
        );
    }
}

// ============================================================================
// AG format: spec says 3 chars, range 000-099
// ============================================================================

#[test]
fn ag_write_is_3_digit_per_spec() {
    let bytes = protocol::serialize(&Command::SetAfGain {
        band: Band::A,
        level: AfGainLevel::new(15),
    });
    let wire = String::from_utf8(bytes).unwrap();
    let payload = wire.trim_end_matches('\r').strip_prefix("AG ").unwrap();
    assert_eq!(
        payload.len(),
        3,
        "AG payload should be 3 chars per spec: got '{payload}'"
    );
    assert_eq!(payload, "015", "AG 15 should serialize as '015'");
}

// ============================================================================
// SM range: spec says signal strength 0-5
// ============================================================================

#[test]
fn sm_range_matches_spec() {
    for raw in 0..=5u8 {
        let frame = format!("SM 0,{raw}");
        let response = protocol::parse(frame.as_bytes()).unwrap();
        let expected = SMeterReading::new(raw).unwrap();
        assert!(
            matches!(response, Response::Smeter { band: Band::A, level } if level == expected),
            "SM 0,{raw} should parse successfully"
        );
    }
}

// ============================================================================
// FO field count: spec says 21 fields
// ============================================================================

#[test]
fn fo_has_21_fields_per_spec() {
    let spec = load_spec();
    let expected = spec["commands"]["FO"]["field_count"].as_u64().unwrap();
    assert_eq!(expected, 21);

    let channel = ChannelMemory::default();
    let bytes = protocol::serialize(&Command::SetFrequencyFull {
        band: Band::A,
        channel,
    });
    let wire = String::from_utf8(bytes).unwrap();
    let payload = wire.trim_end_matches('\r').strip_prefix("FO ").unwrap();
    let count = payload.split(',').count();
    assert_eq!(count, 21, "FO should serialize to 21 fields, got {count}");
}

// ============================================================================
// ME field count: spec says 23 fields
// ============================================================================

#[test]
fn me_has_23_fields_per_spec() {
    let spec = load_spec();
    let expected = spec["commands"]["ME"]["field_count"].as_u64().unwrap();
    assert_eq!(expected, 23);
}

// ============================================================================
// SF/FS mnemonic assignment — firmware-verified
// ============================================================================

#[test]
fn sf_fs_mnemonic_firmware_verified() {
    // Firmware dispatch table proves:
    // - SF = Step Size (band-indexed, step index 0-11)
    // - FS = Fine Step (bare read only, value 0-3)

    // FS = Fine Step (bare read, no band parameter)
    assert_eq!(command_name(&Command::GetFineStep), "FS");

    // SF = Step Size (band-indexed)
    assert_eq!(command_name(&Command::GetStepSize { band: Band::A }), "SF");
}
