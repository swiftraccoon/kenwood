//! Integration tests for APRS, D-STAR, and GPS radio methods.

use kenwood_thd75::error::Error;
use kenwood_thd75::radio::{FirmwareProfile, Radio};
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{NmeaSentence, NmeaSentences};

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
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
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn get_aprs_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS KQ4NIT-7\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio
            .get_aprs_callsign()
            .await?
            .ok_or("radio returned an empty APRS callsign slot")?
            .to_string(),
        "KQ4NIT-7"
    );
    Ok(())
}

#[tokio::test]
async fn set_aprs_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS KQ4NIT-7\r", b"CS KQ4NIT-7\r");
    let mut radio = Radio::new(mock);
    let callsign = kenwood_thd75::types::AprsCallsign::new("KQ4NIT-7")?;
    radio.set_aprs_callsign(callsign).await?;
    Ok(())
}

#[tokio::test]
async fn get_empty_aprs_callsign_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS \r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_aprs_callsign().await?, None);
    Ok(())
}

#[tokio::test]
async fn read_gateway() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"GW\r", b"GW 0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.read_gateway().await?,
        kenwood_thd75::types::DvGatewayMode::Off
    );
    Ok(())
}

#[tokio::test]
async fn get_dstar_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DS\r", b"DS 1\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_dstar_slot().await?,
        kenwood_thd75::types::DstarSlot::new(1)?
    );
    Ok(())
}

#[tokio::test]
async fn get_packet_data_rate() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AS\r", b"AS 0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_packet_data_rate().await?,
        kenwood_thd75::types::PacketDataRate::Bps1200
    );
    Ok(())
}

#[tokio::test]
async fn get_beacon_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PT\r", b"PT 2\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_beacon_mode().await?,
        kenwood_thd75::types::BeaconMode::Auto
    );
    Ok(())
}

#[tokio::test]
async fn transmit_aprs_beacon_requires_the_exact_bare_acknowledgement() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BE\r", b"BE\r");
    let mut radio = Radio::new(mock);
    radio.transmit_aprs_beacon().await?;
    Ok(())
}

#[tokio::test]
async fn transmit_aprs_beacon_preserves_tnc_not_ready() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BE\r", b"N\r");
    let mut radio = Radio::new(mock);
    assert!(matches!(
        radio.transmit_aprs_beacon().await,
        Err(Error::NotAvailableInCurrentMode { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn get_my_position_selection() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MS\r", b"MS 0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_my_position_selection().await?.as_raw(), 0);
    Ok(())
}

#[tokio::test]
async fn get_gps_settings() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP\r", b"GP 0,0\r");
    let mut radio = Radio::new(mock);
    let settings = radio.get_gps_settings().await?;
    assert!(!settings.enabled());
    assert!(!settings.pc_output());
    Ok(())
}

#[tokio::test]
async fn get_gps_sentences() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::new(mock);
    let sentences = radio.get_gps_sentences().await?;
    assert_eq!(sentences, NmeaSentences::all());
    assert!(sentences.contains(NmeaSentence::Rmc));
    Ok(())
}

#[tokio::test]
async fn read_gps_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"GM\r", b"GM 0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.read_gps_mode().await?,
        kenwood_thd75::types::GpsRadioMode::Normal
    );
    Ok(())
}

#[tokio::test]
async fn azimuth_profile_rejects_bare_gateway_and_gps_without_writes() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.AZM\r");
    let mut radio = Radio::new(mock);

    let gateway = radio.read_gateway().await;
    assert!(
        matches!(
            gateway,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GW",
                ref firmware,
            }) if firmware.as_str() == "1.03.AZM"
        ),
        "AZM gateway query should fail locally, got {gateway:?}"
    );
    let gps = radio.read_gps_mode().await;
    assert!(
        matches!(
            gps,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GM",
                ref firmware,
            }) if firmware.as_str() == "1.03.AZM"
        ),
        "AZM GPS-mode query should fail locally, got {gps:?}"
    );
    assert_eq!(
        radio.cached_firmware_profile(),
        Some(FirmwareProfile::AzimuthAutomation)
    );
    Ok(())
}

#[tokio::test]
async fn unknown_firmware_identity_blocks_unqualified_gateway_command() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.XYZ\r");
    let mut radio = Radio::new(mock);

    let gateway = radio.read_gateway().await;
    assert!(
        matches!(
            gateway,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GW",
                ref firmware,
            }) if firmware.as_str() == "1.03.XYZ"
        ),
        "unknown firmware must fail closed, got {gateway:?}"
    );
    assert_eq!(
        radio.cached_firmware_profile(),
        Some(FirmwareProfile::Unknown)
    );
    Ok(())
}
