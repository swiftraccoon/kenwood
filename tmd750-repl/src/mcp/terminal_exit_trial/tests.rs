//! Offline admission and exact command-scope contracts; no radio is opened.

use super::*;
use kenwood_tmd750::{FirmwareIdentity, Identity, RadioModel, RadioType};
use serde_json::Value;
use std::io::{BufWriter, Write};

type TestResult = AppResult<()>;

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/never-open-terminal-exit-fixture".to_owned(),
        vid: Some(0x2166),
        pid: Some(TMD750_MAIN_PID),
    }
}

fn trial() -> AppResult<TerminalExitTrial> {
    let identity = Identity {
        model: RadioModel::TmD750,
        firmware: FirmwareIdentity::new("1.02")?,
        radio_type: RadioType::new("K,2,1")?,
    };
    let mut target = [0xA5; 256];
    *target.first_mut().ok_or("Gateway byte")? = 0;
    *target.get_mut(2).ok_or("subtype byte")? = 0;
    let mut control = [0x5A; 256];
    *control.get_mut(9).ok_or("PM byte")? = 0;
    let mut routing = [0xC3; 256];
    *routing.get_mut(71).ok_or("USB function byte")? = 0;
    *routing.get_mut(77).ok_or("Gateway route byte")? = 1;
    Ok(TerminalExitTrial::prepare_unqualified_offline(
        &identity, &target, &control, &routing,
    )?)
}

fn fixture() -> AppResult<Value> {
    let trial = trial()?;
    let mut document = crate::mcp::snapshot::tests::fixture();
    let segments = document
        .pointer_mut("/backup/segments")
        .and_then(Value::as_array_mut)
        .ok_or("fixture segments")?;
    for segment in segments {
        let address = segment
            .get("address")
            .and_then(Value::as_u64)
            .ok_or("fixture address")?;
        let data = match address {
            8 => Some(vec![0; 40]),
            323_584 => Some(trial.control_page().to_vec()),
            328_960 => Some(trial.routing_page().to_vec()),
            331_776 => Some(trial.off_page().to_vec()),
            _ => None,
        };
        if let Some(data) = data {
            *segment.get_mut("data").ok_or("fixture data")? = serde_json::to_value(data)?;
        }
    }
    Ok(document)
}

fn save(path: &Path, document: &Value) -> TestResult {
    let mut writer = BufWriter::new(File::create_new(path)?);
    serde_json::to_writer(&mut writer, document)?;
    writer.flush()?;
    Ok(())
}

fn request(path: &Path) -> Request {
    Request {
        backup: path.to_owned(),
        approve_live_test: true,
        output: None,
    }
}

fn arguments(extra: &[&str]) -> Vec<String> {
    [
        "mcp",
        "terminal-exit-trial",
        "--backup",
        "backup report.json",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}

#[test]
fn parser_requires_exact_approval_and_explicit_endpoint() -> TestResult {
    assert!(
        crate::mcp::parse(&arguments(&[])).is_err(),
        "approval cannot default to true"
    );
    let command = crate::mcp::parse(&arguments(&["--approve-live-test"]))?;
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "no automatic endpoint selection"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        crate::mcp::run_offline(&command).is_none(),
        "the trial must never masquerade as offline inspection"
    );
    Ok(())
}

#[test]
fn parser_cannot_expand_the_fixed_write_scope() {
    for option in [
        "--mode",
        "--callsign",
        "--value",
        "--address",
        "--slot",
        "--field",
        "--force",
        "--interpret-unqualified",
        "--confirmed-name",
    ] {
        assert!(
            crate::mcp::parse(&arguments(&["--approve-live-test", option, "1"])).is_err(),
            "reject scope option {option}"
        );
    }
}

#[test]
fn endpoint_baud_and_constructed_approval_fail_before_reading_backup() {
    let mut request = request(Path::new("missing-terminal-exit-backup.json"));
    request.approve_live_test = false;
    assert!(
        request
            .prepare(&endpoint(), DEFAULT_BAUD)
            .is_err_and(|error| error.to_string().contains("approval")),
        "constructed requests must also require approval"
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
            "unsupported metadata must fail before backup access"
        );
    }
    assert!(
        request
            .prepare(&endpoint(), 115_200)
            .is_err_and(|error| error.to_string().contains("9600 baud")),
        "unsupported baud must fail before backup access"
    );
}

#[test]
fn complete_backup_preserves_all_three_pages_and_never_modifies_its_input() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("backup report.json");
    save(&path, &fixture()?)?;
    let original = std::fs::read(&path)?;
    let prepared = request(&path).prepare(&endpoint(), DEFAULT_BAUD)?;
    let expected = trial()?;
    assert_eq!(prepared.identity(), expected.identity());
    assert_eq!(prepared.off_page(), expected.off_page());
    assert_eq!(
        prepared.expected_active_page(),
        expected.expected_active_page()
    );
    assert_eq!(prepared.control_page(), expected.control_page());
    assert_eq!(prepared.routing_page(), expected.routing_page());
    assert_eq!(std::fs::read(path)?, original);
    Ok(())
}

#[test]
fn every_stored_scope_guard_is_required_before_opening() -> TestResult {
    for (address, offset, value) in [
        (8, 2, 1),
        (323_584, 9, 1),
        (331_776, 0, 2),
        (331_776, 2, 1),
        (328_960, 71, 1),
        (328_960, 77, 0),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        let page = document
            .pointer_mut("/backup/segments")
            .and_then(Value::as_array_mut)
            .ok_or("fixture segments")?
            .iter_mut()
            .find(|page| page.get("address").and_then(Value::as_u64) == Some(address))
            .ok_or("fixture target")?;
        *page
            .get_mut("data")
            .and_then(Value::as_array_mut)
            .and_then(|data| data.get_mut(offset))
            .ok_or("fixture byte")? = Value::from(value);
        save(&path, &document)?;
        assert!(
            request(&path).prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "refuse {address}+{offset}={value}"
        );
    }
    Ok(())
}

#[test]
fn incomplete_capture_cannot_prepare_a_live_exit() -> TestResult {
    for pointer in [
        "/transcript/complete",
        "/backup/complete_configuration",
        "/post_exit_verification/transcript/complete",
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("backup.json");
        let mut document = fixture()?;
        *document
            .pointer_mut(pointer)
            .ok_or("fixture completion field")? = Value::Bool(false);
        save(&path, &document)?;
        assert!(
            request(&path).prepare(&endpoint(), DEFAULT_BAUD).is_err(),
            "refuse incomplete {pointer}"
        );
    }
    Ok(())
}
