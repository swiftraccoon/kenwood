//! Exact MCP wire schedules for one entry, and the journal records written.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kenwood_tmd750::protocol::mcp::{ACK, read_request, write_request};
use kenwood_tmd750::radio::programming::McpJournal;
use kenwood_tmd750::{FirmwareIdentity, Page, RadioModel, RadioType};
use kenwood_transport::MockTransport;
use serde_json::Value;
use tempfile::TempDir;

use super::*;
use crate::capture::{Recorder, create_private_file};

type TestResult = AppResult<()>;

pub(crate) fn identity() -> AppResult<Identity> {
    Ok(Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    })
}

pub(crate) fn configuration(gateway: u8, route: u8, subtype: u8) -> AppResult<MenuFieldSnapshot> {
    let mut pages: Vec<_> = regions::menu_regions()
        .into_iter()
        .flat_map(kenwood_tmd750::Region::pages)
        .map(|page| (page, vec![0xA5; page.len()]))
        .collect();
    for (address, value) in [
        (10, 0),
        (323_593, 0),
        (329_031, 0),
        (329_037, route),
        (331_776, gateway),
        (331_778, subtype),
    ] {
        let (page, bytes) = pages
            .iter_mut()
            .find(|(page, _)| (page.address().as_u32()..page.end()).contains(&address))
            .ok_or("configuration guard page missing")?;
        let offset = usize::try_from(address - page.address().as_u32())?;
        *bytes.get_mut(offset).ok_or("guard byte missing")? = value;
    }
    Ok(MenuFieldSnapshot::from_pages(pages)?)
}

pub(crate) fn cat(gateway: u8) -> MockTransport {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.02\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    mock.expect(b"GW\r", format!("GW {gateway}\r").as_bytes());
    mock
}

fn frame(page: Page, bytes: &[u8]) -> Vec<u8> {
    let mut frame = write_request(page).to_vec();
    frame.extend_from_slice(bytes);
    frame
}

pub(crate) fn read(mock: &mut MockTransport, page: Page, bytes: &[u8]) {
    mock.expect(&read_request(page), &frame(page, bytes));
    mock.expect(&[ACK], &[ACK]);
}

pub(crate) fn backup(mock: &mut MockTransport, snapshot: &MenuFieldSnapshot) {
    for (page, bytes) in snapshot.pages() {
        read(mock, *page, bytes);
    }
}

pub(crate) fn compare(mock: &mut MockTransport, plan: &TerminalPlan) {
    for page in plan.replacements() {
        read(mock, page.page(), page.expected());
    }
}

pub(crate) fn changes(mock: &mut MockTransport, plan: &TerminalPlan) {
    for page in plan.replacements().iter().filter(|page| !page.is_noop()) {
        mock.expect(&frame(page.page(), page.replacement()), &[ACK]);
        read(mock, page.page(), page.replacement());
    }
}

fn events(path: &Path) -> AppResult<Vec<Value>> {
    std::fs::read_to_string(path)?
        .lines()
        .map(|line| Ok(serde_json::from_str::<Value>(line)?))
        .collect()
}

#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    CancelAfterPage,
    CancelAfterWrite,
}

struct Wire {
    mock: MockTransport,
    journal_path: PathBuf,
    snapshot: Option<MenuFieldSnapshot>,
    phase: Phase,
    cancelled: Arc<AtomicBool>,
    fault: Fault,
    last_write: Vec<u8>,
    writes: usize,
    reads_finished: usize,
}

impl Wire {
    fn verify_intent(&self, bytes: &[u8]) -> AppResult<()> {
        let records = events(&self.journal_path)?;
        if let Some(snapshot) = &self.snapshot {
            let backup = records
                .iter()
                .find(|event| event.pointer("/event/kind") == Some(&Value::from("backup")))
                .and_then(|event| event.pointer("/event/pages"))
                .ok_or("complete backup must precede the first write")?;
            let expected: Vec<_> = snapshot
                .pages()
                .iter()
                .map(|(page, bytes)| (page.address().as_u32(), bytes))
                .collect();
            assert_eq!(*backup, serde_json::to_value(expected)?);
            assert!(self.reads_finished >= snapshot.pages().len() + 4);
        }
        let intent = records.last().ok_or("missing write intent")?;
        assert_eq!(
            intent.pointer("/event/kind"),
            Some(&Value::from("before_write"))
        );
        assert_eq!(
            intent.pointer("/event/phase"),
            Some(&serde_json::to_value(self.phase)?)
        );
        let address = u32::try_from(
            intent
                .pointer("/event/page/address")
                .and_then(Value::as_u64)
                .ok_or("intent address missing")?,
        )?;
        let replacement: Vec<u8> = serde_json::from_value(
            intent
                .pointer("/event/page/replacement")
                .ok_or("intent page missing")?
                .clone(),
        )?;
        let page = Page::new(kenwood_tmd750::Address::new(address)?, replacement.len())?;
        assert_eq!(bytes, frame(page, &replacement));
        assert_eq!(
            records
                .iter()
                .filter(|event| event.pointer("/event/kind") == Some(&Value::from("before_write")))
                .count(),
            self.writes + 1,
            "each changed page requires its own preceding durable intent",
        );
        Ok(())
    }
}

