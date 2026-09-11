//! Deterministic reconnect workflow tests with observable handle ownership.

use super::*;
use std::collections::VecDeque;
use std::sync::Mutex;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{KENWOOD_VID, MockTransport, TMD750_MAIN_PID, TransportError};
use kenwood_tmd750::{Address, Page};
use reconnect::{VerificationOutcome, VerificationStage};

type TestResult = Result<(), Box<dyn StdError + Send + Sync>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Observation {
    Open(usize, String, u32),
    Write(usize, Vec<u8>),
    Close(usize),
    Dropped(usize),
    Enumerate,
    Wait(Duration),
}

type Observations = Arc<Mutex<Vec<Observation>>>;

#[derive(Debug)]
struct ObservedConnection {
    id: usize,
    mock: MockTransport,
    observations: Observations,
    close_failure: Option<io::ErrorKind>,
    cancel_on_write: Option<(Vec<u8>, Arc<AtomicBool>)>,
    cancel_on_close: Option<Arc<AtomicBool>>,
}

impl ObservedConnection {
    fn new(id: usize, mock: MockTransport, observations: Observations) -> Self {
        Self {
            id,
            mock,
            observations,
            close_failure: None,
            cancel_on_write: None,
            cancel_on_close: None,
        }
    }

    fn record(&self, observation: Observation) -> Result<(), TransportError> {
        self.observations
            .lock()
            .map_err(|_| TransportError::Read(io::Error::other("test observation log poisoned")))?
            .push(observation);
        Ok(())
    }
}

impl Transport for ObservedConnection {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.record(Observation::Write(self.id, data.to_vec()))?;
        self.mock.write(data).await?;
        if let Some((trigger, cancelled)) = &self.cancel_on_write
            && data == trigger
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.record(Observation::Close(self.id))?;
        if let Some(cancelled) = &self.cancel_on_close {
            cancelled.store(true, Ordering::Relaxed);
        }
        if let Some(kind) = self.close_failure {
            return Err(TransportError::Disconnected(io::Error::new(
                kind,
                "scripted close failure",
            )));
        }
        self.mock.close().await
    }
}

impl Drop for ObservedConnection {
    fn drop(&mut self) {
        if let Ok(mut observations) = self.observations.lock() {
            observations.push(Observation::Dropped(self.id));
        }
    }
}

#[derive(Debug)]
struct FakeBackend {
    connections: VecDeque<Result<ObservedConnection, TransportError>>,
    snapshots: VecDeque<Vec<SerialCandidate>>,
    last_snapshot: Vec<SerialCandidate>,
    observations: Observations,
    elapsed: Duration,
    opens: usize,
    cancel_on_wait: Option<Arc<AtomicBool>>,
}

impl FakeBackend {
    fn new(original: MockTransport, fresh: MockTransport, observations: Observations) -> Self {
        Self {
            connections: VecDeque::from([
                Ok(ObservedConnection::new(
                    0,
                    original,
                    Arc::clone(&observations),
                )),
                Ok(ObservedConnection::new(1, fresh, Arc::clone(&observations))),
            ]),
            snapshots: VecDeque::new(),
            last_snapshot: vec![endpoint()],
            observations,
            elapsed: Duration::ZERO,
            opens: 0,
            cancel_on_wait: None,
        }
    }

    fn record(&self, observation: Observation) {
        if let Ok(mut observations) = self.observations.lock() {
            observations.push(observation);
        }
    }
}

impl Backend for FakeBackend {
    type Connection = ObservedConnection;

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        self.record(Observation::Open(self.opens, endpoint.path.clone(), baud));
        self.opens += 1;
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: endpoint.path.clone(),
                source: io::Error::other("unexpected extra open attempt"),
            })?
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        self.record(Observation::Enumerate);
        if let Some(snapshot) = self.snapshots.pop_front() {
            self.last_snapshot = snapshot;
        }
        Ok(self.last_snapshot.clone())
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.record(Observation::Wait(duration));
        self.elapsed = self.elapsed.saturating_add(duration);
        if let Some(cancelled) = &self.cancel_on_wait {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.selected-radio".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn identity(mock: &mut MockTransport, firmware: &[u8]) {
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", firmware);
    mock.expect(b"TY\r", b"TY K,2,1\r");
}

fn read(mock: &mut MockTransport, page: Page) {
    let mut reply = write_request(page).to_vec();
    reply.extend(vec![0x42; page.len()]);
    mock.expect(&read_request(page), &reply);
    mock.expect(&[ACK], &[ACK]);
}

