//! Tests for the two-connection apply path over mock connections: page
//! comparison, journaled write intent, readback, and the post-exit CAT check.
//!
//! Ordering assertions read the capture files; fsync itself is exercised
//! through the injected synchronization-failure seams.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID};
use kenwood_tmd750::{
    Address, FirmwareIdentity, Identity, MenuAssignment, MenuFieldSnapshot, MenuUpdatePlan, Page,
    PageReplacement, RadioModel, RadioType, SlotIndex,
};
use kenwood_transport::{MockTransport, Transport, TransportError};
use serde_json::Value;

use super::{Captures, WorkflowResult, run_workflow};
use crate::capture::{Recorder, create_private_file};
use crate::mcp::ExitDisposition;
use crate::mcp::reconnect::{Backend, PostExit, VerificationOutcome, VerificationStage};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Open(usize),
    Write(usize, Vec<u8>),
    Baud(usize, u32),
    Close(usize),
    Dropped(usize),
    Enumerate,
}

type Log = Arc<Mutex<Vec<Event>>>;

fn append(log: &Log, event: Event) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Write(io::Error::other("test log poisoned")))?
        .push(event);
    Ok(())
}

fn records(path: &Path) -> Result<Vec<Value>, TestError> {
    let text = std::fs::read_to_string(path)?;
    assert!(
        text.is_empty() || text.ends_with('\n'),
        "capture records must retain complete lines"
    );
    let records = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.get("sequence"),
            Some(&serde_json::json!(index)),
            "record sequence must be contiguous"
        );
        let timestamp = record
            .get("utc_unix_nanoseconds")
            .and_then(Value::as_str)
            .ok_or("record timestamp missing")?
            .parse::<i128>()?;
        assert!(
            timestamp > 0,
            "capture timestamps must be actual UTC observations"
        );
    }
    Ok(records)
}

