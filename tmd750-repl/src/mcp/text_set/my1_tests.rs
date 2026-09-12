//! MY1 command admission uses only local arguments and synthetic backup files.

use clap::Parser;
use serde_json::Value;

use super::*;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    request: SetRequest,
}

fn request(backup: &Path, expected: &str, desired: &str) -> Result<SetRequest, TestError> {
    Ok(Arguments::try_parse_from([
        "text-set".as_ref(),
        "--backup".as_ref(),
        backup.as_os_str(),
        "--expect".as_ref(),
        expected.as_ref(),
        "--apply".as_ref(),
        "dstar-my-callsign-1".as_ref(),
        desired.as_ref(),
    ])?
    .request)
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/never-open-my1-tests".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn arguments() -> Vec<String> {
    [
        "mcp",
        "text",
        "set",
        "--backup",
        "missing-backup.json",
        "--expect",
        "",
        "--apply",
        "dstar-my-callsign-1",
        "KQ4NIT",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn page_mut(document: &mut Value, address: u32) -> Result<&mut Vec<Value>, TestError> {
    document
        .pointer_mut("/backup/segments")
        .and_then(Value::as_array_mut)
        .and_then(|segments| {
            segments
                .iter_mut()
                .find(|segment| segment.get("address") == Some(&Value::from(address)))
        })
        .and_then(|segment| segment.get_mut("data"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("fixture lacks page {address}").into())
}

fn fixture() -> Result<Value, TestError> {
    let mut document = super::super::snapshot::tests::fixture();
    *page_mut(&mut document, 8)?
        .get_mut(2)
        .ok_or("memory format")? = Value::from(0);
    let mut target = [0xA5_u8; 256];
    target
        .get_mut(0..2)
        .ok_or("Gateway and MY selection")?
        .fill(0);
    target.get_mut(8..16).ok_or("MY1 field")?.fill(0);
    *page_mut(&mut document, 331_776)? = target.into_iter().map(Value::from).collect();
    let control = page_mut(&mut document, 323_584)?;
    *control.get_mut(9).ok_or("PM selection")? = Value::from(0);
    Ok(document)
}

fn save(path: &Path, document: &Value) -> TestResult {
    serde_json::to_writer(File::create_new(path)?, document)?;
    Ok(())
}

#[test]
fn report_discriminants_separate_my1_updates_without_changing_pm1() {
    assert_eq!(
        (
            UpdateKind::PmOffMy1.format_version(),
            UpdateKind::PmOffMy1.operation()
        ),
        (6, "my1_callsign_update"),
        "MY1 reports must identify their distinct scope and Gateway verification contract"
    );
    assert_eq!(
        (
            UpdateKind::Pm1Name.format_version(),
            UpdateKind::Pm1Name.operation()
        ),
        (5, "pm1_name_update"),
        "adding MY1 must not change existing PM1 report discriminants"
    );
}

#[test]
fn empty_expected_field_is_admitted_but_empty_desired_value_is_not() -> TestResult {
    let candidate = request(Path::new("missing.json"), "", "KQ4NIT")?;
    candidate.validate_options()?;
    assert_eq!(
        candidate.expected, "",
        "empty expected text must denote eight NUL bytes"
    );
    for (expected, desired) in [("", ""), ("", "   "), ("   ", "KQ4NIT")] {
        let invalid = request(Path::new("missing.json"), expected, desired);
        assert!(
            invalid.map_or(true, |candidate| candidate.validate_options().is_err()),
            "invalid MY1 syntax must fail before backup access: {expected:?} -> {desired:?}"
        );
    }
    Ok(())
}

#[test]
fn my1_storage_syntax_rejects_normalization_candidates_before_io() {
    for value in [
        "kq4nit",
        "KQ4NIT/A",
        "ABCDEFGHI",
        "KQ\t4NIT",
        "KQ4NIT\n",
        "KQ4NIT\0",
        "CAF\u{c9}",
    ] {
        for expected_field in [false, true] {
            let (expected, desired) = if expected_field {
                (value, "N0CALL")
            } else {
                ("", value)
            };
            let candidate = request(Path::new("missing.json"), expected, desired);
            assert!(
                candidate.map_or(true, |candidate| candidate.validate_options().is_err()),
                "MY1 text {value:?} must not be normalized or accepted; expected_field={expected_field}"
            );
        }
    }
}

#[test]
fn exact_spaces_and_backup_paths_survive_argument_parsing() -> TestResult {
    let candidate = request(Path::new("Backup Dir/report.json"), " N0CALL ", " KQ4NIT ")?;
    candidate.validate_options()?;
    assert_eq!(
        candidate.expected, " N0CALL ",
        "expected spaces are storage bytes"
    );
    assert_eq!(
        candidate.value, " KQ4NIT ",
        "desired spaces must not be trimmed"
    );
    assert_eq!(
        candidate.backup,
        PathBuf::from("Backup Dir/report.json"),
        "backup paths must retain spaces"
    );
    Ok(())
}

#[test]
fn explicit_approval_and_endpoint_are_required_for_my1_dispatch() -> TestResult {
    let command = super::super::parse(&arguments())?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "MY1 cannot select a radio implicitly"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        super::super::run_offline(&command).is_none(),
        "MY1 set must remain a live command"
    );
    let mut missing_approval = arguments();
    missing_approval.retain(|word| word != "--apply");
    assert!(
        super::super::parse(&missing_approval).is_err(),
        "MY1 approval must be explicit in the command"
    );
    let mut missing_expected = arguments();
    missing_expected.retain(|word| word != "--expect" && !word.is_empty());
    assert!(
        super::super::parse(&missing_expected).is_err(),
        "an omitted expected field must not mean an explicitly approved empty field"
    );
    let mut candidate = request(Path::new("missing.json"), "", "KQ4NIT")?;
    candidate.apply = false;
    assert!(
        candidate
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("--apply")),
        "constructed requests must reject missing approval before backup access"
    );
    Ok(())
}

#[test]
fn slot_address_force_and_unrelated_settings_cannot_expand_my1_scope() -> TestResult {
    for flag in [
        "--slot",
        "--pm",
        "--address",
        "--force",
        "--interpret-unqualified",
    ] {
        let mut candidate = arguments();
        candidate.extend([flag.to_owned(), "1".to_owned()]);
        assert!(
            super::super::parse(&candidate).is_err(),
            "{flag} must not expand the fixed MY1 scope"
        );
    }
    for setting in TextSetting::all().iter().copied().filter(|setting| {
        !matches!(
            setting.to_string().as_str(),
            "pm-name-1" | "dstar-my-callsign-1"
        )
    }) {
        let mut candidate = request(Path::new("missing.json"), "", "KQ4NIT")?;
        candidate.setting = setting;
        assert!(
            candidate.validate_options().is_err(),
            "{setting} must remain outside the live setters"
        );
    }
    Ok(())
}

#[test]
fn no_op_and_unqualified_endpoint_fail_before_backup_access() -> TestResult {
    let candidate = request(Path::new("missing.json"), "KQ4NIT", "KQ4NIT")?;
    assert!(
        candidate.validate_options().is_err(),
        "an exact no-op must fail before any file or radio I/O"
    );
    let candidate = request(Path::new("missing.json"), "", "KQ4NIT")?;
    for selected in [
        SerialCandidate {
            pid: Some(0x9032),
            ..endpoint()
        },
        SerialCandidate {
            vid: None,
            pid: None,
            ..endpoint()
        },
        SerialCandidate {
            vid: Some(0xFFFF),
            ..endpoint()
        },
    ] {
        assert!(
            candidate
                .prepare(&selected, DEFAULT_BAUD)
                .is_err_and(|error| error.to_string().contains("main-unit USB")),
            "MY1 must reject an unqualified endpoint before opening the backup"
        );
    }
    assert!(
        candidate
            .prepare(&endpoint(), 115_200)
            .is_err_and(|error| error.to_string().contains("9600 baud")),
        "MY1 must reject unqualified baud before opening the backup"
    );
    Ok(())
}

#[test]
fn complete_backup_binds_both_pages_and_changes_only_the_eight_my1_bytes() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let document = fixture()?;
    save(&path, &document)?;
    let before = std::fs::read(&path)?;
    let candidate = request(&path, "", " KQ4NIT ")?;
    let PreparedUpdate::My1(update) = candidate.prepare(&endpoint(), DEFAULT_BAUD)? else {
        return Err("MY1 request prepared a different update kind".into());
    };
    assert_eq!(
        update.page().address().as_u32(),
        331_776,
        "MY1 target page must remain fixed"
    );
    assert_eq!(
        update.control_page_spec().address().as_u32(),
        323_584,
        "PM control page must remain fixed"
    );
    assert_eq!(
        update.current_callsign(),
        None,
        "eight captured NUL bytes mean no current callsign"
    );
    assert_eq!(
        update.desired_callsign().as_str(),
        " KQ4NIT ",
        "all eight desired bytes must survive preparation"
    );
    assert_eq!(
        update.desired_page().get(8..16),
        Some(b" KQ4NIT ".as_slice()),
        "MY1 must contain exact requested storage bytes"
    );
    for (offset, (original, desired)) in update
        .original_page()
        .iter()
        .zip(update.desired_page())
        .enumerate()
    {
        assert!(
            original == desired || (8..16).contains(&offset),
            "unrelated target byte {offset} must not change"
        );
    }
    for (offset, byte) in update.control_page().iter().copied().enumerate() {
        assert_eq!(
            byte,
            if offset == 9 { 0 } else { 0x42 },
            "control byte {offset} must be retained exactly"
        );
    }
    assert_eq!(
        std::fs::read(&path)?,
        before,
        "offline preparation must not rewrite its backup"
    );
    Ok(())
}

