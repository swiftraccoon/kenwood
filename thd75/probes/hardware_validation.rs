//! Comprehensive hardware validation against a live TH-D75.
//!
//! Exercises every command group and prints typed responses alongside the
//! corresponding offline wire encoding for capture.
//!
//! This archival probe source is not registered as a Cargo target. Before a
//! hardware run, review it against `docs/audit/probe_queue.md`, promote the
//! reviewed copy to an explicit test target, and run that target serially.

mod firmware_guard;

use kenwood_thd75::error::Error;
use kenwood_thd75::protocol::{self, Command};
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::SerialTransport;
use kenwood_thd75::types::*;

/// Helper: connect to the first discovered TH-D75.
fn connect() -> Radio<SerialTransport> {
    let ports = SerialTransport::discover_usb().expect("USB discovery failed");
    assert!(!ports.is_empty(), "No TH-D75 found; connect radio via USB");
    let transport = SerialTransport::open(&ports[0].port_name).expect("Failed to open serial port");
    Radio::new(transport)
}

/// Record offline wire metadata around one typed radio operation.
async fn probe<T, F>(cmd: Command, operation: F) -> Result<T, Error>
where
    T: std::fmt::Debug,
    F: std::future::Future<Output = Result<T, Error>>,
{
    let cmd_name = protocol::command_name(&cmd);
    let wire = protocol::serialize(&cmd);
    let wire_without_terminator = wire.strip_suffix(b"\r").unwrap_or(&wire);
    let wire_str = String::from_utf8_lossy(wire_without_terminator);
    let result = operation.await;
    match &result {
        Ok(resp) => println!("  {cmd_name} OK | sent: {wire_str} | response: {resp:?}"),
        Err(e) => println!("  {cmd_name} ERR | sent: {wire_str} | error: {e}"),
    }
    result
}

