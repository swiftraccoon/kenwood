//! Managed lifecycle coverage over a fake backend: Terminal entry, the probe,
//! and restoration.

mod failures;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};
use kenwood_tmd750::{
    Address, FirmwareIdentity, Identity, MenuFieldSnapshot, Page, RadioModel, RadioType,
};
use kenwood_transport::{MockTransport, Transport};
use serde_json::{Value, json};

use super::*;

type TestResult = AppResult<()>;

const FAST: Limits = Limits {
    cat_io_step: Duration::from_millis(20),
    binary_total: Duration::from_millis(80),
    close: Duration::from_millis(20),
};

fn endpoints() -> Endpoints {
    let candidate = |path: &str, pid| SerialCandidate {
        path: path.to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(pid),
    };
    Endpoints {
        control: candidate("/dev/cu.main", TMD750_MAIN_PID),
        modem: candidate("/dev/cu.panel", TMD750_PANEL_PID),
    }
}

fn plan(before: u8) -> AppResult<TerminalPlan> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut pages = vec![
        (Page::new(Address::new(8)?, 40)?, vec![0xA5; 40]),
        (Page::new(Address::new(323_584)?, 256)?, vec![0xA5; 256]),
        (Page::new(Address::new(328_960)?, 256)?, vec![0xA5; 256]),
        (Page::new(Address::new(331_776)?, 256)?, vec![0xA5; 256]),
    ];
    for (address, value) in [
        (10, 0),
        (323_593, 0),
        (329_031, 0),
        (329_037, 1),
        (331_776, before),
        (331_778, 0),
    ] {
        let (page, bytes) = pages
            .iter_mut()
            .find(|(page, _bytes)| (page.address().as_u32()..page.end()).contains(&address))
            .ok_or("fixture address lacks complete coverage")?;
        *bytes
            .get_mut(usize::try_from(address - page.address().as_u32())?)
            .ok_or("fixture byte absent")? = value;
    }
    Ok(TerminalPlan::new(
        &identity,
        &MenuFieldSnapshot::from_pages(pages)?,
        TerminalTarget::ReflectorTerminal,
    )?)
}

fn identity_script(gateway: Option<u8>) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    if let Some(gateway) = gateway {
        mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    }
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

fn comparisons(plan: &TerminalPlan) -> MockTransport {
    let mut mock = identity_script(Some(plan.before().into()));
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    for page in plan.replacements() {
        read(&mut mock, page.page(), page.expected());
    }
    mock
}

fn complete(plan: &TerminalPlan) -> MockTransport {
    let mut mock = comparisons(plan);
    for page in plan.replacements().iter().filter(|page| !page.is_noop()) {
        mock.expect(&frame(page.page(), page.replacement()), &[ACK]);
        read(&mut mock, page.page(), page.replacement());
    }
    mock.expect(b"E", &[ACK]);
    mock
}

fn modem_script() -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect_hang(b"ID\r");
    mock.expect(&[0xe0, 3, 0], &[0xe0, 8, 0, 1, b'T', b'E', b'S', b'T']);
    mock.expect(&[0xe0, 3, 1], &[0xe0, 10, 1, 1, 1, 0, 7, 0, 0, 0]);
    mock
}

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    CancelWrite,
    Close,
}

#[derive(Default)]
struct Observed {
    events: Vec<String>,
    commands: Vec<Vec<u8>>,
    opens: usize,
    live: usize,
    writes: usize,
}

struct Connection {
    mock: MockTransport,
    observed: Arc<Mutex<Observed>>,
    cancelled: Arc<AtomicBool>,
    journal: std::path::PathBuf,
    fault: Fault,
}

