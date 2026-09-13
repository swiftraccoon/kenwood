//! Fixed re-entry ordering and retained evidence using only scripted handles.

use std::collections::VecDeque;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, ENTER, EXIT, read_request, write_request};
use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID};
use kenwood_tmd750::{Address, Page};
use kenwood_transport::{MockTransport, Transport, TransportError};
use serde_json::Value;

use super::{Captures, JournalEvent, Workflow, run};
use crate::capture::{Recorder, create_private_file};
use crate::mcp::reconnect::Backend;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Observation {
    Open(usize),
    Write(usize, Vec<u8>),
    Baud(usize, u32),
    Close(usize),
    Dropped(usize),
    Enumerate,
}

type Log = Arc<Mutex<Vec<Observation>>>;

fn append(log: &Log, event: Observation) -> Result<(), TransportError> {
    log.lock()
        .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?
        .push(event);
    Ok(())
}

fn records(path: &Path) -> Result<Vec<Value>, TestError> {
    let text = std::fs::read_to_string(path)?;
    assert!(
        text.ends_with('\n'),
        "completed records must retain their line boundary"
    );
    let records = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(
            record.get("sequence"),
            Some(&serde_json::json!(index)),
            "capture sequence must be contiguous"
        );
        let timestamp = record
            .get("utc_unix_nanoseconds")
            .and_then(Value::as_str)
            .ok_or("timestamp missing")?
            .parse::<i128>()?;
        assert!(
            timestamp > 0,
            "retain an actual timestamp for every observation"
        );
    }
    Ok(records)
}

fn kind(record: &Value) -> Option<&str> {
    record.pointer("/event/kind").and_then(Value::as_str)
}

fn transcript_name(id: usize) -> &'static str {
    match id {
        0 => "transcript.jsonl",
        1 => "post-exit-transcript.jsonl",
        2 => "session-2-transcript.jsonl",
        _ => "session-2-post-exit-transcript.jsonl",
    }
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.fixed-reentry-test".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn pages() -> Result<[Page; 2], TestError> {
    Ok([
        Page::new(Address::new(8)?, 40)?,
        Page::new(Address::new(327_681)?, 255)?,
    ])
}

fn fresh_script(gateway: u8) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    mock
}

fn original_script() -> Result<MockTransport, TestError> {
    let mut mock = fresh_script(0);
    mock.expect(ENTER, b"0M\r");
    for (page, fill) in pages()?.into_iter().zip([0x11, 0x22]) {
        let mut response = write_request(page).to_vec();
        response.extend(vec![fill; page.len()]);
        mock.expect(&read_request(page), &response);
        mock.expect(&[ACK], &[ACK]);
    }
    mock.expect(&[EXIT], &[ACK]);
    Ok(mock)
}

#[derive(Debug)]
struct Connection {
    id: usize,
    mock: MockTransport,
    directory: PathBuf,
    log: Log,
    entry_error: Option<io::Error>,
    close_error: bool,
    cancel_on_exit: Option<Arc<AtomicBool>>,
}

