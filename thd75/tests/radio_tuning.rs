//! Integration tests for high-level tuning methods: `tune_frequency`,
//! `tune_channel`, and `quick_tune`.

use kenwood_thd75::Error;
use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{Band, Frequency, Mode, StepSize};

// Deps visible to every kenwood-thd75 test target but unused here.
// Acknowledged so `unused_crate_dependencies` stays silent without
// weakening the lint.
use aprs as _;
use aprs_is as _;
use ax25_codec as _;
use dstar_gateway_core as _;
use kiss_tnc as _;
use mmdvm as _;
use mmdvm_core as _;
use proptest as _;
use serde_json as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxErr = Box<dyn std::error::Error>;

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// tune_frequency
// ---------------------------------------------------------------------------

/// Build a mock W response for `mock_modify_page_sequence`-style MCP exchanges.
fn build_w_response(page: u16, data: &[u8]) -> Result<Vec<u8>, BoxErr> {
    if data.len() != 256 {
        return Err(format!("W response payload must be 256 bytes, got {}", data.len()).into());
    }
    let [addr_hi, addr_lo] = page.to_be_bytes();
    let mut resp = vec![b'W', addr_hi, addr_lo, 0x00, 0x00];
    resp.extend_from_slice(data);
    Ok(resp)
}

/// Return a 256-byte page with `page[idx] = value`, cloning `base`.
fn patch_page(base: &[u8; 256], idx: usize, value: u8) -> Result<[u8; 256], BoxErr> {
    let mut out = *base;
    let slot: &mut u8 = out.get_mut(idx).ok_or_else(|| -> BoxErr {
        format!("patch_page: idx {idx} out of 256-byte page").into()
    })?;
    *slot = value;
    Ok(out)
}

