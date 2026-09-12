//! External Terminal-exit lifecycle and durable evidence using only mock handles.

mod observations;
mod open_lifecycle;

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::memory::{TerminalExitTrial, TerminalExitTrialStatus};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{
    KENWOOD_VID, MockTransport, SerialCandidate, TMD750_MAIN_PID, Transport, TransportError,
};
use kenwood_tmd750::{Address, FirmwareIdentity, Identity, Page, RadioModel, RadioType};

use super::super::journal::Journal;
use super::{SessionCaptures, WorkflowResult, run};
use crate::mcp::capture::{Artifacts, Recorder};
use crate::mcp::reconnect::Backend;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open(usize),
    Write(usize, Vec<u8>),
    Close(usize),
    Dropped(usize),
    Enumerate,
}

type Log = Arc<Mutex<Vec<Event>>>;

#[derive(serde::Deserialize)]
struct JournalRecord {
    sequence: usize,
    kind: String,
    evidence: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct IntentRecord {
    session_id: u8,
    intent_id: u8,
    memory_format: u8,
    pre_entry_gateway: u8,
    desired_gateway: u8,
    status: String,
    scope: ScopeRecord,
}

#[derive(serde::Deserialize)]
struct ScopeRecord {
    trial_kind: String,
    field: String,
    identity: IdentityRecord,
    off_page: PageRecord,
    expected_active_page: PageRecord,
    control_page: PageRecord,
    routing_page: PageRecord,
}

#[derive(serde::Deserialize)]
struct IdentityRecord {
    model: String,
    firmware: String,
    radio_type: String,
}

#[derive(serde::Deserialize)]
struct PageRecord {
    address: u32,
    length: usize,
    data: Vec<u8>,
}

fn records(path: &Path) -> Result<Vec<JournalRecord>, TestError> {
    let content = std::fs::read_to_string(path)?;
    assert!(
        content.ends_with('\n'),
        "every retained journal record must be complete"
    );
    let records = content
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<JournalRecord>, _>>()?;
    for (sequence, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence, sequence,
            "journal sequence numbers must remain contiguous"
        );
    }
    Ok(records)
}

#[derive(Debug)]
struct JournalProof {
    path: PathBuf,
    off: [u8; 256],
    active: [u8; 256],
    control: [u8; 256],
    routing: [u8; 256],
}

impl JournalProof {
    fn verify_next_session(&self) -> TestResult {
        let records = records(&self.path)?;
        assert_eq!(
            records.len(),
            3,
            "the first durable session record must precede Verify open"
        );
        let evidence = records.last().ok_or("first-session evidence missing")?;
        assert_eq!(
            evidence.kind, "session_evidence",
            "Verify requires completed first-session evidence"
        );
        let details = evidence
            .evidence
            .get("details")
            .ok_or("session details missing")?;
        for path in ["/transcript/complete", "/post_exit/transcript/complete"] {
            assert_eq!(
                details.pointer(path).and_then(serde_json::Value::as_bool),
                Some(true),
                "both required transcripts must be complete before Verify"
            );
        }
        for field in ["open_error", "close_error", "synchronization_error"] {
            assert_eq!(
                details.get(field),
                Some(&serde_json::Value::Null),
                "recorded lifecycle errors prohibit Verify"
            );
        }
        assert_eq!(
            details
                .pointer("/post_exit/outcome/status")
                .and_then(serde_json::Value::as_str),
            Some("matched"),
            "fresh CAT Off must already have succeeded"
        );
        let directory = self.path.parent().ok_or("journal parent missing")?;
        for filename in ["transcript.jsonl", "post-exit-transcript.jsonl"] {
            let content = std::fs::read_to_string(directory.join("session-0").join(filename))?;
            assert!(
                content.ends_with('\n'),
                "both transcript files must end in complete records before Verify"
            );
            let events = content
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<Vec<serde_json::Value>, _>>()?;
            assert_eq!(
                events
                    .last()
                    .and_then(|event| event.pointer("/event/kind"))
                    .and_then(serde_json::Value::as_str),
                Some("close_completed"),
                "both captured closes must exist on disk before Verify"
            );
        }
        Ok(())
    }

