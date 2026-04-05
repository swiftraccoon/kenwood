//! Tests for all newly added CAT command wrappers.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{
    Band, BeaconMode, DetectOutputMode, DstarSlot, DvGatewayMode, FilterMode, TncBaud,
    VfoMemoryMode,
};

// ---- BC: get_band / set_band ----

#[tokio::test]
async fn get_band() {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let band = radio.get_band().await.unwrap();
    assert_eq!(band, Band::A);
}

#[tokio::test]
async fn get_band_b() {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let band = radio.get_band().await.unwrap();
    assert_eq!(band, Band::B);
}

#[tokio::test]
async fn set_band() {
    let mut mock = MockTransport::new();
    mock.expect(b"BC 1\r", b"BC 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_band(Band::B).await.unwrap();
}

// ---- VM: get_vfo_memory_mode / set_vfo_memory_mode ----

#[tokio::test]
async fn get_vfo_memory_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let mode = radio.get_vfo_memory_mode(Band::A).await.unwrap();
    assert_eq!(mode, VfoMemoryMode::Vfo);
}

#[tokio::test]
async fn get_vfo_memory_mode_memory() {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 1\r", b"VM 1,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let mode = radio.get_vfo_memory_mode(Band::B).await.unwrap();
    assert_eq!(mode, VfoMemoryMode::Memory);
}

#[tokio::test]
async fn set_vfo_memory_mode() {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0,2\r", b"VM 0,2\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_vfo_memory_mode(Band::A, VfoMemoryMode::Call)
        .await
        .unwrap();
}

// ---- MR: recall_channel ----

#[tokio::test]
async fn recall_channel() {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 0,042\r", b"MR 0,042\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.recall_channel(Band::A, 42).await.unwrap();
}

#[tokio::test]
async fn recall_channel_band_b() {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 1,000\r", b"MR 1,000\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.recall_channel(Band::B, 0).await.unwrap();
}

// ---- UP: frequency_up ----

