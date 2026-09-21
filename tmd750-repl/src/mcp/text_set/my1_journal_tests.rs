//! Tests that the MY1 journal records name the exact target page, control page
//! and operation, and that they are fsynced in order.

#![cfg(unix)]

use std::fs;
use std::num::NonZeroU64;
use std::os::unix::fs::PermissionsExt;

use kenwood_tmd750::memory::{
    My1Callsign, My1CallsignUpdate, My1CallsignUpdateEvent, My1CallsignUpdateStatus, Pm1Name,
    Pm1NameUpdate,
};
use kenwood_tmd750::types::{DvGatewayMode, FirmwareIdentity, RadioModel, RadioType};
use serde_json::Value;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    directory: tempfile::TempDir,
    journal: UpdateJournal,
    failed: Arc<AtomicBool>,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let directory = tempfile::tempdir()?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let failed = Arc::new(AtomicBool::new(false));
        let journal = UpdateJournal::create(directory.path(), Arc::clone(&failed))?;
        Ok(Self {
            directory,
            journal,
            failed,
        })
    }

    fn bytes(&self) -> io::Result<Vec<u8>> {
        fs::read(self.directory.path().join(FILENAME))
    }

    fn prepare(&mut self, update: &My1CallsignUpdate) -> io::Result<()> {
        self.journal
            .prepare(update, Path::new("approved-my1-backup.json"))
    }
}

fn update() -> Result<My1CallsignUpdate, Box<dyn std::error::Error>> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut page = [0xA5; PAGE_SIZE];
    page.get_mut(0..2)
        .ok_or("Gateway and MY selection")?
        .fill(0);
    page.get_mut(8..16).ok_or("MY1 field")?.fill(0);
    let mut control = [0x5A; PAGE_SIZE];
    *control.get_mut(9).ok_or("PM selection")? = 0;
    Ok(My1CallsignUpdate::prepare(
        &identity,
        &page,
        &control,
        None,
        &My1Callsign::new("KQ4NIT")?,
    )?)
}

fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
    NonZeroU64::new(value).ok_or_else(|| "nonzero session ID".into())
}

fn fresh(update: &mut My1CallsignUpdate, session: u64) -> TestResult {
    let identity = update.identity().clone();
    let control = *update.control_page();
    let bytes = if session == 1 {
        *update.original_page()
    } else {
        *update.desired_page()
    };
    update.record(My1CallsignUpdateEvent::FreshSession {
        id: id(session)?,
        identity: &identity,
        memory_format: 0,
        gateway_mode: DvGatewayMode::Off,
        control_page: &control,
        whole_page: &bytes,
    })?;
    Ok(())
}

fn finalize(update: &mut My1CallsignUpdate, session: u64) -> TestResult {
    let identity = update.identity().clone();
    update.record(My1CallsignUpdateEvent::SessionFinalized {
        id: id(session)?,
        identity: &identity,
        gateway_mode: DvGatewayMode::Off,
    })?;
    Ok(())
}

fn apply(update: &mut My1CallsignUpdate) -> TestResult {
    update.record(My1CallsignUpdateEvent::DurableWriteIntent { id: id(1)? })?;
    let desired = *update.desired_page();
    update.record(My1CallsignUpdateEvent::ImmediateReadback {
        whole_page: &desired,
    })?;
    finalize(update, 1)
}

fn verify(update: &mut My1CallsignUpdate) -> TestResult {
    fresh(update, 2)?;
    finalize(update, 2)
}

