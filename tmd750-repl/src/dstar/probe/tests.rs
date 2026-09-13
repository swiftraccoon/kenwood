//! Wire-boundary, ownership, and evidence checks without a live radio.

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_PANEL_PID};
use kenwood_transport::MockTransport;
use serde_json::Value;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const FAST: Limits = Limits {
    cat_io_step: Duration::from_millis(30),
    // Required capture synchronizes real files. Leave bounded headroom for
    // concurrently running backup fixtures without changing device policy.
    binary_total: Duration::from_secs(1),
    close: Duration::from_millis(20),
};
const VERSION_REQUEST: &[u8] = &[0xe0, 3, 0];
const STATUS_REQUEST: &[u8] = &[0xe0, 3, 1];
const VERSION_REPLY: &[u8] = &[0xe0, 8, 0, 1, b'T', b'E', b'S', b'T'];
const STATUS_REPLY: &[u8] = &[0xe0, 10, 1, 1, 1, 0, 7, 0, 0, 0];

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.TestPanel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

fn arguments(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

#[test]
fn cli_preserves_output_path_and_requires_scope() -> TestResult {
    let request = parse(&arguments(&[
        "dstar",
        "probe",
        "--output",
        "/tmp/Probe Case",
        "--approve-live-test",
    ]))?;
    assert_eq!(request.output, Some(PathBuf::from("/tmp/Probe Case")));
    assert_eq!(
        request.validate(Some("/dev/cu.TestPanel"), DEFAULT_BAUD)?,
        "/dev/cu.TestPanel"
    );
    assert!(request.validate(None, DEFAULT_BAUD).is_err());
    assert!(request.validate(Some(""), DEFAULT_BAUD).is_err());
    assert!(
        request
            .validate(Some("/dev/cu.TestPanel"), 115_200)
            .is_err()
    );
    assert!(
        parse(&arguments(&["dstar", "probe"]))?
            .validate(Some("selected"), DEFAULT_BAUD)
            .is_err()
    );
    assert!(parse(&arguments(&["dstar", "probe", "--arbitrary-command", "TX"])).is_err());
    assert!(parse(&arguments(&["dstar", "probe", "KQ4NIT", "REF030C"])).is_err());
    let help = parse(&arguments(&["dstar", "probe", "--help"]));
    assert!(matches!(help, Err(error) if error.exit_code() == 0));
    assert!(matches_arguments(&arguments(&["D-STAR", "PROBE"])));
    assert!(!matches_arguments(&arguments(&["dstar", "start"])));
    Ok(())
}

#[test]
fn managed_cli_preserves_control_and_backup_paths() -> TestResult {
    let request = parse(&arguments(&[
        "dstar",
        "probe",
        "--manage-terminal",
        "--control-port",
        "/dev/cu.Control Case",
        "--backup",
        "/tmp/Backup Case/report.json",
        "--approve-live-test",
    ]))?;
    assert!(request.manage_terminal);
    assert_eq!(
        request.control_port.as_deref(),
        Some("/dev/cu.Control Case")
    );
    assert_eq!(
        request.backup,
        Some(PathBuf::from("/tmp/Backup Case/report.json"))
    );
    assert_eq!(
        request.validate(Some("/dev/cu.TestPanel"), DEFAULT_BAUD)?,
        "/dev/cu.TestPanel"
    );
    assert!(
        request
            .validate(Some("/dev/cu.Control Case"), DEFAULT_BAUD)
            .is_err()
    );
    Ok(())
}

#[test]
fn managed_cli_requires_a_complete_explicit_scope() {
    for options in [
        vec!["--manage-terminal"],
        vec!["--manage-terminal", "--control-port", "/dev/cu.Control"],
        vec!["--manage-terminal", "--backup", "/tmp/report.json"],
        vec!["--control-port", "/dev/cu.Control"],
        vec!["--backup", "/tmp/report.json"],
    ] {
        let mut input = arguments(&["dstar", "probe", "--approve-live-test"]);
        input.extend(arguments(&options));
        assert!(
            parse(&input).is_err(),
            "accepted incomplete scope: {input:?}"
        );
    }
}

#[test]
fn endpoint_selection_rejects_aliases_unknown_devices_and_conflicts() -> TestResult {
    let expected = endpoint();
    assert_eq!(
        select_endpoint(&expected.path, vec![expected.clone()])?,
        expected
    );
    assert!(select_endpoint("/dev/tty.TestPanel", vec![expected.clone()]).is_err());
    assert!(select_endpoint(&expected.path, Vec::new()).is_err());
    let mut unknown = expected.clone();
    unknown.pid = Some(0x9023);
    assert!(select_endpoint(&expected.path, vec![unknown.clone()]).is_err());
    assert!(select_endpoint(&expected.path, vec![expected.clone(), unknown]).is_err());
    assert!(select_endpoint(&expected.path, vec![expected.clone(), expected.clone()]).is_err());
    Ok(())
}

#[derive(Clone, Copy, Default)]
enum CloseBehavior {
    #[default]
    Success,
    Failure,
    Hang,
}

#[derive(Clone, Copy, Default)]
enum WriteBehavior {
    #[default]
    Success,
    Failure,
    Hang,
}

#[derive(Default)]
struct Observations {
    writes: Mutex<Vec<Vec<u8>>>,
    closes: AtomicUsize,
    drops: AtomicUsize,
}

struct Connection {
    mock: MockTransport,
    observations: Arc<Observations>,
    cancelled: Arc<AtomicBool>,
    cancel_on_write: Option<&'static [u8]>,
    close: CloseBehavior,
    write: WriteBehavior,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _previous = self.observations.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.observations
            .writes
            .lock()
            .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?
            .push(bytes.to_vec());
        if self.cancel_on_write == Some(bytes) {
            self.cancelled.store(true, Ordering::Relaxed);
        }
        match self.write {
            WriteBehavior::Success => {}
            WriteBehavior::Failure => {
                return Err(TransportError::Write(io::Error::other("write failed")));
            }
            WriteBehavior::Hang => return std::future::pending().await,
        }
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        let _previous = self.observations.closes.fetch_add(1, Ordering::Relaxed);
        match self.close {
            CloseBehavior::Success => self.mock.close().await,
            CloseBehavior::Failure => Err(TransportError::Disconnected(io::Error::other(
                "close failed",
            ))),
            CloseBehavior::Hang => std::future::pending().await,
        }
    }
}

