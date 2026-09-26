//! Tests for the PM1 write across four mock connections: the journaled write
//! intent precedes the W frame, and nothing is rolled back.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::memory::{Pm1Name, Pm1NameUpdate, TextFieldUpdateStatus};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::radio::readiness::SETTLE_QUIET;
use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID};
use kenwood_tmd750::{Address, FirmwareIdentity, Identity, Page, RadioModel, RadioType};
use kenwood_transport::bluetooth::{BluetoothService, RfcommChannel};
use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::capture::{Artifacts, CaptureKind, create_private_file};
use crate::mcp::link::{NativeLink, UsbLink};
use crate::mcp::reconnect::{Backend, VerificationOutcome, VerificationStage};
use crate::native;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open(usize),
    Write(usize, Vec<u8>),
    Close(usize),
    Dropped(usize),
    Enumerate,
    /// A native backend wait, such as the hold after an exit.
    Wait(Duration),
}

type Log = Arc<Mutex<Vec<Event>>>;

#[derive(serde::Deserialize)]
struct JournalEnvelope {
    sequence: u64,
    event: JournalRecord,
}

#[derive(serde::Deserialize)]
struct JournalRecord {
    kind: String,
    evidence: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct IntentRecord {
    session_id: u8,
    intent_id: u8,
    memory_format: u8,
    scope: ScopeRecord,
    status: String,
}

#[derive(serde::Deserialize)]
struct ScopeRecord {
    identity: IdentityRecord,
    field: String,
    page_address: u32,
    page_length: usize,
    original_page: Vec<u8>,
    desired_page: Vec<u8>,
    current_name: String,
    desired_name: String,
}

#[derive(serde::Deserialize)]
struct IdentityRecord {
    model: String,
    firmware: String,
    radio_type: String,
}

fn read_records(path: &Path) -> Result<Vec<JournalRecord>, TestError> {
    let mut records = Vec::new();
    for (index, line) in std::fs::read_to_string(path)?.lines().enumerate() {
        let envelope: JournalEnvelope = serde_json::from_str(line)?;
        assert_eq!(
            envelope.sequence,
            u64::try_from(index)?,
            "journal sequence must be contiguous"
        );
        records.push(envelope.event);
    }
    Ok(records)
}

#[derive(Debug)]
struct JournalProof {
    path: PathBuf,
    original: Vec<u8>,
    desired: Vec<u8>,
}

impl JournalProof {
    fn verify_dispatch(&self, connection: usize, bytes: &[u8]) -> TestResult {
        assert_eq!(connection, 0, "only the first MCP connection may write");
        let records = read_records(&self.path)?;
        let prepared = records.first().ok_or("prepared record missing")?;
        assert_eq!(
            prepared.kind, "prepared",
            "prepared scope must precede intent"
        );
        assert_eq!(
            prepared.evidence.get("operator_approved_apply"),
            Some(&serde_json::Value::Bool(true)),
            "explicit apply approval must be retained"
        );
        let intents: Vec<_> = records
            .iter()
            .filter(|record| record.kind == "write_intent")
            .collect();
        assert_eq!(
            intents.len(),
            1,
            "exactly one intent must already exist before W"
        );
        let record = intents.first().ok_or("intent record missing")?;
        let intent: IntentRecord = serde_json::from_value(record.evidence.clone())?;
        assert_eq!(intent.session_id, 1, "apply session must own the intent");
        assert_eq!(intent.intent_id, 1, "the update has exactly one intent ID");
        assert_eq!(
            intent.memory_format, 0,
            "only memory format zero is supported"
        );
        assert_eq!(
            intent.status, "possibly_changed",
            "risk must be recorded before dispatch"
        );
        assert_eq!(
            intent.scope.field, "pm.PmName1",
            "only PM1 belongs in the journal"
        );
        assert_eq!(
            intent.scope.page_address, 323_584,
            "canonical page must be recorded"
        );
        assert_eq!(
            intent.scope.page_length, 256,
            "partial recovery pages are invalid"
        );
        assert_eq!(
            intent.scope.current_name, "PM1",
            "original text must be recorded"
        );
        assert_eq!(
            intent.scope.desired_name, "BASE",
            "requested text must be recorded"
        );
        assert_eq!(
            intent.scope.identity.model, "TM-D750",
            "exact model must be retained"
        );
        assert_eq!(
            intent.scope.identity.firmware, "1.02",
            "exact firmware must be retained"
        );
        assert_eq!(
            intent.scope.identity.radio_type, "K,2,1",
            "complete type must be retained"
        );
        assert_eq!(
            intent.scope.original_page, self.original,
            "original recovery bytes must precede W"
        );
        assert_eq!(
            intent.scope.desired_page, self.desired,
            "exact desired bytes must precede W"
        );
        assert_eq!(
            bytes.len(),
            261,
            "one frame must contain header and complete page"
        );
        assert_eq!(
            bytes.get(5..),
            Some(self.desired.as_slice()),
            "wire payload must equal the recorded desired page"
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
    cancel_on_memory_write: Option<Arc<AtomicBool>>,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        append(&self.log, Event::Write(self.id, bytes.to_vec()))?;
        if bytes.first() == Some(&b'W') {
            self.journal
                .verify_dispatch(self.id, bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        self.mock.write(bytes).await?;
        if bytes.first() == Some(&b'W')
            && let Some(cancelled) = &self.cancel_on_memory_write
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
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
        self.mock.close().await
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
    /// The one endpoint this backend enumerates and opens.
    endpoint: SerialCandidate,
    opens: usize,
    elapsed: Duration,
    /// Once `opens` reaches the first value, the next `count` enumerations
    /// report the endpoint absent.
    absent_after_open: Option<(usize, usize)>,
}

impl Backend for TestBackend {
    type Connection = Connection;

    fn open(
        &mut self,
        selected: &SerialCandidate,
        baud: u32,
    ) -> Result<Connection, TransportError> {
        assert_eq!(
            selected, &self.endpoint,
            "only the selected endpoint may open"
        );
        assert_eq!(
            baud, 9600,
            "only the qualified transport settings may be used"
        );
        append(&self.log, Event::Open(self.opens))?;
        self.opens += 1;
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected extra connection"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        append(&self.log, Event::Enumerate)?;
        if let Some((after, count)) = self.absent_after_open
            && self.opens >= after
            && count > 0
        {
            self.absent_after_open = Some((after, count - 1));
            return Ok(Vec::new());
        }
        Ok(vec![self.endpoint.clone()])
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
        path: "/dev/cu.pm1-name-update-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn panel_endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.pm1-name-update-panel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

/// A fresh handle that never answers `ID`, as the operation-panel endpoint
/// does before its tuple is ready.
fn silent_script() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect_hang(b"ID\r");
    mock
}

fn update() -> Result<Pm1NameUpdate, TestError> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut page = [0xA5; 256];
    page.get_mut(10..26).ok_or("name range missing")?.fill(0);
    page.get_mut(10..13)
        .ok_or("name bytes missing")?
        .copy_from_slice(b"PM1");
    Ok(Pm1NameUpdate::prepare(
        &identity,
        &page,
        &Pm1Name::new("PM1")?,
        &Pm1Name::new("BASE")?,
    )?)
}

fn identity_script(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
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

fn read_script(update: &Pm1NameUpdate, before: &[u8]) -> Result<MockTransport, TestError> {
    let mut mock = identity_script("1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    read(&mut mock, update.page(), before);
    Ok(mock)
}

fn mcp_script(
    update: &Pm1NameUpdate,
    apply: bool,
    bad_write_ack: bool,
) -> Result<MockTransport, TestError> {
    let mut mock = read_script(
        update,
        if apply {
            update.original_page()
        } else {
            update.desired_page()
        },
    )?;
    if apply {
        mock.expect(
            &frame(update.page(), update.desired_page()),
            if bad_write_ack { &[0x15] } else { &[ACK] },
        );
        if bad_write_ack {
            return Ok(mock);
        }
        read(&mut mock, update.page(), update.desired_page());
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

struct Harness {
    directory: tempfile::TempDir,
    update: Pm1NameUpdate,
    backend: TestBackend,
    journal: UpdateJournal,
    captures: Option<[SessionCaptures; 2]>,
    cancelled: Arc<AtomicBool>,
    capture_failed: Arc<AtomicBool>,
}

impl Harness {
    /// The main-unit endpoint with two handles per session: the MCP session,
    /// then the fresh identity check.
    fn new() -> Result<Self, TestError> {
        Self::build(endpoint(), false)
    }

    /// `endpoint` with, per session, the MCP session, a silent identity
    /// attempt when `silent_identity`, then the fresh identity check.
    fn build(endpoint: SerialCandidate, silent_identity: bool) -> Result<Self, TestError> {
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let update = update()?;
        let capture_failed = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let proof = Arc::new(JournalProof {
            path: directory.path().join("update-journal.jsonl"),
            original: update.original_page().to_vec(),
            desired: update.desired_page().to_vec(),
        });
        let mut connections = VecDeque::new();
        let mut captures = Vec::new();
        for phase in 0..2 {
            let mut mocks = vec![mcp_script(&update, phase == 0, false)?];
            if silent_identity {
                mocks.push(silent_script());
            }
            mocks.push(identity_script("1.02"));
            for mock in mocks {
                connections.push_back(Connection {
                    id: connections.len(),
                    mock,
                    log: Arc::clone(&log),
                    journal: Arc::clone(&proof),
                    fail_close: false,
                    cancel_on_memory_write: None,
                });
            }
            let artifacts = Artifacts::create(
                CaptureKind::Mcp,
                Some(&directory.path().join(format!("session-{phase}"))),
                Arc::clone(&capture_failed),
            )?;
            let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
            captures.push(SessionCaptures {
                original: artifacts.transcript,
                post_exit,
            });
        }
        let mut journal = UpdateJournal::create(directory.path(), Arc::clone(&capture_failed))?;
        journal.prepare(&update, &directory.path().join("source-backup.json"))?;
        Ok(Self {
            directory,
            update,
            backend: TestBackend {
                connections,
                log,
                endpoint,
                opens: 0,
                elapsed: Duration::ZERO,
                absent_after_open: None,
            },
            journal,
            captures: Some(
                captures
                    .try_into()
                    .map_err(|_| "exactly two capture sets required")?,
            ),
            cancelled: Arc::new(AtomicBool::new(false)),
            capture_failed,
        })
    }

    async fn run(&mut self) -> Result<WorkflowResult, TestError> {
        let endpoint = self.backend.endpoint.clone();
        Ok(run(
            &mut UsbLink::new(&mut self.backend, &endpoint, 9600),
            &mut self.update,
            &mut self.journal,
            self.captures.take().ok_or("workflow already run")?,
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

    fn fail_capture(&mut self, session: usize, post_exit: bool) -> TestResult {
        let path = self.directory.path().join("failed-transcript.jsonl");
        drop(create_private_file(&path)?);
        let mut recorder = Recorder::named(
            File::open(path)?,
            Arc::clone(&self.capture_failed),
            "failed-transcript.jsonl",
        );
        recorder.record("deliberately fail on a read-only file");
        assert!(
            !recorder.summary().complete,
            "capture failure must be injected"
        );
        let captures = self
            .captures
            .as_mut()
            .ok_or("captures consumed")?
            .get_mut(session)
            .ok_or("session missing")?;
        if post_exit {
            captures.post_exit = recorder;
        } else {
            captures.original = recorder;
        }
        Ok(())
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

/// Enumeration snapshots taken after handle `after` was released and before
/// handle `before` opened.
fn enumerations_between(events: &[Event], after: usize, before: usize) -> Result<usize, TestError> {
    let start = position(events, &Event::Dropped(after))?;
    let end = position(events, &Event::Open(before))?;
    Ok(events
        .get(start..end)
        .ok_or("event window out of order")?
        .iter()
        .filter(|event| **event == Event::Enumerate)
        .count())
}

#[tokio::test]
async fn second_session_opens_only_after_the_endpoint_stayed_enumerated_for_the_quiet_period()
-> TestResult {
    let mut harness = Harness::new()?;
    // The three snapshots after the first session's fresh handle report the
    // endpoint absent, which restarts the quiet period.
    harness.backend.absent_after_open = Some((2, 3));
    let result = harness.run().await?;
    assert!(result.succeeded(&harness.update), "{result:?}");
    let events = harness.events()?;
    let polls = usize::try_from(SETTLE_QUIET.as_secs())?;
    assert_eq!(
        enumerations_between(&events, 1, 2)?,
        3 + polls + 1,
        "three absent snapshots, then the quiet period observed once per second plus the first sighting"
    );
    assert!(
        harness.backend.elapsed >= SETTLE_QUIET,
        "the settle wait must consume the quiet period on the clock"
    );
    Ok(())
}

#[tokio::test]
async fn second_session_is_not_opened_when_the_endpoint_never_settles() -> TestResult {
    let mut harness = Harness::new()?;
    harness.backend.absent_after_open = Some((2, usize::MAX));
    let result = harness.run().await?;
    assert!(!result.succeeded(&harness.update), "{result:?}");
    assert_eq!(
        harness.backend.opens, 2,
        "the second programming session must not open on an unsettled endpoint"
    );
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    let second = result
        .sessions
        .get(1)
        .ok_or("second session evidence missing")?;
    let error = second
        .open_error
        .as_ref()
        .ok_or("settle failure must be recorded as the open error")?;
    assert!(
        error.to_string().contains("settle budget"),
        "the open error names the exhausted settle budget: {error}"
    );
    assert!(
        matches!(
            second.post_exit.outcome(),
            VerificationOutcome::Skipped {
                reason: SkipReason::OriginalOpenFailed
            }
        ),
        "no fresh verification follows a session that never opened: {second:?}"
    );
    Ok(())
}

#[tokio::test]
async fn success_uses_four_ordered_handles_and_one_prejournaled_memory_write() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(result.succeeded(&harness.update), "{result:?}");
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    assert_eq!(harness.backend.opens, 4);
    let events = harness.events()?;
    assert_eq!(memory_writes(&events), 1);
    for id in 0..4 {
        assert!(
            position(&events, &Event::Close(id))? < position(&events, &Event::Dropped(id))?,
            "connection {id} must close before release"
        );
        if id < 3 {
            assert!(
                position(&events, &Event::Dropped(id))? < position(&events, &Event::Open(id + 1))?,
                "connection {id} must be released before the next open"
            );
        }
        let observed = writes(&events, id);
        if id % 2 == 0 {
            assert_eq!(observed.last(), Some(&b"E".as_slice()));
        } else {
            assert_eq!(observed, [b"ID\r".as_slice(), b"FV\r", b"TY\r"]);
        }
    }
    assert!(
        writes(&events, 2)
            .iter()
            .all(|bytes| bytes.first() != Some(&b'W')),
        "the second MCP session must be read-only"
    );
    for session in &result.sessions {
        assert!(
            matches!(session.post_exit, PostExit::Single(_)),
            "the main-unit endpoint verifies with one open"
        );
    }
    harness.journal.finish(&harness.update)?;
    let records = read_records(&harness.directory.path().join("update-journal.jsonl"))?;
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
        ]
    );
    assert_eq!(
        records
            .last()
            .and_then(|record| record.evidence.get("status"))
            .and_then(serde_json::Value::as_str),
        Some("verified_across_sessions")
    );
    Ok(())
}

#[tokio::test]
async fn panel_endpoint_retries_a_silent_identity_before_each_fresh_check() -> TestResult {
    let mut harness = Harness::build(panel_endpoint(), true)?;
    let result = harness.run().await?;
    assert!(result.succeeded(&harness.update), "{result:?}");
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    assert_eq!(
        harness.backend.opens, 6,
        "each session opens the MCP handle, a silent attempt and the matching attempt"
    );
    for session in &result.sessions {
        assert!(
            matches!(session.post_exit, PostExit::Readiness(_)),
            "the operation-panel endpoint uses the bounded readiness check"
        );
        assert!(session.post_exit.succeeded(), "{session:?}");
    }
    let events = harness.events()?;
    assert_eq!(memory_writes(&events), 1);
    for id in [1, 4] {
        assert_eq!(
            writes(&events, id),
            [b"ID\r".as_slice()],
            "the silent attempt sends ID only"
        );
        assert!(
            position(&events, &Event::Dropped(id))? < position(&events, &Event::Open(id + 1))?,
            "the silent handle is released before the retry opens"
        );
    }
    for id in [2, 5] {
        assert_eq!(
            writes(&events, id),
            [b"ID\r".as_slice(), b"FV\r", b"TY\r"],
            "the matching attempt reads the whole tuple"
        );
    }
    Ok(())
}

#[tokio::test]
async fn either_original_or_fresh_close_failure_prevents_the_next_session() -> TestResult {
    for connection in 0..2 {
        let mut harness = Harness::new()?;
        harness.connection(connection)?.fail_close = true;
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.update),
            "close failure cannot count as success: {result:?}"
        );
        assert_eq!(harness.backend.opens, connection + 1);
        assert_eq!(
            harness.update.status(),
            TextFieldUpdateStatus::PossiblyChanged
        );
        assert_eq!(result.sessions.len(), 1);
        let first = result.sessions.first().ok_or("session missing")?;
        if connection == 0 {
            assert!(first.close_error.is_some(), "retain original close failure");
            assert!(
                matches!(
                    first.post_exit.outcome(),
                    VerificationOutcome::Skipped {
                        reason: SkipReason::OriginalCloseFailed
                    }
                ),
                "original close failure must prohibit fresh CAT: {first:?}"
            );
            assert!(
                !harness.events()?.contains(&Event::Enumerate),
                "original release failure must prevent reconnect"
            );
        } else {
            assert!(
                matches!(
                    first.post_exit.outcome(),
                    VerificationOutcome::Failed {
                        stage: VerificationStage::Close,
                        ..
                    }
                ),
                "fresh close failure must remain distinct from identity success: {first:?}"
            );
        }
        assert!(
            !harness.events()?.contains(&Event::Open(2)),
            "failed close must prevent the next session"
        );
    }
    Ok(())
}

#[tokio::test]
async fn fresh_identity_mismatch_closes_without_retry_or_following_update_session() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(1)?.mock = identity_script("1.03");
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "fresh identity mismatch cannot qualify the update: {result:?}"
    );
    assert_eq!(harness.backend.opens, 2);
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    assert!(
        matches!(
            result
                .sessions
                .first()
                .ok_or("session missing")?
                .post_exit
                .outcome(),
            VerificationOutcome::Failed {
                stage: VerificationStage::IdentityMismatch,
                ..
            }
        ),
        "fresh identity failure must retain its exact stage: {result:?}"
    );
    let events = harness.events()?;
    assert!(
        events.contains(&Event::Close(1)),
        "mismatching identity connection must close"
    );
    assert!(
        events.contains(&Event::Dropped(1)),
        "mismatching identity connection must be released"
    );
    assert!(
        !events.contains(&Event::Open(2)),
        "identity mismatch must prohibit further sessions"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_after_dispatch_still_completes_independent_verification() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.cancel_on_memory_write = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await?;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "scripted cancellation must occur after W"
    );
    assert!(result.succeeded(&harness.update), "{result:?}");
    assert_eq!(harness.backend.opens, 4);
    assert_eq!(memory_writes(&harness.events()?), 1);
    Ok(())
}

#[tokio::test]
async fn uncertain_ack_prevents_exit_reconnect_retry_and_rollback() -> TestResult {
    let mut harness = Harness::new()?;
    let script = mcp_script(&harness.update, true, true)?;
    harness.connection(0)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "uncertain ACK cannot qualify the update: {result:?}"
    );
    assert_eq!(harness.backend.opens, 1);
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    let events = harness.events()?;
    assert!(
        !writes(&events, 0).contains(&b"E".as_slice()),
        "uncertain W must prohibit speculative E"
    );
    assert_eq!(memory_writes(&events), 1);
    assert!(
        events.contains(&Event::Close(0)),
        "uncertain transport must still close"
    );
    assert!(
        events.contains(&Event::Dropped(0)),
        "uncertain transport must still be released"
    );
    assert!(
        !events.contains(&Event::Enumerate),
        "uncertain framing must prevent reconnect"
    );
    assert!(
        matches!(
            result
                .sessions
                .first()
                .ok_or("session missing")?
                .post_exit
                .outcome(),
            VerificationOutcome::Skipped {
                reason: SkipReason::OriginalUpdateIncomplete
            }
        ),
        "reconnect ineligibility must retain the incomplete-exchange reason: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn stale_first_page_exits_without_journaling_or_dispatching_a_write() -> TestResult {
    let mut harness = Harness::new()?;
    let mut wrong = *harness.update.original_page();
    *wrong.get_mut(9).ok_or("PM selector missing")? ^= 1;
    let mut mock = read_script(&harness.update, &wrong)?;
    mock.expect(b"E", &[ACK]);
    harness.connection(0)?.mock = mock;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "stale baseline must prevent update success: {result:?}"
    );
    assert_eq!(harness.update.status(), TextFieldUpdateStatus::NotWritten);
    assert_eq!(harness.backend.opens, 2);
    assert_eq!(memory_writes(&harness.events()?), 0);
    let records = read_records(&harness.directory.path().join("update-journal.jsonl"))?;
    assert!(
        records.iter().all(|record| record.kind != "write_intent"),
        "stale baseline must prevent intent creation"
    );
    Ok(())
}

#[tokio::test]
async fn separate_session_page_drift_never_claims_success_or_writes_again() -> TestResult {
    let mut harness = Harness::new()?;
    let mut wrong = *harness.update.desired_page();
    *wrong.get_mut(200).ok_or("unrelated page byte missing")? ^= 1;
    let mut mock = read_script(&harness.update, &wrong)?;
    mock.expect(b"E", &[ACK]);
    harness.connection(2)?.mock = mock;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "independent-session mismatch cannot qualify the update: {result:?}"
    );
    assert_eq!(harness.backend.opens, 4);
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    let last = result.sessions.last().ok_or("session missing")?;
    assert!(
        last.post_exit.succeeded(),
        "fresh CAT may succeed despite a page mismatch"
    );
    assert!(
        matches!(
            last.core.as_ref().ok_or("core evidence missing")?.outcome,
            Outcome::Failed {
                stage: Stage::FreshComparison,
                ..
            }
        ),
        "retain the independent-session comparison failure: {last:?}"
    );
    let events = harness.events()?;
    assert_eq!(memory_writes(&events), 1);
    assert!(
        writes(&events, 2)
            .iter()
            .all(|bytes| bytes.first() != Some(&b'W')),
        "a verification mismatch must not trigger another write"
    );
    Ok(())
}

#[tokio::test]
async fn incomplete_original_capture_prevents_open_at_either_session() -> TestResult {
    for session in 0..2 {
        let mut harness = Harness::new()?;
        harness.fail_capture(session, false)?;
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.update),
            "incomplete original capture cannot qualify a session: {result:?}"
        );
        assert_eq!(harness.backend.opens, session * 2);
        assert_eq!(
            harness.update.status(),
            if session == 0 {
                TextFieldUpdateStatus::NotWritten
            } else {
                TextFieldUpdateStatus::PossiblyChanged
            }
        );
        let last = result.sessions.last().ok_or("session missing")?;
        assert!(
            last.synchronization_error.is_some(),
            "retain the failed capture synchronization"
        );
        assert!(
            !last.transcript.complete,
            "capture must remain explicitly incomplete"
        );
        assert_eq!(memory_writes(&harness.events()?), session);
    }
    Ok(())
}

#[tokio::test]
async fn incomplete_fresh_cat_capture_prevents_new_handle_and_preserves_possible_change()
-> TestResult {
    let mut harness = Harness::new()?;
    harness.fail_capture(0, true)?;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "incomplete fresh capture cannot qualify the update: {result:?}"
    );
    assert_eq!(harness.backend.opens, 1);
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    let post_exit = &result.sessions.first().ok_or("session missing")?.post_exit;
    assert!(
        !post_exit.transcript().complete,
        "fresh capture must remain explicitly incomplete"
    );
    assert!(
        matches!(
            post_exit.outcome(),
            VerificationOutcome::Failed {
                stage: VerificationStage::Capture,
                ..
            }
        ),
        "fresh capture failure must retain its stage: {post_exit:?}"
    );
    assert_eq!(memory_writes(&harness.events()?), 1);
    Ok(())
}

#[test]
fn only_unconfirmed_exit_requests_recovery_without_claiming_restoration() -> TestResult {
    for exit in [
        ExitDisposition::RecoveryRequired,
        ExitDisposition::NotAcknowledged,
    ] {
        let guidance = exit_guidance(&exit).ok_or("uncertain exit recovery guidance missing")?;
        assert!(
            guidance.contains("fully power-cycle the radio before reconnecting"),
            "uncertain exit must require an explicit recovery boundary"
        );
        assert!(
            guidance.contains("does not report which text is stored"),
            "a power cycle must not be described as proof of restoration"
        );
        assert!(
            guidance.contains("do not retry or restore blindly"),
            "uncertain updates must prohibit speculative recovery writes"
        );
    }
    for exit in [ExitDisposition::NotEntered, ExitDisposition::Acknowledged] {
        assert!(
            exit_guidance(&exit).is_none(),
            "known exit state must not request unneeded recovery"
        );
    }
    Ok(())
}

#[tokio::test]
async fn failed_durable_session_evidence_blocks_finalization_and_following_sessions() -> TestResult
{
    let mut harness = Harness::new()?;
    harness.journal.fail_evidence_for_test();
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "unsynchronized session evidence must not qualify the update: {result:?}"
    );
    assert_eq!(harness.backend.opens, 2);
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    assert!(
        result.finalization_error.is_some(),
        "retain the durable evidence failure independently of successful I/O"
    );
    let first = result.sessions.first().ok_or("session missing")?;
    assert!(
        first.succeeded(),
        "scripted apply and fresh CAT must have completed"
    );
    assert!(
        !harness.events()?.contains(&Event::Open(2)),
        "unsynchronized evidence must prevent the verify session"
    );
    assert!(
        harness.journal.finish(&harness.update).is_err(),
        "poisoned journal must prohibit a later completion marker"
    );
    let records = read_records(&harness.directory.path().join("update-journal.jsonl"))?;
    assert!(
        records.iter().all(|record| record.kind != "finished"),
        "failed session synchronization must never be followed by a finished record"
    );
    Ok(())
}

/// A native Bluetooth backend over scripted connections: every open is
/// logged with its service, each reports RFCOMM channel 27, and every wait
/// is logged.
struct NativeBackend {
    connections: VecDeque<Connection>,
    log: Log,
    opens: usize,
    services: Vec<BluetoothService>,
}

impl native::Backend for NativeBackend {
    type Connection = Connection;

