//! Tests for all newly added CAT command wrappers.

use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{
    Band, BeaconMode, DetectOutputMode, DstarSlot, DvGatewayMode, FilterMode, FineStep, StepSize,
    TncBaud, VfoMemoryMode,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

// ---- BC: get_band / set_band ----

#[tokio::test]
async fn get_band() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 0\r");
    let mut radio = Radio::connect(mock).await?;
    let band = radio.get_band().await?;
    assert_eq!(band, Band::A);
    Ok(())
}

#[tokio::test]
async fn get_band_b() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC\r", b"BC 1\r");
    let mut radio = Radio::connect(mock).await?;
    let band = radio.get_band().await?;
    assert_eq!(band, Band::B);
    Ok(())
}

#[tokio::test]
async fn set_band() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BC 1\r", b"BC 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_band(Band::B).await?;
    Ok(())
}

// ---- VM: get_vfo_memory_mode / set_vfo_memory_mode ----

#[tokio::test]
async fn get_vfo_memory_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    let mut radio = Radio::connect(mock).await?;
    let mode = radio.get_vfo_memory_mode(Band::A).await?;
    assert_eq!(mode, VfoMemoryMode::Vfo);
    Ok(())
}

#[tokio::test]
async fn get_vfo_memory_mode_memory() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 1\r", b"VM 1,1\r");
    let mut radio = Radio::connect(mock).await?;
    let mode = radio.get_vfo_memory_mode(Band::B).await?;
    assert_eq!(mode, VfoMemoryMode::Memory);
    Ok(())
}

#[tokio::test]
async fn set_vfo_memory_mode() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"VM 0,2\r", b"VM 0,2\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_vfo_memory_mode(Band::A, VfoMemoryMode::Call)
        .await?;
    Ok(())
}

// ---- MR: recall_channel ----

#[tokio::test]
async fn recall_channel() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 0,042\r", b"MR 0,042\r");
    let mut radio = Radio::connect(mock).await?;
    radio.recall_channel(Band::A, 42).await?;
    Ok(())
}

#[tokio::test]
async fn recall_channel_band_b() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MR 1,000\r", b"MR 1,000\r");
    let mut radio = Radio::connect(mock).await?;
    radio.recall_channel(Band::B, 0).await?;
    Ok(())
}

// ---- UP: frequency_up ----

#[tokio::test]
async fn frequency_up() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"UP 0\r", b"UP 0\r");
    let mut radio = Radio::connect(mock).await?;
    radio.frequency_up(Band::A).await?;
    Ok(())
}

// ---- FR: get_fm_radio / set_fm_radio ----

#[tokio::test]
async fn get_fm_radio() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FR\r", b"FR 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_fm_radio().await?);
    Ok(())
}

#[tokio::test]
async fn set_fm_radio() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FR 1\r", b"FR 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_fm_radio(true).await?;
    Ok(())
}

// ---- FS: get_fine_step / set_fine_step ----

#[tokio::test]
async fn get_fine_step() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FS\r", b"FS 0\r");
    let mut radio = Radio::connect(mock).await?;
    let step = radio.get_fine_step().await?;
    assert_eq!(step, FineStep::Hz20);
    Ok(())
}

#[tokio::test]
async fn set_fine_step() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FS 0,2\r", b"FS 2\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_fine_step(Band::A, FineStep::Hz500).await?;
    Ok(())
}

// ---- FT: get_function_type ----

#[tokio::test]
async fn get_function_type() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FT\r", b"FT 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_function_type().await?);
    Ok(())
}

// ---- SH: get_filter_width ----

#[tokio::test]
async fn get_filter_width() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0\r", b"SH 0,3\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_filter_width(FilterMode::Ssb).await?,
        kenwood_thd75::types::FilterWidthIndex::new(3, FilterMode::Ssb)?
    );
    Ok(())
}

// ---- SH: set_filter_width ----

#[tokio::test]
async fn set_filter_width() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SH 0,3\r", b"SH 0,3\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_filter_width(
            FilterMode::Ssb,
            kenwood_thd75::types::FilterWidthIndex::new(3, FilterMode::Ssb)?,
        )
        .await?;
    Ok(())
}

