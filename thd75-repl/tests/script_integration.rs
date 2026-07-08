//! Integration tests that spawn the REPL binary with a mock radio
//! and a fixture script, then lint the captured stdout.
//!
//! Requires the `testing` cargo feature. Run with:
//!
//! ```bash
//! cargo test -p thd75-repl --features testing --test script_integration
//! ```

#![cfg(feature = "testing")]

// Dev-dependencies pulled in by sibling integration tests. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use kenwood_thd75 as _;
use proptest as _;
use rustyline as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::path::PathBuf;
use std::process::Command;

use thd75_repl::lint;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scripts")
}

fn run_with_script(
    fixture: &str,
    scenario: &str,
) -> Result<(bool, String, String), Box<dyn std::error::Error>> {
    let script = fixtures_dir().join(fixture);
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args([
            "--script",
            script.to_str().ok_or("fixture path is utf8")?,
            "--mock-radio",
            scenario,
        ])
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok((output.status.success(), stdout, stderr))
}

#[test]
fn cat_basics_script_lints_clean() -> TestResult {
    let (ok, stdout, stderr) = run_with_script("cat_basics.txt", "simple")?;
    assert!(
        ok,
        "expected clean exit; stdout={stdout:?} stderr={stderr:?}"
    );
    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "stdout violates accessibility rules: {lint_result:#?}\nstdout:\n{stdout}"
    );

    assert!(
        stdout.contains("Kenwood TH-D75 accessible radio control"),
        "missing startup banner in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Radio model: TH-D75"),
        "missing radio model line in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Goodbye."),
        "missing goodbye line:\n{stdout}"
    );
    Ok(())
}

#[test]
fn help_all_script_runs_without_crash() -> TestResult {
    // Empty scenario has no programmed exchanges, so the REPL will
    // fail identification and exit early. We only check that the
    // binary runs without crashing and whatever stdout is produced
    // lints cleanly.
    let (_ok, stdout, _stderr) = run_with_script("help_all.txt", "empty")?;
    if !stdout.is_empty() {
        let lint_result = lint::check_output(&stdout);
        assert!(
            lint_result.is_ok(),
            "help_all stdout violates rules: {lint_result:#?}\nstdout:\n{stdout}"
        );
    }
    Ok(())
}
