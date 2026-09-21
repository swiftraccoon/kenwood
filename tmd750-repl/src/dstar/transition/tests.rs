//! MMDVM acquisition over in-memory transports: reusing the original
//! connection, closing before each reopen, the window and close budgets, and
//! the cancellation and truncated-reply paths.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use kenwood_transport::TransportError;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Log = Arc<Mutex<Vec<Seen>>>;

const VERSION: &[u8] = b"\xE0\x0E\x00\x01MMDVM 2018";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Wait(Duration),
    Open,
    Write(usize, Vec<u8>),
    Close(usize),
    Drop(usize),
}

fn record(log: &Log, event: Seen) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

struct Connection {
    id: usize,
    log: Log,
    reply: VecDeque<u8>,
    cancel_on_read: Option<Arc<AtomicBool>>,
    close: Close,
    read_hangs: bool,
}

enum Close {
    Ready,
    Failed,
    Pending,
}

impl Connection {
    fn new(id: usize, log: &Log, reply: &[u8]) -> Self {
        Self {
            id,
            log: log.clone(),
            reply: reply.iter().copied().collect(),
            cancel_on_read: None,
            close: Close::Ready,
            read_hangs: false,
        }
    }
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        record(&self.log, Seen::Write(self.id, bytes.to_vec()))?;
        if bytes != b"\xE0\x03\x00" {
            return Err(TransportError::Write(io::Error::other(
                "only GET_VERSION belongs in this transition",
            )));
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        if let Some(cancelled) = &self.cancel_on_read {
            cancelled.store(true, Ordering::Release);
        }
        if self.read_hangs {
            return std::future::pending().await;
        }
        let count = bytes.len().min(self.reply.len());
        for slot in bytes.iter_mut().take(count) {
            *slot = self.reply.pop_front().ok_or_else(|| {
                TransportError::Read(io::Error::other("scripted reply length changed"))
            })?;
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        record(&self.log, Seen::Close(self.id))?;
        match self.close {
            Close::Failed => Err(TransportError::Disconnected(io::Error::other(
                "scripted close failure",
            ))),
            Close::Ready => Ok(()),
            Close::Pending => std::future::pending().await,
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Drop(self.id));
        }
    }
}

enum Open {
    Ready(Connection),
    Failed,
    Refused,
    Late(Connection),
    Cancelled(Connection),
}

enum Wait {
    Immediate,
    Cancel,
    Fail,
    Pending,
}

struct FakeBackend {
    log: Log,
    opens: VecDeque<Open>,
    waits: VecDeque<Wait>,
    retirements: Vec<usize>,
    retirement_fails: bool,
}

impl FakeBackend {
    fn new(log: &Log, opens: Vec<Open>) -> Self {
        Self {
            log: log.clone(),
            opens: opens.into(),
            waits: VecDeque::new(),
            retirements: Vec::new(),
            retirement_fails: false,
        }
    }
}

impl Backend for FakeBackend {
    type Connection = Connection;

    async fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self::Connection, ReopenFailure> {
        record(&self.log, Seen::Open).map_err(|error| ReopenFailure {
            error: Failure::from_error(&error),
            retry_allowed: false,
        })?;
        match self.opens.pop_front() {
            Some(Open::Ready(owner)) => Ok(owner),
            Some(Open::Late(owner)) => {
                tokio::time::sleep_until(deadline).await;
                Ok(owner)
            }
            Some(Open::Cancelled(owner)) => {
                cancelled.store(true, Ordering::Release);
                Ok(owner)
            }
            Some(Open::Failed) => Err(ReopenFailure {
                error: failure("scripted reopen failure"),
                retry_allowed: true,
            }),
            Some(Open::Refused) => Err(ReopenFailure {
                error: failure("scripted opening refusal"),
                retry_allowed: false,
            }),
            None => {
                cancelled.store(true, Ordering::Release);
                Err(ReopenFailure {
                    error: failure("no scripted opening remains"),
                    retry_allowed: false,
                })
            }
        }
    }

    async fn wait(&mut self, duration: Duration, cancelled: &AtomicBool) -> Result<(), Failure> {
        record(&self.log, Seen::Wait(duration)).map_err(|error| Failure::from_error(&error))?;
        match self.waits.pop_front().unwrap_or(Wait::Immediate) {
            Wait::Immediate => Ok(()),
            Wait::Cancel => {
                cancelled.store(true, Ordering::Release);
                Ok(())
            }
            Wait::Fail => Err(failure("scripted wait failure")),
            Wait::Pending => std::future::pending().await,
        }
    }

    async fn retire(&mut self, owner: Self::Connection) -> Result<(), Failure> {
        self.retirements.push(owner.id);
        close(owner).await?;
        if self.retirement_fails {
            Err(failure("scripted capture retirement failure"))
        } else {
            Ok(())
        }
    }
}