#[tokio::test]
async fn frequency_up() {
    let mut mock = MockTransport::new();
    mock.expect(b"UP 0\r", b"UP 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.frequency_up(Band::A).await.unwrap();
}

// ---- FR: get_fm_radio / set_fm_radio ----

#[tokio::test]
async fn get_fm_radio() {
    let mut mock = MockTransport::new();
    mock.expect(b"FR\r", b"FR 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_fm_radio().await.unwrap());
}

#[tokio::test]
async fn set_fm_radio() {
    let mut mock = MockTransport::new();
    mock.expect(b"FR 1\r", b"FR 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_fm_radio(true).await.unwrap();
}

// ---- FS: get_frequency_step / set_frequency_step ----

#[tokio::test]
async fn get_frequency_step() {
    let mut mock = MockTransport::new();
    mock.expect(b"FS 0\r", b"FS 0,5\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let step = radio.get_frequency_step(Band::A).await.unwrap();
    assert_eq!(u8::from(step), 5);
}

#[tokio::test]
async fn set_frequency_step() {
    use kenwood_thd75::types::StepSize;
    let mut mock = MockTransport::new();
    mock.expect(b"FS 0,5\r", b"FS 0,5\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_frequency_step(Band::A, StepSize::Hz12500)
        .await
        .unwrap();
}

// ---- FT: get_function_type ----

#[tokio::test]
async fn get_function_type() {
    let mut mock = MockTransport::new();
    mock.expect(b"FT\r", b"FT 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_function_type().await.unwrap());
}

// ---- SH: get_filter_width ----

#[tokio::test]
async fn get_filter_width() {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0\r", b"SH 0,3\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_filter_width(FilterMode::Ssb).await.unwrap(),
        kenwood_thd75::types::FilterWidthIndex::new(3, FilterMode::Ssb).unwrap()
    );
}

// ---- SH: set_filter_width ----

#[tokio::test]
async fn set_filter_width() {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0,3\r", b"SH 0,3\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_filter_width(
            FilterMode::Ssb,
            kenwood_thd75::types::FilterWidthIndex::new(3, FilterMode::Ssb).unwrap(),
        )
        .await
        .unwrap();
}

// ---- IO: get_io_port / set_io_port ----

#[tokio::test]
async fn get_io_port() {
    let mut mock = MockTransport::new();
    mock.expect(b"IO\r", b"IO 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(radio.get_io_port().await.unwrap(), DetectOutputMode::Af);
}

#[tokio::test]
async fn set_io_port() {
    let mut mock = MockTransport::new();
    mock.expect(b"IO 1\r", b"IO 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_io_port(DetectOutputMode::If).await.unwrap();
}

// ---- SF: get_scan_range (band-indexed) ----

#[tokio::test]
async fn get_scan_range() {
    let mut mock = MockTransport::new();
    mock.expect(b"SF 0\r", b"SF 0,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let (band, value) = radio.get_scan_range(Band::A).await.unwrap();
    assert_eq!(band, Band::A);
    assert_eq!(value, 1);
}

// ---- BS: get_band_scope ----

#[tokio::test]
async fn get_band_scope() {
    let mut mock = MockTransport::new();
    mock.expect(b"BS 0\r", b"BS 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let band = radio.get_band_scope(Band::A).await.unwrap();
    assert_eq!(band, Band::A);
}

// ---- SD: get_sd_status ----

#[tokio::test]
async fn get_sd_status() {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_sd_status().await.unwrap());
}

#[tokio::test]
async fn get_sd_status_present() {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(radio.get_sd_status().await.unwrap());
}

// ---- 0E: get_mcp_status ----

#[tokio::test]
async fn get_mcp_status() {
    let mut mock = MockTransport::new();
    mock.expect(b"0E\r", b"0E 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    let status = radio.get_mcp_status().await.unwrap();
    assert_eq!(status, "0");
}

// ---- GP: set_gps_config ----

#[tokio::test]
async fn set_gps_config() {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,0\r", b"GP 1,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_gps_config(true, false).await.unwrap();
}

#[tokio::test]
async fn set_gps_config_both_on() {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,1\r", b"GP 1,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_gps_config(true, true).await.unwrap();
}

// ---- GS: set_gps_sentences ----

#[tokio::test]
async fn set_gps_sentences() {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,0,1,0,1,0\r", b"GS 1,0,1,0,1,0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_gps_sentences(true, false, true, false, true, false)
        .await
        .unwrap();
}

#[tokio::test]
async fn set_gps_sentences_all_on() {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,1,1,1,1,1\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_gps_sentences(true, true, true, true, true, true)
        .await
        .unwrap();
}

// ---- AS: set_tnc_baud ----

#[tokio::test]
async fn set_tnc_baud() {
    let mut mock = MockTransport::new();
    mock.expect(b"AS 1\r", b"AS 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_tnc_baud(TncBaud::Bps9600).await.unwrap();
}

// ---- PT: set_beacon_type ----

#[tokio::test]
async fn set_beacon_type() {
    let mut mock = MockTransport::new();
    mock.expect(b"PT 3\r", b"PT 3\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_beacon_type(BeaconMode::Auto).await.unwrap();
}

// ---- MS: send_message ----

#[tokio::test]
async fn send_message() {
    let mut mock = MockTransport::new();
    mock.expect(b"MS hello\r", b"MS 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.send_message("hello").await.unwrap();
}

// ---- DS: set_dstar_slot ----

#[tokio::test]
async fn set_dstar_slot() {
    let mut mock = MockTransport::new();
    mock.expect(b"DS 2\r", b"DS 2\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_dstar_slot(DstarSlot::new(2).unwrap())
        .await
        .unwrap();
}

// ---- GW: set_gateway ----

#[tokio::test]
async fn set_gateway() {
    let mut mock = MockTransport::new();
    mock.expect(b"GW 1\r", b"GW 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_gateway(DvGatewayMode::ReflectorTerminal)
        .await
        .unwrap();
}

// ---- Serialization tests for new command variants ----

#[test]
fn serialize_set_fm_radio() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(serialize(&Command::SetFmRadio { enabled: true }), b"FR 1\r");
    assert_eq!(
        serialize(&Command::SetFmRadio { enabled: false }),
        b"FR 0\r"
    );
}

#[test]
fn serialize_get_vfo_memory_mode() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::GetVfoMemoryMode { band: Band::A }),
        b"VM 0\r"
    );
    assert_eq!(
        serialize(&Command::GetVfoMemoryMode { band: Band::B }),
        b"VM 1\r"
    );
}

#[test]
fn serialize_set_frequency_step() {
    use kenwood_thd75::protocol::{Command, serialize};
    use kenwood_thd75::types::StepSize;
    assert_eq!(
        serialize(&Command::SetFrequencyStep {
            band: Band::A,
            step: StepSize::Hz12500
        }),
        b"FS 0,5\r"
    );
}

#[test]
fn serialize_set_io_port() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetIoPort {
            value: DetectOutputMode::Detect
        }),
        b"IO 2\r"
    );
}

#[test]
fn serialize_set_tnc_baud() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetTncBaud {
            rate: TncBaud::Bps9600
        }),
        b"AS 1\r"
    );
}

#[test]
fn serialize_set_beacon_type() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetBeaconType {
            mode: BeaconMode::Ptt
        }),
        b"PT 2\r"
    );
}

#[test]
fn serialize_set_dstar_slot() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetDstarSlot {
            slot: DstarSlot::new(5).unwrap()
        }),
        b"DS 5\r"
    );
}

#[test]
fn serialize_set_gateway() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetGateway {
            value: DvGatewayMode::ReflectorTerminal
        }),
        b"GW 1\r"
    );
}

#[test]
fn serialize_set_gps_config() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetGpsConfig {
            gps_enabled: true,
            pc_output: false
        }),
        b"GP 1,0\r"
    );
}

#[test]
fn serialize_set_gps_sentences() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetGpsSentences {
            gga: true,
            gll: false,
            gsa: true,
            gsv: false,
            rmc: true,
            vtg: false
        }),
        b"GS 1,0,1,0,1,0\r"
    );
}
