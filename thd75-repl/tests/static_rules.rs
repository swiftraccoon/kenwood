//! Static accessibility rule checks (R10, R11, R13, R14).
//!
//! These rules are structural — they cannot be verified from
//! captured output alone. Instead this test scans the REPL's source
//! files for forbidden patterns. Runs as part of `cargo test`.

#![expect(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "Static rules integration test. Walks the crate's own source tree and \
              asserts forbidden patterns don't appear. Uses `.expect()` on filesystem \
              operations (CARGO_MANIFEST_DIR always exists at test time) and direct \
              `lines[..=line_no]` slicing to scan source file prefixes — the `line_no` \
              index always comes from the same file's `.lines().enumerate()` so the \
              slice is always in-bounds."
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
use thd75_repl as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

use std::fs;
use std::path::{Path, PathBuf};

fn src_files() -> Vec<PathBuf> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    collect_rs_files(&src, &mut out);
    out
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn r10_no_print_without_newline() {
    // Skip `print!` entirely in user output. `print!` is only
    // allowed for the interactive prompt, which uses
    // rustyline::readline() directly — not `print!`.
    for file in src_files() {
        let text = fs::read_to_string(&file).expect("read file");
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
}

#[test]
fn r11_no_cursor_move_or_spinner_bytes() {
    // Scan for `\x1b[`, `\r` (carriage return), and `\x08` (backspace)
    // inside string literals. These are the three ways to move the
    // cursor or overwrite content, which screen readers cannot handle.
    //
    // Iterate line by line so we can skip comments that legitimately
    // describe these patterns as part of rule documentation.
    for file in src_files() {
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
        let text = fs::read_to_string(&file).expect("read file");
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
}

#[test]
#[expect(
    clippy::literal_string_with_formatting_args,
    reason = "intentional: we scan source files for this literal"
)]
fn r13_no_ad_hoc_bracket_timestamps() {
    // `[HH:MM:SS]` timestamps should only be produced by the
    // `aprintln!` macro. Anywhere else in the code is an ad-hoc
    // timestamp that won't get the verbose/quiet treatment right.
    //
    // We scan for the literal format pattern used by the timestamp
    // macro today (two-digit hours, minutes, seconds). Only `lib.rs`
    // (where the `aprintln!` macro lives) is allowed.
    for file in src_files() {
        let text = fs::read_to_string(&file).expect("read file");
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
}

#[test]
fn r14_no_eprintln_in_user_output_path() {
    // User-facing output goes to stdout via println!/aprintln!.
    // Diagnostics go to stderr via `tracing`. `eprintln!` is a
    // code smell — it bypasses tracing and bypasses stdout. The
    // only exception is startup warnings before tracing is
    // initialized (in `init_logging`).
    for file in src_files() {
        let text = fs::read_to_string(&file).expect("read file");
        let lines: Vec<&str> = text.lines().collect();
        for (line_no, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("eprintln!") {
                // Allow the `eprintln!` if any earlier line in the file
                // declared `fn init_logging`. This is an approximation
                // (we don't track the function's closing brace), but
                // matches the intent: startup warnings before tracing
                // is initialized live in that function.
                let in_init_logging = lines[..=line_no]
                    .iter()
                    .rev()
                    .any(|l| l.contains("fn init_logging"));
                assert!(
                    in_init_logging,
                    "R14 violation: {} line {}: eprintln! outside init_logging: {}",
                    file.display(),
                    line_no + 1,
                    line.trim()
                );
            }
        }
    }
}
