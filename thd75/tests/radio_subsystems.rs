//! Integration tests for APRS, D-STAR, and GPS radio methods.

use kenwood_thd75::error::Error;
use kenwood_thd75::radio::{FirmwareProfile, Radio};
use kenwood_thd75::transport::MockTransport;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn get_aprs_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS KQ4NIT-7\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_aprs_callsign().await?.as_str(), "KQ4NIT-7");
    Ok(())
}

#[tokio::test]
async fn set_aprs_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS KQ4NIT-7\r", b"CS KQ4NIT-7\r");
    let mut radio = Radio::connect(mock).await?;
    let callsign = kenwood_thd75::types::AprsCallsign::new("KQ4NIT-7")
        .ok_or("valid APRS callsign rejected")?;
    radio.set_aprs_callsign(callsign).await?;
    Ok(())
}

#[tokio::test]
async fn get_gateway() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"GW\r", b"GW 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_gateway().await?,
        kenwood_thd75::types::DvGatewayMode::Off
    );
    Ok(())
}

#[tokio::test]
async fn get_dstar_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DS\r", b"DS 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_dstar_slot().await?,
        kenwood_thd75::types::DstarSlot::new(1)?
    );
    Ok(())
}

#[tokio::test]
async fn get_tnc_baud() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AS\r", b"AS 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_tnc_baud().await?,
        kenwood_thd75::types::TncBaud::Bps1200
    );
    Ok(())
}

#[tokio::test]
async fn get_beacon_type() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PT\r", b"PT 2\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_beacon_type().await?,
        kenwood_thd75::types::BeaconMode::Auto
    );
    Ok(())
}

#[tokio::test]
async fn get_my_position_selection() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MS\r", b"MS 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_my_position_selection().await?.as_u8(), 0);
    Ok(())
}

#[tokio::test]
async fn get_serial_info() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AE\r", b"AE C3C10368,K01\r");
    let mut radio = Radio::connect(mock).await?;
    let (serial, model_code) = radio.get_serial_info().await?;
    assert_eq!(serial, "C3C10368");
    assert_eq!(model_code, "K01");
    Ok(())
}

#[tokio::test]
async fn get_gps_config() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP\r", b"GP 0,0\r");
    let mut radio = Radio::connect(mock).await?;
    let (gps_enabled, pc_output) = radio.get_gps_config().await?;
    assert!(!gps_enabled);
    assert!(!pc_output);
    Ok(())
}

#[tokio::test]
async fn get_gps_sentences() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::connect(mock).await?;
    let (gga, gll, gsa, gsv, rmc, vtg) = radio.get_gps_sentences().await?;
    assert!(gga);
    assert!(gll);
    assert!(gsa);
    assert!(gsv);
    assert!(rmc);
    assert!(vtg);
    Ok(())
}

#[tokio::test]
async fn get_gps_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"GM\r", b"GM 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_gps_mode().await?,
        kenwood_thd75::types::GpsRadioMode::Normal
    );
    Ok(())
}

#[tokio::test]
async fn azimuth_profile_rejects_bare_gateway_and_gps_without_writes() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.AZM\r");
    let mut radio = Radio::connect(mock).await?;

    let gateway = radio.get_gateway().await;
    assert!(
        matches!(
            gateway,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GW",
                ref firmware,
            }) if firmware == "1.03.AZM"
        ),
        "AZM gateway query should fail locally, got {gateway:?}"
    );
    let gps = radio.get_gps_mode().await;
    assert!(
        matches!(
            gps,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GM",
                ref firmware,
            }) if firmware == "1.03.AZM"
        ),
        "AZM GPS-mode query should fail locally, got {gps:?}"
    );
    assert_eq!(
        radio.firmware_profile(),
        Some(FirmwareProfile::AzimuthAutomation)
    );
    Ok(())
}

#[tokio::test]
async fn near_match_azimuth_identity_keeps_standard_gateway_command() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.AZM2\r");
    mock.expect(b"GW\r", b"GW 0\r");
    let mut radio = Radio::connect(mock).await?;

    assert_eq!(
        radio.get_gateway().await?,
        kenwood_thd75::types::DvGatewayMode::Off
    );
    assert_eq!(radio.firmware_profile(), Some(FirmwareProfile::StandardCat));
    Ok(())
}