    async fn open(
        &mut self,
        endpoint: &native::Endpoint,
        service: BluetoothService,
        _cancelled: &AtomicBool,
    ) -> Result<native::Opened<Connection>, native::OpenFailure> {
        self.services.push(service);
        let connection = self.connections.pop_front().ok_or_else(|| {
            native::OpenFailure::from_error(&io::Error::other("no scripted native connection"))
        })?;
        append(&self.log, Event::Open(self.opens))
            .map_err(|error| native::OpenFailure::from_error(&error))?;
        self.opens += 1;
        Ok(native::Opened {
            connection,
            resolved: native::Resolved {
                address: endpoint.address.to_string(),
                rfcomm_channel: 27,
            },
        })
    }

    async fn wait(&mut self, duration: Duration) {
        let _logged = append(&self.log, Event::Wait(duration));
    }
}

/// One session over native Bluetooth: `gateway` answers the pre-entry `GW`,
/// and only Gateway Off reaches the MCP session of `mcp_script`.
fn native_mcp_script(
    update: &Pm1NameUpdate,
    apply: bool,
    gateway: &[u8],
) -> Result<MockTransport, TestError> {
    let mut mock = MockTransport::new();
    mock.expect(b"GW\r", gateway);
    if gateway != b"GW 0\r" {
        return Ok(mock);
    }
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    read(
        &mut mock,
        update.page(),
        if apply {
            update.original_page()
        } else {
            update.desired_page()
        },
    );
    if apply {
        mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
        read(&mut mock, update.page(), update.desired_page());
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

/// The native post-exit check: the identity tuple, then Gateway Off.
fn native_fresh_script() -> MockTransport {
    let mut mock = identity_script("1.02");
    mock.expect(b"GW\r", b"GW 0\r");
    mock
}

struct NativeHarness {
    _directory: tempfile::TempDir,
    update: Pm1NameUpdate,
    backend: NativeBackend,
    journal: UpdateJournal,
    captures: Option<[SessionCaptures; 2]>,
    cancelled: AtomicBool,
}

impl NativeHarness {
    /// Scripted native connections, in open order, from `scripts`.
    fn new(
        scripts: impl FnOnce(&Pm1NameUpdate) -> Result<Vec<MockTransport>, TestError>,
    ) -> Result<Self, TestError> {
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let update = update()?;
        let capture_failed = Arc::new(AtomicBool::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let proof = Arc::new(JournalProof {
            path: directory.path().join("update-journal.jsonl"),
            original: update.original_page().to_vec(),
            desired: update.desired_page().to_vec(),
        });
        let connections = scripts(&update)?
            .into_iter()
            .enumerate()
            .map(|(id, mock)| Connection {
                id,
                mock,
                log: Arc::clone(&log),
                journal: Arc::clone(&proof),
                fail_close: false,
                cancel_on_memory_write: None,
            })
            .collect();
        let mut captures = Vec::new();
        for phase in 0..2 {
            let artifacts = Artifacts::create(
                CaptureKind::Mcp,
                Some(&directory.path().join(format!("session-{phase}"))),
                Arc::clone(&capture_failed),
            )?;
            let post_exit = artifacts.reserve_post_exit(Arc::clone(&capture_failed))?;
            captures.push(SessionCaptures {
                original: artifacts.transcript,
                post_exit,
            });
        }
        let mut journal = UpdateJournal::create(directory.path(), Arc::clone(&capture_failed))?;
        journal.prepare(&update, &directory.path().join("source-backup.json"))?;
        Ok(Self {
            _directory: directory,
            update,
            backend: NativeBackend {
                connections,
                log,
                opens: 0,
                services: Vec::new(),
            },
            journal,
            captures: Some(
                captures
                    .try_into()
                    .map_err(|_| "exactly two capture sets required")?,
            ),
            cancelled: AtomicBool::new(false),
        })
    }

    /// Run both sessions, returning whether the workflow succeeded and its
    /// serialized evidence.
    async fn run(&mut self) -> Result<(bool, serde_json::Value), TestError> {
        let endpoint = native::Endpoint {
            address: "01:23:45:67:89:AB".parse()?,
            helper: None,
        };
        let result = run(
            &mut NativeLink::new(&mut self.backend, &endpoint),
            &mut self.update,
            &mut self.journal,
            self.captures.take().ok_or("workflow already run")?,
            &self.cancelled,
        )
        .await;
        Ok((
            result.succeeded(&self.update),
            serde_json::to_value(&result)?,
        ))
    }

    fn events(&self) -> Result<Vec<Event>, TestError> {
        Ok(self
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?
            .clone())
    }
}

fn nth_position(events: &[Event], value: &Event, nth: usize) -> Result<usize, TestError> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| *event == value)
        .nth(nth)
        .map(|(index, _)| index)
        .ok_or_else(|| format!("{value:?} occurrence {nth} missing").into())
}

#[tokio::test]
async fn native_update_pins_the_channel_holds_after_each_exit_and_requires_gateway_off()
-> TestResult {
    let mut harness = NativeHarness::new(|update| {
        Ok(vec![
            native_mcp_script(update, true, b"GW 0\r")?,
            native_fresh_script(),
            native_mcp_script(update, false, b"GW 0\r")?,
            native_fresh_script(),
        ])
    })?;
    let (succeeded, evidence) = harness.run().await?;
    assert!(succeeded, "{evidence}");
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::VerifiedAcrossSessions
    );
    let channel = RfcommChannel::new(27)?;
    assert_eq!(
        harness.backend.services,
        [
            BluetoothService::SerialPort,
            BluetoothService::FixedChannel(channel),
            BluetoothService::FixedChannel(channel),
            BluetoothService::FixedChannel(channel),
        ],
        "only the first open queries the service; later opens pin its channel"
    );
    let events = harness.events()?;
    assert_eq!(memory_writes(&events), 1);
    let hold = Event::Wait(Duration::from_secs(5));
    for (session, connection) in [(0, 0), (1, 2)] {
        let exit = position(&events, &Event::Write(connection, b"E".to_vec()))?;
        let waited = nth_position(&events, &hold, session)?;
        let closed = position(&events, &Event::Close(connection))?;
        assert!(
            exit < waited && waited < closed,
            "session {session} must hold its connection after the exit and before the close"
        );
        assert_eq!(
            writes(&events, connection).first(),
            Some(&b"GW\r".as_slice()),
            "Gateway Off is checked before programming entry"
        );
        assert_eq!(
            writes(&events, connection + 1),
            [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
            "the fresh check reads the identity tuple, then Gateway Off"
        );
    }
    for session in [0, 1] {
        let recorded = at(&evidence, &format!("/sessions/{session}"))?;
        assert_eq!(at(recorded, "/gateway_before/state")?, "off", "{recorded}");
        assert!(at(recorded, "/opening/attempts")?.is_array(), "{recorded}");
        assert_eq!(
            at(recorded, "/post_exit/service")?,
            "fixed_previously_opened_channel"
        );
        assert_eq!(at(recorded, "/post_exit/outcome/status")?, "matched");
        let deadline = at(recorded, "/post_exit/first_reply_timeout_milliseconds")?
            .as_u64()
            .ok_or("first-reply deadline is not a count")?;
        assert!(
            (29_000..=30_000).contains(&deadline),
            "an immediate reconnect waits until 30 s after the exit: {deadline}"
        );
    }
    Ok(())
}

/// The value at JSON `pointer` in `value`.
fn at<'a>(value: &'a serde_json::Value, pointer: &str) -> Result<&'a serde_json::Value, TestError> {
    value
        .pointer(pointer)
        .ok_or_else(|| format!("{pointer} missing from {value}").into())
}

