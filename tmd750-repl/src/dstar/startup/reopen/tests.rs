//! Fake native openings verify deadline, cleanup evidence, and retry admission.

use std::collections::VecDeque;
use std::io;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use kenwood_transport::error::{BluetoothCloseFailure, BluetoothOpenStage};
use kenwood_transport::{Transport, TransportError};

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Log = Arc<Mutex<Vec<Seen>>>;
const ADDRESS: &str = "01-23-45-67-89-AB";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Open(String, BluetoothService, Instant),
    Wait(Duration),
    Close,
    Drop,
}

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
        Err(TransportError::Write(io::Error::other(
            "opening must not send protocol data",
        )))
    }

    async fn read(&mut self, _bytes: &mut [u8]) -> Result<usize, TransportError> {
        Err(TransportError::Read(io::Error::other(
            "opening must not read protocol data",
        )))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        record(&self.log, Seen::Close)?;
        if self.close_fails {
            Err(TransportError::Disconnected(io::Error::other(
                "scripted native close failure",
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
    Owner(Opened<Connection>),
    LateOwner(Opened<Connection>),
    CancelledOwner(Opened<Connection>),
    Error(OpenFailure),
    LateError(OpenFailure),
}

struct Native {
    steps: VecDeque<Step>,
    log: Log,
}

impl native::Backend for Native {
    type Connection = Connection;

    async fn open(
        &mut self,
        _endpoint: &Endpoint,
        _service: BluetoothService,
        _cancelled: &AtomicBool,
    ) -> Result<Opened<Connection>, OpenFailure> {
        Err(OpenFailure::from_error(&io::Error::other(
            "the bounded bridge must forward the lifecycle deadline",
        )))
    }

    async fn open_until(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<Opened<Connection>, OpenFailure> {
        record(
            &self.log,
            Seen::Open(endpoint.address.to_string(), service, deadline),
        )
        .map_err(|error| OpenFailure::from_error(&error))?;
        match self.steps.pop_front() {
            Some(Step::Owner(owner)) => Ok(owner),
            Some(Step::LateOwner(owner)) => {
                tokio::time::sleep_until(deadline).await;
                Ok(owner)
            }
            Some(Step::CancelledOwner(owner)) => {
                cancelled.store(true, Ordering::Release);
                Ok(owner)
            }
            Some(Step::Error(error)) => Err(error),
            Some(Step::LateError(error)) => {
                tokio::time::sleep_until(deadline).await;
                Err(error)
            }
            None => Err(OpenFailure::from_error(&io::Error::other(
                "no scripted native opening remains",
            ))),
        }
    }

    async fn wait(&mut self, duration: Duration) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Wait(duration));
        }
    }
}

struct Harness {
    directory: tempfile::TempDir,
    endpoint: Endpoint,
    cancelled: Arc<AtomicBool>,
    backend: Native,
    openings: Vec<opening::History>,
    retirements: Vec<control::Retirement>,
}

impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            directory: tempfile::tempdir()?,
            endpoint: Endpoint {
                address: ADDRESS.parse()?,
                helper: None,
            },
            cancelled: Arc::new(AtomicBool::new(false)),
            backend: Native {
                steps: VecDeque::new(),
                log: Log::default(),
            },
            openings: Vec::new(),
            retirements: Vec::new(),
        })
    }

    fn owner(&self, channel: u8, close_fails: bool) -> Opened<Connection> {
        Opened {
            connection: Connection {
                log: self.backend.log.clone(),
                close_fails,
            },
            resolved: native::Resolved {
                address: ADDRESS.to_owned(),
                rfcomm_channel: channel,
            },
        }
    }

    async fn run(
        &mut self,
        deadline: Instant,
    ) -> Result<CaptureTransport<Connection, File>, transition::ReopenFailure> {
        let cancelled = Arc::clone(&self.cancelled);
        let channel = RfcommChannel::new(19).map_err(|error| transition::ReopenFailure {
            error: Failure::from_error(&error),
            retry_allowed: false,
        })?;
        transition::Backend::reopen(
            &mut Reopen {
                backend: &mut self.backend,
                endpoint: &self.endpoint,
                channel,
                directory: self.directory.path(),
                cancelled: Arc::clone(&cancelled),
                openings: &mut self.openings,
                retirements: &mut self.retirements,
            },
            deadline,
            &cancelled,
        )
        .await
    }

    async fn retire(&mut self, owner: CaptureTransport<Connection, File>) -> TestResult {
        transition::Backend::retire(
            &mut Reopen {
                backend: &mut self.backend,
                endpoint: &self.endpoint,
                channel: RfcommChannel::new(19)?,
                directory: self.directory.path(),
                cancelled: Arc::clone(&self.cancelled),
                openings: &mut self.openings,
                retirements: &mut self.retirements,
            },
            owner,
        )
        .await
        .map_err(|error| error.to_string().into())
    }

    fn events(&self) -> Result<Vec<Seen>, Box<dyn std::error::Error>> {
        Ok(self
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .clone())
    }
}

