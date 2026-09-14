//! Native command admission is exercised without opening any radio endpoint.

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

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn normalized_text(bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
    Ok(std::str::from_utf8(bytes)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}

#[test]
fn native_help_does_not_open_the_helper_or_create_artifacts() -> TestResult {
    for command in [
        vec!["--help"],
        vec!["mcp", "probe", "--help"],
        vec!["mcp", "backup", "--help"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--bluetooth-address",
                "01:23:45:67:89:AB",
                "--bluetooth-helper",
                "/not/a/native/helper",
            ])
            .args(&command)
            .output()?;
        assert!(result.status.success(), "{command:?}: {result:?}");
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}

#[test]
fn automatic_bluetooth_local_commands_never_launch_discovery() -> TestResult {
    for command in [
        vec!["help"],
        vec!["terminal"],
        vec!["quit"],
        vec!["mcp", "menu", "list"],
        vec!["mcp", "text", "list"],
        vec!["mcp", "backup", "--help"],
        vec!["dstar", "probe", "--help"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args(["--bluetooth", "--bluetooth-helper", "/not/a/native/helper"])
            .args(&command)
            .output()?;
        assert!(result.status.success(), "{command:?}: {result:?}");
        assert_eq!(
            std::fs::read_dir(directory.path())?.count(),
            0,
            "local commands cannot create captures"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn closed_output_pipe_exits_unsuccessfully_without_panicking_or_discovery() -> TestResult {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let directory = tempfile::tempdir()?;
    let (writer, reader) = UnixStream::pair()?;
    drop(reader);
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .stdout(std::process::Stdio::from(OwnedFd::from(writer)))
        .args([
            "--bluetooth",
            "--bluetooth-helper",
            "/not/a/native/helper",
            "help",
        ])
        .output()?;
    assert!(
        !result.status.success(),
        "failed output cannot report success"
    );
    let error = normalized_text(&result.stderr)?;
    assert!(
        error.contains("stdout output failed"),
        "original stream failure is missing: {error}"
    );
    assert!(
        !error.contains("panicked"),
        "closed output must not unwind: {error}"
    );
    assert!(
        !error.contains("/not/a/native/helper"),
        "local help cannot discover radios: {error}"
    );
    assert_eq!(
        std::fs::read_dir(directory.path())?.count(),
        0,
        "no radio artifacts"
    );
    Ok(())
}

#[test]
fn unsupported_native_operations_stop_before_helper_validation_or_capture() -> TestResult {
    for command in [
        vec!["mcp", "reentry-probe", "--approve-live-test"],
        vec!["dstar", "probe"],
        vec!["dstar", "probe", "--approve-live-test"],
        vec![
            "mcp",
            "menu",
            "apply",
            "--backup",
            "/not/a/backup",
            "--apply",
            "beep",
            "1",
        ],
        vec![
            "mcp",
            "text",
            "set",
            "--backup",
            "/not/a/backup",
            "--expect",
            "OLD",
            "--apply",
            "pm-name-1",
            "NEW",
        ],
        vec![
            "mcp",
            "my1-trial",
            "--backup",
            "/not/a/backup",
            "--approve-live-test",
        ],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--bluetooth-address",
                "01:23:45:67:89:AB",
                "--bluetooth-helper",
                "/not/a/native/helper",
            ])
            .args(&command)
            .output()?;
        assert!(!result.status.success(), "{command:?}: {result:?}");
        let stderr = String::from_utf8(result.stderr)?;
        assert!(stderr.contains("native Bluetooth"), "{stderr}");
        assert!(!stderr.contains("/not/a/native/helper"), "{stderr}");
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn no_radio_helper(directory: &std::path::Path) -> TestResult<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let helper = directory.join("no radio helper");
    // Only record process launches and return the private SDP-deadline status.
    // This executable contains no Bluetooth or radio access implementation.
    std::fs::write(&helper, "#!/bin/sh\nprintf . >> opens\nexit 103\n")?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    Ok(helper)
}

#[cfg(target_os = "macos")]
fn inventory_only_helper(
    directory: &std::path::Path,
    records: &[(&str, &str)],
) -> TestResult<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let mut payload = b"KENWBT-READY-v2!".to_vec();
    for (address, name) in records {
        payload.extend_from_slice(&u16::try_from(address.len())?.to_be_bytes());
        payload.extend_from_slice(&u16::try_from(name.len())?.to_be_bytes());
        payload.extend_from_slice(address.as_bytes());
        payload.extend_from_slice(name.as_bytes());
    }
    payload.extend_from_slice(&[0; 4]);
    std::fs::write(directory.join("paired.bin"), payload)?;
    let helper = directory.join("inventory helper");
    // Emit only the local inventory fixture. Every attempted radio opening
    // records its exact selected address and fails before any radio access.
    std::fs::write(
        &helper,
        r#"#!/bin/sh
if [ "$KENWOOD_BT_HELPER_CONTROL_MODE" = paired ]; then
    printf . >> inventories
    /bin/cat paired.bin
    exit 0
fi
printf '%s\n' "$KENWOOD_BT_HELPER_DEVICE" >> selected
exit 103
"#,
    )?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    Ok(helper)
}

#[cfg(target_os = "macos")]
#[test]
fn automatic_bluetooth_uses_one_inventory_and_the_selected_helper_for_opening() -> TestResult {
    for command in [vec![], vec!["status"], vec!["mcp", "backup"]] {
        let directory = tempfile::tempdir()?;
        let helper = inventory_only_helper(
            directory.path(),
            &[("01:23:45:67:89:AB", "stm32mp1-ex5240")],
        )?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .stdin(std::process::Stdio::piped())
            .args(["--bluetooth", "--bluetooth-helper"])
            .arg(&helper)
            .args(&command)
            .output()?;
        assert!(
            !result.status.success(),
            "fake opening must fail: {command:?}"
        );
        assert_eq!(
            std::fs::read(directory.path().join("inventories"))?,
            b".",
            "one inventory selects the session endpoint"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("selected"))?,
            "01-23-45-67-89-AB\n01-23-45-67-89-AB\n",
            "both bounded attempts must use the selected address and fake helper"
        );
        let captures =
            std::fs::read_dir(directory.path().join("captures"))?.collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            captures.len(),
            1,
            "one requested operation creates one capture"
        );
        let capture = captures.first().ok_or("capture missing")?.path();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(capture.join("report.json"))?)?;
        assert_eq!(
            report.get("requested_address"),
            Some(&serde_json::json!("01-23-45-67-89-AB")),
            "automatic selection remains exact-address evidence"
        );
        assert_eq!(
            report.get("helper_executable"),
            Some(&serde_json::to_value(helper)?),
            "inventory and opening share the chosen helper"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn ambiguous_paired_radios_are_listed_without_attempting_any_open() -> TestResult {
    let directory = tempfile::tempdir()?;
    let helper = inventory_only_helper(
        directory.path(),
        &[
            ("01:23:45:67:89:AB", "TM-D750"),
            ("01:23:45:67:89:AC", "TM-D750"),
        ],
    )?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args(["--bluetooth", "--bluetooth-helper"])
        .arg(helper)
        .arg("status")
        .output()?;
    assert!(
        !result.status.success(),
        "ambiguous inventory must be refused"
    );
    let error = normalized_text(&result.stderr)?;
    for detail in [
        "ambiguous",
        "01-23-45-67-89-AB",
        "01-23-45-67-89-AC",
        "--bluetooth-address",
    ] {
        assert!(
            error.contains(detail),
            "actionable selection detail missing: {error}"
        );
    }
    assert!(
        !directory.path().join("selected").exists(),
        "no candidate may be opened"
    );
    assert!(
        !directory.path().join("captures").exists(),
        "selection failure has no radio capture"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_native_with_failed_helper(command: &[&str]) -> TestResult<serde_json::Value> {
    let directory = tempfile::tempdir()?;
    let helper = no_radio_helper(directory.path())?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        // A real pipe with EOF exercises prompt setup without a terminal or
        // inherited stdin. Startup mode commands must not read it at all.
        .stdin(std::process::Stdio::piped())
        .args([
            "--bluetooth-address",
            "01:23:45:67:89:AB",
            "--bluetooth-helper",
        ])
        .arg(&helper)
        .args(command)
        .output()?;
    assert!(!result.status.success(), "{command:?}: {result:?}");
    assert_eq!(
        std::fs::read(directory.path().join("opens"))?,
        b"..",
        "native CLI admission must reach exactly two bounded fake-helper attempts"
    );
    let captures =
        std::fs::read_dir(directory.path().join("captures"))?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        captures.len(),
        1,
        "one CLI operation must create one capture"
    );
    let capture = captures.first().ok_or("capture missing")?.path();
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(capture.join("report.json"))?)?;
    assert_eq!(
        report.get("transport"),
        Some(&serde_json::json!("native_bluetooth")),
        "native commands must retain their selected transport provenance"
    );
    assert_eq!(
        report.get("requested_address"),
        Some(&serde_json::json!("01-23-45-67-89-AB")),
        "native commands must retain the exact normalized address selection"
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(capture.join("transcript.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let kinds: Vec<_> = events
        .iter()
        .map(|event| {
            event
                .pointer("/event/kind")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            Some("native_open_requested"),
            Some("native_open_failed"),
            Some("native_open_retry_wait"),
            Some("native_open_retry_wait_completed"),
            Some("native_open_requested"),
            Some("native_open_failed"),
        ],
        "CLI admission must not send CAT or enter another transport workflow"
    );
    Ok(report)
}

#[cfg(target_os = "macos")]
#[test]
fn native_prompt_and_mode_startup_commands_reach_captured_sessions() -> TestResult {
    for (command, mode) in [
        (vec![], "batch"),
        (vec!["mode", "b"], "startup"),
        (vec!["MODE b"], "startup"),
        (vec!["DV", "a"], "startup"),
        (vec!["FM", "b"], "startup"),
    ] {
        let report = run_native_with_failed_helper(&command)?;
        assert_eq!(
            report.get("operation"),
            Some(&serde_json::json!("cat_session")),
            "{command:?}"
        );
        assert_eq!(report.get("format_version"), Some(&serde_json::json!(1)));
        assert_eq!(report.get("mode"), Some(&serde_json::json!(mode)));
        assert_eq!(
            report.pointer("/outcome/session"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            report.pointer("/outcome/endpoint"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            report.get("input_close_error"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(report.get("signal_error"), Some(&serde_json::Value::Null));
        assert_eq!(
            report
                .pointer("/outcome/opening/attempts")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(2)
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_backup_startup_reaches_its_distinct_configuration_report() -> TestResult {
    let report = run_native_with_failed_helper(&["mcp", "backup"])?;
    assert_eq!(
        report.get("operation"),
        Some(&serde_json::json!({"kind":"configuration_backup"}))
    );
    assert_eq!(report.get("format_version"), Some(&serde_json::json!(3)));
    for field in [
        "original",
        "original_endpoint",
        "fresh_cat",
        "fresh_opening",
        "fresh_endpoint",
    ] {
        assert_eq!(
            report.pointer(&format!("/workflow/{field}")),
            Some(&serde_json::Value::Null)
        );
    }
    assert_eq!(
        report
            .pointer("/workflow/original_opening/attempts")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2)
    );
    Ok(())
}

#[test]
fn automatic_start_validates_arguments_before_any_endpoint_or_capture() -> TestResult {
    for command in [
        vec!["dstar", "start"],
        vec!["dstar", "start", "INVALIDCALLSIGN"],
        vec!["dstar", "start", "KQ4NIT", "REF030!"],
        vec!["dstar", "start", "KQ4NIT", "REF030C", "extra"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--bluetooth-address",
                "01:23:45:67:89:AB",
                "--bluetooth-helper",
                "/not/a/native/helper",
            ])
            .args(command)
            .output()?;
        assert!(!result.status.success(), "{result:?}");
        assert!(!String::from_utf8(result.stderr)?.contains("/not/a/native/helper"));
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn automatic_start_requires_exact_usb_control_before_bluetooth_opening() -> TestResult {
    let directory = tempfile::tempdir()?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args([
            "--bluetooth-address",
            "01:23:45:67:89:AB",
            "--bluetooth-helper",
            "/not/a/native/helper",
            "--control-port",
            "/not/a/usb/radio",
            "dstar",
            "start",
            "KQ4NIT",
            "REF030C",
        ])
        .output()?;
    assert!(!result.status.success(), "{result:?}");
    let stderr = String::from_utf8(result.stderr)?;
    assert!(
        !stderr.contains("supports only"),
        "D-STAR must be admitted by the CLI: {stderr}"
    );
    assert!(
        !stderr.contains("/not/a/native/helper"),
        "USB selection must precede helper validation: {stderr}"
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn automatic_start_routes_aliases_case_and_quoted_commands_identically() -> TestResult {
    for explicit_bluetooth in [false, true] {
        for command in [
            vec!["dstar", "start", "KQ4NIT", "REF030C"],
            vec!["d-star", "start", "KQ4NIT", "REF030C"],
            vec!["D-STAR", "START", "KQ4NIT", "REF030C"],
            vec!["dstar start KQ4NIT REF030C"],
            vec!["D-STAR START KQ4NIT REF030C"],
            vec!["dstar start", "KQ4NIT", "REF030C"],
        ] {
            let directory = tempfile::tempdir()?;
            let mut process = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"));
            let _configured = process.current_dir(directory.path());
            if explicit_bluetooth {
                let _configured = process.args([
                    "--bluetooth-address",
                    "01:23:45:67:89:AB",
                    "--bluetooth-helper",
                    "/not/a/native/helper",
                ]);
            }
            let result = process
                .args(["--control-port", "/not/a/usb/radio"])
                .args(&command)
                .output()?;
            assert!(!result.status.success(), "{command:?}: {result:?}");
            let stderr = normalized_text(&result.stderr)?;
            assert!(
                stderr.contains(
                    "one unambiguous enumerated TM-D750 USB endpoint at /not/a/usb/radio"
                ),
                "parsed startup must reach exact USB admission: {command:?}: {stderr}"
            );
            assert!(!stderr.contains("/not/a/native/helper"), "{stderr}");
            assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        }
    }
    Ok(())
}

#[test]
fn automatic_control_and_manual_port_conflict_before_io() -> TestResult {
    let directory = tempfile::tempdir()?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args([
            "--control-port",
            "/not/a/control/port",
            "--port",
            "/not/a/manual/port",
            "dstar start KQ4NIT REF030C",
        ])
        .output()?;
    assert!(!result.status.success(), "{result:?}");
    let stderr = normalized_text(&result.stderr)?;
    assert!(stderr.contains("cannot be used with"), "{stderr}");
    assert!(
        stderr.contains("--control-port") && stderr.contains("--port"),
        "{stderr}"
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn automatic_start_rejects_custom_baud_before_endpoint_discovery() -> TestResult {
    for command in [
        vec!["dstar", "start", "KQ4NIT", "REF030C"],
        vec!["D-STAR START KQ4NIT REF030C"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args(["--baud", "19200", "--control-port", "/not/a/usb/radio"])
            .args(&command)
            .output()?;
        assert!(!result.status.success(), "{command:?}: {result:?}");
        let stderr = normalized_text(&result.stderr)?;
        assert!(
            stderr.contains("custom --baud requires an explicit USB --port"),
            "{stderr}"
        );
        assert!(!stderr.contains("/not/a/usb/radio"), "{stderr}");
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}

#[test]
fn explicit_port_keeps_manual_policy_for_each_accepted_command_form() -> TestResult {
    for command in [
        vec!["dstar", "start", "KQ4NIT", "REF030C"],
        vec!["D-STAR START KQ4NIT REF030C"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args(["--port", "/not/a/manual/port", "--baud", "19200"])
            .args(&command)
            .output()?;
        assert!(!result.status.success(), "{command:?}: {result:?}");
        let stderr = normalized_text(&result.stderr)?;
        assert!(stderr.contains("/not/a/manual/port"), "{stderr}");
        assert!(!stderr.contains("custom --baud requires"), "{stderr}");
        assert!(
            !stderr.contains("USB endpoint") && !stderr.contains("paired"),
            "{stderr}"
        );
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn failed_sdp_deadline_is_visible_and_captured_without_radio_traffic() -> TestResult {
    use kenwood_transport::TransportError;
    use kenwood_transport::error::BluetoothOpenStage;

    let directory = tempfile::tempdir()?;
    let helper = no_radio_helper(directory.path())?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args([
            "--bluetooth-address",
            "01:23:45:67:89:AB",
            "--bluetooth-helper",
        ])
        .arg(&helper)
        .args(["mcp", "probe", "--output", "observation"])
        .output()?;
    assert!(!result.status.success(), "{result:?}");
    assert_eq!(std::fs::read(directory.path().join("opens"))?, b"..");
    let expected = TransportError::BluetoothOpen {
        stage: BluetoothOpenStage::SdpDeadline,
    }
    .to_string();
    let readable = normalized_text(&result.stderr)?;
    assert!(readable.contains(&expected), "{readable}");
    let capture = directory.path().join("observation");
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(capture.join("report.json"))?)?;
    assert_eq!(report.get("format_version"), Some(&serde_json::json!(2)));
    assert_eq!(
        report.get("maximum_original_open_attempts"),
        Some(&serde_json::json!(2))
    );
    assert_eq!(
        report.get("open_retry_delay_milliseconds"),
        Some(&serde_json::json!(1000))
    );
    let workflow = report.get("workflow").ok_or("native workflow absent")?;
    assert_eq!(
        workflow
            .pointer("/original_opening/attempts/0/error/error/message")
            .and_then(serde_json::Value::as_str),
        Some(expected.as_str())
    );
    assert_eq!(
        workflow
            .pointer("/original_opening/attempts/1/error/error/message")
            .and_then(serde_json::Value::as_str),
        Some(expected.as_str())
    );
    assert_eq!(
        workflow
            .pointer("/original_opening/attempts")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2)
    );
    for field in [
        "original_endpoint",
        "original",
        "fresh_endpoint",
        "fresh_opening",
        "fresh_cat",
    ] {
        assert_eq!(workflow.get(field), Some(&serde_json::Value::Null));
    }
    let events: Vec<serde_json::Value> = std::fs::read_to_string(capture.join("transcript.jsonl"))?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    let kinds: Vec<_> = events
        .iter()
        .map(|event| {
            event
                .pointer("/event/kind")
                .and_then(serde_json::Value::as_str)
        })
        .collect();
    assert_eq!(
        kinds,
        [
            Some("native_open_requested"),
            Some("native_open_failed"),
            Some("native_open_retry_wait"),
            Some("native_open_retry_wait_completed"),
            Some("native_open_requested"),
            Some("native_open_failed"),
        ]
    );
    assert!(std::fs::read(capture.join("post-exit-transcript.jsonl"))?.is_empty());
    Ok(())
}