#[tokio::test]
async fn native_update_refuses_programming_entry_unless_gateway_is_off() -> TestResult {
    let mut harness =
        NativeHarness::new(|update| Ok(vec![native_mcp_script(update, true, b"GW 2\r")?]))?;
    let (succeeded, evidence) = harness.run().await?;
    assert!(!succeeded, "{evidence}");
    assert_eq!(harness.update.status(), TextFieldUpdateStatus::NotWritten);
    let events = harness.events()?;
    assert_eq!(writes(&events, 0), [b"GW\r".as_slice()]);
    assert_eq!(memory_writes(&events), 0);
    assert!(
        !events.iter().any(|event| matches!(event, Event::Wait(_))),
        "no hold without a programming exit"
    );
    let session = at(&evidence, "/sessions/0")?;
    assert_eq!(
        at(session, "/gateway_before/state")?,
        "terminal",
        "{session}"
    );
    assert!(at(session, "/admission_error")?.is_object(), "{session}");
    assert_eq!(
        at(session, "/post_exit/outcome/reason")?,
        "original_admission_failed"
    );
    assert_eq!(
        at(&evidence, "/sessions")?.as_array().map(Vec::len),
        Some(1),
        "a refused session is not followed by another"
    );
    Ok(())
}

#[tokio::test]
async fn native_reopen_failure_after_the_write_leaves_the_update_possibly_changed() -> TestResult {
    let mut harness =
        NativeHarness::new(|update| Ok(vec![native_mcp_script(update, true, b"GW 0\r")?]))?;
    let (succeeded, evidence) = harness.run().await?;
    assert!(!succeeded, "{evidence}");
    assert_eq!(
        harness.update.status(),
        TextFieldUpdateStatus::PossiblyChanged
    );
    assert_eq!(memory_writes(&harness.events()?), 1);
    assert_eq!(
        harness.backend.services.first(),
        Some(&BluetoothService::SerialPort)
    );
    let channel = RfcommChannel::new(27)?;
    assert!(
        harness
            .backend
            .services
            .iter()
            .skip(1)
            .all(|service| *service == BluetoothService::FixedChannel(channel)),
        "the reopen uses only the pinned channel: {:?}",
        harness.backend.services
    );
    let post_exit = at(&evidence, "/sessions/0/post_exit")?;
    assert_eq!(at(post_exit, "/outcome/status")?, "failed", "{post_exit}");
    assert_eq!(at(post_exit, "/outcome/stage")?, "open", "{post_exit}");
    assert_eq!(
        at(&evidence, "/sessions")?.as_array().map(Vec::len),
        Some(1),
        "an unverified write is not followed by the confirmation session"
    );
    Ok(())
}