fn original_script() -> Result<MockTransport, Box<dyn StdError + Send + Sync>> {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?);
    read(&mut mock, Page::new(Address::new(327_681)?, 255)?);
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

fn fresh_script(firmware: &[u8]) -> MockTransport {
    let mut mock = MockTransport::new();
    identity(&mut mock, firmware);
    mock
}

fn observations(log: &Observations) -> Result<Vec<Observation>, Box<dyn StdError + Send + Sync>> {
    Ok(log
        .lock()
        .map_err(|_| "test observation log poisoned")?
        .clone())
}

fn writes(observed: &[Observation], id: usize) -> Vec<Vec<u8>> {
    observed
        .iter()
        .filter_map(|observation| match observation {
            Observation::Write(connection, bytes) if *connection == id => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

fn expected_original_writes() -> Vec<Vec<u8>> {
    [
        b"ID\r".as_slice(),
        b"FV\r",
        b"TY\r",
        b"0M PROGRAM\r",
        &[b'R', 0, 0, 8, 40],
        &[ACK],
        &[b'R', 5, 0, 1, 255],
        &[ACK],
        b"E",
    ]
    .into_iter()
    .map(<[u8]>::to_vec)
    .collect()
}

fn expected_identity_writes() -> Vec<Vec<u8>> {
    [b"ID\r", b"FV\r", b"TY\r"]
        .map(|bytes| bytes.to_vec())
        .to_vec()
}

fn position(
    observed: &[Observation],
    needle: &Observation,
) -> Result<usize, Box<dyn StdError + Send + Sync>> {
    observed
        .iter()
        .position(|observation| observation == needle)
        .ok_or_else(|| format!("required observation missing: {needle:?}").into())
}

fn assert_no_fresh_open(observed: &[Observation]) {
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, Observation::Open(..)))
            .count(),
        1,
        "ineligible verification must not open a second handle"
    );
    assert!(
        writes(observed, 1).is_empty(),
        "ineligible verification must not send fresh CAT"
    );
}

#[derive(Debug)]
struct Harness {
    _directory: tempfile::TempDir,
    backend: FakeBackend,
    captures: Option<WorkflowCaptures>,
    cancelled: Arc<AtomicBool>,
    log: Observations,
}

impl Harness {
    async fn run_backup(
        &mut self,
    ) -> Result<backup::WorkflowResult, Box<dyn StdError + Send + Sync>> {
        let captures = self
            .captures
            .take()
            .ok_or("workflow capture already consumed")?;
        Ok(backup::run_workflow(
            &mut self.backend,
            &endpoint(),
            9_600,
            captures.original,
            captures.post_exit.ok_or("backup requires fresh capture")?,
            &self.cancelled,
        )
        .await)
    }

    fn new(
        original: MockTransport,
        fresh: MockTransport,
    ) -> Result<Self, Box<dyn StdError + Send + Sync>> {
        let directory = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let artifacts = Artifacts::create(
            Some(&directory.path().join("capture")),
            Arc::clone(&cancelled),
        )?;
        let post_exit = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
        let log = Arc::new(Mutex::new(Vec::new()));
        Ok(Self {
            _directory: directory,
            backend: FakeBackend::new(original, fresh, Arc::clone(&log)),
            captures: Some(WorkflowCaptures {
                original: artifacts.transcript,
                post_exit: Some(post_exit),
            }),
            cancelled,
            log,
        })
    }

    fn connection(
        &mut self,
        id: usize,
    ) -> Result<&mut ObservedConnection, Box<dyn StdError + Send + Sync>> {
        self.backend
            .connections
            .get_mut(id)
            .and_then(|connection| connection.as_mut().ok())
            .ok_or_else(|| format!("scripted connection {id} missing").into())
    }

    async fn run(&mut self) -> Result<WorkflowResult, Box<dyn StdError + Send + Sync>> {
        let captures = self
            .captures
            .take()
            .ok_or("workflow capture already consumed")?;
        Ok(run_workflow(
            &mut self.backend,
            &endpoint(),
            9_600,
            captures,
            &self.cancelled,
        )
        .await)
    }
}

fn verification(
    result: &WorkflowResult,
) -> Result<&PostExitVerification, Box<dyn StdError + Send + Sync>> {
    result
        .post_exit
        .as_ref()
        .ok_or_else(|| "post-exit verification evidence missing".into())
}

