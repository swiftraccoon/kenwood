//! Reconnect behavior against the scripted mock transport.
//!
//! Covers the link-state watch channel, `Radio::reconnect`'s
//! reopen → identify → restore sequence, and the fail-fast rule:
//! a command in flight when the link drops stays failed.

use kenwood_thd75::error::Error;
use kenwood_thd75::radio::{FirmwareProfile, LinkState, Radio};
use kenwood_thd75::transport::{MockTransport, Transport};

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

/// Script the identify exchange `Radio::reconnect` performs after a
/// successful transport reopen.
fn expect_identify(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TH-D75\r");
}

#[tokio::test]
async fn reconnect_reopens_and_reidentifies() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);
    let mut radio = Radio::connect(mock).await?;
    radio.reconnect().await?;
    assert_eq!(*radio.link_state().borrow(), LinkState::Up);
    Ok(())
}

#[tokio::test]
async fn dropped_link_fails_command_and_reconnect_restores() -> TestResult {
    let mut mock = MockTransport::new();
    // The firmware query meets a dead link (EOF mid-read)...
    mock.expect_eof(b"FV\r");
    // ...then reopen succeeds and identify answers.
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);

    let mut radio = Radio::connect(mock).await?;
    let watch = radio.link_state();
    assert_eq!(*watch.borrow(), LinkState::Up);

    // Fail-fast: the in-flight command errors and is never replayed.
    let r = radio.get_firmware_version().await;
    assert!(r.is_err(), "expected transport failure, got {r:?}");
    assert_eq!(*watch.borrow(), LinkState::Down);

    radio.reconnect().await?;
    assert_eq!(*watch.borrow(), LinkState::Up);
    Ok(())
}

#[tokio::test]
async fn write_failure_marks_link_down() -> TestResult {
    // No expectations scripted: the very first write fails.
    let mock = MockTransport::new();
    let mut radio = Radio::connect(mock).await?;
    let r = radio.get_firmware_version().await;
    assert!(r.is_err(), "expected write failure, got {r:?}");
    assert_eq!(*radio.link_state().borrow(), LinkState::Down);
    Ok(())
}

#[tokio::test]
async fn reconnect_restores_auto_info_only_if_enabled() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"AI 1\r", b"AI 1\r");
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);
    // Restored exactly once after identify.
    mock.expect(b"AI 1\r", b"AI 1\r");

    let mut radio = Radio::connect(mock).await?;
    radio.set_auto_info(true).await?;
    radio.reconnect().await?;
    Ok(())
}

#[tokio::test]
async fn reconnect_without_session_state_sends_only_identify() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);
    let mut radio = Radio::connect(mock).await?;
    radio.reconnect().await?;
    // The strict mock would have rejected any extra command (AI/GP/GS),
    // so reaching this point proves nothing else was sent.
    Ok(())
}

#[tokio::test]
async fn reconnect_preserves_azimuth_firmware_profile() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"FV\r", b"FV 1.03.AZM\r");
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);

    let mut radio = Radio::connect(mock).await?;
    assert_eq!(radio.get_firmware_version().await?, "1.03.AZM");
    radio.reconnect().await?;
    assert_eq!(
        radio.firmware_profile(),
        Some(FirmwareProfile::AzimuthAutomation)
    );
    let gateway = radio.get_gateway().await;
    assert!(
        matches!(
            gateway,
            Err(Error::CommandUnavailableOnFirmware {
                command: "GW",
                ref firmware,
            }) if firmware == "1.03.AZM"
        ),
        "cached AZM profile should reject GW after reconnect, got {gateway:?}"
    );
    Ok(())
}

#[tokio::test]
async fn reconnect_restores_gps_config() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"GP 1,1\r", b"GP 1,1\r");
    mock.expect_reopen(Ok(()));
    expect_identify(&mut mock);
    mock.expect(b"GP 1,1\r", b"GP 1,1\r");

    let mut radio = Radio::connect(mock).await?;
    radio.set_gps_config(true, true).await?;
    radio.reconnect().await?;
    Ok(())
}