    fn verify_dispatch(&self, connection: usize, bytes: &[u8]) -> TestResult {
        assert_eq!(connection, 0, "only the Apply MCP handle may write memory");
        let records = records(&self.path)?;
        let intents: Vec<_> = records
            .iter()
            .filter(|record| record.kind == "write_intent")
            .collect();
        assert_eq!(
            intents.len(),
            1,
            "exactly one complete intent must exist before the sole W"
        );
        let intent: IntentRecord =
            serde_json::from_value(intents.first().ok_or("intent missing")?.evidence.clone())?;
        assert_eq!(
            (intent.session_id, intent.intent_id),
            (1, 1),
            "the only intent belongs to the Apply session"
        );
        assert_eq!(
            (
                intent.memory_format,
                intent.pre_entry_gateway,
                intent.desired_gateway
            ),
            (0, 2, 0),
            "the intent must pin format and the Terminal-to-Off transition"
        );
        assert_eq!(
            intent.status, "possibly_changed",
            "the journal records possible change before dispatch"
        );
        let scope = intent.scope;
        assert_eq!(
            scope.trial_kind, "terminal_to_off_trial",
            "the operation kind must be bound"
        );
        assert_eq!(
            scope.field, "dv.DvGatewayModeDvGateway",
            "the only writable setting is Gateway mode"
        );
        assert_eq!(
            (
                scope.identity.model.as_str(),
                scope.identity.firmware.as_str(),
                scope.identity.radio_type.as_str()
            ),
            ("TM-D750", "1.02", "K,2,1"),
            "the full identity must precede W"
        );
        for (observed, address, expected) in [
            (&scope.off_page, 331_776, &self.off),
            (&scope.expected_active_page, 331_776, &self.active),
            (&scope.control_page, 323_584, &self.control),
            (&scope.routing_page, 328_960, &self.routing),
        ] {
            assert_eq!(
                (observed.address, observed.length),
                (address, 256),
                "every exact canonical page must be journaled"
            );
            assert_eq!(
                observed.data.as_slice(),
                expected.as_slice(),
                "complete immutable page bytes must precede W"
            );
        }
        assert_eq!(
            bytes.len(),
            261,
            "the write must be a single complete header/data frame"
        );
        assert_eq!(
            bytes.get(..5),
            Some(b"W\x05\x10\x00\x00".as_slice()),
            "only the fixed target may be written"
        );
        assert_eq!(
            bytes.get(5..),
            Some(self.off.as_slice()),
            "the sole payload is the exact captured Off page"
        );
        Ok(())
    }
}

fn append(log: &Log, event: Event) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

