//! Fixed-region backup coverage, partial evidence, and detached lifecycle safety.

use mcp_d75_extract as _;
use thiserror as _;
use tokio_serial as _;
use tracing as _;

use std::cell::Cell;
use std::io;
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{
    Error, McpBackupOutcome, McpBackupReport, McpBackupStage, McpError, McpProbeExit, Page,
    Progress, Radio, Region, ValidationError,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Independently pin the official no-bitmap order, including all six slots.
fn expected_pages() -> Result<Vec<Page>, ValidationError> {
    let mut pages = Vec::new();
    for (start, end) in [
        (8, 48),
        (56, 256),
        (256, 416),
        (480, 512),
        (512, 2048),
        (2048, 86_016),
        (150_784, 311_296),
        (314_624, 315_136),
        (320_512, 327_424),
    ] {
        pages.extend(Region::new(start, end)?.pages());
    }
    for slot in 0..6 {
        for (start, end) in [
            (327_681, 327_936),
            (327_936, 332_032),
            (332_800, 332_928),
            (333_824, 335_360),
        ] {
            pages.extend(Region::new(start + 8192 * slot, end + 8192 * slot)?.pages());
        }
    }
    Ok(pages)
}

fn identity(mock: &mut MockTransport) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn entry(mock: &mut MockTransport) {
    identity(mock);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
}

fn read(mock: &mut MockTransport, page: Page, fill: u8) {
    let mut reply = write_request(page).to_vec();
    reply.extend(vec![fill; page.len()]);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

fn script(pages: &[Page]) -> MockTransport {
    let mut mock = MockTransport::new();
    entry(&mut mock);
    for page in pages {
        read(&mut mock, *page, 0x42);
    }
    mock.expect(b"E", &[ACK]);
    mock
}

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
        if data == b"E" {
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
            return Err(TransportError::Disconnected(io::Error::new(
                io::ErrorKind::NotConnected,
                "USB handle departed after E",
            )));
        }
        self.baud_changes.push(baud);
        self.mock.set_baud_rate(baud)
    }
}

