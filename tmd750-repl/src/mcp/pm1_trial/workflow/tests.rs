//! Entire fixed PM1 transaction with six observable, entirely mock handles.
//!
//! Persistence means equality after MCP exit and re-entry on a fresh connection;
//! the mock does not supply independent evidence of a full-radio reboot.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{
    KENWOOD_VID, MockTransport, TMD750_MAIN_PID, Transport, TransportError,
};
use kenwood_tmd750::{Address, FirmwareIdentity, Identity, Page, RadioModel, RadioType};

use super::*;
use crate::mcp::capture::Artifacts;
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
struct JournalRecord {
    kind: String,
    evidence: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct IntentRecord {
    session_id: usize,
    intent_id: usize,
    write: String,
    scope: ScopeRecord,
    restoration_status: String,
}

#[derive(serde::Deserialize)]
struct ScopeRecord {
    identity: IdentityRecord,
    field: String,
    page_address: u32,
    page_length: usize,
    original_page: Vec<u8>,
    expected_page: Vec<u8>,
    temporary_name: String,
}

#[derive(serde::Deserialize)]
struct IdentityRecord {
    model: String,
    firmware: String,
    radio_type: String,
}

fn read_intents(path: &Path) -> Result<Vec<IntentRecord>, TestError> {
    let mut intents = Vec::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let record: JournalRecord = serde_json::from_str(line)?;
        if record.kind == "write_intent" {
            intents.push(serde_json::from_value(record.evidence)?);
        }
    }
    Ok(intents)
}

#[derive(Debug)]
struct JournalProof {
    path: PathBuf,
    original: Vec<u8>,
    expected: Vec<u8>,
}

