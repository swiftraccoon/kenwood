//! Integration tests for radio core methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint. (`::aprs` is spelled that way because the
// bare crate name.)
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
async fn get_frequency_full() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"FO 0\r",
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    let mut radio = Radio::new(mock);
    let ch = radio.get_frequency_full(Band::A).await?;
    assert_eq!(ch.receive_frequency.as_hz(), 145_000_000);
    Ok(())
}

#[tokio::test]
async fn get_frequency() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FQ 0\r", b"FQ 0,0145000000\r");
    let mut radio = Radio::new(mock);
    let frequency = radio.get_frequency(Band::A).await?;
    assert_eq!(frequency.as_hz(), 145_000_000);
    Ok(())
}

#[tokio::test]
async fn set_power_level() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PC 0,2\r", b"PC 0,2\r");
    let mut radio = Radio::new(mock);
    radio.set_power_level(Band::A, PowerLevel::Low).await?;
    Ok(())
}

#[tokio::test]
async fn get_power_level() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PC 1\r", b"PC 1,0\r");
    let mut radio = Radio::new(mock);
    let level = radio.get_power_level(Band::B).await?;
    assert_eq!(level, PowerLevel::High);
    Ok(())
}

#[tokio::test]
async fn get_smeter() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SM 0\r", b"SM 0,0005\r");
    let mut radio = Radio::new(mock);
    let level = radio.get_smeter(Band::A).await?;
    assert_eq!(level, SMeterReading::new(5)?);
    Ok(())
}

#[tokio::test]
async fn get_firmware_version() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_firmware_version().await?.as_str(), "1.03.000");
    assert_eq!(
        radio
            .cached_firmware_version()
            .map(FirmwareIdentity::as_str),
        Some("1.03.000")
    );
    assert_eq!(
        radio.cached_firmware_profile(),
        Some(kenwood_thd75::FirmwareProfile::StandardCat)
    );
    Ok(())
}

#[tokio::test]
async fn transmit_and_receive() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"TX\r", b"TX\r");
    mock.expect(b"RX\r", b"RX\r");
    let mut radio = Radio::new(mock);
    radio.transmit().await?;
    radio.receive().await?;
    Ok(())
}

#[tokio::test]
async fn get_operating_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    let mut radio = Radio::new(mock);
    let mode = radio.get_operating_mode(Band::A).await?;
    assert_eq!(mode, OperatingMode::Fm);
    Ok(())
}

#[tokio::test]
async fn set_operating_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 1,6\r", b"MD 1,6\r");
    let mut radio = Radio::new(mock);
    radio
        .set_operating_mode(Band::B, OperatingMode::Nfm)
        .await?;
    Ok(())
}

#[tokio::test]
async fn get_squelch() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SQ 0\r", b"SQ 0,05\r");
    let mut radio = Radio::new(mock);
    let level = radio.get_squelch(Band::A).await?;
    assert_eq!(level, SquelchLevel::new(5)?);
    Ok(())
}

#[tokio::test]
async fn set_squelch() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SQ 0,4\r", b"SQ 0,4\r");
    let mut radio = Radio::new(mock);
    radio.set_squelch(Band::A, SquelchLevel::new(4)?).await?;
    Ok(())
}

#[tokio::test]
async fn get_busy() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BY 0\r", b"BY 0,1\r");
    let mut radio = Radio::new(mock);
    assert!(radio.get_busy(Band::A).await?);
    Ok(())
}

#[tokio::test]
async fn get_power_status() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PS\r", b"PS 1\r");
    let mut radio = Radio::new(mock);
    assert!(radio.get_power_status().await?);
    Ok(())
}

#[tokio::test]
async fn identify_returns_radio_model() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.identify().await?.model, RadioModel::ThD75);
    Ok(())
}

#[tokio::test]
async fn typed_operation_routes_unsolicited_to_notifications() -> TestResult {
    // When AI mode is enabled, the radio may send unsolicited frames before
    // the actual command response. The execute() method should route these
    // to the broadcast notification channel and still return the correct
    // response for the command that was sent.
    let mut mock = MockTransport::new();
    // Mock returns an unsolicited BY frame concatenated before the FV response.
    mock.expect(b"FV\r", b"BY 0,1\rFV 1.03\r");
    let mut radio = Radio::new(mock);
    let mut rx = radio.subscribe();

    let version = radio.get_firmware_version().await?;
    assert_eq!(version.as_str(), "1.03");

    // The unsolicited BY notification should appear on the subscribe channel.
    let notification = rx.try_recv()?;
    let kenwood_thd75::protocol::Response::Busy { band, busy } = notification else {
        return Err(format!("expected Busy notification, got {notification:?}").into());
    };
    assert_eq!(band, Band::A);
    assert!(busy);
    Ok(())
}