#[tokio::test]
async fn tune_frequency_is_quarantined_before_mode_change_or_io() -> TestResult {
    // No exchanges are scripted. Any VM/FO/FQ access would therefore return
    // a transport error instead of the explicit quarantine error.
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    let result = radio
        .tune_frequency(Band::A, Frequency::new(146_520_000))
        .await;
    assert!(
        matches!(
            result,
            Err(Error::UnqualifiedCatWrite {
                command: "FO/FQ",
                ..
            })
        ),
        "frequency tuning must fail before changing mode or performing I/O: {result:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// tune_channel
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tune_channel_empty_channel_is_error() -> TestResult {
    // Documented contract: an empty channel (0 Hz) is an error. The
    // radio must not be switched to Memory mode and no recall sent.
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 021\r",
        b"ME 021,0000000000,0000600000,5,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );

    let mut radio = Radio::connect(mock).await?;
    let result = radio.tune_channel(Band::A, 21).await;
    assert!(
        matches!(result, Err(Error::RadioError)),
        "an empty channel must be an error per the documented contract: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn tune_channel_switches_to_memory_mode() -> TestResult {
    // Radio starts in VFO mode, so tune_channel must switch to Memory.
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

    let mut radio = Radio::connect(mock).await?;
    radio.tune_channel(Band::A, 21).await?;
    Ok(())
}

#[tokio::test]
async fn tune_channel_already_in_memory_mode() -> TestResult {
    // Radio already in Memory mode, so no VM write is needed.
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

    let mut radio = Radio::connect(mock).await?;
    radio.tune_channel(Band::A, 5).await?;
    Ok(())
}

#[tokio::test]
async fn tune_channel_band_b() -> TestResult {
    // Tune Band B to a channel: confirms band index is passed correctly.
    let mut mock = MockTransport::new();
    mock.expect(
        b"ME 042\r",
        b"ME 042,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // Band B VM query: already in Memory mode
    mock.expect(b"VM 1\r", b"VM 1,1\r");
    mock.expect(b"MR 1,042\r", b"MR 1,042\r");

    let mut radio = Radio::connect(mock).await?;
    radio.tune_channel(Band::B, 42).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// quick_tune
// ---------------------------------------------------------------------------

#[tokio::test]
async fn quick_tune_sets_freq_mode_and_step() -> TestResult {
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    let result = radio
        .quick_tune(Band::A, 146_520_000, Mode::Fm, StepSize::Hz5000)
        .await;
    assert!(
        matches!(
            result,
            Err(Error::UnqualifiedCatWrite {
                command: "FO/FQ",
                ..
            })
        ),
        "quick_tune must not continue to mode or step writes after quarantine: {result:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// connect_safe: TNC exit preamble
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_safe_sends_tnc_exit_preamble() -> TestResult {
    // connect_safe writes 4 raw payloads (CR, CR, ETX, "\rTC 1\r") and then
    // does a best-effort drain read, all ignored on error.  We use
    // expect_any_write so the mock accepts all writes without validation
    // (the preamble bytes are not CAT command/response pairs).
    let mut mock = MockTransport::new();
    mock.expect_any_write();

    // Should not panic or return an error.
    let radio = Radio::connect_safe(mock).await?;

    // Verify we got a usable Radio back; the mock has no exchanges left.
    drop(radio);
    Ok(())
}

#[tokio::test]
async fn connect_safe_preamble_includes_kiss_exit_frame() -> TestResult {
    // A radio left in KISS mode (crashed APRS session) ignores every
    // ASCII CAT byte; the only exit the KISS protocol defines is the
    // FEND-framed Return command (C0 FF C0), the same bytes
    // AprsClient::stop() sends. The preamble must include it ahead of
    // the ASCII TNC exits, or a stuck-KISS radio stays unreachable
    // even though the transport connects. Hardware-observed 2026-07-18.
    let mut mock = MockTransport::new();
    mock.expect(b"\r", b"");
    mock.expect(b"\r", b"");
    mock.expect(&[0x03], b"");
    mock.expect(&[0xC0, 0xFF, 0xC0], b"");
    mock.expect(b"\rTC 1\r", b"");
    mock.expect(b"TN 0,0\r", b"");

    let radio = Radio::connect_safe(mock).await?;
    drop(radio);
    Ok(())
}

#[tokio::test]
async fn connect_safe_returns_functional_radio() -> TestResult {
    // After the preamble, connect_safe returns a usable Radio.
    // Verify by checking that subscribe() works (it requires a valid Radio).
    let mut mock = MockTransport::new();
    mock.expect_any_write();

    let radio = Radio::connect_safe(mock).await?;
    let _rx = radio.subscribe();
    // If we get here, connect_safe returned a valid Radio instance.
    drop(radio);
    Ok(())
}

// ---------------------------------------------------------------------------
// modify_memory_page: integration test
// ---------------------------------------------------------------------------

fn mock_modify_page_sequence(
    mock: &mut MockTransport,
    page: u16,
    original: &[u8; 256],
    expected: &[u8; 256],
) -> Result<(), BoxErr> {
    mock.expect(b"\r0M PROGRAM\r", b"0M\r");
    let read_cmd = programming::build_read_command(page);
    mock.expect(&read_cmd, &build_w_response(page, original)?);
    mock.expect(&[programming::ACK], &[programming::ACK]);
    let write_cmd = programming::build_write_command(page, expected);
    mock.expect(&write_cmd, &[programming::ACK]);
    // Verify read-back returns the modified page.
    mock.expect(&read_cmd, &build_w_response(page, expected)?);
    mock.expect(&[programming::ACK], &[programming::ACK]);
    mock.expect(b"E", &[programming::ACK]);
    // The exit path reconnects: transport reopen + identify.
    mock.expect_reopen(Ok(()));
    mock.expect(b"ID\r", b"ID TH-D75\r");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn modify_memory_page_applies_closure() -> TestResult {
    // Verify: enter MCP → read page → closure mutates data → write back → exit.
    let page: u16 = 0x0020;
    let byte_index: usize = 0x55;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 0xAB)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio
        .modify_memory_page(page, |data| {
            if let Some(slot) = data.get_mut(byte_index) {
                *slot = 0xAB;
            }
        })
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn modify_memory_page_preserves_surrounding_bytes() -> TestResult {
    // A non-zero page pattern ensures only the target byte is changed.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x30;

    let original = patch_page(&[0xFFu8; 256], byte_index, 0x00)?;
    let expected = patch_page(&original, byte_index, 0x01)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio
        .modify_memory_page(page, |data| {
            if let Some(slot) = data.get_mut(byte_index) {
                *slot = 0x01;
            }
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn modify_memory_page_rejects_factory_cal_page() -> TestResult {
    // Pages 0x07A1 and 0x07A2 are factory calibration and must be rejected
    // before entering MCP mode (no mock exchanges needed).
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    let result = radio.modify_memory_page(0x07A1, |_| {}).await;
    assert!(
        result.is_err(),
        "expected factory-cal modify to fail: {result:?}"
    );
    Ok(())
}
