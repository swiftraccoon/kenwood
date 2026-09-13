//! Four simulated handles, one durable MY1 intent, and complete lifecycle evidence.
//!
//! File reads establish recorded ordering, not independent proof of an fsync.
//! Synchronization failures are exercised separately through scoped failure seams.

use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::memory::{My1Callsign, My1CallsignUpdate, My1CallsignUpdateStatus};
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID};
use kenwood_tmd750::{
    Address, DvGatewayMode, FirmwareIdentity, Identity, Page, RadioModel, RadioType,
};
use kenwood_transport::{MockTransport, Transport, TransportError};

use super::*;
use crate::capture::{Artifacts, CaptureKind, Event as CaptureEvent};
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
struct Record {
    sequence: u64,
    utc_unix_nanoseconds: String,
    elapsed_microseconds: u128,
    event: serde_json::Value,
}

fn records(path: &Path) -> Result<Vec<Record>, TestError> {
    let text = std::fs::read_to_string(path)?;
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "every recorded event must be complete"
    );
    let records = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Record>, _>>()?;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.sequence,
            u64::try_from(index)?,
            "record sequence must be contiguous"
        );
        let timestamp = record.utc_unix_nanoseconds.parse::<i128>()?;
        assert!(timestamp > 0, "every record requires a valid UTC timestamp");
        let _timestamp = time::OffsetDateTime::from_unix_timestamp_nanos(timestamp)?;
    }
    assert!(records.windows(2).all(|pair| matches!(pair, [first, second] if first.elapsed_microseconds <= second.elapsed_microseconds)), "monotonic timestamps must preserve local order");
    Ok(records)
}

