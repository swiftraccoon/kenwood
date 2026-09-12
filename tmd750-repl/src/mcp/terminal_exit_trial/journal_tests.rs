use std::num::NonZeroU64;

use kenwood_tmd750::memory::TerminalExitTrialEvent;
use kenwood_tmd750::types::{Address, DvGatewayMode, FirmwareIdentity, RadioModel, RadioType};
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

fn make_trial() -> Result<TerminalExitTrial, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut off = [0xA5; PAGE_SIZE];
    *off.first_mut().ok_or("Gateway selector")? = 0;
    *off.get_mut(2).ok_or("Reflector subtype")? = 0;
    let mut control = [0x5A; PAGE_SIZE];
    *control.get_mut(9).ok_or("PM selector")? = 0;
    let mut routing = [0xC3; PAGE_SIZE];
    *routing.get_mut(71).ok_or("USB function")? = 0;
    *routing.get_mut(77).ok_or("Gateway route")? = 1;
    Ok(TerminalExitTrial::prepare_unqualified_offline(
        &identity, &off, &control, &routing,
    )?)
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "fixture evidence identifier must be nonzero".into())
}

fn prepare(journal: &mut Journal<Sink>, trial: &TerminalExitTrial) -> io::Result<()> {
    journal.prepare(trial, Path::new("approved-off-backup.json"))
}

fn fresh(trial: &mut TerminalExitTrial, session: u64) -> TestResult {
    let identity = trial.identity().clone();
    let target = if session == 1 {
        *trial.expected_active_page()
    } else {
        *trial.off_page()
    };
    let control = *trial.control_page();
    let routing = *trial.routing_page();
    trial.record(TerminalExitTrialEvent::FreshSession {
        id: id(session)?,
        identity: &identity,
        memory_format: 0,
        gateway_mode: if session == 1 {
            DvGatewayMode::Terminal
        } else {
            DvGatewayMode::Off
        },
        whole_page: &target,
        control_page: &control,
        routing_page: &routing,
    })?;
    Ok(())
}

fn intent_and_readback(trial: &mut TerminalExitTrial) -> TestResult {
    trial.record(TerminalExitTrialEvent::DurableWriteIntent { id: id(1)? })?;
    let off = *trial.off_page();
    trial.record(TerminalExitTrialEvent::ImmediateReadback { whole_page: &off })?;
    Ok(())
}

fn finalized(trial: &mut TerminalExitTrial, session: u64) -> TestResult {
    let identity = trial.identity().clone();
    trial.record(TerminalExitTrialEvent::SessionFinalized {
        id: id(session)?,
        identity: &identity,
        gateway_mode: DvGatewayMode::Off,
    })?;
    Ok(())
}

fn records(bytes: &[u8]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn poison_unchanged(
    journal: &mut Journal<Sink>,
    trial: &TerminalExitTrial,
    original: &io::Error,
) -> TestResult {
    let bytes = journal.sink.bytes.clone();
    let operations = journal.sink.operations.clone();
    journal.sink.failure = None;
    for result in [
        prepare(journal, trial),
        journal.intent(trial),
        journal.evidence(&"later evidence"),
        journal.finish(trial),
    ] {
        let error = result
            .err()
            .ok_or("poisoned journal must refuse every later operation")?;
        assert_eq!(error.kind(), original.kind());
        assert_eq!(error.to_string(), original.to_string());
    }
    assert_eq!(
        journal.sink.bytes, bytes,
        "poisoned journal cannot append or truncate bytes"
    );
    assert_eq!(
        journal.sink.operations, operations,
        "poisoned journal cannot retry storage"
    );
    Ok(())
}

#[test]
fn prepare_and_intent_synchronize_file_and_directory_before_success() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = make_trial()?;
    prepare(&mut journal, &trial)?;
    let required = [
        Operation::Write,
        Operation::Flush,
        Operation::SyncFile,
        Operation::SyncDirectory,
    ];
    assert_eq!(journal.sink.operations, required);
    fresh(&mut trial, 1)?;
    journal.intent(&trial)?;
    assert_eq!(journal.sink.operations.get(4..), Some(required.as_slice()));
    assert_eq!(
        trial.status(),
        TerminalExitTrialStatus::NotWritten,
        "journal callback succeeds before the pure engine accepts its intent"
    );
    let records = records(&journal.sink.bytes)?;
    let prepared = records.first().ok_or("prepared record")?;
    assert!(
        prepared
            .pointer("/evidence/active_page_provenance")
            .and_then(Value::as_str)
            .is_some_and(|value| value.contains("synthetic") && value.contains("not an observed")),
        "preparation must not describe the expected active page as an observation"
    );
    let intent = records.get(1).ok_or("intent record")?;
    assert_eq!(
        intent.pointer("/evidence/session_id"),
        Some(&Value::from(1))
    );
    assert_eq!(intent.pointer("/evidence/intent_id"), Some(&Value::from(1)));
    assert_eq!(
        intent.pointer("/evidence/memory_format"),
        Some(&Value::from(0))
    );
    assert_eq!(
        intent.pointer("/evidence/pre_entry_gateway"),
        Some(&Value::from(2))
    );
    assert_eq!(
        intent.pointer("/evidence/desired_gateway"),
        Some(&Value::from(0))
    );
    assert_eq!(
        intent.pointer("/evidence/status").and_then(Value::as_str),
        Some("possibly_changed")
    );
    assert!(
        intent
            .pointer("/evidence/active_page_provenance")
            .and_then(Value::as_str)
            .is_some_and(
                |value| value.contains("caller-attested") && value.contains("session capture")
            ),
        "the intent must state the source and limits of the fresh-match evidence"
    );
    Ok(())
}