fn records(bytes: &[u8]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    std::str::from_utf8(bytes)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn assert_scope(record: &Value, update: &My1CallsignUpdate) -> TestResult {
    let scope = record.pointer("/event/evidence/scope").ok_or("scope")?;
    for (field, expected) in [
        ("target_kind", Value::from("pm_off_my1")),
        (
            "field",
            Value::from("dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway"),
        ),
        ("page_address", Value::from(331_776)),
        ("page_length", Value::from(256)),
        ("current_name", Value::from("")),
        ("desired_name", Value::from("KQ4NIT")),
        (
            "original_page",
            serde_json::to_value(update.original_page().as_slice())?,
        ),
        (
            "desired_page",
            serde_json::to_value(update.desired_page().as_slice())?,
        ),
        (
            "control_page",
            serde_json::json!({"address":323_584,"length":256,"data":update.control_page().as_slice()}),
        ),
        (
            "identity",
            serde_json::json!({"model":"TM-D750","firmware":"1.02","radio_type":"K,2,1"}),
        ),
    ] {
        assert_eq!(
            scope.get(field),
            Some(&expected),
            "journal scope must bind complete {field} evidence"
        );
    }
    Ok(())
}

#[test]
fn complete_my1_history_retains_target_control_identity_and_the_sole_intent() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    fixture.journal.intent(&update)?;
    assert_eq!(
        update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "journal synchronization precedes the engine intent"
    );
    apply(&mut update)?;
    fixture
        .journal
        .evidence(&serde_json::json!({"session":1,"complete":true}))?;
    verify(&mut update)?;
    fixture
        .journal
        .evidence(&serde_json::json!({"session":2,"complete":true}))?;
    fixture.journal.finish(&update)?;
    let records = records(&fixture.bytes()?)?;
    assert_eq!(
        records.len(),
        5,
        "successful MY1 history requires preparation, intent, two sessions, and finish"
    );
    for (sequence, record) in records.iter().enumerate() {
        assert_eq!(
            record.get("sequence"),
            Some(&Value::from(sequence)),
            "journal sequence {sequence} must be contiguous"
        );
        assert_eq!(
            record.pointer("/event/format_version"),
            Some(&Value::from(1)),
            "journal records retain their versioned envelope"
        );
        assert!(
            record
                .get("utc_unix_nanoseconds")
                .and_then(Value::as_str)
                .is_some(),
            "record {sequence} requires a UTC timestamp"
        );
        if matches!(sequence, 0 | 1 | 4) {
            assert_scope(record, &update)?;
        }
    }
    let intent = records.get(1).ok_or("intent")?;
    assert_eq!(
        intent.pointer("/event/kind"),
        Some(&Value::from("write_intent")),
        "the second record must be the sole write intent"
    );
    for field in ["session_id", "intent_id"] {
        assert_eq!(
            intent.pointer(&format!("/event/evidence/{field}")),
            Some(&Value::from(1)),
            "MY1 {field} must be fixed to the apply session"
        );
    }
    assert_eq!(
        intent.pointer("/event/evidence/status"),
        Some(&Value::from("possibly_changed")),
        "durable intent must retain possible write risk"
    );
    let finished = records.last().ok_or("finished")?;
    assert_eq!(
        finished.pointer("/event/evidence/status"),
        Some(&Value::from("verified_across_sessions")),
        "only the completed two-session model may record verified status"
    );
    assert_eq!(
        finished.pointer("/event/evidence/automatic_restore"),
        Some(&Value::Bool(false)),
        "the leave-in-place setter must not promise rollback"
    );
    assert!(
        !fixture.failed.load(Ordering::Relaxed),
        "complete synchronized evidence must not signal failure"
    );
    Ok(())
}

#[test]
fn every_immutable_control_byte_is_part_of_the_journal_binding() -> TestResult {
    let update = update()?;
    let mut fixture = Fixture::new()?;
    fixture.prepare(&update)?;
    let bound = fixture.journal.bound.as_ref().ok_or("prepared binding")?;
    for offset in 0..PAGE_SIZE {
        if offset == 9 {
            continue;
        }
        let mut control = *update.control_page();
        *control.get_mut(offset).ok_or("control byte")? ^= 1;
        let changed = My1CallsignUpdate::prepare(
            update.identity(),
            update.original_page(),
            &control,
            None,
            update.desired_callsign(),
        )?;
        assert!(
            !bound.matches(&changed),
            "unrelated control byte {offset} must not disappear from the immutable journal binding"
        );
    }
    assert!(
        bound.matches(&update),
        "the original complete control page must still match"
    );
    Ok(())
}

