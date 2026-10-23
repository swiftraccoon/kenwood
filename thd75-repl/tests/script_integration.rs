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
use aprs_is as _;
use clap as _;
use dirs_next as _;
use dstar_gateway_core as _;
use kenwood_thd75 as _;
use kenwood_transport as _;
use mmdvm as _;
use proptest as _;
use rustyline as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    run_with_script_args(fixture, scenario, &[])
}

/// Like [`run_with_script`] but appends extra CLI arguments, used by the
/// APRS transmit test, which needs `--yes` to clear the script-mode
/// transmit confirmation gate.
fn run_with_script_args(
    fixture: &str,
    scenario: &str,
    extra: &[&str],
) -> Result<(bool, String, String), Box<dyn std::error::Error>> {
    let script = fixtures_dir().join(fixture);
    let mut args = vec![
        "--script".to_string(),
        script.to_str().ok_or("fixture path is utf8")?.to_string(),
        "--mock-radio".to_string(),
        scenario.to_string(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_string()));
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args(&args)
        .output()?;
    let stdout = String::from_utf8_lossy_owned(output.stdout);
    let stderr = String::from_utf8_lossy_owned(output.stderr);
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
        stdout.contains("Operation band: A"),
        "missing band read line in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("USB audio output: Audio"),
        "missing USB output read line in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("Goodbye."),
        "missing goodbye line:\n{stdout}"
    );
    Ok(())
}

#[test]
fn piped_transmit_refusal_preserves_the_next_command() -> TestResult {
    let commands: [&[&str]; 3] = [
        &["cq"],
        &["aprs", "start", "N0CALL", "a", "7"],
        &["aprs", "start", "N0CALL", "a", "7", "digi"],
    ];
    for command in commands {
        let mut child = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
            .args(["--mock-radio", "simple"])
            .args(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child.stdin.take().ok_or("piped input was not attached")?;
        input.write_all(b"band\nifout\nid\nquit\n")?;
        drop(input);
        let result = child.wait_with_output()?;
        let stdout = String::from_utf8(result.stdout)?;
        let stderr = String::from_utf8(result.stderr)?;
        assert!(
            result.status.success(),
            "stdout={stdout:?}, stderr={stderr:?}"
        );
        assert!(
            stdout.contains("transmit commands require --yes in script mode"),
            "{stdout}"
        );
        assert!(
            stdout.contains("Operation band: A"),
            "confirmation consumed the next command: {stdout}"
        );
        assert!(
            stdout.contains("USB audio output: Audio"),
            "radio command order changed: {stdout}"
        );
        assert!(
            stdout.contains("Goodbye."),
            "the piped command stream did not finish: {stdout}"
        );
    }
    Ok(())
}

#[test]
fn terminal_mode_guard_intercepts_cat_commands() -> TestResult {
    // The `mmdvm` scenario puts the radio in a DV Gateway mode where CAT
    // identification fails but an MMDVM probe answers. The REPL must take
    // the terminal-mode path and, when the script issues a CAT command
    // (`mode b`), intercept it with Menu 650 guidance instead of letting
    // it block for the full command timeout.
    let (_ok, stdout, stderr) = run_with_script("terminal_mode.txt", "mmdvm")?;

    assert!(
        stdout.contains("DV Gateway/MMDVM mode"),
        "missing MMDVM-mode notice in stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("650"),
        "CAT command should be met with Menu 650 guidance, not a timeout:\n{stdout}"
    );
    assert!(
        !stdout.contains("timed out"),
        "CAT command timed out instead of being intercepted by the guard:\n{stdout}"
    );
    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "terminal-mode stdout violates rules: {lint_result:#?}\nstdout:\n{stdout}"
    );
    Ok(())
}

#[test]
fn terminal_mode_takeover_after_cat_proof_is_detected() -> TestResult {
    // CAT preparation can succeed during the Menu 650 transition window and
    // disappear while model and firmware qualification is still running. The
    // retained owner must still reach positive MMDVM diagnosis.
    let (_ok, stdout, stderr) = run_with_script("terminal_mode.txt", "mmdvm_takeover")?;

    assert!(
        stdout.contains("DV Gateway/MMDVM mode"),
        "missing MMDVM takeover notice in stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("650"),
        "post-CAT takeover should retain Menu 650 guidance:\n{stdout}"
    );
    assert!(
        !stdout.contains("timed out"),
        "post-CAT takeover timed out instead of reaching the MMDVM guard:\n{stdout}"
    );
    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "takeover stdout violates rules: {lint_result:#?}\nstdout:\n{stdout}"
    );
    Ok(())
}