fn readiness_failure() -> OpenFailure {
    OpenFailure::from_error(&TransportError::BluetoothOpen {
        stage: BluetoothOpenStage::RfcommDeadline,
    })
}

#[tokio::test]
async fn expired_window_refuses_native_dispatch() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run(Instant::now()).await;
    let Err(error) = result else {
        return Err("expired opening unexpectedly succeeded".into());
    };
    assert!(!error.retry_allowed);
    assert!(harness.events()?.is_empty());
    assert_eq!(harness.openings.len(), 1);
    Ok(())
}

#[tokio::test]
async fn late_error_retains_independent_cleanup_failure_in_captured_history() -> TestResult {
    let mut harness = Harness::new()?;
    let mut error = readiness_failure();
    error.error.causes.push("native helper context".to_owned());
    error.close_error = Some(Failure::from_error(&io::Error::other(
        "late owner close failed",
    )));
    harness.backend.steps.push_back(Step::LateError(error));
    let result = harness.run(Instant::now() + Duration::from_secs(2)).await;
    let Err(error) = result else {
        return Err("late failure unexpectedly succeeded".into());
    };
    assert!(!error.retry_allowed);
    let history = harness.openings.first().ok_or("missing opening history")?;
    let failure = history
        .attempts
        .first()
        .ok_or("missing attempt")?
        .error
        .as_ref()
        .ok_or("missing error")?;
    assert_eq!(
        failure
            .close_error
            .as_ref()
            .ok_or("lost independent close failure")?
            .message,
        "late owner close failed"
    );
    assert_eq!(failure.error.causes.len(), 2);
    assert_eq!(
        failure.error.causes.last().map(String::as_str),
        Some("native helper context")
    );
    assert!(history.transcript.complete);
    assert_eq!(harness.events()?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn late_success_closes_and_drops_before_returning_failure() -> TestResult {
    for close_fails in [false, true] {
        let mut harness = Harness::new()?;
        harness
            .backend
            .steps
            .push_back(Step::LateOwner(harness.owner(19, close_fails)));
        let deadline = Instant::now() + Duration::from_secs(2);
        let result = harness.run(deadline).await;
        let Err(error) = result else {
            return Err("late owner unexpectedly admitted".into());
        };
        assert!(!error.retry_allowed);
        let failure = harness
            .openings
            .first()
            .ok_or("missing history")?
            .attempts
            .first()
            .ok_or("missing attempt")?
            .error
            .as_ref()
            .ok_or("missing error")?;
        assert_eq!(failure.close_error.is_some(), close_fails);
        assert_eq!(failure.host_retirement_confirmed(), !close_fails);
        assert_eq!(
            harness.events()?,
            [
                Seen::Open(
                    ADDRESS.to_owned(),
                    BluetoothService::FixedChannel(RfcommChannel::new(19)?),
                    deadline
                ),
                Seen::Close,
                Seen::Drop
            ]
        );
    }
    Ok(())
}

#[tokio::test]
async fn window_expiry_retains_native_host_retirement_evidence() -> TestResult {
    for (source, confirmed) in [
        (
            TransportError::BluetoothOpenWithCleanup {
                stage: BluetoothOpenStage::RfcommDeadline,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            },
            true,
        ),
        (
            TransportError::BluetoothClose {
                failure: BluetoothCloseFailure::ReapPending,
            },
            false,
        ),
    ] {
        let mut harness = Harness::new()?;
        harness
            .backend
            .steps
            .push_back(Step::LateError(OpenFailure::from_error(&source)));
        let result = harness.run(Instant::now() + Duration::from_secs(2)).await;
        let error = result.err().ok_or("late native failure became an owner")?;
        assert!(!error.retry_allowed, "outer deadline still refuses retry");
        let history = harness.openings.first().ok_or("missing opening history")?;
        let failure = history
            .attempts
            .first()
            .and_then(|attempt| attempt.error.as_ref())
            .ok_or("missing opening failure")?;
        assert_eq!(failure.host_retirement_confirmed(), confirmed);
        assert_eq!(
            serde_json::to_value(history)?.pointer("/attempts/0/error/host_retirement_confirmed"),
            Some(&serde_json::Value::Bool(confirmed))
        );
        assert_eq!(harness.events()?.len(), 1, "no owner or radio bytes exist");
    }
    Ok(())
}

#[tokio::test]
async fn eligible_exhaustion_preserves_both_attempts_and_allows_transition_retry() -> TestResult {
    let mut harness = Harness::new()?;
    harness.backend.steps.extend([
        Step::Error(readiness_failure()),
        Step::Error(OpenFailure::from_error(
            &TransportError::BluetoothOpenWithCleanup {
                stage: BluetoothOpenStage::RfcommDeadline,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            },
        )),
    ]);
    let deadline = Instant::now() + Duration::from_secs(90);
    let result = harness.run(deadline).await;
    let Err(error) = result else {
        return Err("failed native attempts unexpectedly succeeded".into());
    };
    assert!(error.retry_allowed);
    let history = harness.openings.first().ok_or("missing history")?;
    assert_eq!(history.attempts.len(), 2);
    assert!(
        history
            .attempts
            .iter()
            .all(|attempt| attempt.error.is_some())
    );
    let opening = Seen::Open(
        ADDRESS.to_owned(),
        BluetoothService::FixedChannel(RfcommChannel::new(19)?),
        deadline,
    );
    assert_eq!(
        harness.events()?,
        [opening.clone(), Seen::Wait(Duration::from_secs(1)), opening]
    );
    let serialized = serde_json::to_value(history)?;
    assert_eq!(
        serialized
            .pointer("/attempts/1/error/retry_admission")
            .and_then(serde_json::Value::as_str),
        Some("native_opening")
    );
    Ok(())
}

#[tokio::test]
async fn fatal_native_failures_never_get_local_or_transition_retry() -> TestResult {
    let mut close_failure = readiness_failure();
    close_failure.close_error = Some(Failure::from_error(&io::Error::other(
        "cleanup unconfirmed",
    )));
    for error in [
        OpenFailure::from_error(&io::Error::other("helper framing failed")),
        OpenFailure::from_error(&TransportError::BluetoothOpen {
            stage: BluetoothOpenStage::StartupDeadline,
        }),
        OpenFailure::from_error(&TransportError::BluetoothOpen {
            stage: BluetoothOpenStage::ServiceResolution,
        }),
        OpenFailure::from_error(&TransportError::BluetoothClose {
            failure: BluetoothCloseFailure::ChannelUnconfirmed,
        }),
        close_failure,
    ] {
        let mut harness = Harness::new()?;
        harness.backend.steps.push_back(Step::Error(error));
        let result = harness.run(Instant::now() + Duration::from_secs(90)).await;
        let Err(error) = result else {
            return Err("fatal opening unexpectedly succeeded".into());
        };
        assert!(!error.retry_allowed);
        assert_eq!(harness.events()?.len(), 1);
        assert_eq!(
            harness
                .openings
                .first()
                .ok_or("missing history")?
                .attempts
                .len(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn wrong_channel_and_cancelled_owner_are_retired_without_retry() -> TestResult {
    for cancelled in [false, true] {
        let mut harness = Harness::new()?;
        let step = if cancelled {
            Step::CancelledOwner(harness.owner(19, false))
        } else {
            Step::Owner(harness.owner(20, false))
        };
        harness.backend.steps.push_back(step);
        let result = harness.run(Instant::now() + Duration::from_secs(90)).await;
        let Err(error) = result else {
            return Err("inadmissible owner unexpectedly accepted".into());
        };
        assert!(!error.retry_allowed);
        assert_eq!(harness.events()?.last(), Some(&Seen::Drop));
        assert_eq!(
            harness
                .events()?
                .iter()
                .filter(|event| matches!(event, Seen::Open(..)))
                .count(),
            1
        );
    }
    Ok(())
}

#[tokio::test]
async fn capture_creation_failure_never_opens_native_endpoint() -> TestResult {
    let mut harness = Harness::new()?;
    drop(create_private_file(
        &harness.directory.path().join("reopen-1.jsonl"),
    )?);
    let result = harness.run(Instant::now() + Duration::from_secs(90)).await;
    let Err(error) = result else {
        return Err("capture collision unexpectedly succeeded".into());
    };
    assert!(!error.retry_allowed);
    assert!(harness.events()?.is_empty());
    assert!(harness.openings.is_empty());
    Ok(())
}

#[tokio::test]
async fn recovered_native_open_keeps_the_failed_attempt_and_same_channel() -> TestResult {
    let mut harness = Harness::new()?;
    harness.backend.steps.extend([
        Step::Error(readiness_failure()),
        Step::Owner(harness.owner(19, false)),
    ]);
    let owner = harness
        .run(Instant::now() + Duration::from_secs(90))
        .await
        .map_err(|error| error.error.to_string())?;
    let history = harness.openings.first().ok_or("missing history")?;
    assert!(history.succeeded());
    assert_eq!(history.attempts.len(), 2);
    assert!(
        history
            .attempts
            .first()
            .ok_or("missing failed attempt")?
            .error
            .is_some()
    );
    let opening_events = serde_json::to_value(&history.transcript)?
        .get("events")
        .and_then(serde_json::Value::as_u64)
        .ok_or("opening event count missing")?;
    harness.retire(owner).await?;
    let retirement = harness.retirements.first().ok_or("missing retirement")?;
    assert!(retirement.succeeded());
    let final_events = serde_json::to_value(&retirement.transcript)?
        .get("events")
        .and_then(serde_json::Value::as_u64)
        .ok_or("final event count missing")?;
    assert!(final_events > opening_events);
    assert_eq!(harness.events()?.last(), Some(&Seen::Drop));
    Ok(())
}

#[tokio::test]
async fn capture_retirement_failure_is_retained_after_attempting_native_close() -> TestResult {
    let mut harness = Harness::new()?;
    let path = harness.directory.path().join("read-only-capture.jsonl");
    drop(create_private_file(&path)?);
    let recorder = Recorder::named(
        File::open(path)?,
        Arc::clone(&harness.cancelled),
        "read-only-retirement",
    );
    let owner = CaptureTransport::required(harness.owner(19, false).connection, recorder);
    let result = harness.retire(owner).await;
    assert!(result.is_err());
    assert_eq!(harness.retirements.len(), 1);
    let retirement = harness.retirements.first().ok_or("missing retirement")?;
    assert!(!retirement.succeeded());
    assert!(retirement.capture_error.is_some());
    assert!(!retirement.transcript.complete);
    assert!(harness.cancelled.load(Ordering::Acquire));
    assert_eq!(harness.events()?, [Seen::Close, Seen::Drop]);
    Ok(())
}

#[tokio::test]
async fn native_close_failure_retains_complete_final_capture_separately() -> TestResult {
    let mut harness = Harness::new()?;
    harness
        .backend
        .steps
        .push_back(Step::Owner(harness.owner(19, true)));
    let owner = harness
        .run(Instant::now() + Duration::from_secs(90))
        .await
        .map_err(|error| error.error.to_string())?;
    let result = harness.retire(owner).await;
    assert!(result.is_err());
    assert_eq!(harness.retirements.len(), 1);
    let retirement = harness.retirements.first().ok_or("missing retirement")?;
    assert!(!retirement.succeeded());
    assert!(retirement.close_error.is_some());
    assert!(retirement.capture_error.is_none());
    assert!(retirement.transcript.complete);
    assert_eq!(harness.events()?.last(), Some(&Seen::Drop));
    Ok(())
}
