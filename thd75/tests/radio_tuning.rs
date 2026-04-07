//! Integration tests for high-level tuning methods: `tune_frequency`,
//! `tune_channel`, and `quick_tune`.

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{Band, Frequency, Mode, StepSize};

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Typical FO response for Band A at 145.000 MHz.
const FO_RESPONSE_145: &[u8] =
    b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r";

/// FO write command for 146.520 MHz (all other fields preserved from
/// `FO_RESPONSE_145`).
const FO_WRITE_146520: &[u8] =
    b"FO 0,0146520000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r";

/// FO echo after writing 146.520 MHz.
const FO_RESPONSE_146520: &[u8] =
    b"FO 0,0146520000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r";

/// FQ short response for Band A at 146.520 MHz.
const FQ_RESPONSE_146520: &[u8] = b"FQ 0,0146520000\r";

// ---------------------------------------------------------------------------
// tune_frequency
// ---------------------------------------------------------------------------

/// Build a mock for `mock_modify_page_sequence`-style MCP exchanges.
fn build_w_response(page: u16, data: &[u8]) -> Vec<u8> {
    assert_eq!(data.len(), 256);
    let addr = page.to_be_bytes();
    let mut resp = vec![b'W', addr[0], addr[1], 0x00, 0x00];
    resp.extend_from_slice(data);
    resp
}

#[tokio::test]
async fn tune_frequency_when_already_vfo() {
    // Radio is already in VFO mode — no VM write needed.
    let mut mock = MockTransport::new();
    // ensure_mode: query VM -> already VFO
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    // read current FO
    mock.expect(b"FO 0\r", FO_RESPONSE_145);
    // write updated frequency
    mock.expect(FO_WRITE_146520, FO_RESPONSE_146520);
    // verify readback
    mock.expect(b"FQ 0\r", FQ_RESPONSE_146520);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .tune_frequency(Band::A, Frequency::new(146_520_000))
        .await
        .unwrap();
}

