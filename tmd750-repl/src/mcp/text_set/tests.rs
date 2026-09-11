//! Parser and preflight checks never enumerate or open a radio connection.

use super::*;
use clap::Parser;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    request: SetRequest,
}

fn arguments() -> Vec<String> {
    [
        "mcp",
        "text",
        "set",
        "--backup",
        "backup report.json",
        "--expect",
        "PM1",
        "--apply",
        "pm-name-1",
        "Home",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn request(backup: PathBuf) -> Result<SetRequest, TestError> {
    Ok(SetRequest {
        backup,
        expected: Pm1Name::new("PM1")?,
        apply: true,
        output: None,
        setting: TextSetting::PmName1,
        value: Pm1Name::new("Home")?,
    })
}

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/never-open".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

#[test]
fn live_text_dispatch_requires_explicit_port_and_never_runs_offline() -> TestResult {
    let command = super::super::parse(&arguments())?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "set requires an explicitly pinned endpoint"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        super::super::run_offline(&command).is_none(),
        "live set must not be dispatched as an offline command"
    );
    Ok(())
}

#[test]
fn approval_expected_name_and_supported_scope_are_mandatory() -> TestResult {
    let mut without_approval = arguments();
    without_approval.retain(|word| word != "--apply");
    assert!(
        super::super::parse(&without_approval).is_err(),
        "CLI approval cannot default to true"
    );
    assert!(
        super::super::parse(
            &[
                "mcp",
                "text",
                "set",
                "--backup",
                "capture",
                "--apply",
                "pm-name-1",
                "Home"
            ]
            .map(str::to_owned)
        )
        .is_err(),
        "expected current name is required"
    );
    for setting in TextSetting::all()
        .iter()
        .copied()
        .filter(|setting| *setting != TextSetting::PmName1)
    {
        let mut candidate = request(PathBuf::from("missing.json"))?;
        candidate.setting = setting;
        assert!(
            candidate.validate_options().is_err(),
            "{setting} must remain offline-only"
        );
        assert!(
            candidate
                .prepare(&endpoint(), DEFAULT_BAUD)
                .is_err_and(|error| error.to_string().contains("no qualified live setter")),
            "unsupported scope must fail before backup access"
        );
    }
    for flag in ["--slot", "--address", "--force", "--interpret-unqualified"] {
        let mut candidate = arguments();
        candidate.extend([flag.to_owned(), "1".to_owned()]);
        assert!(
            super::super::parse(&candidate).is_err(),
            "{flag} cannot expand the live scope"
        );
    }
    Ok(())
}

#[test]
fn parser_preserves_case_spaces_and_paths_without_normalization() -> TestResult {
    let parsed = Arguments::try_parse_from([
        "text-set",
        "--backup",
        "Backup Dir/report.json",
        "--expect",
        " PM1 ",
        "--apply",
        "--output",
        "New Evidence",
        "pm-name-1",
        " Home /a ",
    ])?;
    assert_eq!(parsed.request.expected.as_str(), " PM1 ");
    assert_eq!(parsed.request.value.as_str(), " Home /a ");
    assert_eq!(
        parsed.request.backup,
        PathBuf::from("Backup Dir/report.json")
    );
    assert_eq!(parsed.request.output, Some(PathBuf::from("New Evidence")));
    Ok(())
}

#[test]
fn invalid_labels_and_no_change_are_rejected_before_any_io() -> TestResult {
    for value in ["", "ABCDEFGHIJKLMNOPQ", "home\n", "caf\u{e9}", "tab\tname"] {
        assert!(
            Arguments::try_parse_from([
                "text-set",
                "--backup",
                "missing.json",
                "--expect",
                "PM1",
                "--apply",
                "pm-name-1",
                value
            ])
            .is_err(),
            "invalid label {value:?} must fail during parsing"
        );
    }
    let mut candidate = request(PathBuf::from("missing.json"))?;
    candidate.value = candidate.expected.clone();
    assert!(
        candidate
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("no name change")),
        "no-op must not touch the backup or radio"
    );
    candidate.value = Pm1Name::new("Home")?;
    candidate.apply = false;
    assert!(
        candidate
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("explicit --apply")),
        "constructed requests also require approval"
    );
    Ok(())
}

#[test]
fn endpoint_and_baud_policy_precede_backup_access() -> TestResult {
    let candidate = request(PathBuf::from("missing.json"))?;
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
    ] {
        assert!(
            candidate
                .prepare(&selected, DEFAULT_BAUD)
                .is_err_and(|error| error.to_string().contains("main-unit USB")),
            "wrong USB interface must be refused before file access"
        );
    }
    assert!(
        candidate
            .prepare(&endpoint(), 115_200)
            .is_err_and(|error| error.to_string().contains("9600 baud")),
        "wrong baud must be refused before file access"
    );
    Ok(())
}

fn backup_fixture(path: &Path) -> AppResult<()> {
    let mut fixture = super::super::snapshot::tests::fixture();
    let segments = fixture
        .get_mut("backup")
        .and_then(|value| value.get_mut("segments"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks segments")?;
    let segment = segments
        .iter_mut()
        .find(|segment| segment.get("address") == Some(&serde_json::json!(323_584)))
        .ok_or("fixture lacks PM1 page")?;
    let mut page = vec![0x42_u8; 256];
    page.get_mut(10..26).ok_or("name range")?.fill(0);
    page.get_mut(10..13)
        .ok_or("name prefix")?
        .copy_from_slice(b"PM1");
    *segment.get_mut("data").ok_or("fixture lacks page data")? = serde_json::to_value(page)?;
    serde_json::to_writer(File::create_new(path)?, &fixture)?;
    Ok(())
}

#[test]
fn complete_backup_retains_all_unrelated_bytes_and_binds_expected_name() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("report.json");
    backup_fixture(&path)?;
    let mut candidate = request(path)?;
    let update = candidate.prepare(&endpoint(), DEFAULT_BAUD)?;
    assert_eq!(update.original_page().first(), Some(&0x42));
    assert_eq!(update.current_name().as_str(), "PM1");
    assert_eq!(update.desired_name().as_str(), "Home");
    for (index, (before, after)) in update
        .original_page()
        .iter()
        .zip(update.desired_page())
        .enumerate()
    {
        assert!(
            before == after || (10..26).contains(&index),
            "unrelated byte {index} must not change"
        );
    }
    candidate.expected = Pm1Name::new("Other")?;
    assert!(
        candidate.prepare(&endpoint(), DEFAULT_BAUD).is_err(),
        "expected current name must match actual captured bytes"
    );
    Ok(())
}
