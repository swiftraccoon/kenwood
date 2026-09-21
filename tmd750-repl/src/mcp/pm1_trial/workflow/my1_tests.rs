//! Tests for the whole MY1 workflow: the journal record written before the W
//! frame, and the page guards re-read on each fresh connection.

use super::*;
use crate::capture::{Artifacts, CaptureKind};
use crate::mcp::pm1_trial::my1_tests::trial;
use kenwood_tmd750::memory::MyCallsignTrial;
use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::TMD750_MAIN_PID;
use kenwood_tmd750::{Address, Page};
use kenwood_transport::{MockTransport, Transport, TransportError};
use serde_json::Value;
use std::collections::VecDeque;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type TestResult = crate::AppResult<()>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Open(usize),
    Write(usize, Vec<u8>),
    Close(usize),
    Drop(usize),
}

type Log = Arc<Mutex<Vec<Event>>>;

fn append(log: &Log, event: Event) -> Result<(), TransportError> {
    log.lock()
        .map_err(|_| TransportError::Read(io::Error::other("MY1 test log poisoned")))?
        .push(event);
    Ok(())
}

fn records(path: &Path) -> crate::AppResult<Vec<Value>> {
    std::fs::read_to_string(path)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[derive(Debug)]
struct JournalProof {
    path: PathBuf,
    original: Vec<u8>,
    expected: Vec<u8>,
    control: Vec<u8>,
}

impl JournalProof {
    fn before_write(&self, connection: usize, bytes: &[u8]) -> TestResult {
        assert!(
            matches!(connection, 0 | 2),
            "only MY1 rename and restore handles may issue W"
        );
        let records = records(&self.path)?;
        let intents: Vec<_> = records
            .iter()
            .filter(|record| record.get("kind").and_then(Value::as_str) == Some("write_intent"))
            .collect();
        let id = connection / 2 + 1;
        assert_eq!(
            intents.len(),
            id,
            "the exact intent must exist on disk before its W"
        );
        let intent = intents.last().ok_or("MY1 intent absent before dispatch")?;
        assert_eq!(
            intent.pointer("/evidence/session_id"),
            Some(&Value::from(id)),
            "journal session must match the current connection"
        );
        assert_eq!(
            intent.pointer("/evidence/intent_id"),
            Some(&Value::from(id)),
            "each W needs a distinct durable intent"
        );
        assert_eq!(
            intent.pointer("/evidence/write"),
            Some(&Value::from(if id == 1 { "rename" } else { "restore" })),
            "journal direction must match the actual W"
        );
        assert_eq!(
            intent.pointer("/evidence/restoration_status"),
            Some(&Value::from("possibly_changed")),
            "durable intent must retain the restoration obligation"
        );
        let scope = intent
            .pointer("/evidence/scope")
            .ok_or("MY1 scope missing")?;
        for (pointer, expected) in [
            ("/trial_kind", Value::from("pm_off_my1")),
            ("/field", Value::from("dstar-my-callsign-1")),
            ("/page_address", Value::from(331_776)),
            ("/page_length", Value::from(256)),
            ("/temporary_name", Value::from("KQ4NIT")),
            ("/identity/model", Value::from("TM-D750")),
            ("/identity/firmware", Value::from("1.02")),
            ("/identity/radio_type", Value::from("K,2,1")),
            ("/control_page/address", Value::from(323_584)),
            ("/control_page/length", Value::from(256)),
        ] {
            assert_eq!(
                scope.pointer(pointer),
                Some(&expected),
                "durable MY1 scope field {pointer} must match before W"
            );
        }
        assert_eq!(
            scope.get("original_page"),
            Some(&serde_json::to_value(&self.original)?),
            "all original target bytes must already be durable"
        );
        assert_eq!(
            scope.get("expected_page"),
            Some(&serde_json::to_value(&self.expected)?),
            "all temporary target bytes must already be durable"
        );
        assert_eq!(
            scope.pointer("/control_page/data"),
            Some(&serde_json::to_value(&self.control)?),
            "all immutable control bytes must already be durable"
        );
        let desired = if id == 1 {
            &self.expected
        } else {
            &self.original
        };
        assert_eq!(
            bytes,
            frame(MyCallsignTrial::required_page()?, desired),
            "actual fixed W must match the already-durable complete target image"
        );
        Ok(())
    }
}

#[derive(Debug)]
struct Connection {
    id: usize,
    script: MockTransport,
    log: Log,
    proof: Arc<JournalProof>,
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        append(&self.log, Event::Write(self.id, bytes.to_vec()))?;
        if bytes.first() == Some(&b'W') {
            self.proof
                .before_write(self.id, bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
        }
        self.script.write(bytes).await
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.script.read(buffer).await
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        append(&self.log, Event::Close(self.id))?;
        self.script.assert_complete();
        self.script.close().await
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut events) = self.log.lock() {
            events.push(Event::Drop(self.id));
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
            "only the explicit main-unit fixture endpoint may open"
        );
        assert_eq!(baud, 9600, "MY1 transport must retain its qualified baud");
        append(&self.log, Event::Open(self.opens))?;
        self.opens += 1;
        self.connections
            .pop_front()
            .ok_or_else(|| TransportError::Open {
                path: selected.path.clone(),
                source: io::Error::other("unexpected additional MY1 connection"),
            })
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
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
        path: "/dev/cu.fixed-my1-test".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn identity_script() -> MockTransport {
    let mut script = MockTransport::new();
    script.expect(b"ID\r", b"ID TM-D750\r");
    script.expect(b"FV\r", b"FV 1.02\r");
    script.expect(b"TY\r", b"TY K,2,1\r");
    script
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

fn read(script: &mut MockTransport, page: Page, bytes: &[u8]) {
    script.expect(&read_request(page), &frame(page, bytes));
    script.expect(&[ACK], &[ACK]);
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    None,
    GatewayActive,
    GatewayRejected,
    ControlDrift,
    TargetDrift,
}

fn mcp_script(
    trial: &MyCallsignTrial,
    phase: usize,
    fault: Fault,
) -> crate::AppResult<MockTransport> {
    let mut script = identity_script();
    match fault {
        Fault::GatewayActive => {
            script.expect(b"GW\r", b"GW 2\r");
            return Ok(script);
        }
        Fault::GatewayRejected => {
            script.expect(b"GW\r", b"?\r");
            return Ok(script);
        }
        Fault::None | Fault::ControlDrift | Fault::TargetDrift => script.expect(b"GW\r", b"GW 0\r"),
    }
    script.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut script, Page::new(Address::new(8)?, 40)?, &[0; 40]);
    let mut control = *trial.control_page();
    if matches!(fault, Fault::ControlDrift) {
        *control.get_mut(40).ok_or("control drift byte missing")? ^= 1;
    }
    read(&mut script, trial.control_page_spec(), &control);
    let mut before = if phase == 1 {
        *trial.expected_page()
    } else {
        *trial.original_page()
    };
    if matches!(fault, Fault::TargetDrift) {
        *before.get_mut(40).ok_or("target drift byte missing")? ^= 1;
    }
    read(&mut script, trial.page(), &before);
    if phase < 2 && matches!(fault, Fault::None) {
        let after = if phase == 0 {
            trial.expected_page()
        } else {
            trial.original_page()
        };
        script.expect(&frame(trial.page(), after), &[ACK]);
        read(&mut script, trial.page(), after);
    }
    script.expect(b"E", &[ACK]);
    Ok(script)
}

struct Harness {
    directory: tempfile::TempDir,
    trial: MyCallsignTrial,
    backend: TestBackend,
    journal: Journal,
    captures: Vec<SessionCaptures>,
}

impl Harness {
    fn new() -> crate::AppResult<Self> {
        let directory = tempfile::tempdir()?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
        let trial = trial()?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let proof = Arc::new(JournalProof {
            path: directory.path().join("trial-journal.jsonl"),
            original: trial.original_page().to_vec(),
            expected: trial.expected_page().to_vec(),
            control: trial.control_page().to_vec(),
        });
        let failed = Arc::new(AtomicBool::new(false));
        let mut connections = VecDeque::new();
        let mut captures = Vec::new();
        for phase in 0..3 {
            for script in [mcp_script(&trial, phase, Fault::None)?, identity_script()] {
                connections.push_back(Connection {
                    id: connections.len(),
                    script,
                    log: Arc::clone(&log),
                    proof: Arc::clone(&proof),
                });
            }
            let artifacts = Artifacts::create(
                CaptureKind::Mcp,
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
        journal.prepare(&trial, &directory.path().join("source-backup.json"), "")?;
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
            &AtomicBool::new(false),
        )
        .await
    }

    fn fault(&mut self, phase: usize, fault: Fault) -> TestResult {
        let script = mcp_script(&self.trial, phase, fault)?;
        self.backend
            .connections
            .get_mut(phase * 2)
            .ok_or("MY1 connection missing")?
            .script = script;
        Ok(())
    }

    fn events(&self) -> crate::AppResult<Vec<Event>> {
        Ok(self
            .backend
            .log
            .lock()
            .map_err(|_| "MY1 test log poisoned")?
            .clone())
    }
}

fn writes(events: &[Event], connection: usize) -> Vec<&[u8]> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Write(id, bytes) if *id == connection => Some(bytes.as_slice()),
            _ => None,
        })
        .collect()
}