fn kind(record: &Value) -> Option<&str> {
    record.pointer("/event/kind").and_then(Value::as_str)
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct RecordedPage {
    address: u32,
    length: usize,
    expected: Vec<u8>,
    replacement: Vec<u8>,
}

impl From<&PageReplacement> for RecordedPage {
    fn from(replacement: &PageReplacement) -> Self {
        Self {
            address: replacement.page().address().as_u32(),
            length: replacement.page().len(),
            expected: replacement.expected().to_vec(),
            replacement: replacement.replacement().to_vec(),
        }
    }
}

#[derive(Debug)]
struct Proof {
    directory: PathBuf,
    plan: MenuUpdatePlan,
    endpoint: SerialCandidate,
}

impl Proof {
    fn transcript(&self, id: usize) -> PathBuf {
        self.directory.join(if id == 0 {
            "transcript.jsonl"
        } else {
            "post-exit-transcript.jsonl"
        })
    }

    fn journal(&self) -> PathBuf {
        self.directory.join("menu-journal.jsonl")
    }

    fn before_open(&self, id: usize, log: &Log) -> TestResult {
        let captured = records(&self.transcript(id))?;
        let request = captured.last().ok_or("opening request not recorded")?;
        assert_eq!(
            kind(request),
            Some("open_requested"),
            "record the request before acquiring a handle"
        );
        assert_eq!(
            request.pointer("/event/path"),
            Some(&serde_json::json!(self.endpoint.path)),
            "opening evidence must bind the selected endpoint"
        );
        assert_eq!(
            request.pointer("/event/baud"),
            Some(&serde_json::json!(9600)),
            "opening evidence must bind the fixed baud"
        );
        let journal = records(&self.journal())?;
        let prepared = journal.first().ok_or("prepared plan missing before open")?;
        assert_eq!(
            kind(prepared),
            Some("prepared"),
            "the immutable plan must precede radio access"
        );
        let pages: Vec<RecordedPage> = serde_json::from_value(
            prepared
                .pointer("/event/pages")
                .ok_or("prepared pages missing")?
                .clone(),
        )?;
        assert_eq!(
            pages,
            self.plan
                .replacements()
                .iter()
                .map(RecordedPage::from)
                .collect::<Vec<_>>(),
            "preparation must retain every complete target and guard image"
        );
        if id == 1 {
            let events = log.lock().map_err(|_| "test log poisoned")?;
            assert!(
                events.contains(&Event::Close(0)),
                "original close must precede fresh opening"
            );
            assert!(
                events.contains(&Event::Dropped(0)),
                "original drop must precede fresh opening"
            );
            drop(events);
            let finished = journal
                .last()
                .ok_or("original evidence missing before fresh open")?;
            assert_eq!(
                kind(finished),
                Some("original_finished"),
                "original lifecycle evidence must precede fresh opening"
            );
            assert_eq!(
                finished.pointer("/event/evidence/exit"),
                Some(&serde_json::json!("acknowledged")),
                "fresh opening requires acknowledged detached exit"
            );
        }
        Ok(())
    }

    fn before_protocol(&self, id: usize, bytes: &[u8]) -> TestResult {
        let captured = records(&self.transcript(id))?;
        assert!(
            captured
                .iter()
                .any(|record| kind(record) == Some("open_completed")),
            "opening completion must be captured before protocol dispatch"
        );
        let request = captured.last().ok_or("protocol request not captured")?;
        assert_eq!(
            kind(request),
            Some("write_requested"),
            "required capture must precede actual dispatch"
        );
        assert_eq!(
            request.pointer("/event/bytes"),
            Some(&serde_json::json!(bytes)),
            "capture must retain exact dispatched bytes"
        );
        if bytes.first() == Some(&b'W') {
            assert_eq!(
                id, 0,
                "the fresh CAT handle must never issue a memory write"
            );
            self.before_write(bytes)?;
        }
        Ok(())
    }

    fn before_write(&self, bytes: &[u8]) -> TestResult {
        let journal = records(&self.journal())?;
        let intent = journal.last().ok_or("write intent missing")?;
        assert_eq!(
            kind(intent),
            Some("before_write"),
            "the durable intent must immediately precede its write"
        );
        let recorded: RecordedPage = serde_json::from_value(
            intent
                .pointer("/event/page")
                .ok_or("complete intended page missing")?
                .clone(),
        )?;
        let replacement = self
            .plan
            .replacements()
            .iter()
            .find(|replacement| replacement.page().address().as_u32() == recorded.address)
            .ok_or("intent page is outside the immutable plan")?;
        assert!(
            !replacement.is_noop(),
            "unchanged guards must not create write intents"
        );
        assert_eq!(
            recorded,
            RecordedPage::from(replacement),
            "intent must retain the exact complete before and after images"
        );
        assert_eq!(
            bytes,
            frame(replacement.page(), replacement.replacement()),
            "one complete frame must match its recorded intent"
        );
        Ok(())
    }
}

#[derive(Debug)]
struct Connection {
    id: usize,
    mock: MockTransport,
    proof: Arc<Proof>,
    log: Log,
    fail_close: bool,
    error_after: Option<Vec<u8>>,
    cancellation: Option<(Vec<u8>, Arc<AtomicBool>)>,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.proof
            .before_protocol(self.id, bytes)
            .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        append(&self.log, Event::Write(self.id, bytes.to_vec()))?;
        self.mock.write(bytes).await?;
        if let Some((trigger, flag)) = &self.cancellation
            && bytes == trigger
        {
            flag.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        if self
            .error_after
            .as_ref()
            .is_some_and(|trigger| self.mock.writes().last() == Some(trigger))
        {
            let _consumed = self.error_after.take();
            return Err(TransportError::Read(io::Error::from_raw_os_error(6)));
        }
        self.mock.read(bytes).await
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

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        append(&self.log, Event::Baud(self.id, baud))?;
        self.mock.set_baud_rate(baud)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Event::Dropped(self.id));
        }
    }
}

#[derive(Debug)]
struct TestBackend {
    connections: VecDeque<Connection>,
    proof: Arc<Proof>,
    log: Log,
    opens: usize,
    elapsed: Duration,
    endpoint: SerialCandidate,
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
        assert_eq!(baud, 9600, "the ordinary workflow has one fixed CAT baud");
        self.proof
            .before_open(self.opens, &self.log)
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
                source: io::Error::other("unexpected additional handle"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        append(&self.log, Event::Enumerate)?;
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
        path: "/dev/cu.menu-apply-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn panel_endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.menu-apply-panel".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_PANEL_PID),
    }
}

