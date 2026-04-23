//! Integration tests for service mode protocol commands:
//! 0G enter/exit, 2V version read, 0S calibration data read.

use kenwood_thd75::protocol::{self, Command, Response};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ============================================================================
// 0G — Enter / Exit service mode
// ============================================================================

#[test]
fn serialize_enter_service_mode() {
    let bytes = protocol::serialize(&Command::EnterServiceMode);
    assert_eq!(bytes, b"0G KENWOOD\r");
}

#[test]
fn serialize_exit_service_mode() {
    let bytes = protocol::serialize(&Command::ExitServiceMode);
    assert_eq!(bytes, b"0G\r");
}

#[test]
fn parse_enter_service_mode_response() -> TestResult {
    let r = protocol::parse(b"0G")?;
    let Response::ServiceMode { data } = r else {
        return Err(format!("expected ServiceMode, got {r:?}").into());
    };
    assert_eq!(data, "");
    Ok(())
}

#[test]
fn parse_enter_service_mode_with_payload() -> TestResult {
    let r = protocol::parse(b"0G OK")?;
    let Response::ServiceMode { data } = r else {
        return Err(format!("expected ServiceMode, got {r:?}").into());
    };
    assert_eq!(data, "OK");
    Ok(())
}

// ============================================================================
// 2V — Service version read
// ============================================================================

#[test]
fn serialize_service_get_version() {
    let bytes = protocol::serialize(&Command::ServiceGetVersion {
        param1: "00".to_owned(),
        param2: "000".to_owned(),
    });
    assert_eq!(bytes, b"2V 00,000\r");
}

#[test]
fn parse_service_version_response() -> TestResult {
    let r = protocol::parse(b"2V EX-5210")?;
    let Response::ServiceVersion { data } = r else {
        return Err(format!("expected ServiceVersion, got {r:?}").into());
    };
    assert_eq!(data, "EX-5210");
    Ok(())
}

// ============================================================================
// 0S — Calibration data read
// ============================================================================

#[test]
fn serialize_read_calibration_data() {
    let bytes = protocol::serialize(&Command::ReadCalibrationData);
    assert_eq!(bytes, b"0S\r");
}

#[test]
fn parse_calibration_data_response() -> TestResult {
    let r = protocol::parse(b"0S AABBCCDD")?;
    let Response::ServiceCalibrationData { data } = r else {
        return Err(format!("expected ServiceCalibrationData, got {r:?}").into());
    };
    assert_eq!(data, "AABBCCDD");
    Ok(())
}