impl JournalProof {
    fn verify_dispatch(&self, connection: usize, bytes: &[u8]) -> TestResult {
        assert!(
            matches!(connection, 0 | 2),
            "only the rename and restore connections may dispatch W"
        );
        let intents = read_intents(&self.path)?;
        let id = connection / 2 + 1;
        assert_eq!(
            intents.len(),
            id,
            "the corresponding durable journal intent must exist before W"
        );
        let intent = intents.last().ok_or("write intent absent before W")?;
        assert_eq!(
            intent.session_id, id,
            "journal session must match the current write"
        );
        assert_eq!(
            intent.intent_id, id,
            "each write must have its own intent ID"
        );
        assert_eq!(
            intent.write,
            if id == 1 { "rename" } else { "restore" },
            "journal must describe the actual fixed intent"
        );
        assert_eq!(
            intent.restoration_status, "possibly_changed",
            "journal must preserve the restoration obligation before W"
        );
        let scope = &intent.scope;
        assert_eq!(
            scope.field, "pm.PmName1",
            "only the approved field belongs in the journal"
        );
        assert_eq!(
            scope.page_address, 323_584,
            "the entire canonical page must be identified"
        );
        assert_eq!(
            scope.page_length, 256,
            "the journal cannot contain a partial page"
        );
        assert_eq!(scope.temporary_name, "PC TEXT TEST", "the label is fixed");
        assert_eq!(scope.identity.model, "TM-D750", "model must be recorded");
        assert_eq!(
            scope.identity.firmware, "1.02",
            "exact firmware must be recorded"
        );
        assert_eq!(
            scope.identity.radio_type, "K,2,1",
            "complete type must be recorded"
        );
        assert_eq!(
            scope.original_page, self.original,
            "exact original bytes must precede W"
        );
        assert_eq!(
            scope.expected_page, self.expected,
            "exact temporary bytes must precede W"
        );
        assert_eq!(
            bytes.get(5..),
            Some(if id == 1 {
                self.expected.as_slice()
            } else {
                self.original.as_slice()
            }),
            "actual W payload must be the corresponding already-recorded full page"
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
            selected.path,
            endpoint().path,
            "only the pinned endpoint may open"
        );
        assert_eq!(baud, 9600, "only the pinned transport settings may be used");
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
        path: "/dev/cu.fixed-pm1-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn trial() -> Result<PmNameTrial, TestError> {
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
    Ok(PmNameTrial::prepare_unqualified_offline(
        &identity, &page, "PM1",
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

fn mcp_script(
    trial: &PmNameTrial,
    phase: usize,
    bad_write_ack: bool,
) -> Result<MockTransport, TestError> {
    let mut mock = identity_script("1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    let before = if phase == 1 {
        trial.expected_page()
    } else {
        trial.original_page()
    };
    read(&mut mock, trial.page(), before);
    if phase < 2 {
        let after = if phase == 0 {
            trial.expected_page()
        } else {
            trial.original_page()
        };
        mock.expect(
            &frame(trial.page(), after),
            if bad_write_ack { &[0x15] } else { &[ACK] },
        );
        if bad_write_ack {
            return Ok(mock);
        }
        read(&mut mock, trial.page(), after);
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

struct Harness {
    directory: tempfile::TempDir,
    trial: PmNameTrial,
    backend: TestBackend,
    journal: Journal,
    captures: Vec<SessionCaptures>,
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
            path: directory.path().join("trial-journal.jsonl"),
            original: trial.original_page().to_vec(),
            expected: trial.expected_page().to_vec(),
        });
        let mut connections = VecDeque::new();
        let mut captures = Vec::new();
        for phase in 0..3 {
            for mock in [mcp_script(&trial, phase, false)?, identity_script("1.02")] {
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
                Arc::clone(&failed),
            )?;
            let post_exit = artifacts.reserve_post_exit(Arc::clone(&failed))?;
            captures.push(SessionCaptures {
                original: artifacts.transcript,
                post_exit,
            });
        }
        let mut journal = Journal::create(directory.path())?;
        journal.prepare(&trial, &directory.path().join("source-backup.json"), "PM1")?;
        Ok(Self {
            directory,
            trial,
            backend: TestBackend {
                connections,
                log,
                opens: 0,
                elapsed: Duration::ZERO,
            },
            journal,
            captures,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    async fn run(&mut self) -> WorkflowResult {
        run(
            &mut self.backend,
            &endpoint(),
            9600,
            &mut self.trial,
            &mut self.journal,
            std::mem::take(&mut self.captures),
            &self.cancelled,
        )
        .await
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

#[tokio::test]
async fn three_session_success_writes_only_twice_and_retires_every_handle_in_order() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await;
    assert!(
        result.succeeded(&harness.trial),
        "all required evidence must succeed: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::RestorationVerified,
        "only the third finalized session clears restoration"
    );
    assert_eq!(
        harness.backend.opens, 6,
        "exactly three MCP and three fresh CAT connections"
    );
    let events = harness.events()?;
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'W')))
            .count(),
        2,
        "only the rename and exact restore may write"
    );
    for id in 0..6 {
        assert!(
            position(&events, &Event::Close(id))? < position(&events, &Event::Dropped(id))?,
            "each connection must close before release"
        );
        if id < 5 {
            assert!(
                position(&events, &Event::Dropped(id))? < position(&events, &Event::Open(id + 1))?,
                "each old handle must be gone before the next open"
            );
        }
        let observed = writes(&events, id);
        if id % 2 == 0 {
            assert_eq!(
                observed.last(),
                Some(&b"E".as_slice()),
                "E must be the final command on every MCP connection"
            );
        } else {
            assert_eq!(
                observed,
                vec![b"ID\r".as_slice(), b"FV\r", b"TY\r"],
                "each fresh verification is read-only identity exactly once"
            );
        }
    }
    let intents = read_intents(&harness.directory.path().join("trial-journal.jsonl"))?;
    assert_eq!(
        intents
            .iter()
            .map(|intent| (intent.session_id, intent.intent_id))
            .collect::<Vec<_>>(),
        [(1, 1), (2, 2)],
        "the durable journal must retain exactly both fixed write intents"
    );
    Ok(())
}

#[tokio::test]
async fn old_close_failure_prevents_fresh_cat_and_any_restoration_attempt() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.fail_close = true;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "old close failure cannot count as success: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "failed old close prohibits every new handle"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "rename remains unresolved"
    );
    assert_eq!(result.sessions.len(), 1, "no second session may begin");
    let session = result.sessions.first().ok_or("session missing")?;
    assert!(session.close_error.is_some(), "retain the close failure");
    assert!(
        matches!(
            session.post_exit.outcome,
            VerificationOutcome::Skipped {
                reason: SkipReason::OriginalCloseFailed
            }
        ),
        "fresh CAT must be explicitly ineligible"
    );
    assert!(
        !harness.events()?.contains(&Event::Enumerate),
        "failed release prevents reconnect entirely"
    );
    Ok(())
}

#[tokio::test]
async fn fresh_identity_failure_closes_but_never_starts_the_restore_session() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(1)?.mock = identity_script("1.03");
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "fresh identity mismatch cannot count as success: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "no retry and no restore connection after mismatch"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "identity mismatch cannot clear restoration"
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
        "the exact mismatch must survive in the report"
    );
    let events = harness.events()?;
    assert!(
        events.contains(&Event::Close(1)),
        "the mismatched connection must still close"
    );
    assert!(
        events.contains(&Event::Dropped(1)),
        "the mismatched connection must be released"
    );
    assert!(
        !events.contains(&Event::Open(2)),
        "do not restore through an unproven connection"
    );
    Ok(())
}