fn plan(noop: bool) -> Result<MenuUpdatePlan, TestError> {
    let mut format = vec![0x11; 40];
    *format.get_mut(2).ok_or("format guard missing")? = 0;
    let mut control = vec![0xA5; 256];
    *control.get_mut(9).ok_or("PM guard missing")? = 0;
    control
        .get_mut(10..42)
        .ok_or("PM name spans missing")?
        .fill(0);
    let mut gateway = vec![0x5A; 256];
    *gateway.first_mut().ok_or("Gateway guard missing")? = 0;
    gateway.get_mut(8..16).ok_or("MY field missing")?.fill(0);
    let captured = [
        (8, format),
        (0x04_F000, control),
        (0x05_1000, gateway),
        (0x05_A500, vec![0xA5; 256]),
    ]
    .into_iter()
    .map(|(address, bytes)| Ok((Page::new(Address::new(address)?, bytes.len())?, bytes)))
    .collect::<Result<Vec<_>, kenwood_tmd750::error::ValidationError>>()?;
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    Ok(MenuUpdatePlan::new(
        &identity,
        &MenuFieldSnapshot::from_pages(captured)?,
        vec![
            MenuAssignment::new("pm.PmName1", None, "")?,
            MenuAssignment::new("pm.PmName2", None, if noop { "" } else { "BASE" })?,
            MenuAssignment::new(
                "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
                Some(SlotIndex::new(0)?),
                if noop { "" } else { "KQ4NIT" },
            )?,
            MenuAssignment::new(
                "radio.TxEqualizerFmNfm",
                Some(SlotIndex::new(5)?),
                if noop { "off" } else { "on" },
            )?,
        ],
    )?)
}

fn fresh_script(firmware: &[u8], gateway: &[u8]) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", firmware);
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", gateway);
    mock
}

fn entry_script() -> MockTransport {
    let mut mock = fresh_script(b"FV 1.02\r", b"GW 0\r");
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    mock
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

fn compared_script(plan: &MenuUpdatePlan) -> MockTransport {
    let mut mock = entry_script();
    for replacement in plan.replacements() {
        read(&mut mock, replacement.page(), replacement.expected());
    }
    mock
}

fn complete_script(plan: &MenuUpdatePlan) -> MockTransport {
    let mut mock = compared_script(plan);
    for replacement in plan
        .replacements()
        .iter()
        .filter(|replacement| !replacement.is_noop())
    {
        mock.expect(
            &frame(replacement.page(), replacement.replacement()),
            &[ACK],
        );
        read(&mut mock, replacement.page(), replacement.replacement());
    }
    mock.expect(b"E", &[ACK]);
    mock
}

struct Harness {
    _directory: tempfile::TempDir,
    plan: MenuUpdatePlan,
    backend: TestBackend,
    captures: Option<Captures>,
    journal: Recorder<File>,
    cancelled: Arc<AtomicBool>,
    capture_failed: Arc<AtomicBool>,
}

impl Harness {
    /// The main-unit endpoint with one fresh connection answering the identity
    /// tuple and Gateway Off.
    fn new(noop: bool) -> Result<Self, TestError> {
        Self::build(
            noop,
            endpoint(),
            vec![fresh_script(b"FV 1.02\r", b"GW 0\r")],
        )
    }

    /// `endpoint` with the complete apply script first, then `fresh` in order.
    fn build(
        noop: bool,
        endpoint: SerialCandidate,
        fresh: Vec<MockTransport>,
    ) -> Result<Self, TestError> {
        let directory = tempfile::tempdir()?;
        let plan = plan(noop)?;
        let capture_failed = Arc::new(AtomicBool::new(false));
        let reserve = |name| -> Result<Recorder<File>, TestError> {
            Ok(Recorder::named(
                create_private_file(&directory.path().join(name))?,
                Arc::clone(&capture_failed),
                name,
            ))
        };
        let captures = Captures {
            original: reserve("transcript.jsonl")?,
            post_exit: reserve("post-exit-transcript.jsonl")?,
        };
        let journal = reserve("menu-journal.jsonl")?;
        let proof = Arc::new(Proof {
            directory: directory.path().to_owned(),
            plan: plan.clone(),
            endpoint: endpoint.clone(),
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let connections = std::iter::once(complete_script(&plan))
            .chain(fresh)
            .enumerate()
            .map(|(id, mock)| Connection {
                id,
                mock,
                proof: Arc::clone(&proof),
                log: Arc::clone(&log),
                fail_close: false,
                error_after: None,
                cancellation: None,
            })
            .collect();
        Ok(Self {
            _directory: directory,
            plan,
            backend: TestBackend {
                connections,
                proof,
                log,
                opens: 0,
                elapsed: Duration::ZERO,
                endpoint,
            },
            captures: Some(captures),
            journal,
            cancelled: Arc::new(AtomicBool::new(false)),
            capture_failed,
        })
    }

    async fn run(&mut self) -> Result<WorkflowResult, TestError> {
        let endpoint = self.backend.endpoint.clone();
        Ok(run_workflow(
            &mut self.backend,
            &endpoint,
            &self.plan,
            self.captures
                .take()
                .ok_or("workflow captures already consumed")?,
            &mut self.journal,
            &self.cancelled,
        )
        .await)
    }

    fn connection(&mut self, id: usize) -> Result<&mut Connection, TestError> {
        self.backend
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
            .ok_or_else(|| "requested fixture handle missing".into())
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
            Event::Write(actual, bytes) if *actual == id => Some(bytes.as_slice()),
            _ => None,
        })
        .collect()
}

fn assert_released(events: &[Event], count: usize) {
    for id in 0..count {
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == Event::Close(id))
                .count(),
            1,
            "each opened handle must close exactly once"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == Event::Dropped(id))
                .count(),
            1,
            "each opened handle must be dropped exactly once"
        );
    }
}

