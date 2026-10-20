use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use kenwood_transport::{MockTransport, TransportError};

use super::*;
use crate::protocol::mcp::{ACK, Header, regions};
use crate::radio::backup::{McpBackupOutcome, McpBackupStage};
use crate::transport::{DEFAULT_BAUD, KENWOOD_VID, TMD750_MAIN_PID};
use crate::types::{Address, FirmwareIdentity, Page, RadioModel, RadioType, Region};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// A host whose main-unit endpoint is always enumerated and whose opens hand
/// out scripted connections in order, on a simulated clock; an open raises
/// `cancel_on_open` when it is set.
struct Host {
    endpoint: SerialCandidate,
    connections: VecDeque<MockTransport>,
    opens: usize,
    elapsed: Duration,
    stages: Vec<ControlStage>,
    cancel_on_open: Option<Arc<AtomicBool>>,
}

impl Host {
    fn new(connections: Vec<MockTransport>) -> Self {
        Self {
            endpoint: SerialCandidate {
                path: "/dev/cu.usbmodem101".to_owned(),
                vid: Some(KENWOOD_VID),
                pid: Some(TMD750_MAIN_PID),
            },
            connections: connections.into(),
            opens: 0,
            elapsed: Duration::ZERO,
            stages: Vec::new(),
            cancel_on_open: None,
        }
    }
}

impl ControlHost for Host {
    type Connection = MockTransport;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        Ok(vec![self.endpoint.clone()])
    }

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: NonZeroU32,
    ) -> Result<MockTransport, TransportError> {
        assert_eq!(selected, &self.endpoint, "only the selected endpoint opens");
        assert_eq!(baud.get(), 9600, "the selected baud is kept");
        self.opens += 1;
        if let Some(flag) = &self.cancel_on_open {
            flag.store(true, Ordering::Relaxed);
        }
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("no scripted connection"),
            })
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.elapsed += duration;
    }

    async fn close(&mut self, connection: MockTransport) -> Result<(), CloseFailure> {
        connection.assert_complete();
        Ok(())
    }

    fn stage(&mut self, stage: ControlStage) {
        self.stages.push(stage);
    }
}

fn identity() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn identify(mock: &mut MockTransport, firmware: &str) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

/// The fresh connection the readiness check opens after the exit.
fn fresh(gateway: bool) -> MockTransport {
    let mut mock = MockTransport::new();
    identify(&mut mock, "1.02");
    if gateway {
        mock.expect(b"GW\r", b"GW 0\r");
    }
    mock
}

fn read(mock: &mut MockTransport, page: Page, data: &[u8]) {
    let mut reply = Header::write_request(page).to_vec();
    reply.extend_from_slice(data);
    mock.expect(&Header::read_request(page), reply);
    mock.expect(&[ACK], &[ACK]);
}

fn replacement() -> Result<PageReplacement, Box<dyn std::error::Error>> {
    let page = Page::new(Address::new(0x1_3E00)?, 256)?;
    let mut after = [0_u8; 256];
    after
        .get_mut(..9)
        .ok_or("the name")?
        .copy_from_slice(b"Batch 995");
    Ok(PageReplacement::new(page, &[0; 256], &after)?)
}

#[tokio::test]
async fn a_plan_is_written_then_verified_on_a_fresh_identity() -> TestResult {
    let replacement = replacement()?;
    let mut session = MockTransport::new();
    identify(&mut session, "1.02");
    session.expect(b"GW\r", b"GW 0\r");
    session.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut session, replacement.page(), replacement.expected());
    let mut frame = Header::write_request(replacement.page()).to_vec();
    frame.extend_from_slice(replacement.replacement());
    session.expect(frame, &[ACK]);
    read(&mut session, replacement.page(), replacement.replacement());
    session.expect(b"E", &[ACK]);
    let mut host = Host::new(vec![session, fresh(true)]);
    let endpoint = host.endpoint.clone();
    let mut intents = Vec::new();
    let report = write_pages(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity()?,
        std::slice::from_ref(&replacement),
        &mut |page: &PageReplacement| {
            intents.push(page.page());
            Ok(())
        },
        &AtomicBool::new(false),
    )
    .await;
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(intents, [replacement.page()]);
    assert_eq!(report.journal.verified, [replacement.page()]);
    assert_eq!(report.exit, SessionExit::Acknowledged);
    assert_eq!(
        host.opens, 2,
        "the session and one fresh readiness connection"
    );
    assert_eq!(
        host.stages,
        [
            ControlStage::Programming,
            ControlStage::ReadinessAfterProgramming
        ]
    );
    Ok(())
}