fn kind(record: &Record) -> Result<&str, TestError> {
    record
        .event
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "event kind missing".into())
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct IdentityRecord {
    model: String,
    firmware: String,
    radio_type: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct ControlRecord {
    address: u32,
    length: usize,
    data: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct ScopeRecord {
    identity: IdentityRecord,
    target_kind: String,
    field: String,
    page_address: u32,
    page_length: usize,
    original_page: Vec<u8>,
    desired_page: Vec<u8>,
    control_page: ControlRecord,
    current_name: String,
    desired_name: String,
}

#[derive(serde::Deserialize)]
struct IntentRecord {
    session_id: u8,
    intent_id: u8,
    memory_format: u8,
    scope: ScopeRecord,
    status: String,
}

#[derive(Debug)]
struct Proof {
    directory: PathBuf,
    original: Vec<u8>,
    desired: Vec<u8>,
    control: Vec<u8>,
}

impl Proof {
    fn transcript(&self, id: usize) -> PathBuf {
        self.directory
            .join(format!("session-{}", id / 2))
            .join(if id.is_multiple_of(2) {
                "transcript.jsonl"
            } else {
                "post-exit-transcript.jsonl"
            })
    }

    fn journal(&self) -> PathBuf {
        self.directory.join("update-journal.jsonl")
    }

    fn verify_open(&self, id: usize) -> TestResult {
        let captured = records(&self.transcript(id))?;
        let request = captured
            .last()
            .ok_or("open request not recorded before backend open")?;
        assert_eq!(
            kind(request)?,
            "open_requested",
            "the open request must precede actual handle acquisition"
        );
        assert_eq!(
            request.event.get("path"),
            Some(&serde_json::json!(endpoint().path)),
            "opening evidence must bind the selected endpoint"
        );
        assert_eq!(
            request.event.get("baud"),
            Some(&serde_json::json!(9600)),
            "opening evidence must bind serial settings"
        );
        if id == 2 {
            let journal = records(&self.journal())?;
            assert_eq!(
                journal.len(),
                3,
                "prepared, intent, and first session evidence must precede second MCP open"
            );
            let evidence = journal.last().ok_or("first session evidence missing")?;
            assert_eq!(
                kind(evidence)?,
                "session_evidence",
                "second MCP open requires recorded first-session evidence"
            );
            assert_eq!(
                evidence.event.pointer("/evidence/post_exit/outcome/status"),
                Some(&serde_json::json!("matched")),
                "first fresh verifier must have matched before second MCP open"
            );
        }
        Ok(())
    }

    fn verify_protocol_admission(&self, id: usize) -> TestResult {
        let captured = records(&self.transcript(id))?;
        let request = captured
            .iter()
            .position(|record| kind(record).ok() == Some("open_requested"))
            .ok_or("open request missing before protocol")?;
        let completed = captured
            .iter()
            .position(|record| kind(record).ok() == Some("open_completed"))
            .ok_or("open completion missing before protocol")?;
        assert!(
            request < completed,
            "handle acquisition must be captured before any active protocol call"
        );
        Ok(())
    }

    fn verify_scope(&self, scope: &ScopeRecord) {
        assert_eq!(
            scope.identity,
            IdentityRecord {
                model: "TM-D750".to_owned(),
                firmware: "1.02".to_owned(),
                radio_type: "K,2,1".to_owned()
            },
            "scope binds the exact captured identity"
        );
        assert_eq!(
            scope.target_kind, "pm_off_my1",
            "scope cannot fall back to PM1"
        );
        assert_eq!(
            scope.field, "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
            "only the first PM-Off MY entry is admitted"
        );
        assert_eq!(
            (scope.page_address, scope.page_length),
            (331_776, 256),
            "the complete canonical target page is required"
        );
        assert_eq!(
            (scope.control_page.address, scope.control_page.length),
            (323_584, 256),
            "the complete canonical control page is required"
        );
        assert_eq!(
            scope.original_page, self.original,
            "original recovery bytes stay immutable"
        );
        assert_eq!(
            scope.desired_page, self.desired,
            "requested target bytes stay immutable"
        );
        assert_eq!(
            scope.control_page.data, self.control,
            "every control-page byte is bound to the intent"
        );
        assert_eq!(
            scope.current_name, "",
            "empty original MY1 is represented explicitly"
        );
        assert_eq!(
            scope.desired_name, "KQ4NIT",
            "the intended text is preserved without normalization"
        );
    }

    fn verify_dispatch(&self, id: usize, bytes: &[u8]) -> TestResult {
        assert_eq!(id, 0, "only the first original handle may write");
        let journal = records(&self.journal())?;
        let [prepared, intent] = journal.as_slice() else {
            return Err("exactly prepared scope and one intent must exist before W".into());
        };
        assert_eq!(
            kind(prepared)?,
            "prepared",
            "preparation must precede intent"
        );
        assert_eq!(
            kind(intent)?,
            "write_intent",
            "intent must precede dispatch"
        );
        assert_eq!(
            prepared.event.pointer("/evidence/operator_approved_apply"),
            Some(&serde_json::json!(true)),
            "record explicit apply approval"
        );
        assert_eq!(
            prepared.event.pointer("/evidence/automatic_restore"),
            Some(&serde_json::json!(false)),
            "this setter provides no automatic rollback"
        );
        let prepared_scope: ScopeRecord = serde_json::from_value(
            prepared
                .event
                .pointer("/evidence/scope")
                .ok_or("prepared scope missing")?
                .clone(),
        )?;
        let intent: IntentRecord = serde_json::from_value(
            intent
                .event
                .get("evidence")
                .ok_or("intent evidence missing")?
                .clone(),
        )?;
        self.verify_scope(&prepared_scope);
        assert_eq!(
            intent.scope, prepared_scope,
            "intent must bind the unchanged prepared scope"
        );
        assert_eq!(
            (intent.session_id, intent.intent_id, intent.memory_format),
            (1, 1, 0),
            "one apply intent follows the supported format guard"
        );
        assert_eq!(
            intent.status, "possibly_changed",
            "conservative risk is recorded before dispatch"
        );
        assert_eq!(
            bytes.get(..5),
            Some(b"W\x05\x10\0\0".as_slice()),
            "literal-pinned address and full-page write length"
        );
        assert_eq!(
            bytes.len(),
            261,
            "the full header and page must be one write"
        );
        assert_eq!(
            bytes.get(5..),
            Some(self.desired.as_slice()),
            "wire payload must equal the durably intended page"
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
struct Cancellation {
    when: Vec<u8>,
    flag: Arc<AtomicBool>,
}

#[derive(Debug)]
struct Connection {
    id: usize,
    mock: MockTransport,
    log: Log,
    proof: Arc<Proof>,
    fail_close: bool,
    cancellation: Option<Cancellation>,
    entry_read_error: Option<io::Error>,
    reading_entry: bool,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.proof
            .verify_protocol_admission(self.id)
            .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        append(&self.log, Event::Write(self.id, bytes.to_vec()))?;
        if bytes.first() == Some(&b'W') {
            self.proof
                .verify_dispatch(self.id, bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        self.mock.write(bytes).await?;
        self.reading_entry = bytes == b"0M PROGRAM\r";
        if let Some(cancellation) = &self.cancellation
            && bytes == cancellation.when
        {
            cancellation.flag.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        if self.reading_entry
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
    proof: Arc<Proof>,
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
        assert_eq!(baud, 9600, "only the selected line settings may be used");
        self.proof
            .verify_open(self.opens)
            .map_err(|error| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other(error),
            })?;
        append(&self.log, Event::Open(self.opens))?;
        self.opens += 1;
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected extra open"),
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
        path: "/dev/cu.my1-workflow-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn update() -> Result<My1CallsignUpdate, TestError> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut target = [0xA5; 256];
    target.get_mut(..3).ok_or("target guards missing")?.fill(0);
    target.get_mut(8..16).ok_or("MY1 field missing")?.fill(0);
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM selector missing")? = 0;
    Ok(My1CallsignUpdate::prepare(
        &identity,
        &target,
        &control,
        None,
        &My1Callsign::new("KQ4NIT")?,
    )?)
}

fn identity_script(firmware: &str) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", format!("FV {firmware}\r").as_bytes());
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock
}

fn fresh_script(gateway: &[u8]) -> MockTransport {
    let mut mock = identity_script("1.02");
    mock.expect(b"GW\r", gateway);
    mock
}

fn frame(page: Page, data: &[u8]) -> Vec<u8> {
    let mut result = write_request(page).to_vec();
    result.extend_from_slice(data);
    result
}

fn read(mock: &mut MockTransport, page: Page, data: &[u8]) {
    mock.expect(&read_request(page), &frame(page, data));
    mock.expect(&[ACK], &[ACK]);
}

fn read_script(
    update: &My1CallsignUpdate,
    control: &[u8],
    target: &[u8],
) -> Result<MockTransport, TestError> {
    let mut mock = fresh_script(b"GW 0\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    read(&mut mock, update.control_page_spec(), control);
    read(&mut mock, update.page(), target);
    Ok(mock)
}

fn mcp_script(update: &My1CallsignUpdate, apply: bool) -> Result<MockTransport, TestError> {
    let mut mock = read_script(
        update,
        update.control_page(),
        if apply {
            update.original_page()
        } else {
            update.desired_page()
        },
    )?;
    if apply {
        mock.expect(&frame(update.page(), update.desired_page()), &[ACK]);
        read(&mut mock, update.page(), update.desired_page());
    }
    mock.expect(b"E", &[ACK]);
    Ok(mock)
}

struct Harness {
    _directory: tempfile::TempDir,
    update: My1CallsignUpdate,
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
        let proof = Arc::new(Proof {
            directory: directory.path().to_owned(),
            original: update.original_page().to_vec(),
            desired: update.desired_page().to_vec(),
            control: update.control_page().to_vec(),
        });
        let mut connections = VecDeque::new();
        let mut captures = Vec::new();
        for phase in 0..2 {
            for mock in [mcp_script(&update, phase == 0)?, fresh_script(b"GW 0\r")] {
                connections.push_back(Connection {
                    id: connections.len(),
                    mock,
                    log: Arc::clone(&log),
                    proof: Arc::clone(&proof),
                    fail_close: false,
                    cancellation: None,
                    entry_read_error: None,
                    reading_entry: false,
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
            _directory: directory,
            update,
            backend: TestBackend {
                connections,
                log,
                proof,
                opens: 0,
                elapsed: Duration::ZERO,
            },
            journal,
            captures: Some(
                captures
                    .try_into()
                    .map_err(|_| "exactly two capture sets are required")?,
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

    fn connection(&mut self, id: usize) -> Result<&mut Connection, TestError> {
        self.backend
            .connections
            .get_mut(id)
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

    fn fail_capture(&mut self, id: usize) -> TestResult {
        let path = self.backend.proof.transcript(id);
        let recorder = Recorder::named(
            File::open(path)?,
            Arc::clone(&self.capture_failed),
            "read-only-transcript.jsonl",
        );
        let captures = self
            .captures
            .as_mut()
            .ok_or("captures consumed")?
            .get_mut(id / 2)
            .ok_or("session missing")?;
        if id.is_multiple_of(2) {
            captures.original = recorder;
        } else {
            captures.post_exit = recorder;
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

fn memory_writes(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'W')))
        .count()
}

fn position(events: &[Event], expected: &Event) -> Result<usize, TestError> {
    events
        .iter()
        .position(|event| event == expected)
        .ok_or_else(|| format!("missing {expected:?}").into())
}

fn expected_original(update: &My1CallsignUpdate, apply: bool) -> Vec<Vec<u8>> {
    let mut expected: Vec<Vec<u8>> = [
        b"ID\r".as_slice(),
        b"FV\r",
        b"TY\r",
        b"GW\r",
        b"0M PROGRAM\r",
        b"R\0\0\x08\x28",
        &[ACK],
        b"R\x04\xF0\0\0",
        &[ACK],
        b"R\x05\x10\0\0",
        &[ACK],
    ]
    .into_iter()
    .map(<[u8]>::to_vec)
    .collect();
    if apply {
        let mut write = b"W\x05\x10\0\0".to_vec();
        write.extend_from_slice(update.desired_page());
        expected.extend([write, b"R\x05\x10\0\0".to_vec(), vec![ACK]]);
    }
    expected.push(b"E".to_vec());
    expected
}

fn assert_released(events: &[Event], opens: usize) -> TestResult {
    for id in 0..opens {
        assert!(
            position(events, &Event::Close(id))? < position(events, &Event::Dropped(id))?,
            "connection {id} must close before drop"
        );
        if id + 1 < opens {
            assert!(
                position(events, &Event::Dropped(id))? < position(events, &Event::Open(id + 1))?,
                "drop must precede the next fresh open"
            );
        }
    }
    Ok(())
}

fn assert_my1_session(
    session: &SessionEvidence,
    update: &My1CallsignUpdate,
    apply: bool,
) -> TestResult {
    let core = session.core.as_ref().ok_or("core evidence missing")?;
    assert!(
        matches!(core.outcome, Outcome::AwaitingCatVerification),
        "original protocol phase must complete"
    );
    assert_eq!(
        serde_json::to_value(&core.gateway_mode)?,
        serde_json::json!({"state":"off","raw":0}),
        "retain typed original Gateway Off"
    );
    assert_eq!(
        core.segments.len(),
        if apply { 4 } else { 3 },
        "retain every complete read"
    );
    assert_eq!(
        (
            core.segments.first().ok_or("format missing")?.address,
            core.segments.first().ok_or("format missing")?.length
        ),
        (8, 40),
        "format fragment is first"
    );
    let control = core.segments.get(1).ok_or("control page missing")?;
    assert_eq!(
        (control.address, control.length),
        (323_584, 256),
        "control page is complete and canonical"
    );
    assert_eq!(
        control.data,
        update.control_page().as_slice(),
        "retain all control bytes"
    );
    let target = core.segments.get(2).ok_or("target page missing")?;
    assert_eq!(
        (target.address, target.length),
        (331_776, 256),
        "target page is complete and canonical"
    );
    assert_eq!(
        target.data,
        if apply {
            update.original_page().as_slice()
        } else {
            update.desired_page().as_slice()
        },
        "retain the actual phase-specific target bytes"
    );
    if apply {
        assert_eq!(
            core.segments.last().ok_or("readback missing")?.data,
            update.desired_page().as_slice(),
            "immediate readback retains every desired-page byte"
        );
    }
    let (identity, gateway) = session
        .post_exit
        .gateway_off_evidence()
        .ok_or("fresh identity and typed Off proof missing")?;
    assert_eq!(
        identity,
        update.identity(),
        "fresh identity must match the bound tuple"
    );
    assert_eq!(
        gateway,
        DvGatewayMode::Off,
        "fresh Gateway Off is mandatory"
    );
    assert!(
        session.transcript.complete && session.post_exit.transcript.complete,
        "both captures must be complete"
    );
    Ok(())
}

#[tokio::test]
async fn four_handles_pin_the_full_wire_scope_and_durable_session_order() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(&harness.update),
        "MY1 workflow must qualify only complete evidence: {result:?}"
    );
    assert_eq!(
        harness.update.status(),
        My1CallsignUpdateStatus::VerifiedAcrossSessions,
        "both finalized sessions are required"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "exactly two original and two fresh handles are allowed"
    );
    let events = harness.events()?;
    assert_released(&events, 4)?;
    assert_eq!(
        memory_writes(&events),
        1,
        "MY1 must be written once without restoration"
    );
    for id in 0..4 {
        let actual: Vec<Vec<u8>> = writes(&events, id)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        let expected = if id % 2 == 0 {
            expected_original(&harness.update, id == 0)
        } else {
            [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"]
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect()
        };
        assert_eq!(
            actual, expected,
            "every command on connection {id} is independently pinned"
        );
    }
    for (index, session) in result.sessions.iter().enumerate() {
        assert_my1_session(session, &harness.update, index == 0)?;
    }
    harness.journal.finish(&harness.update)?;
    let journal = records(&harness.backend.proof.journal())?;
    let kinds = journal.iter().map(kind).collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        kinds,
        [
            "prepared",
            "write_intent",
            "session_evidence",
            "session_evidence",
            "finished"
        ],
        "journal must retain the exact two-session lifecycle"
    );
    assert_eq!(
        journal
            .last()
            .ok_or("finished record missing")?
            .event
            .pointer("/evidence/status"),
        Some(&serde_json::json!("verified_across_sessions")),
        "only complete external evidence permits verified status"
    );
    Ok(())
}

#[tokio::test]
async fn pre_intent_cancellation_opens_nothing_new_and_never_writes() -> TestResult {
    for before_open in [true, false] {
        let mut harness = Harness::new()?;
        if before_open {
            harness.cancelled.store(true, Ordering::Relaxed);
        } else {
            let mut mock = fresh_script(b"GW 0\r");
            mock.expect(b"0M PROGRAM\r", b"0M\r");
            mock.expect(b"E", &[ACK]);
            let flag = Arc::clone(&harness.cancelled);
            let connection = harness.connection(0)?;
            connection.mock = mock;
            connection.cancellation = Some(Cancellation {
                when: b"0M PROGRAM\r".to_vec(),
                flag,
            });
        }
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.update),
            "pre-intent cancellation cannot be success"
        );
        assert_eq!(
            harness.backend.opens,
            usize::from(!before_open),
            "cancellation must prohibit every later open"
        );
        assert_eq!(
            harness.update.status(),
            My1CallsignUpdateStatus::NotWritten,
            "no write obligation was created"
        );
        let events = harness.events()?;
        assert_eq!(
            memory_writes(&events),
            0,
            "cancellation before intent prohibits W"
        );
        assert_released(&events, harness.backend.opens)?;
        assert!(
            !events.contains(&Event::Enumerate),
            "cancellation must not start a fresh verification search"
        );
        if before_open {
            assert!(
                records(&harness.backend.proof.transcript(0))?.is_empty(),
                "initial cancellation must precede all original opening evidence"
            );
        }
        assert!(
            records(&harness.backend.proof.journal())?
                .iter()
                .all(|record| kind(record).ok() != Some("write_intent")),
            "cancelled preflight must not record a write intent"
        );
    }
    Ok(())
}

#[test]
fn synchronized_open_request_rechecks_cancellation_before_handle_acquisition() -> TestResult {
    let mut harness = Harness::new()?;
    let [
        SessionCaptures {
            mut original,
            post_exit,
        },
        _unused,
    ] = harness.captures.take().ok_or("captures missing")?;
    let mut session = SessionEvidence {
        core: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        synchronization_error: None,
        post_exit: PostExitVerification::skipped(
            SkipReason::OriginalOpenFailed,
            post_exit.summary(),
        ),
    };
    harness.cancelled.store(true, Ordering::Relaxed);
    let acquired = open_original(
        &mut harness.backend,
        &endpoint(),
        9600,
        &mut original,
        &mut session,
        &harness.cancelled,
    );
    assert!(
        acquired.is_none(),
        "the final pre-open cancellation check must prevent acquisition"
    );
    assert_eq!(
        harness.backend.opens, 0,
        "recorded opening intent is not permission to ignore cancellation"
    );
    assert!(
        harness.events()?.is_empty(),
        "no backend or protocol call may follow cancellation"
    );
    let captured = records(&harness.backend.proof.transcript(0))?;
    assert_eq!(
        captured.len(),
        1,
        "this seam starts after the early cancellation check and records only opening intent"
    );
    assert_eq!(
        kind(captured.first().ok_or("open request missing")?)?,
        "open_requested",
        "retain the requested action without inventing acquisition"
    );
    assert!(
        session.transcript.complete && session.synchronization_error.is_none(),
        "clean synchronized request capture is distinct from cancellation"
    );
    assert!(
        matches!(
            session.post_exit.outcome,
            VerificationOutcome::Skipped {
                reason: SkipReason::Cancelled
            }
        ),
        "retain the exact reason acquisition was refused"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_after_the_only_write_finishes_both_valid_verifications() -> TestResult {
    let mut harness = Harness::new()?;
    let when = frame(harness.update.page(), harness.update.desired_page());
    let flag = Arc::clone(&harness.cancelled);
    harness.connection(0)?.cancellation = Some(Cancellation { when, flag });
    let result = harness.run().await?;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "cancellation must occur after dispatch"
    );
    assert!(
        result.succeeded(&harness.update),
        "late cancellation cannot abandon required verification: {result:?}"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "both post-exit verifiers and the read-only session remain required"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        1,
        "late cancellation must not cause restoration or retry"
    );
    assert_released(&harness.events()?, 4)?;
    Ok(())
}

#[tokio::test]
async fn each_original_or_fresh_close_failure_blocks_the_next_open() -> TestResult {
    for failed in 0..4 {
        let mut harness = Harness::new()?;
        harness.connection(failed)?.fail_close = true;
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.update),
            "close failure {failed} cannot qualify completion"
        );
        assert_eq!(
            harness.backend.opens,
            failed + 1,
            "a failed close must prohibit every following handle"
        );
        assert_eq!(
            harness.update.status(),
            My1CallsignUpdateStatus::PossiblyChanged,
            "failed close cannot finalize a possible change"
        );
        let session = result.sessions.last().ok_or("session evidence missing")?;
        if failed % 2 == 0 {
            assert!(
                session.close_error.is_some(),
                "retain the original close failure"
            );
            assert!(
                matches!(
                    session.post_exit.outcome,
                    VerificationOutcome::Skipped {
                        reason: SkipReason::OriginalCloseFailed
                    }
                ),
                "original close failure prohibits fresh verification"
            );
        } else {
            assert!(
                matches!(
                    session.post_exit.outcome,
                    VerificationOutcome::Failed {
                        stage: VerificationStage::Close,
                        ..
                    }
                ),
                "fresh close failure remains distinct from matched CAT"
            );
        }
        assert_released(&harness.events()?, failed + 1)?;
        assert_eq!(
            memory_writes(&harness.events()?),
            1,
            "failed close cannot cause another write"
        );
    }
    Ok(())
}

#[tokio::test]
async fn fresh_identity_or_gateway_failure_is_never_retried_or_finalized() -> TestResult {
    for fresh in [1, 3] {
        for failure in 0..3 {
            let mut harness = Harness::new()?;
            harness.connection(fresh)?.mock = match failure {
                0 => identity_script("1.03"),
                1 => fresh_script(b"GW 2\r"),
                _ => fresh_script(b"N\r"),
            };
            let result = harness.run().await?;
            assert!(
                !result.succeeded(&harness.update),
                "fresh observation failure cannot qualify MY1"
            );
            assert_eq!(
                harness.backend.opens,
                fresh + 1,
                "no fresh-verification retry is permitted"
            );
            let last = result.sessions.last().ok_or("session missing")?;
            let expected = match failure {
                0 => VerificationStage::IdentityMismatch,
                1 => VerificationStage::GatewayMismatch,
                _ => VerificationStage::Gateway,
            };
            assert!(
                matches!(&last.post_exit.outcome, VerificationOutcome::Failed { stage, .. } if std::mem::discriminant(stage) == std::mem::discriminant(&expected)),
                "retain the exact fresh-verification boundary: {last:?}"
            );
            assert!(
                last.post_exit.gateway_off_evidence().is_none(),
                "identity or active/unknown Gateway cannot become Off evidence"
            );
            assert_eq!(
                harness.update.status(),
                My1CallsignUpdateStatus::PossiblyChanged,
                "retain possible change when fresh qualification fails"
            );
            assert_released(&harness.events()?, fresh + 1)?;
            assert_eq!(
                memory_writes(&harness.events()?),
                1,
                "fresh failure must not trigger another write"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn identity_only_verification_cannot_satisfy_an_otherwise_complete_my1_result() -> TestResult
{
    let mut harness = Harness::new()?;
    let mut result = harness.run().await?;
    assert!(
        result.succeeded(&harness.update),
        "establish the complete MY1 positive control first"
    );
    let verification = independent_verification(VerificationFixture::IdentityOnly).await?;
    assert!(
        verification.succeeded(),
        "the identity-only positive control must genuinely complete"
    );
    assert!(
        verification.gateway_off_evidence().is_none(),
        "identity matching alone is not a Gateway observation"
    );
    result
        .sessions
        .first_mut()
        .ok_or("first session missing")?
        .post_exit = verification;
    assert!(
        !result.succeeded(&harness.update),
        "MY1 success must require typed fresh Gateway Off even when all other evidence is complete"
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum VerificationFixture {
    IdentityOnly,
    DifferentIdentity,
}

async fn independent_verification(
    fixture: VerificationFixture,
) -> Result<PostExitVerification, TestError> {
    let mut harness = Harness::new()?;
    drop(
        harness
            .backend
            .connections
            .pop_front()
            .ok_or("unused original missing")?,
    );
    harness.backend.opens = 1;
    let [
        SessionCaptures {
            original: _unused,
            post_exit,
        },
        _second,
    ] = harness.captures.take().ok_or("capture pairs missing")?;
    let mut identity = harness.update.identity().clone();
    let connection = harness
        .backend
        .connections
        .front_mut()
        .ok_or("fresh connection missing")?;
    match fixture {
        VerificationFixture::IdentityOnly => {
            connection.mock = identity_script("1.02");
            Ok(reconnect::verify_required(
                &mut harness.backend,
                &endpoint(),
                9600,
                &identity,
                post_exit,
                &AtomicBool::new(false),
            )
            .await)
        }
        VerificationFixture::DifferentIdentity => {
            identity.firmware = FirmwareIdentity::new("1.03")?;
            connection.mock = identity_script("1.03");
            connection.mock.expect(b"GW\r", b"GW 0\r");
            Ok(reconnect::verify_required_gateway_off(
                &mut harness.backend,
                &endpoint(),
                9600,
                &identity,
                post_exit,
                &AtomicBool::new(false),
            )
            .await)
        }
    }
}

#[tokio::test]
async fn adapter_finalization_never_substitutes_gateway_or_identity_observations() -> TestResult {
    for fixture in [
        VerificationFixture::IdentityOnly,
        VerificationFixture::DifferentIdentity,
    ] {
        let mut harness = Harness::new()?;
        let [captures, _unused] = harness.captures.take().ok_or("capture pairs missing")?;
        let session = run_session(
            &mut harness.backend,
            &endpoint(),
            9600,
            &mut harness.update,
            &mut harness.journal,
            captures,
            &harness.cancelled,
        )
        .await;
        assert_my1_session(&session, &harness.update, true)?;
        assert!(
            harness.update.next_session().is_err(),
            "the positive apply control must await real finalization"
        );
        let id = session
            .core
            .as_ref()
            .and_then(|core| core.session_id)
            .ok_or("apply session ID missing")?;
        let verification = independent_verification(fixture).await?;
        assert!(
            verification.succeeded(),
            "the alternative verifier must succeed within its own requested scope"
        );
        match fixture {
            VerificationFixture::IdentityOnly => {
                assert!(
                    verification.gateway_off_evidence().is_none(),
                    "identity-only evidence genuinely lacks a Gateway observation"
                );
            }
            VerificationFixture::DifferentIdentity => {
                let (identity, gateway) = verification
                    .gateway_off_evidence()
                    .ok_or("alternative Off evidence missing")?;
                assert_ne!(
                    identity,
                    harness.update.identity(),
                    "the alternative verifier must retain the different actual identity"
                );
                assert_eq!(
                    gateway,
                    DvGatewayMode::Off,
                    "the alternative verifier did observe Off"
                );
            }
        }
        let finalized = harness.update.finalize_session(id, &verification);
        assert!(
            finalized.is_err(),
            "MY1 adapter must not invent Off or replace actual identity to finalize: {finalized:?}"
        );
        assert_eq!(
            harness.update.status(),
            My1CallsignUpdateStatus::PossiblyChanged,
            "unacceptable external evidence cannot clear possible change"
        );
        assert!(
            harness.update.next_session().is_err(),
            "unacceptable evidence cannot admit verification"
        );
    }
    Ok(())
}

#[tokio::test]
async fn complete_control_or_target_tail_drift_never_rebases_or_repairs() -> TestResult {
    for session in 0..2 {
        for change_control in [false, true] {
            let mut harness = Harness::new()?;
            let mut control = *harness.update.control_page();
            let mut target = if session == 0 {
                *harness.update.original_page()
            } else {
                *harness.update.desired_page()
            };
            if change_control {
                *control.last_mut().ok_or("control tail missing")? ^= 1;
            } else {
                *target.last_mut().ok_or("target tail missing")? ^= 1;
            }
            let mut mock = read_script(&harness.update, &control, &target)?;
            mock.expect(b"E", &[ACK]);
            harness.connection(session * 2)?.mock = mock;
            let result = harness.run().await?;
            assert!(
                !result.succeeded(&harness.update),
                "full-page drift must prohibit success"
            );
            assert_eq!(
                harness.backend.opens,
                (session + 1) * 2,
                "known-boundary mismatch permits one fresh verifier, not another MCP session"
            );
            assert_eq!(
                memory_writes(&harness.events()?),
                session,
                "mismatch cannot create a write or repair"
            );
            let last = result.sessions.last().ok_or("session evidence missing")?;
            assert!(
                matches!(
                    last.core.as_ref().ok_or("core missing")?.outcome,
                    Outcome::Failed {
                        stage: Stage::FreshComparison,
                        ..
                    }
                ),
                "retain the complete-page comparison failure"
            );
            assert!(
                last.post_exit.gateway_off_evidence().is_some(),
                "fresh CAT may succeed without erasing the earlier page mismatch"
            );
            assert_eq!(
                harness.update.status(),
                if session == 0 {
                    My1CallsignUpdateStatus::NotWritten
                } else {
                    My1CallsignUpdateStatus::PossiblyChanged
                },
                "preserve prior write evidence without rebasing"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn each_failed_capture_prevents_its_handle_from_opening() -> TestResult {
    for failed in 0..4 {
        let mut harness = Harness::new()?;
        harness.fail_capture(failed)?;
        let result = harness.run().await?;
        assert!(
            !result.succeeded(&harness.update),
            "incomplete capture cannot qualify the workflow"
        );
        assert_eq!(
            harness.backend.opens, failed,
            "required capture must fail before its associated open"
        );
        assert_eq!(
            memory_writes(&harness.events()?),
            usize::from(failed > 0),
            "capture loss permits no additional write"
        );
        let session = result.sessions.last().ok_or("session missing")?;
        if failed % 2 == 0 {
            assert!(
                session.synchronization_error.is_some(),
                "retain original capture admission failure"
            );
        } else {
            assert!(
                matches!(
                    session.post_exit.outcome,
                    VerificationOutcome::Failed {
                        stage: VerificationStage::Capture,
                        ..
                    }
                ),
                "retain required fresh-capture failure"
            );
        }
        assert_released(&harness.events()?, failed)?;
    }
    Ok(())
}

#[tokio::test]
async fn failed_post_open_capture_admission_only_closes_and_drops_the_handle() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(0)?.mock = MockTransport::new();
    let [
        SessionCaptures {
            mut original,
            post_exit,
        },
        _unused,
    ] = harness.captures.take().ok_or("captures missing")?;
    let selected = endpoint();
    original.record(CaptureEvent::OpenRequested {
        path: &selected.path,
        baud: 9600,
    });
    original.synchronize()?;
    let connection = harness.backend.open(&selected, 9600)?;
    drop(original);
    let mut original = Recorder::named(
        File::open(harness.backend.proof.transcript(0))?,
        Arc::clone(&harness.capture_failed),
        "read-only-open-completion.jsonl",
    );
    original.record(CaptureEvent::OpenCompleted);
    let mut session = SessionEvidence {
        core: None,
        transcript: original.summary(),
        open_error: None,
        close_error: None,
        synchronization_error: None,
        post_exit: PostExitVerification::skipped(
            SkipReason::OriginalCaptureIncomplete,
            post_exit.summary(),
        ),
    };
    session.synchronize_original(&mut original);
    let first_error = serde_json::to_value(
        session
            .synchronization_error
            .as_ref()
            .ok_or("capture failure missing")?,
    )?;
    let admitted = session.admit_original(connection, original).await;
    assert!(
        admitted.is_none(),
        "failed open-completion evidence cannot admit CAT"
    );
    assert_eq!(
        harness.events()?,
        [Event::Open(0), Event::Close(0), Event::Dropped(0)],
        "only close/drop may follow failed opening evidence"
    );
    assert_eq!(
        serde_json::to_value(
            session
                .synchronization_error
                .as_ref()
                .ok_or("first capture failure lost")?
        )?,
        first_error,
        "cleanup must preserve the first capture failure"
    );
    assert!(
        !session.transcript.complete && session.core.is_none(),
        "no protocol evidence may be manufactured"
    );
    assert!(
        session.post_exit.gateway_off_evidence().is_none(),
        "failed admission cannot create fresh Gateway evidence"
    );
    Ok(())
}

#[tokio::test]
async fn failed_pre_write_raw_sync_survives_successful_cleanup_and_blocks_fresh_open() -> TestResult
{
    let mut harness = Harness::new()?;
    let mut mock = read_script(
        &harness.update,
        harness.update.control_page(),
        harness.update.original_page(),
    )?;
    mock.expect(b"E", &[ACK]);
    harness.connection(0)?.mock = mock;
    harness.journal.fail_raw_sync_for_test();
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "failed raw synchronization cannot qualify the update"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "pre-W raw synchronization failure prohibits fresh opens"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        0,
        "raw evidence must synchronize before intent and W"
    );
    assert_eq!(
        harness.update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "no write intent may be accepted"
    );
    let session = result.sessions.first().ok_or("session missing")?;
    let failure = session
        .synchronization_error
        .as_ref()
        .ok_or("raw sync failure lost")?;
    assert!(
        failure
            .message
            .contains("injected pre-write raw-capture synchronization failure"),
        "later successful final sync must retain the earlier failure"
    );
    assert!(
        session.transcript.complete,
        "the injected fsync error is distinct from missing recorded bytes"
    );
    assert!(
        session.close_error.is_none(),
        "ordinary close and final capture synchronization can still succeed"
    );
    assert!(
        matches!(
            session.post_exit.outcome,
            VerificationOutcome::Skipped {
                reason: SkipReason::OriginalCaptureIncomplete
            }
        ),
        "failed durability must prohibit fresh verification"
    );
    assert!(
        records(&harness.backend.proof.journal())?
            .iter()
            .all(|record| kind(record).ok() != Some("write_intent")),
        "raw synchronization failure must precede journal intent"
    );
    assert_released(&harness.events()?, 1)?;
    Ok(())
}

#[tokio::test]
async fn failed_session_journal_sync_prevents_second_open_and_finalization() -> TestResult {
    let mut harness = Harness::new()?;
    harness.journal.fail_evidence_for_test();
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "unsynchronized session evidence cannot qualify MY1"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "session evidence must synchronize before second MCP open"
    );
    assert_eq!(
        harness.update.status(),
        My1CallsignUpdateStatus::PossiblyChanged,
        "failed finalization preserves possible change"
    );
    assert!(
        result.finalization_error.is_some(),
        "retain journal failure separately from successful protocol"
    );
    assert!(
        result
            .sessions
            .first()
            .ok_or("first session missing")?
            .succeeded(),
        "protocol and fresh Off can succeed without durable finalization"
    );
    assert!(
        harness.journal.finish(&harness.update).is_err(),
        "a poisoned journal must reject a later success marker"
    );
    assert!(
        records(&harness.backend.proof.journal())?
            .iter()
            .all(|record| kind(record).ok() != Some("finished")),
        "failed durable evidence cannot be followed by finished"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        1,
        "journal failure cannot authorize retry or restoration"
    );
    Ok(())
}

#[tokio::test]
async fn journal_control_binding_failure_prevents_intent_write_and_fresh_open() -> TestResult {
    let mut harness = Harness::new()?;
    let mut control = *harness.update.control_page();
    *control.last_mut().ok_or("control tail missing")? ^= 1;
    harness.update = My1CallsignUpdate::prepare(
        harness.update.identity(),
        harness.update.original_page(),
        &control,
        None,
        &My1Callsign::new("KQ4NIT")?,
    )?;
    let mut mock = read_script(&harness.update, &control, harness.update.original_page())?;
    mock.expect(b"E", &[ACK]);
    harness.connection(0)?.mock = mock;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "changed control binding cannot qualify the journal intent"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "poisoned journal must prohibit fresh verification after safe cleanup"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        0,
        "the original journal's control page cannot be silently rebased"
    );
    assert_eq!(
        harness.update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "failed bound intent must precede possible dispatch"
    );
    let session = result.sessions.first().ok_or("session evidence missing")?;
    let core = session.core.as_ref().ok_or("core evidence missing")?;
    assert!(
        matches!(
            core.outcome,
            Outcome::Failed {
                stage: Stage::DurableIntent,
                ..
            }
        ),
        "actual journal binding failure must occur before W"
    );
    assert!(
        matches!(core.exit, ExitDisposition::Acknowledged),
        "known-boundary journal rejection permits safe E/ACK"
    );
    assert!(
        session.post_exit.gateway_off_evidence().is_none(),
        "a poisoned journal must not create a new verification handle"
    );
    let failure = result
        .finalization_error
        .as_ref()
        .ok_or("sticky journal error missing")?;
    assert!(
        failure
            .message
            .contains("journal update differs from its prepared recovery record"),
        "retain the exact journal binding error after cleanup"
    );
    assert!(
        records(&harness.backend.proof.journal())?
            .iter()
            .all(|record| kind(record).ok() != Some("write_intent")),
        "binding rejection must not append an intent"
    );
    assert_released(&harness.events()?, 1)?;
    Ok(())
}

#[tokio::test]
async fn second_entry_enxio_retains_first_off_evidence_without_retry_or_speculative_exit()
-> TestResult {
    let mut harness = Harness::new()?;
    let mut mock = fresh_script(b"GW 0\r");
    mock.expect(b"0M PROGRAM\r", &[]);
    let connection = harness.connection(2)?;
    connection.mock = mock;
    connection.entry_read_error = Some(io::Error::from_raw_os_error(6));
    let result = harness.run().await?;
    assert!(
        !result.succeeded(&harness.update),
        "second entry with zero response bytes cannot qualify verification"
    );
    assert_eq!(
        harness.backend.opens, 3,
        "uncertain second entry prohibits every additional open"
    );
    assert_eq!(
        harness.update.status(),
        My1CallsignUpdateStatus::PossiblyChanged,
        "earlier write remains possible despite failed later entry"
    );
    assert_my1_session(
        result.sessions.first().ok_or("apply evidence missing")?,
        &harness.update,
        true,
    )?;
    let failed = result.sessions.get(1).ok_or("second session missing")?;
    let core = failed.core.as_ref().ok_or("second core missing")?;
    assert!(
        matches!(
            core.outcome,
            Outcome::Failed {
                stage: Stage::Entry,
                ..
            }
        ),
        "retain the exact failed second-entry boundary"
    );
    assert!(
        matches!(core.exit, ExitDisposition::RecoveryRequired),
        "unknown entry state prohibits speculative E"
    );
    assert!(
        core.entry_reply.is_none() && core.segments.is_empty(),
        "no entry or readback bytes may be invented"
    );
    assert!(
        failed.post_exit.gateway_off_evidence().is_none(),
        "first-session Off does not prove a second post-exit observation"
    );
    let events = harness.events()?;
    assert_eq!(
        writes(&events, 2),
        [
            b"ID\r".as_slice(),
            b"FV\r",
            b"TY\r",
            b"GW\r",
            b"0M PROGRAM\r"
        ],
        "only the failed entry follows the fresh guard queries"
    );
    assert_eq!(
        memory_writes(&events),
        1,
        "uncertain re-entry must not retry or restore the earlier write"
    );
    assert_released(&events, 3)?;
    let transcript = records(&harness.backend.proof.transcript(2))?;
    let entry = transcript
        .iter()
        .position(|record| {
            record.event.get("bytes") == Some(&serde_json::json!(b"0M PROGRAM\r".as_slice()))
        })
        .ok_or("entry request capture missing")?;
    let remaining = transcript
        .get(entry + 1..)
        .ok_or("entry result records missing")?;
    assert_eq!(
        remaining.iter().map(kind).collect::<Result<Vec<_>, _>>()?,
        [
            "write_completed",
            "read_failed",
            "close_requested",
            "close_completed"
        ],
        "entry failure must retain zero response bytes and close-only cleanup"
    );
    Ok(())
}
