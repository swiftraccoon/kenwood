//! Integration test for the `check` subcommand.
//!
//! Spawns the REPL binary with `check` as the subcommand argument
//! and asserts that the compliance report is emitted cleanly. This
//! test runs without the `testing` feature because `check` never
//! opens a radio transport.

use std::process::Command;

#[test]
fn check_subcommand_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .arg("check")
        .output()
        .expect("spawn thd75-repl check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "check command exited with failure.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("All 14 rules passed"),
        "expected 'All 14 rules passed' in report.\nstdout:\n{stdout}"
    );
}

#[test]
fn check_subcommand_lists_nine_standards() {
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .arg("check")
        .output()
        .expect("spawn thd75-repl check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = stdout.matches("Standard:").count();
    assert_eq!(count, 9, "expected 9 standard lines, got {count}");
}
