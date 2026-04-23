//! Integration tests for radio audio methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::Band;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn get_set_af_gain() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 015\r");
    mock.expect(b"AG 020\r", b"AG 020\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_af_gain().await?,
        kenwood_thd75::types::AfGainLevel::new(15)
    );
    radio
        .set_af_gain(Band::A, kenwood_thd75::types::AfGainLevel::new(20))
        .await?;
    Ok(())
}

#[tokio::test]
async fn get_tnc_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"TN\r", b"TN 0,0\r");
    let mut radio = Radio::connect(mock).await?;
    let (mode, setting) = radio.get_tnc_mode().await?;
    assert_eq!(mode, kenwood_thd75::types::TncMode::Aprs);
    assert_eq!(setting, kenwood_thd75::types::TncBaud::Bps1200);
    Ok(())
}

#[tokio::test]
async fn get_dstar_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DC 1\r", b"DC 1,KQ4NIT  ,D75A\r");
    let mut radio = Radio::connect(mock).await?;
    let (callsign, suffix) = radio
        .get_dstar_callsign(kenwood_thd75::types::DstarSlot::new(1)?)
        .await?;
    assert_eq!(callsign, "KQ4NIT  ");
    assert_eq!(suffix, "D75A");
    Ok(())
}

#[tokio::test]
async fn get_real_time_clock() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"RT\r", b"RT 240104095700\r");
    let mut radio = Radio::connect(mock).await?;
    let datetime = radio.get_real_time_clock().await?;
    assert_eq!(datetime, "240104095700");
    Ok(())
}

#[tokio::test]
async fn vox_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VX\r", b"VX 0\r");
    mock.expect(b"VX 1\r", b"VX 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_vox().await?);
    radio.set_vox(true).await?;
    Ok(())
}

#[tokio::test]
async fn vox_gain() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VG\r", b"VG 5\r");
    mock.expect(b"VG 8\r", b"VG 8\r");
    let mut radio = Radio::connect(mock).await?;
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
    mock.expect(b"VD 7\r", b"VD 7\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_vox_delay().await?,
        kenwood_thd75::types::VoxDelay::new(3)?
    );
    radio
        .set_vox_delay(kenwood_thd75::types::VoxDelay::new(7)?)
        .await?;
    Ok(())
}