#[tokio::test]
async fn tune_frequency_switches_from_memory_to_vfo() {
    // Radio starts in Memory mode — must switch to VFO first.
    let mut mock = MockTransport::new();
    // ensure_mode: query VM -> Memory (1), needs to switch
    mock.expect(b"VM 0\r", b"VM 0,1\r");
    // switch to VFO mode
    mock.expect(b"VM 0,0\r", b"VM 0,0\r");
    // read current FO
    mock.expect(b"FO 0\r", FO_RESPONSE_145);
    // write updated frequency
    mock.expect(FO_WRITE_146520, FO_RESPONSE_146520);
    // verify readback
    mock.expect(b"FQ 0\r", FQ_RESPONSE_146520);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .tune_frequency(Band::A, Frequency::new(146_520_000))
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// tune_channel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tune_channel_switches_to_memory_mode() {
    // Radio starts in VFO mode — tune_channel must switch to Memory.
    let mut mock = MockTransport::new();
    // read_channel: verify channel is populated
    mock.expect(
        b"ME 021\r",
        b"ME 021,0146520000,0000600000,5,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // ensure_mode: query VM -> VFO (0), needs to switch
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    mock.expect(b"VM 0,1\r", b"VM 0,1\r");
    // recall channel
    mock.expect(b"MR 0,021\r", b"MR 0,021\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.tune_channel(Band::A, 21).await.unwrap();
}

#[tokio::test]
async fn tune_channel_already_in_memory_mode() {
    // Radio already in Memory mode — no VM write needed.
    let mut mock = MockTransport::new();
    // read_channel: verify channel is populated
    mock.expect(
        b"ME 005\r",
        b"ME 005,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // ensure_mode: query VM -> Memory (1), no switch needed
    mock.expect(b"VM 0\r", b"VM 0,1\r");
    // recall channel
    mock.expect(b"MR 0,005\r", b"MR 0,005\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.tune_channel(Band::A, 5).await.unwrap();
}

#[tokio::test]
async fn tune_channel_band_b() {
    // Tune Band B to a channel — confirms band index is passed correctly.
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 042\r",
        b"ME 042,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // Band B VM query — already in Memory mode
    mock.expect(b"VM 1\r", b"VM 1,1\r");
    mock.expect(b"MR 1,042\r", b"MR 1,042\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.tune_channel(Band::B, 42).await.unwrap();
}

// ---------------------------------------------------------------------------
// quick_tune
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quick_tune_sets_freq_mode_and_step() {
    let mut mock = MockTransport::new();
    // tune_frequency:
    //   ensure_mode: already VFO
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    //   FO read
    mock.expect(b"FO 0\r", FO_RESPONSE_145);
    //   FO write
    mock.expect(FO_WRITE_146520, FO_RESPONSE_146520);
    //   FQ verify
    mock.expect(b"FQ 0\r", FQ_RESPONSE_146520);
    // set_mode: FM = 0
    mock.expect(b"MD 0,0\r", b"MD 0,0\r");
    // set_step_size: Hz5000 = 0
    mock.expect(b"SF 0,0\r", b"SF 0,0\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .quick_tune(Band::A, 146_520_000, Mode::Fm, StepSize::Hz5000)
        .await
        .unwrap();
}

#[tokio::test]
async fn quick_tune_nfm_with_step_12500() {
    // Different mode and step size to confirm all three sub-calls forward
    // their parameters correctly.
    let mut mock = MockTransport::new();
    // tune_frequency sub-sequence (145 -> 145, same frequency, just verifying)
    mock.expect(b"VM 0\r", b"VM 0,0\r");
    mock.expect(b"FO 0\r", FO_RESPONSE_145);
    // FO write — frequency 145.000 MHz (same as readback, so write identical)
    mock.expect(
        b"FO 0,0145000000,0000600000,0,0,0,0,0,0,0,0,0,0,2,08,08,000,0,CQCQCQ,0,00\r",
        FO_RESPONSE_145,
    );
    mock.expect(b"FQ 0\r", b"FQ 0,0145000000\r");
    // set_mode: NFM = 6
    mock.expect(b"MD 0,6\r", b"MD 0,6\r");
    // set_step_size: Hz12500 = 5
    mock.expect(b"SF 0,5\r", b"SF 0,5\r");

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .quick_tune(Band::A, 145_000_000, Mode::Nfm, StepSize::Hz12500)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// connect_safe — TNC exit preamble
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_safe_sends_tnc_exit_preamble() {
    // connect_safe writes 4 raw payloads (CR, CR, ETX, "\rTC 1\r") and then
    // does a best-effort drain read — all ignored on error.  We use
    // expect_any_write so the mock accepts all writes without validation
    // (the preamble bytes are not CAT command/response pairs).
    let mut mock = MockTransport::new();
    mock.expect_any_write();

    // Should not panic or return an error.
    let radio = Radio::connect_safe(mock).await.unwrap();

    // Verify we got a usable Radio back — the mock has no exchanges left.
    drop(radio);
}

#[tokio::test]
async fn connect_safe_returns_functional_radio() {
    // After the preamble, connect_safe returns a usable Radio.
    // Verify by checking that subscribe() works (it requires a valid Radio).
    let mut mock = MockTransport::new();
    mock.expect_any_write();

    let radio = Radio::connect_safe(mock).await.unwrap();
    let _rx = radio.subscribe();
    // If we get here, connect_safe returned a valid Radio instance.
    drop(radio);
}

// ---------------------------------------------------------------------------
// modify_memory_page — integration test
// ---------------------------------------------------------------------------

fn mock_modify_page_sequence(
    mock: &mut MockTransport,
    page: u16,
    original: &[u8; 256],
    expected: &[u8; 256],
) {
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    let read_cmd = programming::build_read_command(page);
    mock.expect(&read_cmd, &build_w_response(page, original));
    mock.expect(&[programming::ACK], &[programming::ACK]);
    let write_cmd = programming::build_write_command(page, expected);
    mock.expect(&write_cmd, &[programming::ACK]);
    mock.expect(b"E", &[]);
}

#[tokio::test]
async fn modify_memory_page_applies_closure() {
    // Verify: enter MCP → read page → closure mutates data → write back → exit.
    let page: u16 = 0x0020;
    let byte_index: usize = 0x55;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 0xAB;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .modify_memory_page(page, |data| {
            data[byte_index] = 0xAB;
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn modify_memory_page_preserves_surrounding_bytes() {
    // A non-zero page pattern ensures only the target byte is changed.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x30;

    let mut original = [0xFFu8; 256];
    original[byte_index] = 0x00;
    let mut expected = original;
    expected[byte_index] = 0x01;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .modify_memory_page(page, |data| {
            data[byte_index] = 0x01;
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn modify_memory_page_rejects_factory_cal_page() {
    // Pages 0x07A1 and 0x07A2 are factory calibration — must be rejected
    // before entering MCP mode (no mock exchanges needed).
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await.unwrap();
    let result = radio.modify_memory_page(0x07A1, |_| {}).await;
    assert!(result.is_err());
}