#[tokio::test]
async fn cancel_after_rename_dispatch_still_restores_and_proves_persistence() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.cancel_on_memory_write = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "the scripted cancellation must have occurred"
    );
    assert!(
        result.succeeded(&harness.trial),
        "late cancellation must preserve safe restoration: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 6,
        "all restoration and fresh-session verification connections remain required"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::RestorationVerified,
        "complete the restoration despite cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn uncertain_write_ack_never_sends_exit_or_opens_another_handle() -> TestResult {
    let mut harness = Harness::new()?;
    let script = mcp_script(&harness.trial, 0, true)?;
    harness.connection(0)?.mock = script;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "uncertain W cannot count as success: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "uncertain framing prohibits fresh connections"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "missing ACK cannot prove no change"
    );
    let events = harness.events()?;
    assert!(
        !writes(&events, 0).contains(&b"E".as_slice()),
        "never speculate E after an unacknowledged write"
    );
    assert!(
        events.contains(&Event::Close(0)),
        "still close the failed transport"
    );
    assert!(
        events.contains(&Event::Dropped(0)),
        "release the failed transport"
    );
    assert!(
        !events.contains(&Event::Enumerate),
        "do not launch reconnect after uncertain framing"
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
                reason: SkipReason::OriginalTrialIncomplete
            }
        ),
        "retain the exact reconnect ineligibility"
    );
    Ok(())
}

#[tokio::test]
async fn mismatching_third_session_never_claims_persisted_restoration_or_writes_again() -> TestResult
{
    let mut harness = Harness::new()?;
    let mut mock = identity_script("1.02");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    let mut wrong = *harness.trial.original_page();
    *wrong.get_mut(200).ok_or("unrelated page byte missing")? ^= 1;
    read(&mut mock, harness.trial.page(), &wrong);
    mock.expect(b"E", &[ACK]);
    harness.connection(4)?.mock = mock;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "a mismatching fresh-session page cannot establish restoration: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "immediate restore readback did not prove equality after MCP exit and re-entry"
    );
    assert_eq!(
        result.sessions.len(),
        3,
        "the mismatch occurs only in the final read-only proof"
    );
    let last = result.sessions.last().ok_or("third session missing")?;
    assert!(
        last.post_exit.succeeded(),
        "fresh CAT may succeed despite a failed fresh-session page comparison"
    );
    assert!(
        matches!(
            last.core.as_ref().ok_or("core evidence missing")?.outcome,
            Outcome::Failed {
                stage: Stage::FreshComparison,
                ..
            }
        ),
        "retain the whole-page mismatch"
    );
    let events = harness.events()?;
    assert!(
        writes(&events, 4)
            .iter()
            .all(|bytes| bytes.first() != Some(&b'W')),
        "the fresh-session verification must remain read-only even when bytes differ"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'W')))
            .count(),
        2,
        "never issue a stale third restoration write"
    );
    Ok(())
}