fn position(events: &[Event], expected: &Event) -> crate::AppResult<usize> {
    events
        .iter()
        .position(|event| event == expected)
        .ok_or_else(|| format!("missing MY1 lifecycle event {expected:?}").into())
}

fn memory_writes(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'W')))
        .count()
}

#[tokio::test]
async fn my1_success_has_three_fresh_guards_eleven_reads_two_durable_writes_and_six_handles()
-> TestResult {
    let mut harness = Harness::new()?;
    let result = harness.run().await;
    assert!(
        result.succeeded(&harness.trial),
        "MY1 workflow must complete every session: {result:?}"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::RestorationVerified,
        "only the third finalized read session clears restoration"
    );
    assert_eq!(
        harness.backend.opens, 6,
        "every MCP session requires an independently opened fresh CAT handle"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        2,
        "only the approved fixed MY1 replacement and exact restore may write"
    );
    for id in 0..6 {
        assert!(
            position(&events, &Event::Close(id))? < position(&events, &Event::Drop(id))?,
            "each handle must close before release"
        );
        if id < 5 {
            assert!(
                position(&events, &Event::Drop(id))? < position(&events, &Event::Open(id + 1))?,
                "retire old handles before any new connection"
            );
        }
        let commands = writes(&events, id);
        if id % 2 == 0 {
            assert_eq!(
                commands.get(..4),
                Some([b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"].as_slice()),
                "each original session independently queries Gateway Off before MCP"
            );
            assert_eq!(
                commands.last(),
                Some(&b"E".as_slice()),
                "E must be the last command on each MCP handle"
            );
        } else {
            assert_eq!(
                commands,
                [b"ID\r".as_slice(), b"FV\r", b"TY\r"],
                "fresh post-exit CAT remains identity-only"
            );
        }
    }
    let json = serde_json::to_value(&result)?;
    let mut segment_count = 0;
    for index in 0..3 {
        let core = json
            .pointer(&format!("/sessions/{index}/core"))
            .ok_or("MY1 core evidence missing")?;
        assert_eq!(
            core.get("gateway_mode"),
            Some(&serde_json::json!({"state":"off","raw":0})),
            "each session must retain its independently observed Gateway Off"
        );
        segment_count += core
            .get("segments")
            .and_then(Value::as_array)
            .ok_or("MY1 captured segments missing")?
            .len();
    }
    assert_eq!(
        segment_count, 11,
        "format and full control/target reads in all sessions plus two immediate target readbacks"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'R')))
            .count(),
        11,
        "saved read segments must correspond to exactly eleven wire requests"
    );
    assert!(events.iter().all(|event| !matches!(event, Event::Write(_, bytes) if bytes.first() == Some(&b'Z') || bytes.starts_with(b"TX") || bytes.starts_with(b"PM ") || bytes.starts_with(b"GW "))), "no fill, RF, PM recall, or Gateway setter may be sent");
    harness.journal.finish(&harness.trial)?;
    let journal = records(&harness.directory.path().join("trial-journal.jsonl"))?;
    assert_eq!(
        journal.len(),
        7,
        "retain prepared scope, two intents, three sessions, and final restoration evidence"
    );
    assert_eq!(
        journal
            .last()
            .and_then(|record| record.pointer("/evidence/status")),
        Some(&Value::from("restoration_verified")),
        "durable final status must follow the third completed session"
    );
    Ok(())
}

