//! Integration tests for radio system and scan methods.

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::{
    BacklightControl, Band, BandMode, LinkedVolumeLevel, RadioRegion, RegularChannel,
    ScanResumeMethod,
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
type BoxErr = Box<dyn std::error::Error>;

#[tokio::test]
async fn serial_information_is_typed_at_the_radio_boundary() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AE\r", b"AE C3C10368,K01\r");
    let mut radio = Radio::new(mock);

    let information = radio.get_serial_information().await?;
    assert_eq!(information.serial_number().as_str(), "C3C10368");
    assert_eq!(information.model_code().as_str(), "K01");
    Ok(())
}

#[tokio::test]
async fn radio_type_is_typed_at_the_radio_boundary() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"TY\r", b"TY K,F\r");
    let mut radio = Radio::new(mock);

    let radio_type = radio.get_radio_type().await?;
    assert_eq!(radio_type.region(), RadioRegion::UnitedStates);
    assert_eq!(radio_type.hardware_variant().as_raw(), 15);
    Ok(())
}

/// Build a mock 261-byte W response for a page read in MCP programming mode.
fn build_w_response(page: u16, data: &[u8]) -> Result<Vec<u8>, BoxErr> {
    if data.len() != 256 {
        return Err(format!("W response payload must be 256 bytes, got {}", data.len()).into());
    }
    let [addr_hi, addr_lo] = page.to_be_bytes();
    let mut resp = vec![b'W', addr_hi, addr_lo, 0x00, 0x00];
    resp.extend_from_slice(data);
    Ok(resp)
}

/// Set up `MockTransport` exchanges for a single-page `modify_memory_page`
/// call: enter MCP, read page, ACK, write modified page, ACK, exit.
///
/// `original` is the 256-byte page content the mock will return on read.
/// `expected` is the 256-byte page content the mock expects on write.
fn mock_modify_page_sequence(
    mock: &mut MockTransport,
    page: u16,
    original: &[u8; 256],
    expected: &[u8; 256],
) -> Result<(), BoxErr> {
    // Enter programming mode.
    mock.expect(b"\r0M PROGRAM\r", b"0M\r");

    // Read page.
    let read_cmd = programming::build_read_command(programming::McpPage::new(page)?);
    mock.expect(&read_cmd, &build_w_response(page, original)?);

    // ACK exchange after read.
    mock.expect(&[programming::ACK], &[programming::ACK]);

    // Write modified page.
    let write_cmd =
        programming::build_write_command(programming::WritableMcpPage::new(page)?, expected);
    mock.expect(&write_cmd, &[programming::ACK]);

    // Verify read-back returns the modified page.
    mock.expect(&read_cmd, &build_w_response(page, expected)?);
    mock.expect(&[programming::ACK], &[programming::ACK]);

    // Exit programming mode, then the exit path reconnects.
    mock.expect(b"E", &[programming::ACK]);
    mock.expect_reopen(Ok(()));
    mock.expect(b"ID\r", b"ID TH-D75\r");
    Ok(())
}

#[tokio::test]
async fn backlight_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"LC\r", b"LC 2\r");
    mock.expect(b"LC 3\r", b"LC 3\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_backlight_control().await?, BacklightControl::Auto);
    radio
        .set_backlight_control(BacklightControl::AutoDcIn)
        .await?;
    Ok(())
}

#[tokio::test]
async fn battery_level_read() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BL\r", b"BL 3\r");
    let mut radio = Radio::new(mock);
    assert_eq!(
        radio.get_battery_level().await?,
        kenwood_thd75::types::BatteryLevel::Full
    );
    Ok(())
}

#[tokio::test]
async fn band_mode_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DL\r", b"DL 0\r");
    mock.expect(b"DL 1\r", b"DL 1\r");
    let mut radio = Radio::new(mock);
    assert_eq!(radio.get_band_mode().await?, BandMode::Dual);
    radio.set_band_mode(BandMode::Single).await?;
    Ok(())
}