async fn assert_protocol_is_blocked(radio: &mut Radio<DepartingEndpoint>) {
    let result = radio.identify().await;
    assert!(
        matches!(
            result,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "CAT must not touch the uncertain or retired handle: {result:?}"
    );
    {
        let result = radio.enter_mcp().await;
        assert!(
            matches!(
                result,
                Err(Error::Mcp(
                    McpError::RecoveryRequired | McpError::ConnectionRetired
                ))
            ),
            "entry must not touch the uncertain or retired handle: {result:?}"
        );
    }
    let result = radio.mcp_session();
    assert!(
        matches!(
            result,
            Err(Error::Mcp(
                McpError::RecoveryRequired | McpError::ConnectionRetired
            ))
        ),
        "an uncertain or retired handle is not an idle MCP session: {result:?}"
    );
}

fn assert_detached(transport: &DepartingEndpoint, pages: usize) {
    assert_eq!(transport.baud_changes, [9600], "only entry configures baud");
    assert_eq!(
        transport.post_exit_baud_changes, 0,
        "exit must not touch a departed handle"
    );
    assert_eq!(
        transport.mock.writes().len(),
        5 + 2 * pages,
        "only identity, entry, page reads/ACKs, and exit are permitted"
    );
    assert_eq!(
        transport.mock.writes().last().map(Vec::as_slice),
        Some(b"E".as_slice()),
        "E must be the last protocol request"
    );
    assert!(
        transport
            .mock
            .writes()
            .iter()
            .all(|write| !matches!(write.first(), Some(b'W' | b'Z'))),
        "backup must never write or fill memory"
    );
    transport.mock.assert_complete();
}

fn assert_data(report: &McpBackupReport, pages: &[Page]) {
    assert_eq!(
        report.segments.len(),
        pages.len(),
        "retain every fully acknowledged page, and only those pages"
    );
    for (segment, page) in report.segments.iter().zip(pages) {
        assert_eq!(
            segment.page, *page,
            "page order and exact boundaries must match"
        );
        assert_eq!(
            segment.data,
            vec![0x42; page.len()],
            "retain the original page payload"
        );
    }
}

#[tokio::test]
async fn complete_backup_reads_exact_official_regions_and_retires_the_handle() -> TestResult {
    let pages = expected_pages()?;
    assert_eq!(pages.len(), 1138);
    assert_eq!(pages.iter().map(|page| page.len()).sum::<usize>(), 289_962);
    let mut radio = Radio::new(DepartingEndpoint::new(script(&pages)));
    let mut progress = Vec::new();
    let report = radio
        .backup_mcp_until_exit(|| false, |value| progress.push(value))
        .await;
    assert!(
        matches!(report.outcome, McpBackupOutcome::AwaitingCatVerification),
        "unexpected outcome: {:?}",
        report.outcome
    );
    assert!(report.has_complete_configuration());
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_eq!(report.entry_reply.as_deref(), Some(b"0M".as_slice()));
    assert_eq!(
        report
            .identity
            .as_ref()
            .map(|identity| identity.firmware.as_str()),
        Some("1.02")
    );
    assert_data(&report, &pages);
    assert_eq!(
        progress,
        (1..=pages.len())
            .map(|done| Progress {
                done,
                total: pages.len()
            })
            .collect::<Vec<_>>()
    );
    assert_protocol_is_blocked(&mut radio).await;
    assert_detached(&radio.into_transport(), pages.len());
    Ok(())
}

#[tokio::test]
async fn cancellation_at_a_page_boundary_preserves_progress_and_exits() -> TestResult {
    let pages = expected_pages()?.into_iter().take(2).collect::<Vec<_>>();
    let mut radio = Radio::new(DepartingEndpoint::new(script(&pages)));
    let completed = Cell::new(0);
    let report = radio
        .backup_mcp_until_exit(|| completed.get() >= 2, |value| completed.set(value.done))
        .await;
    assert!(matches!(report.outcome, McpBackupOutcome::Cancelled));
    assert!(!report.has_complete_configuration());
    assert_eq!(report.exit, McpProbeExit::Acknowledged);
    assert_data(&report, &pages);
    assert_protocol_is_blocked(&mut radio).await;
    assert_detached(&radio.into_transport(), 2);
    Ok(())
}

#[tokio::test]
async fn cancellation_before_identity_or_entry_never_enters_programming() -> TestResult {
    for after_identity in [false, true] {
        let mut mock = MockTransport::new();
        if after_identity {
            identity(&mut mock);
        }
        let mut radio = Radio::new(mock);
        let mut checks = 0;
        let mut progress = Vec::new();
        let report = radio
            .backup_mcp_until_exit(
                || {
                    checks += 1;
                    !after_identity || checks >= 2
                },
                |value| progress.push(value),
            )
            .await;
        assert!(matches!(report.outcome, McpBackupOutcome::Cancelled));
        assert_eq!(report.exit, McpProbeExit::NotEntered);
        assert_eq!(report.identity.is_some(), after_identity);
        assert!(report.entry_reply.is_none());
        assert!(report.segments.is_empty());
        assert!(progress.is_empty());
        let transport = radio.into_transport();
        assert_eq!(transport.writes().len(), if after_identity { 3 } else { 0 });
        transport.assert_complete();
    }
    Ok(())
}

#[tokio::test]
async fn read_failure_keeps_completed_pages_and_never_sends_exit() -> TestResult {
    let pages = expected_pages()?.into_iter().take(6).collect::<Vec<_>>();
    let failed_page = *pages.last().ok_or("failed page missing")?;
    let completed = pages.iter().copied().take(5).collect::<Vec<_>>();
    let mut mock = MockTransport::new();
    entry(&mut mock);
    for page in &completed {
        read(&mut mock, *page, 0x42);
    }
    mock.expect(&read_request(failed_page), &[b'W', 0, 0, 8, 40]);
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
    let mut progress = Vec::new();
    let report = radio
        .backup_mcp_until_exit(|| false, |value| progress.push(value))
        .await;
    assert!(
        matches!(report.outcome, McpBackupOutcome::Failed { stage: McpBackupStage::Read { page }, .. } if page == failed_page)
    );
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert!(!report.has_complete_configuration());
    assert_data(&report, &completed);
    assert_eq!(progress.len(), 5);
    assert_protocol_is_blocked(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(transport.mock.writes().len(), 15);
    assert_eq!(
        transport.mock.writes().last().map(Vec::as_slice),
        Some(read_request(failed_page).as_slice())
    );
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn rejected_entry_keeps_identity_and_sends_no_page_or_exit() -> TestResult {
    let mut mock = MockTransport::new();
    identity(&mut mock);
    mock.expect(b"0M PROGRAM\r", b"unexpected\r");
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
    let report = radio.backup_mcp_until_exit(|| false, |_| {}).await;
    assert!(matches!(
        report.outcome,
        McpBackupOutcome::Failed {
            stage: McpBackupStage::Entry,
            ..
        }
    ));
    assert!(report.identity.is_some());
    assert!(report.entry_reply.is_none());
    assert!(report.segments.is_empty());
    assert_eq!(report.exit, McpProbeExit::RecoveryRequired);
    assert_protocol_is_blocked(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(transport.mock.writes().len(), 4);
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_exit_overrides_cancellation_but_keeps_page_evidence() -> TestResult {
    let page = *expected_pages()?.first().ok_or("first page missing")?;
    let mut mock = MockTransport::new();
    entry(&mut mock);
    read(&mut mock, page, 0x42);
    mock.expect(b"E", &[0x15]);
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
    let completed = Cell::new(false);
    let report = radio
        .backup_mcp_until_exit(|| completed.get(), |_| completed.set(true))
        .await;
    assert!(matches!(
        report.outcome,
        McpBackupOutcome::Failed {
            stage: McpBackupStage::Exit,
            ..
        }
    ));
    assert_eq!(report.exit, McpProbeExit::NotAcknowledged);
    assert!(!report.has_complete_configuration());
    assert_data(&report, &[page]);
    assert_protocol_is_blocked(&mut radio).await;
    assert_detached(&radio.into_transport(), 1);
    Ok(())
}

#[tokio::test]
async fn dropping_a_mid_page_backup_blocks_all_subsequent_protocol_traffic() -> TestResult {
    let page = *expected_pages()?.first().ok_or("first page missing")?;
    let mut mock = MockTransport::new();
    entry(&mut mock);
    mock.expect_hang(&read_request(page));
    let mut radio = Radio::new(DepartingEndpoint::new(mock));
    radio.set_timeout(Duration::from_secs(1));
    let result = tokio::time::timeout(
        Duration::from_millis(5),
        radio.backup_mcp_until_exit(|| false, |_| {}),
    )
    .await;
    assert!(
        result.is_err(),
        "the caller must drop an in-flight page, not receive a normal report"
    );
    assert_protocol_is_blocked(&mut radio).await;
    let transport = radio.into_transport();
    assert_eq!(transport.mock.writes().len(), 5);
    assert_eq!(
        transport.mock.writes().last().map(Vec::as_slice),
        Some(read_request(page).as_slice())
    );
    transport.mock.assert_complete();
    Ok(())
}

#[tokio::test]
async fn completeness_checks_all_page_and_lifecycle_evidence() -> TestResult {
    let pages = expected_pages()?;
    let mut radio = Radio::new(DepartingEndpoint::new(script(&pages)));
    let mut report = radio.backup_mcp_until_exit(|| false, |_| {}).await;
    assert!(report.has_complete_configuration());
    let identity = report.identity.take();
    assert!(!report.has_complete_configuration());
    report.identity = identity;
    report.entry_reply = Some(b"unexpected".to_vec());
    assert!(!report.has_complete_configuration());
    report.entry_reply = Some(b"0M".to_vec());
    report.exit = McpProbeExit::NotAcknowledged;
    assert!(!report.has_complete_configuration());
    report.exit = McpProbeExit::Acknowledged;
    report.outcome = McpBackupOutcome::Cancelled;
    assert!(!report.has_complete_configuration());
    report.outcome = McpBackupOutcome::AwaitingCatVerification;
    let segment = report.segments.pop().ok_or("last segment missing")?;
    assert!(!report.has_complete_configuration());
    report.segments.push(segment.clone());
    report.segments.push(segment);
    assert!(!report.has_complete_configuration());
    let _extra = report.segments.pop();
    report.segments.reverse();
    assert!(!report.has_complete_configuration());
    report.segments.reverse();
    let first = report.segments.first_mut().ok_or("first segment missing")?;
    let byte = first.data.pop().ok_or("first payload empty")?;
    assert!(!report.has_complete_configuration());
    report
        .segments
        .first_mut()
        .ok_or("first segment missing")?
        .data
        .push(byte);
    assert!(report.has_complete_configuration());
    assert_detached(&radio.into_transport(), pages.len());
    Ok(())
}
