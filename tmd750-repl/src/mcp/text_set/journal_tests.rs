#[cfg(unix)]
mod unix {
    use std::fs;
    use std::num::NonZeroU64;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use kenwood_tmd750::memory::{Pm1Name, Pm1NameUpdateEvent};
    use kenwood_tmd750::types::{FirmwareIdentity, RadioModel, RadioType};
    use serde::Serializer;
    use serde_json::Value;

    use super::super::*;

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

        fn prepare(&mut self, update: &Pm1NameUpdate) -> io::Result<()> {
            self.journal
                .prepare(update, Path::new("approved-backup.json"))
        }
    }

    fn update() -> Result<Pm1NameUpdate, Box<dyn std::error::Error>> {
        let identity = Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new("1.02")?,
            radio_type: RadioType::new("K,2,1")?,
        };
        let mut page = [0xA5; PAGE_SIZE];
        page.get_mut(10..26).ok_or("name field")?.fill(0);
        page.get_mut(10..13)
            .ok_or("current name")?
            .copy_from_slice(b"PM1");
        Ok(Pm1NameUpdate::prepare(
            &identity,
            &page,
            &Pm1Name::new("PM1")?,
            &Pm1Name::new("LOCAL")?,
        )?)
    }

    fn id(value: u64) -> Result<NonZeroU64, Box<dyn std::error::Error>> {
        NonZeroU64::new(value).ok_or_else(|| "nonzero session ID".into())
    }

    fn fresh(update: &mut Pm1NameUpdate, session: u64) -> TestResult {
        let identity = update.identity().clone();
        let bytes = if session == 1 {
            *update.original_page()
        } else {
            *update.desired_page()
        };
        update.record(Pm1NameUpdateEvent::FreshSession {
            id: id(session)?,
            identity: &identity,
            memory_format: 0,
            whole_page: &bytes,
        })?;
        Ok(())
    }

    fn write_and_finalize(update: &mut Pm1NameUpdate) -> TestResult {
        update.record(Pm1NameUpdateEvent::DurableWriteIntent { id: id(1)? })?;
        let bytes = *update.desired_page();
        update.record(Pm1NameUpdateEvent::ImmediateReadback { whole_page: &bytes })?;
        update.record(Pm1NameUpdateEvent::SessionFinalized { id: id(1)? })?;
        Ok(())
    }

    fn finish_engine(update: &mut Pm1NameUpdate) -> TestResult {
        fresh(update, 2)?;
        update.record(Pm1NameUpdateEvent::SessionFinalized { id: id(2)? })?;
        Ok(())
    }

    fn records(bytes: &[u8]) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        std::str::from_utf8(bytes)?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }

    fn assert_scope(record: &Value, update: &Pm1NameUpdate) -> TestResult {
        let scope = record.pointer("/event/evidence/scope").ok_or("scope")?;
        assert_eq!(
            scope.get("original_page"),
            Some(&serde_json::to_value(update.original_page().as_slice())?)
        );
        assert_eq!(
            scope.get("desired_page"),
            Some(&serde_json::to_value(update.desired_page().as_slice())?)
        );
        assert_eq!(scope.get("page_address"), Some(&Value::from(323_584)));
        assert_eq!(scope.get("page_length"), Some(&Value::from(256)));
        assert_eq!(
            scope.get("current_name").and_then(Value::as_str),
            Some("PM1")
        );
        assert_eq!(
            scope.get("desired_name").and_then(Value::as_str),
            Some("LOCAL")
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
        assert_eq!(
            scope.get("field").and_then(Value::as_str),
            Some("pm.PmName1")
        );
        Ok(())
    }

    #[test]
    fn complete_history_binds_exact_approved_names_and_both_whole_pages() -> TestResult {
        let mut fixture = Fixture::new()?;
        let mut update = update()?;
        fixture.prepare(&update)?;
        fresh(&mut update, 1)?;
        fixture.journal.intent(&update)?;
        write_and_finalize(&mut update)?;
        fixture
            .journal
            .evidence(&serde_json::json!({"session": 1, "complete": true}))?;
        finish_engine(&mut update)?;
        fixture
            .journal
            .evidence(&serde_json::json!({"session": 2, "complete": true}))?;
        fixture.journal.finish(&update)?;

        let records = records(&fixture.bytes()?)?;
        assert_eq!(records.len(), 5);
        for (sequence, record) in records.iter().enumerate() {
            assert_eq!(record.get("sequence"), Some(&Value::from(sequence)));
            assert_eq!(
                record.pointer("/event/format_version"),
                Some(&Value::from(1))
            );
            assert!(
                record
                    .get("utc_unix_nanoseconds")
                    .and_then(Value::as_str)
                    .is_some(),
                "record {sequence} must retain its UTC timestamp: {record}"
            );
            if matches!(
                record.pointer("/event/kind").and_then(Value::as_str),
                Some("prepared" | "write_intent" | "finished")
            ) {
                assert_scope(record, &update)?;
            }
        }
        let prepared = records.first().ok_or("prepared")?;
        assert_eq!(
            prepared.pointer("/event/evidence/operator_approved_apply"),
            Some(&Value::Bool(true))
        );
        let intent = records.get(1).ok_or("intent")?;
        for field in ["session_id", "intent_id"] {
            assert_eq!(
                intent.pointer(&format!("/event/evidence/{field}")),
                Some(&Value::from(1))
            );
        }
        assert_eq!(
            intent.pointer("/event/evidence/memory_format"),
            Some(&Value::from(0))
        );
        assert_eq!(
            intent
                .pointer("/event/evidence/status")
                .and_then(Value::as_str),
            Some("possibly_changed")
        );
        let finished = records.last().ok_or("finished")?;
        assert_eq!(
            finished
                .pointer("/event/evidence/status")
                .and_then(Value::as_str),
            Some("verified_across_sessions")
        );
        assert_eq!(
            finished.pointer("/event/evidence/manual_recovery_may_be_required"),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            finished.pointer("/event/evidence/automatic_restore"),
            Some(&Value::Bool(false))
        );
        assert!(
            !fixture.failed.load(Ordering::Relaxed),
            "a fully synchronized history must not signal capture failure"
        );
        Ok(())
    }

    #[test]
    fn unwritten_finish_is_explicit_and_refuses_every_later_append() -> TestResult {
        let mut fixture = Fixture::new()?;
        let update = update()?;
        fixture.prepare(&update)?;
        fixture.journal.finish(&update)?;
        let prefix = fixture.bytes()?;
        let records = records(&prefix)?;
        let finished = records.last().ok_or("finished")?;
        assert_eq!(
            finished
                .pointer("/event/evidence/status")
                .and_then(Value::as_str),
            Some("not_written")
        );
        assert!(
            fixture.journal.evidence(&"too late").is_err(),
            "a finished journal must reject later evidence"
        );
        assert!(
            fixture.journal.intent(&update).is_err(),
            "a finished journal must reject a write intent"
        );
        assert!(
            fixture.journal.finish(&update).is_err(),
            "a finished journal must reject a second finish marker"
        );
        assert_eq!(fixture.bytes()?, prefix);
        Ok(())
    }

    #[test]
    fn verified_engine_cannot_replace_missing_durable_intent() -> TestResult {
        let mut fixture = Fixture::new()?;
        let mut update = update()?;
        fixture.prepare(&update)?;
        fresh(&mut update, 1)?;
        write_and_finalize(&mut update)?;
        finish_engine(&mut update)?;
        let prefix = fixture.bytes()?;
        assert!(
            fixture.journal.finish(&update).is_err(),
            "verified engine state must not replace a missing durable intent"
        );
        assert_eq!(fixture.bytes()?, prefix);
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "inconsistent final evidence must signal capture failure"
        );
        Ok(())
    }

    #[test]
    fn durable_intent_cannot_be_cleared_as_not_written() -> TestResult {
        let mut fixture = Fixture::new()?;
        let mut update = update()?;
        fixture.prepare(&update)?;
        fresh(&mut update, 1)?;
        fixture.journal.intent(&update)?;
        assert_eq!(update.status(), Pm1NameUpdateStatus::NotWritten);
        let prefix = fixture.bytes()?;
        assert!(
            fixture.journal.finish(&update).is_err(),
            "a recorded write intent must not be cleared as not written"
        );
        assert_eq!(fixture.bytes()?, prefix);
        Ok(())
    }

    #[test]
    fn failed_session_retains_possible_change_and_manual_recovery_warning() -> TestResult {
        let mut fixture = Fixture::new()?;
        let mut update = update()?;
        fixture.prepare(&update)?;
        fresh(&mut update, 1)?;
        fixture.journal.intent(&update)?;
        update.record(Pm1NameUpdateEvent::DurableWriteIntent { id: id(1)? })?;
        update.halt();
        fixture
            .journal
            .evidence(&serde_json::json!({"session": 1, "complete": false}))?;
        fixture.journal.finish(&update)?;
        let records = records(&fixture.bytes()?)?;
        let finished = records.last().ok_or("finished")?;
        assert_eq!(
            finished
                .pointer("/event/evidence/status")
                .and_then(Value::as_str),
            Some("possibly_changed")
        );
        assert_eq!(
            finished.pointer("/event/evidence/manual_recovery_may_be_required"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            finished.pointer("/event/evidence/automatic_restore"),
            Some(&Value::Bool(false))
        );
        Ok(())
    }

    #[test]
    fn scope_or_desired_name_changes_are_rejected_without_replacing_recovery_bytes() -> TestResult {
        let update = update()?;
        for change_page in [false, true] {
            let mut fixture = Fixture::new()?;
            fixture.prepare(&update)?;
            let mut page = *update.original_page();
            if change_page {
                *page.first_mut().ok_or("page byte")? ^= 1;
            }
            let desired = if change_page { "LOCAL" } else { "REMOTE" };
            let changed = Pm1NameUpdate::prepare(
                update.identity(),
                &page,
                update.current_name(),
                &Pm1Name::new(desired)?,
            )?;
            let prefix = fixture.bytes()?;
            assert!(
                fixture.journal.intent(&changed).is_err(),
                "a changed scope must be rejected; change_page={change_page}"
            );
            assert!(
                fixture.journal.intent(&update).is_err(),
                "scope rejection must remain sticky for the original update"
            );
            assert_eq!(fixture.bytes()?, prefix);
        }
        Ok(())
    }

    #[test]
    fn a_different_verified_update_cannot_finish_the_bound_journal() -> TestResult {
        let mut fixture = Fixture::new()?;
        let mut original = update()?;
        fixture.prepare(&original)?;
        fresh(&mut original, 1)?;
        fixture.journal.intent(&original)?;
        let mut changed = Pm1NameUpdate::prepare(
            original.identity(),
            original.original_page(),
            original.current_name(),
            &Pm1Name::new("REMOTE")?,
        )?;
        fresh(&mut changed, 1)?;
        write_and_finalize(&mut changed)?;
        finish_engine(&mut changed)?;
        assert_eq!(
            changed.status(),
            Pm1NameUpdateStatus::VerifiedAcrossSessions
        );
        let prefix = fixture.bytes()?;
        assert!(
            fixture.journal.finish(&changed).is_err(),
            "a different verified update must not finish the bound journal"
        );
        assert_eq!(fixture.bytes()?, prefix);
        Ok(())
    }

    #[test]
    fn out_of_order_calls_and_duplicate_intents_are_sticky() -> TestResult {
        let update = update()?;
        for prepare_first in [false, true] {
            let mut fixture = Fixture::new()?;
            if prepare_first {
                fixture.prepare(&update)?;
                fixture.journal.intent(&update)?;
            }
            let prefix = fixture.bytes()?;
            let error = fixture
                .journal
                .intent(&update)
                .err()
                .ok_or("invalid intent")?;
            let later = fixture.prepare(&update).err().ok_or("sticky error")?;
            assert_eq!(later.to_string(), error.to_string());
            assert_eq!(fixture.bytes()?, prefix);
        }
        Ok(())
    }

    #[test]
    fn preparation_directory_sync_failure_prevents_every_later_append() -> TestResult {
        let mut fixture = Fixture::new()?;
        let update = update()?;
        fixture.journal.directory = fixture.directory.path().join("missing-directory");
        let error = fixture
            .prepare(&update)
            .err()
            .ok_or("directory sync failure")?;
        let prefix = fixture.bytes()?;
        assert!(
            !prefix.is_empty(),
            "preparation must preserve its record before the injected directory-sync failure"
        );
        fixture.journal.directory = fixture.directory.path().canonicalize()?;
        let later = fixture
            .prepare(&update)
            .err()
            .ok_or("sticky directory failure")?;
        assert_eq!(later.kind(), error.kind());
        assert_eq!(later.to_string(), error.to_string());
        assert!(
            fixture.journal.intent(&update).is_err(),
            "directory-sync failure must block a later intent"
        );
        assert_eq!(fixture.bytes()?, prefix);
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "directory-sync failure must signal capture failure"
        );
        Ok(())
    }

    #[test]
    fn recorder_write_and_file_sync_failures_cannot_accept_an_intent() -> TestResult {
        let update = update()?;
        for sync_failure in [true, false] {
            let mut fixture = Fixture::new()?;
            fixture.prepare(&update)?;
            let result = if sync_failure {
                fixture.journal.intent_with_sync(&update, |_recorder| {
                    Err(io::Error::other("injected file synchronization failure"))
                })
            } else {
                fixture.journal.recorder = Recorder::named(
                    File::open(fixture.directory.path().join(FILENAME))?,
                    Arc::clone(&fixture.failed),
                    FILENAME,
                );
                fixture.journal.intent(&update)
            };
            let error = result.err().ok_or_else(|| {
                format!("intent accepted an injected storage failure; sync_failure={sync_failure}")
            })?;
            let prefix = fixture.bytes()?;
            assert_eq!(fixture.journal.stage, Stage::Prepared);
            let replacement = tempfile::NamedTempFile::new()?;
            fixture.journal.recorder =
                Recorder::named(replacement.reopen()?, Arc::clone(&fixture.failed), FILENAME);
            for result in [
                fixture.journal.intent(&update),
                fixture.journal.evidence(&"later evidence"),
                fixture.journal.finish(&update),
            ] {
                let later = result.err().ok_or("sticky recorder error")?;
                assert_eq!(later.kind(), error.kind());
                assert_eq!(later.to_string(), error.to_string());
            }
            assert_eq!(fs::metadata(replacement.path())?.len(), 0);
            assert_eq!(fixture.bytes()?, prefix);
            assert!(
                fixture.failed.load(Ordering::Relaxed),
                "storage failure must signal capture failure; sync_failure={sync_failure}"
            );
            assert_eq!(update.status(), Pm1NameUpdateStatus::NotWritten);
        }
        Ok(())
    }

    struct InvalidEvidence;

    impl Serialize for InvalidEvidence {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("injected serialization failure"))
        }
    }

    #[test]
    fn serialization_failure_preserves_the_prior_prefix_and_stops_later_appends() -> TestResult {
        let mut fixture = Fixture::new()?;
        let update = update()?;
        fixture.prepare(&update)?;
        let before = fixture.bytes()?;
        assert!(
            fixture.journal.evidence(&InvalidEvidence).is_err(),
            "invalid evidence must fail serialization"
        );
        let failed = fixture.bytes()?;
        assert!(
            failed.starts_with(&before),
            "serialization failure must preserve the preceding valid prefix"
        );
        assert!(
            fixture.journal.evidence(&"later valid evidence").is_err(),
            "serialization failure must block later evidence"
        );
        assert!(
            fixture.journal.finish(&update).is_err(),
            "serialization failure must block a finish marker"
        );
        assert_eq!(fixture.bytes()?, failed);
        assert!(
            fixture.failed.load(Ordering::Relaxed),
            "serialization failure must signal capture failure"
        );
        Ok(())
    }

    #[test]
    fn journal_is_private_exclusive_and_rejects_shared_or_symlink_directories() -> TestResult {
        let mut fixture = Fixture::new()?;
        fixture.prepare(&update()?)?;
        let path = fixture.directory.path().join(FILENAME);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let prefix = fixture.bytes()?;
        assert!(
            UpdateJournal::create(fixture.directory.path(), Arc::clone(&fixture.failed)).is_err(),
            "journal creation must not reopen an existing recovery file"
        );
        assert_eq!(fixture.bytes()?, prefix);
        let shared = fixture.directory.path().join("shared");
        fs::create_dir(&shared)?;
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755))?;
        assert!(
            UpdateJournal::create(&shared, Arc::clone(&fixture.failed)).is_err(),
            "journal creation must reject a shared directory"
        );
        assert!(
            !shared.join(FILENAME).exists(),
            "shared-directory rejection must precede file creation"
        );
        let link = fixture.directory.path().join("link");
        symlink(fixture.directory.path(), &link)?;
        assert!(
            UpdateJournal::create(&link, Arc::clone(&fixture.failed)).is_err(),
            "journal creation must reject a directory symlink"
        );
        assert_eq!(fixture.bytes()?, prefix);
        Ok(())
    }
}

#[cfg(not(unix))]
#[test]
fn unsupported_platform_refuses_creation_without_touching_the_directory()
-> Result<(), Box<dyn std::error::Error>> {
    use super::*;

    let directory = tempfile::tempdir()?;
    let result = UpdateJournal::create(directory.path(), Arc::new(AtomicBool::new(false)));
    assert!(
        matches!(&result, Err(error) if error.kind() == io::ErrorKind::Unsupported),
        "unsupported hosts must fail closed before file creation: {result:?}"
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
