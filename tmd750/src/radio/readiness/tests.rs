//! Readiness and observation over scripted connections: which attempts are
//! followed by another open, and how each connection is closed.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use kenwood_transport::MockTransport;

use super::*;
use crate::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};
use crate::types::{FirmwareIdentity, RadioModel, RadioType};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Enumerate,
    Open(usize),
    Write(usize, Vec<u8>),
    Close(usize),
    Drop(usize),
    Wait(Duration),
}

#[derive(Debug, Default)]
struct Shared {
    seen: Mutex<Vec<Seen>>,
    elapsed: Mutex<Duration>,
}

impl Shared {
    fn record(&self, event: Seen) -> io::Result<()> {
        self.seen
            .lock()
            .map_err(|_| io::Error::other("event log poisoned"))?
            .push(event);
        Ok(())
    }

    fn advance(&self, duration: Duration) -> io::Result<()> {
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
    cancel_on_write: Option<(Vec<u8>, Arc<AtomicBool>)>,
    cancel_on_close: Option<Arc<AtomicBool>>,
    close_cost: Duration,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.shared
            .record(Seen::Write(self.ordinal, bytes.to_vec()))
            .map_err(TransportError::Write)?;
        match self.fault {
            Fault::Write => return Err(TransportError::Write(io::Error::other("write failed"))),
            Fault::WriteTimeout => return std::future::pending().await,
            _ => {}
        }
        self.script.write(bytes).await?;
        if let Some((trigger, cancelled)) = &self.cancel_on_write
            && (trigger.is_empty() || bytes == trigger.as_slice())
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        match self.fault {
            Fault::Read => Err(TransportError::Read(io::Error::other("read failed"))),
            Fault::InvalidReadCount => Ok(bytes.len().saturating_add(1)),
            _ => self.script.read(bytes).await,
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.shared
            .record(Seen::Close(self.ordinal))
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
        let _result = self.shared.record(Seen::Drop(self.ordinal));
    }
}

struct FakeHost {
    connections: VecDeque<Connection>,
    shared: Arc<Shared>,
    snapshots: VecDeque<Vec<SerialCandidate>>,
    snapshot: Vec<SerialCandidate>,
    opens: usize,
    enumerations: usize,
    enumeration_cost: Duration,
    open_cost: Duration,
    open_error: Option<io::ErrorKind>,
    waits: usize,
    cancel_on_wait: Option<(usize, Arc<AtomicBool>)>,
    stages: Vec<ControlStage>,
}

impl FakeHost {
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
            enumerations: 0,
            enumeration_cost: Duration::ZERO,
            open_cost: Duration::ZERO,
            open_error: None,
            waits: 0,
            cancel_on_wait: None,
            stages: Vec::new(),
        }
    }

    fn first(&mut self) -> Result<&mut Connection, io::Error> {
        self.connections
            .front_mut()
            .ok_or_else(|| io::Error::other("no pending connection"))
    }

    fn seen(&self) -> Result<Vec<String>, io::Error> {
        Ok(self
            .shared
            .seen
            .lock()
            .map_err(|_| io::Error::other("event log poisoned"))?
            .iter()
            .map(|event| format!("{event:?}"))
            .collect())
    }

    /// Assert the exact writes per opened handle, that each handle closed
    /// before it dropped, and that each later open followed an enumeration and
    /// the retry wait.
    fn assert_wire(&self, expected: &[&[&[u8]]]) -> TestResult {
        let observed = self.shared.seen.lock().map_err(|_| "event log poisoned")?;
        assert_eq!(self.opens, expected.len(), "exact fresh-open count");
        for (ordinal, writes) in expected.iter().enumerate() {
            let actual: Vec<&[u8]> = observed
                .iter()
                .filter_map(|event| match event {
                    Seen::Write(connection, bytes) if *connection == ordinal => {
                        Some(bytes.as_slice())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(actual, *writes, "exact command scope for handle {ordinal}");
            let close = observed
                .iter()
                .position(|event| *event == Seen::Close(ordinal))
                .ok_or("missing close")?;
            let dropped = observed
                .iter()
                .position(|event| *event == Seen::Drop(ordinal))
                .ok_or("missing drop")?;
            assert!(close < dropped, "close precedes handle drop");
            if let Some(next) = observed
                .iter()
                .position(|event| *event == Seen::Open(ordinal + 1))
            {
                assert!(dropped < next, "old handle drops before the next open");
                let between = observed.get(dropped + 1..next).ok_or("event interval")?;
                assert!(
                    between.contains(&Seen::Enumerate),
                    "each later open requires a new enumeration"
                );
                assert!(
                    between.contains(&Seen::Wait(RETRY_INTERVAL)),
                    "each retry is paced after release"
                );
            }
        }
        drop(observed);
        Ok(())
    }
}

impl ControlHost for FakeHost {
    type Connection = Connection;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        self.shared
            .record(Seen::Enumerate)
            .and_then(|()| self.shared.advance(self.enumeration_cost))
            .map_err(TransportError::Read)?;
        self.enumerations += 1;
        if let Some(snapshot) = self.snapshots.pop_front() {
            self.snapshot = snapshot;
        }
        Ok(self.snapshot.clone())
    }

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        assert_eq!(selected, &endpoint(), "only the pinned endpoint may open");
        assert_eq!(baud, 9600, "preserve the selected CAT baud");
        self.shared
            .record(Seen::Open(self.opens))
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

    fn now(&self) -> Duration {
        self.shared
            .elapsed
            .lock()
            .map_or(Duration::ZERO, |elapsed| *elapsed)
    }

    async fn wait(&mut self, duration: Duration) {
        let result = self
            .shared
            .record(Seen::Wait(duration))
            .and_then(|()| self.shared.advance(duration));
        assert!(result.is_ok(), "record simulated wait: {result:?}");
        self.waits += 1;
        if let Some((ordinal, cancelled)) = &self.cancel_on_wait
            && *ordinal == self.waits
        {
            cancelled.store(true, Ordering::Relaxed);
        }
    }

    fn stage(&mut self, stage: ControlStage) {
        self.stages.push(stage);
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.readiness-panel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

fn identity() -> Result<Identity, Box<dyn std::error::Error>> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

fn identity_script(firmware: &[u8], radio_type: &[u8]) -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", firmware);
    script.expect(b"TY\r", radio_type);
    script
}

fn ready_script() -> MockTransport {
    identity_script(b"FV 1.02\r", b"TY K,2,1\r")
}

fn gateway_script(reply: &[u8]) -> MockTransport {
    let mut script = ready_script();
    script.expect(b"GW\r", reply);
    script
}

fn silent_script() -> MockTransport {
    let mut script = MockTransport::new();
    script.expect_hang(b"ID\r");
    script
}

async fn readiness(
    host: &mut FakeHost,
    cancelled: &AtomicBool,
) -> Result<ReadinessReport, Box<dyn std::error::Error>> {
    let expected = identity()?;
    Ok(verify_readiness(host, &endpoint(), 9600, &expected, cancelled).await)
}

const IDENTITY_WRITES: &[&[u8]] = &[b"ID\r", b"FV\r", b"TY\r"];
const GATEWAY_WRITES: &[&[u8]] = &[b"ID\r", b"FV\r", b"TY\r", b"GW\r"];

#[tokio::test]
async fn silent_id_then_matching_identity_retries_only_after_release_and_reselection() -> TestResult
{
    let mut host = FakeHost::new([silent_script(), ready_script()]);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert!(report.succeeded(), "{report:?}");
    assert!(report.released());
    host.assert_wire(&[&[b"ID\r"], IDENTITY_WRITES])?;
    assert_eq!(report.attempts.len(), 2, "both attempts stay in the report");
    let first = report.attempts.first().ok_or("first attempt")?;
    assert!(!first.enumerations.is_empty());
    assert!(matches!(
        first.outcome,
        ReadinessOutcome::Failed(ReadinessError::Cat(Error::Timeout { .. }))
    ));
    assert_eq!(first.retry, RetryAdmission::SilentIdentityTimeout);
    assert!(
        first
            .connection
            .as_ref()
            .is_some_and(|connection| connection.identity.is_none() && connection.released())
    );
    let last = report.attempts.last().ok_or("last attempt")?;
    assert!(last.outcome.is_matched());
    assert_eq!(last.retry, RetryAdmission::Terminal);
    assert_eq!(
        last.connection
            .as_ref()
            .and_then(|connection| connection.identity.as_ref()),
        Some(&identity()?)
    );
    Ok(())
}

#[tokio::test]
async fn repeated_silence_stops_at_the_open_attempt_cap() -> TestResult {
    let mut host = FakeHost::new((0..5).map(|_| silent_script()));
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::AttemptCapExhausted);
    assert!(!report.succeeded());
    host.assert_wire(&[&[b"ID\r"], &[b"ID\r"], &[b"ID\r"], &[b"ID\r"]])?;
    assert_eq!(report.attempts.len(), MAXIMUM_OPEN_ATTEMPTS);
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
        (radio_type, IDENTITY_WRITES),
    ];
    for (script, commands) in cases {
        let mut host = FakeHost::new([script, ready_script()]);
        let report = readiness(&mut host, &AtomicBool::new(false)).await?;
        assert_eq!(report.ending, ReadinessEnding::Failed);
        host.assert_wire(&[commands])?;
    }
    Ok(())
}