#[derive(Debug)]
struct Connection {
    id: usize,
    mock: MockTransport,
    log: Log,
    journal: Arc<JournalProof>,
    fail_close: bool,
    entry_read_error: Option<io::Error>,
    cancel_on_write: Option<Arc<AtomicBool>>,
    move_directory_on_close: Option<(PathBuf, PathBuf)>,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.mock.writes().is_empty() {
            open_lifecycle::verify_before_first_write(self, bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        append(&self.log, Event::Write(self.id, bytes.to_vec()))?;
        if bytes.first() == Some(&b'W') {
            self.journal
                .verify_dispatch(self.id, bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        self.mock.write(bytes).await?;
        if bytes.first() == Some(&b'W')
            && let Some(cancelled) = &self.cancel_on_write
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        if self
            .mock
            .writes()
            .last()
            .is_some_and(|bytes| bytes == b"0M PROGRAM\r")
            && let Some(error) = self.entry_read_error.take()
        {
            return Err(TransportError::Read(error));
        }
        self.mock.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        append(&self.log, Event::Close(self.id))?;
        self.mock.assert_complete();
        if self.fail_close {
            return Err(TransportError::Disconnected(io::Error::other(
                "scripted close failure",
            )));
        }
        self.mock.close().await?;
        if let Some((original, moved)) = &self.move_directory_on_close {
            std::fs::rename(original, moved).map_err(TransportError::Disconnected)?;
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut events) = self.log.lock() {
            events.push(Event::Dropped(self.id));
        }
    }
}

#[derive(Debug)]
struct TestBackend {
    connections: VecDeque<Connection>,
    log: Log,
    opens: usize,
    open_failure_at: Option<usize>,
    elapsed: Duration,
}

impl Backend for TestBackend {
    type Connection = Connection;

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: u32,
    ) -> Result<Connection, TransportError> {
        assert_eq!(
            selected,
            &endpoint(),
            "only the exact pinned endpoint may open"
        );
        assert_eq!(baud, 9600, "the transport setting must remain fixed");
        if let Some(connection) = self.connections.front() {
            open_lifecycle::verify_before_open(connection).map_err(|error| {
                TransportError::Open {
                    path: selected.path.clone(),
                    source: io::Error::other(error),
                }
            })?;
        }
        if self.opens == 2 {
            self.connections
                .front()
                .ok_or_else(|| TransportError::Open {
                    path: selected.path.clone(),
                    source: io::Error::other("Verify connection missing"),
                })?
                .journal
                .verify_next_session()
                .map_err(|error| TransportError::Open {
                    path: selected.path.clone(),
                    source: io::Error::other(error),
                })?;
        }
        append(&self.log, Event::Open(self.opens))?;
        let fail_open = self.open_failure_at == Some(self.opens);
        self.opens += 1;
        if fail_open {
            return Err(TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("scripted original open failure"),
            });
        }
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected extra connection"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        append(&self.log, Event::Enumerate)?;
        Ok(vec![endpoint()])
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.elapsed = self.elapsed.saturating_add(duration);
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.fixed-terminal-exit-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn trial() -> Result<TerminalExitTrial, TestError> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut off = [0xA5; 256];
    *off.first_mut().ok_or("Gateway byte missing")? = 0;
    *off.get_mut(2).ok_or("subtype byte missing")? = 0;
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM selector missing")? = 0;
    let mut routing = [0x3C; 256];
    *routing.get_mut(71).ok_or("USB function missing")? = 0;
    *routing.get_mut(77).ok_or("Gateway route missing")? = 1;
    Ok(TerminalExitTrial::prepare_unqualified_offline(
        &identity, &off, &control, &routing,
    )?)
}

fn identity_script(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

fn fresh_script(gateway: u8) -> MockTransport {
    let mut mock = identity_script("1.02");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    mock
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut result = write_request(page).to_vec();
    result.extend_from_slice(bytes);
    result
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scenario {
    Success,
    GuardDrift,
    BadWriteAck,
    JournalRefusal,
}

fn mcp_script(
    trial: &TerminalExitTrial,
    phase: usize,
    scenario: Scenario,
) -> Result<MockTransport, TestError> {
    let mut mock = fresh_script(if phase == 0 { 2 } else { 0 });
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    read(&mut mock, trial.control_page_spec(), trial.control_page());
    let mut routing = *trial.routing_page();
    if scenario == Scenario::GuardDrift {
        *routing
            .get_mut(200)
            .ok_or("unrelated routing byte missing")? ^= 1;
    }
    read(&mut mock, trial.routing_page_spec(), &routing);
    read(
        &mut mock,
        trial.page(),
        if phase == 0 {
            trial.expected_active_page()
        } else {
            trial.off_page()
        },
    );
    if phase == 0 && !matches!(scenario, Scenario::GuardDrift | Scenario::JournalRefusal) {
        mock.expect(
            &frame(trial.page(), trial.off_page()),
            if scenario == Scenario::BadWriteAck {
                &[0x15]
            } else {
                &[ACK]
            },
        );
        if scenario == Scenario::BadWriteAck {
            return Ok(mock);
        }
        read(&mut mock, trial.page(), trial.off_page());
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

struct Harness {
    directory: tempfile::TempDir,
    trial: TerminalExitTrial,
    backend: TestBackend,
    journal: Journal,
    captures: Option<[SessionCaptures; 2]>,
    cancelled: Arc<AtomicBool>,
}

impl Harness {
    fn new() -> Result<Self, TestError> {
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let trial = trial()?;
        let failed = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let proof = Arc::new(JournalProof {
            path: directory.path().join("terminal-exit-journal.jsonl"),
            off: *trial.off_page(),
            active: *trial.expected_active_page(),
            control: *trial.control_page(),
            routing: *trial.routing_page(),
        });
        let mut connections = VecDeque::new();
        let mut captures = Vec::new();
        for phase in 0..2 {
            for mock in [
                mcp_script(&trial, phase, Scenario::Success)?,
                fresh_script(0),
            ] {
                connections.push_back(Connection {
                    id: connections.len(),
                    mock,
                    log: Arc::clone(&log),
                    journal: Arc::clone(&proof),
                    fail_close: false,
                    entry_read_error: None,
                    cancel_on_write: None,
                    move_directory_on_close: None,
                });
            }
            let artifacts = Artifacts::create(
                Some(&directory.path().join(format!("session-{phase}"))),
                Arc::clone(&failed),
            )?;
            let post_exit = artifacts.reserve_post_exit(Arc::clone(&failed))?;
            captures.push(SessionCaptures {
                original: artifacts.transcript,
                post_exit,
            });
        }
        let mut journal = Journal::create(directory.path())?;
        journal.prepare(&trial, &directory.path().join("source-backup.json"))?;
        let captures = captures
            .try_into()
            .map_err(|_| "exactly two capture pairs are required")?;
        Ok(Self {
            directory,
            trial,
            backend: TestBackend {
                connections,
                log,
                opens: 0,
                open_failure_at: None,
                elapsed: Duration::ZERO,
            },
            journal,
            captures: Some(captures),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn run(&mut self) -> Result<WorkflowResult, TestError> {
        Ok(run(
            &mut self.backend,
            &endpoint(),
            9600,
            &mut self.trial,
            &mut self.journal,
            self.captures.take().ok_or("captures already consumed")?,
            &self.cancelled,
        )
        .await)
    }

    fn connection(&mut self, index: usize) -> Result<&mut Connection, TestError> {
        self.backend
            .connections
            .get_mut(index)
            .ok_or_else(|| "connection missing".into())
    }

    fn events(&self) -> Result<Vec<Event>, TestError> {
        Ok(self
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .clone())
    }

    fn journal_path(&self) -> PathBuf {
        self.directory.path().join("terminal-exit-journal.jsonl")
    }
}

fn writes(events: &[Event], id: usize) -> Vec<&[u8]> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Write(connection, bytes) if *connection == id => Some(bytes.as_slice()),
            _ => None,
        })
        .collect()
}

fn position(events: &[Event], value: &Event) -> Result<usize, TestError> {
    events
        .iter()
        .position(|event| event == value)
        .ok_or_else(|| format!("missing {value:?}").into())
}

fn memory_writes(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'W')))
        .count()
}

#[tokio::test]
async fn four_handle_success_has_one_bound_intent_and_exact_close_drop_order() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(&harness.trial),
        "all required external evidence must succeed: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::OffVerifiedAcrossSessions,
        "only both finalized sessions establish Off across sessions"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "exactly two MCP and two fresh CAT handles are required"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        1,
        "only the fixed Apply write may occur"
    );
    for id in 0..4 {
        assert!(
            position(&events, &Event::Close(id))? < position(&events, &Event::Dropped(id))?,
            "every handle must close before it is dropped"
        );
        if id < 3 {
            assert!(
                position(&events, &Event::Dropped(id))? < position(&events, &Event::Open(id + 1))?,
                "the old handle must be gone before another opens"
            );
        }
        let observed = writes(&events, id);
        if id % 2 == 0 {
            assert_eq!(
                observed.last(),
                Some(&b"E".as_slice()),
                "E must be the last command on each MCP handle"
            );
        } else {
            assert_eq!(
                observed,
                [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
                "each fresh handle must check identity and Gateway Off exactly once"
            );
        }
    }
    harness.journal.finish(&harness.trial)?;
    let records = records(&harness.journal_path())?;
    assert_eq!(
        records
            .iter()
            .map(|record| record.kind.as_str())
            .collect::<Vec<_>>(),
        [
            "prepared",
            "write_intent",
            "session_evidence",
            "session_evidence",
            "finished"
        ],
        "the durable journal must retain one intent and both completed lifecycles"
    );
    let last = records.last().ok_or("finished record missing")?;
    assert_eq!(
        last.evidence
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("off_verified_across_sessions"),
        "durable final status must match the engine"
    );
    Ok(())
}

#[tokio::test]
async fn fresh_terminal_or_unknown_gateway_stops_before_another_mcp_session() -> TestResult {
    for raw in [1, 2, 3, u8::MAX] {
        let mut harness = Harness::new()?;
        harness.connection(1)?.mock = fresh_script(raw);
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.trial),
            "fresh non-Off Gateway must reject finalization: {result:?}"
        );
        assert_eq!(
            harness.backend.opens, 2,
            "a wrong post-exit Gateway state must prevent the next MCP handle"
        );
        assert_eq!(
            harness.trial.status(),
            TerminalExitTrialStatus::PossiblyChanged,
            "readback alone does not establish live Gateway Off"
        );
        let events = harness.events()?;
        assert!(
            events.contains(&Event::Close(1)) && events.contains(&Event::Dropped(1)),
            "the mismatching fresh handle must still close and drop"
        );
        assert_eq!(
            memory_writes(&events),
            1,
            "a non-Off observation cannot trigger another write"
        );
        harness.journal.finish(&harness.trial)?;
    }
    Ok(())
}

