//! Integration tests for radio audio methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::Band;

#[tokio::test]
async fn get_set_af_gain() {
    let mut mock = MockTransport::new();
    mock.expect(b"AG\r", b"AG 015\r");
    mock.expect(b"AG 0,20\r", b"AG 020\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_af_gain().await.unwrap(), 15);
    radio.set_af_gain(Band::A, 20).await.unwrap();
}

#[tokio::test]
async fn get_tnc_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"TN\r", b"TN 0,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (mode, setting) = radio.get_tnc_mode().await.unwrap();
    assert_eq!(mode, 0);
    assert_eq!(setting, 0);
}

#[tokio::test]
async fn get_dstar_callsign() {
    let mut mock = MockTransport::new();
    mock.expect(b"DC 1\r", b"DC 1,KQ4NIT  ,D75A\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (callsign, suffix) = radio.get_dstar_callsign(1).await.unwrap();
    assert_eq!(callsign, "KQ4NIT  ");
    assert_eq!(suffix, "D75A");
}

#[tokio::test]
async fn get_real_time_clock() {
    let mut mock = MockTransport::new();
    mock.expect(b"RT\r", b"RT 240104095700\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let datetime = radio.get_real_time_clock().await.unwrap();
    assert_eq!(datetime, "240104095700");
}

#[tokio::test]
async fn vox_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"VX\r", b"VX 0\r");
    mock.expect(b"VX 1\r", b"VX 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_vox().await.unwrap());
    radio.set_vox(true).await.unwrap();
}

#[tokio::test]
async fn vox_gain() {
    let mut mock = MockTransport::new();
    mock.expect(b"VG\r", b"VG 5\r");
    mock.expect(b"VG 8\r", b"VG 8\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_vox_gain().await.unwrap(), 5);
    radio.set_vox_gain(8).await.unwrap();
}

#[tokio::test]
async fn vox_delay() {
    let mut mock = MockTransport::new();
    mock.expect(b"VD\r", b"VD 3\r");
    mock.expect(b"VD 7\r", b"VD 7\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_vox_delay().await.unwrap(), 3);
    radio.set_vox_delay(7).await.unwrap();
}