#[tokio::test]
async fn wrong_or_malformed_identity_and_eof_never_retry() -> TestResult {
    for response in [b"ID TH-D75\r".as_slice(), b"ID unknown\r", b"?\r", b"\r"] {
        let mut script = MockTransport::new();
        script.expect(b"ID\r", response);
        let mut host = FakeHost::new([script, ready_script()]);
        let report = readiness(&mut host, &AtomicBool::new(false)).await?;
        assert_eq!(report.ending, ReadinessEnding::Failed, "{response:?}");
        host.assert_wire(&[&[b"ID\r"]])?;
    }
    let mut script = MockTransport::new();
    script.expect_eof(b"ID\r");
    let mut host = FakeHost::new([script, ready_script()]);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    host.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn complete_but_different_firmware_never_retries() -> TestResult {
    let mut host = FakeHost::new([identity_script(b"FV 1.03\r", b"TY K,2,1\r"), ready_script()]);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert!(matches!(
        report.attempts.first().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(
            ReadinessError::IdentityMismatch { .. }
        ))
    ));
    host.assert_wire(&[IDENTITY_WRITES])?;
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
        let mut host = FakeHost::new([silent_script(), ready_script()]);
        host.first()?.fault = fault;
        let report = tokio::time::timeout(
            Duration::from_secs(10),
            readiness(&mut host, &AtomicBool::new(false)),
        )
        .await??;
        assert!(!report.succeeded(), "{fault:?} must remain terminal");
        host.assert_wire(&[&[b"ID\r"]])?;
        if matches!(fault, Fault::Close | Fault::CloseTimeout) {
            assert!(
                !report.released(),
                "{fault:?} leaves the handle unconfirmed"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn open_failure_is_terminal_without_protocol_traffic() -> TestResult {
    let mut host = FakeHost::new([ready_script()]);
    host.open_error = Some(io::ErrorKind::PermissionDenied);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert_eq!(host.opens, 1, "never retry a failed open");
    assert!(matches!(
        report.attempts.first().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(ReadinessError::Open(_)))
    ));
    assert!(report.released(), "a failed open holds nothing");
    let seen = host.seen()?;
    assert!(
        !seen
            .iter()
            .any(|event| event.starts_with("Write") || event.starts_with("Close")),
        "a failed open produces neither CAT nor a close: {seen:?}"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_during_silent_id_or_close_prevents_another_attempt() -> TestResult {
    for during_write in [true, false] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut host = FakeHost::new([silent_script(), ready_script()]);
        if during_write {
            host.first()?.cancel_on_write = Some((Vec::new(), Arc::clone(&cancelled)));
        } else {
            host.first()?.cancel_on_close = Some(Arc::clone(&cancelled));
        }
        let report = readiness(&mut host, &cancelled).await?;
        assert!(!report.succeeded());
        host.assert_wire(&[&[b"ID\r"]])?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_identity_finishes_the_tuple_and_close() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut host = FakeHost::new([ready_script(), ready_script()]);
    host.first()?.cancel_on_write = Some((Vec::new(), Arc::clone(&cancelled)));
    let report = readiness(&mut host, &cancelled).await?;
    assert_eq!(report.ending, ReadinessEnding::Cancelled);
    host.assert_wire(&[IDENTITY_WRITES])?;
    Ok(())
}

#[tokio::test]
async fn cancellation_before_work_or_during_settle_prevents_open() -> TestResult {
    for initially_cancelled in [true, false] {
        let cancelled = Arc::new(AtomicBool::new(initially_cancelled));
        let mut host = FakeHost::new([ready_script()]);
        host.cancel_on_wait = Some((1, Arc::clone(&cancelled)));
        let report = readiness(&mut host, &cancelled).await?;
        assert_eq!(report.ending, ReadinessEnding::Cancelled);
        host.assert_wire(&[])?;
    }
    Ok(())
}

#[tokio::test]
async fn cancellation_during_retry_wait_prevents_another_open() -> TestResult {
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut host = FakeHost::new([silent_script(), ready_script()]);
    host.cancel_on_wait = Some((2, Arc::clone(&cancelled)));
    let report = readiness(&mut host, &cancelled).await?;
    assert_eq!(report.ending, ReadinessEnding::Cancelled);
    host.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn changed_endpoint_metadata_after_silence_prevents_reopen() -> TestResult {
    let mut changed = endpoint();
    changed.pid = Some(TMD750_MAIN_PID);
    let mut host = FakeHost::new([silent_script(), ready_script()]);
    host.snapshots = VecDeque::from([vec![endpoint()], vec![changed]]);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert!(matches!(
        report.attempts.last().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(ReadinessError::Reenumeration(
            ReenumerationRejection::SelectedEndpointChanged { .. }
        )))
    ));
    host.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn absent_endpoint_is_polled_then_reported_after_the_budget() -> TestResult {
    let mut host = FakeHost::new([ready_script()]);
    host.snapshots = VecDeque::from([Vec::new(), Vec::new(), vec![endpoint()]]);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert!(report.succeeded());
    let attempt = report.attempts.first().ok_or("attempt")?;
    assert_eq!(attempt.enumerations.len(), 3);
    assert!(
        host.seen()?
            .contains(&format!("{:?}", Seen::Wait(ENUMERATION_INTERVAL)))
    );

    let mut host = FakeHost::new([ready_script()]);
    host.snapshot = Vec::new();
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert!(matches!(
        report.attempts.first().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(
            ReadinessError::EndpointAbsent { .. }
        ))
    ));
    host.assert_wire(&[])?;
    Ok(())
}

#[tokio::test]
async fn insufficient_identity_and_close_budget_prevents_open() -> TestResult {
    let mut host = FakeHost::new([ready_script()]);
    host.enumeration_cost = Duration::from_secs(50);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert!(matches!(
        report.attempts.first().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(ReadinessError::BudgetExhausted))
    ));
    host.assert_wire(&[])?;
    Ok(())
}

#[tokio::test]
async fn deadline_expiry_during_open_still_closes_without_cat() -> TestResult {
    let mut host = FakeHost::new([ready_script()]);
    host.open_cost = Duration::from_secs(61);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    host.assert_wire(&[&[]])?;
    Ok(())
}

#[tokio::test]
async fn attempt_and_cleanup_time_consume_the_shared_retry_budget() -> TestResult {
    let mut host = FakeHost::new([silent_script(), ready_script()]);
    host.first()?.close_cost = Duration::from_secs(49);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::BudgetExhausted);
    host.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn observation_reads_identity_then_gateway_on_one_closed_connection() -> TestResult {
    let mut host = FakeHost::new([gateway_script(b"GW 0\r")]);
    let expected = identity()?;
    let report = observe_control(
        &mut host,
        &endpoint(),
        9600,
        Expectation {
            identity: Some(&expected),
            gateway: Some(DvGatewayMode::Off),
        },
        &AtomicBool::new(false),
    )
    .await;
    assert!(report.succeeded(), "{report:?}");
    assert_eq!(report.observed(), Some((&expected, DvGatewayMode::Off)));
    assert_eq!(host.enumerations, 0, "an observation never enumerates");
    assert_eq!(host.waits, 0, "an observation never waits");
    host.assert_wire(&[GATEWAY_WRITES])?;
    Ok(())
}

#[tokio::test]
async fn observation_without_expectations_records_whatever_it_reads() -> TestResult {
    let mut host = FakeHost::new([gateway_script(b"GW 2\r")]);
    let report = observe_control(
        &mut host,
        &endpoint(),
        9600,
        Expectation::default(),
        &AtomicBool::new(false),
    )
    .await;
    assert!(report.succeeded());
    assert_eq!(
        report.observed().map(|(_, gateway)| gateway),
        Some(DvGatewayMode::Terminal)
    );
    Ok(())
}

#[tokio::test]
async fn other_gateway_states_are_recorded_but_never_accepted_as_the_required_one() -> TestResult {
    for (reply, mode) in [
        (b"GW 2\r".as_slice(), DvGatewayMode::Terminal),
        (b"GW 1\r", DvGatewayMode::Unqualified(1)),
        (b"GW 255\r", DvGatewayMode::Unqualified(255)),
    ] {
        let mut host = FakeHost::new([gateway_script(reply)]);
        let report = observe_control(
            &mut host,
            &endpoint(),
            9600,
            Expectation {
                identity: None,
                gateway: Some(DvGatewayMode::Off),
            },
            &AtomicBool::new(false),
        )
        .await;
        assert!(matches!(
            report.outcome,
            ReadinessOutcome::Failed(ReadinessError::GatewayMismatch {
                required: DvGatewayMode::Off,
                actual
            }) if actual == mode
        ));
        assert_eq!(
            report
                .connection
                .as_ref()
                .and_then(|connection| connection.gateway),
            Some(mode)
        );
        assert!(report.observed().is_none());
        host.assert_wire(&[GATEWAY_WRITES])?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_gateway_replies_close_without_retry() -> TestResult {
    for reply in [b"GW invalid\r".as_slice(), b"N\r", b"MD 0,0\r"] {
        let mut host = FakeHost::new([gateway_script(reply)]);
        let report = observe_control(
            &mut host,
            &endpoint(),
            9600,
            Expectation::default(),
            &AtomicBool::new(false),
        )
        .await;
        assert!(matches!(
            report.outcome,
            ReadinessOutcome::Failed(ReadinessError::Cat(_))
        ));
        assert!(
            report
                .connection
                .as_ref()
                .is_some_and(|connection| connection.gateway.is_none() && connection.released())
        );
        host.assert_wire(&[GATEWAY_WRITES])?;
    }
    let mut script = ready_script();
    script.expect_hang(b"GW\r");
    let mut host = FakeHost::new([script]);
    let report = observe_control(
        &mut host,
        &endpoint(),
        9600,
        Expectation::default(),
        &AtomicBool::new(false),
    )
    .await;
    assert!(matches!(
        report.outcome,
        ReadinessOutcome::Failed(ReadinessError::Cat(Error::Timeout { .. }))
    ));
    host.assert_wire(&[GATEWAY_WRITES])?;
    Ok(())
}

#[tokio::test]
async fn identity_mismatch_or_failure_never_sends_the_gateway_query() -> TestResult {
    let expected = identity()?;
    for (firmware, radio_type) in [
        (b"FV 1.03\r".as_slice(), b"TY K,2,1\r".as_slice()),
        (b"FV 1.02\r", b"TY J,2,1\r"),
    ] {
        let mut host = FakeHost::new([identity_script(firmware, radio_type)]);
        let report = observe_control(
            &mut host,
            &endpoint(),
            9600,
            Expectation {
                identity: Some(&expected),
                gateway: Some(DvGatewayMode::Off),
            },
            &AtomicBool::new(false),
        )
        .await;
        assert!(matches!(
            report.outcome,
            ReadinessOutcome::Failed(ReadinessError::IdentityMismatch { .. })
        ));
        host.assert_wire(&[IDENTITY_WRITES])?;
    }
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID OTHER\r");
    let mut host = FakeHost::new([script]);
    let report = observe_control(
        &mut host,
        &endpoint(),
        9600,
        Expectation::default(),
        &AtomicBool::new(false),
    )
    .await;
    assert!(matches!(
        report.outcome,
        ReadinessOutcome::Failed(ReadinessError::Cat(_))
    ));
    host.assert_wire(&[&[b"ID\r"]])?;
    Ok(())
}

#[tokio::test]
async fn observation_cancellation_stops_at_the_next_query_and_still_closes() -> TestResult {
    let mut unopened = FakeHost::new([MockTransport::new()]);
    let report = observe_control(
        &mut unopened,
        &endpoint(),
        9600,
        Expectation::default(),
        &AtomicBool::new(true),
    )
    .await;
    assert!(matches!(report.outcome, ReadinessOutcome::Cancelled));
    assert!(report.connection.is_none());
    assert!(report.released());
    unopened.assert_wire(&[])?;
    for (trigger, writes) in [
        (b"TY\r".as_slice(), IDENTITY_WRITES),
        (b"GW\r", GATEWAY_WRITES),
    ] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut host = FakeHost::new([gateway_script(b"GW 0\r")]);
        host.first()?.cancel_on_write = Some((trigger.to_vec(), Arc::clone(&cancelled)));
        let report = observe_control(
            &mut host,
            &endpoint(),
            9600,
            Expectation::default(),
            &cancelled,
        )
        .await;
        assert!(
            matches!(report.outcome, ReadinessOutcome::Cancelled),
            "{trigger:?}"
        );
        assert!(report.observed().is_none());
        assert!(report.released());
        host.assert_wire(&[writes])?;
    }
    Ok(())
}

#[tokio::test]
async fn a_failed_close_replaces_a_match_but_never_an_earlier_failure() -> TestResult {
    for (reply, mismatch) in [(b"GW 0\r".as_slice(), false), (b"GW 2\r", true)] {
        let mut host = FakeHost::new([gateway_script(reply)]);
        host.first()?.fault = Fault::Close;
        let report = observe_control(
            &mut host,
            &endpoint(),
            9600,
            Expectation {
                identity: None,
                gateway: Some(DvGatewayMode::Off),
            },
            &AtomicBool::new(false),
        )
        .await;
        if mismatch {
            assert!(matches!(
                report.outcome,
                ReadinessOutcome::Failed(ReadinessError::GatewayMismatch { .. })
            ));
        } else {
            assert!(matches!(
                report.outcome,
                ReadinessOutcome::Failed(ReadinessError::Close)
            ));
        }
        assert!(!report.released());
        assert!(matches!(
            report
                .connection
                .as_ref()
                .and_then(|connection| connection.close.as_ref()),
            Some(Err(CloseFailure::Transport(_)))
        ));
        host.assert_wire(&[GATEWAY_WRITES])?;
    }
    Ok(())
}

#[tokio::test]
async fn an_ambiguous_endpoint_is_rejected_before_any_open() -> TestResult {
    let mut host = FakeHost::new([ready_script()]);
    let mut other = endpoint();
    other.path = "/dev/cu.other-panel".to_owned();
    host.snapshot.push(other);
    let report = readiness(&mut host, &AtomicBool::new(false)).await?;
    assert_eq!(report.ending, ReadinessEnding::Failed);
    assert!(matches!(
        report.attempts.first().map(|attempt| &attempt.outcome),
        Some(ReadinessOutcome::Failed(ReadinessError::Reenumeration(
            ReenumerationRejection::AmbiguousEndpoints { .. }
        )))
    ));
    host.assert_wire(&[])?;
    Ok(())
}

#[tokio::test]
async fn the_bounded_close_helper_reports_a_timeout_and_drops_the_connection() -> TestResult {
    let shared = Arc::new(Shared::default());
    let connection = Connection {
        ordinal: 7,
        script: MockTransport::new(),
        shared: Arc::clone(&shared),
        fault: Fault::CloseTimeout,
        cancel_on_write: None,
        cancel_on_close: None,
        close_cost: Duration::ZERO,
    };
    let result = close_within(connection, Duration::from_millis(20)).await;
    assert!(
        matches!(result, Err(CloseFailure::Timeout { budget }) if budget == Duration::from_millis(20))
    );
    let seen = shared
        .seen
        .lock()
        .map_err(|_| "event log poisoned")?
        .iter()
        .map(|event| format!("{event:?}"))
        .collect::<Vec<_>>();
    assert_eq!(seen, ["Close(7)", "Drop(7)"]);
    Ok(())
}

#[test]
fn the_attempt_allowance_covers_three_exchanges_and_the_close() {
    assert_eq!(
        ATTEMPT_ALLOWANCE,
        Duration::from_millis(1_500 * 6) + Duration::from_secs(2)
    );
    assert!(can_dispatch(Duration::ZERO, ATTEMPT_ALLOWANCE));
    assert!(!can_dispatch(Duration::from_millis(1), ATTEMPT_ALLOWANCE));
}