#[tokio::test]
async fn bluetooth_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BT\r", b"BT 0\r");
    mock.expect(b"BT 1\r", b"BT 1\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_bluetooth().await?);
    radio.set_bluetooth(true).await?;
    Ok(())
}

#[tokio::test]
async fn attenuator_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"RA 0\r", b"RA 0,0\r");
    mock.expect(b"RA 0,1\r", b"RA 0,1\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_attenuator(Band::A).await?);
    radio.set_attenuator(Band::A, true).await?;
    Ok(())
}

#[tokio::test]
async fn auto_info_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AI\r", b"AI 0\r");
    mock.expect(b"AI 1\r", b"AI 1\r");
    let mut radio = Radio::new(mock);
    assert!(!radio.get_auto_info().await?);
    radio.set_auto_info(true).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP-based setting writes
// ---------------------------------------------------------------------------

/// Return a 256-byte page with `page[idx] = value`, cloning `base`.
/// Returns an error if `idx` is out of range (256-byte array, so it can't happen
/// in practice, but the Result preserves the no-panic policy).
fn patch_page(base: &[u8; 256], idx: usize, value: u8) -> Result<[u8; 256], BoxErr> {
    let mut out = *base;
    let slot: &mut u8 = out.get_mut(idx).ok_or_else(|| -> BoxErr {
        format!("patch_page: idx {idx} out of 256-byte page").into()
    })?;
    *slot = value;
    Ok(out)
}