#[test]
fn kind_identity_and_both_page_specifications_are_independent_binding_requirements() -> TestResult {
    let update = update()?;
    let mut fixture = Fixture::new()?;
    fixture.prepare(&update)?;
    let bound = fixture.journal.bound.as_mut().ok_or("prepared binding")?;
    bound.kind = UpdateKind::Pm1Name;
    assert!(
        !bound.matches(&update),
        "target kind must be checked independently of all page bytes"
    );
    bound.kind = UpdateKind::PmOffMy1;
    bound.identity.firmware = FirmwareIdentity::new("1.03")?;
    assert!(
        !bound.matches(&update),
        "firmware identity must be part of the immutable binding"
    );
    bound.identity = update.identity().clone();
    bound.identity.radio_type = RadioType::new("K,2,2")?;
    assert!(
        !bound.matches(&update),
        "radio type must be part of the immutable binding"
    );
    bound.identity = update.identity().clone();
    let original_target = bound.page;
    bound.page = Page::new(original_target.address(), 255)?;
    assert!(
        !bound.matches(&update),
        "target page length must not be inferred from the data array"
    );
    bound.page = original_target;
    let original_control = bound.control;
    let (_, data) = original_control.ok_or("bound control page")?;
    bound.control = None;
    assert!(
        !bound.matches(&update),
        "MY1 cannot match a journal lacking its control page"
    );
    bound.control = Some((Page::new(update.control_page_spec().address(), 255)?, data));
    assert!(
        !bound.matches(&update),
        "control page length must be checked independently of its data"
    );
    bound.control = Some((update.page(), data));
    assert!(
        !bound.matches(&update),
        "control page address must not be replaced by the target address"
    );
    bound.control = original_control;
    assert!(
        bound.matches(&update),
        "restoring the exact binding must restore the independent match"
    );
    Ok(())
}

#[test]
fn every_original_and_desired_byte_is_independently_bound_by_the_journal() -> TestResult {
    let update = update()?;
    let mut fixture = Fixture::new()?;
    fixture.prepare(&update)?;
    let bound = fixture.journal.bound.as_mut().ok_or("prepared binding")?;
    for offset in 0..PAGE_SIZE {
        *bound.original.get_mut(offset).ok_or("original byte")? ^= 1;
        assert!(
            !bound.matches(&update),
            "original target byte {offset} must not be omitted from the binding"
        );
        *bound.original.get_mut(offset).ok_or("original byte")? ^= 1;
        *bound.desired.get_mut(offset).ok_or("desired byte")? ^= 1;
        assert!(
            !bound.matches(&update),
            "desired target byte {offset} must not be omitted from the binding"
        );
        *bound.desired.get_mut(offset).ok_or("desired byte")? ^= 1;
    }
    assert!(
        bound.matches(&update),
        "restored original and desired arrays must still match the approved update"
    );
    Ok(())
}

#[test]
fn changed_control_target_or_desired_text_cannot_replace_bound_recovery_bytes() -> TestResult {
    let update = update()?;
    for fault in 0..3 {
        let mut fixture = Fixture::new()?;
        fixture.prepare(&update)?;
        let mut target = *update.original_page();
        let mut control = *update.control_page();
        match fault {
            0 => *control.first_mut().ok_or("control byte")? ^= 1,
            1 => *target.last_mut().ok_or("target byte")? ^= 1,
            _ => {}
        }
        let desired = My1Callsign::new(if fault == 2 { "N0CALL" } else { "KQ4NIT" })?;
        let changed =
            My1CallsignUpdate::prepare(update.identity(), &target, &control, None, &desired)?;
        let prefix = fixture.bytes()?;
        let first = fixture
            .journal
            .intent(&changed)
            .err()
            .ok_or("changed binding accepted")?;
        let later = fixture
            .journal
            .intent(&update)
            .err()
            .ok_or("binding rejection was not sticky")?;
        assert_eq!(
            later.to_string(),
            first.to_string(),
            "the original binding fault {fault} must remain the first error"
        );
        assert_eq!(
            fixture.bytes()?,
            prefix,
            "binding fault {fault} must preserve the existing recovery record"
        );
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "binding rejection must signal capture failure"
        );
    }
    Ok(())
}