#[tokio::test]
async fn my1_non_off_gateway_refuses_entry_and_all_writes() -> TestResult {
    let mut harness = Harness::new()?;
    harness.fault(0, Fault::GatewayActive)?;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "a non-Off gateway cannot admit MY1 programming"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::NotWritten,
        "pre-entry refusal creates no write obligation"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "do not reconnect after refusing entry"
    );
    let events = harness.events()?;
    assert_eq!(
        writes(&events, 0),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
        "non-Off gateway must prevent entry, E, and every memory command"
    );
    let json = serde_json::to_value(&result)?;
    assert_eq!(
        json.pointer("/sessions/0/core/gateway_mode"),
        Some(&serde_json::json!({"state":"terminal","raw":2})),
        "retain the raw disallowed gateway value without assigning unqualified semantics"
    );
    assert_eq!(
        json.pointer("/sessions/0/core/outcome/stage/kind"),
        Some(&Value::from("gateway_guard")),
        "failure must identify the live Gateway guard"
    );
    Ok(())
}

#[tokio::test]
async fn my1_rejected_gateway_query_is_not_treated_as_off() -> TestResult {
    let mut harness = Harness::new()?;
    harness.fault(0, Fault::GatewayRejected)?;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "an unproved Gateway state cannot admit MCP"
    );
    assert_eq!(
        harness.backend.opens, 1,
        "missing guard proof must stop before any second handle"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::NotWritten,
        "no write was attempted"
    );
    assert_eq!(
        writes(&harness.events()?, 0),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
        "no command beyond the rejected guard is allowed"
    );
    let json = serde_json::to_value(&result)?;
    assert!(
        json.pointer("/sessions/0/core/gateway_mode").is_none(),
        "an absent observation must not become fabricated Gateway Off evidence"
    );
    assert_eq!(
        json.pointer("/sessions/0/core/outcome/stage/kind"),
        Some(&Value::from("gateway_guard")),
        "retain the exact guard failure stage"
    );
    Ok(())
}