#[test]
fn unsupported_dstar_start_stops_before_mock_construction_or_radio_writes() -> TestResult {
    let (ok, stdout, stderr) =
        run_with_script("terminal_mode_dstar_start.txt", "unknown-scenario")?;
    assert!(!ok, "unsupported startup must fail");
    assert!(stderr.contains("KenwoodTerminal"), "{stderr}");
    assert!(
        !stderr.contains("Unknown mock scenario"),
        "mock construction preceded preflight: {stderr}"
    );
    assert!(stdout.is_empty(), "radio startup must not run: {stdout}");
    Ok(())
}

#[test]
fn invalid_dstar_identity_stops_before_transport_discovery() -> TestResult {
    let output = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args([
            "--port",
            "/nonexistent-kenwood-preflight-port",
            "dstar",
            "start",
            "N0,CALL",
        ])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(!output.status.success(), "invalid identity must fail");
    assert!(stderr.contains("invalid station callsign"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "discovery and startup output must not run"
    );
    Ok(())
}

#[test]
fn cli_help_and_rejected_arguments_use_bounded_ascii_output() -> TestResult {
    let help = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .env("CLICOLOR_FORCE", "1")
        .arg("--help")
        .output()?;
    assert!(help.status.success(), "help must succeed");
    let help = String::from_utf8(help.stdout)?;
    assert!(help.contains("Usage:"), "{help}");
    for line in help.lines() {
        assert!(
            line.len() <= 80 && line.bytes().all(|byte| (32..=126).contains(&byte)),
            "{line:?}"
        );
    }
    let error = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .arg(format!("--unknown-{}{}", char::from(27), "x".repeat(100)))
        .output()?;
    assert!(!error.status.success(), "unknown CLI argument must fail");
    let error = String::from_utf8(error.stderr)?;
    assert!(error.starts_with("Error:"), "{error}");
    for line in error.lines() {
        assert!(
            line.len() <= 80 && line.bytes().all(|byte| (32..=126).contains(&byte)),
            "{line:?}"
        );
    }
    Ok(())
}

#[test]
fn aprs_transmit_surface_drives_every_command() -> TestResult {
    // Drive the full APRS transmit surface through the REPL against the
    // `aprs` mock scenario. `--yes` clears the script-mode transmit
    // gate so each command actually keys the (mock) radio. This proves
    // the dispatch glue end-to-end (argument parsing wired to the right
    // client method in the right order); the exact wire bytes per
    // format are pinned separately by the library unit tests.
    let (ok, stdout, stderr) = run_with_script_args("aprs_tx.txt", "aprs", &["--yes"])?;
    assert!(
        ok,
        "expected clean exit; stdout={stdout:?} stderr={stderr:?}"
    );

    for expected in [
        "Entering APRS mode as N0CALL-7",
        "Position beacon sent: 35.3000, -82.4600 (Portable).",
        "Compressed position beacon sent: 35.3000, -82.4600.",
        // Argument-wiring proof: lat, lon, speed, and course must all
        // land in the right slots; a swap would show here.
        "Mic-E beacon sent: 35.3000, -82.4600, 25 knots, course 90.",
        "Object TESTOBJ sent at 35.3100, -82.4500.",
        "Status sent: QRV testing",
        // Just "Motion updated.": whether SmartBeaconing also fires a
        // beacon depends on the shared beacon timer, which the earlier
        // position/compressed/mice sends have just reset, so no beacon
        // is due here. The beacon-timing logic itself is covered by the
        // library's SmartBeaconing tests.
        "Motion updated.",
        "APRS mode stopped. Returned to CAT mode.",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in stdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "APRS transmit stdout violates accessibility rules: {lint_result:#?}\nstdout:\n{stdout}"
    );
    Ok(())
}

#[test]
fn help_all_script_runs_without_crash() -> TestResult {
    let (ok, stdout, stderr) = run_with_script("help_all.txt", "startup")?;
    assert!(
        ok,
        "help dispatch failed: stdout={stdout:?}, stderr={stderr:?}"
    );
    assert!(
        stdout.contains("d75> help all"),
        "script did not reach help dispatch: {stdout}"
    );
    assert!(
        stdout.contains("Second argument controls PC serial output of NMEA sentences."),
        "help all omitted an unrelated command's detailed help: {stdout}"
    );
    assert!(
        stdout.contains("Syntax: confirm [on or off]"),
        "help all omitted confirmation help: {stdout}"
    );
    assert!(
        stdout.contains("Goodbye."),
        "help script did not finish: {stdout}"
    );
    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "help output violates rules: {lint_result:#?}\n{stdout}"
    );
    Ok(())
}
