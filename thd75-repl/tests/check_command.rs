//! Integration test for the `check` subcommand.
//!
//! Spawns the REPL binary with `check` as the subcommand argument
//! and asserts that the output-check report is emitted cleanly. This
//! test runs without the `testing` feature because `check` never
//! opens a radio transport.

// Crate-level dev-dependencies pulled in by sibling integration tests. Acknowledge
// them here so `unused_crate_dependencies` stays silent for this compilation unit.
use aprs_is as _;
use clap as _;
use dirs_next as _;
use dstar_gateway_core as _;
use kenwood_thd75 as _;
use kenwood_transport as _;
use mmdvm as _;
use proptest as _;
use rustyline as _;
use thd75_repl as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::process::Command;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn check_subcommand_exits_zero() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .arg("check")
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "check command exited with failure.\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("8 line rules passed; 6 rules require separate tests."),
        "expected evaluated and separate rule counts.\nstdout:\n{stdout}"
    );
    let timestamped = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args(["--timestamps", "check"])
        .output()?;
    assert!(timestamped.status.success(), "timestamped check failed");
    for line in String::from_utf8(timestamped.stdout)?.lines() {
        assert!(
            line.starts_with('[') && line.as_bytes().get(9) == Some(&b']'),
            "the check subcommand bypassed timestamp selection: {line}"
        );
        assert!(
            line.len() <= 80,
            "timestamped output exceeded its physical bound: {line}"
        );
    }
    Ok(())
}

#[test]
fn check_subcommand_does_not_claim_unevaluated_rules_passed() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .arg("check")
        .output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let count = stdout
        .matches("not evaluated; separate tests required.")
        .count();
    assert_eq!(count, 6, "unevaluated rules must be explicit: {stdout}");
    assert!(!stdout.contains("All 14 rules passed"), "{stdout}");
    assert!(
        !stdout.contains("Reference:"),
        "no external certification is evaluated: {stdout}"
    );
    Ok(())
}