fn events(log: &Log) -> Result<Vec<Seen>, Box<dyn std::error::Error>> {
    Ok(log.lock().map_err(|_| "test log poisoned")?.clone())
}

#[tokio::test]
async fn original_version_proof_preserves_that_owner_without_reopening() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
    assert!(outcome.error.is_none());
    assert_eq!(outcome.attempts.len(), 1);
    assert!(
        outcome
            .attempts
            .first()
            .ok_or("missing attempt")?
            .version_proved
    );
    let proof = outcome.proof.ok_or("missing binary proof")?;
    assert_eq!(proof.transport().id, 1);
    let owner = proof.into_transport();
    assert_eq!(owner.id, 1);
    assert_eq!(
        events(&log)?,
        [
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(1, b"\xE0\x03\x00".to_vec())
        ]
    );
    close(owner).await.map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn negative_probe_closes_and_drops_before_reopen_and_waits_before_next_probe() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let fresh = Connection::new(2, &log, VERSION);
    let mut backend = FakeBackend::new(&log, vec![Open::Ready(fresh)]);
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
    assert!(outcome.error.is_none());
    assert_eq!(outcome.attempts.len(), 2);
    assert!(
        outcome
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .probe_error
            .is_some()
    );
    assert!(
        outcome
            .attempts
            .get(1)
            .ok_or("missing fresh attempt")?
            .version_proved
    );
    assert_eq!(
        events(&log)?,
        [
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(1, b"\xE0\x03\x00".to_vec()),
            Seen::Close(1),
            Seen::Drop(1),
            Seen::Open,
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(2, b"\xE0\x03\x00".to_vec()),
        ]
    );
    close(outcome.proof.ok_or("missing proof")?.into_transport())
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn failed_reopen_is_retained_when_a_later_owner_proves_version() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let fresh = Connection::new(2, &log, VERSION);
    let mut backend = FakeBackend::new(&log, vec![Open::Failed, Open::Ready(fresh)]);
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
    assert!(outcome.error.is_none());
    assert_eq!(outcome.attempts.len(), 3);
    let attempt = outcome.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert_eq!(
        attempt
            .reopen_error
            .as_ref()
            .ok_or("missing reopen failure")?
            .message,
        "scripted reopen failure"
    );
    close(outcome.proof.ok_or("missing proof")?.into_transport())
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[tokio::test]
async fn failed_close_stops_before_reopen_and_keeps_both_errors() -> TestResult {
    let log = Log::default();
    let mut original = Connection::new(1, &log, b"");
    original.close = Close::Failed;
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    let attempt = outcome.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert!(attempt.retirement_error.is_some());
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    assert!(!events(&log)?.contains(&Seen::Open));
    Ok(())
}