impl Connection {
    fn before_first_write(&self, bytes: &[u8]) -> TestResult {
        let records = records(&self.directory.join(transcript_name(self.id)))?;
        let command = records.last().ok_or("first command capture missing")?;
        assert_eq!(
            kind(command),
            Some("write_requested"),
            "request must be recorded before dispatch"
        );
        assert_eq!(
            command.pointer("/event/bytes"),
            Some(&serde_json::json!(bytes)),
            "capture must contain actual dispatched bytes"
        );
        assert_eq!(bytes, b"ID\r", "every new handle must begin with identity");
        if self.id.is_multiple_of(2) {
            assert_eq!(
                records.len(),
                3,
                "original open request and completion precede first CAT request"
            );
            assert_eq!(
                records.first().and_then(kind),
                Some("open_requested"),
                "opening intent must precede CAT dispatch"
            );
            assert_eq!(
                records.get(1).and_then(kind),
                Some("open_completed"),
                "opening completion must precede CAT dispatch"
            );
        } else {
            assert!(
                records
                    .iter()
                    .any(|record| kind(record) == Some("open_completed")),
                "fresh open completion must precede CAT"
            );
        }
        Ok(())
    }
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.mock.writes().is_empty() {
            self.before_first_write(bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        append(&self.log, Observation::Write(self.id, bytes.to_vec()))?;
        self.mock.write(bytes).await?;
        if bytes == [EXIT]
            && let Some(cancelled) = &self.cancel_on_exit
        {
            cancelled.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        if self
            .mock
            .writes()
            .last()
            .is_some_and(|request| request == ENTER)
            && let Some(error) = self.entry_error.take()
        {
            return Err(TransportError::Read(error));
        }
        self.mock.read(bytes).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        append(&self.log, Observation::Close(self.id))?;
        self.mock.assert_complete();
        if self.close_error {
            return Err(TransportError::Disconnected(io::Error::other(
                "scripted close failure",
            )));
        }
        self.mock.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        append(&self.log, Observation::Baud(self.id, baud))?;
        self.mock.set_baud_rate(baud)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut log) = self.log.lock() {
            log.push(Observation::Dropped(self.id));
        }
    }
}

#[derive(Debug)]
struct TestBackend {
    connections: VecDeque<Connection>,
    directory: PathBuf,
    log: Log,
    opens: usize,
    fail_open: Option<usize>,
    elapsed: Duration,
}

impl TestBackend {
    fn before_open(&self) -> TestResult {
        if self.opens.is_multiple_of(2) {
            let original = records(&self.directory.join(transcript_name(self.opens)))?;
            assert_eq!(
                original.len(),
                1,
                "original opening request must already be retained"
            );
            let request = original.first().ok_or("opening request missing")?;
            assert_eq!(
                kind(request),
                Some("open_requested"),
                "record the opening request before acquiring a handle"
            );
            assert_eq!(
                request.pointer("/event/path").and_then(Value::as_str),
                Some(endpoint().path.as_str()),
                "the recorded endpoint must equal the selected endpoint"
            );
            assert_eq!(
                request.pointer("/event/baud").and_then(Value::as_u64),
                Some(9600),
                "the recorded baud must equal the selected baud"
            );
        }
        if self.opens > 0 {
            let log = self
                .log
                .lock()
                .map_err(|error| io::Error::other(error.to_string()))?;
            assert!(
                log.contains(&Observation::Close(self.opens - 1)),
                "prior handle must close before another opens"
            );
            assert!(
                log.contains(&Observation::Dropped(self.opens - 1)),
                "prior handle must be dropped before another opens"
            );
            drop(log);
        }
        if self.opens == 2 {
            self.first_session_is_durable()?;
        }
        Ok(())
    }

    fn first_session_is_durable(&self) -> TestResult {
        let journal = records(&self.directory.join("session-journal.jsonl"))?;
        let [prepared, finished] = journal.as_slice() else {
            return Err(
                "exactly preparation and first-session evidence must precede second entry".into(),
            );
        };
        assert_eq!(
            kind(prepared),
            Some("prepared"),
            "the fixed scope must be prepared first"
        );
        assert_eq!(
            kind(finished),
            Some("session_finished"),
            "the completed first session must be retained before re-entry"
        );
        assert_eq!(
            finished.pointer("/event/number").and_then(Value::as_u64),
            Some(1),
            "the retained evidence belongs to the first session"
        );
        assert_eq!(
            finished
                .pointer("/event/evidence/gateway_mode")
                .and_then(Value::as_u64),
            Some(0),
            "the first session must retain its pre-entry Gateway Off observation"
        );
        assert_eq!(
            finished
                .pointer("/event/evidence/post_exit/outcome/status")
                .and_then(Value::as_str),
            Some("matched"),
            "fresh verification must have completed before the next original open"
        );
        assert_eq!(
            finished
                .pointer("/event/evidence/post_exit/attempt/gateway_mode/raw")
                .and_then(Value::as_u64),
            Some(0),
            "retain the actual fresh Gateway Off reply rather than reusing the pre-entry value"
        );
        for id in [0, 1] {
            let transcript = records(&self.directory.join(transcript_name(id)))?;
            assert_eq!(
                transcript.last().and_then(kind),
                Some("close_completed"),
                "both first-session transcripts must retain the final close"
            );
        }
        Ok(())
    }
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
            "every opening must retain the explicit endpoint"
        );
        assert_eq!(baud, 9600, "every opening must retain the required baud");
        self.before_open().map_err(|error| TransportError::Open {
            path: selected.path.clone(),
            source: io::Error::other(error),
        })?;
        let id = self.opens;
        append(&self.log, Observation::Open(id))?;
        self.opens += 1;
        if self.fail_open == Some(id) {
            return Err(TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("scripted opening failure"),
            });
        }
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected extra opening"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        append(&self.log, Observation::Enumerate)?;
        Ok(vec![endpoint()])
    }

    fn now(&self) -> Duration {
        self.elapsed
    }

    async fn wait(&mut self, duration: Duration) {
        self.elapsed = self.elapsed.saturating_add(duration);
    }
}

