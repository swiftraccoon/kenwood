//! Multi-step workflow integration tests exercising Radio -> Transport -> Protocol -> Types.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn full_session_from_fixture() -> TestResult {
    let mock = MockTransport::from_fixture(Path::new("tests/fixtures/basic_session.txt"))?;
    let mut radio = Radio::connect(mock).await?;

    let info = radio.identify().await?;
    assert_eq!(info.model, "TH-D75");

    let ch = radio.get_frequency_full(Band::A).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);

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

    let mut radio = Radio::connect(mock).await?;
    let info = radio.identify().await?;
    assert!(info.model.contains("TH-D75"));
    assert_eq!(radio.get_firmware_version().await?, "1.03.000");
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

    let mut radio = Radio::connect(mock).await?;
    let ch = radio.get_frequency_full(Band::A).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
    radio.set_mode(Band::A, Mode::Dv).await?;
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

    let mut radio = Radio::connect(mock).await?;
    let ch = radio.read_channel(5).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
    Ok(())
}

#[tokio::test]
async fn audio_settings_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 020\r");
    mock.expect(b"TN\r", b"TN 0,0\r");
    mock.expect(b"VX\r", b"VX 0\r");

    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_af_gain().await?, AfGainLevel::new(20));
    let (tnc_mode, tnc_setting) = radio.get_tnc_mode().await?;
    assert_eq!(tnc_mode, TncMode::Aprs);
    assert_eq!(tnc_setting, TncBaud::Bps1200);
    assert!(!radio.get_vox().await?);
    Ok(())
}

#[tokio::test]
async fn system_settings_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    // LC wire value 0 = locked on D75 (inverted), so get_lock() returns true.
    mock.expect(b"LC\r", b"LC 0\r");
    mock.expect(b"BT\r", b"BT 1\r");

    let mut radio = Radio::connect(mock).await?;
    assert!(radio.get_lock().await?);
    assert!(radio.get_bluetooth().await?);
    Ok(())
}

#[tokio::test]
async fn dstar_callsign_slot_workflow() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"CS\r", b"CS 10\r");
    mock.expect(b"CS 5\r", b"CS 5\r");
    mock.expect(b"CS\r", b"CS 5\r");

    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_active_callsign_slot().await?,
        CallsignSlot::new(10)?
    );
    radio
        .set_active_callsign_slot(CallsignSlot::new(5)?)
        .await?;
    assert_eq!(
        radio.get_active_callsign_slot().await?,
        CallsignSlot::new(5)?
    );
    Ok(())
}