#[tokio::test]
async fn cancelled_or_failed_wait_closes_original_without_probe() -> TestResult {
    for wait in [Wait::Cancel, Wait::Fail] {
        let log = Log::default();
        let original = Connection::new(1, &log, VERSION);
        let mut backend = FakeBackend::new(&log, Vec::new());
        backend.waits.push_back(wait);
        let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
        assert!(outcome.proof.is_none());
        assert!(outcome.error.is_some());
        assert!(outcome.attempts.is_empty());
        assert_eq!(backend.retirements, [1]);
        assert_eq!(
            events(&log)?,
            [Seen::Wait(POLL_INTERVAL), Seen::Close(1), Seen::Drop(1)]
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_racing_successful_version_closes_instead_of_admitting_proof() -> TestResult {
    let log = Log::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut original = Connection::new(1, &log, VERSION);
    original.cancel_on_read = Some(cancelled.clone());
    original.close = Close::Failed;
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = run(&mut backend, Some(original), &cancelled).await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    assert!(outcome.cleanup_error.is_some());
    assert_eq!(backend.retirements, [1]);
    assert!(
        outcome
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .version_proved
    );
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn late_or_cancelled_open_owner_is_closed_without_protocol_io() -> TestResult {
    for late in [false, true] {
        let log = Log::default();
        let fresh = Connection::new(1, &log, VERSION);
        let opening = if late {
            Open::Late(fresh)
        } else {
            Open::Cancelled(fresh)
        };
        let mut backend = FakeBackend::new(&log, vec![opening]);
        let outcome = run_with_window(
            &mut backend,
            None,
            &AtomicBool::new(false),
            Duration::from_millis(10),
        )
        .await;
        assert!(outcome.proof.is_none());
        assert!(outcome.error.is_some());
        assert!(outcome.cleanup_error.is_none());
        assert_eq!(backend.retirements, [1]);
        assert!(
            events(&log)?
                .iter()
                .all(|event| !matches!(event, Seen::Write(..)))
        );
        assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    }
    Ok(())
}

#[tokio::test]
async fn absolute_window_bounds_a_pending_wait_and_still_closes_original() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut backend = FakeBackend::new(&log, Vec::new());
    backend.waits.push_back(Wait::Pending);
    let outcome = run_with_window(
        &mut backend,
        Some(original),
        &AtomicBool::new(false),
        Duration::from_millis(10),
    )
    .await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    assert!(outcome.attempts.is_empty());
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn invalid_or_truncated_version_never_creates_proof() -> TestResult {
    for reply in [
        b"\xE0\x03\x00".as_slice(),
        b"\xE0\x0E\x00\x01MM",
        b"ID TM-D750\r",
    ] {
        let log = Log::default();
        let original = Connection::new(1, &log, reply);
        let mut backend = FakeBackend::new(&log, Vec::new());
        let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
        assert!(outcome.proof.is_none());
        assert!(
            outcome
                .attempts
                .first()
                .ok_or("missing original attempt")?
                .probe_error
                .is_some()
        );
        assert!(
            outcome
                .attempts
                .iter()
                .all(|attempt| !attempt.version_proved)
        );
        assert!(events(&log)?.contains(&Seen::Drop(1)));
    }
    Ok(())
}

#[tokio::test]
async fn absolute_window_caps_pending_version_and_prevents_another_open() -> TestResult {
    let log = Log::default();
    let mut original = Connection::new(1, &log, b"");
    original.read_hangs = true;
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = run_with_window(
        &mut backend,
        Some(original),
        &AtomicBool::new(false),
        Duration::from_millis(10),
    )
    .await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    assert!(
        outcome
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .probe_error
            .is_some()
    );
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn pending_close_is_bounded_and_dropped_without_reopening() -> TestResult {
    let log = Log::default();
    let mut original = Connection::new(1, &log, b"");
    original.close = Close::Pending;
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        run(&mut backend, Some(original), &AtomicBool::new(false)),
    )
    .await?;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    assert!(
        outcome
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .retirement_error
            .is_some()
    );
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn already_cancelled_entry_closes_without_waiting_or_probing() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut backend = FakeBackend::new(&log, Vec::new());
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(true)).await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    assert_eq!(events(&log)?, [Seen::Close(1), Seen::Drop(1)]);
    assert_eq!(backend.retirements, [1]);
    Ok(())
}

#[tokio::test]
async fn nonretryable_reopen_stops_without_another_wait_or_open() -> TestResult {
    for initial in [false, true] {
        let log = Log::default();
        let original = initial.then(|| Connection::new(1, &log, b""));
        let mut backend = FakeBackend::new(&log, vec![Open::Refused]);
        let outcome = run(&mut backend, original, &AtomicBool::new(false)).await;
        assert!(outcome.proof.is_none());
        assert_eq!(
            outcome.error.as_ref().ok_or("missing refusal")?.message,
            "scripted opening refusal"
        );
        let attempt = outcome.attempts.first().ok_or("missing opening attempt")?;
        assert!(attempt.reopen_error.is_some());
        assert!(!attempt.reopen_retry_allowed);
        assert_eq!(events(&log)?.last(), Some(&Seen::Open));
        assert_eq!(
            events(&log)?
                .iter()
                .filter(|event| matches!(event, Seen::Wait(_)))
                .count(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_capture_retirement_stops_before_reopen_after_successful_close() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let mut backend = FakeBackend::new(&log, Vec::new());
    backend.retirement_fails = true;
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(false)).await;
    assert!(outcome.proof.is_none());
    assert!(outcome.error.is_some());
    let attempt = outcome.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert_eq!(
        attempt
            .retirement_error
            .as_ref()
            .ok_or("lost capture retirement failure")?
            .message,
        "scripted capture retirement failure"
    );
    assert_eq!(backend.retirements, [1]);
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn cancellation_cleanup_retains_capture_failure_independently() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut backend = FakeBackend::new(&log, Vec::new());
    backend.retirement_fails = true;
    let outcome = run(&mut backend, Some(original), &AtomicBool::new(true)).await;
    assert!(outcome.proof.is_none());
    assert_eq!(
        outcome
            .error
            .as_ref()
            .ok_or("missing cancellation")?
            .message,
        "MMDVM transition cancelled"
    );
    assert_eq!(
        outcome
            .cleanup_error
            .as_ref()
            .ok_or("lost independent capture failure")?
            .message,
        "scripted capture retirement failure"
    );
    assert_eq!(backend.retirements, [1]);
    assert_eq!(events(&log)?, [Seen::Close(1), Seen::Drop(1)]);
    Ok(())
}
