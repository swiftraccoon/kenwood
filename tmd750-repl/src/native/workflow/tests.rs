//! Exact native workflow traffic and owner retirement, without Bluetooth I/O.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::{Address, Page};
use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::native::{OpenFailure, Opened};

type TestResult = AppResult<()>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Seen {
    Open(usize, String),
    Service(usize, BluetoothService),
    Write(usize, Vec<u8>),
    Read(usize, Vec<u8>),
    Close(usize),
    Drop(usize),
    Wait(Duration),
}

type Log = Arc<Mutex<Vec<Seen>>>;

fn record(log: &Log, event: Seen) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

struct Fixture {
    script: MockTransport,
    close_fails: bool,
    cancel_on_close: bool,
    address: Option<String>,
    channel: u8,
    open_error: Option<TransportError>,
}

impl Fixture {
    fn new(script: MockTransport) -> Self {
        Self {
            script,
            close_fails: false,
            cancel_on_close: false,
            address: None,
            channel: 27,
            open_error: None,
        }
    }

    fn failed(error: TransportError) -> Self {
        Self {
            open_error: Some(error),
            ..Self::new(MockTransport::new())
        }
    }
}

struct Connection {
    id: usize,
    fixture: Fixture,
    log: Log,
    cancelled: Arc<AtomicBool>,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        record(&self.log, Seen::Write(self.id, bytes.to_vec()))?;
        self.fixture.script.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.fixture.script.read(bytes).await?;
        record(
            &self.log,
            Seen::Read(self.id, bytes.get(..count).unwrap_or_default().to_vec()),
        )?;
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        record(&self.log, Seen::Close(self.id))?;
        self.fixture.script.assert_complete();
        if self.fixture.cancel_on_close {
            self.cancelled.store(true, Ordering::Relaxed);
        }
        if self.fixture.close_fails {
            Err(TransportError::Disconnected(io::Error::other(
                "scripted close failure",
            )))
        } else {
            self.fixture.script.close().await
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

struct FakeBackend {
    fixtures: VecDeque<Fixture>,
    log: Log,
    cancelled: Arc<AtomicBool>,
    opens: usize,
    cancel_during_wait: bool,
}

impl Backend for FakeBackend {
    type Connection = Connection;

    async fn open(
        &mut self,
        endpoint: &Endpoint,
        service: BluetoothService,
        _cancelled: &AtomicBool,
    ) -> Result<Opened<Connection>, OpenFailure> {
        let id = self.opens;
        self.opens += 1;
        record(&self.log, Seen::Open(id, endpoint.address.to_string()))
            .map_err(|error| OpenFailure::from_error(&error))?;
        record(&self.log, Seen::Service(id, service))
            .map_err(|error| OpenFailure::from_error(&error))?;
        let mut fixture = self
            .fixtures
            .pop_front()
            .ok_or_else(|| OpenFailure::from_error(&io::Error::other("scripted open failure")))?;
        if let Some(error) = fixture.open_error.take() {
            return Err(OpenFailure::from_error(&error));
        }
        let resolved = Resolved {
            address: fixture
                .address
                .clone()
                .unwrap_or_else(|| endpoint.address.to_string()),
            rfcomm_channel: fixture.channel,
        };
        Ok(Opened {
            connection: Connection {
                id,
                fixture,
                log: Arc::clone(&self.log),
                cancelled: Arc::clone(&self.cancelled),
            },
            resolved,
        })
    }

    async fn wait(&mut self, duration: Duration) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Seen::Wait(duration));
        }
        if self.cancel_during_wait {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn identity(firmware: &[u8]) -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", firmware);
    script.expect(b"TY\r", b"TY K,2,1\r");
    script
}

fn fixed(exit_reply: &[u8]) -> AppResult<MockTransport> {
    let mut script = identity(b"FV 1.02\r");
    script.expect(b"GW\r", b"GW 0\r");
    script.expect(b"0M PROGRAM\r", b"0M\r");
    for page in [
        Page::new(Address::new(8)?, 40)?,
        Page::new(Address::new(327_681)?, 255)?,
    ] {
        let mut response = write_request(page).to_vec();
        response.extend(vec![0x42; page.len()]);
        script.expect(&read_request(page), &response);
        script.expect(&[ACK], &[ACK]);
    }
    script.expect(b"E", exit_reply);
    Ok(script)
}

fn fresh_gateway(reply: &[u8]) -> MockTransport {
    let mut script = identity(b"FV 1.02\r");
    script.expect(b"GW\r", reply);
    script
}

fn endpoint() -> AppResult<Endpoint> {
    Ok(Endpoint {
        address: "01:23:45:67:89:AB".parse()?,
        helper: None,
    })
}

async fn execute(
    fixtures: impl IntoIterator<Item = Fixture>,
    operation: Operation,
) -> AppResult<(WorkflowResult, Vec<Seen>)> {
    execute_with_faults(fixtures, operation, Faults::default()).await
}

#[derive(Default)]
struct Faults {
    cancel_during_wait: bool,
    readonly_settle: bool,
}

async fn execute_with_faults(
    fixtures: impl IntoIterator<Item = Fixture>,
    operation: Operation,
    faults: Faults,
) -> AppResult<(WorkflowResult, Vec<Seen>)> {
    let temporary = tempfile::tempdir()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let artifacts = Artifacts::create(
        CaptureKind::NativeBluetooth,
        Some(&temporary.path().join("native capture")),
        Arc::clone(&cancelled),
    )?;
    let fresh = if faults.readonly_settle {
        Recorder::named(
            File::open(artifacts.directory.join("report.json"))?,
            Arc::clone(&cancelled),
            "post-exit-transcript.jsonl",
        )
    } else {
        artifacts.reserve_post_exit(Arc::clone(&cancelled))?
    };
    let mut backend = FakeBackend {
        fixtures: fixtures.into_iter().collect(),
        log: Arc::clone(&log),
        cancelled: Arc::clone(&cancelled),
        opens: 0,
        cancel_during_wait: faults.cancel_during_wait,
    };
    let result = run_workflow(
        &mut backend,
        &endpoint()?,
        operation,
        artifacts.transcript,
        fresh,
        &cancelled,
    )
    .await;
    assert!(
        backend.fixtures.is_empty(),
        "not all expected opens occurred"
    );
    let events = log.lock().map_err(|_| "test log poisoned")?.clone();
    Ok((result, events))
}

fn writes(events: &[Seen], id: usize) -> Vec<Vec<u8>> {
    events
        .iter()
        .filter_map(|event| match event {
            Seen::Write(owner, bytes) if *owner == id => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

fn opens(events: &[Seen]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Seen::Open(..)))
        .count()
}

fn position(events: &[Seen], expected: &Seen) -> AppResult<usize> {
    events
        .iter()
        .position(|event| event == expected)
        .ok_or_else(|| format!("missing observation {expected:?}").into())
}

#[tokio::test]
async fn status_has_exact_read_only_schedule_and_retires_its_owner() -> TestResult {
    let mut script = identity(b"FV 1.02\r");
    script.expect(b"MD 0\r", b"MD 0,0\r");
    script.expect(b"MD 1\r", b"MD 1,1\r");
    script.expect(b"GW\r", b"GW 0\r");
    let (result, events) =
        execute([Fixture::new(script)], Operation::Cat(CatScope::Status)).await?;
    assert!(result.succeeded(), "{result:?}");
    assert_eq!(opens(&events), 1);
    assert_eq!(
        writes(&events, 0),
        [
            b"ID\r".as_slice(),
            b"FV\r",
            b"TY\r",
            b"MD 0\r",
            b"MD 1\r",
            b"GW\r"
        ]
    );
    assert!(position(&events, &Seen::Close(0))? < position(&events, &Seen::Drop(0))?);
    assert!(result.fresh_endpoint.is_none());
    Ok(())
}

#[tokio::test]
async fn fixed_probe_retains_owner_during_settle_before_fresh_gateway_check() -> TestResult {
    let (mut result, events) = execute(
        [
            Fixture::new(fixed(&[ACK])?),
            Fixture::new(fresh_gateway(b"GW 0\r")),
        ],
        Operation::FixedMcp,
    )
    .await?;
    assert!(result.succeeded(), "{result:?}");
    assert_eq!(opens(&events), 2);
    assert_eq!(
        writes(&events, 0),
        [
            b"ID\r".as_slice(),
            b"FV\r",
            b"TY\r",
            b"GW\r",
            b"0M PROGRAM\r",
            &[b'R', 0, 0, 8, 40],
            &[ACK],
            &[b'R', 5, 0, 1, 255],
            &[ACK],
            b"E"
        ]
    );
    assert_eq!(
        writes(&events, 1),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"]
    );
    let exit = position(&events, &Seen::Write(0, b"E".to_vec()))?;
    assert_eq!(events.get(exit + 1), Some(&Seen::Read(0, vec![ACK])));
    let close = position(&events, &Seen::Close(0))?;
    let drop = position(&events, &Seen::Drop(0))?;
    let settle = position(&events, &Seen::Wait(Duration::from_secs(5)))?;
    let fresh = position(&events, &Seen::Open(1, endpoint()?.address.to_string()))?;
    assert!(exit < settle && settle < close && close < drop && drop < fresh);
    assert!(position(&events, &Seen::Close(1))? < position(&events, &Seen::Drop(1))?);
    if let Some(Observation::FixedMcp { transcript, .. }) = &mut result.original {
        transcript.complete = false;
    } else {
        return Err("fixed evidence absent".into());
    }
    assert!(
        !result.succeeded(),
        "fresh CAT must not hide an incomplete original capture"
    );
    Ok(())
}

#[tokio::test]
async fn failed_exit_never_opens_a_fresh_connection() -> TestResult {
    let (result, events) = execute([Fixture::new(fixed(&[0x15])?)], Operation::FixedMcp).await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 1);
    assert!(result.fresh_cat.is_none());
    assert!(position(&events, &Seen::Close(0))? < position(&events, &Seen::Drop(0))?);
    Ok(())
}

#[tokio::test]
async fn failed_original_close_blocks_fresh_traffic() -> TestResult {
    let mut fixture = Fixture::new(fixed(&[ACK])?);
    fixture.close_fails = true;
    let (result, events) = execute([fixture], Operation::FixedMcp).await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 1);
    assert!(matches!(
        result.original,
        Some(Observation::FixedMcp {
            close_error: Some(_),
            ..
        })
    ));
    assert!(result.fresh_cat.is_none());
    Ok(())
}