#[tokio::test]
async fn failed_reopen_leaves_link_down() -> TestResult {
    use kenwood_thd75::error::TransportError;

    let mut mock = MockTransport::new();
    mock.expect_eof(b"FV\r");
    mock.expect_reopen(Err(TransportError::NotFound));
    let mut radio = Radio::connect(mock).await?;
    let cmd = radio.get_firmware_version().await;
    assert!(cmd.is_err(), "expected transport failure, got {cmd:?}");
    let r = radio.reconnect().await;
    assert!(r.is_err(), "expected reopen failure, got {r:?}");
    assert_eq!(*radio.link_state().borrow(), LinkState::Down);
    Ok(())
}

/// `MockTransport::reopen` consumes its script through the `Transport`
/// trait when driven by `Radio::reconnect`; this pins the re-export
/// path tests rely on.
#[tokio::test]
async fn mock_reopen_reachable_through_trait() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect_reopen(Ok(()));
    mock.reopen().await?;
    Ok(())
}

/// A transport whose write never resolves, modelling a platform write
/// wedged against a dying link (macOS `IOBluetooth` `writeSync:` on a
/// rebooting radio never returns).
#[derive(Debug)]
struct WedgedWriteTransport;

impl Transport for WedgedWriteTransport {
    async fn write(&mut self, _d: &[u8]) -> Result<(), kenwood_thd75::error::TransportError> {
        std::future::pending().await
    }
    async fn read(&mut self, _b: &mut [u8]) -> Result<usize, kenwood_thd75::error::TransportError> {
        std::future::pending().await
    }
    async fn close(&mut self) -> Result<(), kenwood_thd75::error::TransportError> {
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn wedged_write_times_out_and_marks_link_down() -> TestResult {
    use kenwood_thd75::error::Error;

    let mut radio = Radio::connect(WedgedWriteTransport).await?;
    let r = radio.get_firmware_version().await;
    assert!(
        matches!(r, Err(Error::Timeout(_))),
        "a wedged write must surface as a timeout, got {r:?}"
    );
    assert_eq!(*radio.link_state().borrow(), LinkState::Down);
    Ok(())
}

/// Build the 261-byte `W` frame the radio answers a page read with.
fn build_w_response(page: u16, data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    if data.len() != 256 {
        return Err(format!("W response payload must be 256 bytes, got {}", data.len()).into());
    }
    let [addr_hi, addr_lo] = page.to_be_bytes();
    let mut resp = vec![b'W', addr_hi, addr_lo, 0x00, 0x00];
    resp.extend_from_slice(data);
    Ok(resp)
}

/// Leaving MCP programming mode drops the USB link; the exit path now
/// waits out the settle period, reopens the transport, and
/// re-identifies, so CAT works immediately after any MCP operation.
#[tokio::test(start_paused = true)]
async fn mcp_exit_reconnects_and_cat_works() -> TestResult {
    use kenwood_thd75::protocol::programming;

    let page: u16 = 0x0020;
    let mut mock = MockTransport::new();
    // Enter programming mode, read one page, ACK, exit.
    mock.expect(b"\r0M PROGRAM\r", b"0M\r");
    let cmd = programming::build_read_command(page);
    mock.expect(&cmd, &build_w_response(page, &[0x5Au8; 256])?);
    mock.expect(&[programming::ACK], &[programming::ACK]);
    mock.expect(b"E", &[programming::ACK]);
    // The exit path reconnects: reopen + identify...
    mock.expect_reopen(Ok(()));
    mock.expect(b"ID\r", b"ID TH-D75\r");
    // ...and CAT answers immediately afterwards.
    mock.expect(b"FV\r", b"FV 1.03.000\r");

    let mut radio = Radio::connect(mock).await?;
    let data = radio.read_page(page).await?;
    assert_eq!(*data.first().ok_or("data[0] missing")?, 0x5A);
    assert_eq!(*radio.link_state().borrow(), LinkState::Up);
    let fw = radio.get_firmware_version().await?;
    assert!(fw.contains("1.03"), "unexpected firmware string: {fw}");
    Ok(())
}