fn backup_script() -> MockTransport {
    let mut mock = MockTransport::new();
    identity(&mut mock, b"FV 1.02\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    for region in kenwood_tmd750::protocol::mcp::regions::menu_regions() {
        for page in region.pages() {
            read(&mut mock, page);
        }
    }
    mock.expect(b"E", &[ACK]);
    mock
}

#[tokio::test]
async fn backup_reads_complete_scope_then_retires_handle_before_fresh_cat() -> TestResult {
    let mut harness = Harness::new(backup_script(), fresh_script(b"FV 1.02\r"))?;
    let result = harness.run_backup().await?;
    assert!(
        result.succeeded(),
        "full configuration workflow must succeed"
    );
    let backup = result.backup.as_ref().ok_or("backup evidence missing")?;
    assert!(
        backup.has_complete_configuration(),
        "every configured page is required"
    );
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert!(
        position(&observed, &Observation::Dropped(0))?
            < position(&observed, &Observation::Enumerate)?,
        "old handle must be retired before enumeration"
    );
    let original = writes(&observed, 0);
    assert_eq!(
        original.last(),
        Some(&b"E".to_vec()),
        "no old-handle CAT after exit"
    );
    assert!(
        original
            .iter()
            .all(|bytes| !matches!(bytes.first(), Some(b'W' | b'Z'))),
        "backup must never write or fill memory"
    );
    Ok(())
}

#[tokio::test]
async fn backup_partial_page_failure_retains_prior_page_without_exit_or_reconnect() -> TestResult {
    let mut original = MockTransport::new();
    identity(&mut original, b"FV 1.02\r");
    original.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut original, Page::new(Address::new(8)?, 40)?);
    original.expect(&[b'R', 0, 0, 56, 200], &[b'W', 0, 0, 57, 200]);
    let mut harness = Harness::new(original, fresh_script(b"FV 1.02\r"))?;
    let result = harness.run_backup().await?;
    let backup = result.backup.as_ref().ok_or("backup evidence missing")?;
    assert_eq!(backup.segments.len(), 1);
    assert_eq!(backup.exit, McpProbeExit::RecoveryRequired);
    let observed = observations(&harness.log)?;
    assert_no_fresh_open(&observed);
    assert!(
        !writes(&observed, 0).contains(&b"E".to_vec()),
        "incomplete framing prohibits exit"
    );
    assert!(!result.succeeded(), "partial backup cannot succeed");
    Ok(())
}

#[tokio::test]
async fn backup_close_failure_keeps_complete_pages_but_prevents_reconnect() -> TestResult {
    let mut harness = Harness::new(backup_script(), fresh_script(b"FV 1.02\r"))?;
    harness.connection(0)?.close_failure = Some(io::ErrorKind::NotConnected);
    let result = harness.run_backup().await?;
    assert!(
        result
            .backup
            .as_ref()
            .is_some_and(kenwood_tmd750::McpBackupReport::has_complete_configuration),
        "close failure must not discard completed pages"
    );
    assert_no_fresh_open(&observations(&harness.log)?);
    assert!(
        matches!(
            result.post_exit.outcome,
            VerificationOutcome::Skipped {
                reason: SkipReason::OriginalCloseFailed
            }
        ),
        "close failure blocks fresh traffic"
    );
    assert!(
        !result.succeeded(),
        "failed cleanup prevents overall success"
    );
    Ok(())
}

#[tokio::test]
async fn backup_cancel_during_old_close_prevents_reconnect() -> TestResult {
    let mut harness = Harness::new(backup_script(), fresh_script(b"FV 1.02\r"))?;
    harness.connection(0)?.cancel_on_close = Some(Arc::clone(&harness.cancelled));
    let result = harness.run_backup().await?;
    assert_no_fresh_open(&observations(&harness.log)?);
    assert!(
        matches!(
            result.post_exit.outcome,
            VerificationOutcome::Skipped {
                reason: SkipReason::Cancelled
            }
        ),
        "late cancellation remains observable"
    );
    assert!(!result.succeeded(), "cancellation prevents overall success");
    Ok(())
}

#[tokio::test]
async fn backup_fresh_identity_mismatch_retains_backup_and_never_retries() -> TestResult {
    let mut harness = Harness::new(backup_script(), fresh_script(b"FV 1.03\r"))?;
    let result = harness.run_backup().await?;
    assert!(
        result
            .backup
            .as_ref()
            .is_some_and(kenwood_tmd750::McpBackupReport::has_complete_configuration),
        "fresh mismatch must preserve captured bytes"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "one original and one fresh attempt only"
    );
    assert!(
        matches!(
            result.post_exit.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::IdentityMismatch,
                ..
            }
        ),
        "firmware changes must reject identity match"
    );
    assert!(!result.succeeded(), "identity mismatch prevents success");
    Ok(())
}

