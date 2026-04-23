//! Integration tests for APRS, D-STAR, and GPS radio methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn get_active_callsign_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS 10\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_active_callsign_slot().await?,
        kenwood_thd75::types::CallsignSlot::new(10)?
    );
    Ok(())
}

#[tokio::test]
async fn set_active_callsign_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS 5\r", b"CS 5\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_active_callsign_slot(kenwood_thd75::types::CallsignSlot::new(5)?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn get_gateway() -> TestResult {
    let mut mock = MockTransport::new();
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
        kenwood_thd75::types::BeaconMode::Ptt
    );
    Ok(())
}

#[tokio::test]
async fn get_position_source() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MS\r", b"MS 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_position_source().await?, 0);
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
    mock.expect(b"GM\r", b"GM 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_gps_mode().await?,
        kenwood_thd75::types::GpsRadioMode::Normal
    );
    Ok(())
}
