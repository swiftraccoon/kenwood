use std::num::NonZeroU64;

use kenwood_tmd750::memory::{PmNameTrialEvent, PmNameTrialSession};
use kenwood_tmd750::types::{FirmwareIdentity, RadioModel, RadioType};
use serde::Serializer;
use serde_json::Value;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Write,
    Flush,
    SyncFile,
    SyncDirectory,
}

#[derive(Debug, Default)]
struct Sink {
    bytes: Vec<u8>,
    operations: Vec<Operation>,
    failure: Option<Operation>,
}

impl Sink {
    fn observe(&mut self, operation: Operation) -> io::Result<()> {
        self.operations.push(operation);
        if self.failure == Some(operation) {
            return Err(io::Error::other(format!("injected {operation:?} failure")));
        }
        Ok(())
    }
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Err(error) = self.observe(Operation::Write) {
            self.bytes
                .extend_from_slice(bytes.get(..3).unwrap_or_default());
            return Err(error);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.observe(Operation::Flush)
    }
}

impl DurableWrite for Sink {
    fn synchronize(&mut self) -> io::Result<()> {
        self.observe(Operation::SyncFile)
    }
    fn synchronize_directory(&mut self) -> io::Result<()> {
        self.observe(Operation::SyncDirectory)
    }
}

fn trial() -> Result<PmNameTrial, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut page = [0xA5; PAGE_SIZE];
    page.get_mut(10..26).ok_or("PM1 range")?.fill(0);
    page.get_mut(10..13)
        .ok_or("PM1 bytes")?
        .copy_from_slice(b"PM1");
    Ok(PmNameTrial::prepare_unqualified_offline(
        &identity, &page, "PM1",
    )?)
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "nonzero fixture ID".into())
}

fn prepare(journal: &mut Journal<Sink>, trial: &PmNameTrial) -> io::Result<()> {
    journal.prepare(trial, Path::new("approved-backup.json"), "PM1")
}

fn fresh(trial: &mut PmNameTrial, session: u64) -> TestResult {
    let identity = trial.identity().clone();
    let bytes = if session == 2 {
        *trial.expected_page()
    } else {
        *trial.original_page()
    };
    trial.record(PmNameTrialEvent::FreshSession {
        id: id(session)?,
        identity: &identity,
        memory_format: 0,
        whole_page: &bytes,
    })?;
    Ok(())
}

fn intent_and_readback(trial: &mut PmNameTrial, session: u64) -> TestResult {
    let write = if session == 1 {
        PmNameTrialWrite::Rename
    } else {
        PmNameTrialWrite::Restore
    };
    trial.record(PmNameTrialEvent::DurableWriteIntent {
        id: id(session)?,
        write,
    })?;
    let bytes = if session == 1 {
        *trial.expected_page()
    } else {
        *trial.original_page()
    };
    trial.record(PmNameTrialEvent::ImmediateReadback { whole_page: &bytes })?;
    Ok(())
}

fn finalized(trial: &mut PmNameTrial, session: u64) -> TestResult {
    trial.record(PmNameTrialEvent::SessionFinalized { id: id(session)? })?;
    Ok(())
}

fn complete_engine(trial: &mut PmNameTrial) -> TestResult {
    for session in [1, 2, 3] {
        fresh(trial, session)?;
        if session != 3 {
            intent_and_readback(trial, session)?;
        }
        finalized(trial, session)?;
    }
    Ok(())
}

fn records(bytes: &[u8]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[test]
fn prepare_synchronizes_file_then_directory_before_accepting_an_intent() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = trial()?;
    prepare(&mut journal, &trial)?;
    assert_eq!(trial.next_session()?, PmNameTrialSession::Rename);
    assert_eq!(
        journal.sink.operations,
        [
            Operation::Write,
            Operation::Flush,
            Operation::SyncFile,
            Operation::SyncDirectory
        ]
    );
    fresh(&mut trial, 1)?;
    journal.intent(&trial, PmNameTrialWrite::Rename)?;
    assert_eq!(
        journal.sink.operations.get(4..),
        Some([Operation::Write, Operation::Flush, Operation::SyncFile].as_slice())
    );
    assert_eq!(trial.status(), PmNameTrialStatus::NotWritten);
    let records = records(&journal.sink.bytes)?;
    let intent = records.get(1).ok_or("intent record")?;
    assert_eq!(intent.pointer("/evidence/intent_id"), Some(&Value::from(1)));
    assert_eq!(
        intent.pointer("/evidence/session_id"),
        Some(&Value::from(1))
    );
    assert_eq!(
        intent.pointer("/evidence/memory_format"),
        Some(&Value::from(0))
    );
    assert_eq!(
        intent
            .pointer("/evidence/restoration_status")
            .and_then(Value::as_str),
        Some("possibly_changed")
    );
    Ok(())
}

