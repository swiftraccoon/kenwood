//! Comprehensive hardware validation against a live TH-D75.
//!
//! Exercises every command group and prints raw responses for capture.
//! Run with: `cargo test --test hardware_validation -- --ignored --nocapture --test-threads=1`

mod firmware_guard;

use kenwood_thd75::error::Error;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

/// Helper: connect to the first discovered TH-D75.
async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found; connect radio via USB");
    let transport = SerialTransport::open(&ports[0].port_name, SerialTransport::DEFAULT_BAUD)
        .expect("Failed to open serial port");
    Radio::connect(transport).await.expect("Failed to connect")
}

/// Helper: execute a raw command and print the result.
async fn probe(radio: &mut Radio<SerialTransport>, cmd: Command) -> Result<Response, Error> {
    let cmd_name = protocol::command_name(&cmd);
    let wire = protocol::serialize(&cmd);
    let wire_str = String::from_utf8_lossy(&wire[..wire.len() - 1]); // strip \r
    let result = radio.execute(cmd).await;
    match &result {
        Ok(resp) => println!("  {cmd_name} OK | sent: {wire_str} | response: {resp:?}"),
        Err(e) => println!("  {cmd_name} ERR | sent: {wire_str} | error: {e}"),
    }
    result
}

/// Determine the exact firmware identity before any stock bare `GM`/`GW` probe.
async fn exact_firmware_version(radio: &mut Radio<SerialTransport>) -> Option<String> {
    match probe(radio, Command::GetFirmwareVersion).await {
        Ok(Response::FirmwareVersion { version }) => Some(version),
        Ok(other) => {
            println!("  FV ERR | unexpected response while authorizing stock probes: {other:?}");
            None
        }
        Err(error) => {
            println!("  FV ERR | cannot authorize stock probes: {error}");
            None
        }
    }
}

