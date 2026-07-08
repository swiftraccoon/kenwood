//! Static accessibility rule checks (R10, R11, R13, R14).
//!
//! These rules are structural — they cannot be verified from
//! captured output alone. Instead this test scans the REPL's source
//! files for forbidden patterns. Runs as part of `cargo test`.

// Dev-dependencies pulled in by sibling integration tests. Acknowledge them here so
// `unused_crate_dependencies` stays silent for this compilation unit.
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
    // rustyline::readline() directly — not `print!`.
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
    // Scan for `\x1b[`, `\r` (carriage return), and `\x08` (backspace)
    // inside string literals. These are the three ways to move the
    // cursor or overwrite content, which screen readers cannot handle.
    //
    // Iterate line by line so we can skip comments that legitimately
    // describe these patterns as part of rule documentation.
    for file in src_files()? {
        // This test file itself mentions these escape sequences in prose.
        if file.ends_with("static_rules.rs") {
            continue;
        }
        // lint.rs has unit tests whose inputs deliberately contain
        // `\x1b[` and `\r` to exercise the lint rule that rejects them.
        if file.ends_with("lint.rs") {
            continue;
        }
        // mock_scenarios.rs programs CAT wire bytes into a mock
        // transport. Those bytes are never printed to stdout, so the
        // `\r` they contain does not violate the cursor-move rule.
        if file.ends_with("mock_scenarios.rs") {
            continue;
        }
        let text = fs::read_to_string(&file)?;
        for (line_no, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            assert!(
                !line.contains("\\x1b["),
                "R11 violation: {} line {}: contains \\x1b[ (ANSI cursor/color)",
                file.display(),
                line_no + 1
            );
            assert!(
                !line.contains("\\r"),
                "R11 violation: {} line {}: contains \\r (carriage return)",
                file.display(),
                line_no + 1
            );
            assert!(
                !line.contains("\\x08"),
                "R11 violation: {} line {}: contains \\x08 (backspace)",
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
    // `[HH:MM:SS]` timestamps should only be produced by the
    // `aprintln!` macro. Anywhere else in the code is an ad-hoc
    // timestamp that won't get the verbose/quiet treatment right.
    //
    // We scan for the literal format pattern used by the timestamp
    // macro today (two-digit hours, minutes, seconds). Only `lib.rs`
    // (where the `aprintln!` macro lives) is allowed.
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

/// Line spans (0-based, inclusive) of the named top-level functions,
/// found by counting braces from each `fn <name>(` declaration line
/// until the body's depth returns to zero.
///
/// Brace counting ignores braces inside string literals and comments;
/// that is fine here because format strings keep their braces paired,
/// so imbalance from literals cannot occur in this crate's source. A
/// span that ends early would only make the check stricter (an
/// `eprintln!` would fall outside it and fail loudly), never looser.
fn function_spans(lines: &[&str], names: &[&str]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut line_no = 0;
    while line_no < lines.len() {
        let is_decl = lines
            .get(line_no)
            .is_some_and(|current| names.iter().any(|n| current.contains(&format!("fn {n}("))));
        if !is_decl {
            line_no += 1;
            continue;
        }
        let mut depth = 0i64;
        let mut body_started = false;
        let mut end = line_no;
        for (j, line) in lines.iter().enumerate().skip(line_no) {
            for c in line.chars() {
                match c {
                    '{' => {
                        depth += 1;
                        body_started = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if body_started && depth <= 0 {
                end = j;
                break;
            }
        }
        spans.push((line_no, end));
        line_no = end + 1;
    }
    spans
}

#[test]
fn r14_no_eprintln_in_user_output_path() -> TestResult {
    // User-facing output goes to stdout via println!/aprintln!.
    // Diagnostics go to stderr via `tracing`. `eprintln!` is a
    // code smell — it bypasses tracing and bypasses stdout. The
    // exceptions, checked by actual function span rather than "the
    // declaration appears somewhere earlier in the file" (which
    // exempted everything below `init_logging` in main.rs):
    // - `init_logging` / `run_main`: startup warnings before the
    //   tracing subscriber exists.
    // - `main`: the fatal-error printer (renders errors via
    //   `Display` on stderr with a real exit code).
    for file in src_files()? {
        let text = fs::read_to_string(&file)?;
        let lines: Vec<&str> = text.lines().collect();
        let allowed = function_spans(&lines, &["init_logging", "run_main", "main"]);
        for (line_no, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("eprintln!") {
                let in_allowed = allowed
                    .iter()
                    .any(|&(start, end)| line_no >= start && line_no <= end);
                assert!(
                    in_allowed,
                    "R14 violation: {} line {}: eprintln! outside init_logging/run_main/main: {}",
                    file.display(),
                    line_no + 1,
                    line.trim()
                );
            }
        }
    }
    Ok(())
}