#[test]
fn existing_callsign_comparison_preserves_spaces_and_requires_exact_nul_padding() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let mut document = fixture()?;
    let target = page_mut(&mut document, 331_776)?;
    for (destination, byte) in target
        .get_mut(8..16)
        .ok_or("MY1 field")?
        .iter_mut()
        .zip(*b" N0CALL ")
    {
        *destination = Value::from(byte);
    }
    save(&path, &document)?;
    let candidate = request(&path, " N0CALL ", "KQ4NIT")?;
    let PreparedUpdate::My1(update) = candidate.prepare(&endpoint(), DEFAULT_BAUD)? else {
        return Err("MY1 replacement prepared a different update kind".into());
    };
    assert_eq!(
        update.current_callsign().map(My1Callsign::as_str),
        Some(" N0CALL "),
        "current spacing must not be normalized"
    );
    assert_eq!(
        update.desired_page().get(8..16),
        Some(b"KQ4NIT\0\0".as_slice()),
        "short desired callsigns require NUL padding"
    );
    for expected in ["", "N0CALL", " N0CALL", "N0CALL "] {
        let candidate = request(&path, expected, "KQ4NIT")?;
        assert!(
            candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "expected {expected:?} must not match differently spaced storage"
        );
    }
    Ok(())
}