#[tokio::test]
async fn fresh_identity_mismatch_prevents_gateway_query_and_next_mcp_session() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(1)?.mock = identity_script("1.03");
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "identity mismatch cannot finalize an exit: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "there is no retry after fresh identity mismatch"
    );
    let events = harness.events()?;
    assert_eq!(
        writes(&events, 1),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r"],
        "Gateway must not be queried through a mismatching fresh identity"
    );
    assert!(
        events.contains(&Event::Close(1)) && events.contains(&Event::Dropped(1)),
        "the mismatching fresh handle must be released"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "the existing write remains unresolved"
    );
    Ok(())
}

#[tokio::test]
async fn original_close_failure_prevents_all_reconnect_work() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.fail_close = true;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "failed original close cannot count as success: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "a failed old close must prevent every fresh handle"
    );
    let events = harness.events()?;
    assert!(
        !events.contains(&Event::Enumerate),
        "do not attempt reconnect after failed original release"
    );
    assert!(
        events.contains(&Event::Dropped(0)),
        "the failed original handle must still be dropped"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "failed close cannot clear the write obligation"
    );
    Ok(())
}

#[tokio::test]
async fn fresh_close_failure_prevents_the_next_mcp_session() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(1)?.fail_close = true;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "matching CAT without a successful fresh close is insufficient: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "failed fresh close must prevent the next MCP session"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "fresh close failure leaves the lifecycle incomplete"
    );
    Ok(())
}