#[test]
fn every_prepare_and_intent_storage_failure_is_sticky_without_retries() -> TestResult {
    for at_intent in [false, true] {
        for failure in [
            Operation::Write,
            Operation::Flush,
            Operation::SyncFile,
            Operation::SyncDirectory,
        ] {
            let mut journal = Journal::new(Sink::default());
            let mut trial = make_trial()?;
            if at_intent {
                prepare(&mut journal, &trial)?;
                fresh(&mut trial, 1)?;
            }
            journal.sink.failure = Some(failure);
            let result = if at_intent {
                journal.intent(&trial)
            } else {
                prepare(&mut journal, &trial)
            };
            let error = result
                .err()
                .ok_or("injected durability failure must propagate")?;
            poison_unchanged(&mut journal, &trial, &error)?;
            assert_eq!(trial.status(), TerminalExitTrialStatus::NotWritten);
        }
    }
    Ok(())
}

#[test]
fn journal_retains_all_bound_pages_and_the_complete_two_session_history() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = make_trial()?;
    prepare(&mut journal, &trial)?;
    fresh(&mut trial, 1)?;
    journal.intent(&trial)?;
    intent_and_readback(&mut trial)?;
    journal.evidence(&serde_json::json!({ "session": 1, "complete": true }))?;
    finalized(&mut trial, 1)?;
    fresh(&mut trial, 2)?;
    journal.evidence(&serde_json::json!({ "session": 2, "complete": true }))?;
    finalized(&mut trial, 2)?;
    journal.finish(&trial)?;
    let records = records(&journal.sink.bytes)?;
    assert_eq!(records.len(), 5);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.get("format_version"), Some(&Value::from(1)));
        assert_eq!(record.get("sequence"), Some(&Value::from(index)));
        assert!(
            !record
                .get("utc_unix_nanoseconds")
                .and_then(Value::as_str)
                .ok_or("timestamp")?
                .is_empty(),
            "every durable record must retain its timestamp"
        );
        if matches!(
            record.get("kind").and_then(Value::as_str),
            Some("prepared" | "write_intent")
        ) {
            assert_scope(record, &trial)?;
        }
    }
    for (index, attempt) in [(2, 1), (3, 2)] {
        let record = records.get(index).ok_or("attempt evidence")?;
        assert_eq!(
            record.pointer("/evidence/attempt"),
            Some(&Value::from(attempt))
        );
        assert_eq!(
            record.pointer("/evidence/details/session"),
            Some(&Value::from(attempt))
        );
    }
    let finished = records.last().ok_or("finished record")?;
    assert_eq!(
        finished.pointer("/evidence/status").and_then(Value::as_str),
        Some("off_verified_across_sessions")
    );
    assert_eq!(
        finished.pointer("/evidence/further_recovery_assessment_required"),
        Some(&Value::Bool(false))
    );
    assert_eq!(
        finished.pointer("/evidence/attempted_sessions"),
        Some(&Value::from(2))
    );
    assert_eq!(
        journal.sink.bytes.last(),
        Some(&b'\n'),
        "every complete record must terminate with newline"
    );
    assert_eq!(
        journal.sink.operations.len(),
        20,
        "all five records require the full durability sequence"
    );
    let bytes = journal.sink.bytes.clone();
    assert!(
        journal.evidence(&"extra").is_err(),
        "finished journal must be terminal"
    );
    assert_eq!(journal.sink.bytes, bytes);
    Ok(())
}