struct Harness {
    directory: tempfile::TempDir,
    backend: TestBackend,
    captures: Option<[Captures; 2]>,
    journal: Recorder<File>,
    cancelled: Arc<AtomicBool>,
    log: Log,
}

impl Harness {
    fn new() -> Result<Self, TestError> {
        let directory = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let reserve = |name| -> Result<Recorder<File>, TestError> {
            Ok(Recorder::named(
                create_private_file(&directory.path().join(name))?,
                Arc::clone(&cancelled),
                name,
            ))
        };
        let captures = [
            Captures {
                original: reserve(transcript_name(0))?,
                post_exit: reserve(transcript_name(1))?,
            },
            Captures {
                original: reserve(transcript_name(2))?,
                post_exit: reserve(transcript_name(3))?,
            },
        ];
        let mut journal = reserve("session-journal.jsonl")?;
        journal.record(JournalEvent::Prepared {
            scope: "fixed mock read-only re-entry pair",
        });
        journal.synchronize()?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut connections = VecDeque::new();
        for id in 0_usize..4 {
            connections.push_back(Connection {
                id,
                mock: if id.is_multiple_of(2) {
                    original_script()?
                } else {
                    fresh_script(0)
                },
                directory: directory.path().to_path_buf(),
                log: Arc::clone(&log),
                entry_error: None,
                close_error: false,
                cancel_on_exit: None,
            });
        }
        let backend = TestBackend {
            connections,
            directory: directory.path().to_path_buf(),
            log: Arc::clone(&log),
            opens: 0,
            fail_open: None,
            elapsed: Duration::ZERO,
        };
        Ok(Self {
            directory,
            backend,
            captures: Some(captures),
            journal,
            cancelled,
            log,
        })
    }

    fn connection(&mut self, id: usize) -> Result<&mut Connection, TestError> {
        self.backend
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
            .ok_or_else(|| "requested fixture connection missing".into())
    }

    async fn run(&mut self) -> Result<Workflow, TestError> {
        Ok(run(
            &mut self.backend,
            &endpoint(),
            9600,
            self.captures
                .take()
                .ok_or("workflow captures already consumed")?,
            &mut self.journal,
            &self.cancelled,
        )
        .await)
    }

    fn events(&self) -> Result<Vec<Observation>, TestError> {
        Ok(self
            .log
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .clone())
    }
}

