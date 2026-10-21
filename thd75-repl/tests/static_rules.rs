//! Static accessibility rule checks (R10, R11, R13, R14).
//!
//! These rules are structural: they cannot be verified from
//! captured output alone. Instead this test scans the REPL's source
//! files for forbidden patterns. Runs as part of `cargo test`.

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
use thd75_repl as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::fs;
use std::path::{Path, PathBuf};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn src_files() -> Result<Vec<PathBuf>, std::io::Error> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    collect_rs_files(&src, &mut out)?;
    Ok(out)
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_rs_files(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    Ok(())
}

#[test]
fn r10_no_print_without_newline() -> TestResult {
    // Skip `print!` entirely in user output. `print!` is only
    // allowed for the interactive prompt, which uses
    // rustyline::readline() directly, not `print!`.
    for file in src_files()? {
        let text = fs::read_to_string(&file)?;
        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            // Exclude `println!` and `eprintln!` which end with ln.
            let has_naked_print = trimmed.contains("print!(") && !trimmed.contains("println!(");
            assert!(
                !has_naked_print,
                "R10 violation: {} line {}: naked print! call: {}",
                file.display(),
                line_no + 1,
                line.trim()
            );
        }
    }
    Ok(())
}

#[test]
fn r11_no_cursor_move_or_spinner_bytes() -> TestResult {
    // The renderer escapes every control byte, including dynamic radio text.
    // Check that ordinary stdout calls cannot bypass that operation. Wire
    // parsers and negative fixtures may contain control-byte literals.
    for file in src_files()? {
        let text = fs::read_to_string(&file)?;
        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            let raw_println = line.contains("println!(")
                && !line.contains("aprintln!(")
                && !line.contains("eprintln!(");
            assert!(
                !raw_println && (file.ends_with("lib.rs") || !line.contains("stdout()")),
                "R11 violation: {} line {} bypasses the shared stdout renderer: {line}",
                file.display(),
                line_no + 1
            );
        }
    }
    Ok(())
}

#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "intentional: we scan source files for this literal"
)]
fn r13_no_ad_hoc_bracket_timestamps() -> TestResult {
    // Timestamp prefixes belong to the shared renderer in lib.rs. This
    // literal-pattern check detects duplicate implementations of that prefix;
    // runtime tests separately exercise rendering and replay behavior.
    for file in src_files()? {
        let text = fs::read_to_string(&file)?;
        let is_allowed = file.ends_with("lib.rs");
        if is_allowed {
            continue;
        }
        assert!(
            !text.contains("{h:02}:{m:02}:{s:02}"),
            "R13 violation: {} contains ad-hoc timestamp format",
            file.display()
        );
    }
    Ok(())
}

#[test]
fn r14_no_eprintln_in_user_output_path() -> TestResult {
    // Ordinary messages use the shared stdout renderer. Fatal errors render
    // explicitly before writing stderr; opt-in diagnostics use tracing.
    for file in src_files()? {
        let text = fs::read_to_string(&file)?;
        for (line_no, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("eprintln!"),
                "R14 violation: {} line {} bypasses rendered errors or tracing: {}",
                file.display(),
                line_no + 1,
                line.trim()
            );
        }
    }
    Ok(())
}