fn assert_scope(record: &Value, trial: &TerminalExitTrial) -> TestResult {
    let scope = record.pointer("/evidence/scope").ok_or("immutable scope")?;
    assert_eq!(
        scope.get("trial_kind").and_then(Value::as_str),
        Some("terminal_to_off_trial")
    );
    assert_eq!(
        scope.pointer("/identity/firmware").and_then(Value::as_str),
        Some("1.02")
    );
    assert_eq!(
        scope.pointer("/identity/model").and_then(Value::as_str),
        Some("TM-D750")
    );
    assert_eq!(
        scope
            .pointer("/identity/radio_type")
            .and_then(Value::as_str),
        Some("K,2,1")
    );
    for (name, page, data) in [
        ("off_page", trial.page(), trial.off_page()),
        (
            "expected_active_page",
            trial.page(),
            trial.expected_active_page(),
        ),
        (
            "control_page",
            trial.control_page_spec(),
            trial.control_page(),
        ),
        (
            "routing_page",
            trial.routing_page_spec(),
            trial.routing_page(),
        ),
    ] {
        let retained = scope.get(name).ok_or("bound page")?;
        assert_eq!(
            retained.get("address"),
            Some(&Value::from(page.address().as_u32())),
            "{name}"
        );
        assert_eq!(
            retained.get("length"),
            Some(&Value::from(PAGE_SIZE)),
            "{name}"
        );
        assert_eq!(
            retained.get("data"),
            Some(&serde_json::to_value(data.as_slice())?),
            "every {name} byte must be durable"
        );
    }
    Ok(())
}

#[test]
fn every_bound_page_byte_is_load_bearing_for_the_intent() -> TestResult {
    for page in 0..4 {
        for offset in 0..PAGE_SIZE {
            let mut journal = Journal::new(Sink::default());
            let mut trial = make_trial()?;
            prepare(&mut journal, &trial)?;
            fresh(&mut trial, 1)?;
            let bound = journal.bound.as_mut().ok_or("bound scope")?;
            let bytes = match page {
                0 => &mut bound.off,
                1 => &mut bound.expected_active,
                2 => &mut bound.control.1,
                _ => &mut bound.routing.1,
            };
            *bytes.get_mut(offset).ok_or("bound page byte")? ^= 1;
            let prefix = journal.sink.bytes.clone();
            let error = journal
                .intent(&trial)
                .err()
                .ok_or("changed bound page must refuse intent")?;
            assert_eq!(journal.sink.bytes, prefix, "page {page}, byte {offset}");
            poison_unchanged(&mut journal, &trial, &error)?;
        }
    }
    Ok(())
}

#[test]
fn identity_and_all_page_addresses_are_bound_without_rebasing() -> TestResult {
    for changed in 0..4 {
        let mut journal = Journal::new(Sink::default());
        let mut trial = make_trial()?;
        prepare(&mut journal, &trial)?;
        fresh(&mut trial, 1)?;
        let bound = journal.bound.as_mut().ok_or("bound scope")?;
        let other_page = Page::new(Address::new(320_512)?, PAGE_SIZE)?;
        match changed {
            0 => bound.identity.radio_type = RadioType::new("J,2,1")?,
            1 => bound.target = other_page,
            2 => bound.control.0 = other_page,
            _ => bound.routing.0 = other_page,
        }
        let prefix = journal.sink.bytes.clone();
        assert!(
            journal.intent(&trial).is_err(),
            "immutable scope component {changed} must match"
        );
        assert_eq!(journal.sink.bytes, prefix);
    }
    Ok(())
}