#[test]
fn pm1_and_my1_journals_cannot_cross_bind_in_either_direction() -> TestResult {
    let my1 = update()?;
    let mut pm1_page = [0xA5; PAGE_SIZE];
    pm1_page.get_mut(10..26).ok_or("PM1 field")?.fill(0);
    pm1_page
        .get_mut(10..13)
        .ok_or("PM1 text")?
        .copy_from_slice(b"PM1");
    let pm1 = Pm1NameUpdate::prepare(
        my1.identity(),
        &pm1_page,
        &Pm1Name::new("PM1")?,
        &Pm1Name::new("HOME")?,
    )?;
    for prepared_my1 in [true, false] {
        let mut fixture = Fixture::new()?;
        if prepared_my1 {
            fixture.prepare(&my1)?;
        } else {
            fixture
                .journal
                .prepare(&pm1, Path::new("pm1-backup.json"))?;
        }
        let prefix = fixture.bytes()?;
        let result = if prepared_my1 {
            fixture.journal.intent(&pm1)
        } else {
            fixture.journal.intent(&my1)
        };
        assert!(
            result.is_err(),
            "the journal must reject the other update kind; prepared_my1={prepared_my1}"
        );
        assert_eq!(
            fixture.bytes()?,
            prefix,
            "cross-kind refusal must not append or replace recovery data"
        );
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "cross-kind refusal must remain a fatal journal inconsistency"
        );
    }
    Ok(())
}

#[test]
fn write_intent_synchronization_failure_never_advances_the_journal_or_engine() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    let first = fixture
        .journal
        .intent_with_sync(&update, |_recorder| {
            Err(io::Error::other("injected MY1 intent sync failure"))
        })
        .err()
        .ok_or("intent synchronization unexpectedly succeeded")?;
    assert_eq!(
        fixture.journal.stage,
        Stage::Prepared,
        "failed synchronization must not admit a write intent"
    );
    assert_eq!(
        update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "the engine cannot infer accepted intent from a failed journal sync"
    );
    let prefix = fixture.bytes()?;
    let replacement = tempfile::NamedTempFile::new()?;
    fixture.journal.recorder =
        Recorder::named(replacement.reopen()?, Arc::clone(&fixture.failed), FILENAME);
    for result in [
        fixture.journal.intent(&update),
        fixture.journal.evidence(&"later evidence"),
        fixture.journal.finish(&update),
    ] {
        let later = result.err().ok_or("failed intent allowed another append")?;
        assert_eq!(
            later.kind(),
            first.kind(),
            "intent sync failure must preserve its original error kind"
        );
        assert_eq!(
            later.to_string(),
            first.to_string(),
            "intent sync failure must preserve its original message"
        );
    }
    assert_eq!(
        fs::metadata(replacement.path())?.len(),
        0,
        "replacing the writer must not clear a sticky failure"
    );
    assert_eq!(
        fixture.bytes()?,
        prefix,
        "failed intent evidence must remain unchanged after later requests"
    );
    assert!(
        fixture.failed.load(Ordering::Relaxed),
        "intent sync failure must signal the shared capture flag"
    );
    Ok(())
}

#[test]
fn durable_intent_record_is_complete_before_sync_success_is_accepted() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    let path = fixture.directory.path().join(FILENAME);
    fixture.journal.intent_with_sync(&update, |recorder| {
        recorder.synchronize()?;
        let bytes = fs::read(&path)?;
        let records = records(&bytes).map_err(|error| io::Error::other(error.to_string()))?;
        assert_eq!(
            records.len(),
            2,
            "intent must be physically readable after preparation and before callback success"
        );
        let intent = records
            .last()
            .ok_or_else(|| io::Error::other("missing intent"))?;
        assert_scope(intent, &update).map_err(|error| io::Error::other(error.to_string()))?;
        assert_eq!(
            intent.pointer("/event/kind"),
            Some(&Value::from("write_intent")),
            "the synchronized final record must authorize only the sole write"
        );
        Ok(())
    })?;
    assert_eq!(
        fixture.journal.stage,
        Stage::WriteIntent,
        "only successful synchronization may advance the durable journal stage"
    );
    assert_eq!(
        update.status(),
        My1CallsignUpdateStatus::NotWritten,
        "the caller must still record engine intent before dispatch"
    );
    Ok(())
}

#[test]
fn out_of_order_and_duplicate_my1_intents_fail_stickily() -> TestResult {
    let update = update()?;
    for duplicate in [false, true] {
        let mut fixture = Fixture::new()?;
        if duplicate {
            fixture.prepare(&update)?;
            fixture.journal.intent(&update)?;
        }
        let prefix = fixture.bytes()?;
        let first = fixture
            .journal
            .intent(&update)
            .err()
            .ok_or("out-of-order intent accepted")?;
        let later = fixture
            .prepare(&update)
            .err()
            .ok_or("out-of-order rejection was not sticky")?;
        assert_eq!(
            later.to_string(),
            first.to_string(),
            "out-of-order intent must preserve the initial error; duplicate={duplicate}"
        );
        assert_eq!(
            fixture.bytes()?,
            prefix,
            "out-of-order intent must not append; duplicate={duplicate}"
        );
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "invalid intent ordering must signal evidence failure"
        );
    }
    Ok(())
}