#[tokio::test]
async fn mixed_update_uses_two_handles_and_complete_page_intents() -> TestResult {
    let mut harness = Harness::new(false)?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(),
        "the complete two-handle workflow must succeed: {result:?}"
    );
    assert_eq!(
        result.compared_pages.len(),
        harness.plan.replacements().len(),
        "every complete guard and target must be compared"
    );
    assert_eq!(
        result.verified_pages.len(),
        3,
        "only the three changed pages may be written"
    );
    assert_eq!(
        result.possible_pages, result.verified_pages,
        "acknowledged verified writes remain in the conservative dispatch journal"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "ordinary updates must not add an MCP re-entry verification handle"
    );
    let events = harness.events()?;
    assert_released(&events, 2);
    assert_eq!(
        writes(&events, 1),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
        "fresh verification must only read CAT state"
    );
    let journal = records(&harness.backend.proof.journal())?;
    assert_eq!(
        kind(journal.last().ok_or("final journal missing")?),
        Some("finished"),
        "final lifecycle evidence must be retained"
    );
    Ok(())
}

#[tokio::test]
async fn no_op_compares_every_guard_but_writes_no_pages() -> TestResult {
    let mut harness = Harness::new(true)?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(),
        "a fresh all-no-op comparison still requires clean exit and CAT: {result:?}"
    );
    assert!(
        result.possible_pages.is_empty() && result.verified_pages.is_empty(),
        "equal pages must not produce a write disposition"
    );
    let events = harness.events()?;
    assert!(
        !writes(&events, 0)
            .iter()
            .any(|bytes| bytes.first() == Some(&b'W')),
        "equal pages must never be dispatched"
    );
    assert!(
        !records(&harness.backend.proof.journal())?
            .iter()
            .any(|record| kind(record) == Some("before_write")),
        "no-op comparisons must not record write intent"
    );
    assert_released(&events, 2);
    Ok(())
}

#[tokio::test]
async fn stale_later_before_image_prevents_every_write_and_fresh_open() -> TestResult {
    let mut harness = Harness::new(false)?;
    let mut script = entry_script();
    for (index, replacement) in harness.plan.replacements().iter().enumerate() {
        let mut actual = replacement.expected().to_vec();
        if index + 1 == harness.plan.replacements().len() {
            *actual.last_mut().ok_or("page empty")? ^= 1;
        }
        read(&mut script, replacement.page(), &actual);
    }
    script.expect(b"E", &[ACK]);
    harness.connection(0)?.mock = script;
    let result = harness.run().await?;
    assert!(
        !result.succeeded() && result.operation_error.is_some(),
        "stale complete images must fail the update"
    );
    assert!(
        result.possible_pages.is_empty(),
        "all before-images must match before the first write"
    );
    assert!(
        matches!(result.exit, ExitDisposition::Acknowledged),
        "a complete comparison mismatch remains safe to exit"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "a refused update must not open another handle"
    );
    assert_released(&harness.events()?, 1);
    Ok(())
}

