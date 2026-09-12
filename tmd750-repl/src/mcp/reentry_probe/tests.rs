//! Offline CLI admission and exclusive capture reservation.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use kenwood_tmd750::transport::{KENWOOD_VID, SerialCandidate, TMD750_MAIN_PID, TMD750_PANEL_PID};

use super::{Request, Reserved};
use crate::mcp::capture::Artifacts;
use crate::mcp::{McpCommand, parse};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn endpoint() -> SerialCandidate {
    SerialCandidate {
        path: "/dev/cu.mock-reentry".to_owned(),
        vid: Some(KENWOOD_VID),
        pid: Some(TMD750_MAIN_PID),
    }
}

#[test]
fn approval_and_explicit_endpoint_are_both_required() -> TestResult {
    let arguments = ["mcp", "reentry-probe"].map(str::to_owned);
    assert!(
        parse(&arguments).is_err(),
        "an unapproved experiment must fail CLI parsing"
    );
    let approved = ["mcp", "reentry-probe", "--approve-live-test"].map(str::to_owned);
    let command = parse(&approved)?;
    assert!(
        matches!(command, McpCommand::ReentryProbe(_)),
        "select only the fixed workflow"
    );
    assert!(
        command.validate_endpoint_selection(false).is_err(),
        "approval cannot replace endpoint selection"
    );
    command.validate_endpoint_selection(true)?;
    assert!(
        crate::mcp::run_offline(&command).is_none(),
        "this is not an offline interpretation command"
    );
    Ok(())
}

#[test]
fn wrong_endpoint_baud_and_missing_approval_refuse_preparation() {
    let mut request = Request {
        approve_live_test: true,
        output: None,
    };
    let mut selected = endpoint();
    assert!(
        request.validate(&selected, 115_200).is_err(),
        "the experiment pins 9600 baud"
    );
    selected.pid = Some(TMD750_PANEL_PID);
    assert!(
        request.validate(&selected, 9600).is_err(),
        "the panel is outside this control's scope"
    );
    selected = endpoint();
    selected.vid = None;
    assert!(
        request.validate(&selected, 9600).is_err(),
        "missing USB metadata must fail closed"
    );
    request.approve_live_test = false;
    assert!(
        request.validate(&endpoint(), 9600).is_err(),
        "programmatic construction cannot bypass approval"
    );
}

#[test]
fn all_evidence_is_exclusively_reserved_with_no_extra_transcript() -> TestResult {
    let root = tempfile::tempdir()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let artifacts = Artifacts::create(Some(&root.path().join("probe")), Arc::clone(&cancelled))?;
    let directory = artifacts.directory;
    let reserved = Reserved::create(&directory, artifacts.transcript, &cancelled)?;
    let mut filenames = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    filenames.sort();
    let expected = [
        "post-exit-transcript.jsonl",
        "report.json",
        "serial-registry.jsonl",
        "session-2-post-exit-transcript.jsonl",
        "session-2-transcript.jsonl",
        "session-journal.jsonl",
        "transcript.jsonl",
    ]
    .map(std::ffi::OsString::from);
    assert_eq!(
        filenames, expected,
        "all four radio transcripts, journal, observer, and report must be reserved exactly once"
    );
    assert!(
        reserved.journal.summary().complete,
        "prepared journal must be complete"
    );
    let journal = std::fs::read_to_string(directory.join("session-journal.jsonl"))?;
    let prepared: serde_json::Value = serde_json::from_str(journal.trim_end())?;
    assert_eq!(
        prepared
            .pointer("/event/kind")
            .and_then(serde_json::Value::as_str),
        Some("prepared")
    );
    assert!(
        Artifacts::create(Some(&directory), cancelled).is_err(),
        "existing captures must not be overwritten"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&directory)?.permissions().mode() & 0o777,
            0o700
        );
        for entry in std::fs::read_dir(directory)? {
            assert_eq!(
                entry?.metadata()?.permissions().mode() & 0o777,
                0o600,
                "captured device information is private"
            );
        }
    }
    Ok(())
}
