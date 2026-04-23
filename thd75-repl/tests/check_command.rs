//! Integration test for the `check` subcommand.
//!
//! Spawns the REPL binary with `check` as the subcommand argument
//! and asserts that the compliance report is emitted cleanly. This
//! test runs without the `testing` feature because `check` never
//! opens a radio transport.

#![expect(
    clippy::expect_used,
    reason = "Integration test. `.expect()` on `Command::output()` correctly aborts the \
              test with a specific message if the spawn itself fails — there's no useful \
              recovery path when the binary under test can't even be invoked."
)]

// Crate-level dev-dependencies pulled in by sibling integration tests. Acknowledge
// them here so `unused_crate_dependencies` stays silent for this compilation unit.
use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use kenwood_thd75 as _;
use proptest as _;
use rustyline as _;
use thd75_repl as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

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
