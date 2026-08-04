//! Integration tests for the 14 control and bare-action protocol commands:
//! AI, BY, DL, DW, UP, BE, RX, TX, LC, IO, BL, VD, VG, VX.

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use ::aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use encoding_rs as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// AI -- Auto-info (bare read and boolean write)
// ============================================================================

#[test]
fn serialize_ai_read() {
    assert_eq!(protocol::serialize(&Command::GetAutoInfo), b"AI\r");
}

#[test]
fn serialize_ai_on() {
    assert_eq!(
        protocol::serialize(&Command::SetAutoInfo { enabled: true }),
        b"AI 1\r"
    );
}

#[test]
fn serialize_ai_off() {
    assert_eq!(
        protocol::serialize(&Command::SetAutoInfo { enabled: false }),
        b"AI 0\r"
    );
}

#[test]
fn parse_ai_response_on() -> TestResult {
    let r = protocol::parse(b"AI 1")?;
    let Response::AutoInfo { enabled } = r else {
        return Err(format!("expected AutoInfo, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_ai_response_off() -> TestResult {
    let r = protocol::parse(b"AI 0")?;
    let Response::AutoInfo { enabled } = r else {
        return Err(format!("expected AutoInfo, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}

#[test]
fn parse_ai_bare_ack_is_not_disabled_state() -> TestResult {
    let response = protocol::parse(b"AI")?;
    assert!(matches!(response, Response::AutoInfoAck));
    Ok(())
}

#[test]
fn parse_ai_rejects_non_boolean_state() {
    assert!(protocol::parse(b"AI 2").is_err());
}

// ============================================================================
// BY -- Busy (read-only, band + boolean)
// ============================================================================

#[test]
fn serialize_by_read() {
    assert_eq!(
        protocol::serialize(&Command::GetBusy { band: Band::A }),
        b"BY 0\r"
    );
}

#[test]
fn parse_by_busy() -> TestResult {
    let r = protocol::parse(b"BY 0,1")?;
    let Response::Busy { band, busy } = r else {
        return Err(format!("expected Busy, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert!(busy);
    Ok(())
}

#[test]
fn parse_by_not_busy() -> TestResult {
    let r = protocol::parse(b"BY 1,0")?;
    let Response::Busy { band, busy } = r else {
        return Err(format!("expected Busy, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert!(!busy);
    Ok(())
}

#[test]
fn parse_by_rejects_non_boolean() {
    assert!(protocol::parse(b"BY 0,2").is_err());
}

// ============================================================================
// DL -- Single-band or dual-band display
// ============================================================================

#[test]
fn serialize_dl_read() {
    assert_eq!(protocol::serialize(&Command::GetBandMode), b"DL\r");
}

#[test]
fn serialize_dl_single_band() {
    assert_eq!(
        protocol::serialize(&Command::SetBandMode {
            mode: BandMode::Single,
        }),
        b"DL 1\r"
    );
}

#[test]
fn parse_dl_single_band() -> TestResult {
    let r = protocol::parse(b"DL 1")?;
    let Response::BandMode { mode } = r else {
        return Err(format!("expected BandMode, got {r:?}").into());
    };
    assert_eq!(mode, BandMode::Single);
    Ok(())
}

#[test]
fn parse_dl_dual_band() -> TestResult {
    let r = protocol::parse(b"DL 0")?;
    let Response::BandMode { mode } = r else {
        return Err(format!("expected BandMode, got {r:?}").into());
    };
    assert_eq!(mode, BandMode::Dual);
    Ok(())
}

// ============================================================================
// DW -- Frequency Down (step frequency down, counterpart to UP)
// ============================================================================

#[test]
fn serialize_dw_band_a() {
    assert_eq!(protocol::serialize(&Command::FrequencyDown), b"DW\r");
}

#[test]
fn serialize_dw_band_b() {
    assert_eq!(protocol::serialize(&Command::FrequencyDown), b"DW\r");
}

#[test]
fn parse_dw_response() -> TestResult {
    let r = protocol::parse(b"DW")?;
    let Response::FrequencyDownAck = r else {
        return Err(format!("expected FrequencyDown, got {r:?}").into());
    };
    Ok(())
}

#[test]
fn parse_dw_rejects_payload() {
    assert!(protocol::parse(b"DW 0").is_err());
}

#[test]
fn parse_up_bare_response() -> TestResult {
    let response = protocol::parse(b"UP")?;
    assert!(matches!(response, Response::FrequencyUpAck));
    Ok(())
}

#[test]
fn parse_up_rejects_payload() {
    assert!(protocol::parse(b"UP 0").is_err());
}

// ============================================================================
// BE -- transmit one APRS beacon (bare on-air action)
// ============================================================================

#[test]
fn serialize_beacon_transmit_action() {
    assert_eq!(protocol::serialize(&Command::TransmitAprsBeacon), b"BE\r");
}

#[test]
fn parse_beacon_transmit_acknowledgement() -> TestResult {
    assert!(matches!(
        protocol::parse(b"BE")?,
        Response::AprsBeaconTransmitAck
    ));
    Ok(())
}

#[test]
fn parse_beacon_transmit_rejects_a_payload() {
    assert!(protocol::parse(b"BE 0").is_err());
}

// ============================================================================
// RX -- Receive (bare action)
// ============================================================================

#[test]
fn serialize_rx() {
    assert_eq!(protocol::serialize(&Command::Receive), b"RX\r");
}

#[test]
fn parse_rx_echo() -> TestResult {
    assert!(matches!(protocol::parse(b"RX")?, Response::ReceiveAck));
    Ok(())
}

#[test]
fn parse_rx_rejects_payload() {
    assert!(protocol::parse(b"RX 0").is_err());
}

// ============================================================================
// TX -- Transmit (bare action)
// ============================================================================

#[test]
fn serialize_tx() {
    assert_eq!(protocol::serialize(&Command::Transmit), b"TX\r");
}

#[test]
fn parse_tx_echo() -> TestResult {
    assert!(matches!(protocol::parse(b"TX")?, Response::TransmitAck));
    Ok(())
}

#[test]
fn parse_tx_rejects_payload() {
    assert!(protocol::parse(b"TX 0").is_err());
}

// ============================================================================
// LC -- LCD backlight control (0-3)
// ============================================================================

#[test]
fn serialize_lc_read() {
    assert_eq!(protocol::serialize(&Command::GetBacklightControl), b"LC\r");
}

#[test]
fn serialize_lc_auto() {
    assert_eq!(
        protocol::serialize(&Command::SetBacklightControl {
            mode: BacklightControl::Auto
        }),
        b"LC 2\r"
    );
}

#[test]
fn serialize_lc_auto_dc_in() {
    assert_eq!(
        protocol::serialize(&Command::SetBacklightControl {
            mode: BacklightControl::AutoDcIn
        }),
        b"LC 3\r"
    );
}

#[test]
fn parse_lc_on() -> TestResult {
    let r = protocol::parse(b"LC 1")?;
    let Response::BacklightControl { mode } = r else {
        return Err(format!("expected BacklightControl, got {r:?}").into());
    };
    assert_eq!(mode, BacklightControl::On);
    Ok(())
}

#[test]
fn parse_lc_manual() -> TestResult {
    let r = protocol::parse(b"LC 0")?;
    let Response::BacklightControl { mode } = r else {
        return Err(format!("expected BacklightControl, got {r:?}").into());
    };
    assert_eq!(mode, BacklightControl::Manual);
    Ok(())
}

#[test]
fn parse_lc_rejects_out_of_range_mode() {
    assert!(protocol::parse(b"LC 4").is_err());
}

// ============================================================================
// IO -- USB audio output selection
// ============================================================================

#[test]
fn serialize_io_read() {
    assert_eq!(protocol::serialize(&Command::GetUsbAudioOutput), b"IO\r");
}

#[test]
fn parse_io_response() -> TestResult {
    let r = protocol::parse(b"IO 0")?;
    let Response::UsbAudioOutput { output } = r else {
        return Err(format!("expected UsbAudioOutput, got {r:?}").into());
    };
    assert_eq!(output, UsbAudioOutput::Audio);
    Ok(())
}

// ============================================================================
// BL -- Battery Level (read-only: 0=Empty, 1=1/3, 2=2/3, 3=Full)
// ============================================================================

#[test]
fn serialize_bl_read() {
    assert_eq!(protocol::serialize(&Command::GetBatteryLevel), b"BL\r");
}

#[test]
fn parse_bl_response() -> TestResult {
    let r = protocol::parse(b"BL 3")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Full);
    Ok(())
}

#[test]
fn parse_bl_empty() -> TestResult {
    let r = protocol::parse(b"BL 0")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Empty);
    Ok(())
}

#[test]
fn parse_bl_charging() -> TestResult {
    let r = protocol::parse(b"BL 4")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Charging);
    Ok(())
}

#[test]
fn parse_bl_accepts_a_valid_unsolicited_band_prefix() -> TestResult {
    let response = protocol::parse(b"BL 0,3")?;
    assert!(matches!(
        response,
        Response::BatteryLevel {
            level: BatteryLevel::Full
        }
    ));
    Ok(())
}

#[test]
fn parse_bl_rejects_an_unknown_or_malformed_prefix() {
    for frame in [b"BL 2,3".as_slice(), b"BL x,3", b"BL ,3", b"BL 0, 3"] {
        assert!(
            protocol::parse(frame).is_err(),
            "malformed BL prefix must be rejected: {frame:?}"
        );
    }
}

#[test]
fn response_envelope_requires_exactly_one_space_before_payload() {
    for frame in [
        b"BL3".as_slice(),
        b"BL  3",
        b"BL\t3",
        b"BL\n3",
        b"bl 3",
        b"B! 3",
        &[b'B', b'L', b' ', 0xFF],
    ] {
        assert!(
            protocol::parse(frame).is_err(),
            "malformed response envelope must be rejected: {frame:?}"
        );
    }
}

#[test]
fn parse_bl_preserves_unqualified_raw_five() -> TestResult {
    let r = protocol::parse(b"BL 5")?;
    let Response::BatteryLevel { level } = r else {
        return Err(format!("expected BatteryLevel, got {r:?}").into());
    };
    assert_eq!(level, BatteryLevel::Unidentified5);
    Ok(())
}

// ============================================================================
// VD -- VOX delay (numeric)
// ============================================================================

#[test]
fn serialize_vd_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxDelay), b"VD\r");
}

#[test]
fn serialize_vd_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetVoxDelay {
            delay: VoxDelay::new(6)?
        }),
        b"VD 6\r"
    );
    Ok(())
}

#[test]
fn parse_vd_response() -> TestResult {
    let r = protocol::parse(b"VD 4")?;
    let Response::VoxDelay { delay } = r else {
        return Err(format!("expected VoxDelay, got {r:?}").into());
    };
    assert_eq!(delay, VoxDelay::new(4)?);
    assert_eq!(delay.as_milliseconds(), 1500);
    Ok(())
}

#[test]
fn parse_vd_rejects_out_of_range_index() {
    assert!(protocol::parse(b"VD 7").is_err());
}

// ============================================================================
// VG -- VOX gain (numeric)
// ============================================================================

#[test]
fn serialize_vg_read() {
    assert_eq!(protocol::serialize(&Command::GetVoxGain), b"VG\r");
}

#[test]
fn serialize_vg_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetVoxGain {
            gain: VoxGain::new(4)?
        }),
        b"VG 4\r"
    );
    Ok(())
}

#[test]
fn parse_vg_response() -> TestResult {
    let r = protocol::parse(b"VG 9")?;
    let Response::VoxGain { gain } = r else {
        return Err(format!("expected VoxGain, got {r:?}").into());
    };
    assert_eq!(gain, VoxGain::new(9)?);
    Ok(())
}

// ============================================================================
// VX -- VOX on/off (boolean)
// ============================================================================

#[test]
fn serialize_vx_read() {
    assert_eq!(protocol::serialize(&Command::GetVox), b"VX\r");
}

#[test]
fn serialize_vx_on() {
    assert_eq!(
        protocol::serialize(&Command::SetVox { enabled: true }),
        b"VX 1\r"
    );
}

#[test]
fn parse_vx_enabled() -> TestResult {
    let r = protocol::parse(b"VX 1")?;
    let Response::Vox { enabled } = r else {
        return Err(format!("expected Vox, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_vx_disabled() -> TestResult {
    let r = protocol::parse(b"VX 0")?;
    let Response::Vox { enabled } = r else {
        return Err(format!("expected Vox, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}