struct TestBackend {
    connection: Option<Connection>,
    opens: usize,
}

impl Backend for TestBackend {
    type Connection = Connection;
    fn open(&mut self, selected: &SerialCandidate) -> Result<Connection, TransportError> {
        assert_eq!(*selected, endpoint());
        self.opens += 1;
        self.connection.take().ok_or_else(|| TransportError::Open {
            path: selected.path.clone(),
            source: io::Error::other("open failed"),
        })
    }
}

struct Harness {
    root: tempfile::TempDir,
    artifacts: Artifacts,
    backend: TestBackend,
    cancelled: Arc<AtomicBool>,
    observations: Arc<Observations>,
}

impl Harness {
    fn new(mock: MockTransport) -> io::Result<Self> {
        let root = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let observations = Arc::new(Observations::default());
        let artifacts = Artifacts::create(
            CaptureKind::DstarProbe,
            Some(&root.path().join("Probe Case")),
            Arc::clone(&cancelled),
        )?;
        let connection = Connection {
            mock,
            observations: Arc::clone(&observations),
            cancelled: Arc::clone(&cancelled),
            cancel_on_write: None,
            close: CloseBehavior::Success,
            write: WriteBehavior::Success,
        };
        Ok(Self {
            root,
            artifacts,
            backend: TestBackend {
                connection: Some(connection),
                opens: 0,
            },
            cancelled,
            observations,
        })
    }

    fn connection(&mut self) -> io::Result<&mut Connection> {
        self.backend
            .connection
            .as_mut()
            .ok_or_else(|| io::Error::other("fixture connection missing"))
    }

    async fn run(mut self) -> TestResultWithEvidence {
        let result = run_workflow(
            &mut self.backend,
            &endpoint(),
            self.artifacts.transcript,
            &self.cancelled,
            FAST,
        )
        .await;
        let writes = self
            .observations
            .writes
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .clone();
        let text = std::fs::read_to_string(self.root.path().join("Probe Case/transcript.jsonl"))?;
        let transcript = text
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()?;
        Ok(Evidence {
            result,
            writes,
            transcript,
            closes: self.observations.closes.load(Ordering::Relaxed),
            drops: self.observations.drops.load(Ordering::Relaxed),
            opens: self.backend.opens,
        })
    }
}