/// Determine the exact firmware identity before any stock bare `GM`/`GW` probe.
async fn exact_firmware_version(radio: &mut Radio<SerialTransport>) -> Option<FirmwareIdentity> {
    match probe(Command::GetFirmwareVersion, radio.get_firmware_version()).await {
        Ok(version) => Some(version),
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
    let mut radio = connect();
    println!("\n=== CORE COMMANDS ===");

    // ID: Radio identification
    let _ = probe(Command::GetRadioId, radio.identify()).await;

    // FV: Firmware version
    let _ = probe(Command::GetFirmwareVersion, radio.get_firmware_version()).await;

    // PS: Power status
    let _ = probe(Command::GetPowerStatus, radio.get_power_status()).await;

    // BE: Send beacon (DANGEROUS: transmits; skipped in normal testing)
    // Intentionally do not call `radio.transmit_aprs_beacon()` here: it emits RF.

    // FQ: Quick frequency read (Band A)
    let _ = probe(
        Command::GetFrequency { band: Band::A },
        radio.get_frequency(Band::A),
    )
    .await;

    // FQ: Quick frequency read (Band B)
    let _ = probe(
        Command::GetFrequency { band: Band::B },
        radio.get_frequency(Band::B),
    )
    .await;

    // FO: Full frequency + settings (Band A)
    let _ = probe(
        Command::GetFrequencyFull { band: Band::A },
        radio.get_frequency_full(Band::A),
    )
    .await;

    // FO: Full frequency + settings (Band B)
    let _ = probe(
        Command::GetFrequencyFull { band: Band::B },
        radio.get_frequency_full(Band::B),
    )
    .await;

    // PC: Power level (Band A)
    let _ = probe(
        Command::GetPowerLevel { band: Band::A },
        radio.get_power_level(Band::A),
    )
    .await;

    // PC: Power level (Band B)
    let _ = probe(
        Command::GetPowerLevel { band: Band::B },
        radio.get_power_level(Band::B),
    )
    .await;

    // FR: Frequency range
    let _ = probe(Command::GetFmRadio, radio.get_fm_radio()).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// VFO commands (AG, SQ, SM, MD, FS, FT, SH, RA)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_vfo_commands() {
    let mut radio = connect();
    println!("\n=== VFO COMMANDS ===");

    // AG: AF Gain (bare read)
    let _ = probe(Command::GetAfGain, radio.get_af_gain()).await;

    // SQ: Squelch
    let _ = probe(
        Command::GetSquelch { band: Band::A },
        radio.get_squelch(Band::A),
    )
    .await;
    let _ = probe(
        Command::GetSquelch { band: Band::B },
        radio.get_squelch(Band::B),
    )
    .await;

    // SM: S-meter
    let _ = probe(
        Command::GetSmeter { band: Band::A },
        radio.get_smeter(Band::A),
    )
    .await;
    let _ = probe(
        Command::GetSmeter { band: Band::B },
        radio.get_smeter(Band::B),
    )
    .await;

    // MD: Mode
    let _ = probe(
        Command::GetOperatingMode { band: Band::A },
        radio.get_operating_mode(Band::A),
    )
    .await;
    let _ = probe(
        Command::GetOperatingMode { band: Band::B },
        radio.get_operating_mode(Band::B),
    )
    .await;

    // FS: Fine step (bare read)
    let _ = probe(Command::GetFineStep, radio.get_fine_step()).await;

    // FT: Function type (bare read, no band)
    let _ = probe(Command::GetFineTune, radio.get_fine_tune()).await;

    // SH: Filter width (by receiver mode)
    let _ = probe(
        Command::GetFilterWidth {
            mode: FilterMode::Ssb,
        },
        radio.get_filter_width(FilterMode::Ssb),
    )
    .await;
    let _ = probe(
        Command::GetFilterWidth {
            mode: FilterMode::Cw,
        },
        radio.get_filter_width(FilterMode::Cw),
    )
    .await;
    let _ = probe(
        Command::GetFilterWidth {
            mode: FilterMode::Am,
        },
        radio.get_filter_width(FilterMode::Am),
    )
    .await;

    // RA: Attenuator
    let _ = probe(
        Command::GetAttenuator { band: Band::A },
        radio.get_attenuator(Band::A),
    )
    .await;
    let _ = probe(
        Command::GetAttenuator { band: Band::B },
        radio.get_attenuator(Band::B),
    )
    .await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Control commands (AI, BY, DL, DW, LC, BL, VX, VG, VD, IO)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_control_commands() {
    let mut radio = connect();
    println!("\n=== CONTROL COMMANDS ===");

    // BY: Busy status
    let _ = probe(Command::GetBusy { band: Band::A }, radio.get_busy(Band::A)).await;
    let _ = probe(Command::GetBusy { band: Band::B }, radio.get_busy(Band::B)).await;

    // DL: Dual band display
    let _ = probe(Command::GetBandMode, radio.get_band_mode()).await;

    // DW: Frequency Down (action, not probed; it would change frequency)

    // LC: LCD backlight control
    let _ = probe(Command::GetBacklightControl, radio.get_backlight_control()).await;

    // BL: Battery Level
    let _ = probe(Command::GetBatteryLevel, radio.get_battery_level()).await;

    // VX: VOX
    let _ = probe(Command::GetVox, radio.get_vox()).await;

    // VG: VOX gain
    let _ = probe(Command::GetVoxGain, radio.get_vox_gain()).await;

    // VD: VOX delay
    let _ = probe(Command::GetVoxDelay, radio.get_vox_delay()).await;

    // IO: I/O port
    let _ = probe(Command::GetUsbAudioOutput, radio.get_usb_audio_output()).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// TNC / D-STAR Callsign / Clock commands (TN, DC, RT)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_tnc_dstar_clock_commands() {
    let mut radio = connect();
    println!("\n=== TNC / D-STAR / CLOCK COMMANDS ===");

    // TN: TNC mode (bare read)
    let _ = probe(Command::GetTncMode, radio.get_tnc_mode()).await;

    // DC: D-STAR callsign slots 1-6
    let slot_1 = DstarSlot::new(1).expect("slot 1 is valid");
    let _ = probe(
        Command::GetDstarCallsign { slot: slot_1 },
        radio.get_dstar_callsign(slot_1),
    )
    .await;
    let slot_2 = DstarSlot::new(2).expect("slot 2 is valid");
    let _ = probe(
        Command::GetDstarCallsign { slot: slot_2 },
        radio.get_dstar_callsign(slot_2),
    )
    .await;

    // RT: Real-time clock (bare read)
    let _ = probe(Command::GetRealTimeClock, radio.get_real_time_clock()).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Memory commands (ME, MR)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_memory_commands() {
    let mut radio = connect();
    println!("\n=== MEMORY COMMANDS ===");

    // ME: Read memory channel 0
    let channel_0 = RegularChannel::new(0).expect("channel 0 is valid");
    let _ = probe(
        Command::GetMemoryChannel {
            selector: channel_0.into(),
        },
        radio.get_regular_channel_record(channel_0),
    )
    .await;

    // ME: Read memory channel 1
    let channel_1 = RegularChannel::new(1).expect("channel 1 is valid");
    let _ = probe(
        Command::GetMemoryChannel {
            selector: channel_1.into(),
        },
        radio.get_regular_channel_record(channel_1),
    )
    .await;

    // MR: Recall memory channel 0 on band A
    let _ = probe(
        Command::RecallMemoryChannel {
            band: Band::A,
            selector: channel_0.into(),
        },
        radio.recall_channel(Band::A, channel_0),
    )
    .await;

    // MR: Recall memory channel 1 on band A
    let _ = probe(
        Command::RecallMemoryChannel {
            band: Band::A,
            selector: channel_1.into(),
        },
        radio.recall_channel(Band::A, channel_1),
    )
    .await;

    let _ = radio.disconnect().await;
}

// ============================================================
// APRS commands (AS, AE, PT, MS)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_aprs_commands() {
    let mut radio = connect();
    println!("\n=== APRS COMMANDS ===");

    let _ = probe(Command::GetPacketDataRate, radio.get_packet_data_rate()).await;
    let _ = probe(Command::GetSerialInfo, radio.get_serial_information()).await;
    let _ = probe(Command::GetBeaconMode, radio.get_beacon_mode()).await;
    let _ = probe(
        Command::GetMyPositionSelection,
        radio.get_my_position_selection(),
    )
    .await;

    let _ = radio.disconnect().await;
}

// ============================================================
// D-STAR commands (DS, CS, GW)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_dstar_commands() {
    let mut radio = connect();
    println!("\n=== D-STAR COMMANDS ===");
    let firmware_version = exact_firmware_version(&mut radio).await;

    let _ = probe(Command::GetDstarSlot, radio.get_dstar_slot()).await;
    match firmware_guard::require_stock_bare_probe(
        "GW",
        firmware_version.as_ref().map(FirmwareIdentity::as_str),
    ) {
        Ok(()) => {
            let _ = probe(Command::GetGateway, radio.read_gateway()).await;
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
    let mut radio = connect();
    println!("\n=== GPS COMMANDS ===");
    let firmware_version = exact_firmware_version(&mut radio).await;

    let _ = probe(Command::GetGpsSettings, radio.get_gps_settings()).await;
    match firmware_guard::require_stock_bare_probe(
        "GM",
        firmware_version.as_ref().map(FirmwareIdentity::as_str),
    ) {
        Ok(()) => {
            let _ = probe(Command::GetGpsMode, radio.read_gps_mode()).await;
        }
        Err(diagnostic) => println!("  {diagnostic}"),
    }
    let _ = probe(Command::GetGpsSentences, radio.get_gps_sentences()).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// System commands (BT, SD)
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_system_commands() {
    let mut radio = connect();
    println!("\n=== SYSTEM COMMANDS ===");

    let _ = probe(Command::GetBluetooth, radio.get_bluetooth()).await;
    let _ = probe(Command::GetSdCard, radio.get_sd_status()).await;

    let _ = radio.disconnect().await;
}

// ============================================================
// Scan commands (SR, SF), plus BS antenna selection
// ============================================================

#[tokio::test]
#[ignore]
async fn hw_scan_commands() {
    let mut radio = connect();
    println!("\n=== SCAN COMMANDS ===");

    // SR is write-only on D75 (bare `SR\r` returns `?`)
    let _ = probe(
        Command::GetStepSize { band: Band::A },
        radio.get_step_size(Band::A),
    )
    .await;
    let _ = probe(Command::GetAntennaInput, radio.get_antenna_input()).await;

    let _ = radio.disconnect().await;
}
