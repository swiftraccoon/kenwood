//! Every MCP exit retires the original handle and requires fresh CAT verification.

use kenwood_schema as _;
use mcp_d75_extract as _;
use mmdvm as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::io;
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, BytePatch, EXIT, PagePatch, read_request, write_request};
use kenwood_tmd750::{
    Address, Error, McpError, McpProbeExit, McpProbeOutcome, McpProbeStage, Page, ProtocolError,
    Radio,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct DepartingEndpoint {
    mock: MockTransport,
    exited: bool,
    baud_changes: Vec<u32>,
    post_exit_baud_changes: usize,
}

impl DepartingEndpoint {
    const fn new(mock: MockTransport) -> Self {
        Self {
            mock,
            exited: false,
            baud_changes: Vec::new(),
            post_exit_baud_changes: 0,
        }
    }
}

impl Transport for DepartingEndpoint {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.mock.write(data).await?;
        if data == [EXIT] {
            self.exited = true;
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        if self.exited {
            self.post_exit_baud_changes += 1;
            return Err(TransportError::Open {
                path: "departed-test-endpoint".to_owned(),
                source: io::Error::new(io::ErrorKind::NotConnected, "USB handle departed after E"),
            });
        }
        self.baud_changes.push(baud);
        self.mock.set_baud_rate(baud)
    }
}

async fn assert_old_radio_is_retired(radio: &mut Radio<DepartingEndpoint>) {
    let result = radio.identify().await;
    assert!(
        matches!(result, Err(Error::Mcp(McpError::ConnectionRetired))),
        "CAT must be refused without touching the retired handle: {result:?}"
    );
    {
        let result = radio.enter_mcp().await;
        assert!(
            matches!(result, Err(Error::Mcp(McpError::ConnectionRetired))),
            "MCP entry must be refused on the retired handle: {result:?}"
        );
    }
    let result = radio.mcp_session();
    assert!(
        matches!(result, Err(Error::Mcp(McpError::ConnectionRetired))),
        "the retired handle cannot be reborrowed as an idle MCP session: {result:?}"
    );
}

fn assert_no_post_exit_baud(transport: &DepartingEndpoint) {
    assert_eq!(
        transport.baud_changes,
        [9600],
        "only MCP entry may configure this handle"
    );
    assert_eq!(
        transport.post_exit_baud_changes, 0,
        "no baud ioctl is permitted after E"
    );
}

fn identity_and_entry(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
}

fn global_page() -> Result<Page, Error> {
    Ok(Page::new(Address::new(8)?, 40)?)
}

fn slot_page() -> Result<Page, Error> {
    Ok(Page::new(Address::new(327_681)?, 255)?)
}

fn read(mock: &mut MockTransport, page: Page, fill: u8) {
    let mut reply = write_request(page).to_vec();
    reply.extend(vec![fill; page.len()]);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

fn completed_reads() -> Result<MockTransport, Error> {
    let mut mock = MockTransport::new();
    identity_and_entry(&mut mock);
    read(&mut mock, global_page()?, 0x12);
    read(&mut mock, slot_page()?, 0x34);
    Ok(mock)
}

fn assert_stopped_at_exit(mock: &MockTransport, count: usize) {
    assert_eq!(mock.writes().len(), count, "no command may follow exit");
    assert_eq!(
        mock.writes().last().map(Vec::as_slice),
        Some([EXIT].as_slice()),
        "the original handle must send nothing after exit"
    );
    assert!(
        mock.writes()
            .iter()
            .all(|write| !matches!(write.first(), Some(b'W' | b'Z'))),
        "qualification must never program or fill memory"
    );
    mock.assert_complete();
}

#[tokio::test]
async fn probe_stops_after_two_fragments_and_exit_ack() -> TestResult {
    let mut mock = completed_reads()?;
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(report.outcome, McpProbeOutcome::AwaitingCatVerification),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(
        report
            .identity
            .as_ref()
            .map(|identity| identity.firmware.as_str()),
        Some("1.02")
    );
    assert_eq!(report.entry_reply.as_deref(), Some(b"0M".as_slice()));
    assert_eq!(report.segments.len(), 2);
    let global = report.segments.first().ok_or("missing global fragment")?;
    assert_eq!(global.page, global_page()?);
    assert_eq!(global.data, [0x12; 40]);
    let slot = report.segments.get(1).ok_or("missing slot fragment")?;
    assert_eq!(slot.page, slot_page()?);
    assert_eq!(slot.data, [0x34; 255]);
    assert_old_radio_is_retired(&mut radio).await;
    let transport = radio.into_transport();
    assert_no_post_exit_baud(&transport);
    assert_stopped_at_exit(&transport.mock, 9);
    Ok(())
}

#[tokio::test]
async fn cancellation_preserves_first_fragment_and_exits_without_cat() -> TestResult {
    let mut mock = MockTransport::new();
    identity_and_entry(&mut mock);
    read(&mut mock, global_page()?, 0x12);
    mock.expect(&[EXIT], &[ACK]);
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
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
    assert_eq!(
        report.segments.first().map(|segment| segment.page),
        Some(global_page()?)
    );
    assert_old_radio_is_retired(&mut radio).await;
    let transport = radio.into_transport();
    assert_no_post_exit_baud(&transport);
    assert_stopped_at_exit(&transport.mock, 7);
    Ok(())
}

#[tokio::test]
async fn rejected_exit_ack_does_not_authorize_fresh_cat_verification() -> TestResult {
    let mut mock = completed_reads()?;
    mock.expect(&[EXIT], &[0x15]);
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::Exit,
                error: Error::Protocol(ProtocolError::MissingAck {
                    stage: "MCP exit",
                    byte: 0x15
                }),
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::NotAcknowledged);
    assert_eq!(report.segments.len(), 2);
    assert_stopped_at_exit(&radio.into_transport(), 9);
    Ok(())
}

#[tokio::test]
async fn absent_exit_ack_remains_a_timeout_not_awaiting_verification() -> TestResult {
    let mut mock = completed_reads()?;
    mock.expect_hang(&[EXIT]);
    let mut radio = Radio::new(mock);
    radio.set_timeout(Duration::from_millis(5));
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::Exit,
                error: Error::Timeout {
                    operation: "MCP exit",
                    ..
                },
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::NotAcknowledged);
    assert_eq!(report.segments.len(), 2);
    assert_stopped_at_exit(&radio.into_transport(), 9);
    Ok(())
}