type TestResultWithEvidence = Result<Evidence, Box<dyn std::error::Error + Send + Sync>>;
struct Evidence {
    result: WorkflowResult,
    writes: Vec<Vec<u8>>,
    transcript: Vec<Value>,
    opens: usize,
    closes: usize,
    drops: usize,
}

fn cat() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

fn binary() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect_hang(b"ID\r");
    mock.expect(VERSION_REQUEST, VERSION_REPLY);
    mock
}

fn assert_retired(evidence: &Evidence) {
    assert_eq!((evidence.opens, evidence.closes, evidence.drops), (1, 1, 1));
    assert!(evidence.result.transcript.complete);
    assert!(evidence.result.synchronization_error.is_none());
}

#[tokio::test]
async fn normal_cat_is_reported_without_binary_or_configuration_traffic() -> TestResult {
    let mut mock = cat();
    mock.expect(b"GW\r", b"GW 2\r");
    let evidence = Harness::new(mock)?.run().await?;
    assert_retired(&evidence);
    assert!(evidence.result.succeeded());
    assert!(matches!(
        evidence.result.outcome,
        Outcome::CatObserved { gateway: 2, .. }
    ));
    assert_eq!(
        evidence.writes,
        [
            b"ID\r".to_vec(),
            b"FV\r".to_vec(),
            b"TY\r".to_vec(),
            b"GW\r".to_vec()
        ]
    );
    assert_eq!(
        evidence
            .transcript
            .first()
            .and_then(|value| value.pointer("/event/kind"))
            .and_then(Value::as_str),
        Some("open_requested")
    );
    assert_eq!(
        evidence
            .transcript
            .last()
            .and_then(|value| value.pointer("/event/kind"))
            .and_then(Value::as_str),
        Some("close_completed")
    );
    Ok(())
}

#[tokio::test]
async fn binary_success_sends_only_version_then_status_on_one_connection() -> TestResult {
    let mut mock = binary();
    mock.expect(STATUS_REQUEST, STATUS_REPLY);
    let evidence = Harness::new(mock)?.run().await?;
    assert_retired(&evidence);
    assert!(evidence.result.succeeded(), "{:?}", evidence.result);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::MmdvmObserved { .. }
    ));
    assert_eq!(
        evidence.writes,
        [
            b"ID\r".to_vec(),
            VERSION_REQUEST.to_vec(),
            STATUS_REQUEST.to_vec()
        ]
    );
    let json = serde_json::to_value(&evidence.result)?;
    assert_eq!(
        json.pointer("/outcome/version/protocol"),
        Some(&Value::from(1))
    );
    assert_eq!(
        json.pointer("/outcome/status/dstar_space"),
        Some(&Value::from(7))
    );
    assert!(json.pointer("/outcome/cat_silence/message").is_some());
    Ok(())
}

#[tokio::test]
async fn every_non_silent_or_later_cat_failure_stops_protocol_fallback() -> TestResult {
    let mut partial = MockTransport::new();
    partial.expect_partial_then_hang(b"ID\r", b"I");
    let mut wrong = MockTransport::new();
    wrong.expect(b"ID\r", b"ID NOT-A-D750\r");
    let mut late = MockTransport::new();
    late.expect(b"ID\r", b"ID TM-D750\r");
    late.expect_hang(b"FV\r");
    let mut eof = MockTransport::new();
    eof.expect_eof(b"ID\r");
    for mock in [partial, wrong, late, eof] {
        let evidence = Harness::new(mock)?.run().await?;
        assert_retired(&evidence);
        assert!(!evidence.result.succeeded());
        assert!(matches!(
            evidence.result.outcome,
            Outcome::Failed {
                stage: Stage::CatIdentity,
                ..
            }
        ));
        assert!(
            evidence
                .writes
                .iter()
                .all(|bytes| bytes == b"ID\r" || bytes == b"FV\r")
        );
    }
    for write in [WriteBehavior::Failure, WriteBehavior::Hang] {
        let mut harness = Harness::new(MockTransport::new())?;
        harness.connection()?.write = write;
        let evidence = harness.run().await?;
        assert_retired(&evidence);
        assert_eq!(evidence.writes, [b"ID\r".to_vec()]);
        assert!(!evidence.result.succeeded());
    }
    Ok(())
}

