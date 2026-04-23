//! Integration tests for radio system and scan methods.

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::Band;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type BoxErr = Box<dyn std::error::Error>;

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
    mock.expect(b"0M PROGRAM\r", b"0M\r");

    // Read page.
    let read_cmd = programming::build_read_command(page);
    mock.expect(&read_cmd, &build_w_response(page, original)?);

    // ACK exchange after read.
    mock.expect(&[programming::ACK], &[programming::ACK]);

    // Write modified page.
    let write_cmd = programming::build_write_command(page, expected);
    mock.expect(&write_cmd, &[programming::ACK]);

    // Exit programming mode.
    mock.expect(b"E", &[]);
    Ok(())
}

#[tokio::test]
async fn lock_control() -> TestResult {
    let mut mock = MockTransport::new();
    // Wire LC 0 = locked on D75 (inverted); get_lock() returns true.
    mock.expect(b"LC\r", b"LC 0\r");
    // set_lock(false) → unlocked → sends wire LC 1 (inverted).
    mock.expect(b"LC 1\r", b"LC 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(radio.get_lock().await?);
    radio.set_lock(false).await?;
    Ok(())
}

#[tokio::test]
async fn battery_level_read() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BL\r", b"BL 3\r");
    let mut radio = Radio::connect(mock).await?;
    assert_eq!(
        radio.get_battery_level().await?,
        kenwood_thd75::types::BatteryLevel::Full
    );
    Ok(())
}

#[tokio::test]
async fn dual_band_control() -> TestResult {
    let mut mock = MockTransport::new();
    // Wire DL 0 = dual band on D75 (inverted); get_dual_band() returns true.
    mock.expect(b"DL\r", b"DL 0\r");
    // set_dual_band(false) → single band → sends wire DL 1 (inverted).
    mock.expect(b"DL 1\r", b"DL 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(radio.get_dual_band().await?);
    radio.set_dual_band(false).await?;
    Ok(())
}

#[tokio::test]
async fn bluetooth_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"BT\r", b"BT 0\r");
    mock.expect(b"BT 1\r", b"BT 1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_bluetooth().await?);
    radio.set_bluetooth(true).await?;
    Ok(())
}

#[tokio::test]
async fn attenuator_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"RA 0\r", b"RA 0,0\r");
    mock.expect(b"RA 0,1\r", b"RA 0,1\r");
    let mut radio = Radio::connect(mock).await?;
    assert!(!radio.get_attenuator(Band::A).await?);
    radio.set_attenuator(Band::A, true).await?;
    Ok(())
}

#[tokio::test]
async fn auto_info_control() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AI 1\r", b"AI 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio.set_auto_info(true).await?;
    Ok(())
}

#[tokio::test]
async fn scan_resume() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"SR 1\r", b"SR 1\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_scan_resume(kenwood_thd75::types::ScanResumeMethod::CarrierOperated)
        .await?;
    Ok(())
}