#[tokio::test]
async fn another_identity_or_an_active_gateway_stops_before_entry() -> TestResult {
    for (firmware, gateway) in [("1.03", None), ("1.02", Some(&b"GW 2\r"[..]))] {
        let mut session = MockTransport::new();
        identify(&mut session, firmware);
        if let Some(reply) = gateway {
            session.expect(b"GW\r", reply);
        }
        let mut host = Host::new(vec![session]);
        let endpoint = host.endpoint.clone();
        let report = write_pages(
            &mut host,
            &endpoint,
            DEFAULT_BAUD,
            &identity()?,
            &[replacement()?],
            &mut |_page: &PageReplacement| Ok(()),
            &AtomicBool::new(false),
        )
        .await;
        let expected = if gateway.is_some() {
            matches!(report.error, Some(Error::GatewayNotOff { .. }))
        } else {
            matches!(report.error, Some(Error::IdentityMismatch { .. }))
        };
        assert!(expected, "{firmware}: {report:?}");
        assert_eq!(report.exit, SessionExit::NotEntered);
        assert!(
            matches!(report.close, Some(Ok(()))),
            "the connection is closed"
        );
        assert!(report.readiness.is_none(), "no readiness without an exit");
        assert!(!report.succeeded());
        assert_eq!(host.opens, 1);
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_before_the_open_opens_nothing() -> TestResult {
    let mut host = Host::new(Vec::new());
    let endpoint = host.endpoint.clone();
    let report = write_pages(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity()?,
        &[replacement()?],
        &mut |_page: &PageReplacement| Ok(()),
        &AtomicBool::new(true),
    )
    .await;
    assert_eq!(host.opens, 0);
    assert!(report.cancelled, "{report:?}");
    assert!(report.close.is_none() && report.error.is_none());
    assert!(!report.succeeded());
    Ok(())
}

#[tokio::test]
async fn cancellation_after_the_open_closes_without_entering() -> TestResult {
    let mut session = MockTransport::new();
    identify(&mut session, "1.02");
    session.expect(b"GW\r", b"GW 0\r");
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut host = Host::new(vec![session]);
    host.cancel_on_open = Some(Arc::clone(&cancelled));
    let endpoint = host.endpoint.clone();
    let report = write_pages(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity()?,
        &[replacement()?],
        &mut |_page: &PageReplacement| Ok(()),
        &cancelled,
    )
    .await;
    assert!(report.cancelled, "{report:?}");
    assert_eq!(report.exit, SessionExit::NotEntered);
    assert!(
        report.error.is_none() && matches!(report.close, Some(Ok(()))),
        "the connection closes without an error: {report:?}"
    );
    assert!(report.readiness.is_none() && !report.succeeded());
    assert_eq!(host.opens, 1);
    Ok(())
}

#[tokio::test]
async fn a_backup_session_reads_every_page_and_verifies_the_identity() -> TestResult {
    let mut session = MockTransport::new();
    identify(&mut session, "1.02");
    session.expect(b"GW\r", b"GW 0\r");
    session.expect(b"0M PROGRAM\r", b"0M\r");
    let pages: Vec<Page> = regions::menu_regions()
        .into_iter()
        .flat_map(Region::pages)
        .collect();
    for page in &pages {
        read(&mut session, *page, &vec![0xA5; page.len()]);
    }
    session.expect(b"E", &[ACK]);
    let mut host = Host::new(vec![session, fresh(true)]);
    let endpoint = host.endpoint.clone();
    let backup = backup(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity()?,
        &AtomicBool::new(false),
    )
    .await;
    assert!(backup.succeeded(), "{:?}", backup.session);
    assert_eq!(
        backup.backup.as_ref().map(|report| report.segments.len()),
        Some(pages.len())
    );
    assert_eq!(host.opens, 2);
    Ok(())
}

#[tokio::test]
async fn backup_rejects_an_unexpected_identity_before_entry() -> TestResult {
    let mut session = MockTransport::new();
    identify(&mut session, "1.02");
    let mut expected = identity()?;
    expected.firmware = FirmwareIdentity::new("1.03")?;
    let mut host = Host::new(vec![session]);
    let endpoint = host.endpoint.clone();
    let captured = backup(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &expected,
        &AtomicBool::new(false),
    )
    .await;
    let report = captured.backup.as_ref().ok_or("missing backup report")?;
    assert!(
        matches!(
            report.outcome,
            McpBackupOutcome::Failed {
                stage: McpBackupStage::Identity,
                error: Error::IdentityMismatch { .. },
            }
        ),
        "{captured:?}"
    );
    assert_eq!(captured.session.exit, SessionExit::NotEntered);
    assert_eq!(
        host.opens, 1,
        "refusal must not open a readiness connection"
    );
    assert!(
        matches!(captured.session.close, Some(Ok(()))),
        "{captured:?}"
    );
    assert!(report.segments.is_empty(), "no MCP page may be read");
    Ok(())
}

#[tokio::test]
async fn backup_rejects_an_active_gateway_before_entry() -> TestResult {
    let mut session = MockTransport::new();
    identify(&mut session, "1.02");
    session.expect(b"GW\r", b"GW 2\r");
    let mut host = Host::new(vec![session]);
    let endpoint = host.endpoint.clone();
    let captured = backup(
        &mut host,
        &endpoint,
        DEFAULT_BAUD,
        &identity()?,
        &AtomicBool::new(false),
    )
    .await;
    let report = captured.backup.as_ref().ok_or("missing backup report")?;
    assert!(
        matches!(
            report.outcome,
            McpBackupOutcome::Failed {
                stage: McpBackupStage::Gateway,
                error: Error::GatewayNotOff { .. },
            }
        ),
        "{captured:?}"
    );
    assert_eq!(captured.session.exit, SessionExit::NotEntered);
    assert_eq!(
        host.opens, 1,
        "refusal must not open a readiness connection"
    );
    assert!(
        matches!(captured.session.close, Some(Ok(()))),
        "{captured:?}"
    );
    assert!(report.segments.is_empty(), "no MCP page may be read");
    Ok(())
}