#[test]
fn incomplete_failed_and_update_reports_cannot_be_used_as_backups() -> TestResult {
    let directory = tempfile::tempdir()?;
    for (fault, (pointer, value)) in [
        ("/format_version", Value::from(5)),
        ("/format_version", Value::from(6)),
        ("/operation", Value::from("text_update")),
        ("/backup/complete_configuration", Value::Bool(false)),
        ("/transcript/complete", Value::Bool(false)),
        ("/transcript/error", Value::from("capture failure")),
        ("/close_error", Value::from("close failure")),
        ("/signal_error", Value::from("signal failure")),
        ("/backup/exit", Value::from("not_acknowledged")),
        (
            "/post_exit_verification/outcome/status",
            Value::from("mismatched"),
        ),
        (
            "/post_exit_verification/attempts/0/connection/close/status",
            Value::from("failed"),
        ),
        (
            "/post_exit_verification/transcript/complete",
            Value::Bool(false),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut document = fixture()?;
        *document
            .pointer_mut(pointer)
            .ok_or("backup evidence pointer")? = value;
        let path = directory.path().join(format!("invalid-{fault}.json"));
        save(&path, &document)?;
        let candidate = request(&path, "", "KQ4NIT")?;
        assert!(
            candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "backup evidence {pointer} must be valid before MY1 preparation"
        );
    }
    Ok(())
}

#[test]
fn partial_reordered_or_missing_control_coverage_is_refused() -> TestResult {
    let directory = tempfile::tempdir()?;
    for fault in 0..4 {
        let mut document = fixture()?;
        let segments = document
            .pointer_mut("/backup/segments")
            .and_then(Value::as_array_mut)
            .ok_or("segments")?;
        match fault {
            0 | 1 => {
                let address = if fault == 0 { 323_584 } else { 331_776 };
                segments.retain(|segment| segment.get("address") != Some(&Value::from(address)));
            }
            2 => segments.swap(0, 1),
            _ => {
                let _removed = page_mut(&mut document, 323_584)?.pop();
            }
        }
        let path = directory.path().join(format!("coverage-{fault}.json"));
        save(&path, &document)?;
        let candidate = request(&path, "", "KQ4NIT")?;
        assert!(
            candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "incomplete or noncanonical coverage fault {fault} must fail"
        );
    }
    Ok(())
}

#[test]
fn every_identity_component_and_stored_scope_guard_is_required() -> TestResult {
    let directory = tempfile::tempdir()?;
    for (fault, (field, value)) in [
        ("model", "TH-D75"),
        ("firmware", "1.03"),
        ("radio_type", "K,2,2"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut document = fixture()?;
        for parent in [
            "/backup/identity",
            "/post_exit_verification/attempts/0/connection/identity",
        ] {
            *document
                .pointer_mut(&format!("{parent}/{field}"))
                .ok_or("identity field")? = Value::from(value);
        }
        let path = directory.path().join(format!("identity-{fault}.json"));
        save(&path, &document)?;
        assert!(
            request(&path, "", "KQ4NIT")?
                .prepare(&endpoint(), DEFAULT_BAUD)
                .is_err(),
            "changed {field} must not qualify the pinned MY1 setter"
        );
    }
    for (fault, (address, offset, value)) in [
        (8, 2, 1),
        (323_584, 9, 1),
        (331_776, 0, 1),
        (331_776, 0, 2),
        (331_776, 0, 255),
        (331_776, 1, 1),
        (331_776, 1, 255),
    ]
    .into_iter()
    .enumerate()
    {
        let mut document = fixture()?;
        *page_mut(&mut document, address)?
            .get_mut(offset)
            .ok_or("scope guard")? = Value::from(value);
        let path = directory.path().join(format!("guard-{fault}.json"));
        save(&path, &document)?;
        assert!(
            request(&path, "", "KQ4NIT")?
                .prepare(&endpoint(), DEFAULT_BAUD)
                .is_err(),
            "stored scope guard {address}+{offset}={value} must prevent preparation"
        );
    }
    Ok(())
}

#[test]
fn empty_expected_does_not_accept_erased_spaces_or_partially_populated_storage() -> TestResult {
    let directory = tempfile::tempdir()?;
    for (fault, bytes) in [[0xFF; 8], [b' '; 8], *b"N0CALL\0\0", *b"\0\0\0\0\0\0\0A"]
        .into_iter()
        .enumerate()
    {
        let mut document = fixture()?;
        for (destination, byte) in page_mut(&mut document, 331_776)?
            .get_mut(8..16)
            .ok_or("MY1 field")?
            .iter_mut()
            .zip(bytes)
        {
            *destination = Value::from(byte);
        }
        let path = directory.path().join(format!("current-{fault}.json"));
        save(&path, &document)?;
        assert!(
            request(&path, "", "KQ4NIT")?
                .prepare(&endpoint(), DEFAULT_BAUD)
                .is_err(),
            "empty expected must require eight NUL bytes, not storage case {fault}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn all_my1_output_artifacts_are_private_and_conflicts_preserve_existing_evidence() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    save(&path, &fixture()?)?;
    let candidate = request(&path, "", "KQ4NIT")?;
    let PreparedUpdate::My1(update) = candidate.prepare(&endpoint(), DEFAULT_BAUD)? else {
        return Err("MY1 artifact fixture prepared a different update kind".into());
    };
    let output = directory.path().join("private evidence");
    let failed = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(Some(&output), Arc::clone(&failed))?;
    let _post_exit = artifacts.reserve_post_exit(Arc::clone(&failed))?;
    let _verification = verification_captures(&output, &failed)?;
    let mut journal = UpdateJournal::create(&output, Arc::clone(&failed))?;
    journal.prepare(update.as_ref(), &path)?;
    assert_eq!(
        std::fs::metadata(&output)?.permissions().mode() & 0o777,
        0o700,
        "MY1 artifact directory must exclude other users"
    );
    for name in [
        "report.json",
        "transcript.jsonl",
        "post-exit-transcript.jsonl",
        "verify-transcript.jsonl",
        "verify-post-exit-transcript.jsonl",
        "update-journal.jsonl",
    ] {
        let file = output.join(name);
        assert_eq!(
            std::fs::metadata(&file)?.permissions().mode() & 0o777,
            0o600,
            "MY1 artifact {name} must be private"
        );
    }
    let prefix = std::fs::read(output.join("update-journal.jsonl"))?;
    assert!(
        Artifacts::create(Some(&output), failed).is_err(),
        "explicit output conflicts must never reuse existing evidence"
    );
    assert_eq!(
        std::fs::read(output.join("update-journal.jsonl"))?,
        prefix,
        "refusing an output conflict must leave existing MY1 recovery data unchanged"
    );
    Ok(())
}