impl Transport for Wire {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.first() == Some(&b'W') {
            self.verify_intent(bytes)
                .map_err(|error| TransportError::Write(io::Error::other(error)))?;
            self.writes += 1;
            if matches!(self.fault, Fault::CancelAfterWrite) {
                self.cancelled.store(true, Ordering::Relaxed);
            }
        }
        self.last_write = bytes.to_vec();
        self.mock.write(bytes).await
    }

    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.mock.read(bytes).await?;
        if self.last_write == [ACK] && bytes.get(..count) == Some(&[ACK]) {
            self.reads_finished += 1;
            if matches!(self.fault, Fault::CancelAfterPage) {
                self.cancelled.store(true, Ordering::Relaxed);
            }
        }
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.mock.close().await
    }
}

struct Harness {
    directory: TempDir,
    cancelled: Arc<AtomicBool>,
    journal: Journal,
}

impl Harness {
    fn new() -> AppResult<Self> {
        let directory = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let journal = Journal::create(directory.path(), Arc::clone(&cancelled))?;
        Ok(Self {
            directory,
            cancelled,
            journal,
        })
    }

    async fn run(
        &mut self,
        mock: MockTransport,
        snapshot: Option<MenuFieldSnapshot>,
        gateway: DvGatewayMode,
        restore: Option<&TerminalPlan>,
        fault: Fault,
    ) -> AppResult<AppResult<TerminalPlan>> {
        let mut wire = Wire {
            mock,
            journal_path: self.directory.path().join("journal.jsonl"),
            snapshot,
            phase: if restore.is_some() {
                Phase::Restore
            } else {
                Phase::Entry
            },
            cancelled: Arc::clone(&self.cancelled),
            fault,
            last_write: Vec::new(),
            writes: 0,
            reads_finished: 0,
        };
        let recorder = Recorder::named(
            create_private_file(&self.directory.path().join("wire.jsonl"))?,
            Arc::clone(&self.cancelled),
            "wire.jsonl",
        );
        let mut capture = CaptureTransport::required(Borrowed(&mut wire), recorder);
        let result = apply(
            &mut capture,
            &identity()?,
            gateway,
            restore,
            &mut self.journal,
            &self.cancelled,
        )
        .await;
        capture.synchronize()?;
        let _recorder = capture.into_recorder();
        wire.mock.assert_complete();
        Ok(result)
    }
}