#[tokio::test]
async fn version_failure_suppresses_status_and_status_failure_preserves_version() -> TestResult {
    let mut no_version = MockTransport::new();
    no_version.expect_hang(b"ID\r");
    no_version.expect_hang(VERSION_REQUEST);
    let evidence = Harness::new(no_version)?.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::VersionFailed { .. }
    ));
    assert_eq!(
        evidence.writes,
        [b"ID\r".to_vec(), VERSION_REQUEST.to_vec()]
    );
    let mut no_status = binary();
    no_status.expect_hang(STATUS_REQUEST);
    let evidence = Harness::new(no_status)?.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::StatusFailed { .. }
    ));
    let value = serde_json::to_value(&evidence.result)?;
    assert_eq!(
        value.pointer("/outcome/version/description"),
        Some(&Value::from("TEST"))
    );
    assert!(!evidence.result.succeeded());
    Ok(())
}

#[tokio::test]
async fn cancellation_before_open_or_after_identity_prevents_further_queries() -> TestResult {
    let harness = Harness::new(MockTransport::new())?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let evidence = harness.run().await?;
    assert_eq!((evidence.opens, evidence.closes), (0, 0));
    assert!(evidence.writes.is_empty());
    assert!(matches!(evidence.result.outcome, Outcome::Cancelled { .. }));
    let mut harness = Harness::new(cat())?;
    harness.connection()?.cancel_on_write = Some(b"ID\r");
    let evidence = harness.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::Cancelled {
            identity: Some(_),
            ..
        }
    ));
    assert_eq!(
        evidence.writes,
        [b"ID\r".to_vec(), b"FV\r".to_vec(), b"TY\r".to_vec()]
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_during_version_preserves_it_and_suppresses_status() -> TestResult {
    let mut harness = Harness::new(binary())?;
    harness.connection()?.cancel_on_write = Some(VERSION_REQUEST);
    let evidence = harness.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::Cancelled {
            version: Some(_),
            ..
        }
    ));
    assert_eq!(
        evidence.writes,
        [b"ID\r".to_vec(), VERSION_REQUEST.to_vec()]
    );
    Ok(())
}

#[tokio::test]
async fn close_errors_do_not_erase_successful_protocol_evidence_or_repeat_close() -> TestResult {
    for close in [CloseBehavior::Failure, CloseBehavior::Hang] {
        let mut mock = binary();
        mock.expect(STATUS_REQUEST, STATUS_REPLY);
        let mut harness = Harness::new(mock)?;
        harness.connection()?.close = close;
        let evidence = harness.run().await?;
        assert_retired(&evidence);
        assert!(matches!(
            evidence.result.outcome,
            Outcome::MmdvmObserved { .. }
        ));
        assert!(evidence.result.close_error.is_some());
        assert!(!evidence.result.succeeded());
    }
    Ok(())
}

#[tokio::test]
async fn open_failure_is_recorded_without_any_protocol_or_close_attempt() -> TestResult {
    let mut harness = Harness::new(MockTransport::new())?;
    let _unopened = harness.backend.connection.take();
    let evidence = harness.run().await?;
    assert!(matches!(
        evidence.result.outcome,
        Outcome::Failed {
            stage: Stage::Open,
            ..
        }
    ));
    assert_eq!((evidence.opens, evidence.closes), (1, 0));
    assert!(evidence.writes.is_empty());
    assert!(evidence.result.transcript.complete);
    Ok(())
}

#[tokio::test]
async fn signal_cancellation_awaits_the_owned_workflow_and_retains_listener_failure() -> TestResult
{
    let cancelled = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let workflow = async {
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(cancelled.load(Ordering::Relaxed));
        finished.store(true, Ordering::Relaxed);
        42
    };
    let (value, signal_error) = finish_on_interrupt(
        workflow,
        async { Err(io::Error::other("signal failed")) },
        &cancelled,
    )
    .await;
    assert_eq!(value, 42);
    assert!(finished.load(Ordering::Relaxed));
    assert!(signal_error.is_some());
    Ok(())
}

#[tokio::test]
async fn status_report_preserves_nonordinal_wire_mode_values() -> TestResult {
    let mut mock = binary();
    mock.expect(STATUS_REQUEST, &[0xe0, 10, 1, 1, 10, 0, 7, 0, 0, 0]);
    let evidence = Harness::new(mock)?.run().await?;
    assert!(
        evidence.result.succeeded(),
        "diagnostic outcome: {:#?}",
        evidence.result
    );
    let report = serde_json::to_value(evidence.result)?;
    assert_eq!(
        report.pointer("/outcome/status/mode"),
        Some(&Value::from(10))
    );
    Ok(())
}

