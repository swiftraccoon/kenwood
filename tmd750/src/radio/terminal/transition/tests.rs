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
const GET_VERSION: &[u8] = b"\xE0\x03\x00";

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
        if bytes != GET_VERSION {
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
    Pending,
}

struct FakeHost {
    log: Log,
    opens: VecDeque<Open>,
    waits: VecDeque<Wait>,
    retirements: Vec<usize>,
    retirement_fails: bool,
}

impl FakeHost {
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

fn open_failure(message: &str, retry_allowed: bool) -> ModemOpenFailure {
    ModemOpenFailure {
        source: io::Error::other(message.to_owned()).into(),
        retry_allowed,
        released: true,
    }
}

impl ModemHost for FakeHost {
    type Connection = Connection;

    async fn open(&mut self, _cancelled: &AtomicBool) -> Result<Connection, ModemOpenFailure> {
        Err(open_failure(
            "the transition reopens; it never performs the initial open",
            false,
        ))
    }

    async fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Connection, ModemOpenFailure> {
        record(&self.log, Seen::Open).map_err(|error| ModemOpenFailure {
            source: error.into(),
            retry_allowed: false,
            released: true,
        })?;
        match self.opens.pop_front() {
            Some(Open::Ready(connection)) => Ok(connection),
            Some(Open::Late(connection)) => {
                tokio::time::sleep_until(deadline).await;
                Ok(connection)
            }
            Some(Open::Cancelled(connection)) => {
                cancelled.store(true, Ordering::Release);
                Ok(connection)
            }
            Some(Open::Failed) => Err(open_failure("scripted reopen failure", true)),
            Some(Open::Refused) => Err(open_failure("scripted opening refusal", false)),
            None => {
                cancelled.store(true, Ordering::Release);
                Err(open_failure("no scripted opening remains", false))
            }
        }
    }

    async fn wait(&mut self, duration: Duration) {
        let recorded = record(&self.log, Seen::Wait(duration));
        assert!(recorded.is_ok(), "record simulated wait");
        match self.waits.pop_front().unwrap_or(Wait::Immediate) {
            Wait::Immediate => {}
            Wait::Pending => std::future::pending().await,
        }
    }

    async fn retire(&mut self, connection: Connection) -> Result<(), CloseFailure> {
        self.retirements.push(connection.id);
        close_within(connection, CLOSE_BUDGET).await?;
        if self.retirement_fails {
            Err(CloseFailure::Host(
                io::Error::other("scripted capture retirement failure").into(),
            ))
        } else {
            Ok(())
        }
    }
}

/// A host whose scripted wait sets the cancellation flag before returning.
struct CancellingHost {
    inner: FakeHost,
    cancelled: Arc<AtomicBool>,
}

impl ModemHost for CancellingHost {
    type Connection = Connection;

    async fn open(&mut self, cancelled: &AtomicBool) -> Result<Connection, ModemOpenFailure> {
        self.inner.open(cancelled).await
    }

    async fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Connection, ModemOpenFailure> {
        self.inner.reopen(deadline, cancelled).await
    }

    async fn wait(&mut self, duration: Duration) {
        self.inner.wait(duration).await;
        self.cancelled.store(true, Ordering::Release);
    }

    async fn retire(&mut self, connection: Connection) -> Result<(), CloseFailure> {
        self.inner.retire(connection).await
    }
}

fn events(log: &Log) -> Result<Vec<Seen>, Box<dyn std::error::Error>> {
    Ok(log.lock().map_err(|_| "test log poisoned")?.clone())
}

#[tokio::test]
async fn original_version_proof_preserves_that_owner_without_reopening() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut host = FakeHost::new(&log, Vec::new());
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
    assert!(report.error.is_none());
    assert!(report.released());
    assert_eq!(report.attempts.len(), 1);
    let attempt = report.attempts.first().ok_or("missing attempt")?;
    assert!(attempt.version_proved);
    assert_eq!(attempt.step, TransitionStep::Probe);
    let proof = report.proof.ok_or("missing binary proof")?;
    assert_eq!(proof.transport().id, 1);
    assert_eq!(proof.version().description, "MMDVM 2018");
    assert_eq!(proof.version().protocol, 1);
    let owner = proof.into_transport();
    assert_eq!(owner.id, 1);
    assert_eq!(
        events(&log)?,
        [
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(1, GET_VERSION.to_vec())
        ]
    );
    close_within(owner, CLOSE_BUDGET).await?;
    Ok(())
}

