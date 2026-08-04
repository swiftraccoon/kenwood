//! Integration tests for radio memory methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;

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
async fn get_regular_channel_record() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 000\r",
        b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
    );
    let mut radio = Radio::new(mock);
    let ch = radio
        .get_regular_channel_record(RegularChannel::new(0)?)
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
async fn get_regular_channel_record_with_callsign() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 010\r",
        b"ME 010,0440000000,0005000000,0,0,0,0,0,1,0,0,0,0,0,1,14,14,023,0,REPEATER,1,05,0\r",
    );
    let mut radio = Radio::new(mock);
    let ch = radio
        .get_regular_channel_record(RegularChannel::new(10)?)
        .await?;
    assert_eq!(ch.channel.receive_frequency.as_hz(), 440_000_000);
    assert_eq!(ch.channel.tone_mode, ToneMode::Tone);
    assert_eq!(ch.channel.ur_call, DstarCallsign::new("REPEATER")?);
    assert!(!ch.split);
    assert!(!ch.scan_lockout);
    Ok(())
}
