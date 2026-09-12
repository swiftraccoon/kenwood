//! Fail-closed MCP exchange boundaries and the exact firmware write gate.

use kenwood_thd75 as _;
use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use kenwood_tmd750::protocol::mcp::{
    ACK, BytePatch, ENTER, EXIT, PagePatch, read_request, write_request,
};
use kenwood_tmd750::radio::Radio;
use kenwood_tmd750::transport::{MockTransport, Transport, TransportError};
use kenwood_tmd750::{Address, Error, McpError, Page, ProtocolError, Region};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct BlockedWrite {
    mock: MockTransport,
    blocked: Vec<u8>,
}

impl Transport for BlockedWrite {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.mock.write(data).await?;
        if data == self.blocked {
            std::future::pending().await
        } else {
            Ok(())
        }
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }
}

fn identity(mock: &mut MockTransport, firmware: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn ready_mock(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    identity(&mut mock, firmware);
    mock.expect(ENTER, b"0M\r");
    mock
}

fn patch(page: Page) -> Result<PagePatch, Box<dyn std::error::Error>> {
    Ok(PagePatch::new(page, vec![BytePatch::new(2, 0xFF, 0x42)?])?)
}

fn data_reply(page: Page, data: &[u8]) -> Vec<u8> {
    let mut reply = write_request(page).to_vec();
    reply.extend_from_slice(data);
    reply
}

async fn cancel_pending(future: impl Future) {
    let mut future = pin!(future);
    let result = poll_fn(|context| Poll::Ready(future.as_mut().poll(context))).await;
    assert!(result.is_pending(), "scripted exchange must be in flight");
}

async fn assert_cat_blocked<T: Transport + std::fmt::Debug>(radio: &mut Radio<T>) {
    let result = radio.identify().await;
    assert!(
        matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
        "{result:?}"
    );
    let result = radio.mcp_session();
    assert!(
        matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
        "{result:?}"
    );
}

#[tokio::test]
async fn idle_session_can_be_reborrowed_but_cannot_authorize_cat_before_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.00");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    {
        let session = radio.enter_mcp().await?;
        assert_eq!(session.entry_reply(), b"0M");
        assert!(session.is_ready());
    }
    let result = radio.identify().await;
    assert!(
        matches!(result, Err(Error::Mcp(McpError::SessionActive))),
        "{result:?}"
    );
    {
        let result = radio.enter_mcp().await;
        assert!(
            matches!(result, Err(Error::Mcp(McpError::SessionActive))),
            "{result:?}"
        );
    }
    let session = radio.mcp_session()?;
    assert_eq!(session.entry_reply(), b"0M");
    session.exit().await?;
    let identity = radio.identify().await;
    assert!(
        matches!(identity, Err(Error::Mcp(McpError::ConnectionRetired))),
        "acknowledged exit must not admit CAT on the original handle: {identity:?}"
    );
    assert!(
        matches!(
            radio.mcp_session(),
            Err(Error::Mcp(McpError::ConnectionRetired))
        ),
        "an acknowledged exit cannot be reborrowed as an MCP session"
    );
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn unexpected_entry_reply_blocks_further_protocol_traffic() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.00");
    mock.expect(ENTER, b"0M PROGRAM\r");
    let mut radio = Radio::new(mock);
    {
        let result = radio.enter_mcp().await;
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::EntryReply { .. }))
            ),
            "{result:?}"
        );
    }
    assert_cat_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 4);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancelling_entry_blocks_further_protocol_traffic() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, "1.00");
    mock.expect_partial_then_hang(ENTER, b"0");
    let mut radio = Radio::new(mock);
    cancel_pending(radio.enter_mcp()).await;
    assert_cat_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 4);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancelling_wire_writes_poison_entry_read_and_exit() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let region = Region::new(8, 48)?;
    for blocked in [ENTER.to_vec(), read_request(page).to_vec(), vec![EXIT]] {
        let mut mock = ready_mock("1.02");
        if blocked != ENTER {
            mock.expect(&blocked, &[]);
        }
        let entry = blocked == ENTER;
        let exit = blocked == [EXIT];
        let mut radio = Radio::new(BlockedWrite { mock, blocked });
        if entry {
            cancel_pending(radio.enter_mcp()).await;
        } else {
            let mut session = radio.enter_mcp().await?;
            if exit {
                cancel_pending(session.exit()).await;
            } else {
                cancel_pending(session.read_regions(&[region], |_| {})).await;
                assert!(!session.is_ready());
                let result = session.exit().await;
                assert!(
                    matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
                    "{result:?}"
                );
            }
        }
        assert_cat_blocked(&mut radio).await;
        let transport = radio.into_transport();
        assert_eq!(transport.mock.writes().len(), if entry { 4 } else { 5 });
        transport.mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn mismatched_schema_refuses_all_page_traffic_and_leaves_empty_journal() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let mut mock = ready_mock("1.02");
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let mut progress = Vec::new();
    let result = session
        .write_pages_verified(&[patch(page)?], |value| progress.push(value))
        .await;
    assert!(
        matches!(result, Err(Error::UnsupportedSchemaTarget {
        expected_model: "TM-D750", expected_firmware: "1.00", ref actual_model,
        ref actual_firmware, accepted: &["1.00"],
    }) if actual_model == "TM-D750" && actual_firmware == "1.02"),
        "{result:?}"
    );
    assert!(session.journal().possibly_written.is_empty());
    assert!(session.journal().verified.is_empty());
    assert!(progress.is_empty());
    assert!(session.is_ready());
    session.exit().await?;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 5);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancelling_any_read_phase_refuses_exit_and_cat() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let region = Region::new(8, 48)?;
    for phase in 0..3 {
        let mut mock = ready_mock("1.02");
        match phase {
            0 => mock.expect_partial_then_hang(&read_request(page), b"W\0"),
            1 => mock.expect_partial_then_hang(&read_request(page), &data_reply(page, &[0x12; 7])),
            _ => {
                mock.expect(&read_request(page), &data_reply(page, &[0x12; 40]));
                mock.expect_hang(&[ACK]);
            }
        }
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        cancel_pending(session.read_regions(&[region], |_| {})).await;
        assert!(!session.is_ready(), "read phase {phase}");
        let result = session.exit().await;
        assert!(
            matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
            "{result:?}"
        );
        assert_cat_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(mock.writes().len(), if phase == 2 { 6 } else { 5 });
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn rejected_read_header_and_ack_refuse_exit_and_cat() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let other = Page::new(Address::new(56)?, 40)?;
    let region = Region::new(8, 48)?;
    for bad_ack in [false, true] {
        let mut mock = ready_mock("1.02");
        if bad_ack {
            mock.expect(&read_request(page), &data_reply(page, &[0; 40]));
            mock.expect(&[ACK], &[0x15]);
        } else {
            mock.expect(&read_request(page), &write_request(other));
        }
        let mut radio = Radio::new(mock);
        let mut session = radio.enter_mcp().await?;
        let result = session.read_regions(&[region], |_| {}).await;
        assert!(matches!(result, Err(Error::Protocol(_))), "{result:?}");
        assert!(!session.is_ready());
        let result = session.exit().await;
        assert!(
            matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
            "{result:?}"
        );
        assert_cat_blocked(&mut radio).await;
        let mock = radio.into_transport();
        assert_eq!(mock.writes().len(), if bad_ack { 6 } else { 5 });
        mock.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn cancelled_memory_write_retains_journal_and_refuses_exit() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch(page)?;
    let mut intended = vec![0; page.len()];
    patch.apply(&mut intended)?;
    let mut mock = ready_mock("1.00");
    mock.expect(&read_request(page), &data_reply(page, &[0; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect_hang(&data_reply(page, &intended));
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    cancel_pending(session.write_pages_verified(&[patch], |_| {})).await;
    assert_eq!(session.journal().possibly_written, [page]);
    assert!(session.journal().verified.is_empty());
    assert!(!session.is_ready());
    let result = session.exit().await;
    assert!(
        matches!(result, Err(Error::Mcp(McpError::RecoveryRequired))),
        "{result:?}"
    );
    assert_cat_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 7);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn complete_verify_mismatch_still_permits_safe_exit() -> TestResult {
    let page = Page::new(Address::new(8)?, 40)?;
    let patch = patch(page)?;
    let mut intended = vec![0; page.len()];
    patch.apply(&mut intended)?;
    let mut mock = ready_mock("1.00");
    mock.expect(&read_request(page), &data_reply(page, &[0; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&data_reply(page, &intended), &[ACK]);
    mock.expect(&read_request(page), &data_reply(page, &[0; 40]));
    mock.expect(&[ACK], &[ACK]);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut session = radio.enter_mcp().await?;
    let result = session.write_pages_verified(&[patch], |_| {}).await;
    assert!(
        matches!(result, Err(Error::Mcp(McpError::Interrupted { ref source, .. }))
        if matches!(**source, Error::Mcp(McpError::VerifyMismatch { offset: 2, .. }))),
        "{result:?}"
    );
    assert!(session.is_ready());
    assert_eq!(session.journal().possibly_written, [page]);
    session.exit().await?;
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_exit_blocks_cat() -> TestResult {
    let mut mock = ready_mock("1.02");
    mock.expect(&[EXIT], &[0x15]);
    let mut radio = Radio::new(mock);
    let session = radio.enter_mcp().await?;
    let result = session.exit().await;
    assert!(
        matches!(
            result,
            Err(Error::Protocol(ProtocolError::MissingAck {
                stage: "MCP exit",
                byte: 0x15,
            }))
        ),
        "{result:?}"
    );
    assert_cat_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 5);
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancelled_exit_blocks_cat() -> TestResult {
    let mut mock = ready_mock("1.02");
    mock.expect_hang(&[EXIT]);
    let mut radio = Radio::new(mock);
    let session = radio.enter_mcp().await?;
    cancel_pending(session.exit()).await;
    assert_cat_blocked(&mut radio).await;
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 5);
    mock.assert_complete();
    Ok(())
}