#[tokio::test]
async fn cancellation_during_original_close_blocks_fresh_traffic() -> TestResult {
    let mut fixture = Fixture::new(fixed(&[ACK])?);
    fixture.cancel_on_close = true;
    let (result, events) = execute([fixture], Operation::FixedMcp).await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 1);
    assert!(result.fresh_cat.is_none());
    Ok(())
}

#[tokio::test]
async fn wrong_resolved_address_is_closed_without_cat() -> TestResult {
    let mut fixture = Fixture::new(MockTransport::new());
    fixture.address = Some("01-23-45-67-89-AC".to_owned());
    let (result, events) = execute([fixture], Operation::Cat(CatScope::Identity)).await?;
    assert!(!result.succeeded());
    assert!(
        result
            .original_opening
            .is_some_and(|history| !history.succeeded())
    );
    assert!(writes(&events, 0).is_empty());
    assert_eq!(opens(&events), 1);
    assert!(position(&events, &Seen::Close(0))? < position(&events, &Seen::Drop(0))?);
    Ok(())
}

#[tokio::test]
async fn changed_fresh_identity_is_retained_as_failure_without_retry() -> TestResult {
    let (result, events) = execute(
        [
            Fixture::new(fixed(&[ACK])?),
            Fixture::new(identity(b"FV 1.03\r")),
        ],
        Operation::FixedMcp,
    )
    .await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 2);
    let fresh = result.fresh_cat.ok_or("fresh CAT evidence absent")?;
    assert!(fresh.operation_error.is_some());
    assert_eq!(
        fresh
            .identity
            .ok_or("fresh tuple absent")?
            .0
            .firmware
            .as_str(),
        "1.03"
    );
    assert!(position(&events, &Seen::Close(1))? < position(&events, &Seen::Drop(1))?);
    Ok(())
}

