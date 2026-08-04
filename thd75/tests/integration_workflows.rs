//! Multi-step workflow integration tests exercising Radio -> Transport -> Protocol -> Types.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;
use std::path::Path;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// `types::*` glob shadows the bare crate name.)
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
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn full_session_from_fixture() -> TestResult {
    let mock = MockTransport::from_fixture(Path::new("tests/fixtures/basic_session.txt"))?;
    let mut radio = Radio::new(mock);

    let info = radio.identify().await?;
    assert_eq!(info.model, RadioModel::ThD75);

    let ch = radio.get_frequency_full(Band::A).await?;
    assert_eq!(ch.receive_frequency.as_hz(), 145_000_000);

    radio.set_auto_info(true).await?;

    let level = radio.get_smeter(Band::A).await?;
    assert_eq!(level, SMeterReading::new(5)?);

    radio.set_power_level(Band::A, PowerLevel::Low).await?;
    Ok(())
}

#[tokio::test]
async fn identify_then_basics() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"PS\r", b"PS 1\r");

    let mut radio = Radio::new(mock);
    let info = radio.identify().await?;
    assert_eq!(info.model, RadioModel::ThD75);
    assert_eq!(radio.get_firmware_version().await?.as_str(), "1.03.000");
    assert!(radio.get_power_status().await?);
    Ok(())
}

#[tokio::test]
async fn frequency_change_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    // Read current frequency
    mock.expect(
        b"FO 0\r",
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    // Change mode
    mock.expect(b"MD 0,1\r", b"MD 0,1\r");
    // Read S-meter
    mock.expect(b"SM 0\r", b"SM 0,0003\r");

    let mut radio = Radio::new(mock);
    let ch = radio.get_frequency_full(Band::A).await?;
    assert_eq!(ch.receive_frequency.as_hz(), 145_000_000);
    radio.set_operating_mode(Band::A, OperatingMode::Dv).await?;
    assert_eq!(radio.get_smeter(Band::A).await?, SMeterReading::new(3)?);
    Ok(())
}

#[tokio::test]
async fn memory_channel_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 005\r",
        b"ME 005,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
    );

    let mut radio = Radio::new(mock);
    let ch = radio
        .get_regular_channel_record(RegularChannel::new(5)?)
        .await?;
    assert_eq!(ch.channel.receive_frequency.as_hz(), 145_000_000);
    assert!(!ch.split);
    assert!(!ch.scan_lockout);
    assert_eq!(
        ch.transmit_value(),
        ChannelTransmitValue::RepeaterOffset(Frequency::new(600_000)),
    );
    Ok(())
}

#[tokio::test]
async fn audio_settings_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 020\r");
    mock.expect(b"TN\r", b"TN 0,0\r");
    mock.expect(b"VX\r", b"VX 0\r");

    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_af_gain().await?, AfGainLevel::new(20)?);
    let tnc = radio.get_tnc_mode().await?;
    assert_eq!(tnc.mode, TncMode::Off);
    assert_eq!(tnc.data_rate, PacketDataRate::Bps1200);
    assert!(!radio.get_vox().await?);
    Ok(())
}

#[tokio::test]
async fn system_settings_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"LC\r", b"LC 2\r");
    mock.expect(b"BT\r", b"BT 1\r");

    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_backlight_control().await?, BacklightControl::Auto);
    assert!(radio.get_bluetooth().await?);
    Ok(())
}

#[tokio::test]
async fn aprs_callsign_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS N0CALL-7\r");
    mock.expect(b"CS KQ4NIT-7\r", b"CS KQ4NIT-7\r");
    mock.expect(b"CS\r", b"CS KQ4NIT-7\r");

    let mut radio = Radio::new(mock);
    assert_eq!(
        radio
            .get_aprs_callsign()
            .await?
            .ok_or("radio returned an empty APRS callsign slot")?
            .to_string(),
        "N0CALL-7"
    );
    let callsign = AprsCallsign::new("KQ4NIT-7")?;
    radio.set_aprs_callsign(callsign).await?;
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