#[tokio::test(start_paused = true)]
async fn set_analog_scan_resume_via_mcp_changes_only_menu_130() -> TestResult {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x0C;
    let original = [0xA5; 256];
    let expected = patch_page(
        &original,
        byte_index,
        ScanResumeMethod::CarrierOperated.as_raw(),
    )?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio
        .set_analog_scan_resume_via_mcp(ScanResumeMethod::CarrierOperated)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_digital_scan_resume_via_mcp_changes_only_menu_131() -> TestResult {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x0D;
    let original = [0x5A; 256];
    let expected = patch_page(&original, byte_index, ScanResumeMethod::Seek.as_raw())?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio
        .set_digital_scan_resume_via_mcp(ScanResumeMethod::Seek)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_fm_radio_via_mcp_changes_only_menu_700() -> TestResult {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x40;
    let original = [0xA5; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_fm_radio_via_mcp(true).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_beep_via_mcp_enables() -> TestResult {
    // Offset 0x1071 => page 0x0010, byte index 0x71.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_beep_via_mcp(true).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_beep_via_mcp_disables() -> TestResult {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let original = patch_page(&[0u8; 256], byte_index, 1)?; // currently enabled
    let expected = patch_page(&original, byte_index, 0)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_beep_via_mcp(false).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_beep_volume_via_mcp() -> TestResult {
    // Offset 0x1072 => page 0x0010, byte index 0x72.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 5)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio
        .set_beep_volume_via_mcp(LinkedVolumeLevel::try_from(5)?)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_vox_via_mcp_enables() -> TestResult {
    // Offset 0x101B => page 0x0010, byte index 0x1B.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x1B;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_vox_via_mcp(true).await?;
    Ok(())
}

// NOTE: `set_lock_via_mcp` no longer exists. MCP offset 0x1060 and CAT LC
// both control `radio.BacklightControl`; no key-lock state operation is
// currently verified.

#[tokio::test(start_paused = true)]
async fn set_bluetooth_via_mcp_enables() -> TestResult {
    // Offset 0x1078 => page 0x0010, byte index 0x78.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x78;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_bluetooth_via_mcp(true).await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_beep_via_mcp_preserves_other_bytes() -> TestResult {
    // The page should be read-modify-write: only the target byte changes,
    // all other bytes in the page are preserved.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    // Fill original with non-zero pattern to verify preservation.
    let original = patch_page(&[0xABu8; 256], byte_index, 0x00)?; // beep currently off
    let expected = patch_page(&original, byte_index, 0x01)?; // beep turning on

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio.set_beep_via_mcp(true).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// frequency_down: steps down and reads back frequency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn frequency_down() -> TestResult {
    let mut mock = MockTransport::new();
    // Resolve the current context, then issue the only accepted bare DW action.
    mock.expect(b"BC\r", b"BC 0\r");
    mock.expect(b"DW\r", b"DW\r");
    // Then we read back the new frequency.
    mock.expect(b"FQ 0\r", b"FQ 0,0144000000\r");
    let mut radio = Radio::new(mock);
    let frequency = radio.frequency_down().await?;
    assert_eq!(frequency.as_hz(), 144_000_000);
    Ok(())
}

#[tokio::test]
async fn frequency_down_blind() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DW\r", b"DW\r");
    let mut radio = Radio::new(mock);
    radio.frequency_down_blind().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// set_beep_volume_via_mcp: typed domain and boundary success
// ---------------------------------------------------------------------------

#[test]
fn beep_volume_type_rejects_out_of_range() {
    assert!(LinkedVolumeLevel::try_from(8).is_err());
}

#[tokio::test(start_paused = true)]
async fn set_beep_volume_boundary_max() -> TestResult {
    // Volume 7 is the maximum valid value, so it should succeed and do an MCP write.
    // Offset 0x1072 => page 0x0010, byte index 0x72.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 7)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio
        .set_beep_volume_via_mcp(LinkedVolumeLevel::try_from(7)?)
        .await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn set_beep_volume_boundary_min() -> TestResult {
    // Volume 0 is the minimum valid value, so it should succeed.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = patch_page(&[0u8; 256], byte_index, 5)?; // currently at 5
    let expected = patch_page(&original, byte_index, 0)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::new(mock);
    radio
        .set_beep_volume_via_mcp(LinkedVolumeLevel::VOLUME_LINK)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// read_regular_channel_records: skip-N integration test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_regular_channel_records_skips_empty_and_not_available() -> TestResult {
    // Verifies that read_regular_channel_records:
    //   - skips channels returning N (not available / unprogrammed)
    //   - skips channels with zero frequency
    //   - returns only populated channels with their correct numbers
    let mut mock = MockTransport::new();
    // Channel 0: not available (N)
    mock.expect(b"ME 000\r", b"N\r");
    // Channel 1: populated at 146.520 MHz
    mock.expect(
        b"ME 001\r",
        b"ME 001,0146520000,0000600000,5,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // Channel 2: zero frequency (empty, should be skipped)
    mock.expect(
        b"ME 002\r",
        b"ME 002,0000000000,0000000000,0,0,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );
    // Channel 3: not available (N)
    mock.expect(b"ME 003\r", b"N\r");
    // Channel 4: populated at 440.000 MHz
    mock.expect(
        b"ME 004\r",
        b"ME 004,0440000000,0005000000,5,2,0,0,0,0,0,0,0,0,0,0,08,08,000,0,,0,00,0\r",
    );

    let mut radio = Radio::new(mock);
    let channels = radio
        .read_regular_channel_records(RegularChannel::range_inclusive(
            RegularChannel::new(0)?,
            RegularChannel::new(4)?,
        ))
        .await?;

    assert_eq!(channels.len(), 2, "only 2 populated channels expected");
    let first = channels.first().ok_or("channels[0] missing")?;
    assert_eq!(
        first.0,
        RegularChannel::new(1)?,
        "first result should be channel 1"
    );
    assert_eq!(
        first.1.channel.receive_frequency.as_hz(),
        146_520_000,
        "channel 1 frequency"
    );
    assert!(!first.1.split);
    assert!(!first.1.scan_lockout);
    let second = channels.get(1).ok_or("channels[1] missing")?;
    assert_eq!(
        second.0,
        RegularChannel::new(4)?,
        "second result should be channel 4"
    );
    assert_eq!(
        second.1.channel.receive_frequency.as_hz(),
        440_000_000,
        "channel 4 frequency"
    );
    Ok(())
}
