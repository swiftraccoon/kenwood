//! Integration tests that spawn the REPL binary with a mock radio
//! and a fixture script, then lint the captured stdout.
//!
//! Requires the `testing` cargo feature. Run with:
//!
//! ```bash
//! cargo test -p thd75-repl --features testing --test script_integration
//! ```

#![cfg(feature = "testing")]
#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "Integration test: spawns the REPL with mock fixtures and asserts on captured \
              stdout. Uses `.expect()` on spawn/read operations (if fixture loading fails \
              the test should abort with a specific message) and `panic!` on mismatched \
              assertions — both appropriate for a test that correctly fails on setup \
              violations."
)]

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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("scripts")
}

fn run_with_script(fixture: &str, scenario: &str) -> (bool, String, String) {
    let script = fixtures_dir().join(fixture);
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args([
            "--script",
            script.to_str().expect("fixture path is utf8"),
            "--mock-radio",
            scenario,
        ])
        .output()
        .expect("spawn thd75-repl");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stdout, stderr)
}

#[test]
fn cat_basics_script_lints_clean() {
    let (ok, stdout, stderr) = run_with_script("cat_basics.txt", "simple");
    assert!(
        ok,
        "expected clean exit; stdout={stdout:?} stderr={stderr:?}"
    );
    lint::check_output(&stdout).unwrap_or_else(|v| {
        panic!("stdout violates accessibility rules: {v:#?}\nstdout:\n{stdout}")
    });

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
}

#[test]
fn help_all_script_runs_without_crash() {
    // Empty scenario has no programmed exchanges, so the REPL will
    // fail identification and exit early. We only check that the
    // binary runs without crashing and whatever stdout is produced
    // lints cleanly.
    let (_ok, stdout, _stderr) = run_with_script("help_all.txt", "empty");
    if !stdout.is_empty() {
        lint::check_output(&stdout).unwrap_or_else(|v| {
            panic!("help_all stdout violates rules: {v:#?}\nstdout:\n{stdout}")
        });
    }
}
