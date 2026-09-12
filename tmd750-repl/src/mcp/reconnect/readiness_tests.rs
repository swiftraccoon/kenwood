//! Synthetic readiness retries with explicit wire and handle-ownership evidence.

use super::*;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};
use kenwood_tmd750::{FirmwareIdentity, RadioModel, RadioType};
use kenwood_transport::MockTransport;
use serde_json::Value;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, PartialEq, Eq)]
enum Observation {
    Enumerate,
    Open(usize),
    Write(usize, Vec<u8>),
    Close(usize),
    Drop(usize),
    Wait(Duration),
}

#[derive(Debug, Default)]
struct Shared {
    observations: Mutex<Vec<Observation>>,
    elapsed: Mutex<Duration>,
}

impl Shared {
    fn record(&self, observation: Observation) -> Result<(), io::Error> {
        self.observations
            .lock()
            .map_err(|_| io::Error::other("observation log poisoned"))?
            .push(observation);
        Ok(())
    }

    fn advance(&self, duration: Duration) -> Result<(), io::Error> {
        *self
            .elapsed
            .lock()
            .map_err(|_| io::Error::other("clock poisoned"))? += duration;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum Fault {
    #[default]
    None,
    Read,
    Write,
    WriteTimeout,
    InvalidReadCount,
    Close,
    CloseTimeout,
}

#[derive(Debug)]
struct Connection {
    ordinal: usize,
    script: MockTransport,
    shared: Arc<Shared>,
    fault: Fault,
    cancel_on_write: Option<Arc<AtomicBool>>,
    cancel_on_close: Option<Arc<AtomicBool>>,
    close_cost: Duration,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.shared
            .record(Observation::Write(self.ordinal, bytes.to_vec()))
            .map_err(TransportError::Write)?;
        match self.fault {
            Fault::Write => return Err(TransportError::Write(io::Error::other("write failed"))),
            Fault::WriteTimeout => return std::future::pending().await,
            _ => {}
        }
        self.script.write(bytes).await?;
        if let Some(cancelled) = &self.cancel_on_write {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        if matches!(self.fault, Fault::Read) {
            return Err(TransportError::Read(io::Error::other("read failed")));
        }
        if matches!(self.fault, Fault::InvalidReadCount) {
            return Ok(bytes.len().saturating_add(1));
        }
        self.script.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.shared
            .record(Observation::Close(self.ordinal))
            .and_then(|()| self.shared.advance(self.close_cost))
            .map_err(TransportError::Disconnected)?;
        if let Some(cancelled) = &self.cancel_on_close {
            cancelled.store(true, Ordering::Relaxed);
        }
        match self.fault {
            Fault::Close => Err(TransportError::Disconnected(io::Error::other(
                "close failed",
            ))),
            Fault::CloseTimeout => std::future::pending().await,
            _ => self.script.close().await,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _result = self.shared.record(Observation::Drop(self.ordinal));
    }
}

struct ReadinessBackend {
    connections: VecDeque<Connection>,
    shared: Arc<Shared>,
    snapshots: VecDeque<Vec<SerialCandidate>>,
    snapshot: Vec<SerialCandidate>,
    opens: usize,
    started: Instant,
    enumeration_cost: Duration,
    open_cost: Duration,
    open_error: Option<io::ErrorKind>,
    waits: usize,
    cancel_on_wait: Option<(usize, Arc<AtomicBool>)>,
}

impl ReadinessBackend {
    fn new(scripts: impl IntoIterator<Item = MockTransport>) -> Self {
        let shared = Arc::new(Shared::default());
        let connections = scripts
            .into_iter()
            .enumerate()
            .map(|(ordinal, script)| Connection {
                ordinal,
                script,
                shared: Arc::clone(&shared),
                fault: Fault::None,
                cancel_on_write: None,
                cancel_on_close: None,
                close_cost: Duration::ZERO,
            })
            .collect();
        Self {
            connections,
            shared,
            snapshots: VecDeque::new(),
            snapshot: vec![endpoint()],
            opens: 0,
            started: Instant::now(),
            enumeration_cost: Duration::ZERO,
            open_cost: Duration::ZERO,
            open_error: None,
            waits: 0,
            cancel_on_wait: None,
        }
    }

    fn first(&mut self) -> Result<&mut Connection, io::Error> {
        self.connections
            .front_mut()
            .ok_or_else(|| io::Error::other("no pending connection"))
    }

    fn assert_wire(&self, expected: &[&[&[u8]]]) -> TestResult {
        let observed = self
            .shared
            .observations
            .lock()
            .map_err(|_| "observation log poisoned")?;
        assert_eq!(self.opens, expected.len(), "exact fresh-open count");
        for (ordinal, writes) in expected.iter().enumerate() {
            let actual: Vec<&[u8]> = observed
                .iter()
                .filter_map(|event| match event {
                    Observation::Write(connection, bytes) if *connection == ordinal => {
                        Some(bytes.as_slice())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(actual, *writes, "exact command scope for handle {ordinal}");
            let close = observed
                .iter()
                .position(|event| *event == Observation::Close(ordinal))
                .ok_or("missing close")?;
            let dropped = observed
                .iter()
                .position(|event| *event == Observation::Drop(ordinal))
                .ok_or("missing drop")?;
            assert!(close < dropped, "close precedes handle drop");
            if let Some(next) = observed
                .iter()
                .position(|event| *event == Observation::Open(ordinal + 1))
            {
                assert!(dropped < next, "old handle drops before subsequent open");
                let between = observed.get(dropped + 1..next).ok_or("event interval")?;
                assert!(
                    between.contains(&Observation::Enumerate),
                    "each later open requires a new endpoint observation"
                );
                assert!(
                    between.contains(&Observation::Wait(Duration::from_secs(2))),
                    "each retry is paced after release"
                );
            }
        }
        drop(observed);
        Ok(())
    }
}

impl Backend for ReadinessBackend {
    type Connection = Connection;

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        assert_eq!(selected, &endpoint(), "only the selected endpoint may open");
        assert_eq!(baud, 9600, "preserve the selected CAT baud");
        self.shared
            .record(Observation::Open(self.opens))
            .and_then(|()| self.shared.advance(self.open_cost))
            .map_err(TransportError::Disconnected)?;
        self.opens += 1;
        if let Some(kind) = self.open_error {
            return Err(TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::new(kind, "open failed"),
            });
        }
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected additional open"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        self.shared
            .record(Observation::Enumerate)
            .and_then(|()| self.shared.advance(self.enumeration_cost))
            .map_err(TransportError::Read)?;
        if let Some(snapshot) = self.snapshots.pop_front() {
            self.snapshot = snapshot;
        }
        Ok(self.snapshot.clone())
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
            + self
                .shared
                .elapsed
                .lock()
                .map_or(Duration::ZERO, |elapsed| *elapsed)
    }

    async fn wait(&mut self, duration: Duration) {
        let result = self
            .shared
            .record(Observation::Wait(duration))
            .and_then(|()| self.shared.advance(duration));
        assert!(result.is_ok(), "record simulated wait: {result:?}");
        self.waits += 1;
        if let Some((ordinal, cancelled)) = &self.cancel_on_wait
            && *ordinal == self.waits
        {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.readiness-panel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

fn identity() -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn ready_script() -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", b"FV 1.02\r");
    script.expect(b"TY\r", b"TY K,2,1\r");
    script
}

fn silent_script() -> MockTransport {
    let mut script = MockTransport::new();
    script.expect_hang(b"ID\r");
    script
}

async fn run(
    backend: &mut ReadinessBackend,
    cancelled: &AtomicBool,
) -> Result<ReadinessVerification, Box<dyn std::error::Error + Send + Sync>> {
    run_with_file(backend, tempfile::tempfile()?, cancelled).await
}

async fn run_with_file(
    backend: &mut ReadinessBackend,
    file: File,
    cancelled: &AtomicBool,
) -> Result<ReadinessVerification, Box<dyn std::error::Error + Send + Sync>> {
    let recorder = Recorder::named(
        file,
        Arc::new(AtomicBool::new(false)),
        "post-exit-transcript.jsonl",
    );
    Ok(verify_readiness(
        backend,
        &endpoint(),
        9600,
        &identity()?,
        recorder,
        cancelled,
    )
    .await)
}

fn attempts(report: &ReadinessVerification) -> Result<Vec<Value>, serde_json::Error> {
    let value = serde_json::to_value(report)?;
    Ok(value
        .get("attempts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

#[tokio::test]
async fn silent_id_then_matching_identity_retries_only_after_release_and_reselection() -> TestResult
{
    let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(
        report.succeeded(),
        "later complete identity qualifies: {report:?}"
    );
    backend.assert_wire(&[&[b"ID\r"], &[b"ID\r", b"FV\r", b"TY\r"]])?;
    let attempts = attempts(&report)?;
    assert_eq!(attempts.len(), 2, "retain both attempts in the report");
    for attempt in &attempts {
        assert!(
            attempt.get("outcome").is_some(),
            "each attempt has its own outcome"
        );
        assert!(
            attempt
                .get("enumerations")
                .and_then(Value::as_array)
                .is_some_and(|rows| !rows.is_empty()),
            "each attempt retains its endpoint observations"
        );
    }
    assert!(
        attempts
            .first()
            .and_then(|attempt| attempt.pointer("/connection/identity"))
            .is_some_and(Value::is_null),
        "a later success does not fabricate identity for the silent attempt"
    );
    assert_eq!(
        attempts
            .first()
            .and_then(|attempt| attempt.pointer("/outcome/status"))
            .and_then(Value::as_str),
        Some("failed"),
        "retain the failed readiness observation even after eventual success"
    );
    assert_eq!(
        attempts
            .first()
            .and_then(|attempt| attempt.pointer("/outcome/stage"))
            .and_then(Value::as_str),
        Some("identity"),
        "the prior identity timeout retains its original failure stage"
    );
    assert_eq!(
        attempts
            .first()
            .and_then(|attempt| attempt.get("retry_admission"))
            .and_then(Value::as_str),
        Some("silent_identity_timeout"),
        "retry admission explicitly records the narrowly qualified timeout"
    );
    assert_eq!(
        attempts
            .last()
            .and_then(|attempt| attempt.pointer("/outcome/status"))
            .and_then(Value::as_str),
        Some("matched"),
        "the successful tuple belongs to its own attempt"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_silence_stops_at_four_opens() -> TestResult {
    let mut backend = ReadinessBackend::new((0..5).map(|_| silent_script()));
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(!report.succeeded(), "silence cannot qualify identity");
    assert!(
        matches!(
            report.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::Readiness,
                ..
            }
        ),
        "the final outcome names exhausted readiness policy, not a rewritten CAT attempt"
    );
    backend.assert_wire(&[&[b"ID\r"], &[b"ID\r"], &[b"ID\r"], &[b"ID\r"]])?;
    assert_eq!(
        attempts(&report)?.len(),
        4,
        "all bounded attempts remain visible"
    );
    Ok(())
}

#[tokio::test]
async fn partial_id_and_later_identity_timeouts_never_retry() -> TestResult {
    let mut partial = MockTransport::new();
    partial.expect_partial_then_hang(b"ID\r", b"ID TM-");
    let mut firmware = MockTransport::new();
    firmware.expect(b"ID\r", b"ID TM-D750\r");
    firmware.expect_hang(b"FV\r");
    let mut radio_type = MockTransport::new();
    radio_type.expect(b"ID\r", b"ID TM-D750\r");
    radio_type.expect(b"FV\r", b"FV 1.02\r");
    radio_type.expect_hang(b"TY\r");
    let cases: [(MockTransport, &[&[u8]]); 3] = [
        (partial, &[b"ID\r"]),
        (firmware, &[b"ID\r", b"FV\r"]),
        (radio_type, &[b"ID\r", b"FV\r", b"TY\r"]),
    ];
    for (script, commands) in cases {
        let mut backend = ReadinessBackend::new([script, ready_script()]);
        let report = run(&mut backend, &AtomicBool::new(false)).await?;
        assert!(!report.succeeded(), "incomplete identity is terminal");
        backend.assert_wire(&[commands])?;
    }
    Ok(())
}

#[tokio::test]
async fn wrong_or_malformed_identity_and_eof_never_retry() -> TestResult {
    for response in [b"ID TH-D75\r".as_slice(), b"ID unknown\r", b"?\r", b"\r"] {
        let mut script = MockTransport::new();
        script.expect(b"ID\r", response);
        let mut backend = ReadinessBackend::new([script, ready_script()]);
        let report = run(&mut backend, &AtomicBool::new(false)).await?;
        assert!(
            !report.succeeded(),
            "invalid identity is terminal: {response:?}"
        );
        backend.assert_wire(&[&[b"ID\r"]])?;
    }
    let mut script = MockTransport::new();
    script.expect_eof(b"ID\r");
    let mut backend = ReadinessBackend::new([script, ready_script()]);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(!report.succeeded(), "EOF is not a readiness timeout");
    backend.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn complete_but_different_firmware_never_retries() -> TestResult {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", b"FV 1.03\r");
    script.expect(b"TY\r", b"TY K,2,1\r");
    let mut backend = ReadinessBackend::new([script, ready_script()]);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(
        matches!(
            report.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::IdentityMismatch,
                ..
            }
        ),
        "the full identity tuple must match: {report:?}"
    );
    backend.assert_wire(&[&[b"ID\r", b"FV\r", b"TY\r"]])?;
    Ok(())
}

#[tokio::test]
async fn transport_and_cleanup_failures_never_retry() -> TestResult {
    for fault in [
        Fault::Read,
        Fault::Write,
        Fault::WriteTimeout,
        Fault::InvalidReadCount,
        Fault::Close,
        Fault::CloseTimeout,
    ] {
        let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
        backend.first()?.fault = fault;
        let report = run(&mut backend, &AtomicBool::new(false)).await?;
        assert!(!report.succeeded(), "{fault:?} must remain terminal");
        backend.assert_wire(&[&[b"ID\r"]])?;
    }
    Ok(())
}

#[tokio::test]
async fn open_failure_is_terminal_without_protocol_traffic() -> TestResult {
    let mut backend = ReadinessBackend::new([ready_script()]);
    backend.open_error = Some(io::ErrorKind::PermissionDenied);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(!report.succeeded(), "open failure is terminal");
    assert_eq!(backend.opens, 1, "never retry a failed open");
    let observed = backend
        .shared
        .observations
        .lock()
        .map_err(|_| "observation log poisoned")?;
    assert!(
        !observed
            .iter()
            .any(|event| matches!(event, Observation::Write(..) | Observation::Close(_))),
        "a failed open produces neither CAT nor a fabricated close"
    );
    drop(observed);
    Ok(())
}

#[tokio::test]
async fn cancellation_during_silent_id_or_close_prevents_another_attempt() -> TestResult {
    for during_write in [true, false] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
        if during_write {
            backend.first()?.cancel_on_write = Some(Arc::clone(&cancelled));
        } else {
            backend.first()?.cancel_on_close = Some(Arc::clone(&cancelled));
        }
        let report = run(&mut backend, &cancelled).await?;
        assert!(!report.succeeded(), "cancelled readiness cannot succeed");
        backend.assert_wire(&[&[b"ID\r"]])?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_identity_finishes_tuple_and_close() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut backend = ReadinessBackend::new([ready_script(), ready_script()]);
    backend.first()?.cancel_on_write = Some(Arc::clone(&cancelled));
    let report = run(&mut backend, &cancelled).await?;
    assert!(
        !report.succeeded(),
        "cancellation remains visible after a complete tuple"
    );
    backend.assert_wire(&[&[b"ID\r", b"FV\r", b"TY\r"]])?;
    Ok(())
}

#[tokio::test]
async fn cancellation_before_work_or_during_settle_prevents_open() -> TestResult {
    for initially_cancelled in [true, false] {
        let cancelled = Arc::new(AtomicBool::new(initially_cancelled));
        let mut backend = ReadinessBackend::new([ready_script()]);
        backend.cancel_on_wait = Some((1, Arc::clone(&cancelled)));
        let report = run(&mut backend, &cancelled).await?;
        assert!(
            !report.succeeded(),
            "cancelled workflow cannot qualify identity"
        );
        backend.assert_wire(&[])?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_retry_wait_prevents_another_open() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
    backend.cancel_on_wait = Some((2, Arc::clone(&cancelled)));
    let report = run(&mut backend, &cancelled).await?;
    assert!(
        matches!(report.outcome, VerificationOutcome::Cancelled),
        "cancellation at the retry boundary remains explicit"
    );
    backend.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn changed_endpoint_metadata_after_silence_prevents_reopen() -> TestResult {
    let mut changed = endpoint();
    changed.pid = Some(TMD750_MAIN_PID);
    let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
    backend.snapshots = VecDeque::from([vec![endpoint()], vec![changed]]);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(
        !report.succeeded(),
        "endpoint metadata changes are terminal"
    );
    backend.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn insufficient_identity_and_close_budget_prevents_open() -> TestResult {
    let mut backend = ReadinessBackend::new([ready_script()]);
    backend.enumeration_cost = Duration::from_secs(50);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(
        !report.succeeded(),
        "reserve complete identity and cleanup before open"
    );
    backend.assert_wire(&[])?;
    Ok(())
}

#[tokio::test]
async fn deadline_expiry_during_open_still_closes_without_cat() -> TestResult {
    let mut backend = ReadinessBackend::new([ready_script()]);
    backend.open_cost = Duration::from_secs(61);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(
        !report.succeeded(),
        "an expired dispatch deadline forbids CAT"
    );
    backend.assert_wire(&[&[]])?;
    Ok(())
}

#[tokio::test]
async fn attempt_and_cleanup_time_consume_the_shared_retry_budget() -> TestResult {
    let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
    backend.first()?.close_cost = Duration::from_secs(49);
    let report = run(&mut backend, &AtomicBool::new(false)).await?;
    assert!(!report.succeeded(), "a new retry cannot reset the deadline");
    backend.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn capture_failure_before_open_blocks_dispatch_with_independent_cancel_flags() -> TestResult {
    let file = tempfile::NamedTempFile::new()?;
    let mut backend = ReadinessBackend::new([ready_script()]);
    let cancelled = AtomicBool::new(false);
    let report = run_with_file(&mut backend, File::open(file.path())?, &cancelled).await?;
    assert!(
        !report.succeeded(),
        "capture failure prevents qualification"
    );
    assert!(
        !report.transcript.complete,
        "failed recording remains visible"
    );
    assert!(
        !cancelled.load(Ordering::Relaxed),
        "the user flag is independent"
    );
    backend.assert_wire(&[])?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn synchronization_failure_after_silent_identity_prevents_retry() -> TestResult {
    let mut backend = ReadinessBackend::new([silent_script(), ready_script()]);
    let file = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    let report = run_with_file(&mut backend, file, &AtomicBool::new(false)).await?;
    assert!(!report.succeeded(), "retry requires synchronized evidence");
    assert!(
        !report.transcript.complete,
        "synchronization failure is capture failure"
    );
    backend.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}