#[tokio::test]
async fn complete_backup_comparison_update_and_exit_use_one_identity_and_entry() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    changes(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    let result = harness
        .run(mock, Some(snapshot), DvGatewayMode::Off, None, Fault::None)
        .await??;
    assert_eq!(result, plan);
    assert_eq!(harness.journal.entry_plan(), Some(&plan));
    assert!(harness.journal.write_started);
    assert!(harness.journal.entry_exit_acknowledged);
    let records = events(&harness.directory.path().join("journal.jsonl"))?;
    let checkpoint = records.last().ok_or("checkpoint missing")?;
    assert_eq!(
        checkpoint.pointer("/event/verified"),
        Some(&serde_json::json!([328_960, 331_776]))
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_at_completed_backup_page_exits_without_more_reads_or_writes() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let (page, bytes) = snapshot.pages().first().ok_or("configuration missing")?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    read(&mut mock, *page, bytes);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    assert!(
        harness
            .run(mock, None, DvGatewayMode::Off, None, Fault::CancelAfterPage)
            .await?
            .is_err()
    );
    assert!(!harness.journal.write_started);
    assert!(harness.journal.entry_plan().is_none());
    assert!(harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn cancellation_after_first_write_completes_the_bounded_transaction_and_exit() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    changes(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    assert_eq!(
        harness
            .run(
                mock,
                Some(snapshot),
                DvGatewayMode::Off,
                None,
                Fault::CancelAfterWrite
            )
            .await??,
        plan
    );
    assert!(harness.cancelled.load(Ordering::Relaxed));
    assert!(harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn missing_write_ack_retains_plan_debt_and_refuses_exit_or_second_write() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let changed = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("changed page missing")?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    mock.expect_eof(&frame(changed.page(), changed.replacement()));
    let mut harness = Harness::new()?;
    let result = harness
        .run(mock, Some(snapshot), DvGatewayMode::Off, None, Fault::None)
        .await?;
    let error = result
        .err()
        .ok_or("write acknowledgment failure must fail")?;
    let failure = error
        .downcast_ref::<ApplyFailure>()
        .ok_or("typed aggregate missing")?;
    assert!(failure.operation.is_some());
    assert!(failure.exit.is_some());
    assert!(failure.checkpoint.is_none());
    assert_eq!(harness.journal.entry_plan(), Some(&plan));
    assert!(harness.journal.write_started);
    assert!(!harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn fresh_guard_conflict_after_backup_refuses_all_writes_and_retains_plan() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    for (index, page) in plan.replacements().iter().enumerate() {
        let mut bytes = page.expected().to_vec();
        if index == 3 {
            *bytes.last_mut().ok_or("target guard missing")? ^= 1;
        }
        read(&mut mock, page.page(), &bytes);
    }
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    assert!(
        harness
            .run(mock, Some(snapshot), DvGatewayMode::Off, None, Fault::None)
            .await?
            .is_err()
    );
    assert!(!harness.journal.write_started);
    assert!(harness.journal.entry_exit_acknowledged);
    assert_eq!(harness.journal.entry_plan(), Some(&plan));
    Ok(())
}

#[tokio::test]
async fn already_satisfied_terminal_still_backs_up_and_compares_without_write_debt() -> TestResult {
    let snapshot = configuration(2, 2, 0)?;
    let plan = TerminalPlan::for_route(&identity()?, &snapshot, TerminalGatewayRoute::Bluetooth)?;
    let mut mock = cat(2);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    compare(&mut mock, &plan);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    let _plan = harness
        .run(
            mock,
            Some(snapshot),
            DvGatewayMode::Terminal,
            None,
            Fault::None,
        )
        .await??;
    assert!(!harness.journal.write_started);
    assert!(harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn changed_gateway_refuses_programming_entry() -> TestResult {
    let mut harness = Harness::new()?;
    assert!(
        harness
            .run(cat(2), None, DvGatewayMode::Off, None, Fault::None)
            .await?
            .is_err()
    );
    assert!(!harness.journal.write_started);
    assert!(!harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[tokio::test]
async fn preexisting_cancellation_refuses_every_protocol_request() -> TestResult {
    let mut harness = Harness::new()?;
    harness.cancelled.store(true, Ordering::Relaxed);
    assert!(
        harness
            .run(
                MockTransport::new(),
                None,
                DvGatewayMode::Off,
                None,
                Fault::None
            )
            .await?
            .is_err()
    );
    assert!(!harness.journal.write_started);
    Ok(())
}

#[tokio::test]
async fn changed_firmware_refuses_gateway_query_and_programming_entry() -> TestResult {
    let mut mock = MockTransport::new();
    mock.expect(b"ID\r", b"ID TM-D750\r");
    mock.expect(b"FV\r", b"FV 1.03\r");
    mock.expect(b"TY\r", b"TY K,2,1\r");
    let mut harness = Harness::new()?;
    assert!(
        harness
            .run(mock, None, DvGatewayMode::Off, None, Fault::None)
            .await?
            .is_err()
    );
    assert!(!harness.journal.write_started);
    Ok(())
}

#[tokio::test]
async fn restoration_compares_exact_inverse_without_a_second_backup() -> TestResult {
    let forward = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?;
    let restore = forward.restoration()?;
    let mut mock = cat(2);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    compare(&mut mock, &restore);
    changes(&mut mock, &restore);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    harness.journal.planned(&forward)?;
    let result = harness
        .run(
            mock,
            None,
            DvGatewayMode::Terminal,
            Some(&restore),
            Fault::None,
        )
        .await??;
    assert_eq!(result, restore);
    assert_eq!(harness.journal.entry_plan(), Some(&forward));
    assert!(
        !harness.journal.entry_exit_acknowledged,
        "restoration cannot manufacture entry evidence"
    );
    let records = events(&harness.directory.path().join("journal.jsonl"))?;
    assert!(
        !records
            .iter()
            .any(|event| event.pointer("/event/kind") == Some(&Value::from("backup")))
    );
    Ok(())
}

#[tokio::test]
async fn inconsistent_restoration_admission_fails_before_identity_or_mcp() -> TestResult {
    let restore = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?
    .restoration()?;
    let mut harness = Harness::new()?;
    assert!(
        harness
            .run(
                MockTransport::new(),
                None,
                DvGatewayMode::Off,
                Some(&restore),
                Fault::None
            )
            .await?
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn failed_backup_journal_exits_without_any_write_or_lost_checkpoint_error() -> TestResult {
    let snapshot = configuration(0, 1, 1)?;
    let mut mock = cat(0);
    mock.expect(b"0M PROGRAM\r", b"0M\r");
    backup(&mut mock, &snapshot);
    mock.expect(b"E", &[ACK]);
    let mut harness = Harness::new()?;
    let readonly = File::open(harness.directory.path().join("journal.jsonl"))?;
    harness.journal = Journal::new(Recorder::named(
        readonly,
        Arc::clone(&harness.cancelled),
        "journal.jsonl",
    ));
    let error = harness
        .run(mock, None, DvGatewayMode::Off, None, Fault::None)
        .await?
        .err()
        .ok_or("journal failure must fail")?;
    let failure = error
        .downcast_ref::<ApplyFailure>()
        .ok_or("typed aggregate missing")?;
    assert!(failure.operation.is_some());
    assert!(failure.exit.is_none());
    assert!(failure.checkpoint.is_some());
    assert!(!harness.journal.write_started);
    assert!(harness.journal.entry_exit_acknowledged);
    Ok(())
}

#[test]
fn journal_failure_retains_plan_and_observed_entry_ack_without_claiming_write() -> TestResult {
    let plan = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?;
    let mut harness = Harness::new()?;
    let readonly = File::open(harness.directory.path().join("journal.jsonl"))?;
    harness.journal = Journal::new(Recorder::named(
        readonly,
        Arc::clone(&harness.cancelled),
        "journal.jsonl",
    ));
    assert!(harness.journal.planned(&plan).is_err());
    assert_eq!(harness.journal.entry_plan(), Some(&plan));
    assert!(harness.journal.planned(&plan.restoration()?).is_err());
    assert_eq!(harness.journal.entry_plan(), Some(&plan));
    assert!(
        harness
            .journal
            .checkpoint(Phase::Entry, true, &McpJournal::default())
            .is_err()
    );
    assert!(harness.journal.entry_exit_acknowledged);
    assert!(!harness.journal.write_started);
    Ok(())
}

#[test]
fn operation_exit_and_checkpoint_failures_are_retained_independently() -> TestResult {
    let outcome = finish(
        Err(CommandError("operation marker".to_owned()).into()),
        Err(CommandError("exit marker".to_owned()).into()),
        Err(io::Error::other("checkpoint marker")),
    );
    let error = outcome.err().ok_or("three failures cannot succeed")?;
    let failure = error
        .downcast_ref::<ApplyFailure>()
        .ok_or("typed aggregate missing")?;
    assert_eq!(
        failure
            .operation
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("operation marker")
    );
    assert_eq!(
        failure.exit.as_ref().map(ToString::to_string).as_deref(),
        Some("exit marker")
    );
    assert_eq!(
        failure
            .checkpoint
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("checkpoint marker")
    );
    Ok(())
}

#[test]
fn failed_wire_synchronization_cannot_record_intent_or_admit_a_write() -> TestResult {
    let plan = TerminalPlan::for_route(
        &identity()?,
        &configuration(0, 1, 1)?,
        TerminalGatewayRoute::Bluetooth,
    )?;
    let changed = plan
        .replacements()
        .iter()
        .find(|page| !page.is_noop())
        .ok_or("changed page missing")?;
    let mut harness = Harness::new()?;
    harness.journal.planned(&plan)?;
    let mut admitted = false;
    assert!(
        admit_write(
            Phase::Entry,
            changed,
            &mut harness.journal,
            &harness.cancelled,
            &mut admitted,
            || Err(io::Error::other("wire sync failed")),
        )
        .is_err()
    );
    assert!(!admitted);
    assert!(!harness.journal.write_started);
    let records = events(&harness.directory.path().join("journal.jsonl"))?;
    assert_eq!(records.len(), 1);
    assert_eq!(
        records
            .first()
            .and_then(|event| event.pointer("/event/kind")),
        Some(&Value::from("planned"))
    );
    Ok(())
}