// ---- IO: get_io_port / set_io_port ----

#[tokio::test]
async fn get_io_port() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"IO\r", b"IO 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_io_port().await?, DetectOutputMode::Af);
    Ok(())
}

#[tokio::test]
async fn set_io_port() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"IO 1\r", b"IO 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_io_port(DetectOutputMode::If).await?;
    Ok(())
}

// ---- SF: get_step_size / set_step_size (band-indexed) ----

#[tokio::test]
async fn get_step_size() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SF 0\r", b"SF 0,5\r");
    let mut radio = Radio::connect(mock).await?;
    let (band, step) = radio.get_step_size(Band::A).await?;
    assert_eq!(band, Band::A);
    assert_eq!(step, StepSize::Hz12500);
    Ok(())
}

#[tokio::test]
async fn set_step_size() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SF 0,5\r", b"SF 0,5\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_step_size(Band::A, StepSize::Hz12500).await?;
    Ok(())
}

// ---- BS: get_band_scope ----

#[tokio::test]
async fn get_band_scope() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BS 0\r", b"BS 0\r");
    let mut radio = Radio::connect(mock).await?;
    let band = radio.get_band_scope(Band::A).await?;
    assert_eq!(band, Band::A);
    Ok(())
}

// ---- SD: get_sd_status ----

#[tokio::test]
async fn get_sd_status() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 0\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_sd_status().await?);
    Ok(())
}

#[tokio::test]
async fn get_sd_status_present() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SD\r", b"SD 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(radio.get_sd_status().await?);
    Ok(())
}

// ---- 0E: get_mcp_status ----

#[tokio::test]
async fn get_mcp_status() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"0E\r", b"0E 0\r");
    let mut radio = Radio::connect(mock).await?;
    let status = radio.get_mcp_status().await?;
    assert_eq!(status, "0");
    Ok(())
}

// ---- GP: set_gps_config ----

#[tokio::test]
async fn set_gps_config() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,0\r", b"GP 1,0\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_gps_config(true, false).await?;
    Ok(())
}

#[tokio::test]
async fn set_gps_config_both_on() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,1\r", b"GP 1,1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_gps_config(true, true).await?;
    Ok(())
}

// ---- GS: set_gps_sentences ----

#[tokio::test]
async fn set_gps_sentences() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,0,1,0,1,0\r", b"GS 1,0,1,0,1,0\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_gps_sentences(true, false, true, false, true, false)
        .await?;
    Ok(())
}

#[tokio::test]
async fn set_gps_sentences_all_on() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GS 1,1,1,1,1,1\r", b"GS 1,1,1,1,1,1\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_gps_sentences(true, true, true, true, true, true)
        .await?;
    Ok(())
}

// ---- AS: set_tnc_baud ----

#[tokio::test]
async fn set_tnc_baud() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AS 1\r", b"AS 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_tnc_baud(TncBaud::Bps9600).await?;
    Ok(())
}

// ---- PT: set_beacon_type ----

#[tokio::test]
async fn set_beacon_type() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"PT 3\r", b"PT 3\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_beacon_type(BeaconMode::Auto).await?;
    Ok(())
}

// ---- MS: send_message ----

#[tokio::test]
async fn send_message() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"MS hello\r", b"MS 0\r");
    let mut radio = Radio::connect(mock).await?;
    radio.send_message("hello").await?;
    Ok(())
}

// ---- DS: set_dstar_slot ----

#[tokio::test]
async fn set_dstar_slot() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DS 2\r", b"DS 2\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_dstar_slot(DstarSlot::new(2)?).await?;
    Ok(())
}

// ---- GW: set_gateway ----

#[tokio::test]
async fn set_gateway() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GW 1\r", b"GW 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_gateway(DvGatewayMode::ReflectorTerminal).await?;
    Ok(())
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
fn serialize_set_fine_step() {
    use kenwood_thd75::protocol::{Command, serialize};
    assert_eq!(
        serialize(&Command::SetFineStep {
            band: Band::A,
            step: FineStep::Hz500
        }),
        b"FS 0,2\r"
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