#[tokio::test]
async fn my1_gateway_change_before_restore_preserves_the_write_obligation() -> TestResult {
    let mut harness = Harness::new()?;
    harness.fault(1, Fault::GatewayActive)?;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "Gateway must be checked again before the restoration session"
    );
    assert_eq!(
        harness.backend.opens, 3,
        "late guard refusal must stop on the restore-session handle"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "a previously renamed MY1 must retain restoration-required status"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        1,
        "do not blindly restore through an active Gateway"
    );
    assert_eq!(
        writes(&events, 2),
        [b"ID\r".as_slice(), b"FV\r", b"TY\r", b"GW\r"],
        "the second session must stop before MCP entry"
    );
    Ok(())
}

#[tokio::test]
async fn my1_full_control_drift_prevents_restore_even_when_cat_recovers() -> TestResult {
    let mut harness = Harness::new()?;
    harness.fault(1, Fault::ControlDrift)?;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "unrelated control-page drift must block restoration"
    );
    assert_eq!(
        harness.backend.opens, 4,
        "allow safe exit and fresh CAT but no third MCP session"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "fresh CAT cannot clear an unresolved replacement"
    );
    assert_eq!(
        memory_writes(&harness.events()?),
        1,
        "no restore may proceed against changed control bytes"
    );
    let session = result.sessions.get(1).ok_or("second session missing")?;
    assert!(
        session.post_exit.succeeded(),
        "a synchronized refused session may still recover CAT"
    );
    assert!(
        matches!(
            session
                .core
                .as_ref()
                .ok_or("guard failure evidence missing")?
                .outcome,
            Outcome::Failed {
                stage: Stage::FreshComparison,
                ..
            }
        ),
        "report the full fresh-control comparison failure"
    );
    Ok(())
}

#[tokio::test]
async fn my1_final_target_drift_never_claims_restoration_or_dispatches_a_third_write() -> TestResult
{
    let mut harness = Harness::new()?;
    harness.fault(2, Fault::TargetDrift)?;
    let result = harness.run().await;
    assert!(
        !result.succeeded(&harness.trial),
        "the final full-page proof must be independently checked"
    );
    assert_eq!(
        harness.backend.opens, 6,
        "the final synchronized failure still requires clean fresh CAT recovery"
    );
    assert_eq!(
        harness.trial.status(),
        PmNameTrialStatus::PossiblyChanged,
        "immediate restore readback alone does not establish separate-session restoration"
    );
    let events = harness.events()?;
    assert_eq!(
        memory_writes(&events),
        2,
        "never issue a speculative third MY1 write"
    );
    assert!(
        writes(&events, 4)
            .iter()
            .all(|bytes| bytes.first() != Some(&b'W')),
        "final verification remains read-only on mismatch"
    );
    let last = result.sessions.last().ok_or("third session missing")?;
    assert!(
        last.post_exit.succeeded(),
        "CAT liveness is independent of restored-page equality"
    );
    assert!(
        matches!(
            last.core
                .as_ref()
                .ok_or("final MY1 evidence missing")?
                .outcome,
            Outcome::Failed {
                stage: Stage::FreshComparison,
                ..
            }
        ),
        "retain the separate-session target mismatch"
    );
    Ok(())
}