#[test]
fn every_storage_failure_is_sticky_and_preserves_the_existing_prefix() -> TestResult {
    for failure in [
        Operation::Write,
        Operation::Flush,
        Operation::SyncFile,
        Operation::SyncDirectory,
    ] {
        let mut journal = Journal::new(Sink {
            failure: Some(failure),
            ..Sink::default()
        });
        let trial = trial()?;
        let error = prepare(&mut journal, &trial)
            .err()
            .ok_or("injected prepare failure")?;
        let bytes = journal.sink.bytes.clone();
        let operations = journal.sink.operations.clone();
        journal.sink.failure = None;
        for result in [
            journal.evidence(&"later diagnostics"),
            journal.intent(&trial, PmNameTrialWrite::Rename),
            journal.finish(&trial),
        ] {
            let later = result.err().ok_or("poisoned journal must fail")?;
            assert_eq!(later.kind(), error.kind());
            assert_eq!(later.to_string(), error.to_string());
        }
        assert_eq!(journal.sink.bytes, bytes, "{failure:?}");
        assert_eq!(journal.sink.operations, operations, "{failure:?}");
        assert_eq!(trial.status(), PmNameTrialStatus::NotWritten);
    }
    Ok(())
}

#[test]
fn failed_intent_synchronization_never_reports_success_or_accepts_later_evidence() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = trial()?;
    prepare(&mut journal, &trial)?;
    fresh(&mut trial, 1)?;
    journal.sink.failure = Some(Operation::SyncFile);
    assert!(journal.intent(&trial, PmNameTrialWrite::Rename).is_err());
    assert_eq!(trial.status(), PmNameTrialStatus::NotWritten);
    let prefix = journal.sink.bytes.clone();
    let operations = journal.sink.operations.clone();
    journal.sink.failure = None;
    assert!(journal.evidence(&"transport failed").is_err());
    assert!(journal.finish(&trial).is_err());
    assert_eq!(journal.sink.bytes, prefix);
    assert_eq!(journal.sink.operations, operations);
    Ok(())
}

#[test]
fn journal_pins_all_recovery_bytes_and_records_the_full_three_session_history() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = trial()?;
    prepare(&mut journal, &trial)?;
    for session in [1, 2, 3] {
        fresh(&mut trial, session)?;
        if session != 3 {
            let write = if session == 1 {
                PmNameTrialWrite::Rename
            } else {
                PmNameTrialWrite::Restore
            };
            journal.intent(&trial, write)?;
            intent_and_readback(&mut trial, session)?;
        }
        journal.evidence(&serde_json::json!({ "session": session, "complete": true }))?;
        finalized(&mut trial, session)?;
    }
    journal.finish(&trial)?;
    let records = records(&journal.sink.bytes)?;
    assert_eq!(records.len(), 7);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.get("format_version"), Some(&Value::from(1)));
        assert_eq!(record.get("sequence"), Some(&Value::from(index)));
        assert!(
            !record
                .get("utc_unix_nanoseconds")
                .and_then(Value::as_str)
                .ok_or("timestamp")?
                .is_empty()
        );
        if matches!(
            record.get("kind").and_then(Value::as_str),
            Some("prepared" | "write_intent")
        ) {
            assert_eq!(
                record.pointer("/evidence/scope/page_address"),
                Some(&Value::from(323_584))
            );
            assert_eq!(
                record.pointer("/evidence/scope/page_length"),
                Some(&Value::from(256))
            );
            assert_eq!(
                record.pointer("/evidence/scope/original_page"),
                Some(&serde_json::to_value(trial.original_page().as_slice())?)
            );
            assert_eq!(
                record.pointer("/evidence/scope/expected_page"),
                Some(&serde_json::to_value(trial.expected_page().as_slice())?)
            );
            assert_eq!(
                record
                    .pointer("/evidence/scope/identity/firmware")
                    .and_then(Value::as_str),
                Some("1.02")
            );
        }
    }
    let restoration = records.get(3).ok_or("restore intent")?;
    assert_eq!(
        restoration.pointer("/evidence/intent_id"),
        Some(&Value::from(2))
    );
    assert_eq!(
        restoration
            .pointer("/evidence/write")
            .and_then(Value::as_str),
        Some("restore")
    );
    let finished = records.last().ok_or("finished record")?;
    assert_eq!(
        finished.pointer("/evidence/status").and_then(Value::as_str),
        Some("restoration_verified")
    );
    assert_eq!(
        finished.pointer("/evidence/restoration_required"),
        Some(&Value::Bool(false))
    );
    let bytes = journal.sink.bytes.clone();
    assert!(journal.evidence(&"extra evidence").is_err());
    assert_eq!(journal.sink.bytes, bytes);
    Ok(())
}

#[test]
fn successful_engine_proof_cannot_substitute_for_missing_durable_journal_intents() -> TestResult {
    for record_rename in [false, true] {
        let mut journal = Journal::new(Sink::default());
        let mut trial = trial()?;
        prepare(&mut journal, &trial)?;
        if record_rename {
            journal.intent(&trial, PmNameTrialWrite::Rename)?;
        }
        complete_engine(&mut trial)?;
        assert_eq!(trial.status(), PmNameTrialStatus::RestorationVerified);
        let bytes = journal.sink.bytes.clone();
        assert!(journal.finish(&trial).is_err());
        assert_eq!(journal.sink.bytes, bytes);
    }
    Ok(())
}

