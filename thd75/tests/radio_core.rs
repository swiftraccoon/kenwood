//! Integration tests for radio core methods (Task 24).

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::*;

#[tokio::test]
async fn get_frequency_full() {
    let mut mock = MockTransport::new();
    mock.expect(
        b"FO 0\r",
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    let mut radio = Radio::connect(mock).await.unwrap();
    let ch = radio.get_frequency_full(Band::A).await.unwrap();
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
}

#[tokio::test]
async fn get_frequency() {
    let mut mock = MockTransport::new();
    mock.expect(
        b"FQ 0\r",
        b"FQ 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    let mut radio = Radio::connect(mock).await.unwrap();
    let ch = radio.get_frequency(Band::A).await.unwrap();
    assert_eq!(ch.rx_frequency.as_hz(), 145_000_000);
}

#[tokio::test]
async fn set_frequency_full() {
    let mut mock = MockTransport::new();
    mock.expect(
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
    );
    let mut radio = Radio::connect(mock).await.unwrap();
    let ch = ChannelMemory {
        rx_frequency: Frequency::new(145_000_000),
        tx_offset: Frequency::new(600_000),
        step_size: StepSize::Hz5000,
        mode_flags_raw: 0,
        shift: ShiftDirection::DOWN,
        reverse: false,
        tone_enable: false,
        ctcss_mode: CtcssMode::Off,
        dcs_enable: false,
        cross_tone_reverse: false,
        flags_0a_raw: 0x02, // shift- = 2
        tone_code: ToneCode::new(8).unwrap(),
        ctcss_code: ToneCode::new(8).unwrap(),
        dcs_code: DcsCode::new(0).unwrap(),
        cross_tone_combo: CrossToneType::DcsOff,
        digital_squelch: FlashDigitalSquelch::Off,
        urcall: ChannelName::new("CQCQCQ").unwrap(),
        data_mode: 0,
    };
    radio.set_frequency_full(Band::A, &ch).await.unwrap();
}

#[tokio::test]
async fn set_power_level() {
    let mut mock = MockTransport::new();
    mock.expect(b"PC 0,2\r", b"PC 0,2\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_power_level(Band::A, PowerLevel::Low)
        .await
        .unwrap();
}

#[tokio::test]
async fn get_power_level() {
    let mut mock = MockTransport::new();
    mock.expect(b"PC 1\r", b"PC 1,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let level = radio.get_power_level(Band::B).await.unwrap();
    assert_eq!(level, PowerLevel::High);
}

#[tokio::test]
async fn get_smeter() {
    let mut mock = MockTransport::new();
    mock.expect(b"SM 0\r", b"SM 0,0005\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let level = radio.get_smeter(Band::A).await.unwrap();
    assert_eq!(level, SMeterReading::new(5).unwrap());
}

#[tokio::test]
async fn get_firmware_version() {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.000\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_firmware_version().await.unwrap(), "1.03.000");
}

#[tokio::test]
async fn transmit_and_receive() {
    let mut mock = MockTransport::new();
    mock.expect(b"TX 0\r", b"TX 0\r");
    mock.expect(b"RX 0\r", b"RX 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.transmit(Band::A).await.unwrap();
    radio.receive(Band::A).await.unwrap();
}

#[tokio::test]
async fn get_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let mode = radio.get_mode(Band::A).await.unwrap();
    assert_eq!(mode, Mode::Fm);
}

#[tokio::test]
async fn set_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 1,6\r", b"MD 1,6\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_mode(Band::B, Mode::Nfm).await.unwrap();
}

#[tokio::test]
async fn get_squelch() {
    let mut mock = MockTransport::new();
    mock.expect(b"SQ 0\r", b"SQ 0,05\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let level = radio.get_squelch(Band::A).await.unwrap();
    assert_eq!(level, SquelchLevel::new(5).unwrap());
}

#[tokio::test]
async fn set_squelch() {
    let mut mock = MockTransport::new();
    mock.expect(b"SQ 0,4\r", b"SQ 0,4\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_squelch(Band::A, SquelchLevel::new(4).unwrap())
        .await
        .unwrap();
}

#[tokio::test]
async fn get_busy() {
    let mut mock = MockTransport::new();
    mock.expect(b"BY 0\r", b"BY 0,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(radio.get_busy(Band::A).await.unwrap());
}

#[tokio::test]
async fn get_power_status() {
    let mut mock = MockTransport::new();
    mock.expect(b"PS\r", b"PS 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(radio.get_power_status().await.unwrap());
}

#[tokio::test]
async fn get_radio_id() {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TH-D75\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_radio_id().await.unwrap(), "TH-D75");
}

#[tokio::test]
async fn execute_routes_unsolicited_to_notifications() {
    // When AI mode is enabled, the radio may send unsolicited frames before
    // the actual command response. The execute() method should route these
    // to the broadcast notification channel and still return the correct
    // response for the command that was sent.
    let mut mock = MockTransport::new();
    // Mock returns an unsolicited BY frame concatenated before the FV response.
    mock.expect(b"FV\r", b"BY 0,1\rFV 1.03\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let mut rx = radio.subscribe();

    let response = radio
        .execute(kenwood_thd75::protocol::Command::GetFirmwareVersion)
        .await
        .unwrap();

    // The actual FV response should be returned.
    match response {
        kenwood_thd75::protocol::Response::FirmwareVersion { version } => {
            assert_eq!(version, "1.03");
        }
        other => panic!("expected FirmwareVersion, got {other:?}"),
    }

    // The unsolicited BY notification should appear on the subscribe channel.
    let notification = rx.try_recv().unwrap();
    match notification {
        kenwood_thd75::protocol::Response::Busy { band, busy } => {
            assert_eq!(band, Band::A);
            assert!(busy);
        }
        other => panic!("expected Busy notification, got {other:?}"),
    }
}