#[tokio::test]
async fn unknown_write_ack_sends_no_exit_and_opens_no_fresh_handle() -> TestResult {
    let mut harness = Harness::new()?;
    let script = mcp_script(&harness.trial, 0, Scenario::BadWriteAck)?;
    harness.connection(0)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "uncertain write cannot qualify Gateway Off: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "uncertain framing prohibits all reconnect work"
    );
    let events = harness.events()?;
    assert!(
        !writes(&events, 0).contains(&b"E".as_slice()),
        "no speculative E may follow uncertain W"
    );
    assert!(
        !events.contains(&Event::Enumerate),
        "no reconnect may follow uncertain framing"
    );
    assert!(
        events.contains(&Event::Close(0)) && events.contains(&Event::Dropped(0)),
        "the uncertain handle must still be released"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "uncertain dispatch cannot erase possible change"
    );
    Ok(())
}

#[tokio::test]
async fn changed_routing_guard_cannot_reach_intent_or_write() -> TestResult {
    let mut harness = Harness::new()?;
    let script = mcp_script(&harness.trial, 0, Scenario::GuardDrift)?;
    harness.connection(0)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "unrelated routing drift must reject Apply: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::NotWritten,
        "guard drift cannot accept intent"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        0,
        "complete guard mismatch prohibits W"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "known-boundary cleanup permits only fresh CAT, never Verify MCP"
    );
    let records = records(&harness.journal_path())?;
    assert!(
        records.iter().all(|record| record.kind != "write_intent"),
        "no intent may be journaled after a rejected guard"
    );
    Ok(())
}