fn writes(log: &[Observation], id: usize) -> Vec<&[u8]> {
    log.iter()
        .filter_map(|event| match event {
            Observation::Write(actual, bytes) if *actual == id => Some(bytes.as_slice()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn two_fixed_pairs_record_each_open_and_finish_evidence_before_the_next_pair() -> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await?;
    assert!(
        result.succeeded(),
        "all four mock handles must complete: {result:?}"
    );
    assert_eq!(harness.backend.opens, 4);
    assert_eq!(result.sessions.len(), 2);
    let log = harness.events()?;
    let [global, slot] = pages()?;
    for id in [0, 2] {
        assert_eq!(
            writes(&log, id),
            [
                b"ID\r".as_slice(),
                b"FV\r",
                b"TY\r",
                b"GW\r",
                ENTER,
                &read_request(global),
                &[ACK],
                &read_request(slot),
                &[ACK],
                &[EXIT],
            ],
            "original handles have only the fixed acknowledged read schedule"
        );
        assert_eq!(
            log.iter()
                .filter(|event| matches!(event, Observation::Baud(actual, _) if *actual == id))
                .count(),
            1,
            "only entry changes baud, never post-exit"
        );
    }
    for id in [1, 3] {
        assert_eq!(
            writes(&log, id),
            [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
            "fresh verification is exactly four read-only queries"
        );
    }
    assert_eq!(
        log.last(),
        Some(&Observation::Dropped(3)),
        "all handles must be released"
    );
    let journal = records(&harness.directory.path().join("session-journal.jsonl"))?;
    assert_eq!(
        journal.len(),
        3,
        "preparation plus both complete session records"
    );
    assert_eq!(journal.last().and_then(kind), Some("session_finished"));
    Ok(())
}

#[tokio::test]
async fn second_entry_enxio_preserves_first_pair_but_never_exits_reconnects_or_retries()
-> TestResult {
    let mut harness = Harness::new()?;
    let mut script = fresh_script(0);
    script.expect(ENTER, b"");
    let connection = harness.connection(2)?;
    connection.mock = script;
    connection.entry_error = Some(io::Error::from_raw_os_error(6));
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "prior fresh Off is not a completed second lifecycle"
    );
    assert_eq!(harness.backend.opens, 3);
    assert!(
        result
            .sessions
            .first()
            .is_some_and(super::Session::succeeded),
        "the completed first pair remains evidence"
    );
    let failed = result.sessions.get(1).ok_or("second session missing")?;
    assert!(
        failed.post_exit.gateway_off_evidence().is_none(),
        "no old Off observation may be reassigned"
    );
    let encoded = serde_json::to_value(failed)?;
    assert_eq!(
        encoded
            .pointer("/probe/outcome/stage")
            .and_then(Value::as_str),
        Some("entry")
    );
    assert_eq!(
        encoded.pointer("/probe/exit").and_then(Value::as_str),
        Some("recovery_required")
    );
    let log = harness.events()?;
    assert_eq!(
        writes(&log, 2),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r", ENTER],
        "uncertain entry permits no page, E, CAT, or retry"
    );
    let after_entry = log
        .iter()
        .rposition(|event| matches!(event, Observation::Write(2, _)))
        .ok_or("entry request missing")?
        + 1;
    assert_eq!(
        log.get(after_entry..),
        Some([Observation::Close(2), Observation::Dropped(2)].as_slice()),
        "only close and drop may follow failed entry"
    );
    Ok(())
}

#[tokio::test]
async fn original_or_fresh_close_failure_prevents_the_second_original_open() -> TestResult {
    for id in [0, 1] {
        let mut harness = Harness::new()?;
        harness.connection(id)?.close_error = true;
        let result = harness.run().await?;
        assert!(!result.succeeded(), "a failed close cannot qualify a pair");
        assert_eq!(harness.backend.opens, id + 1);
        let log = harness.events()?;
        assert_eq!(
            log.last(),
            Some(&Observation::Dropped(id)),
            "failed close still releases ownership"
        );
        assert_eq!(result.sessions.len(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn fresh_gateway_mismatch_is_retained_and_prevents_the_next_original_open() -> TestResult {
    let mut harness = Harness::new()?;
    harness.connection(1)?.mock = fresh_script(2);
    let result = harness.run().await?;
    assert!(!result.succeeded(), "fresh Terminal is not fresh Off");
    assert_eq!(harness.backend.opens, 2);
    let session = result.sessions.first().ok_or("first pair missing")?;
    assert!(
        session.post_exit.gateway_off_evidence().is_none(),
        "mismatch cannot be relabeled Off"
    );
    let encoded = serde_json::to_value(session)?;
    assert_eq!(
        encoded
            .pointer("/post_exit/attempt/gateway_mode/raw")
            .and_then(Value::as_u64),
        Some(2)
    );
    assert_eq!(
        harness.events()?.last(),
        Some(&Observation::Dropped(1)),
        "mismatched fresh handle still closes and drops"
    );
    Ok(())
}

#[tokio::test]
async fn original_capture_failure_before_open_prevents_all_radio_access() -> TestResult {
    let mut harness = Harness::new()?;
    let path = harness.directory.path().join(transcript_name(0));
    let capture_failed = Arc::new(AtomicBool::new(false));
    let captures = harness.captures.as_mut().ok_or("captures missing")?;
    captures
        .first_mut()
        .ok_or("first captures missing")?
        .original = Recorder::named(
        File::open(path)?,
        Arc::clone(&capture_failed),
        transcript_name(0),
    );
    let result = harness.run().await?;
    assert!(
        capture_failed.load(Ordering::Relaxed),
        "the read-only file must cause actual recording failure"
    );
    assert!(
        !result.succeeded(),
        "incomplete capture cannot authorize any open"
    );
    assert_eq!(harness.backend.opens, 0);
    assert!(
        harness.events()?.is_empty(),
        "capture admission fails before transport activity"
    );
    assert!(
        result
            .sessions
            .first()
            .is_some_and(|session| session.synchronization_error.is_some()),
        "retain the actual capture failure"
    );
    let encoded = serde_json::to_value(&result)?;
    assert_eq!(
        encoded
            .pointer("/sessions/0/post_exit/outcome/reason")
            .and_then(Value::as_str),
        Some("original_capture_incomplete"),
        "capture failure is not operator cancellation"
    );
    Ok(())
}

#[tokio::test]
async fn failed_session_journal_prevents_a_second_original_open() -> TestResult {
    let mut harness = Harness::new()?;
    let capture_failed = Arc::new(AtomicBool::new(false));
    harness.journal = Recorder::named(
        File::open(harness.directory.path().join("session-journal.jsonl"))?,
        Arc::clone(&capture_failed),
        "session-journal.jsonl",
    );
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "successful radio exchanges cannot replace required session evidence"
    );
    assert!(
        capture_failed.load(Ordering::Relaxed),
        "the read-only journal must fail actual recording"
    );
    assert!(
        result.synchronization_error.is_some(),
        "retain the journal error"
    );
    assert_eq!(
        harness.backend.opens, 2,
        "no second original open may follow failed journal recording"
    );
    assert_eq!(
        harness.events()?.last(),
        Some(&Observation::Dropped(1)),
        "both first-session handles are still released"
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_before_start_opens_nothing_and_keeps_only_preparation() -> TestResult {
    let mut harness = Harness::new()?;
    harness.cancelled.store(true, Ordering::Relaxed);
    let result = harness.run().await?;
    assert!(!result.succeeded(), "a cancelled pair was not executed");
    assert_eq!(harness.backend.opens, 0);
    assert!(
        result.sessions.is_empty(),
        "no session should be fabricated"
    );
    assert!(
        harness.events()?.is_empty(),
        "cancellation prohibits opening"
    );
    assert_eq!(
        records(&harness.directory.path().join("session-journal.jsonl"))?.len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_during_exit_finishes_ack_and_close_but_prohibits_a_fresh_open() -> TestResult
{
    let mut harness = Harness::new()?;
    harness.connection(0)?.cancel_on_exit = Some(Arc::clone(&harness.cancelled));
    let result = harness.run().await?;
    assert!(
        harness.cancelled.load(Ordering::Relaxed),
        "the callback must actually request cancellation"
    );
    assert!(
        !result.succeeded(),
        "cancelled fresh verification leaves the control incomplete"
    );
    assert_eq!(harness.backend.opens, 1);
    let session = result.sessions.first().ok_or("first session missing")?;
    assert!(
        session.post_exit.gateway_off_evidence().is_none(),
        "no fresh check is fabricated after cancellation"
    );
    let encoded = serde_json::to_value(session)?;
    assert_eq!(
        encoded.pointer("/probe/exit").and_then(Value::as_str),
        Some("acknowledged")
    );
    let log = harness.events()?;
    assert_eq!(writes(&log, 0).last().copied(), Some([EXIT].as_slice()));
    assert_eq!(
        log.last(),
        Some(&Observation::Dropped(0)),
        "awaited exit still closes and drops"
    );
    assert!(
        !log.contains(&Observation::Enumerate),
        "cancellation must prevent any fresh-open workflow"
    );
    Ok(())
}

#[tokio::test]
async fn opening_failure_retains_error_without_protocol_or_phantom_close() -> TestResult {
    let mut harness = Harness::new()?;
    harness.backend.fail_open = Some(0);
    let result = harness.run().await?;
    assert!(
        !result.succeeded(),
        "failed opening cannot qualify a session"
    );
    assert_eq!(harness.backend.opens, 1);
    assert_eq!(
        harness.events()?,
        [Observation::Open(0)],
        "there is no handle to close or use"
    );
    let session = result.sessions.first().ok_or("failed session missing")?;
    assert!(
        session.open_error.is_some(),
        "retain the actual opening failure"
    );
    assert!(
        session.close_error.is_none(),
        "do not fabricate a failed close"
    );
    let capture = records(&harness.directory.path().join(transcript_name(0)))?;
    assert_eq!(capture.len(), 2);
    assert_eq!(capture.last().and_then(kind), Some("open_failed"));
    Ok(())
}
