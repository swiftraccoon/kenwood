//! `dstar probe` argument parsing: help output, the checks that reject a
//! request before any capture directory is created, and the extra arguments
//! `--manage-terminal` requires.

use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use kenwood_tmd750 as _;
use kenwood_transport as _;
use mmdvm as _;
#[cfg(unix)]
use nix as _;
#[cfg(unix)]
use rustix as _;
use rustyline as _;
use serde as _;
use serde_json as _;
use thiserror as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn help_is_available_without_endpoint_approval_or_artifacts() -> TestResult {
    let directory = tempfile::tempdir()?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args(["dstar", "probe", "--help"])
        .output()?;
    assert!(result.status.success(), "{result:?}");
    let text = String::from_utf8(result.stdout)?;
    assert!(text.contains("--approve-live-test"));
    assert!(text.contains("--output"));
    assert!(text.contains("--manage-terminal"));
    assert!(text.contains("--control-port"));
    assert!(text.contains("--backup"));
    assert!(text.contains("no modem setup"));
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn malformed_or_unapproved_diagnostics_stop_before_capture_creation() -> TestResult {
    for arguments in [
        vec!["dstar", "probe"],
        vec!["dstar", "probe", "--approve-live-test"],
        vec!["dstar", "probe", "--unknown"],
        vec!["dstar", "probe", "KQ4NIT", "REF030C"],
        vec![
            "--port",
            "/not/a/radio",
            "--baud",
            "115200",
            "dstar",
            "probe",
            "--approve-live-test",
        ],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args(arguments)
            .output()?;
        assert!(!result.status.success());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        let text = String::from_utf8(result.stderr)?;
        assert!(!text.contains("open failed"));
    }
    Ok(())
}

#[test]
fn incomplete_managed_scope_stops_before_endpoint_enumeration() -> TestResult {
    for options in [
        vec!["--manage-terminal"],
        vec!["--manage-terminal", "--control-port", "/not/a/control"],
        vec!["--manage-terminal", "--backup", "/not/a/backup.json"],
        vec!["--control-port", "/not/a/control"],
        vec!["--backup", "/not/a/backup.json"],
        vec![
            "--manage-terminal",
            "--control-port",
            "/not/a/modem",
            "--backup",
            "/not/a/backup.json",
        ],
        vec![
            "--manage-terminal",
            "--control-port",
            "",
            "--backup",
            "/not/a/backup.json",
        ],
        vec![
            "--manage-terminal",
            "--control-port",
            "/not/a/control",
            "--backup",
            "",
        ],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--port",
                "/not/a/modem",
                "dstar",
                "probe",
                "--approve-live-test",
            ])
            .args(&options)
            .output()?;
        assert!(!result.status.success(), "accepted {options:?}");
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        let stderr = String::from_utf8(result.stderr)?;
        assert!(!stderr.contains("enumerated"), "{stderr}");
        assert!(!stderr.contains("open failed"), "{stderr}");
    }
    Ok(())
}
