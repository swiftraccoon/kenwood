//! Comprehensive hardware validation against a live TH-D75.
//!
//! Exercises every command group and prints raw responses for capture.
//! Run with: `cargo test --test hardware_validation -- --ignored --nocapture --test-threads=1`

use kenwood_thd75::error::Error;
use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

/// Helper: connect to the first discovered TH-D75.
async fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found — connect radio via USB");
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

// ============================================================
// Core commands (FV, ID, PS, BE, FQ, FO, PC, MD, VM, FR)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_core_commands() {
    let mut radio = connect().await;
    println!("\n=== CORE COMMANDS ===");

    // ID — Radio identification
    let _ = probe(&mut radio, Command::GetRadioId).await;

    // FV — Firmware version
    let _ = probe(&mut radio, Command::GetFirmwareVersion).await;

    // PS — Power status
    let _ = probe(&mut radio, Command::GetPowerStatus).await;

    // BE — Send beacon (DANGEROUS: transmits — skipped in normal testing)
    // let _ = probe(&mut radio, Command::SendBeacon).await;

    // FQ — Quick frequency read (Band A)
    let _ = probe(&mut radio, Command::GetFrequency { band: Band::A }).await;

    // FQ — Quick frequency read (Band B)
    let _ = probe(&mut radio, Command::GetFrequency { band: Band::B }).await;

    // FO — Full frequency + settings (Band A)
    let _ = probe(&mut radio, Command::GetFrequencyFull { band: Band::A }).await;

    // FO — Full frequency + settings (Band B)
    let _ = probe(&mut radio, Command::GetFrequencyFull { band: Band::B }).await;

    // PC — Power level (Band A)
    let _ = probe(&mut radio, Command::GetPowerLevel { band: Band::A }).await;

    // PC — Power level (Band B)
    let _ = probe(&mut radio, Command::GetPowerLevel { band: Band::B }).await;

    // FR — Frequency range
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

    // AG — AF Gain (bare read)
    let _ = probe(&mut radio, Command::GetAfGain).await;

    // SQ — Squelch
    let _ = probe(&mut radio, Command::GetSquelch { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetSquelch { band: Band::B }).await;

    // SM — S-meter
    let _ = probe(&mut radio, Command::GetSmeter { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetSmeter { band: Band::B }).await;

    // MD — Mode
    let _ = probe(&mut radio, Command::GetMode { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetMode { band: Band::B }).await;

    // FS — Frequency step (band-indexed)
    let _ = probe(&mut radio, Command::GetFrequencyStep { band: Band::A }).await;

    // FT — Function type (bare read, no band)
    let _ = probe(&mut radio, Command::GetFunctionType).await;

    // SH — Filter width (by mode index)
    let _ = probe(&mut radio, Command::GetFilterWidth { mode_index: 0 }).await;
    let _ = probe(&mut radio, Command::GetFilterWidth { mode_index: 1 }).await;
    let _ = probe(&mut radio, Command::GetFilterWidth { mode_index: 2 }).await;

    // RA — Attenuator
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

    // BY — Busy status
    let _ = probe(&mut radio, Command::GetBusy { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetBusy { band: Band::B }).await;

    // DL — Dual band display
    let _ = probe(&mut radio, Command::GetDualBand).await;

    // DW — Dual Watch
    let _ = probe(&mut radio, Command::GetDualWatch).await;

    // LC — Lock/Backlight control
    let _ = probe(&mut radio, Command::GetLock).await;

    // BL — Backlight brightness
    let _ = probe(&mut radio, Command::GetBacklight).await;

    // VX — VOX
    let _ = probe(&mut radio, Command::GetVox).await;

    // VG — VOX gain
    let _ = probe(&mut radio, Command::GetVoxGain).await;

    // VD — VOX delay
    let _ = probe(&mut radio, Command::GetVoxDelay).await;

    // IO — I/O port
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

    // TN — TNC mode (bare read)
    let _ = probe(&mut radio, Command::GetTncMode).await;

    // DC — D-STAR callsign slots 1-6
    let _ = probe(&mut radio, Command::GetDstarCallsign { slot: 1 }).await;
    let _ = probe(&mut radio, Command::GetDstarCallsign { slot: 2 }).await;

    // RT — Real-time clock (bare read)
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

    // ME — Read memory channel 0
    let _ = probe(&mut radio, Command::GetMemoryChannel { channel: 0 }).await;

    // ME — Read memory channel 1
    let _ = probe(&mut radio, Command::GetMemoryChannel { channel: 1 }).await;

    // MR — Recall memory channel 0 on band A
    let _ = probe(
        &mut radio,
        Command::RecallMemoryChannel {
            band: Band::A,
            channel: 0,
        },
    )
    .await;

    // MR — Recall memory channel 1 on band A
    let _ = probe(
        &mut radio,
        Command::RecallMemoryChannel {
            band: Band::A,
            channel: 1,
        },
    )
    .await;

    // 0M — Enter programming mode (DANGEROUS — skipped in normal testing)
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
    let _ = probe(&mut radio, Command::GetPositionSource).await;

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

    let _ = probe(&mut radio, Command::GetDstarSlot).await;
    let _ = probe(&mut radio, Command::GetActiveCallsignSlot).await;
    let _ = probe(&mut radio, Command::GetGateway).await;

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

    let _ = probe(&mut radio, Command::GetGpsConfig).await;
    let _ = probe(&mut radio, Command::GetGpsMode).await;
    let _ = probe(&mut radio, Command::GetGpsSentences).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// System commands (BT, SD, US)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_system_commands() {
    let mut radio = connect().await;
    println!("\n=== SYSTEM COMMANDS ===");

    let _ = probe(&mut radio, Command::GetBluetooth).await;
    let _ = probe(&mut radio, Command::GetSdCard).await;
    let _ = probe(&mut radio, Command::GetUserSettings).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Scan commands (SR, SF, BS)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_scan_commands() {
    let mut radio = connect().await;
    println!("\n=== SCAN COMMANDS ===");

    // SR is write-only on D75 (bare `SR\r` returns `?`)
    let _ = probe(&mut radio, Command::GetScanRange { band: Band::A }).await;
    let _ = probe(&mut radio, Command::GetBandScope { band: Band::A }).await;

    let _ = radio.disconnect().await;
}
