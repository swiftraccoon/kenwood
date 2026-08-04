//! Tests for all newly added CAT command wrappers.

use kenwood_thd75::error::Error;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{
    AntennaInput, Band, BeaconMode, DstarSlot, FilterMode, FineStep, GpsSettings,
    MyPositionSelection, NmeaSentences, OperatingMode, PacketDataRate, RegularChannel, StepSize,
    TuningMode, UsbAudioOutput,
};

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
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

// ---- BC: get_band / set_band ----

#[tokio::test]
async fn get_band() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 0\r");
    let mut radio = Radio::new(mock);
    let band = radio.get_band().await?;
    assert_eq!(band, Band::A);
    Ok(())
}

#[tokio::test]
async fn get_band_b() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 1\r");
    let mut radio = Radio::new(mock);
    let band = radio.get_band().await?;
    assert_eq!(band, Band::B);
    Ok(())
}

#[tokio::test]
async fn set_band() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC 1\r", b"BC 1\r");
    let mut radio = Radio::new(mock);
    radio.set_band(Band::B).await?;
    Ok(())
}

// ---- VM: get_tuning_mode / set_tuning_mode ----

#[tokio::test]
async fn get_tuning_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    let mut radio = Radio::new(mock);
    let mode = radio.get_tuning_mode(Band::A).await?;
    assert_eq!(mode, TuningMode::Vfo);
    Ok(())
}

#[tokio::test]
async fn get_tuning_mode_memory() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 1\r", b"VM 1,1\r");
    let mut radio = Radio::new(mock);
    let mode = radio.get_tuning_mode(Band::B).await?;
    assert_eq!(mode, TuningMode::Memory);
    Ok(())
}

#[tokio::test]
async fn set_tuning_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0,2\r", b"VM 0,2\r");
    let mut radio = Radio::new(mock);
    radio.set_tuning_mode(Band::A, TuningMode::Call).await?;
    Ok(())
}

#[tokio::test]
async fn set_operating_mode_accepts_negative_ack_only_after_matching_readback() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0,7\r", b"N\r");
    mock.expect(b"MD 0\r", b"MD 0,7\r");
    let mut radio = Radio::new(mock);

    radio.set_operating_mode(Band::A, OperatingMode::Dr).await?;
    Ok(())
}

#[tokio::test]
async fn set_operating_mode_preserves_rejection_after_nonmatching_readback() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MD 0,2\r", b"?\r");
    mock.expect(b"MD 0\r", b"MD 0,0\r");
    let mut radio = Radio::new(mock);

    assert!(matches!(
        radio.set_operating_mode(Band::A, OperatingMode::Am).await,
        Err(Error::CommandRejected)
    ));
    Ok(())
}

// ---- MR: recall_channel ----

#[tokio::test]
async fn recall_channel() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 0,042\r", b"MR 0,042\r");
    let mut radio = Radio::new(mock);
    radio
        .recall_channel(Band::A, RegularChannel::new(42)?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn recall_channel_band_b() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 1,000\r", b"MR 1,000\r");
    let mut radio = Radio::new(mock);
    radio
        .recall_channel(Band::B, RegularChannel::new(0)?)
        .await?;
    Ok(())
}

// ---- UP: frequency_up ----

#[tokio::test]
async fn frequency_up() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 0\r");
    mock.expect(b"UP\r", b"UP\r");
    mock.expect(b"FQ 0\r", b"FQ 0,0144000000\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.frequency_up().await?.as_hz(), 144_000_000);
    Ok(())
}

#[tokio::test]
async fn frequency_up_blind() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"UP\r", b"UP\r");
    let mut radio = Radio::new(mock);
    radio.frequency_up_blind().await?;
    Ok(())
}

// ---- FR: get_fm_radio ----

#[tokio::test]
async fn get_fm_radio() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FR\r", b"FR 0\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_fm_radio().await?);
    Ok(())
}

// ---- FS: get_fine_step ----

#[tokio::test]
async fn get_fine_step() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FS\r", b"FS 0\r");
    let mut radio = Radio::new(mock);
    let step = radio.get_fine_step().await?;
    assert_eq!(step, FineStep::Hz20);
    Ok(())
}

// ---- FT: Fine Tune ----

#[tokio::test]
async fn get_and_set_fine_tune() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FT\r", b"FT 0\r");
    mock.expect(b"FT 1\r", b"FT 1\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_fine_tune().await?);
    radio.set_fine_tune(true).await?;
    Ok(())
}

// ---- SH: get_filter_width ----

#[tokio::test]
async fn get_filter_width() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0\r", b"SH 0,3\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_filter_width(FilterMode::Ssb).await?,
        kenwood_thd75::types::FilterWidthIndex::new(FilterMode::Ssb, 3)?
    );
    Ok(())
}

// ---- SH: set_filter_width ----

#[tokio::test]
async fn set_filter_width() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0,3\r", b"SH 0,3\r");
    let mut radio = Radio::new(mock);
    radio
        .set_filter_width(kenwood_thd75::types::FilterWidthIndex::new(
            FilterMode::Ssb,
            3,
        )?)
        .await?;
    Ok(())
}

// ---- IO: get_usb_audio_output / set_usb_audio_output ----

#[tokio::test]
async fn get_usb_audio_output() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"IO\r", b"IO 0\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_usb_audio_output().await?, UsbAudioOutput::Audio);
    Ok(())
}