#[tokio::test]
async fn read_failure_preserves_prior_evidence_without_sending_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity_and_entry(&mut mock);
    read(&mut mock, global_page()?, 0x12);
    mock.expect(&read_request(slot_page()?), b"W\x05\x00\x01\x00");
    let mut radio = Radio::new(mock);
    let report = radio.probe_mcp(|| false).await;
    assert!(
        matches!(
            report.outcome,
            McpProbeOutcome::Failed {
                stage: McpProbeStage::SlotRead,
                error: Error::Protocol(ProtocolError::HeaderEcho { .. }),
            }
        ),
        "{report:?}"
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(report.segments.len(), 1);
    assert_eq!(
        report.segments.first().map(|segment| segment.page),
        Some(global_page()?)
    );
    let mock = radio.into_transport();
    assert_eq!(mock.writes().len(), 7);
    assert_eq!(
        mock.writes().last().map(Vec::as_slice),
        Some(read_request(slot_page()?).as_slice())
    );
    mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn read_success_does_not_unlock_firmware_102_schema_writes() -> TestResult {
    let mut original = completed_reads()?;
    original.expect(&[EXIT], &[ACK]);
    let mut original = Radio::new(original);
    let report = original.probe_mcp(|| false).await;
    assert!(
        matches!(report.outcome, McpProbeOutcome::AwaitingCatVerification),
        "{report:?}"
    );
    assert_stopped_at_exit(&original.into_transport(), 9);

    let mut fresh = MockTransport::new();
    identity_and_entry(&mut fresh);
    fresh.expect(&[EXIT], &[ACK]);
    let mut fresh = Radio::new(fresh);
    let mut session = fresh.enter_mcp().await?;
    let patch = PagePatch::new(global_page()?, vec![BytePatch::new(2, 0xFF, 0x42)?])?;
    let mut progress = Vec::new();
    let result = session
        .write_pages_verified(&[patch], |value| progress.push(value))
        .await;
    assert!(
        matches!(result, Err(Error::UnsupportedSchemaTarget {
        expected_model: "TM-D750", expected_firmware: "1.00",
        ref actual_firmware, ..
    }) if actual_firmware == "1.02"),
        "{result:?}"
    );
    assert!(session.journal().possibly_written.is_empty());
    assert!(session.journal().verified.is_empty());
    assert!(progress.is_empty());
    assert!(session.is_ready());
    session.exit().await?;
    assert_stopped_at_exit(&fresh.into_transport(), 5);
    Ok(())
}