#[tokio::test]
async fn verify_entry_disconnect_preserves_prior_off_evidence_without_retry() -> TestResult {
    let mut harness = Harness::new()?;
    let mut script = fresh_script(0);
    script.expect(b"0M PROGRAM\r", b"");
    let connection = harness.connection(2)?;
    connection.mock = script;
    connection.entry_read_error = Some(io::Error::from_raw_os_error(6));
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "entry failure must remain incomplete"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "failed independent verification cannot discharge the prior write"
    );
    assert_eq!(
        harness.backend.opens, 3,
        "no post-failure connection may open"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        1,
        "the prior write must not be repeated"
    );
    assert_eq!(
        writes(&events, 2),
        [
            b"ID\r".as_slice(),
            b"FV\r",
            b"TY\r",
            b"GW\r",
            b"0M PROGRAM\r"
        ],
        "failed entry permits no page, exit, CAT, or retry command"
    );
    let closed = position(&events, &Event::Close(2))?;
    assert_eq!(
        events.get(closed..),
        Some([Event::Close(2), Event::Dropped(2)].as_slice()),
        "the final handle must only close and drop after failure"
    );
    let opened = position(&events, &Event::Open(2))?;
    assert!(
        !events
            .get(opened..)
            .ok_or("Verify events missing")?
            .contains(&Event::Enumerate),
        "uncertain entry must not start reconnect observation"
    );
    let first = result.sessions.first().ok_or("Apply evidence missing")?;
    assert!(
        first.succeeded(),
        "the completed first session must remain recorded"
    );
    let second = result.sessions.get(1).ok_or("Verify evidence missing")?;
    assert_failed_entry_evidence(second)?;
    harness.journal.finish(&harness.trial)?;
    assert_failed_entry_journal(&harness.journal_path())
}

fn assert_failed_entry_evidence(second: &super::SessionEvidence) -> TestResult {
    let core = second.core.as_ref().ok_or("Verify core evidence missing")?;
    assert!(
        matches!(
            &core.outcome,
            super::Outcome::Failed { stage: super::Stage::Entry, error }
                if error.message == "serial read failed" && error.causes.len() == 1
        ),
        "retain the entry read error and its underlying OS cause"
    );
    assert!(
        core.entry_reply.is_none() && core.segments.is_empty(),
        "zero response bytes cannot supply entry or page evidence"
    );
    assert!(
        matches!(core.exit, super::ExitDisposition::RecoveryRequired),
        "uncertain entry requires recovery assessment, not speculative exit"
    );
    assert!(
        matches!(core.write, super::WriteDisposition::NotAttempted),
        "Verify must not dispatch a second memory write"
    );
    assert!(
        second.transcript.complete,
        "the failed exchange must remain fully captured"
    );
    assert!(
        second.post_exit.gateway_off_evidence().is_none(),
        "do not borrow the earlier session's Off proof for the failed session"
    );
    let evidence = serde_json::to_value(second)?;
    assert_eq!(
        evidence
            .pointer("/transcript/events")
            .and_then(serde_json::Value::as_u64),
        Some(21),
        "retain all open, preflight, entry failure, and close events"
    );
    assert_eq!(
        evidence.pointer("/post_exit/attempt"),
        Some(&serde_json::Value::Null),
        "no fresh reconnect may follow uncertain entry"
    );
    assert_eq!(
        evidence
            .pointer("/post_exit/transcript/events")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "the skipped post-exit capture must contain no manufactured events"
    );
    Ok(())
}