#[tokio::test]
async fn failed_fresh_open_is_not_retried_and_preserves_original_evidence() -> TestResult {
    let (result, events) = execute([Fixture::new(fixed(&[ACK])?)], Operation::FixedMcp).await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 2);
    assert!(
        result
            .fresh_opening
            .is_some_and(|history| !history.succeeded())
    );
    assert!(result.original.is_some());
    assert!(result.fresh_cat.is_none());
    Ok(())
}

#[tokio::test]
async fn active_gateway_blocks_native_programming_entry() -> TestResult {
    let (result, events) = execute(
        [Fixture::new(fresh_gateway(b"GW 2\r"))],
        Operation::FixedMcp,
    )
    .await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 1);
    assert_eq!(
        writes(&events, 0),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"]
    );
    assert!(result.fresh_cat.is_none());
    Ok(())
}

#[tokio::test]
async fn changed_fresh_gateway_is_failure_without_an_extra_connection() -> TestResult {
    let (result, events) = execute(
        [
            Fixture::new(fixed(&[ACK])?),
            Fixture::new(fresh_gateway(b"GW 2\r")),
        ],
        Operation::FixedMcp,
    )
    .await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 2);
    let fresh = result.fresh_cat.ok_or("fresh CAT evidence absent")?;
    assert_eq!(fresh.gateway, Some(2));
    assert!(fresh.operation_error.is_some());
    assert!(position(&events, &Seen::Close(1))? < position(&events, &Seen::Drop(1))?);
    Ok(())
}