impl Connection {
    fn assert_write_intent(&self, bytes: &[u8]) -> TestResult {
        let contents = std::fs::read_to_string(&self.journal)?;
        let records = contents
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<Vec<Value>, _>>()?;
        let prepared = records
            .first()
            .and_then(|record| record.get("event"))
            .ok_or("baseline event absent")?;
        assert_eq!(prepared.get("kind"), Some(&json!("prepared")));
        let forward = plan(0)?;
        let page_value = |page: &kenwood_tmd750::PageReplacement| {
            json!({
                "address": page.page().address().as_u32(), "length": page.page().len(),
                "expected": page.expected(), "replacement": page.replacement(),
            })
        };
        let guards: Vec<_> = forward.replacements().iter().map(page_value).collect();
        assert_eq!(guards.len(), 4);
        assert_eq!(prepared.get("pages"), Some(&json!(guards)));
        let writes = self
            .observed
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .writes;
        let intended = records
            .last()
            .and_then(|record| record.get("event"))
            .ok_or("write intent absent")?;
        assert_eq!(intended.get("kind"), Some(&json!("before_write")));
        assert_eq!(
            records
                .iter()
                .filter(|record| record.pointer("/event/kind") == Some(&json!("before_write")))
                .count(),
            writes + 1,
            "each dispatch requires a new complete intent, never a prior page's record"
        );
        let (phase, expected_plan) = if writes == 0 {
            ("entry", forward)
        } else {
            ("restore", forward.restoration()?)
        };
        assert_eq!(intended.get("phase"), Some(&json!(phase)));
        let replacement = expected_plan
            .replacements()
            .iter()
            .find(|page| frame(page.page(), page.replacement()) == bytes)
            .ok_or("dispatch differs from exact planned page image")?;
        assert_eq!(intended.get("page"), Some(&page_value(replacement)));
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Ok(mut observed) = self.observed.lock() {
            observed.live -= 1;
            observed.events.push("drop".to_owned());
        }
    }
}

impl Transport for Connection {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.observed
            .lock()
            .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?
            .commands
            .push(bytes.to_vec());
        if bytes.first() == Some(&b'W') {
            self.assert_write_intent(bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
            let mut observed = self
                .observed
                .lock()
                .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?;
            observed.writes += 1;
            drop(observed);
            if matches!(self.fault, Fault::CancelWrite) {
                self.cancelled.store(true, Ordering::Relaxed);
            }
        }
        self.mock.write(bytes).await
    }
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        self.mock.read(bytes).await
    }
    async fn close(&mut self) -> Result<(), TransportError> {
        self.observed
            .lock()
            .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?
            .events
            .push("close".to_owned());
        self.mock.assert_complete();
        if matches!(self.fault, Fault::Close) {
            return Err(TransportError::Disconnected(io::Error::other(
                "injected close failure",
            )));
        }
        self.mock.close().await
    }
}

struct TestBackend {
    scripts: VecDeque<(bool, MockTransport, Fault)>,
    observed: Arc<Mutex<Observed>>,
    cancelled: Arc<AtomicBool>,
    journal: std::path::PathBuf,
    now: Duration,
}

impl Backend for TestBackend {
    type Connection = Connection;
    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Connection, TransportError> {
        assert_eq!(baud, DEFAULT_BAUD);
        self.observed
            .lock()
            .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?
            .opens += 1;
        let (modem, mock, fault) =
            self.scripts
                .pop_front()
                .ok_or_else(|| TransportError::Open {
                    path: endpoint.path.clone(),
                    source: io::Error::other("unexpected open"),
                })?;
        assert_eq!(
            *endpoint,
            if modem {
                endpoints().modem
            } else {
                endpoints().control
            }
        );
        let mut observed = self
            .observed
            .lock()
            .map_err(|error| TransportError::Write(io::Error::other(error.to_string())))?;
        assert_eq!(
            observed.live, 0,
            "a previous owner must be dropped before any fresh open"
        );
        observed.live += 1;
        observed
            .events
            .push(if modem { "modem" } else { "control" }.to_owned());
        drop(observed);
        Ok(Connection {
            mock,
            observed: Arc::clone(&self.observed),
            cancelled: Arc::clone(&self.cancelled),
            journal: self.journal.clone(),
            fault,
        })
    }
    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        let endpoints = endpoints();
        Ok(vec![endpoints.control, endpoints.modem])
    }
    fn now(&self) -> Duration {
        self.now
    }
    async fn wait(&mut self, duration: Duration) {
        self.now += duration;
    }
}