#[test]
fn out_of_order_duplicate_and_late_intents_are_refused_before_appending() -> TestResult {
    for stage in 0..5 {
        let mut journal = Journal::new(Sink::default());
        let mut trial = make_trial()?;
        if stage != 0 {
            prepare(&mut journal, &trial)?;
        }
        if stage >= 2 {
            fresh(&mut trial, 1)?;
        }
        match stage {
            2 => journal.intent(&trial)?,
            3 => journal.evidence(&"first attempt failed")?,
            4 => trial.halt(),
            _ => {}
        }
        let prefix = journal.sink.bytes.clone();
        let error = journal
            .intent(&trial)
            .err()
            .ok_or("invalid intent order must fail")?;
        assert_eq!(journal.sink.bytes, prefix, "invalid stage {stage}");
        poison_unchanged(&mut journal, &trial, &error)?;
    }
    Ok(())
}

#[test]
fn preparation_cannot_bind_an_advanced_or_halted_engine_twice() -> TestResult {
    for advance in 0..3 {
        let mut journal = Journal::new(Sink::default());
        let mut trial = make_trial()?;
        match advance {
            0 => prepare(&mut journal, &trial)?,
            1 => fresh(&mut trial, 1)?,
            _ => trial.halt(),
        }
        let prefix = journal.sink.bytes.clone();
        assert!(
            prepare(&mut journal, &trial).is_err(),
            "preparation stage {advance}"
        );
        assert_eq!(journal.sink.bytes, prefix);
    }
    Ok(())
}

#[test]
fn evidence_is_bounded_and_never_creates_missing_intents() -> TestResult {
    let trial = make_trial()?;
    let mut unprepared = Journal::new(Sink::default());
    assert!(
        unprepared.evidence(&"unprepared").is_err(),
        "unprepared journals refuse evidence"
    );
    assert!(
        unprepared.sink.bytes.is_empty(),
        "unprepared rejection must not write a record"
    );
    for with_intent in [false, true] {
        let mut journal = Journal::new(Sink::default());
        let mut trial = make_trial()?;
        prepare(&mut journal, &trial)?;
        if with_intent {
            fresh(&mut trial, 1)?;
            journal.intent(&trial)?;
        }
        journal.evidence(&"first attempt")?;
        if with_intent {
            journal.evidence(&"second attempt")?;
        }
        let prefix = journal.sink.bytes.clone();
        assert!(
            journal.evidence(&"excess attempt").is_err(),
            "evidence count must respect intent scope"
        );
        assert_eq!(journal.sink.bytes, prefix);
    }
    assert_eq!(trial.status(), TerminalExitTrialStatus::NotWritten);
    Ok(())
}

#[test]
fn failed_attempts_finish_with_explicit_conservative_status() -> TestResult {
    for changed in [false, true] {
        let mut journal = Journal::new(Sink::default());
        let mut trial = make_trial()?;
        prepare(&mut journal, &trial)?;
        if changed {
            fresh(&mut trial, 1)?;
            journal.intent(&trial)?;
            trial.record(TerminalExitTrialEvent::DurableWriteIntent { id: id(1)? })?;
        }
        trial.halt();
        journal.evidence(&serde_json::json!({ "complete": false }))?;
        journal.finish(&trial)?;
        let records = records(&journal.sink.bytes)?;
        let finished = records.last().ok_or("failure finish record")?;
        assert_eq!(
            finished.pointer("/evidence/status").and_then(Value::as_str),
            Some(if changed {
                "possibly_changed"
            } else {
                "not_written"
            })
        );
        assert_eq!(
            finished.pointer("/evidence/further_recovery_assessment_required"),
            Some(&Value::Bool(changed))
        );
    }
    Ok(())
}