#[tokio::test]
async fn current_gateway_and_identity_refuse_programming_entry() -> TestResult {
    for identity_mismatch in [false, true] {
        let mut harness = Harness::new(false)?;
        let mut script = MockTransport::new();
        script.expect(b"ID\r", b"ID TM-D750\r");
        script.expect(
            b"FV\r",
            if identity_mismatch {
                b"FV 1.03\r"
            } else {
                b"FV 1.02\r"
            },
        );
        script.expect(b"TY\r", b"TY K,2,1\r");
        if !identity_mismatch {
            script.expect(b"GW\r", b"GW 2\r");
        }
        harness.connection(0)?.mock = script;
        let result = harness.run().await?;
        assert!(
            !result.succeeded() && result.operation_error.is_some(),
            "current CAT guards must block entry"
        );
        assert!(
            matches!(result.exit, ExitDisposition::NotEntered),
            "failed CAT guards must never enter or exit MCP"
        );
        assert_eq!(
            harness.backend.opens, 1,
            "failed original guards must not open a verifier"
        );
        assert_released(&harness.events()?, 1);
    }
    Ok(())
}

#[tokio::test]
async fn uncertain_write_retains_previous_verification_and_does_not_exit_or_retry() -> TestResult {
    let mut harness = Harness::new(false)?;
    let changed = harness
        .plan
        .replacements()
        .iter()
        .filter(|page| !page.is_noop())
        .collect::<Vec<_>>();
    let first = changed.first().ok_or("first changed page missing")?;
    let second = changed.get(1).ok_or("second changed page missing")?;
    let trigger = frame(second.page(), second.replacement());
    let mut script = compared_script(&harness.plan);
    script.expect(&frame(first.page(), first.replacement()), &[ACK]);
    read(&mut script, first.page(), first.replacement());
    script.expect(&trigger, b"");
    harness.connection(0)?.mock = script;
    harness.connection(0)?.error_after = Some(trigger);
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "an uncertain second write must fail the workflow"
    );
    assert_eq!(
        result.possible_pages.len(),
        2,
        "both the earlier verified and uncertain writes must remain recorded"
    );
    assert_eq!(
        result.verified_pages.len(),
        1,
        "the completed earlier readback must remain independently visible"
    );
    assert!(
        matches!(result.exit, ExitDisposition::RecoveryRequired),
        "incomplete ACK exchange must prohibit speculative exit"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "uncertain protocol state must not trigger reconnection"
    );
    assert_released(&harness.events()?, 1);
    Ok(())
}

#[tokio::test]
async fn cancellation_before_open_performs_no_radio_operations() -> TestResult {
    let mut harness = Harness::new(false)?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let result = harness.run().await?;
    assert!(
        !result.succeeded() && result.possible_pages.is_empty(),
        "pre-intent cancellation must not apply anything"
    );
    assert!(
        harness.events()?.is_empty(),
        "pre-open cancellation must not enumerate or obtain a handle"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_after_first_intent_finishes_the_approved_batch_and_cat() -> TestResult {
    let mut harness = Harness::new(false)?;
    let first = harness
        .plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("changed page missing")?;
    let cancellation = (
        frame(first.page(), first.replacement()),
        Arc::clone(&harness.cancelled),
    );
    harness.connection(0)?.cancellation = Some(cancellation);
    let result = harness.run().await?;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "the fixture must actually request cancellation after dispatch"
    );
    assert!(
        result.succeeded(),
        "accepted intent owes full batch completion and required verification: {result:?}"
    );
    assert_eq!(
        result.verified_pages.len(),
        3,
        "later approved changes must not be silently abandoned"
    );
    assert_released(&harness.events()?, 2);
    Ok(())
}

#[tokio::test]
async fn original_close_failure_prevents_fresh_open_without_hiding_readback() -> TestResult {
    let mut harness = Harness::new(false)?;
    harness.connection(0)?.fail_close = true;
    let result = harness.run().await?;
    assert!(
        !result.succeeded() && result.close_error.is_some(),
        "release failure must make the workflow incomplete"
    );
    assert_eq!(
        result.verified_pages.len(),
        3,
        "host cleanup failure must not discard actual readback evidence"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "unreleased original ownership must prohibit fresh opening"
    );
    assert_released(&harness.events()?, 1);
    Ok(())
}

#[tokio::test]
async fn fresh_gateway_mismatch_prevents_success_without_rollback() -> TestResult {
    let mut harness = Harness::new(false)?;
    harness.connection(1)?.mock = fresh_script(b"FV 1.02\r", b"GW 2\r");
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "fresh Gateway disagreement must not be reported as success"
    );
    assert!(
        matches!(
            result.post_exit.outcome(),
            VerificationOutcome::Failed {
                stage: VerificationStage::GatewayMismatch,
                ..
            }
        ),
        "fresh failure must retain its actual stage"
    );
    assert_eq!(
        result.verified_pages.len(),
        3,
        "Gateway mismatch does not imply written pages were restored"
    );
    assert_released(&harness.events()?, 2);
    Ok(())
}