#[test]
fn recorded_intent_cannot_be_finished_as_not_written() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let trial = trial()?;
    prepare(&mut journal, &trial)?;
    journal.intent(&trial, PmNameTrialWrite::Rename)?;
    assert_eq!(trial.status(), PmNameTrialStatus::NotWritten);
    let bytes = journal.sink.bytes.clone();
    assert!(journal.finish(&trial).is_err());
    assert_eq!(journal.sink.bytes, bytes);
    Ok(())
}

#[test]
fn possible_changes_remain_explicit_after_failed_session_evidence() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = trial()?;
    prepare(&mut journal, &trial)?;
    fresh(&mut trial, 1)?;
    journal.intent(&trial, PmNameTrialWrite::Rename)?;
    trial.record(PmNameTrialEvent::DurableWriteIntent {
        id: id(1)?,
        write: PmNameTrialWrite::Rename,
    })?;
    trial.halt();
    journal.evidence(&serde_json::json!({ "session": 1, "complete": false }))?;
    journal.finish(&trial)?;
    let records = records(&journal.sink.bytes)?;
    let finished = records.last().ok_or("finished record")?;
    assert_eq!(
        finished.pointer("/evidence/status").and_then(Value::as_str),
        Some("possibly_changed")
    );
    assert_eq!(
        finished.pointer("/evidence/restoration_required"),
        Some(&Value::Bool(true))
    );
    Ok(())
}

#[test]
fn scope_changes_wrong_display_and_out_of_order_calls_poison_without_appending() -> TestResult {
    let original = trial()?;
    let mut changed_page = *original.original_page();
    *changed_page.get_mut(0).ok_or("unrelated byte")? ^= 1;
    let changed =
        PmNameTrial::prepare_unqualified_offline(original.identity(), &changed_page, "PM1")?;
    let mut wrong_scope = Journal::new(Sink::default());
    prepare(&mut wrong_scope, &original)?;
    let prefix = wrong_scope.sink.bytes.clone();
    assert!(
        wrong_scope
            .intent(&changed, PmNameTrialWrite::Rename)
            .is_err()
    );
    assert_eq!(wrong_scope.sink.bytes, prefix);
    let mut wrong_display = Journal::new(Sink::default());
    assert!(
        wrong_display
            .prepare(&original, Path::new("backup.json"), "PM2")
            .is_err()
    );
    assert!(wrong_display.sink.bytes.is_empty());
    let mut wrong_order = Journal::new(Sink::default());
    assert!(
        wrong_order
            .intent(&original, PmNameTrialWrite::Rename)
            .is_err()
    );
    assert!(wrong_order.sink.bytes.is_empty());
    assert!(prepare(&mut wrong_order, &original).is_err());
    let mut restore_first = Journal::new(Sink::default());
    prepare(&mut restore_first, &original)?;
    assert!(
        restore_first
            .intent(&original, PmNameTrialWrite::Restore)
            .is_err()
    );
    Ok(())
}

struct InvalidEvidence;

impl Serialize for InvalidEvidence {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("injected serialization failure"))
    }
}

#[test]
fn serialization_failure_never_writes_a_record_and_remains_sticky() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let trial = trial()?;
    prepare(&mut journal, &trial)?;
    let prefix = journal.sink.bytes.clone();
    let operations = journal.sink.operations.clone();
    assert!(journal.evidence(&InvalidEvidence).is_err());
    assert!(journal.evidence(&"valid later evidence").is_err());
    assert_eq!(journal.sink.bytes, prefix);
    assert_eq!(journal.sink.operations, operations);
    Ok(())
}

#[cfg(unix)]
#[test]
fn file_is_private_exclusive_and_never_truncates_existing_recovery_bytes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut journal = Journal::create(directory.path())?;
    let trial = trial()?;
    journal.prepare(&trial, Path::new("approved-backup.json"), "PM1")?;
    let path = directory.path().join(FILENAME);
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    let original = std::fs::read(&path)?;
    assert!(Journal::create(directory.path()).is_err());
    assert_eq!(std::fs::read(&path)?, original);
    assert_eq!(trial.next_session()?, PmNameTrialSession::Rename);
    Ok(())
}

#[cfg(unix)]
#[test]
fn shared_or_symlink_directories_are_rejected_before_file_creation() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempfile::tempdir()?;
    let shared = root.path().join("shared");
    std::fs::create_dir(&shared)?;
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755))?;
    assert!(Journal::create(&shared).is_err());
    assert!(!shared.join(FILENAME).exists());
    let link = root.path().join("link");
    symlink(&shared, &link)?;
    assert!(Journal::create(&link).is_err());
    assert!(!shared.join(FILENAME).exists());
    Ok(())
}

#[cfg(not(unix))]
#[test]
fn platforms_without_the_private_directory_contract_fail_before_creating_a_file() -> TestResult {
    let directory = tempfile::tempdir()?;
    let error = Journal::create(directory.path())
        .err()
        .ok_or("unsupported platform")?;
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
