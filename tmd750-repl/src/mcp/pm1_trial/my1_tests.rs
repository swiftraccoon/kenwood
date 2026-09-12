//! Fixed MY1 approval, capture coverage, and immutable recovery-scope contracts.

use super::*;
#[cfg(unix)]
use kenwood_tmd750::memory::PmNameTrialWrite;
use kenwood_tmd750::{FirmwareIdentity, Identity, RadioModel, RadioType};
use serde_json::Value;
use std::io::{BufWriter, Write};

type TestResult = AppResult<()>;

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/never-open-my1-fixture".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

pub(super) fn trial() -> AppResult<MyCallsignTrial> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut target = [0xA5; 256];
    target
        .get_mut(..2)
        .ok_or("gateway and selector bytes missing")?
        .fill(0);
    target.get_mut(8..16).ok_or("MY1 range missing")?.fill(0);
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM selector missing")? = 0;
    Ok(MyCallsignTrial::prepare_unqualified_offline(
        &identity, &target, &control,
    )?)
}

fn fixture() -> AppResult<Value> {
    let trial = trial()?;
    let mut document = crate::mcp::snapshot::tests::fixture();
    let segments = document
        .pointer_mut("/backup/segments")
        .and_then(Value::as_array_mut)
        .ok_or("fixture segments missing")?;
    for segment in segments {
        let address = segment
            .get("address")
            .and_then(Value::as_u64)
            .ok_or("fixture address missing")?;
        let data = match address {
            8 => {
                let mut bytes = vec![0x42; 40];
                *bytes.get_mut(2).ok_or("memory format byte missing")? = 0;
                Some(bytes)
            }
            323_584 => Some(trial.control_page().to_vec()),
            331_776 => Some(trial.original_page().to_vec()),
            _ => None,
        };
        if let Some(data) = data {
            *segment.get_mut("data").ok_or("fixture data missing")? = serde_json::to_value(data)?;
        }
    }
    Ok(document)
}

fn save_fixture(path: &Path, document: &Value) -> TestResult {
    let mut writer = BufWriter::new(File::create_new(path)?);
    serde_json::to_writer(&mut writer, document)?;
    writer.flush()?;
    Ok(())
}

fn request(path: &Path) -> My1TrialRequest {
    My1TrialRequest {
        backup: path.to_owned(),
        approve_live_test: true,
        output: None,
    }
}

fn mutate_byte(document: &mut Value, address: u64, offset: usize, value: u8) -> TestResult {
    let page = document
        .pointer_mut("/backup/segments")
        .and_then(Value::as_array_mut)
        .ok_or("fixture segments missing")?
        .iter_mut()
        .find(|segment| segment.get("address").and_then(Value::as_u64) == Some(address))
        .ok_or("fixture page missing")?;
    *page
        .get_mut("data")
        .and_then(Value::as_array_mut)
        .and_then(|bytes| bytes.get_mut(offset))
        .ok_or("fixture byte missing")? = Value::from(value);
    Ok(())
}

fn arguments(extra: &[&str]) -> Vec<String> {
    ["mcp", "my1-trial", "--backup", "backup report.json"]
        .into_iter()
        .chain(extra.iter().copied())
        .map(str::to_owned)
        .collect()
}

#[test]
fn my1_requires_approval_and_an_explicit_endpoint_and_is_never_offline() -> TestResult {
    assert!(
        crate::mcp::parse(&arguments(&[])).is_err(),
        "MY1 trial must require explicit approval"
    );
    let command = crate::mcp::parse(&arguments(&["--approve-live-test"]))?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "MY1 trial must not auto-select a radio"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        crate::mcp::run_offline(&command).is_none(),
        "MY1 is a live experiment, not an offline text preview"
    );
    Ok(())
}

#[test]
fn my1_parser_has_no_configurable_callsign_field_slot_or_address() {
    for option in [
        "--callsign",
        "--value",
        "--address",
        "--slot",
        "--field",
        "--confirmed-name",
        "--force",
        "--interpret-unqualified",
    ] {
        assert!(
            crate::mcp::parse(&arguments(&["--approve-live-test", option, "1"])).is_err(),
            "MY1 must reject configurable scope option {option}"
        );
    }
}

#[test]
fn my1_endpoint_baud_and_constructed_approval_fail_before_backup_access() {
    let mut request = request(Path::new("missing-my1-backup.json"));
    request.approve_live_test = false;
    assert!(
        request
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("approval")),
        "constructed requests cannot bypass approval"
    );
    request.approve_live_test = true;
    for candidate in [
        SerialCandidate {
            pid: Some(0x9032),
            ..endpoint()
        },
        SerialCandidate {
            vid: Some(0x1234),
            ..endpoint()
        },
        SerialCandidate {
            vid: None,
            pid: None,
            ..endpoint()
        },
    ] {
        assert!(
            request
                .prepare(&candidate, DEFAULT_BAUD)
                .is_err_and(|error| error.to_string().contains("main-unit USB")),
            "wrong endpoint must fail before trying the nonexistent backup"
        );
    }
    assert!(
        request
            .prepare(&endpoint(), 115_200)
            .is_err_and(|error| error.to_string().contains("9600 baud")),
        "wrong baud must fail before backup access"
    );
}

