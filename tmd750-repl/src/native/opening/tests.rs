//! Fake selected opens exercise capture and ownership without native I/O.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use kenwood_transport::TransportError;

use super::*;
use crate::AppResult;

type TestResult = AppResult<()>;
const ADDRESS: &str = "01-23-45-67-89-AB";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Open(String, BluetoothService),
    Wait(Duration),
    Write,
    Read,
    Close,
    Drop,
}

type Log = Arc<Mutex<Vec<Seen>>>;

fn record(log: &Log, event: Seen) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

struct Connection {
    log: Log,
    close_fails: bool,
}

impl Transport for Connection {
    async fn write(&mut self, _bytes: &[u8]) -> Result<(), TransportError> {
        record(&self.log, Seen::Write)?;
        Err(TransportError::Write(io::Error::other(
            "unexpected protocol write",
        )))
    }

    async fn read(&mut self, _bytes: &mut [u8]) -> Result<usize, TransportError> {
        record(&self.log, Seen::Read)?;
        Err(TransportError::Read(io::Error::other(
            "unexpected protocol read",
        )))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        record(&self.log, Seen::Close)?;
        if self.close_fails {
            Err(TransportError::Disconnected(io::Error::other(
                "fake close failure",
            )))
        } else {
            Ok(())
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Drop);
        }
    }
}

enum Step {
    Failure(OpenFailure),
    Success {
        resolved: Resolved,
        cancel: bool,
        close_fails: bool,
    },
}

fn success(channel: u8) -> Step {
    Step::Success {
        resolved: Resolved {
            address: ADDRESS.to_owned(),
            rfcomm_channel: channel,
        },
        cancel: false,
        close_fails: false,
    }
}

fn eligible_failure() -> Step {
    Step::Failure(OpenFailure::from_error(&TransportError::NotFound))
}

struct FakeBackend {
    steps: VecDeque<Step>,
    cancelled: Arc<AtomicBool>,
    cancel_on_wait: bool,
    log: Log,
}