fn succeeded(result: WorkflowResult) -> bool {
    ArtifactReport {
        format_version: if result.post_exit.is_some() { 2 } else { 1 },
        software_version: env!("CARGO_PKG_VERSION"),
        started_at_utc: "2026-09-07T00:00:00Z".to_owned(),
        finished_at_utc: "2026-09-07T00:00:01Z".to_owned(),
        endpoint: Endpoint {
            path: endpoint().path,
            usb_vendor_id: Some(KENWOOD_VID),
            usb_product_id: Some(TMD750_MAIN_PID),
            cat_baud: 9_600,
        },
        transcript: result.transcript,
        probe: result.probe.as_ref().map(ProbeEvidence::from),
        open_error: result.open_error,
        signal_error: None,
        close_error: result.close_error,
        post_exit_verification: result.post_exit,
    }
    .succeeded()
}

#[tokio::test]
async fn success_retires_original_before_one_fresh_identity_and_close() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    let result = harness.run().await?;
    let probe = result
        .probe
        .as_ref()
        .ok_or("original probe evidence missing")?;
    assert!(matches!(
        probe.outcome,
        McpProbeOutcome::AwaitingCatVerification
    ));
    assert_eq!(probe.exit, McpProbeExit::Acknowledged);
    assert!(probe.cat_identity.is_none());
    assert!(verification(&result)?.succeeded());
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert_eq!(harness.backend.opens, 2);
    let close = position(&observed, &Observation::Close(0))?;
    let drop = position(&observed, &Observation::Dropped(0))?;
    let settle = position(&observed, &Observation::Wait(Duration::from_secs(2)))?;
    let enumerate = position(&observed, &Observation::Enumerate)?;
    let fresh = position(&observed, &Observation::Open(1, endpoint().path, 9_600))?;
    assert!(close < drop && drop < settle && settle < enumerate && enumerate < fresh);
    assert!(
        position(&observed, &Observation::Close(1))?
            < position(&observed, &Observation::Dropped(1))?
    );
    assert!(succeeded(result));
    Ok(())
}