#[tokio::test]
async fn set_lock_full() -> TestResult {
    let mut mock = MockTransport::new();
    // set_lock_full(true, ...) → locked=true inverted to wire 0.
    mock.expect(b"LC 0,2,1,0,1,0\r", b"LC 0\r");
    let mut radio = Radio::connect(mock).await?;
    radio
        .set_lock_full(
            true,
            kenwood_thd75::types::KeyLockType::try_from(2)?,
            true,
            false,
            true,
            false,
        )
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// MCP-based setting writes
// ---------------------------------------------------------------------------

/// Return a 256-byte page with `page[idx] = value`, cloning `base`.
/// Returns an error if `idx` is out of range (256-byte array — can't happen in practice,
/// but the Result preserves the no-panic policy).
fn patch_page(base: &[u8; 256], idx: usize, value: u8) -> Result<[u8; 256], BoxErr> {
    let mut out = *base;
    let slot: &mut u8 = out.get_mut(idx).ok_or_else(|| -> BoxErr {
        format!("patch_page: idx {idx} out of 256-byte page").into()
    })?;
    *slot = value;
    Ok(out)
}

#[tokio::test]
async fn set_beep_via_mcp_enables() -> TestResult {
    // Offset 0x1071 => page 0x0010, byte index 0x71.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_via_mcp(true).await?;
    Ok(())
}

#[tokio::test]
async fn set_beep_via_mcp_disables() -> TestResult {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let original = patch_page(&[0u8; 256], byte_index, 1)?; // currently enabled
    let expected = patch_page(&original, byte_index, 0)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_via_mcp(false).await?;
    Ok(())
}

#[tokio::test]
async fn set_beep_volume_via_mcp() -> TestResult {
    // Offset 0x1072 => page 0x0010, byte index 0x72.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 5)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_volume_via_mcp(5).await?;
    Ok(())
}

#[tokio::test]
async fn set_vox_via_mcp_enables() -> TestResult {
    // Offset 0x101B => page 0x0010, byte index 0x1B.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x1B;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_vox_via_mcp(true).await?;
    Ok(())
}

#[tokio::test]
async fn set_lock_via_mcp_enables() -> TestResult {
    // Offset 0x1060 => page 0x0010, byte index 0x60.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x60;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_lock_via_mcp(true).await?;
    Ok(())
}

#[tokio::test]
async fn set_bluetooth_via_mcp_enables() -> TestResult {
    // Offset 0x1078 => page 0x0010, byte index 0x78.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x78;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 1)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_bluetooth_via_mcp(true).await?;
    Ok(())
}

#[tokio::test]
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

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_via_mcp(true).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// frequency_down — steps down and reads back frequency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn frequency_down() -> TestResult {
    let mut mock = MockTransport::new();
    // DW 0 steps frequency down on Band A; radio echoes DW\r.
    mock.expect(b"DW 0\r", b"DW\r");
    // Then we read back the new frequency.
    mock.expect(b"FQ 0\r", b"FQ 0,0144000000\r");
    let mut radio = Radio::connect(mock).await?;
    let ch = radio.frequency_down(Band::A).await?;
    assert_eq!(ch.rx_frequency.as_hz(), 144_000_000);
    Ok(())
}

#[tokio::test]
async fn frequency_down_blind() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"DW 0\r", b"DW\r");
    let mut radio = Radio::connect(mock).await?;
    radio.frequency_down_blind(Band::A).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// set_beep_volume_via_mcp — out-of-range rejection and boundary success
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_beep_volume_rejects_out_of_range() -> TestResult {
    // Volume 8 is out of range (0-7) — should fail before sending anything.
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    let result = radio.set_beep_volume_via_mcp(8).await;
    assert!(
        result.is_err(),
        "expected out-of-range volume to fail: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn set_beep_volume_boundary_max() -> TestResult {
    // Volume 7 is the maximum valid value — should succeed and do an MCP write.
    // Offset 0x1072 => page 0x0010, byte index 0x72.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = [0u8; 256];
    let expected = patch_page(&original, byte_index, 7)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_volume_via_mcp(7).await?;
    Ok(())
}

#[tokio::test]
async fn set_beep_volume_boundary_min() -> TestResult {
    // Volume 0 is the minimum valid value — should succeed.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = patch_page(&[0u8; 256], byte_index, 5)?; // currently at 5
    let expected = patch_page(&original, byte_index, 0)?;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected)?;

    let mut radio = Radio::connect(mock).await?;
    radio.set_beep_volume_via_mcp(0).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// read_channels — skip-N integration test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_channels_skips_empty_and_not_available() -> TestResult {
    // Verifies that read_channels:
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

    let mut radio = Radio::connect(mock).await?;
    let channels = radio.read_channels(0..5).await?;

    assert_eq!(channels.len(), 2, "only 2 populated channels expected");
    let first = channels.first().ok_or("channels[0] missing")?;
    assert_eq!(first.0, 1, "first result should be channel 1");
    assert_eq!(
        first.1.rx_frequency.as_hz(),
        146_520_000,
        "channel 1 frequency"
    );
    let second = channels.get(1).ok_or("channels[1] missing")?;
    assert_eq!(second.0, 4, "second result should be channel 4");
    assert_eq!(
        second.1.rx_frequency.as_hz(),
        440_000_000,
        "channel 4 frequency"
    );
    Ok(())
}
