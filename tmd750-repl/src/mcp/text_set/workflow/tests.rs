//! Four mock connections, one durable-intent-bound PM1 write, and no rollback.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::memory::Pm1Name;
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{
    KENWOOD_VID, MockTransport, TMD750_MAIN_PID, Transport, TransportError,
};
use kenwood_tmd750::{Address, FirmwareIdentity, Identity, Page, RadioModel, RadioType};

use super::*;
use crate::mcp::capture::{Artifacts, create_private_file};
use crate::mcp::reconnect::{VerificationOutcome, VerificationStage};

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
    opens: usize,
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
            "only the selected main-unit endpoint may open"
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
        path: "/dev/cu.pm1-name-update-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
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
    fn new() -> Result<Self, TestError> {
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
            for mock in [
                mcp_script(&update, phase == 0, false)?,
                identity_script("1.02"),
            ] {
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
                opens: 0,
                elapsed: Duration::ZERO,
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
        Ok(run(
            &mut self.backend,
            &endpoint(),
            9600,
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

#[tokio::test]
async fn success_uses_four_ordered_handles_and_one_prejournaled_memory_write() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(result.succeeded(&harness.update), "{result:?}");
    assert_eq!(
        harness.update.status(),
        Pm1NameUpdateStatus::VerifiedAcrossSessions
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
            Pm1NameUpdateStatus::PossiblyChanged
        );
        assert_eq!(result.sessions.len(), 1);
        let first = result.sessions.first().ok_or("session missing")?;
        if connection == 0 {
            assert!(first.close_error.is_some(), "retain original close failure");
            assert!(
                matches!(
                    first.post_exit.outcome,
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
                    first.post_exit.outcome,
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
        Pm1NameUpdateStatus::PossiblyChanged
    );
    assert!(
        matches!(
            result
                .sessions
                .first()
                .ok_or("session missing")?
                .post_exit
                .outcome,
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
        Pm1NameUpdateStatus::PossiblyChanged
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
                .outcome,
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
    assert_eq!(harness.update.status(), Pm1NameUpdateStatus::NotWritten);
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
        Pm1NameUpdateStatus::PossiblyChanged
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
                Pm1NameUpdateStatus::NotWritten
            } else {
                Pm1NameUpdateStatus::PossiblyChanged
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
        Pm1NameUpdateStatus::PossiblyChanged
    );
    let post_exit = &result.sessions.first().ok_or("session missing")?.post_exit;
    assert!(
        !post_exit.transcript.complete,
        "fresh capture must remain explicitly incomplete"
    );
    assert!(
        matches!(
            post_exit.outcome,
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
            guidance.contains("does not establish which name is stored"),
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
        Pm1NameUpdateStatus::PossiblyChanged
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
