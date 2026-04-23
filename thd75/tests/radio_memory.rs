//! Integration tests for radio memory methods.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn read_channel() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 000\r",
        b"ME 000,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
    );
    let mut radio = Radio::connect(mock).await?;
    let ch = radio.read_channel(0).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
    Ok(())
}

#[tokio::test]
async fn write_channel() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 005,0440000000,0005000000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
        b"ME 005,0440000000,0005000000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,CQCQCQ,0,00,0\r",
    );
    let mut radio = Radio::connect(mock).await?;
    let ch = ChannelMemory {
        rx_frequency: Frequency::new(440_000_000),
        tx_offset: Frequency::new(5_000_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::SIMPLEX,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0,
        tone_code: ToneCode::new(8)?,
        ctcss_code: ToneCode::new(8)?,
        dcs_code: DcsCode::new(0)?,
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ")?,
        data_mode: 0,
    };
    radio.write_channel(5, &ch).await?;
    Ok(())
}

#[tokio::test]
async fn read_channel_with_name() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 010\r",
        b"ME 010,0440000000,0005000000,0,0,0,0,0,1,1,1,0,0,0,1,14,14,023,0,REPEATER,1,05,0\r",
    );
    let mut radio = Radio::connect(mock).await?;
    let ch = radio.read_channel(10).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 440_000_000);
    assert_eq!(ch.urcall, ChannelName::new("REPEATER")?);
    Ok(())
}