fn assert_failed_entry_journal(path: &Path) -> TestResult {
    let journal = records(path)?;
    assert_eq!(
        journal.len(),
        5,
        "prepared, sole intent, both sessions, finished"
    );
    let failure = journal
        .get(3)
        .ok_or("failed session journal evidence missing")?;
    assert_eq!(
        failure.kind, "session_evidence",
        "retain the failed session record"
    );
    assert_eq!(
        failure
            .evidence
            .pointer("/details/core/outcome/stage/kind")
            .and_then(serde_json::Value::as_str),
        Some("entry"),
        "the actual failed phase must survive journal synchronization"
    );
    let finished = journal.last().ok_or("finished journal evidence missing")?;
    assert_eq!(
        finished.kind, "finished",
        "journal finalization must be recorded"
    );
    assert_eq!(
        finished
            .evidence
            .get("status")
            .and_then(serde_json::Value::as_str),
        Some("possibly_changed"),
        "a fresh Off observation must not finalize failed independent verification"
    );
    Ok(())
}

#[tokio::test]
async fn changed_verify_guard_keeps_the_only_prior_write_unresolved() -> TestResult {
    let mut harness = Harness::new()?;
    let script = mcp_script(&harness.trial, 1, Scenario::GuardDrift)?;
    harness.connection(2)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "fresh guard drift must prevent completed verification: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "readback from Apply is not proof across sessions"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        1,
        "Verify cannot repair or rebase an unexpected page"
    );
    assert!(
        writes(&events, 2)
            .iter()
            .all(|bytes| bytes.first() != Some(&b'W')),
        "the mismatching Verify session must remain read-only"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_after_dispatch_preserves_both_required_fresh_gateway_checks() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.cancel_on_write = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await?;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "the cancellation must actually occur at W dispatch"
    );
    assert!(
        result.succeeded(&harness.trial),
        "late cancellation cannot abandon required verification: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "both fresh Gateway checks and the read-only MCP proof remain owed"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::OffVerifiedAcrossSessions,
        "all owed verification must complete despite late cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_before_start_opens_no_handle_and_records_no_intent() -> TestResult {
    let mut harness = Harness::new()?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "pre-start cancellation is not successful exit"
    );
    assert_eq!(
        harness.backend.opens, 0,
        "pre-start cancellation must prevent any open"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::NotWritten,
        "pre-start cancellation creates no obligation"
    );
    let records = records(&harness.journal_path())?;
    assert!(
        records.iter().all(|record| record.kind != "write_intent"),
        "pre-start cancellation cannot journal intent"
    );
    Ok(())
}

#[tokio::test]
async fn a_poisoned_journal_prevents_the_only_write_even_after_valid_fresh_reads() -> TestResult {
    let mut harness = Harness::new()?;
    let rejected = harness
        .journal
        .prepare(&harness.trial, Path::new("second-backup.json"));
    assert!(
        rejected.is_err(),
        "duplicate preparation must poison the journal"
    );
    let script = mcp_script(&harness.trial, 0, Scenario::JournalRefusal)?;
    harness.connection(0)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.trial),
        "a poisoned journal cannot authorize an exit write: {result:?}"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        0,
        "failed durable intent must prevent W"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::NotWritten,
        "journal refusal cannot create an accepted intent"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "journal refusal permits known-boundary CAT cleanup but no Verify MCP"
    );
    Ok(())
}