struct Harness {
    directory: tempfile::TempDir,
    cancelled: Arc<AtomicBool>,
    backend: TestBackend,
    captures: Option<Captures>,
    journal: Recorder<File>,
}

impl Harness {
    fn new() -> AppResult<Self> {
        let directory = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let Artifacts {
            directory: capture_directory,
            transcript,
            ..
        } = Artifacts::create(
            CaptureKind::DstarProbe,
            Some(&directory.path().join("capture")),
            Arc::clone(&cancelled),
        )?;
        let captures = Captures::reserve(&capture_directory, transcript, &cancelled)?;
        let journal_path = capture_directory.join("terminal-journal.jsonl");
        let journal = Recorder::named(
            create_private_file(&journal_path)?,
            Arc::clone(&cancelled),
            "terminal-journal.jsonl",
        );
        let backend = TestBackend {
            scripts: VecDeque::new(),
            observed: Arc::new(Mutex::new(Observed::default())),
            cancelled: Arc::clone(&cancelled),
            journal: journal_path,
            now: Duration::ZERO,
        };
        Ok(Self {
            directory,
            cancelled,
            backend,
            captures: Some(captures),
            journal,
        })
    }
    fn add(&mut self, modem: bool, mock: MockTransport, fault: Fault) {
        self.backend.scripts.push_back((modem, mock, fault));
    }
    fn phase(&mut self, plan: &TerminalPlan, fault: Fault) {
        self.add(false, complete(plan), fault);
        self.add(false, identity_script(None), Fault::None);
        self.add(
            false,
            identity_script(Some(match plan.target() {
                TerminalTarget::Off => 0,
                TerminalTarget::ReflectorTerminal => 2,
            })),
            Fault::None,
        );
    }
    async fn run(&mut self, plan: &TerminalPlan) -> AppResult<WorkflowResult> {
        Ok(run_workflow(
            &mut self.backend,
            &endpoints(),
            plan,
            self.captures.take().ok_or("capture already consumed")?,
            &mut self.journal,
            &self.cancelled,
            FAST,
        )
        .await)
    }
    fn assert_retired(&self, owners: usize, writes: usize) -> TestResult {
        let observed = self
            .backend
            .observed
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        assert_eq!(observed.live, 0);
        assert_eq!(observed.opens, owners, "no unplanned open may be attempted");
        assert_eq!(observed.writes, writes);
        assert_eq!(
            observed
                .events
                .iter()
                .filter(|event| *event == "close")
                .count(),
            owners
        );
        assert_eq!(
            observed
                .events
                .iter()
                .filter(|event| *event == "drop")
                .count(),
            owners
        );
        drop(observed);
        assert!(
            self.backend.scripts.is_empty(),
            "all planned owners must be used"
        );
        Ok(())
    }
}