#[test]
fn verified_model_cannot_finish_without_a_durable_intent_and_failure_is_sticky() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    apply(&mut update)?;
    verify(&mut update)?;
    assert_eq!(
        update.status(),
        My1CallsignUpdateStatus::VerifiedAcrossSessions,
        "fixture must reach model verification independently of the missing journal intent"
    );
    let prefix = fixture.bytes()?;
    let first = fixture
        .journal
        .finish(&update)
        .err()
        .ok_or("missing durable intent accepted")?;
    for result in [
        fixture.journal.finish(&update),
        fixture.journal.evidence(&"late repair"),
        fixture.journal.intent(&update),
    ] {
        let later = result
            .err()
            .ok_or("finalization failure allowed a later record")?;
        assert_eq!(
            later.to_string(),
            first.to_string(),
            "a model status must not erase the first journal finalization error"
        );
    }
    assert_eq!(
        fixture.bytes()?,
        prefix,
        "failed finalization must preserve all prior bytes without a success marker"
    );
    assert!(
        fixture.failed.load(Ordering::Relaxed),
        "missing durable intent must signal capture failure"
    );
    Ok(())
}

#[test]
fn durable_my1_intent_cannot_be_reclassified_as_not_written() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    fixture.journal.intent(&update)?;
    let prefix = fixture.bytes()?;
    assert!(
        fixture.journal.finish(&update).is_err(),
        "a durable intent cannot be cleared by a still-unwritten model"
    );
    assert_eq!(
        fixture.bytes()?,
        prefix,
        "risk inconsistency must not append a misleading not-written finish record"
    );
    assert!(
        fixture.failed.load(Ordering::Relaxed),
        "contradictory write risk must stop further evidence"
    );
    Ok(())
}

#[test]
fn failed_my1_session_retains_possible_change_and_never_promises_rollback() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mut update = update()?;
    fixture.prepare(&update)?;
    fresh(&mut update, 1)?;
    fixture.journal.intent(&update)?;
    update.record(My1CallsignUpdateEvent::DurableWriteIntent { id: id(1)? })?;
    update.halt();
    fixture
        .journal
        .evidence(&serde_json::json!({"session":1,"complete":false}))?;
    fixture.journal.finish(&update)?;
    let records = records(&fixture.bytes()?)?;
    let finished = records.last().ok_or("finished")?;
    assert_scope(finished, &update)?;
    assert_eq!(
        finished.pointer("/event/evidence/status"),
        Some(&Value::from("possibly_changed")),
        "a failed session must retain possible change"
    );
    assert_eq!(
        finished.pointer("/event/evidence/manual_recovery_may_be_required"),
        Some(&Value::Bool(true)),
        "incomplete MY1 verification must retain recovery warning"
    );
    assert_eq!(
        finished.pointer("/event/evidence/automatic_restore"),
        Some(&Value::Bool(false)),
        "failure must not invent rollback authority"
    );
    let prefix = fixture.bytes()?;
    assert!(
        fixture.journal.evidence(&"late success").is_err(),
        "finished failed evidence must reject a later success claim"
    );
    assert_eq!(
        fixture.bytes()?,
        prefix,
        "terminal journal state must remain immutable"
    );
    Ok(())
}

#[test]
fn my1_recovery_journal_is_private_and_refuses_existing_output() -> TestResult {
    let mut fixture = Fixture::new()?;
    fixture.prepare(&update()?)?;
    let path = fixture.directory.path().join(FILENAME);
    assert_eq!(
        fs::metadata(&path)?.permissions().mode() & 0o777,
        0o600,
        "MY1 recovery data must be owner-readable and owner-writable only"
    );
    let prefix = fixture.bytes()?;
    assert!(
        UpdateJournal::create(fixture.directory.path(), Arc::clone(&fixture.failed)).is_err(),
        "MY1 evidence must never reopen an existing journal"
    );
    assert_eq!(
        fixture.bytes()?,
        prefix,
        "a conflicting output path must preserve existing recovery evidence"
    );
    Ok(())
}