#[tokio::test]
async fn final_journal_sync_failure_cannot_advance_the_engine_to_verified_off() -> TestResult {
    let mut harness = Harness::new()?;
    let relocation = tempfile::tempdir()?;
    let original = harness.directory.path().to_path_buf();
    let moved = relocation.path().join("evidence");
    harness.connection(3)?.move_directory_on_close = Some((original.clone(), moved.clone()));
    let result = harness.run().await?;
    assert!(
        moved.is_dir(),
        "the injected directory-sync failure must actually relocate the fixture"
    );
    std::fs::rename(&moved, &original)?;
    assert!(
        !result.succeeded(&harness.trial),
        "complete radio exchanges cannot substitute for durable final-session evidence: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "the fault occurs after every required mock connection"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "pure finalization must follow successful journal synchronization"
    );
    assert!(
        result.finalization_error.is_some(),
        "the journal synchronization error must remain in the workflow result"
    );
    assert!(
        harness.journal.finish(&harness.trial).is_err(),
        "the failed durability boundary must permanently poison the journal"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        1,
        "evidence failure cannot trigger another memory write"
    );
    assert!(
        events.contains(&Event::Close(3)) && events.contains(&Event::Dropped(3)),
        "the final fresh connection was still released"
    );
    Ok(())
}

#[tokio::test]
async fn required_original_capture_failure_prevents_protocol_dispatch() -> TestResult {
    let mut harness = Harness::new()?;
    let path = harness.directory.path().join("session-0/transcript.jsonl");
    let failed = Arc::new(AtomicBool::new(false));
    let captures = harness.captures.as_mut().ok_or("capture pairs missing")?;
    captures
        .first_mut()
        .ok_or("first capture pair missing")?
        .original = Recorder::named(
        File::open(path)?,
        Arc::clone(&failed),
        "read-only-original.jsonl",
    );
    harness.connection(0)?.mock = MockTransport::new();
    let result = harness.run().await?;
    assert!(
        failed.load(Ordering::Relaxed),
        "the read-only recorder must really fail"
    );
    assert!(
        !result.succeeded(&harness.trial),
        "missing original capture cannot be accepted: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::NotWritten,
        "capture refusal must happen before intent"
    );
    assert!(
        harness
            .events()?
            .iter()
            .all(|event| !matches!(event, Event::Write(_, _))),
        "required capture failure must prevent actual protocol dispatch"
    );
    assert_eq!(
        harness.backend.opens, 0,
        "failed required open-request recording must prevent even the first open"
    );
    Ok(())
}

#[tokio::test]
async fn required_post_exit_capture_failure_cannot_finalize_or_start_verify() -> TestResult {
    let mut harness = Harness::new()?;
    let path = harness
        .directory
        .path()
        .join("session-0/post-exit-transcript.jsonl");
    let failed = Arc::new(AtomicBool::new(false));
    let captures = harness.captures.as_mut().ok_or("capture pairs missing")?;
    captures
        .first_mut()
        .ok_or("first capture pair missing")?
        .post_exit = Recorder::named(
        File::open(path)?,
        Arc::clone(&failed),
        "read-only-fresh.jsonl",
    );
    harness.connection(1)?.mock = MockTransport::new();
    let result = harness.run().await?;
    assert!(
        failed.load(Ordering::Relaxed),
        "the read-only post-exit recorder must really fail"
    );
    assert!(
        !result.succeeded(&harness.trial),
        "missing fresh capture cannot establish Off: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        TerminalExitTrialStatus::PossiblyChanged,
        "fresh capture failure cannot clear the accepted intent"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "failed fresh lifecycle recording must prevent any fresh open"
    );
    assert!(
        writes(&harness.events()?, 1).is_empty(),
        "required fresh capture failure must prevent fresh CAT dispatch"
    );
    Ok(())
}