#[tokio::test]
async fn original_close_failure_prevents_fresh_open() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.connection(0)?.close_failure = Some(io::ErrorKind::NotConnected);
    let result = harness.run().await?;
    assert!(result.close_error.is_some());
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::OriginalCloseFailed
        }
    ));
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_no_fresh_open(&observed);
    assert!(!observed.contains(&Observation::Enumerate));
    assert!(observed.contains(&Observation::Dropped(0)));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn original_open_failure_has_no_active_retry() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    *harness
        .backend
        .connections
        .get_mut(0)
        .ok_or("original connection missing")? = Err(TransportError::Open {
        path: endpoint().path,
        source: io::Error::other("original open failed"),
    });
    let result = harness.run().await?;
    assert!(result.open_error.is_some());
    assert!(result.probe.is_none());
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::OriginalOpenFailed
        }
    ));
    let observed = observations(&harness.log)?;
    assert_no_fresh_open(&observed);
    assert!(writes(&observed, 0).is_empty());
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn malformed_second_fragment_preserves_first_and_never_exits_or_reconnects() -> TestResult {
    let mut original = MockTransport::new();
    identity(&mut original, b"FV 1.02\r");
    original.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut original, Page::new(Address::new(8)?, 40)?);
    original.expect(&[b'R', 5, 0, 1, 255], &[b'W', 5, 0, 2, 255]);
    let mut harness = Harness::new(original, fresh_script(b"FV 1.02\r"))?;
    let result = harness.run().await?;
    let probe = result
        .probe
        .as_ref()
        .ok_or("original probe evidence missing")?;
    assert!(matches!(
        probe.outcome,
        McpProbeOutcome::Failed {
            stage: McpProbeStage::SlotRead,
            ..
        }
    ));
    assert_eq!(probe.exit, McpProbeExit::RecoveryRequired);
    assert_eq!(probe.segments.len(), 1);
    assert_eq!(
        probe
            .segments
            .first()
            .ok_or("global fragment missing")?
            .data,
        vec![0x42; 40]
    );
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::OriginalProbeIncomplete
        }
    ));
    let observed = observations(&harness.log)?;
    assert_eq!(
        writes(&observed, 0),
        expected_original_writes()
            .into_iter()
            .take(7)
            .collect::<Vec<_>>()
    );
    assert_no_fresh_open(&observed);
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_original_read_finishes_page_and_exit_without_reconnect() -> TestResult
{
    let mut original = MockTransport::new();
    identity(&mut original, b"FV 1.02\r");
    original.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut original, Page::new(Address::new(8)?, 40)?);
    original.expect(b"E", &[ACK]);
    let mut harness = Harness::new(original, fresh_script(b"FV 1.02\r"))?;
    harness.connection(0)?.cancel_on_write =
        Some((vec![b'R', 0, 0, 8, 40], Arc::clone(&harness.cancelled)));
    let result = harness.run().await?;
    let probe = result
        .probe
        .as_ref()
        .ok_or("original probe evidence missing")?;
    assert!(matches!(probe.outcome, McpProbeOutcome::Cancelled));
    assert_eq!(probe.exit, McpProbeExit::Acknowledged);
    assert_eq!(probe.segments.len(), 1);
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::Cancelled
        }
    ));
    let observed = observations(&harness.log)?;
    let mut expected = expected_original_writes()
        .into_iter()
        .take(6)
        .collect::<Vec<_>>();
    expected.push(b"E".to_vec());
    assert_eq!(writes(&observed, 0), expected);
    assert_no_fresh_open(&observed);
    assert!(observed.contains(&Observation::Close(0)));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_default_original_close_prevents_success() -> TestResult {
    let mut original = original_script()?;
    identity(&mut original, b"FV 1.02\r");
    let mut harness = Harness::new(original, MockTransport::new())?;
    harness
        .captures
        .as_mut()
        .ok_or("workflow captures missing")?
        .post_exit = None;
    harness.connection(0)?.cancel_on_close = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await?;
    let probe = result
        .probe
        .as_ref()
        .ok_or("original probe evidence missing")?;
    assert!(matches!(probe.outcome, McpProbeOutcome::Cancelled));
    assert_eq!(probe.exit, McpProbeExit::Acknowledged);
    assert!(probe.cat_identity.is_some());
    assert!(result.post_exit.is_none());
    let observed = observations(&harness.log)?;
    let mut expected = expected_original_writes();
    expected.extend(expected_identity_writes());
    assert_eq!(writes(&observed, 0), expected);
    assert_no_fresh_open(&observed);
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn cancellation_before_workflow_does_not_open_original() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let result = harness.run().await?;
    assert_eq!(harness.backend.opens, 0);
    assert!(observations(&harness.log)?.is_empty());
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Skipped {
            reason: SkipReason::Cancelled
        }
    ));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_settle_never_opens_fresh_connection() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.backend.cancel_on_wait = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Cancelled
    ));
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_no_fresh_open(&observed);
    assert!(!observed.contains(&Observation::Enumerate));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn fresh_open_failure_is_not_retried() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    *harness
        .backend
        .connections
        .get_mut(1)
        .ok_or("fresh connection missing")? = Err(TransportError::Open {
        path: endpoint().path,
        source: io::Error::other("fresh open failed"),
    });
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::Open,
            ..
        }
    ));
    assert_eq!(harness.backend.opens, 2);
    assert!(writes(&observations(&harness.log)?, 1).is_empty());
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn fresh_identity_failure_closes_without_retry() -> TestResult {
    let mut fresh = MockTransport::new();
    fresh.expect(b"ID\r", b"ID OTHER\r");
    let mut harness = Harness::new(original_script()?, fresh)?;
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::Identity,
            ..
        }
    ));
    assert_eq!(harness.backend.opens, 2);
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 1), vec![b"ID\r".to_vec()]);
    assert!(observed.contains(&Observation::Close(1)));
    assert!(observed.contains(&Observation::Dropped(1)));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn fresh_firmware_or_type_mismatch_closes_without_retry_or_success() -> TestResult {
    for (firmware, radio_type) in [
        (b"FV 1.03\r".as_slice(), b"TY K,2,1\r".as_slice()),
        (b"FV 1.02\r", b"TY E,2,1\r"),
    ] {
        let mut fresh = MockTransport::new();
        fresh.expect(b"ID\r", b"ID TM-D750\r");
        fresh.expect(b"FV\r", firmware);
        fresh.expect(b"TY\r", radio_type);
        let mut harness = Harness::new(original_script()?, fresh)?;
        let result = harness.run().await?;
        assert!(matches!(
            verification(&result)?.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::IdentityMismatch,
                ..
            }
        ));
        assert_eq!(harness.backend.opens, 2);
        let observed = observations(&harness.log)?;
        assert_eq!(writes(&observed, 1), expected_identity_writes());
        assert!(observed.contains(&Observation::Close(1)));
        assert!(observed.contains(&Observation::Dropped(1)));
        assert!(!succeeded(result));
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_fresh_identity_finishes_tuple_and_closes() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.connection(1)?.cancel_on_write =
        Some((b"ID\r".to_vec(), Arc::clone(&harness.cancelled)));
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Cancelled
    ));
    assert_eq!(harness.backend.opens, 2);
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert!(
        position(&observed, &Observation::Write(1, b"TY\r".to_vec()))?
            < position(&observed, &Observation::Close(1))?
    );
    assert!(observed.contains(&Observation::Dropped(1)));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn fresh_close_failure_prevents_success_even_after_identity_match() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.connection(1)?.close_failure = Some(io::ErrorKind::BrokenPipe);
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::Close,
            ..
        }
    ));
    let observed = observations(&harness.log)?;
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert_eq!(harness.backend.opens, 2);
    assert!(observed.contains(&Observation::Dropped(1)));
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn ambiguous_metadata_never_opens_a_fresh_connection() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    let mut other = endpoint();
    other.path = "/dev/cu.other-radio".to_owned();
    harness.backend.last_snapshot.push(other);
    let result = harness.run().await?;
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::EndpointSelection,
            ..
        }
    ));
    assert_no_fresh_open(&observations(&harness.log)?);
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn temporary_absence_waits_passively_before_one_fresh_open() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.backend.snapshots = VecDeque::from([vec![], vec![endpoint()]]);
    let result = harness.run().await?;
    let observed = observations(&harness.log)?;
    assert!(observed.contains(&Observation::Wait(Duration::from_millis(250))));
    assert_eq!(harness.backend.elapsed, Duration::from_millis(2_250));
    assert_eq!(harness.backend.opens, 2);
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert!(succeeded(result));
    Ok(())
}