// ============================================================
// Core commands (FV, ID, PS, BE, FQ, FO, PC, MD, VM, FR)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_core_commands() {
    let mut radio = connect().await;
    println!("\n=== CORE COMMANDS ===");

    // ID: Radio identification
    let _ = probe(&mut radio, Command::GetRadioId).await;

    // FV: Firmware version
    let _ = probe(&mut radio, Command::GetFirmwareVersion).await;

    // PS: Power status
    let _ = probe(&mut radio, Command::GetPowerStatus).await;

    // BE: Send beacon (DANGEROUS: transmits; skipped in normal testing)
    // let _ = probe(&mut radio, Command::SendBeacon).await;

    // FQ: Quick frequency read (Band A)
    let _ = probe(&mut radio, Command::GetFrequency { band: Band::A }).await;

    // FQ: Quick frequency read (Band B)
    let _ = probe(&mut radio, Command::GetFrequency { band: Band::B }).await;

    // FO: Full frequency + settings (Band A)
    let _ = probe(&mut radio, Command::GetFrequencyFull { band: Band::A }).await;

    // FO: Full frequency + settings (Band B)
    let _ = probe(&mut radio, Command::GetFrequencyFull { band: Band::B }).await;

    // PC: Power level (Band A)
    let _ = probe(&mut radio, Command::GetPowerLevel { band: Band::A }).await;

    // PC: Power level (Band B)
    let _ = probe(&mut radio, Command::GetPowerLevel { band: Band::B }).await;

    // FR: Frequency range
    let _ = probe(&mut radio, Command::GetFmRadio).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// VFO commands (AG, SQ, SM, MD, FS, FT, SH, RA)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_vfo_commands() {
    let mut radio = connect().await;
    println!("\n=== VFO COMMANDS ===");

    // AG: AF Gain (bare read)
    let _ = probe(&mut radio, Command::GetAfGain).await;

    // SQ: Squelch
    let _ = probe(&mut radio, Command::GetSquelch { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetSquelch { band: Band::B }).await;

    // SM: S-meter
    let _ = probe(&mut radio, Command::GetSmeter { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetSmeter { band: Band::B }).await;

    // MD: Mode
    let _ = probe(&mut radio, Command::GetMode { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetMode { band: Band::B }).await;

    // FS: Fine step (bare read)
    let _ = probe(&mut radio, Command::GetFineStep).await;

    // FT: Function type (bare read, no band)
    let _ = probe(&mut radio, Command::GetFunctionType).await;

    // SH: Filter width (by receiver mode)
    let _ = probe(
        &mut radio,
        Command::GetFilterWidth {
            mode: FilterMode::Ssb,
        },
    )
    .await;
    let _ = probe(
        &mut radio,
        Command::GetFilterWidth {
            mode: FilterMode::Cw,
        },
    )
    .await;
    let _ = probe(
        &mut radio,
        Command::GetFilterWidth {
            mode: FilterMode::Am,
        },
    )
    .await;

    // RA: Attenuator
    let _ = probe(&mut radio, Command::GetAttenuator { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetAttenuator { band: Band::B }).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Control commands (AI, BY, DL, DW, LC, BL, VX, VG, VD, IO)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_control_commands() {
    let mut radio = connect().await;
    println!("\n=== CONTROL COMMANDS ===");

    // BY: Busy status
    let _ = probe(&mut radio, Command::GetBusy { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetBusy { band: Band::B }).await;

    // DL: Dual band display
    let _ = probe(&mut radio, Command::GetDualBand).await;

    // DW: Frequency Down (action, not probed; it would change frequency)

    // LC: LCD backlight control
    let _ = probe(&mut radio, Command::GetBacklightControl).await;

    // BL: Battery Level
    let _ = probe(&mut radio, Command::GetBatteryLevel).await;

    // VX: VOX
    let _ = probe(&mut radio, Command::GetVox).await;

    // VG: VOX gain
    let _ = probe(&mut radio, Command::GetVoxGain).await;

    // VD: VOX delay
    let _ = probe(&mut radio, Command::GetVoxDelay).await;

    // IO: I/O port
    let _ = probe(&mut radio, Command::GetIoPort).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// TNC / D-STAR Callsign / Clock commands (TN, DC, RT)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_tnc_dstar_clock_commands() {
    let mut radio = connect().await;
    println!("\n=== TNC / D-STAR / CLOCK COMMANDS ===");

    // TN: TNC mode (bare read)
    let _ = probe(&mut radio, Command::GetTncMode).await;

    // DC: D-STAR callsign slots 1-6
    let _ = probe(
        &mut radio,
        Command::GetDstarCallsign {
            slot: DstarSlot::new(1).expect("slot 1 is valid"),
        },
    )
    .await;
    let _ = probe(
        &mut radio,
        Command::GetDstarCallsign {
            slot: DstarSlot::new(2).expect("slot 2 is valid"),
        },
    )
    .await;

    // RT: Real-time clock (bare read)
    let _ = probe(&mut radio, Command::GetRealTimeClock).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Memory commands (ME, MR, 0M)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_memory_commands() {
    let mut radio = connect().await;
    println!("\n=== MEMORY COMMANDS ===");

    // ME: Read memory channel 0
    let _ = probe(
        &mut radio,
        Command::GetMemoryChannel {
            selector: MemorySelector::try_from(0_u16).unwrap(),
        },
    )
    .await;

    // ME: Read memory channel 1
    let _ = probe(
        &mut radio,
        Command::GetMemoryChannel {
            selector: MemorySelector::try_from(1_u16).unwrap(),
        },
    )
    .await;

    // MR: Recall memory channel 0 on band A
    let _ = probe(
        &mut radio,
        Command::RecallMemoryChannel {
            band: Band::A,
            selector: MemorySelector::try_from(0_u16).unwrap(),
        },
    )
    .await;

    // MR: Recall memory channel 1 on band A
    let _ = probe(
        &mut radio,
        Command::RecallMemoryChannel {
            band: Band::A,
            selector: MemorySelector::try_from(1_u16).unwrap(),
        },
    )
    .await;

    // 0M: Enter programming mode (DANGEROUS; skipped in normal testing)
    // let _ = probe(&mut radio, Command::EnterProgrammingMode).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// APRS commands (AS, AE, PT, MS)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_aprs_commands() {
    let mut radio = connect().await;
    println!("\n=== APRS COMMANDS ===");

    let _ = probe(&mut radio, Command::GetTncBaud).await;
    let _ = probe(&mut radio, Command::GetSerialInfo).await;
    let _ = probe(&mut radio, Command::GetBeaconType).await;
    let _ = probe(&mut radio, Command::GetMyPositionSelection).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// D-STAR commands (DS, CS, GW)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_dstar_commands() {
    let mut radio = connect().await;
    println!("\n=== D-STAR COMMANDS ===");
    let firmware_version = exact_firmware_version(&mut radio).await;

    let _ = probe(&mut radio, Command::GetDstarSlot).await;
    match firmware_guard::require_stock_bare_probe("GW", firmware_version.as_deref()) {
        Ok(()) => {
            let _ = probe(&mut radio, Command::GetGateway).await;
        }
        Err(diagnostic) => println!("  {diagnostic}"),
    }

    let _ = radio.disconnect().await;
}

// ============================================================
// GPS commands (GP, GM, GS)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_gps_commands() {
    let mut radio = connect().await;
    println!("\n=== GPS COMMANDS ===");
    let firmware_version = exact_firmware_version(&mut radio).await;

    let _ = probe(&mut radio, Command::GetGpsConfig).await;
    match firmware_guard::require_stock_bare_probe("GM", firmware_version.as_deref()) {
        Ok(()) => {
            let _ = probe(&mut radio, Command::GetGpsMode).await;
        }
        Err(diagnostic) => println!("  {diagnostic}"),
    }
    let _ = probe(&mut radio, Command::GetGpsSentences).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// System commands (BT, SD)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_system_commands() {
    let mut radio = connect().await;
    println!("\n=== SYSTEM COMMANDS ===");

    let _ = probe(&mut radio, Command::GetBluetooth).await;
    let _ = probe(&mut radio, Command::GetSdCard).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Scan commands (SR, SF), plus BS antenna selection
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_scan_commands() {
    let mut radio = connect().await;
    println!("\n=== SCAN COMMANDS ===");

    // SR is write-only on D75 (bare `SR\r` returns `?`)
    let _ = probe(&mut radio, Command::GetStepSize { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetBarAntenna).await;

    let _ = radio.disconnect().await;
}