#[tokio::test]
async fn negative_probe_closes_and_drops_before_reopen_and_waits_before_next_probe() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let fresh = Connection::new(2, &log, VERSION);
    let mut host = FakeHost::new(&log, vec![Open::Ready(fresh)]);
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
    assert!(report.error.is_none());
    assert_eq!(report.attempts.len(), 2);
    assert!(
        report
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .probe_error
            .is_some()
    );
    assert!(
        report
            .attempts
            .get(1)
            .ok_or("missing fresh attempt")?
            .version_proved
    );
    assert_eq!(
        events(&log)?,
        [
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(1, GET_VERSION.to_vec()),
            Seen::Close(1),
            Seen::Drop(1),
            Seen::Open,
            Seen::Wait(POLL_INTERVAL),
            Seen::Write(2, GET_VERSION.to_vec()),
        ]
    );
    close_within(
        report.proof.ok_or("missing proof")?.into_transport(),
        CLOSE_BUDGET,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn failed_reopen_is_retained_when_a_later_owner_proves_version() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let fresh = Connection::new(2, &log, VERSION);
    let mut host = FakeHost::new(&log, vec![Open::Failed, Open::Ready(fresh)]);
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
    assert!(report.error.is_none());
    assert_eq!(report.attempts.len(), 3);
    let attempt = report.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert_eq!(
        attempt
            .reopen_error
            .as_ref()
            .ok_or("missing reopen failure")?
            .to_string(),
        "scripted reopen failure"
    );
    assert!(!attempt.reopen_refused());
    assert_eq!(
        report.attempts.get(1).map(|attempt| attempt.step),
        Some(TransitionStep::Reopen)
    );
    close_within(
        report.proof.ok_or("missing proof")?.into_transport(),
        CLOSE_BUDGET,
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn failed_close_stops_before_reopen_and_keeps_both_errors() -> TestResult {
    let log = Log::default();
    let mut original = Connection::new(1, &log, b"");
    original.close = Close::Failed;
    let mut host = FakeHost::new(&log, Vec::new());
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::RetirementFailed));
    assert!(!report.released());
    let attempt = report.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert!(matches!(
        attempt.retirement_error,
        Some(CloseFailure::Transport(_))
    ));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    assert!(!events(&log)?.contains(&Seen::Open));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_the_wait_closes_the_original_without_a_probe() -> TestResult {
    let log = Log::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let original = Connection::new(1, &log, VERSION);
    let mut host = CancellingHost {
        inner: FakeHost::new(&log, Vec::new()),
        cancelled: Arc::clone(&cancelled),
    };
    let report = acquire_modem(&mut host, Some(original), &cancelled).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::Cancelled));
    assert!(report.attempts.is_empty());
    assert_eq!(host.inner.retirements, [1]);
    assert_eq!(
        events(&log)?,
        [Seen::Wait(POLL_INTERVAL), Seen::Close(1), Seen::Drop(1)]
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_racing_successful_version_closes_instead_of_admitting_proof() -> TestResult {
    let log = Log::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut original = Connection::new(1, &log, VERSION);
    original.cancel_on_read = Some(cancelled.clone());
    original.close = Close::Failed;
    let mut host = FakeHost::new(&log, Vec::new());
    let report = acquire_modem(&mut host, Some(original), &cancelled).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::Cancelled));
    assert!(report.cleanup_error.is_some());
    assert!(!report.released());
    assert_eq!(host.retirements, [1]);
    assert!(
        report
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
        let mut host = FakeHost::new(&log, vec![opening]);
        let report = acquire_modem_within(
            &mut host,
            None,
            &AtomicBool::new(false),
            Duration::from_millis(10),
        )
        .await;
        assert!(report.proof.is_none());
        assert!(report.error.is_some());
        assert!(report.cleanup_error.is_none());
        assert_eq!(host.retirements, [1]);
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
    let mut host = FakeHost::new(&log, Vec::new());
    host.waits.push_back(Wait::Pending);
    let report = acquire_modem_within(
        &mut host,
        Some(original),
        &AtomicBool::new(false),
        Duration::from_millis(10),
    )
    .await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::WindowExpired));
    assert!(report.attempts.is_empty());
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn invalid_or_truncated_version_never_creates_proof() -> TestResult {
    for reply in [
        GET_VERSION,
        b"\xE0\x0E\x00\x01MM".as_slice(),
        b"ID TM-D750\r",
    ] {
        let log = Log::default();
        let original = Connection::new(1, &log, reply);
        let mut host = FakeHost::new(&log, Vec::new());
        let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
        assert!(report.proof.is_none());
        assert!(
            report
                .attempts
                .first()
                .ok_or("missing original attempt")?
                .probe_error
                .is_some()
        );
        assert!(
            report
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
    let mut host = FakeHost::new(&log, Vec::new());
    let report = acquire_modem_within(
        &mut host,
        Some(original),
        &AtomicBool::new(false),
        Duration::from_millis(10),
    )
    .await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::WindowExpired));
    assert!(matches!(
        report
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .probe_error,
        Some(ProbeError::Timeout { .. } | ProbeError::InvalidTimeout { .. })
    ));
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn pending_close_is_bounded_and_dropped_without_reopening() -> TestResult {
    let log = Log::default();
    let mut original = Connection::new(1, &log, b"");
    original.close = Close::Pending;
    let mut host = FakeHost::new(&log, Vec::new());
    let report = tokio::time::timeout(
        Duration::from_secs(3),
        acquire_modem(&mut host, Some(original), &AtomicBool::new(false)),
    )
    .await;
    let report = report?;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::RetirementFailed));
    assert!(matches!(
        report
            .attempts
            .first()
            .ok_or("missing original attempt")?
            .retirement_error,
        Some(CloseFailure::Timeout { budget }) if budget == CLOSE_BUDGET
    ));
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn already_cancelled_entry_closes_without_waiting_or_probing() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut host = FakeHost::new(&log, Vec::new());
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(true)).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::Cancelled));
    assert_eq!(events(&log)?, [Seen::Close(1), Seen::Drop(1)]);
    assert_eq!(host.retirements, [1]);
    Ok(())
}

