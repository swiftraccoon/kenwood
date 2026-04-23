//! Tests for the remaining 5 protocol groups: Scan, APRS, D-STAR, GPS,
//! and system commands (Bluetooth, SD, User Settings).

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::*;

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
    assert_eq!(
        protocol::serialize(&Command::GetBandScope { band: Band::A }),
        b"BS 0\r"
    );
}

#[test]
fn serialize_bs_read_band_b() {
    assert_eq!(
        protocol::serialize(&Command::GetBandScope { band: Band::B }),
        b"BS 1\r"
    );
}

#[test]
fn parse_bs_response() -> TestResult {
    let r = protocol::parse(b"BS 0")?;
    let Response::BandScope { band } = r else {
        return Err(format!("expected BandScope, got {r:?}").into());
    };
    assert_eq!(band, Band::A);
    Ok(())
}

#[test]
fn parse_bs_response_band_b() -> TestResult {
    let r = protocol::parse(b"BS 1")?;
    let Response::BandScope { band } = r else {
        return Err(format!("expected BandScope, got {r:?}").into());
    };
    assert_eq!(band, Band::B);
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
fn serialize_pt_read() {
    assert_eq!(protocol::serialize(&Command::GetBeaconType), b"PT\r");
}

#[test]
fn parse_pt_response() -> TestResult {
    let r = protocol::parse(b"PT 2")?;
    let Response::BeaconType { mode } = r else {
        return Err(format!("expected BeaconType, got {r:?}").into());
    };
    assert_eq!(mode, BeaconMode::Ptt);
    Ok(())
}

#[test]
fn serialize_ms_read() {
    assert_eq!(protocol::serialize(&Command::GetPositionSource), b"MS\r");
}

#[test]
fn serialize_ms_write() {
    assert_eq!(
        protocol::serialize(&Command::SendMessage {
            text: "hello world".into()
        }),
        b"MS hello world\r"
    );
}

#[test]
fn parse_ms_response() -> TestResult {
    let r = protocol::parse(b"MS 0")?;
    let Response::PositionSource { source } = r else {
        return Err(format!("expected PositionSource, got {r:?}").into());
    };
    assert_eq!(source, 0);
    Ok(())
}

// === D-STAR (DS, CS, GW) ===

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
    assert_eq!(
        protocol::serialize(&Command::GetActiveCallsignSlot),
        b"CS\r"
    );
}

#[test]
fn serialize_cs_write() -> TestResult {
    assert_eq!(
        protocol::serialize(&Command::SetActiveCallsignSlot {
            slot: CallsignSlot::new(10)?
        }),
        b"CS 10\r"
    );
    Ok(())
}

#[test]
fn parse_cs_response() -> TestResult {
    let r = protocol::parse(b"CS 10")?;
    let Response::ActiveCallsignSlot { slot } = r else {
        return Err(format!("expected ActiveCallsignSlot, got {r:?}").into());
    };
    assert_eq!(slot, CallsignSlot::new(10)?);
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

// === User Settings (US) ===

#[test]
fn serialize_us_read() {
    assert_eq!(protocol::serialize(&Command::GetUserSettings), b"US\r");
}

#[test]
fn parse_us_response() -> TestResult {
    let r = protocol::parse(b"US 5")?;
    let Response::UserSettings { value } = r else {
        return Err(format!("expected UserSettings, got {r:?}").into());
    };
    assert_eq!(value, 5);
    Ok(())
}