fn report_for(result: WorkflowResult, cancelled: bool) -> Report {
    Report {
        format_version: 1,
        operation: "dstar_probe",
        software_version: "test",
        started_at_utc: "2026-09-12T00:00:00Z".to_owned(),
        finished_at_utc: "2026-09-12T00:00:01Z".to_owned(),
        endpoint: Endpoint::from(&endpoint()),
        limits: FAST,
        result,
        signal_error: None,
        cancelled,
    }
}

#[tokio::test]
async fn cancellation_during_status_preserves_observation_but_cannot_succeed() -> TestResult {
    let mut mock = binary();
    mock.expect(STATUS_REQUEST, STATUS_REPLY);
    let mut harness = Harness::new(mock)?;
    harness.connection()?.cancel_on_write = Some(STATUS_REQUEST);
    let cancelled = Arc::clone(&harness.cancelled);
    let evidence = harness.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::MmdvmObserved { .. }
    ));
    let report = report_for(evidence.result, cancelled.load(Ordering::Relaxed));
    assert!(report.cancelled);
    assert!(!report.succeeded());
    Ok(())
}

#[tokio::test]
async fn gateway_query_failure_retains_identity_and_never_admits_binary() -> TestResult {
    let mut mock = cat();
    mock.expect_hang(b"GW\r");
    let evidence = Harness::new(mock)?.run().await?;
    assert_retired(&evidence);
    assert!(matches!(
        evidence.result.outcome,
        Outcome::CatGatewayFailed { .. }
    ));
    assert!(!evidence.result.succeeded());
    assert_eq!(
        evidence.writes,
        [
            b"ID\r".to_vec(),
            b"FV\r".to_vec(),
            b"TY\r".to_vec(),
            b"GW\r".to_vec()
        ]
    );
    let value = serde_json::to_value(&evidence.result)?;
    assert_eq!(
        value.pointer("/outcome/identity/model"),
        Some(&Value::from("TM-D750"))
    );
    Ok(())
}

#[tokio::test]
async fn capture_failure_before_open_suppresses_all_radio_operations() -> TestResult {
    let mut harness = Harness::new(MockTransport::new())?;
    let path = harness.artifacts.directory.join("transcript.jsonl");
    harness.artifacts.transcript = Recorder::named(
        File::open(path)?,
        Arc::clone(&harness.cancelled),
        "transcript.jsonl",
    );
    let evidence = harness.run().await?;
    assert_eq!((evidence.opens, evidence.closes), (0, 0));
    assert!(evidence.writes.is_empty());
    assert!(matches!(
        evidence.result.outcome,
        Outcome::Failed {
            stage: Stage::Capture,
            ..
        }
    ));
    assert!(!evidence.result.transcript.complete);
    assert!(evidence.result.synchronization_error.is_some());
    assert!(!evidence.result.succeeded());
    Ok(())
}

#[tokio::test]
async fn report_is_serializable_and_publication_failure_is_not_success() -> TestResult {
    let mut mock = cat();
    mock.expect(b"GW\r", b"GW 0\r");
    let evidence = Harness::new(mock)?.run().await?;
    let mut report = report_for(evidence.result, false);
    assert!(report.succeeded());
    let root = tempfile::tempdir()?;
    let path = root.path().join("report.json");
    let mut writable = crate::capture::create_private_file(&path)?;
    publish_report(&mut writable, &report)?;
    let value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    assert_eq!(value.get("operation"), Some(&Value::from("dstar_probe")));
    assert_eq!(
        value.pointer("/limits/cat_io_step/nanos"),
        Some(&Value::from(30_000_000))
    );
    let original = std::fs::read(&path)?;
    assert!(publish_report(&mut File::open(&path)?, &report).is_err());
    assert_eq!(std::fs::read(&path)?, original);
    report.signal_error = Some(Failure::from_error(&io::Error::other("signal failure")));
    assert!(!report.succeeded());
    report.signal_error = None;
    report.result.transcript.complete = false;
    assert!(!report.succeeded());
    Ok(())
}