impl Backend for FakeBackend {
    type Connection = Connection;

    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        _cancelled: &AtomicBool,
    ) -> Result<Opened<Connection>, OpenFailure> {
        record(&self.log, Seen::Open(endpoint.address.to_string(), service))
            .map_err(|error| OpenFailure::from_error(&error))?;
        match self.steps.pop_front().ok_or_else(|| {
            OpenFailure::from_error(&io::Error::other("unexpected additional open"))
        })? {
            Step::Failure(error) => Err(error),
            Step::Success {
                resolved,
                cancel,
                close_fails,
            } => {
                if cancel {
                    self.cancelled.store(true, Ordering::Relaxed);
                }
                Ok(Opened {
                    connection: Connection {
                        log: Arc::clone(&self.log),
                        close_fails,
                    },
                    resolved,
                })
            }
        }
    }

    async fn wait(&mut self, duration: Duration) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Wait(duration));
        }
        if self.cancel_on_wait {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

struct Harness {
    temporary: tempfile::TempDir,
    backend: FakeBackend,
    recorder: Option<Recorder<File>>,
}

impl Harness {
    fn new(steps: impl IntoIterator<Item = Step>) -> AppResult<Self> {
        let temporary = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let file = crate::capture::create_private_file(&temporary.path().join("opening.jsonl"))?;
        Ok(Self {
            temporary,
            recorder: Some(Recorder::named(
                file,
                Arc::clone(&cancelled),
                "opening.jsonl",
            )),
            backend: FakeBackend {
                steps: steps.into_iter().collect(),
                cancelled,
                cancel_on_wait: false,
                log: Arc::new(Mutex::new(Vec::new())),
            },
        })
    }

    async fn run(&mut self, service: BluetoothService) -> AppResult<Selection<Connection>> {
        let recorder = self.recorder.take().ok_or("recorder already consumed")?;
        let cancelled = Arc::clone(&self.backend.cancelled);
        Ok(open_selected(
            &mut self.backend,
            &Endpoint {
                address: ADDRESS.parse()?,
                helper: None,
            },
            service,
            recorder,
            &cancelled,
        )
        .await)
    }

    fn events(&self) -> AppResult<Vec<Seen>> {
        Ok(self
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .clone())
    }

    fn transcript(&self) -> AppResult<Vec<serde_json::Value>> {
        std::fs::read_to_string(self.temporary.path().join("opening.jsonl"))?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }
}

async fn retire(selection: &mut Selection<Connection>) -> TestResult {
    let mut opened = selection.opened.take().ok_or("admitted owner absent")?;
    assert!(
        super::super::close(&mut opened.transport).await.is_none(),
        "accepted fake owner closes cleanly"
    );
    drop(opened);
    Ok(())
}

#[tokio::test]
async fn exact_service_and_dynamic_channel_are_retained_without_protocol_traffic() -> TestResult {
    for service in [
        BluetoothService::SerialPort,
        BluetoothService::FixedChannel(RfcommChannel::new(9)?),
    ] {
        let mut harness = Harness::new([success(9)])?;
        let mut selected = harness.run(service).await?;
        assert!(selected.history.succeeded(), "{:?}", selected.history);
        let opened = selected.opened.as_ref().ok_or("owner absent")?;
        assert_eq!(
            opened.channel.get(),
            9,
            "retain actual channel without a model constant"
        );
        assert_eq!(opened.resolved.address, ADDRESS, "retain exact address");
        assert_eq!(
            harness.events()?,
            [Seen::Open(ADDRESS.to_owned(), service)],
            "selection sends no protocol bytes"
        );
        retire(&mut selected).await?;
        assert_eq!(
            harness.events()?,
            [
                Seen::Open(ADDRESS.to_owned(), service),
                Seen::Close,
                Seen::Drop
            ],
            "one close precedes owner retirement"
        );
    }
    Ok(())
}

#[tokio::test]
async fn eligible_failure_then_success_preserves_both_attempts_and_one_capture() -> TestResult {
    let mut harness = Harness::new([eligible_failure(), success(13)])?;
    let mut selected = harness.run(BluetoothService::SerialPort).await?;
    assert!(selected.history.succeeded(), "{:?}", selected.history);
    assert_eq!(
        selected.history.attempts.len(),
        2,
        "retain failure and success"
    );
    let first = selected
        .history
        .attempts
        .first()
        .ok_or("first attempt missing")?;
    assert!(
        first.started && first.resolved.is_none() && first.error.is_some(),
        "first failure remains complete"
    );
    assert_eq!(first.number, 1, "number first dispatched attempt");
    let last = selected
        .history
        .attempts
        .last()
        .ok_or("last attempt missing")?;
    assert!(
        last.started && last.resolved.is_some() && last.error.is_none(),
        "second attempt is independently accepted"
    );
    assert_eq!(last.number, 2, "number successful retry");
    assert_eq!(
        harness.events()?,
        [
            Seen::Open(ADDRESS.to_owned(), BluetoothService::SerialPort),
            Seen::Wait(Duration::from_secs(1)),
            Seen::Open(ADDRESS.to_owned(), BluetoothService::SerialPort)
        ],
        "retry repeats the exact selection after one bounded wait"
    );
    let records = harness.transcript()?;
    let kinds: Vec<_> = records
        .iter()
        .filter_map(|row| {
            row.pointer("/event/kind")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "native_open_requested",
            "native_open_failed",
            "native_open_retry_wait",
            "native_open_retry_wait_completed",
            "native_open_requested",
            "native_open_completed"
        ],
        "one recorder preserves failed attempt and retry boundaries"
    );
    for (index, row) in records.iter().enumerate() {
        assert_eq!(
            row.get("sequence").and_then(serde_json::Value::as_u64),
            Some(u64::try_from(index)?),
            "event sequence never restarts"
        );
    }
    let json = serde_json::to_value(&selected.history)?;
    assert!(
        json.pointer("/attempts/0/error")
            .is_some_and(serde_json::Value::is_object),
        "saved history retains prior error"
    );
    assert!(
        json.pointer("/attempts/1/error")
            .is_some_and(serde_json::Value::is_null),
        "saved history distinguishes success"
    );
    retire(&mut selected).await
}

#[tokio::test]
async fn eligible_exhaustion_never_starts_a_third_attempt() -> TestResult {
    let mut harness = Harness::new([eligible_failure(), eligible_failure(), success(27)])?;
    let selected = harness.run(BluetoothService::SerialPort).await?;
    assert!(
        selected.opened.is_none() && !selected.history.succeeded(),
        "exhaustion cannot admit an owner"
    );
    assert_eq!(selected.history.attempts.len(), 2, "two-attempt bound");
    assert!(
        selected
            .history
            .attempts
            .iter()
            .all(|attempt| attempt.error.is_some()),
        "both failures remain recorded"
    );
    assert_eq!(
        harness.backend.steps.len(),
        1,
        "third fixture is never consumed"
    );
    assert_eq!(harness.events()?.len(), 3, "exactly two opens and one wait");
    Ok(())
}

#[tokio::test]
async fn nonretryable_or_independent_cleanup_failure_stops_selection() -> TestResult {
    let mut independent_close = OpenFailure::from_error(&TransportError::NotFound);
    independent_close.close_error =
        Some(Failure::from_error(&io::Error::other("late-owner cleanup")));
    for error in [
        OpenFailure::from_error(&io::Error::other("not a typed native stage")),
        independent_close,
    ] {
        let mut harness = Harness::new([Step::Failure(error), success(27)])?;
        let selected = harness.run(BluetoothService::SerialPort).await?;
        assert!(
            selected.opened.is_none() && !selected.history.succeeded(),
            "ineligible failure stops selection"
        );
        assert_eq!(
            selected.history.attempts.len(),
            1,
            "one failed attempt retained"
        );
        assert_eq!(
            harness.backend.steps.len(),
            1,
            "no retry after independent cleanup uncertainty"
        );
        assert_eq!(
            harness.events()?.len(),
            1,
            "no retry wait or protocol traffic"
        );
    }
    Ok(())
}

#[tokio::test]
async fn wrong_address_invalid_channel_and_fixed_channel_mismatch_close_without_retry() -> TestResult
{
    for (address, channel) in [
        ("01-23-45-67-89-AC", 27),
        (ADDRESS, 0),
        (ADDRESS, 31),
        (ADDRESS, 26),
    ] {
        let service = BluetoothService::FixedChannel(RfcommChannel::new(27)?);
        let mut harness = Harness::new([
            Step::Success {
                resolved: Resolved {
                    address: address.to_owned(),
                    rfcomm_channel: channel,
                },
                cancel: false,
                close_fails: false,
            },
            success(27),
        ])?;
        let selected = harness.run(service).await?;
        assert!(
            selected.opened.is_none() && !selected.history.succeeded(),
            "invalid endpoint is never admitted"
        );
        let error = selected
            .history
            .attempts
            .first()
            .and_then(|attempt| attempt.error.as_ref())
            .ok_or("error absent")?;
        assert!(
            !error.retry_allowed(),
            "local endpoint rejection cannot become a native retry"
        );
        assert_eq!(
            harness.events()?,
            [
                Seen::Open(ADDRESS.to_owned(), service),
                Seen::Close,
                Seen::Drop
            ],
            "invalid owner closes and drops without protocol traffic"
        );
        assert_eq!(
            harness.backend.steps.len(),
            1,
            "wrong endpoint is not retried"
        );
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_before_dispatch_or_during_wait_prevents_next_open() -> TestResult {
    let mut cancelled = Harness::new([success(27)])?;
    cancelled.backend.cancelled.store(true, Ordering::Relaxed);
    let selected = cancelled.run(BluetoothService::SerialPort).await?;
    assert!(
        selected.opened.is_none() && !selected.history.succeeded(),
        "pre-cancelled selection fails closed"
    );
    assert!(
        selected
            .history
            .attempts
            .iter()
            .all(|attempt| !attempt.started),
        "cancelled intent never reaches backend"
    );
    assert!(
        cancelled.events()?.is_empty(),
        "pre-cancellation opens no owner"
    );

    let mut waiting = Harness::new([eligible_failure(), success(27)])?;
    waiting.backend.cancel_on_wait = true;
    let selected = waiting.run(BluetoothService::SerialPort).await?;
    assert!(
        selected.opened.is_none() && !selected.history.succeeded(),
        "cancelled wait cannot produce success"
    );
    assert!(
        selected.history.retry_error.is_some(),
        "wait cancellation remains separate evidence"
    );
    assert_eq!(
        waiting.events()?,
        [
            Seen::Open(ADDRESS.to_owned(), BluetoothService::SerialPort),
            Seen::Wait(Duration::from_secs(1))
        ],
        "cancellation during wait prevents the second open"
    );
    assert_eq!(
        waiting.backend.steps.len(),
        1,
        "second fixture remains unopened"
    );
    Ok(())
}

#[tokio::test]
async fn late_cancelled_owner_is_closed_and_cleanup_failure_remains_separate() -> TestResult {
    let mut harness = Harness::new([Step::Success {
        resolved: Resolved {
            address: ADDRESS.to_owned(),
            rfcomm_channel: 27,
        },
        cancel: true,
        close_fails: true,
    }])?;
    let selected = harness.run(BluetoothService::SerialPort).await?;
    assert!(
        selected.opened.is_none() && !selected.history.succeeded(),
        "late owner cannot override cancellation"
    );
    let error = selected
        .history
        .attempts
        .first()
        .and_then(|attempt| attempt.error.as_ref())
        .ok_or("error absent")?;
    assert!(
        error.error.message.contains("cancelled"),
        "primary cancellation is retained"
    );
    assert!(
        error.close_error.is_some(),
        "cleanup failure remains independent"
    );
    assert_eq!(
        harness.events()?,
        [
            Seen::Open(ADDRESS.to_owned(), BluetoothService::SerialPort),
            Seen::Close,
            Seen::Drop
        ],
        "late owner is closed and dropped exactly once"
    );
    Ok(())
}

#[tokio::test]
async fn expired_returned_owner_is_closed_before_any_protocol_admission() -> TestResult {
    let mut harness = Harness::new([])?;
    let opened = Opened {
        connection: Connection {
            log: Arc::clone(&harness.backend.log),
            close_fails: false,
        },
        resolved: Resolved {
            address: ADDRESS.to_owned(),
            rfcomm_channel: 27,
        },
    };
    let (owner, failure) = admit(
        opened,
        harness.recorder.take().ok_or("recorder absent")?,
        &Endpoint {
            address: ADDRESS.parse()?,
            helper: None,
        },
        BluetoothService::SerialPort,
        &harness.backend.cancelled,
        Instant::now(),
    )
    .await;
    assert!(
        matches!(owner, Owner::Retired(_)),
        "expired owner is retired"
    );
    assert!(
        failure
            .as_ref()
            .is_some_and(|error| error.error.message.contains("deadline")),
        "deadline refusal is preserved"
    );
    assert_eq!(
        harness.events()?,
        [Seen::Close, Seen::Drop],
        "expiry allows cleanup only"
    );
    Ok(())
}

#[tokio::test]
async fn failed_capture_blocks_dispatch_and_is_reported_independently() -> TestResult {
    let mut harness = Harness::new([success(27)])?;
    let readonly = File::open(harness.temporary.path().join("opening.jsonl"))?;
    harness.recorder = Some(Recorder::named(
        readonly,
        Arc::clone(&harness.backend.cancelled),
        "opening.jsonl",
    ));
    let selected = harness.run(BluetoothService::SerialPort).await?;
    assert!(
        selected.opened.is_none() && !selected.history.succeeded(),
        "uncaptured intent blocks admission"
    );
    assert!(
        selected.history.capture_error.is_some(),
        "capture failure is independently retained"
    );
    assert!(
        !selected.history.transcript.complete,
        "capture remains incomplete"
    );
    assert!(
        selected
            .history
            .attempts
            .iter()
            .all(|attempt| !attempt.started),
        "capture failure blocks backend dispatch"
    );
    assert!(
        harness.events()?.is_empty(),
        "capture failure opens no owner"
    );
    Ok(())
}

#[tokio::test]
async fn failed_retry_capture_never_waits_or_dispatches() -> TestResult {
    let mut harness = Harness::new([])?;
    let readonly = File::open(harness.temporary.path().join("opening.jsonl"))?;
    let cancelled = Arc::clone(&harness.backend.cancelled);
    let mut recorder = Recorder::named(readonly, Arc::clone(&cancelled), "opening.jsonl");
    assert!(
        retry_wait(&mut harness.backend, &mut recorder, &cancelled, 2)
            .await
            .is_err(),
        "uncaptured retry intent is refused"
    );
    assert!(
        !recorder.summary().complete,
        "retry capture failure stays visible"
    );
    assert!(
        harness.events()?.is_empty(),
        "uncaptured retry never waits or opens"
    );
    Ok(())
}

#[tokio::test]
async fn failed_ready_capture_still_closes_and_drops_the_acquired_owner() -> TestResult {
    let harness = Harness::new([])?;
    let readonly = File::open(harness.temporary.path().join("opening.jsonl"))?;
    let recorder = Recorder::named(
        readonly,
        Arc::clone(&harness.backend.cancelled),
        "opening.jsonl",
    );
    let opened = Opened {
        connection: Connection {
            log: Arc::clone(&harness.backend.log),
            close_fails: false,
        },
        resolved: Resolved {
            address: ADDRESS.to_owned(),
            rfcomm_channel: 27,
        },
    };
    let mut recorder = recorder;
    recorder.record(Event::Completed {
        attempt: 1,
        resolved: &opened.resolved,
    });
    let (owner, failure) = admit(
        opened,
        recorder,
        &Endpoint {
            address: ADDRESS.parse()?,
            helper: None,
        },
        BluetoothService::SerialPort,
        &harness.backend.cancelled,
        Instant::now() + OPEN_BUDGET,
    )
    .await;
    let Owner::Retired(recorder) = owner else {
        return Err("uncaptured owner admitted".into());
    };
    assert!(
        !recorder.summary().complete,
        "readiness capture remains incomplete"
    );
    assert!(
        failure.is_some(),
        "readiness admission failure remains visible"
    );
    assert_eq!(
        harness.events()?,
        [Seen::Close, Seen::Drop],
        "capture failure never suppresses owner cleanup"
    );
    Ok(())
}

#[test]
fn eligible_native_error_returned_at_deadline_remains_visible_but_cannot_retry() -> TestResult {
    let mut evidence = Attempt {
        number: 1,
        started: true,
        resolved: None,
        error: None,
        interruption: None,
    };
    evidence.record_failure(
        OpenFailure::from_error(&TransportError::NotFound),
        &AtomicBool::new(false),
        Instant::now(),
    );
    let native = evidence.error.as_ref().ok_or("native error discarded")?;
    assert!(
        native.retry_allowed(),
        "preserve the original typed native failure"
    );
    assert!(
        evidence
            .interruption
            .as_ref()
            .is_some_and(|error| error.message.contains("deadline")),
        "outer deadline remains independent evidence"
    );
    assert!(
        !evidence.retry_allowed(),
        "late native failure cannot authorize another launch"
    );
    Ok(())
}