#[tokio::test]
async fn entry_probe_and_exact_restore_retire_every_owner_in_order() -> TestResult {
    let plan = plan(0)?;
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    harness.add(true, modem_script(), Fault::None);
    harness.phase(&plan.restoration()?, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(result.succeeded(), "{result:#?}");
    assert_eq!(result.restoration, Restoration::Verified);
    harness.assert_retired(7, 2)?;
    let evidence = std::fs::read_to_string(
        harness
            .directory
            .path()
            .join("capture/terminal-journal.jsonl"),
    )?;
    assert!(evidence.contains("\"phase\":\"restore\""));
    assert!(evidence.contains("\"restoration\":\"verified\""));
    Ok(())
}

#[tokio::test]
async fn already_terminal_keeps_original_ownership_without_any_write_or_exit_debt() -> TestResult {
    let plan = plan(2)?;
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    harness.add(true, modem_script(), Fault::None);
    let result = harness.run(&plan).await?;
    assert!(result.succeeded(), "{result:#?}");
    assert_eq!(result.restoration, Restoration::NotOwed);
    assert!(result.restore.is_none());
    harness.assert_retired(4, 0)
}

#[tokio::test]
async fn cancellation_before_intent_never_opens_and_after_intent_restores() -> TestResult {
    let plan = plan(0)?;
    let mut before = Harness::new()?;
    before.cancelled.store(true, Ordering::Relaxed);
    let result = before.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::NotOwed);
    before.assert_retired(0, 0)?;
    let mut after = Harness::new()?;
    after.phase(&plan, Fault::CancelWrite);
    after.phase(&plan.restoration()?, Fault::None);
    let result = after.run(&plan).await?;
    assert!(!result.succeeded());
    assert!(result.probe.is_none());
    assert_eq!(result.restoration, Restoration::Verified);
    after.assert_retired(6, 2)
}

#[tokio::test]
async fn missing_write_ack_keeps_debt_and_never_sends_exit_or_reopens() -> TestResult {
    let plan = plan(0)?;
    let mut script = comparisons(&plan);
    let page = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("changed page missing")?;
    script.expect_hang(&frame(page.page(), page.replacement()));
    let mut harness = Harness::new()?;
    harness.add(false, script, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::Owed);
    assert!(result.restore.is_none());
    let entry = result.entry.ok_or("entry missing")?;
    assert_eq!(entry.exchange.exit, evidence::Exit::Uncertain);
    assert_eq!(entry.exchange.possible.len(), 1);
    assert!(entry.exchange.verified.is_empty());
    assert!(
        !harness
            .backend
            .observed
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?
            .commands
            .iter()
            .any(|command| command == b"E")
    );
    harness.assert_retired(1, 1)
}

#[tokio::test]
async fn failed_entry_close_keeps_debt_and_blocks_new_owners() -> TestResult {
    let plan = plan(0)?;
    let mut harness = Harness::new()?;
    harness.add(false, complete(&plan), Fault::Close);
    let result = harness.run(&plan).await?;
    assert_eq!(result.restoration, Restoration::Owed);
    assert!(!result.succeeded());
    assert!(result.restore.is_none());
    harness.assert_retired(1, 1)
}

#[tokio::test]
async fn diagnostic_failure_and_close_error_are_preserved_while_restoration_finishes() -> TestResult
{
    let plan = plan(0)?;
    let mut harness = Harness::new()?;
    harness.phase(&plan, Fault::None);
    let mut modem = MockTransport::new();
    modem.expect_hang(b"ID\r");
    modem.expect_hang(&[0xe0, 3, 0]);
    harness.add(true, modem, Fault::Close);
    harness.phase(&plan.restoration()?, Fault::None);
    let result = harness.run(&plan).await?;
    assert!(!result.succeeded());
    assert_eq!(result.restoration, Restoration::Verified);
    let probe = result.probe.ok_or("probe missing")?;
    assert!(matches!(
        probe.outcome,
        super::super::Outcome::VersionFailed { .. }
    ));
    assert!(probe.close_error.is_some());
    harness.assert_retired(7, 2)
}

#[test]
fn failed_final_checkpoint_cannot_clear_durable_restoration_debt() -> TestResult {
    let harness = Harness::new()?;
    let file = File::open(&harness.backend.journal)?;
    let mut journal = Recorder::named(
        file,
        Arc::clone(&harness.cancelled),
        "terminal-journal.jsonl",
    );
    let mut result = WorkflowResult {
        restoration: Restoration::Verified,
        ..WorkflowResult::default()
    };
    assert!(!checkpoint(&mut journal, &mut result));
    assert_eq!(result.restoration, Restoration::Owed);
    assert!(!result.succeeded());
    assert_eq!(result.problems.len(), 1);
    Ok(())
}