#[tokio::test]
async fn unsynchronized_open_intent_prevents_backend_access() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let log = Arc::new(Mutex::new(Vec::new()));
    let artifacts = Artifacts::create(
        CaptureKind::NativeBluetooth,
        Some(&temporary.path().join("unwritable")),
        Arc::clone(&cancelled),
    )?;
    let readonly = File::open(artifacts.directory.join("report.json"))?;
    let fresh = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
    let recorder = Recorder::named(readonly, Arc::clone(&cancelled), "transcript.jsonl");
    let mut backend = FakeBackend {
        fixtures: VecDeque::new(),
        log,
        cancelled: Arc::clone(&cancelled),
        opens: 0,
        cancel_during_wait: false,
    };
    let result = run_workflow(
        &mut backend,
        &endpoint()?,
        Operation::FixedMcp,
        recorder,
        fresh,
        &cancelled,
    )
    .await;
    assert!(!result.succeeded());
    assert_eq!(backend.opens, 0);
    assert!(
        result
            .original_opening
            .is_some_and(|history| !history.succeeded())
    );
    Ok(())
}

#[tokio::test]
async fn recovery_pins_the_channel_from_the_original_successful_open() -> TestResult {
    let mut original = Fixture::new(fixed(&[ACK])?);
    original.channel = 19;
    let mut fresh = Fixture::new(fresh_gateway(b"GW 0\r"));
    fresh.channel = 19;
    let (result, events) = execute([original, fresh], Operation::FixedMcp).await?;
    assert!(result.succeeded(), "{result:?}");
    assert!(events.contains(&Seen::Service(0, BluetoothService::SerialPort)));
    assert!(events.contains(&Seen::Service(
        1,
        BluetoothService::FixedChannel(kenwood_transport::bluetooth::RfcommChannel::new(19)?)
    )));
    Ok(())
}