#[test]
fn finish_rejects_unhalted_engines_and_missing_durable_evidence() -> TestResult {
    for recorded_intent in [false, true] {
        for evidence_count in 0..=2 {
            if recorded_intent && evidence_count == 2 {
                continue;
            }
            let mut journal = Journal::new(Sink::default());
            let mut trial = make_trial()?;
            prepare(&mut journal, &trial)?;
            fresh(&mut trial, 1)?;
            if recorded_intent {
                journal.intent(&trial)?;
            }
            intent_and_readback(&mut trial)?;
            if evidence_count >= 1 {
                journal.evidence(&"first attempt")?;
            }
            finalized(&mut trial, 1)?;
            fresh(&mut trial, 2)?;
            if evidence_count >= 2 {
                assert!(
                    journal
                        .evidence(&"second attempt without durable intent")
                        .is_err(),
                    "a second attempt requires the sole durable intent"
                );
            }
            finalized(&mut trial, 2)?;
            let prefix = journal.sink.bytes.clone();
            assert!(
                journal.finish(&trial).is_err(),
                "pure completion cannot replace missing durable evidence"
            );
            assert_eq!(journal.sink.bytes, prefix);
        }
    }
    let mut journal = Journal::new(Sink::default());
    let trial = make_trial()?;
    prepare(&mut journal, &trial)?;
    assert!(
        journal.finish(&trial).is_err(),
        "an unfinished engine cannot become a terminal journal"
    );
    Ok(())
}

#[test]
fn durable_intent_cannot_be_finished_as_no_write() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let mut trial = make_trial()?;
    prepare(&mut journal, &trial)?;
    fresh(&mut trial, 1)?;
    journal.intent(&trial)?;
    trial.halt();
    journal.evidence(&"failed before pure intent acceptance")?;
    let prefix = journal.sink.bytes.clone();
    assert!(
        journal.finish(&trial).is_err(),
        "durable possible change must not be reported as no intent"
    );
    assert_eq!(journal.sink.bytes, prefix);
    Ok(())
}

struct InvalidEvidence;

impl Serialize for InvalidEvidence {
    fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("injected serialization failure"))
    }
}

#[test]
fn serialization_failure_does_not_touch_the_sink_and_is_sticky() -> TestResult {
    let mut journal = Journal::new(Sink::default());
    let trial = make_trial()?;
    prepare(&mut journal, &trial)?;
    let prefix = journal.sink.bytes.clone();
    let operations = journal.sink.operations.clone();
    let error = journal
        .evidence(&InvalidEvidence)
        .err()
        .ok_or("serialization must fail")?;
    assert_eq!(journal.sink.bytes, prefix);
    assert_eq!(journal.sink.operations, operations);
    poison_unchanged(&mut journal, &trial, &error)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_file_is_private_exclusive_and_never_overwrites_existing_evidence() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    let mut journal = Journal::create(directory.path())?;
    let trial = make_trial()?;
    journal.prepare(&trial, Path::new("approved-off-backup.json"))?;
    let path = directory.path().join(FILENAME);
    assert_eq!(
        std::fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600
    );
    let before = std::fs::read(&path)?;
    assert!(
        Journal::create(directory.path()).is_err(),
        "create_new must reject existing journal"
    );
    assert_eq!(std::fs::read(&path)?, before);
    assert_eq!(trial.next_session()?, TerminalExitTrialSession::Apply);
    Ok(())
}

#[cfg(unix)]
#[test]
fn nonprivate_and_symlink_directories_are_rejected_before_file_creation() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let root = tempfile::tempdir()?;
    for mode in [0o755, 0o770, 0o711, 0o750] {
        let directory = root.path().join(format!("mode-{mode:o}"));
        std::fs::create_dir(&directory)?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode))?;
        assert!(
            Journal::create(&directory).is_err(),
            "mode {mode:o} is not the private contract"
        );
        assert!(
            !directory.join(FILENAME).exists(),
            "refused directory must remain untouched"
        );
    }
    let private = root.path().join("private");
    std::fs::create_dir(&private)?;
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))?;
    let link = root.path().join("link");
    symlink(&private, &link)?;
    assert!(
        Journal::create(&link).is_err(),
        "a symlink to a private directory is still refused"
    );
    assert!(
        !private.join(FILENAME).exists(),
        "symlink refusal must not write in its target"
    );
    Ok(())
}

#[cfg(not(unix))]
#[test]
fn unsupported_platform_refuses_before_creating_any_file() -> TestResult {
    let directory = tempfile::tempdir()?;
    let error = Journal::create(directory.path())
        .err()
        .ok_or("unsupported platform")?;
    assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
