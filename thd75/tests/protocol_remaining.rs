//! Tests for the remaining 5 protocol groups: Scan, APRS, D-STAR, GPS,
//! and system commands (Bluetooth, SD, User Settings).

use kenwood_thd75::protocol::{self, Command, Response};
use kenwood_thd75::types::Band;

// === Scan (SR write-only, SF band-indexed, BS) ===

#[test]
fn serialize_scan_resume_write() {
    assert_eq!(
        protocol::serialize(&Command::SetScanResume { mode: 1 }),
        b"SR 1\r"
    );
}

// SR has no read form on D75 (bare `SR\r` returns `?`).

#[test]
fn serialize_sf_read_band_a() {
    assert_eq!(
        protocol::serialize(&Command::GetScanRange { band: Band::A }),
        b"SF 0\r"
    );
}

#[test]
fn serialize_sf_read_band_b() {
    assert_eq!(
        protocol::serialize(&Command::GetScanRange { band: Band::B }),
        b"SF 1\r"
    );
}

#[test]
fn parse_sf_response() {
    match protocol::parse(b"SF 0,0").unwrap() {
        Response::ScanRange { band, value } => {
            assert_eq!(band, Band::A);
            assert_eq!(value, 0);
        }
        other => panic!("expected ScanRange, got {other:?}"),
    }
}

#[test]
fn parse_sf_response_band_b() {
    match protocol::parse(b"SF 1,0").unwrap() {
        Response::ScanRange { band, value } => {
            assert_eq!(band, Band::B);
            assert_eq!(value, 0);
        }
        other => panic!("expected ScanRange, got {other:?}"),
    }
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
fn parse_bs_response() {
    match protocol::parse(b"BS 0").unwrap() {
        Response::BandScope { band } => {
            assert_eq!(band, Band::A);
        }
        other => panic!("expected BandScope, got {other:?}"),
    }
}

#[test]
fn parse_bs_response_band_b() {
    match protocol::parse(b"BS 1").unwrap() {
        Response::BandScope { band } => {
            assert_eq!(band, Band::B);
        }
        other => panic!("expected BandScope, got {other:?}"),
    }
}

// === APRS-related (AS, AE, PT, MS) ===

#[test]
fn serialize_as_read() {
    assert_eq!(protocol::serialize(&Command::GetTncBaud), b"AS\r");
}

#[test]
fn parse_as_response() {
    match protocol::parse(b"AS 0").unwrap() {
        Response::TncBaud { rate } => assert_eq!(rate, 0),
        other => panic!("expected TncBaud, got {other:?}"),
    }
}

#[test]
fn parse_as_response_9600() {
    match protocol::parse(b"AS 1").unwrap() {
        Response::TncBaud { rate } => assert_eq!(rate, 1),
        other => panic!("expected TncBaud, got {other:?}"),
    }
}

#[test]
fn serialize_ae_read() {
    assert_eq!(protocol::serialize(&Command::GetSerialInfo), b"AE\r");
}

#[test]
fn parse_ae_response_serial_info() {
    match protocol::parse(b"AE C3C10368,K01").unwrap() {
        Response::SerialInfo { serial, model_code } => {
            assert_eq!(serial, "C3C10368");
            assert_eq!(model_code, "K01");
        }
        other => panic!("expected SerialInfo, got {other:?}"),
    }
}

#[test]
fn serialize_pt_read() {
    assert_eq!(protocol::serialize(&Command::GetBeaconType), b"PT\r");
}

#[test]
fn parse_pt_response() {
    match protocol::parse(b"PT 2").unwrap() {
        Response::BeaconType { mode } => assert_eq!(mode, 2),
        other => panic!("expected BeaconType, got {other:?}"),
    }
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
fn parse_ms_response() {
    match protocol::parse(b"MS 0").unwrap() {
        Response::PositionSource { source } => assert_eq!(source, 0),
        other => panic!("expected PositionSource, got {other:?}"),
    }
}

// === D-STAR (DS, CS, GW) ===

#[test]
fn serialize_ds_read() {
    assert_eq!(protocol::serialize(&Command::GetDstarSlot), b"DS\r");
}

#[test]
fn parse_ds_response() {
    match protocol::parse(b"DS 1").unwrap() {
        Response::DstarSlot { slot } => assert_eq!(slot, 1),
        other => panic!("expected DstarSlot, got {other:?}"),
    }
}

#[test]
fn serialize_cs_read() {
    assert_eq!(
        protocol::serialize(&Command::GetActiveCallsignSlot),
        b"CS\r"
    );
}

#[test]
fn serialize_cs_write() {
    assert_eq!(
        protocol::serialize(&Command::SetActiveCallsignSlot { slot: 10 }),
        b"CS 10\r"
    );
}

#[test]
fn parse_cs_response() {
    match protocol::parse(b"CS 10").unwrap() {
        Response::ActiveCallsignSlot { slot } => assert_eq!(slot, 10),
        other => panic!("expected ActiveCallsignSlot, got {other:?}"),
    }
}

#[test]
fn serialize_gw_read() {
    assert_eq!(protocol::serialize(&Command::GetGateway), b"GW\r");
}

#[test]
fn parse_gw_response() {
    match protocol::parse(b"GW 0").unwrap() {
        Response::Gateway { value } => assert_eq!(value, 0),
        other => panic!("expected Gateway, got {other:?}"),
    }
}

// === GPS (GP, GM, GS) ===

#[test]
fn serialize_gp_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsConfig), b"GP\r");
}