#[tokio::test]
async fn eligible_open_retry_never_repeats_mcp_or_forgets_the_first_failure() -> TestResult {
    use kenwood_transport::error::{BluetoothCloseFailure, BluetoothOpenStage};

    let (result, events) = execute(
        [
            Fixture::new(fixed(&[ACK])?),
            Fixture::failed(TransportError::BluetoothOpenWithCleanup {
                stage: BluetoothOpenStage::RfcommDeadline,
                cleanup: BluetoothCloseFailure::ChannelUnconfirmed,
            }),
            Fixture::new(fresh_gateway(b"GW 0\r")),
        ],
        Operation::FixedMcp,
    )
    .await?;
    assert!(result.succeeded(), "{result:?}");
    assert_eq!(opens(&events), 3);
    assert_eq!(writes(&events, 0).len(), 10);
    assert!(writes(&events, 1).is_empty());
    assert_eq!(
        writes(&events, 2),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"]
    );
    let retry_wait = position(&events, &Seen::Wait(Duration::from_secs(1)))?;
    assert!(position(&events, &Seen::Open(1, endpoint()?.address.to_string()))? < retry_wait);
    assert!(retry_wait < position(&events, &Seen::Open(2, endpoint()?.address.to_string()))?);
    let history = result.fresh_opening.ok_or("missing opening history")?;
    assert_eq!(history.attempts.len(), 2);
    let first_error = history
        .attempts
        .first()
        .ok_or("missing first attempt")?
        .error
        .as_ref()
        .ok_or("lost first failure")?;
    assert!(first_error.error.message.contains("RFCOMM"));
    assert!(first_error.error.message.contains("ChannelUnconfirmed"));
    assert!(
        history
            .attempts
            .get(1)
            .ok_or("missing second attempt")?
            .error
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn exhausted_open_retries_leave_the_completed_mcp_evidence_intact() -> TestResult {
    let (result, events) = execute(
        [
            Fixture::new(fixed(&[ACK])?),
            Fixture::failed(TransportError::NotFound),
            Fixture::failed(TransportError::NotFound),
        ],
        Operation::FixedMcp,
    )
    .await?;
    assert!(!result.succeeded());
    assert_eq!(opens(&events), 3);
    assert_eq!(writes(&events, 0).len(), 10);
    assert!(result.fresh_cat.is_none());
    let history = result.fresh_opening.ok_or("missing failed attempts")?;
    assert_eq!(history.attempts.len(), 2);
    assert!(
        history
            .attempts
            .iter()
            .all(|attempt| attempt.error.is_some())
    );
    assert!(matches!(
        result.original,
        Some(Observation::FixedMcp {
            close_error: None,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn cancellation_during_settle_still_retires_the_original_owner() -> TestResult {
    let (result, events) = execute_with_faults(
        [Fixture::new(fixed(&[ACK])?)],
        Operation::FixedMcp,
        Faults {
            cancel_during_wait: true,
            ..Faults::default()
        },
    )
    .await?;
    assert!(!result.succeeded());
    assert!(result.settle_error.is_some());
    assert_eq!(opens(&events), 1);
    assert!(
        position(&events, &Seen::Wait(Duration::from_secs(5)))?
            < position(&events, &Seen::Close(0))?
    );
    assert!(position(&events, &Seen::Close(0))? < position(&events, &Seen::Drop(0))?);
    Ok(())
}

#[tokio::test]
async fn failed_settle_capture_still_retires_the_original_owner_without_reopening() -> TestResult {
    let (result, events) = execute_with_faults(
        [Fixture::new(fixed(&[ACK])?)],
        Operation::FixedMcp,
        Faults {
            readonly_settle: true,
            ..Faults::default()
        },
    )
    .await?;
    assert!(!result.succeeded());
    assert!(result.settle_error.is_some());
    assert!(
        result
            .settle_transcript
            .is_some_and(|summary| !summary.complete)
    );
    assert_eq!(opens(&events), 1);
    assert!(!events.iter().any(|event| matches!(event, Seen::Wait(_))));
    assert!(position(&events, &Seen::Close(0))? < position(&events, &Seen::Drop(0))?);
    Ok(())
}
