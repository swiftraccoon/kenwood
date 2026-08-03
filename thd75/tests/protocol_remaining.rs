//! Tests for the remaining protocol groups: Scan, APRS, D-STAR, GPS,
//! and system commands (Bluetooth and SD).

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// `types::*` glob shadows the bare crate name.)
use ::aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
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

// === Scan (SR write-only, SF band-indexed, BS) ===

#[test]
fn serialize_scan_resume_write() {
    assert_eq!(
        protocol::serialize(&Command::SetScanResume {
            mode: ScanResumeMethod::CarrierOperated
        }),
        b"SR 1\r"
    );
}

// SR has no read form on D75 (bare `SR\r` returns `?`).

#[test]
fn serialize_sf_read_band_a() {
    assert_eq!(
        protocol::serialize(&Command::GetStepSize { band: Band::A }),
        b"SF 0\r"
    );
}

#[test]
fn serialize_sf_read_band_b() {
    assert_eq!(
        protocol::serialize(&Command::GetStepSize { band: Band::B }),
        b"SF 1\r"
    );
}

#[test]
fn parse_sf_response() -> TestResult {
    let r = protocol::parse(b"SF 0,0")?;
    let Response::StepSize { band, step } = r else {
        return Err(format!("expected StepSize, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    assert_eq!(step, StepSize::Hz5000);
    Ok(())
}

#[test]
fn parse_sf_response_band_b() -> TestResult {
    let r = protocol::parse(b"SF 1,5")?;
    let Response::StepSize { band, step } = r else {
        return Err(format!("expected StepSize, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
    assert_eq!(step, StepSize::Hz12500);
    Ok(())
}

#[test]
fn serialize_bs_read() {
    assert_eq!(protocol::serialize(&Command::GetBarAntenna), b"BS\r");
}

#[test]
fn serialize_bs_write_bar_antenna() {
    assert_eq!(
        protocol::serialize(&Command::SetBarAntenna { enabled: true }),
        b"BS 1\r"
    );
}

#[test]
fn parse_bs_response() -> TestResult {
    let r = protocol::parse(b"BS 0")?;
    let Response::BarAntenna { enabled } = r else {
        return Err(format!("expected BarAntenna, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}

#[test]
fn parse_bs_response_band_b() -> TestResult {
    let r = protocol::parse(b"BS 1")?;
    let Response::BarAntenna { enabled } = r else {
        return Err(format!("expected BarAntenna, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

// === APRS-related (AS, AE, PT, MS) ===

#[test]
fn serialize_as_read() {
    assert_eq!(protocol::serialize(&Command::GetTncBaud), b"AS\r");
}

#[test]
fn parse_as_response() -> TestResult {
    let r = protocol::parse(b"AS 0")?;
    let Response::TncBaud { rate } = r else {
        return Err(format!("expected TncBaud, got {r:?}").into());
    };
    assert_eq!(rate, TncBaud::Bps1200);
    Ok(())
}

#[test]
fn parse_as_response_9600() -> TestResult {
    let r = protocol::parse(b"AS 1")?;
    let Response::TncBaud { rate } = r else {
        return Err(format!("expected TncBaud, got {r:?}").into());
    };
    assert_eq!(rate, TncBaud::Bps9600);
    Ok(())
}

#[test]
fn serialize_ae_read() {
    assert_eq!(protocol::serialize(&Command::GetSerialInfo), b"AE\r");
}

#[test]
fn parse_ae_response_serial_info() -> TestResult {
    let r = protocol::parse(b"AE C3C10368,K01")?;
    let Response::SerialInfo { serial, model_code } = r else {
        return Err(format!("expected SerialInfo, got {r:?}").into());
    };
    assert_eq!(serial, "C3C10368");
    assert_eq!(model_code, "K01");
    Ok(())
}

#[test]
fn parse_ae_rejects_malformed_shape() {
    assert!(protocol::parse(b"AE C3C10368").is_err());
    assert!(protocol::parse(b"AE SHORT,K01").is_err());
    assert!(protocol::parse(b"AE C3C10368,K001").is_err());
}

#[test]
fn serialize_pt_read() {
    assert_eq!(protocol::serialize(&Command::GetBeaconType), b"PT\r");
}

#[test]
fn parse_pt_response() -> TestResult {
    let r = protocol::parse(b"PT 2")?;
    let Response::BeaconType { mode } = r else {
        return Err(format!("expected BeaconType, got {r:?}").into());
    };
    assert_eq!(mode, BeaconMode::Auto);
    Ok(())
}

#[test]
fn serialize_ms_read() {
    assert_eq!(
        protocol::serialize(&Command::GetMyPositionSelection),
        b"MS\r"
    );
}

#[test]
fn serialize_ms_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetMyPositionSelection {
            selection: MyPositionSelection::new(5)?
        }),
        b"MS 5\r"
    );
    Ok(())
}

#[test]
fn parse_ms_response() -> TestResult {
    let r = protocol::parse(b"MS 0")?;
    let Response::MyPositionSelection { selection } = r else {
        return Err(format!("expected MyPositionSelection, got {r:?}").into());
    };
    assert_eq!(selection.as_u8(), 0);
    Ok(())
}

#[test]
fn parse_ms_rejects_out_of_range_selection() {
    assert!(protocol::parse(b"MS 6").is_err());
}

// === APRS CS + D-STAR (DS, GW) ===

#[test]
fn serialize_ds_read() {
    assert_eq!(protocol::serialize(&Command::GetDstarSlot), b"DS\r");
}

#[test]
fn parse_ds_response() -> TestResult {
    let r = protocol::parse(b"DS 1")?;
    let Response::DstarSlot { slot } = r else {
        return Err(format!("expected DstarSlot, got {r:?}").into());
    };
    assert_eq!(slot, DstarSlot::new(1)?);
    Ok(())
}

#[test]
fn serialize_cs_read() {
    assert_eq!(protocol::serialize(&Command::GetAprsCallsign), b"CS\r");
}

#[test]
fn serialize_cs_write() -> TestResult {
    let callsign = AprsCallsign::new("KQ4NIT-7").ok_or("valid APRS callsign rejected")?;
    assert_eq!(
        protocol::serialize(&Command::SetAprsCallsign { callsign }),
        b"CS KQ4NIT-7\r"
    );
    Ok(())
}

#[test]
fn parse_cs_response() -> TestResult {
    let r = protocol::parse(b"CS KQ4NIT-7")?;
    let Response::AprsCallsign { callsign } = r else {
        return Err(format!("expected AprsCallsign, got {r:?}").into());
    };
    assert_eq!(callsign.as_str(), "KQ4NIT-7");
    Ok(())
}

#[test]
fn serialize_gw_read() {
    assert_eq!(protocol::serialize(&Command::GetGateway), b"GW\r");
}

#[test]
fn parse_gw_response() -> TestResult {
    let r = protocol::parse(b"GW 0")?;
    let Response::Gateway { value } = r else {
        return Err(format!("expected Gateway, got {r:?}").into());
    };
    assert_eq!(value, DvGatewayMode::Off);
    Ok(())
}

#[test]
fn parse_gw_reflector_terminal() -> TestResult {
    let response = protocol::parse(b"GW 1")?;
    let Response::Gateway { value } = response else {
        return Err(format!("expected Gateway, got {response:?}").into());
    };
    assert_eq!(value, DvGatewayMode::ReflectorTerminal);
    Ok(())
}

#[test]
fn parse_gw_rejects_fabricated_value_two() {
    assert!(protocol::parse(b"GW 2").is_err());
}

// === GPS (GP, GM, GS) ===

#[test]
fn serialize_gp_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsConfig), b"GP\r");
}

#[test]
fn parse_gp_response() -> TestResult {
    let r = protocol::parse(b"GP 0,0")?;
    let Response::GpsConfig {
        gps_enabled,
        pc_output,
    } = r
    else {
        return Err(format!("expected GpsConfig, got {r:?}").into());
    };
    assert!(!gps_enabled);
    assert!(!pc_output);
    Ok(())
}

#[test]
fn parse_gp_response_enabled() -> TestResult {
    let r = protocol::parse(b"GP 1,1")?;
    let Response::GpsConfig {
        gps_enabled,
        pc_output,
    } = r
    else {
        return Err(format!("expected GpsConfig, got {r:?}").into());
    };
    assert!(gps_enabled);
    assert!(pc_output);
    Ok(())
}

#[test]
fn parse_gp_rejects_non_boolean_fields() {
    assert!(protocol::parse(b"GP 2,0").is_err());
    assert!(protocol::parse(b"GP 0,9").is_err());
}

#[test]
fn serialize_gm_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsMode), b"GM\r");
}

#[test]
fn parse_gm_response() -> TestResult {
    let r = protocol::parse(b"GM 0")?;
    let Response::GpsMode { mode } = r else {
        return Err(format!("expected GpsMode, got {r:?}").into());
    };
    assert_eq!(mode, GpsRadioMode::Normal);
    Ok(())
}

#[test]
fn serialize_gs_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsSentences), b"GS\r");
}

#[test]
fn parse_gs_response() -> TestResult {
    let r = protocol::parse(b"GS 1,1,1,1,1,1")?;
    let Response::GpsSentences {
        gga,
        gll,
        gsa,
        gsv,
        rmc,
        vtg,
    } = r
    else {
        return Err(format!("expected GpsSentences, got {r:?}").into());
    };
    assert!(gga);
    assert!(gll);
    assert!(gsa);
    assert!(gsv);
    assert!(rmc);
    assert!(vtg);
    Ok(())
}

#[test]
fn parse_gs_response_mixed() -> TestResult {
    let r = protocol::parse(b"GS 1,0,1,0,1,0")?;
    let Response::GpsSentences {
        gga,
        gll,
        gsa,
        gsv,
        rmc,
        vtg,
    } = r
    else {
        return Err(format!("expected GpsSentences, got {r:?}").into());
    };
    assert!(gga);
    assert!(!gll);
    assert!(gsa);
    assert!(!gsv);
    assert!(rmc);
    assert!(!vtg);
    Ok(())
}

#[test]
fn parse_gs_rejects_non_boolean_fields() {
    assert!(protocol::parse(b"GS 1,0,2,0,1,0").is_err());
}

// === Radio type (TY) ===

#[test]
fn parse_ty_accepts_exact_region_and_hex_variant() -> TestResult {
    let response = protocol::parse(b"TY K,F")?;
    let Response::RadioType { region, variant } = response else {
        return Err(format!("expected RadioType, got {response:?}").into());
    };
    assert_eq!(region, "K");
    assert_eq!(variant, 15);
    Ok(())
}

#[test]
fn parse_ty_rejects_invalid_region_or_variant_shape() {
    assert!(protocol::parse(b"TY X,2").is_err());
    assert!(protocol::parse(b"TY K,10").is_err());
    assert!(protocol::parse(b"TY K,f").is_err());
}

// === Bluetooth (BT) ===

#[test]
fn serialize_bt_read() {
    assert_eq!(protocol::serialize(&Command::GetBluetooth), b"BT\r");
}

#[test]
fn serialize_bt_write_on() {
    assert_eq!(
        protocol::serialize(&Command::SetBluetooth { enabled: true }),
        b"BT 1\r"
    );
}

#[test]
fn serialize_bt_write_off() {
    assert_eq!(
        protocol::serialize(&Command::SetBluetooth { enabled: false }),
        b"BT 0\r"
    );
}

#[test]
fn parse_bt_response_enabled() -> TestResult {
    let r = protocol::parse(b"BT 1")?;
    let Response::Bluetooth { enabled } = r else {
        return Err(format!("expected Bluetooth, got {r:?}").into());
    };
    assert!(enabled);
    Ok(())
}

#[test]
fn parse_bt_response_disabled() -> TestResult {
    let r = protocol::parse(b"BT 0")?;
    let Response::Bluetooth { enabled } = r else {
        return Err(format!("expected Bluetooth, got {r:?}").into());
    };
    assert!(!enabled);
    Ok(())
}

// === SD (SD) ===

#[test]
fn serialize_sd_read() {
    assert_eq!(protocol::serialize(&Command::GetSdCard), b"SD\r");
}

#[test]
fn parse_sd_response_present() -> TestResult {
    let r = protocol::parse(b"SD 1")?;
    let Response::SdCard { present } = r else {
        return Err(format!("expected SdCard, got {r:?}").into());
    };
    assert!(present);
    Ok(())
}

#[test]
fn parse_sd_response_absent() -> TestResult {
    let r = protocol::parse(b"SD 0")?;
    let Response::SdCard { present } = r else {
        return Err(format!("expected SdCard, got {r:?}").into());
    };
    assert!(!present);
    Ok(())
}