#[test]
fn complete_backup_preserves_both_pages_and_replaces_only_exact_empty_my1() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup report.json");
    save_fixture(&path, &fixture()?)?;
    let original = std::fs::read(&path)?;
    let prepared = request(&path).prepare(&endpoint(), DEFAULT_BAUD)?;
    let baseline = trial()?;
    assert_eq!(
        prepared.page().address().as_u32(),
        331_776,
        "MY1 target must be the fixed PM Off DV page"
    );
    assert_eq!(
        prepared.control_page_spec().address().as_u32(),
        323_584,
        "active PM must be guarded by its complete canonical page"
    );
    assert_eq!(
        prepared.original_page(),
        baseline.original_page(),
        "all original target bytes must survive"
    );
    assert_eq!(
        prepared.control_page(),
        baseline.control_page(),
        "all control bytes must survive"
    );
    assert_eq!(
        prepared.original_page().get(8..16),
        Some([0_u8; 8].as_slice()),
        "only an exactly empty MY1 baseline is admitted"
    );
    assert_eq!(
        prepared.expected_page().get(8..16),
        Some(b"KQ4NIT\0\0".as_slice()),
        "temporary callsign and NUL padding are fixed"
    );
    for (offset, (before, after)) in prepared
        .original_page()
        .iter()
        .zip(prepared.expected_page())
        .enumerate()
    {
        if !(8..16).contains(&offset) {
            assert_eq!(
                before, after,
                "unrelated target byte {offset} must remain identical"
            );
        }
    }
    assert_eq!(
        std::fs::read(&path)?,
        original,
        "preflight must not modify the source backup"
    );
    Ok(())
}

#[test]
fn my1_preflight_rejects_unknown_format_active_pm_gateway_selector_or_nonempty_text() -> TestResult
{
    for (address, offset, value) in [
        (8, 2, 1),
        (323_584, 9, 1),
        (331_776, 0, 2),
        (331_776, 1, 1),
        (331_776, 8, b'X'),
        (331_776, 15, b' '),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        mutate_byte(&mut document, address, offset, value)?;
        save_fixture(&path, &document)?;
        assert!(
            request(&path).prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "guard mutation {address}+{offset} must be rejected"
        );
    }
    Ok(())
}

#[test]
fn my1_preflight_requires_exact_firmware_and_complete_type_tuple() -> TestResult {
    for (component, value) in [
        ("firmware", "1.00"),
        ("firmware", "1.03"),
        ("radio_type", "K,2,2"),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        for base in [
            "/backup/identity",
            "/post_exit_verification/attempts/0/connection/identity",
        ] {
            *document
                .pointer_mut(&format!("{base}/{component}"))
                .ok_or("identity component missing")? = Value::from(value);
        }
        save_fixture(&path, &document)?;
        assert!(
            request(&path).prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "unqualified identity {component}={value} must fail"
        );
    }
    Ok(())
}

#[test]
fn my1_preflight_rejects_partial_or_unsuccessful_backup_lifecycles() -> TestResult {
    for (pointer, value) in [
        ("/backup/segments", serde_json::json!([])),
        ("/backup/complete_configuration", Value::Bool(false)),
        ("/format_version", Value::from(5)),
        (
            "/post_exit_verification/outcome/status",
            Value::from("failed"),
        ),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        *document
            .pointer_mut(pointer)
            .ok_or("fixture lifecycle field missing")? = value;
        save_fixture(&path, &document)?;
        assert!(
            request(&path).prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "ineligible backup {pointer} must not supply write scope"
        );
    }
    Ok(())
}

#[tokio::test]
async fn refused_my1_guard_creates_no_capture_directory_or_serial_access() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup.json");
    let mut document = fixture()?;
    mutate_byte(&mut document, 331_776, 0, 2)?;
    save_fixture(&path, &document)?;
    let mut request = request(&path);
    let output = directory.path().join("must-not-create");
    request.output = Some(output.clone());
    let original = std::fs::read(&path)?;
    let result = run_my1(&endpoint(), DEFAULT_BAUD, &request).await;
    assert!(
        result.is_err(),
        "active gateway must be refused by offline preflight"
    );
    assert!(
        !output.exists(),
        "offline refusal must precede artifact creation and connection setup"
    );
    assert_eq!(
        std::fs::read(&path)?,
        original,
        "refusal must preserve source evidence"
    );
    Ok(())
}

