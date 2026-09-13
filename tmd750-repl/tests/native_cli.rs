//! Native command admission is exercised without opening any radio endpoint.

use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use kenwood_tmd750 as _;
use kenwood_transport as _;
use mmdvm as _;
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

fn normalized_text(bytes: &[u8]) -> Result<String, std::str::Utf8Error> {
    Ok(std::str::from_utf8(bytes)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" "))
}

#[test]
fn native_help_does_not_open_the_helper_or_create_artifacts() -> TestResult {
    for command in [vec!["--help"], vec!["mcp", "probe", "--help"]] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--bluetooth",
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
fn unsupported_native_operations_stop_before_helper_validation_or_capture() -> TestResult {
    for command in [
        vec![],
        vec!["dv", "a"],
        vec!["mode", "b", "fm"],
        vec!["mode", "a"],
        vec!["mcp", "backup"],
    ] {
        let directory = tempfile::tempdir()?;
        let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
            .current_dir(directory.path())
            .args([
                "--bluetooth",
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
                "--bluetooth",
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
            "--bluetooth",
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
                    "--bluetooth",
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
    use std::os::unix::fs::PermissionsExt;

    use kenwood_transport::TransportError;
    use kenwood_transport::error::BluetoothOpenStage;

    let directory = tempfile::tempdir()?;
    let helper = directory.path().join("no radio helper");
    // The private helper exit codec identifies an expired SDP callback budget.
    // This fixture only counts process launches; it cannot access Bluetooth.
    std::fs::write(&helper, "#!/bin/sh\nprintf . >> opens\nexit 103\n")?;
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
    let result = Command::new(env!("CARGO_BIN_EXE_tmd750-repl"))
        .current_dir(directory.path())
        .args(["--bluetooth", "01:23:45:67:89:AB", "--bluetooth-helper"])
        .arg(&helper)
        .args(["mcp", "probe", "--output", "observation"])
        .output()?;
    assert!(!result.status.success(), "{result:?}");
    assert_eq!(std::fs::read(directory.path().join("opens"))?, b"..");
    let expected = TransportError::BluetoothOpen {
        stage: BluetoothOpenStage::SdpDeadline,
    }
    .to_string();
    let stdout = String::from_utf8(result.stdout)?;
    let readable = stdout.split_whitespace().collect::<Vec<_>>().join(" ");
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