#[test]
fn pre_write_synchronization_failure_is_sticky() -> TestResult {
    let directory = tempfile::tempdir()?;
    let mut synchronization =
        super::RawSynchronization::new(create_private_file(&directory.path().join("raw.jsonl"))?);
    synchronization.fail_next = true;
    assert!(
        synchronization.synchronize().is_err(),
        "injected synchronization failure must be observable"
    );
    synchronization.file.sync_all()?;
    assert!(
        synchronization.synchronize().is_err(),
        "a later successful OS sync must not erase the first failure"
    );
    Ok(())
}

#[tokio::test]
async fn failed_journal_blocks_the_first_open_independently_of_user_cancellation() -> TestResult {
    let mut harness = Harness::new(false)?;
    harness.journal = Recorder::named(
        File::open(harness.backend.proof.journal())?,
        Arc::clone(&harness.capture_failed),
        "menu-journal.jsonl",
    );
    let result = harness.run().await?;
    assert!(
        !result.succeeded() && result.journal_error.is_some(),
        "failed preparation evidence must block radio access"
    );
    assert!(
        !harness.cancelled.load(Ordering::Relaxed),
        "the test must not rely on the user cancellation flag to block opening"
    );
    assert!(
        harness.events()?.is_empty(),
        "journal failure must precede every hardware operation"
    );
    Ok(())
}

#[tokio::test]
async fn panel_endpoint_retries_a_silent_identity_then_requires_gateway_off() -> TestResult {
    let mut silent = MockTransport::new();
    silent.expect_hang(b"ID\r");
    let mut harness = Harness::build(
        false,
        panel_endpoint(),
        vec![silent, fresh_script(b"FV 1.02\r", b"GW 0\r")],
    )?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(),
        "the panel endpoint's bounded readiness check must succeed: {result:?}"
    );
    assert!(
        matches!(result.post_exit, PostExit::Readiness(_)),
        "the panel endpoint must use the bounded readiness check"
    );
    let (identity, mode) = result
        .post_exit
        .gateway_off_evidence()
        .ok_or("Gateway Off evidence missing")?;
    assert_eq!(
        identity,
        harness.plan.identity(),
        "evidence carries the fresh tuple"
    );
    assert_eq!(
        mode,
        kenwood_tmd750::DvGatewayMode::Off,
        "evidence carries Gateway Off"
    );
    assert_eq!(
        harness.backend.opens, 3,
        "one apply handle plus two readiness attempts"
    );
    let events = harness.events()?;
    assert_released(&events, 3);
    assert_eq!(
        writes(&events, 1),
        [b"ID\r".as_slice()],
        "the silent attempt sends ID only"
    );
    assert_eq!(
        writes(&events, 2),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
        "the matching attempt reads the tuple, then Gateway"
    );
    Ok(())
}

#[tokio::test]
async fn panel_endpoint_gateway_mismatch_is_terminal_after_a_matched_tuple() -> TestResult {
    let mut harness = Harness::build(
        false,
        panel_endpoint(),
        vec![
            fresh_script(b"FV 1.02\r", b"GW 2\r"),
            fresh_script(b"FV 1.02\r", b"GW 0\r"),
        ],
    )?;
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "a Terminal Gateway after the write must not be reported as success"
    );
    assert!(
        matches!(
            result.post_exit.outcome(),
            VerificationOutcome::Failed {
                stage: VerificationStage::GatewayMismatch,
                ..
            }
        ),
        "the readiness check retains the Gateway stage: {:?}",
        result.post_exit.outcome()
    );
    assert!(
        result.post_exit.gateway_off_evidence().is_none(),
        "a mismatch yields no evidence"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "a Gateway mismatch admits no further open"
    );
    assert_eq!(
        result.verified_pages.len(),
        3,
        "Gateway mismatch does not imply written pages were restored"
    );
    assert_released(&harness.events()?, 2);
    Ok(())
}