#[tokio::test]
async fn delayed_return_beyond_old_budget_opens_once_with_only_fresh_identity() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.backend.snapshots = std::iter::repeat_with(Vec::new).take(80).collect();
    harness.backend.snapshots.push_back(vec![endpoint()]);
    let result = harness.run().await?;
    let observed = observations(&harness.log)?;
    assert_eq!(harness.backend.elapsed, Duration::from_secs(22));
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, Observation::Enumerate))
            .count(),
        81
    );
    assert_eq!(harness.backend.opens, 2);
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_eq!(writes(&observed, 1), expected_identity_writes());
    assert!(observed.contains(&Observation::Close(1)));
    assert!(observed.contains(&Observation::Dropped(1)));
    assert!(succeeded(result));
    Ok(())
}

#[tokio::test]
async fn sustained_absence_exhausts_only_passive_enumeration_budget() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.backend.last_snapshot.clear();
    let result = harness.run().await?;
    assert_eq!(harness.backend.elapsed, Duration::from_secs(62));
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::Enumeration,
            ..
        }
    ));
    assert_no_fresh_open(&observations(&harness.log)?);
    assert!(!succeeded(result));
    Ok(())
}

#[tokio::test]
async fn endpoint_at_deadline_is_not_enumerated_or_opened() -> TestResult {
    let mut harness = Harness::new(original_script()?, fresh_script(b"FV 1.02\r"))?;
    harness.backend.snapshots = std::iter::repeat_with(Vec::new).take(240).collect();
    harness.backend.snapshots.push_back(vec![endpoint()]);
    let result = harness.run().await?;
    let observed = observations(&harness.log)?;
    assert_eq!(harness.backend.elapsed, Duration::from_secs(62));
    assert_eq!(harness.backend.snapshots.len(), 1);
    assert_eq!(
        observed
            .iter()
            .filter(|event| matches!(event, Observation::Enumerate))
            .count(),
        240
    );
    assert!(matches!(
        verification(&result)?.outcome,
        VerificationOutcome::Failed {
            stage: VerificationStage::Enumeration,
            ..
        }
    ));
    assert_eq!(writes(&observed, 0), expected_original_writes());
    assert_no_fresh_open(&observed);
    assert!(!succeeded(result));
    Ok(())
}
