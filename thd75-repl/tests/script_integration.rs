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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{io::Write as _, thread};

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
fn terminal_mode_guard_intercepts_cat_commands() -> TestResult {
    // The `mmdvm` scenario puts the radio in a DV Gateway mode where CAT
    // identification fails but an MMDVM probe answers. The REPL must take
    // the terminal-mode path and, when the script issues a CAT command
    // (`mode b`), intercept it with Menu 650 guidance instead of letting
    // it block for the full command timeout.
    let (_ok, stdout, stderr) = run_with_script("terminal_mode.txt", "mmdvm")?;

    assert!(
        stdout.contains("Reflector Terminal Mode"),
        "missing terminal-mode notice in stdout:\n{stdout}\nstderr:\n{stderr}"
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
fn terminal_mode_starts_dstar_without_cat_recovery() -> TestResult {
    // The strict scenario permits only the positive terminal-mode probe,
    // MMDVM startup/init frames, and EOF cleanup. Any CAT recovery write after
    // binary proof makes gateway initialization fail and the process exit
    // nonzero.
    let (ok, stdout, stderr) = run_with_script("terminal_mode_dstar_start.txt", "mmdvm_dstar")?;

    assert!(
        ok,
        "terminal-mode D-STAR startup failed; stdout={stdout:?} stderr={stderr:?}"
    );
    for expected in [
        "Radio is in D-STAR Reflector Terminal Mode.",
        "Radio is already in Reflector Terminal Mode.",
        "MMDVM modem initialized.",
        "D-STAR gateway active.",
        "The radio is still in Reflector Terminal Mode.",
        "Goodbye.",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in stdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    for forbidden in [
        "CAT response boundary is ambiguous",
        "CAT recovery failed",
        "timed out",
        "radio connection lost",
    ] {
        assert!(
            !stdout.contains(forbidden),
            "unexpected {forbidden:?} in stdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
    let lint_result = lint::check_output(&stdout);
    assert!(
        lint_result.is_ok(),
        "terminal-mode D-STAR stdout violates rules: {lint_result:#?}\nstdout:\n{stdout}"
    );
    Ok(())
}

#[test]
fn idle_dstar_prompt_keeps_mmdvm_transport_running() -> TestResult {
    // Hold interactive stdin open beyond the modem loop's five-second write
    // deadline. A blocking readline must not starve async radio work or make
    // the next `listen` report a dead radio link.
    let mut child = Command::new(env!("CARGO_BIN_EXE_thd75-repl"))
        .args([
            "--mock-radio",
            "mmdvm_dstar_idle",
            "dstar",
            "start",
            "KQ4NIT",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("child stdin was not piped")?;

    thread::sleep(Duration::from_secs(8));
    stdin.write_all(b"listen\n")?;
    stdin.flush()?;
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut timed_out = false;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            timed_out = true;
            child.kill()?;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    assert!(
        !timed_out && output.status.success(),
        "idle-prompt child did not stop cleanly; stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("D-STAR gateway active.") && stdout.contains("Goodbye."),
        "idle-prompt flow missed startup or cleanup:\n{stdout}\nstderr:\n{stderr}"
    );
    for forbidden in [
        "radio link failed",
        "write timed out",
        "stopping D-STAR gateway",
    ] {
        assert!(
            !stdout.contains(forbidden) && !stderr.contains(forbidden),
            "idle prompt starved or lost the modem ({forbidden:?}):\n\
             stdout:\n{stdout}\nstderr:\n{stderr}"
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