#[cfg(unix)]
fn private_directory() -> AppResult<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir()?;
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))?;
    Ok(directory)
}

#[cfg(unix)]
fn records(path: &Path) -> AppResult<Vec<Value>> {
    std::fs::read_to_string(path)?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[cfg(unix)]
#[test]
fn my1_prepared_and_intent_records_pin_control_target_and_fixed_callsign_without_display_claim()
-> TestResult {
    let directory = private_directory()?;
    let trial = trial()?;
    let mut journal = Journal::create(directory.path())?;
    journal.prepare(&trial, Path::new("source backup.json"), "")?;
    journal.intent(&trial, PmNameTrialWrite::Rename)?;
    journal.intent(&trial, PmNameTrialWrite::Restore)?;
    let records = records(&directory.path().join("trial-journal.jsonl"))?;
    assert_eq!(
        records.len(),
        3,
        "retain preparation and both fixed intents"
    );
    for record in &records {
        let scope = record.pointer("/evidence/scope").ok_or("scope missing")?;
        assert_eq!(
            scope.get("trial_kind"),
            Some(&Value::from("pm_off_my1")),
            "MY1 must be distinguished from PM1 name writes"
        );
        assert_eq!(
            scope.get("field"),
            Some(&Value::from("dstar-my-callsign-1")),
            "the only target is PM Off MY1"
        );
        assert_eq!(
            scope.get("page_address"),
            Some(&Value::from(331_776)),
            "target address must be fixed"
        );
        assert_eq!(
            scope.get("page_length"),
            Some(&Value::from(256)),
            "target recovery images must cover a whole page"
        );
        assert_eq!(
            scope.get("original_page"),
            Some(&serde_json::to_value(trial.original_page().as_slice())?),
            "complete original target bytes must be durable"
        );
        assert_eq!(
            scope.get("expected_page"),
            Some(&serde_json::to_value(trial.expected_page().as_slice())?),
            "complete expected target bytes must be durable"
        );
        assert_eq!(
            scope.get("temporary_name"),
            Some(&Value::from("KQ4NIT")),
            "temporary text is fixed"
        );
        assert_eq!(
            scope.pointer("/control_page/address"),
            Some(&Value::from(323_584)),
            "control page address must be recorded"
        );
        assert_eq!(
            scope.pointer("/control_page/length"),
            Some(&Value::from(256)),
            "control evidence cannot be field-only"
        );
        assert_eq!(
            scope.pointer("/control_page/data"),
            Some(&serde_json::to_value(trial.control_page().as_slice())?),
            "complete control page must be pinned"
        );
    }
    let prepared = records.first().ok_or("prepared record missing")?;
    assert!(
        prepared
            .pointer("/evidence/operator_confirmed_name")
            .is_none(),
        "do not invent independent display confirmation for MY1"
    );
    assert!(
        prepared
            .pointer("/evidence/baseline_confirmation")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("not independently observed")),
        "backup-based confirmation must state its evidence limit"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn my1_journal_rejects_swapped_target_or_control_pages_without_appending() -> TestResult {
    for change_control in [false, true] {
        let directory = private_directory()?;
        let baseline = trial()?;
        let mut target = *baseline.original_page();
        let mut control = *baseline.control_page();
        let bytes = if change_control {
            &mut control
        } else {
            &mut target
        };
        *bytes.get_mut(40).ok_or("unrelated byte missing")? ^= 1;
        let other =
            MyCallsignTrial::prepare_unqualified_offline(baseline.identity(), &target, &control)?;
        let mut journal = Journal::create(directory.path())?;
        journal.prepare(&baseline, Path::new("source.json"), "")?;
        let path = directory.path().join("trial-journal.jsonl");
        let original = std::fs::read(&path)?;
        assert!(
            journal.intent(&other, PmNameTrialWrite::Rename).is_err(),
            "changing target/control scope after preparation must fail; control={change_control}"
        );
        assert_eq!(
            std::fs::read(&path)?,
            original,
            "rejected scope must not append an intent"
        );
        assert!(
            journal.intent(&baseline, PmNameTrialWrite::Rename).is_err(),
            "a failed scope check must poison the journal"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn my1_journal_refuses_fabricated_display_confirmation() -> TestResult {
    let directory = private_directory()?;
    let mut journal = Journal::create(directory.path())?;
    assert!(
        journal
            .prepare(&trial()?, Path::new("source.json"), "KQ4NIT")
            .is_err(),
        "MY1 does not accept a fabricated PM1-style display claim"
    );
    assert_eq!(
        std::fs::metadata(directory.path().join("trial-journal.jsonl"))?.len(),
        0,
        "refused confirmation cannot produce a prepared record"
    );
    Ok(())
}