#[tokio::test]
async fn set_usb_audio_output() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"IO 1\r", b"IO 1\r");
    let mut radio = Radio::new(mock);
    radio
        .set_usb_audio_output(UsbAudioOutput::IntermediateFrequency)
        .await?;
    Ok(())
}

// ---- SF: get_step_size / set_step_size (band-indexed) ----

#[tokio::test]
async fn get_step_size() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SF 0\r", b"SF 0,5\r");
    let mut radio = Radio::new(mock);
    let step = radio.get_step_size(Band::A).await?;
    assert_eq!(step, StepSize::Hz12500);
    Ok(())
}

#[tokio::test]
async fn set_step_size() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SF 0,5\r", b"SF 0,5\r");
    let mut radio = Radio::new(mock);
    radio.set_step_size(Band::A, StepSize::Hz12500).await?;
    Ok(())
}

// ---- BS: antenna input ----

#[tokio::test]
async fn get_antenna_input() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BS\r", b"BS 1\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_antenna_input().await?, AntennaInput::InternalBar);
    Ok(())
}

// ---- SD: get_sd_status ----

#[tokio::test]
async fn get_sd_status() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 0\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_sd_status().await?);
    Ok(())
}

#[tokio::test]
async fn get_sd_status_present() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 1\r");
    let mut radio = Radio::new(mock);
    assert!(radio.get_sd_status().await?);
    Ok(())
}

// ---- GP: set_gps_settings ----

#[tokio::test]
async fn set_gps_settings() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,0\r", b"GP 1,0\r");
    let mut radio = Radio::new(mock);
    radio
        .set_gps_settings(GpsSettings::new(true, false))
        .await?;
    Ok(())
}

#[tokio::test]
async fn set_gps_settings_both_on() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,1\r", b"GP 1,1\r");
    let mut radio = Radio::new(mock);
    radio.set_gps_settings(GpsSettings::new(true, true)).await?;
    Ok(())
}

// ---- GS: set_gps_sentences ----

#[tokio::test]
async fn set_gps_sentences() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,0,1,0,1,0\r", b"GS 1,0,1,0,1,0\r");
    let mut radio = Radio::new(mock);
    radio
        .set_gps_sentences(NmeaSentences::try_from(0b01_0101)?)
        .await?;
    Ok(())
}

#[tokio::test]
async fn set_gps_sentences_all_on() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,1,1,1,1,1\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::new(mock);
    radio.set_gps_sentences(NmeaSentences::all()).await?;
    Ok(())
}

// ---- AS: set_packet_data_rate ----

#[tokio::test]
async fn set_packet_data_rate() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AS 1\r", b"AS 1\r");
    let mut radio = Radio::new(mock);
    radio.set_packet_data_rate(PacketDataRate::Bps9600).await?;
    Ok(())
}

// ---- PT: set_beacon_mode ----

#[tokio::test]
async fn set_beacon_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PT 2\r", b"PT 2\r");
    let mut radio = Radio::new(mock);
    radio.set_beacon_mode(BeaconMode::Auto).await?;
    Ok(())
}

// ---- MS: My Position selection ----

#[tokio::test]
async fn set_my_position_selection() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MS 5\r", b"MS 5\r");
    let mut radio = Radio::new(mock);
    radio
        .set_my_position_selection(MyPositionSelection::new(5)?)
        .await?;
    Ok(())
}

// ---- DS: set_dstar_slot ----

#[tokio::test]
async fn set_dstar_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DS 2\r", b"DS 2\r");
    let mut radio = Radio::new(mock);
    radio.set_dstar_slot(DstarSlot::new(2)?).await?;
    Ok(())
}

// ---- Serialization tests for new command variants ----

#[test]
fn serialize_get_tuning_mode() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::GetTuningMode { band: Band::A }),
        b"VM 0\r"
    );
    assert_eq!(
        serialize(&Command::GetTuningMode { band: Band::B }),
        b"VM 1\r"
    );
}

#[test]
fn serialize_set_step_size() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetStepSize {
            band: Band::A,
            step: StepSize::Hz12500
        }),
        b"SF 0,5\r"
    );
}

#[test]
fn serialize_set_usb_audio_output() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetUsbAudioOutput {
            output: UsbAudioOutput::Detect
        }),
        b"IO 2\r"
    );
}

#[test]
fn serialize_set_packet_data_rate() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetPacketDataRate {
            data_rate: PacketDataRate::Bps9600
        }),
        b"AS 1\r"
    );
}

#[test]
fn serialize_set_beacon_mode() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetBeaconMode {
            mode: BeaconMode::Ptt
        }),
        b"PT 1\r"
    );
}

#[test]
fn serialize_set_dstar_slot() -> TestResult {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetDstarSlot {
            slot: DstarSlot::new(5)?
        }),
        b"DS 5\r"
    );
    Ok(())
}

#[test]
fn serialize_set_gps_settings() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetGpsSettings {
            settings: GpsSettings::new(true, false)
        }),
        b"GP 1,0\r"
    );
}

#[test]
fn serialize_set_gps_sentences() -> TestResult {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetGpsSentences {
            sentences: NmeaSentences::try_from(0b01_0101)?
        }),
        b"GS 1,0,1,0,1,0\r"
    );
    Ok(())
}
