//! Multi-step workflow integration tests exercising Radio -> Transport -> Protocol -> Types.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;
use std::path::Path;

#[tokio::test]
async fn full_session_from_fixture() {
    let mock = MockTransport::from_fixture(Path::new("tests/fixtures/basic_session.txt")).unwrap();
    let mut radio = Radio::connect(mock).await.unwrap();

    let info = radio.identify().await.unwrap();
    assert_eq!(info.model, "TH-D75");

    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);

    radio.set_auto_info(true).await.unwrap();

    let level = radio.get_smeter(Band::A).await.unwrap();
    assert_eq!(level, SMeterReading::new(5).unwrap());

    radio
        .set_power_level(Band::A, PowerLevel::Low)
        .await
        .unwrap();
}

#[tokio::test]
async fn identify_then_basics() {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    mock.expect(b"PS\r", b"PS 1\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    let info = radio.identify().await.unwrap();
    assert!(info.model.contains("TH-D75"));
    assert_eq!(radio.get_firmware_version().await.unwrap(), "1.03.000");
    assert!(radio.get_power_status().await.unwrap());
}

#[tokio::test]
async fn frequency_change_workflow() {
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

    let mut radio = Radio::connect(mock).await.unwrap();
    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
    radio.set_mode(Band::A, Mode::Dv).await.unwrap();
    assert_eq!(
        radio.get_smeter(Band::A).await.unwrap(),
        SMeterReading::new(3).unwrap()
    );
}

#[tokio::test]
async fn memory_channel_workflow() {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 005\r",
        b"ME 005,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
    );

    let mut radio = Radio::connect(mock).await.unwrap();
    let ch = radio.read_channel(5).await.unwrap();
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
}

#[tokio::test]
async fn audio_settings_workflow() {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 020\r");
    mock.expect(b"TN\r", b"TN 0,0\r");
    mock.expect(b"VX\r", b"VX 0\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_af_gain().await.unwrap(), AfGainLevel::new(20));
    let (tnc_mode, tnc_setting) = radio.get_tnc_mode().await.unwrap();
    assert_eq!(tnc_mode, TncMode::Aprs);
    assert_eq!(tnc_setting, TncBaud::Bps1200);
    assert!(!radio.get_vox().await.unwrap());
}

#[tokio::test]
async fn system_settings_workflow() {
    let mut mock = MockTransport::new();
    // LC wire value 0 = locked on D75 (inverted), so get_lock() returns true.
    mock.expect(b"LC\r", b"LC 0\r");
    mock.expect(b"BT\r", b"BT 1\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(radio.get_lock().await.unwrap());
    assert!(radio.get_bluetooth().await.unwrap());
}

#[tokio::test]
async fn dstar_callsign_slot_workflow() {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS 10\r");
    mock.expect(b"CS 5\r", b"CS 5\r");
    mock.expect(b"CS\r", b"CS 5\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_active_callsign_slot().await.unwrap(),
        CallsignSlot::new(10).unwrap()
    );
    radio
        .set_active_callsign_slot(CallsignSlot::new(5).unwrap())
        .await
        .unwrap();
    assert_eq!(
        radio.get_active_callsign_slot().await.unwrap(),
        CallsignSlot::new(5).unwrap()
    );
}