#[tokio::test]
async fn nonretryable_reopen_stops_without_another_wait_or_open() -> TestResult {
    for initial in [false, true] {
        let log = Log::default();
        let original = initial.then(|| Connection::new(1, &log, b""));
        let mut host = FakeHost::new(&log, vec![Open::Refused]);
        let report = acquire_modem(&mut host, original, &AtomicBool::new(false)).await;
        assert!(report.proof.is_none());
        assert_eq!(report.error, Some(TransitionError::ReopenRefused));
        let attempt = report.attempts.first().ok_or("missing opening attempt")?;
        assert_eq!(
            attempt
                .reopen_error
                .as_ref()
                .ok_or("missing reopen failure")?
                .to_string(),
            "scripted opening refusal"
        );
        assert!(attempt.reopen_refused());
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
async fn failed_host_retirement_stops_before_reopen_after_successful_close() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, b"");
    let mut host = FakeHost::new(&log, Vec::new());
    host.retirement_fails = true;
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(false)).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::RetirementFailed));
    let attempt = report.attempts.first().ok_or("missing original attempt")?;
    assert!(attempt.probe_error.is_some());
    assert_eq!(
        attempt
            .retirement_error
            .as_ref()
            .ok_or("lost host retirement failure")?
            .to_string(),
        "host release failed: scripted capture retirement failure"
    );
    assert_eq!(host.retirements, [1]);
    assert!(!events(&log)?.contains(&Seen::Open));
    assert_eq!(events(&log)?.last(), Some(&Seen::Drop(1)));
    Ok(())
}

#[tokio::test]
async fn cancellation_cleanup_retains_the_host_release_failure_independently() -> TestResult {
    let log = Log::default();
    let original = Connection::new(1, &log, VERSION);
    let mut host = FakeHost::new(&log, Vec::new());
    host.retirement_fails = true;
    let report = acquire_modem(&mut host, Some(original), &AtomicBool::new(true)).await;
    assert!(report.proof.is_none());
    assert_eq!(report.error, Some(TransitionError::Cancelled));
    assert!(matches!(report.cleanup_error, Some(CloseFailure::Host(_))));
    assert_eq!(host.retirements, [1]);
    assert_eq!(events(&log)?, [Seen::Close(1), Seen::Drop(1)]);
    Ok(())
}
