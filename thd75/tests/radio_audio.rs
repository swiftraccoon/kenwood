//! Integration tests for selected audio, packet, D-STAR, and system CAT methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{DstarCallsign, DstarSlot, DstarSuffix};

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
async fn get_set_af_gain() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 015\r");
    mock.expect(b"AG 020\r", b"AG 020\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_af_gain().await?,
        kenwood_thd75::types::AfGainLevel::new(15)?
    );
    radio
        .set_af_gain(kenwood_thd75::types::AfGainLevel::new(20)?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn get_tnc_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"TN\r", b"TN 0,0\r");
    let mut radio = Radio::new(mock);
    let state = radio.get_tnc_mode().await?;
    assert_eq!(state.mode, kenwood_thd75::types::TncMode::Off);
    assert_eq!(state.data_band, kenwood_thd75::types::TncDataBand::A);
    Ok(())
}

#[tokio::test]
async fn get_dstar_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DC 1\r", b"DC 1,KQ4NIT  ,D75A\r");
    let mut radio = Radio::new(mock);
    let entry = radio.get_dstar_callsign(DstarSlot::new(1)?).await?;
    assert_eq!(entry.callsign, DstarCallsign::new("KQ4NIT")?);
    assert_eq!(entry.suffix, DstarSuffix::new("D75A")?);
    Ok(())
}

#[tokio::test]
async fn set_dstar_callsign_uses_validated_fixed_width_fields() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DC 1,KQ4NIT  ,    \r", b"DC 1,KQ4NIT  ,    \r");
    let mut radio = Radio::new(mock);
    radio
        .set_dstar_callsign(
            DstarSlot::new(1)?,
            DstarCallsign::new("KQ4NIT")?,
            DstarSuffix::default(),
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn get_real_time_clock() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"RT\r", b"RT 240104095700\r");
    let mut radio = Radio::new(mock);
    let clock = radio.get_real_time_clock().await?;
    let Some(datetime) = clock.date_time() else {
        return Err("expected available radio clock".into());
    };
    assert_eq!(datetime.to_wire_string(), "240104095700");
    assert_eq!(datetime.to_string(), "2024-01-04 09:57:00");
    Ok(())
}

#[tokio::test]
async fn get_unavailable_real_time_clock() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"RT\r", b"RT ------------\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_real_time_clock().await?,
        kenwood_thd75::types::RadioClock::Unavailable
    );
    Ok(())
}

#[tokio::test]
async fn vox_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VX\r", b"VX 0\r");
    mock.expect(b"VX 1\r", b"VX 1\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_vox().await?);
    radio.set_vox(true).await?;
    Ok(())
}

#[tokio::test]
async fn vox_gain() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VG\r", b"VG 5\r");
    mock.expect(b"VG 8\r", b"VG 8\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_vox_gain().await?,
        kenwood_thd75::types::VoxGain::new(5)?
    );
    radio
        .set_vox_gain(kenwood_thd75::types::VoxGain::new(8)?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn vox_delay() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VD\r", b"VD 3\r");
    mock.expect(b"VD 6\r", b"VD 6\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_vox_delay().await?,
        kenwood_thd75::types::VoxDelay::new(3)?
    );
    radio
        .set_vox_delay(kenwood_thd75::types::VoxDelay::new(6)?)
        .await?;
    Ok(())
}
