//! Offline parsing and preflight tests; no host serial discovery or radio I/O.

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn arguments(extra: &[&str]) -> Vec<String> {
    [
        "mcp",
        "pm1-trial",
        "--backup",
        "backup report.json",
        "--confirmed-name",
        "PM1",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}

#[test]
fn approval_and_explicit_endpoint_are_required() -> TestResult {
    assert!(
        super::super::parse(&arguments(&[])).is_err(),
        "approval must be explicit"
    );
    let command = super::super::parse(&arguments(&["--approve-live-test"]))?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "automatic endpoint selection must fail"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        super::super::run_offline(&command).is_none(),
        "trial is not an offline text command"
    );
    Ok(())
}

#[test]
fn arbitrary_values_and_addresses_are_not_accepted() {
    for option in ["--value", "--address", "--force", "--slot"] {
        assert!(
            super::super::parse(&arguments(&["--approve-live-test", option, "1"])).is_err(),
            "arbitrary scope option {option} must fail"
        );
    }
}

#[test]
fn wrong_endpoint_or_baud_is_rejected_before_backup_access() {
    let request = TrialRequest {
        backup: PathBuf::from("does-not-exist.json"),
        confirmed_name: "PM1".to_owned(),
        approve_live_test: true,
        output: None,
    };
    let panel = SerialCandidate {
        path: "/dev/never-open".to_owned(),
        vid: Some(0x2166),
        pid: Some(0x9032),
    };
    let error = request.prepare(&panel, DEFAULT_BAUD);
    assert!(
        error.is_err_and(|error| error.to_string().contains("main-unit USB")),
        "panel rejection must precede file access"
    );
    let main = SerialCandidate {
        pid: Some(TMD750_MAIN_PID),
        ..panel
    };
    let error = request.prepare(&main, 115_200);
    assert!(
        error.is_err_and(|error| error.to_string().contains("9600 baud")),
        "baud rejection must precede file access"
    );
}

fn backup_fixture(path: &Path) -> AppResult<()> {
    let mut fixture = super::super::snapshot::tests::fixture();
    let segments = fixture
        .get_mut("backup")
        .and_then(|value| value.get_mut("segments"))
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("fixture lacks pages")?;
    let segment = segments
        .iter_mut()
        .find(|segment| segment.get("address") == Some(&serde_json::json!(323_584)))
        .ok_or("fixture lacks PM1 page")?;
    let mut page = vec![0x42_u8; 256];
    page.get_mut(10..26).ok_or("name range")?.fill(0);
    page.get_mut(10..13)
        .ok_or("name prefix")?
        .copy_from_slice(b"PM1");
    *segment.get_mut("data").ok_or("fixture lacks data")? = serde_json::to_value(page)?;
    serde_json::to_writer(File::create_new(path)?, &fixture)?;
    Ok(())
}

#[test]
fn successful_backup_supplies_exact_whole_page_but_not_operator_confirmation() -> TestResult {
    let directory = tempfile::tempdir()?;
    let backup = directory.path().join("backup report.json");
    backup_fixture(&backup)?;
    let mut request = TrialRequest {
        backup,
        confirmed_name: "PM1".to_owned(),
        approve_live_test: true,
        output: None,
    };
    let endpoint = SerialCandidate {
        path: "/dev/never-open".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    };
    let trial = request.prepare(&endpoint, DEFAULT_BAUD)?;
    assert_eq!(
        trial.original_page().get(10..26),
        Some(b"PM1\0\0\0\0\0\0\0\0\0\0\0\0\0".as_slice())
    );
    assert_eq!(trial.original_page().first(), Some(&0x42));
    request.confirmed_name = "PM2".to_owned();
    assert!(
        request.prepare(&endpoint, DEFAULT_BAUD).is_err(),
        "display mismatch must fail"
    );
    request.confirmed_name = "PM1".to_owned();
    request.approve_live_test = false;
    assert!(
        request.prepare(&endpoint, DEFAULT_BAUD).is_err(),
        "constructed requests cannot omit approval"
    );
    Ok(())
}