#[test]
fn parse_gp_response() {
    match protocol::parse(b"GP 0,0").unwrap() {
        Response::GpsConfig {
            gps_enabled,
            pc_output,
        } => {
            assert!(!gps_enabled);
            assert!(!pc_output);
        }
        other => panic!("expected GpsConfig, got {other:?}"),
    }
}

#[test]
fn parse_gp_response_enabled() {
    match protocol::parse(b"GP 1,1").unwrap() {
        Response::GpsConfig {
            gps_enabled,
            pc_output,
        } => {
            assert!(gps_enabled);
            assert!(pc_output);
        }
        other => panic!("expected GpsConfig, got {other:?}"),
    }
}

#[test]
fn serialize_gm_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsMode), b"GM\r");
}

#[test]
fn parse_gm_response() {
    match protocol::parse(b"GM 0").unwrap() {
        Response::GpsMode { mode } => assert_eq!(mode, 0),
        other => panic!("expected GpsMode, got {other:?}"),
    }
}

#[test]
fn serialize_gs_read() {
    assert_eq!(protocol::serialize(&Command::GetGpsSentences), b"GS\r");
}

#[test]
fn parse_gs_response() {
    match protocol::parse(b"GS 1,1,1,1,1,1").unwrap() {
        Response::GpsSentences {
            gga,
            gll,
            gsa,
            gsv,
            rmc,
            vtg,
        } => {
            assert!(gga);
            assert!(gll);
            assert!(gsa);
            assert!(gsv);
            assert!(rmc);
            assert!(vtg);
        }
        other => panic!("expected GpsSentences, got {other:?}"),
    }
}

#[test]
fn parse_gs_response_mixed() {
    match protocol::parse(b"GS 1,0,1,0,1,0").unwrap() {
        Response::GpsSentences {
            gga,
            gll,
            gsa,
            gsv,
            rmc,
            vtg,
        } => {
            assert!(gga);
            assert!(!gll);
            assert!(gsa);
            assert!(!gsv);
            assert!(rmc);
            assert!(!vtg);
        }
        other => panic!("expected GpsSentences, got {other:?}"),
    }
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
fn parse_bt_response_enabled() {
    match protocol::parse(b"BT 1").unwrap() {
        Response::Bluetooth { enabled } => assert!(enabled),
        other => panic!("expected Bluetooth, got {other:?}"),
    }
}

#[test]
fn parse_bt_response_disabled() {
    match protocol::parse(b"BT 0").unwrap() {
        Response::Bluetooth { enabled } => assert!(!enabled),
        other => panic!("expected Bluetooth, got {other:?}"),
    }
}

// === SD (SD) ===

#[test]
fn serialize_sd_read() {
    assert_eq!(protocol::serialize(&Command::GetSdCard), b"SD\r");
}

#[test]
fn parse_sd_response_present() {
    match protocol::parse(b"SD 1").unwrap() {
        Response::SdCard { present } => assert!(present),
        other => panic!("expected SdCard, got {other:?}"),
    }
}

#[test]
fn parse_sd_response_absent() {
    match protocol::parse(b"SD 0").unwrap() {
        Response::SdCard { present } => assert!(!present),
        other => panic!("expected SdCard, got {other:?}"),
    }
}

// === User Settings (US) ===

#[test]
fn serialize_us_read() {
    assert_eq!(protocol::serialize(&Command::GetUserSettings), b"US\r");
}

#[test]
fn parse_us_response() {
    match protocol::parse(b"US 5").unwrap() {
        Response::UserSettings { value } => assert_eq!(value, 5),
        other => panic!("expected UserSettings, got {other:?}"),
    }
}
