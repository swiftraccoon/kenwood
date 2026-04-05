//! Integration tests for service mode protocol commands:
//! 0G enter/exit, 2V version read, 0S calibration data read.

use kenwood_thd75::protocol::{self, Command, Response};

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
fn parse_enter_service_mode_response() {
    let r = protocol::parse(b"0G").unwrap();
    match r {
        Response::ServiceMode { data } => assert_eq!(data, ""),
        other => panic!("expected ServiceMode, got {other:?}"),
    }
}

#[test]
fn parse_enter_service_mode_with_payload() {
    let r = protocol::parse(b"0G OK").unwrap();
    match r {
        Response::ServiceMode { data } => assert_eq!(data, "OK"),
        other => panic!("expected ServiceMode, got {other:?}"),
    }
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
fn parse_service_version_response() {
    let r = protocol::parse(b"2V EX-5210").unwrap();
    match r {
        Response::ServiceVersion { data } => assert_eq!(data, "EX-5210"),
        other => panic!("expected ServiceVersion, got {other:?}"),
    }
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
fn parse_calibration_data_response() {
    let r = protocol::parse(b"0S AABBCCDD").unwrap();
    match r {
        Response::ServiceCalibrationData { data } => assert_eq!(data, "AABBCCDD"),
        other => panic!("expected ServiceCalibrationData, got {other:?}"),
    }
}
