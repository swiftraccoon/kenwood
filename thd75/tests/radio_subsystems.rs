//! Integration tests for APRS, D-STAR, and GPS radio methods (Task 27).

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;

#[tokio::test]
async fn get_active_callsign_slot() {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS 10\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_active_callsign_slot().await.unwrap(),
        kenwood_thd75::types::CallsignSlot::new(10).unwrap()
    );
}

#[tokio::test]
async fn set_active_callsign_slot() {
    let mut mock = MockTransport::new();
    mock.expect(b"CS 5\r", b"CS 5\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_active_callsign_slot(kenwood_thd75::types::CallsignSlot::new(5).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn get_gateway() {
    let mut mock = MockTransport::new();
    mock.expect(b"GW\r", b"GW 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_gateway().await.unwrap(),
        kenwood_thd75::types::DvGatewayMode::Off
    );
}

#[tokio::test]
async fn get_dstar_slot() {
    let mut mock = MockTransport::new();
    mock.expect(b"DS\r", b"DS 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_dstar_slot().await.unwrap(),
        kenwood_thd75::types::DstarSlot::new(1).unwrap()
    );
}

#[tokio::test]
async fn get_tnc_baud() {
    let mut mock = MockTransport::new();
    mock.expect(b"AS\r", b"AS 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_tnc_baud().await.unwrap(),
        kenwood_thd75::types::TncBaud::Bps1200
    );
}

#[tokio::test]
async fn get_beacon_type() {
    let mut mock = MockTransport::new();
    mock.expect(b"PT\r", b"PT 2\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_beacon_type().await.unwrap(),
        kenwood_thd75::types::BeaconMode::Ptt
    );
}

#[tokio::test]
async fn get_position_source() {
    let mut mock = MockTransport::new();
    mock.expect(b"MS\r", b"MS 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_position_source().await.unwrap(), 0);
}

#[tokio::test]
async fn get_serial_info() {
    let mut mock = MockTransport::new();
    mock.expect(b"AE\r", b"AE C3C10368,K01\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (serial, model_code) = radio.get_serial_info().await.unwrap();
    assert_eq!(serial, "C3C10368");
    assert_eq!(model_code, "K01");
}

#[tokio::test]
async fn get_gps_config() {
    let mut mock = MockTransport::new();
    mock.expect(b"GP\r", b"GP 0,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (gps_enabled, pc_output) = radio.get_gps_config().await.unwrap();
    assert!(!gps_enabled);
    assert!(!pc_output);
}

#[tokio::test]
#[allow(clippy::similar_names)]
async fn get_gps_sentences() {
    let mut mock = MockTransport::new();
    mock.expect(b"GS\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (gga, gll, gsa, gsv, rmc, vtg) = radio.get_gps_sentences().await.unwrap();
    assert!(gga);
    assert!(gll);
    assert!(gsa);
    assert!(gsv);
    assert!(rmc);
    assert!(vtg);
}

#[tokio::test]
async fn get_gps_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"GM\r", b"GM 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_gps_mode().await.unwrap(),
        kenwood_thd75::types::GpsRadioMode::Normal
    );
}
