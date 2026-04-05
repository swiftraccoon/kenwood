//! Integration tests for radio system and scan methods.

use kenwood_thd75::protocol::programming;
use kenwood_thd75::radio::Radio;
use kenwood_thd75::transport::MockTransport;
use kenwood_thd75::types::Band;

/// Build a mock 261-byte W response for a page read in MCP programming mode.
fn build_w_response(page: u16, data: &[u8]) -> Vec<u8> {
    assert_eq!(data.len(), 256, "W response payload must be 256 bytes");
    let addr = page.to_be_bytes();
    let mut resp = vec![b'W', addr[0], addr[1], 0x00, 0x00];
    resp.extend_from_slice(data);
    resp
}

/// Set up MockTransport exchanges for a single-page modify_memory_page
/// call: enter MCP, read page, ACK, write modified page, ACK, exit.
///
/// `original` is the 256-byte page content the mock will return on read.
/// `expected` is the 256-byte page content the mock expects on write.
fn mock_modify_page_sequence(
    mock: &mut MockTransport,
    page: u16,
    original: &[u8; 256],
    expected: &[u8; 256],
) {
    // Enter programming mode.
    mock.expect(b"0M PROGRAM\r", b"0M\r");

    // Read page.
    let read_cmd = programming::build_read_command(page);
    mock.expect(&read_cmd, &build_w_response(page, original));

    // ACK exchange after read.
    mock.expect(&[programming::ACK], &[programming::ACK]);

    // Write modified page.
    let write_cmd = programming::build_write_command(page, expected);
    mock.expect(&write_cmd, &[programming::ACK]);

    // Exit programming mode.
    mock.expect(b"E", &[]);
}

#[tokio::test]
async fn lock_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"LC\r", b"LC 0\r");
    mock.expect(b"LC 1\r", b"LC 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_lock().await.unwrap());
    radio.set_lock(true).await.unwrap();
}

#[tokio::test]
async fn battery_level_read() {
    let mut mock = MockTransport::new();
    mock.expect(b"BL\r", b"BL 3\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert_eq!(
        radio.get_battery_level().await.unwrap(),
        kenwood_thd75::types::BatteryLevel::Full
    );
}

#[tokio::test]
async fn dual_band_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"DL\r", b"DL 1\r");
    mock.expect(b"DL 0\r", b"DL 0\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(radio.get_dual_band().await.unwrap());
    radio.set_dual_band(false).await.unwrap();
}

#[tokio::test]
async fn bluetooth_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"BT\r", b"BT 0\r");
    mock.expect(b"BT 1\r", b"BT 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_bluetooth().await.unwrap());
    radio.set_bluetooth(true).await.unwrap();
}

#[tokio::test]
async fn attenuator_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"RA 0\r", b"RA 0,0\r");
    mock.expect(b"RA 0,1\r", b"RA 0,1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    assert!(!radio.get_attenuator(Band::A).await.unwrap());
    radio.set_attenuator(Band::A, true).await.unwrap();
}

#[tokio::test]
async fn auto_info_control() {
    let mut mock = MockTransport::new();
    mock.expect(b"AI 1\r", b"AI 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_auto_info(true).await.unwrap();
}

#[tokio::test]
async fn scan_resume() {
    let mut mock = MockTransport::new();
    mock.expect(b"SR 1\r", b"SR 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_scan_resume(kenwood_thd75::types::ScanResumeMethod::CarrierOperated)
        .await
        .unwrap();
}

#[tokio::test]
async fn set_lock_full() {
    let mut mock = MockTransport::new();
    mock.expect(b"LC 1,2,1,0,1,0\r", b"LC 1\r");
    let mut radio = Radio::connect(mock).await.unwrap();
    radio
        .set_lock_full(
            true,
            kenwood_thd75::types::KeyLockType::try_from(2).unwrap(),
            true,
            false,
            true,
            false,
        )
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// MCP-based setting writes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn set_beep_via_mcp_enables() {
    // Offset 0x1071 => page 0x0010, byte index 0x71.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 1;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_beep_via_mcp(true).await.unwrap();
}

#[tokio::test]
async fn set_beep_via_mcp_disables() {
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    let mut original = [0u8; 256];
    original[byte_index] = 1; // currently enabled
    let mut expected = original;
    expected[byte_index] = 0;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_beep_via_mcp(false).await.unwrap();
}

#[tokio::test]
async fn set_beep_volume_via_mcp() {
    // Offset 0x1072 => page 0x0010, byte index 0x72.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x72;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 5;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_beep_volume_via_mcp(5).await.unwrap();
}

#[tokio::test]
async fn set_vox_via_mcp_enables() {
    // Offset 0x101B => page 0x0010, byte index 0x1B.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x1B;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 1;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_vox_via_mcp(true).await.unwrap();
}

#[tokio::test]
async fn set_lock_via_mcp_enables() {
    // Offset 0x1060 => page 0x0010, byte index 0x60.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x60;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 1;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_lock_via_mcp(true).await.unwrap();
}

#[tokio::test]
async fn set_bluetooth_via_mcp_enables() {
    // Offset 0x1078 => page 0x0010, byte index 0x78.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x78;

    let original = [0u8; 256];
    let mut expected = original;
    expected[byte_index] = 1;

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_bluetooth_via_mcp(true).await.unwrap();
}

#[tokio::test]
async fn set_beep_via_mcp_preserves_other_bytes() {
    // The page should be read-modify-write: only the target byte changes,
    // all other bytes in the page are preserved.
    let page: u16 = 0x0010;
    let byte_index: usize = 0x71;

    // Fill original with non-zero pattern to verify preservation.
    let mut original = [0xABu8; 256];
    original[byte_index] = 0x00; // beep currently off

    let mut expected = original;
    expected[byte_index] = 0x01; // beep turning on

    let mut mock = MockTransport::new();
    mock_modify_page_sequence(&mut mock, page, &original, &expected);

    let mut radio = Radio::connect(mock).await.unwrap();
    radio.set_beep_via_mcp(true).await.unwrap();
}
