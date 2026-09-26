//! Fixed-scope MCP probe behavior, including partial captures and cancellation.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use proptest as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, read_request, write_request};
use kenwood_tmd750::{
    Error, McpError, McpProbeExit, McpProbeOutcome, McpProbeStage, Page, Radio, Region,
};
use kenwood_transport::MockTransport;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity(mock: &mut MockTransport, firmware: &[u8]) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", firmware);
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn global_page() -> Result<Page, Box<dyn std::error::Error>> {
    Region::new(8, 48)?
        .pages()
        .first()
        .copied()
        .ok_or_else(|| "missing global page".into())
}

fn slot_page() -> Result<Page, Box<dyn std::error::Error>> {
    Region::new(327_681, 327_936)?
        .pages()
        .first()
        .copied()
        .ok_or_else(|| "missing slot page".into())
}

fn read(mock: &mut MockTransport, page: Page, value: u8) {
    let mut reply = write_request(page).to_vec();
    reply.extend(vec![value; page.len()]);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

#[tokio::test]
async fn probe_reads_only_two_official_fragments_and_requires_fresh_cat() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    read(&mut mock, global_page()?, 0xA5);
    read(&mut mock, slot_page()?, 0x5A);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(report.outcome, McpProbeOutcome::AwaitingCatVerification),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    let cat = radio.identify().await;
    assert!(
        matches!(cat, Err(Error::Mcp(McpError::ConnectionRetired))),
        "an acknowledged probe exit requires a new connection: {cat:?}"
    );
    assert_eq!(report.entry_reply.as_deref(), Some(b"0M".as_slice()));
    assert_eq!(report.segments.len(), 2);
    let global = report.segments.first().ok_or("missing global segment")?;
    assert_eq!(global.page, global_page()?);
    assert_eq!(global.data, vec![0xA5; 40]);
    let slot = report.segments.get(1).ok_or("missing slot segment")?;
    assert_eq!(slot.page, slot_page()?);
    assert_eq!(slot.data, vec![0x5A; 255]);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancellation_before_identity_sends_nothing() {
    let mut radio = Radio::new(MockTransport::new());
    let report = radio.probe_mcp(|| true).await;
    assert!(matches!(report.outcome, McpProbeOutcome::Cancelled));
    assert_eq!(report.exit, McpProbeExit::NotEntered);
    assert!(report.identity.is_none());
    radio.into_transport().assert_complete();
}

#[tokio::test]
async fn cancellation_after_identity_does_not_enter_programming_mode() {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    let mut radio = Radio::new(mock);
    let mut checks = 0;
    let report = radio
        .probe_mcp(|| {
            checks += 1;
            checks >= 2
        })
        .await;
    assert!(matches!(report.outcome, McpProbeOutcome::Cancelled));
    assert_eq!(report.exit, McpProbeExit::NotEntered);
    assert!(report.identity.is_some());
    assert!(report.entry_reply.is_none());
    radio.into_transport().assert_complete();
}

#[tokio::test]
async fn cancellation_at_a_page_boundary_exits_and_preserves_the_page() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    read(&mut mock, global_page()?, 0x42);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let mut checks = 0;
    let report = radio
        .probe_mcp(|| {
            checks += 1;
            checks >= 4
        })
        .await;
    assert!(
        matches!(report.outcome, McpProbeOutcome::Cancelled),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.segments.len(), 1);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn malformed_second_reply_preserves_first_segment_without_blind_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    read(&mut mock, global_page()?, 0x42);
    mock.expect(&read_request(slot_page()?), b"W\0\0\0\0");
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::SlotRead,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.segments.len(), 1);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn unexpected_entry_does_not_issue_a_read_or_exit() {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"N\r");
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::Entry,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert!(report.segments.is_empty());
    radio.into_transport().assert_complete();
}

#[tokio::test]
async fn partial_read_timeout_retains_identity_without_sending_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    mock.expect_partial_then_hang(&read_request(global_page()?), b"W\0");
    let mut radio = Radio::new(mock);
    radio.set_timeout(std::time::Duration::from_millis(5));
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::GlobalRead,
                error: Error::Timeout { .. }
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert!(report.identity.is_some());
    assert!(report.segments.is_empty());
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn exit_failure_is_not_reported_as_completed_or_followed_by_cat() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    read(&mut mock, global_page()?, 0);
    read(&mut mock, slot_page()?, 0);
    mock.expect(&[EXIT], b"N");
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::Exit,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::NotAcknowledged);
    assert_eq!(report.segments.len(), 2);
    radio.into_transport().assert_complete();
    Ok(())
}

#[tokio::test]
async fn fresh_identity_is_a_separate_observation_after_probe_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(ENTER, b"0M\r");
    read(&mut mock, global_page()?, 0);
    read(&mut mock, slot_page()?, 0);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(report.outcome, McpProbeOutcome::AwaitingCatVerification),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    radio.into_transport().assert_complete();
    let mut fresh = MockTransport::new();
    identity(&mut fresh, b"FV 1.03\r");
    let mut fresh = Radio::new(fresh);
    let observed = fresh.identify().await?;
    assert_ne!(
        report.identity.as_ref(),
        Some(&observed),
        "a probe cannot establish a future connection's identity"
    );
    fresh.into_transport().assert_complete();
    Ok(())
}
