//! Library crate for `thd75-repl`.
//!
//! This exposes the internal modules that need to be reachable from
//! integration tests in `tests/` and from the binary entry point in
//! `src/main.rs`.
//!
//! The binary is a thin wrapper that imports from this library via its
//! own crate name.
//!
//! Public modules here are part of the REPL's internal API, not a
//! stable library API for external consumers.

#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
#![deny(clippy::nursery)]
#![expect(
    clippy::module_name_repetitions,
    reason = "REPL crate exposes types like `AprsClient`, `DStarSession`, and helpers whose names \
              deliberately repeat module names so call sites read naturally. Suppressing crate-wide \
              keeps the public surface ergonomic without adding per-item attributes."
)]
#![cfg_attr(
    test,
    expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "Unit tests in this crate use `.unwrap()` / `.unwrap_err()` on `Result` values \
                  returned by the REPL's own validators (lint, script, confirm) to assert the \
                  expected Ok/Err split, use `.expect()` on known-valid constructor outputs, \
                  and use `panic!` as assertion-style reporters. All are safe because the \
                  tests' setup guarantees the accessed values exist — any violation correctly \
                  fails the test."
    )
)]

// Dev-dependencies / optional deps pulled in only by `src/main.rs` (the binary target).
// Acknowledge at the lib level so `unused_crate_dependencies` stays silent for the lib
// compilation unit. When this crate's lib grows to use one of these directly, delete the
// corresponding `use X as _;` line.
use clap as _;
use dirs_next as _;
use dstar_gateway as _;
use dstar_gateway_core as _;
use rustyline as _;
use time as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

// proptest is a dev-dependency, not referenced in the library source. Acknowledge under
// `cfg(test)` so `unused_crate_dependencies` stays silent for the lib's test build.
#[cfg(test)]
use proptest as _;

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

/// Accessibility lint checker for REPL output lines.
///
/// Implements fourteen hard rules (R1 through R14) covering ASCII
/// purity, line length, ANSI escapes, error and warning prefixes,
/// list summaries, label-colon-value formatting, boolean rendering,
/// and unit spelling. See `lint::Rule` for the full enumeration.
pub mod lint;

/// Pure format functions for every user-facing string.
///
/// Zero I/O, zero async, zero radio access. Tested directly with
/// golden strings and the lint.
pub mod output;

/// Per-command detailed help text, returned by `help <command>`, plus
/// the multi-line mode help blobs used by `help` with no arguments.
pub mod help_text;

/// Script mode: parse a file of REPL commands and execute them.
pub mod script;

/// Mock radio scenarios for integration tests (feature-gated).
#[cfg(feature = "testing")]
pub mod mock_scenarios;

/// Accessibility compliance self-check for the `check` subcommand.
pub mod check;

/// Transmit confirmation prompt and script-mode gating.
pub mod confirm;

/// Global flag for timestamp output mode.
///
/// When `true`, the [`aprintln!`] macro prepends a `[HH:MM:SS]`
/// timestamp prefix to every line. Set at startup from the
/// `--timestamps`, `--local-time`, and `--utc-offset` CLI flags.
pub static TIMESTAMPS: AtomicBool = AtomicBool::new(false);

/// UTC offset in seconds, added to UTC time when forming timestamps.
///
/// Zero means UTC. Set at startup from `--utc-offset`, `--local-time`,
/// or both; otherwise unused. Atomic so the [`aprintln!`] macro can
/// read it without synchronisation.
pub static UTC_OFFSET_SECS: AtomicI32 = AtomicI32::new(0);

/// Global flag for verbose output mode.
///
/// When `false` (quiet mode), low-signal recurring events such as
/// digipeat notifications, APRS query responses, raw packet dumps, and
/// D-STAR voice-lost frames are suppressed. Defaults to `true`.
pub static VERBOSE: AtomicBool = AtomicBool::new(true);

/// Read the current verbose flag.
#[must_use]
pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Default number of lines retained by the history buffer.
///
/// Screen-reader users rely on the `last` command to replay recent
/// output without rewinding the buffer one line at a time; 30 lines
/// covers most common single-screen contexts. See [`last_lines`] and
/// [`record_output`].
pub const HISTORY_CAPACITY_DEFAULT: usize = 30;

/// Runtime-adjustable history capacity. Set from
/// `--history-lines` at startup via [`set_history_capacity`].
static HISTORY_CAPACITY: AtomicUsize = AtomicUsize::new(HISTORY_CAPACITY_DEFAULT);

/// Rolling buffer of recent user-facing output lines, ordered oldest
/// to newest. Appended to by every [`aprintln!`] invocation and read
/// by the `last` REPL command.
///
/// Uses `OnceLock` rather than `LazyLock` to keep the accessible REPL
/// crate compatible with older stable Rust releases.
#[must_use]
pub fn last_output() -> &'static Mutex<VecDeque<String>> {
    static BUF: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();
    BUF.get_or_init(|| Mutex::new(VecDeque::with_capacity(HISTORY_CAPACITY_DEFAULT)))
}

/// Update the history buffer capacity at startup.
///
/// Values of 0 are allowed (disables history, `last` will print the
/// "no previous output" message). Existing entries beyond the new
/// capacity are dropped from the front (oldest first).
pub fn set_history_capacity(n: usize) {
    HISTORY_CAPACITY.store(n, Ordering::Relaxed);
    if let Ok(mut buf) = last_output().lock() {
        while buf.len() > n {
            drop(buf.pop_front());
        }
    }
}

/// Append `line` to the history buffer, evicting the oldest entry
/// when the buffer would exceed the configured capacity. Intended to
/// be called only from the [`aprintln!`] macro.
pub fn record_output(line: String) {
    let cap = HISTORY_CAPACITY.load(Ordering::Relaxed);
    if cap == 0 {
        return;
    }
    if let Ok(mut buf) = last_output().lock() {
        while buf.len() >= cap {
            drop(buf.pop_front());
        }
        buf.push_back(line);
    }
}

/// Retrieve the most recent `n` lines from the history buffer.
///
/// Entries are returned oldest-to-newest. Passing `n == 0` always
/// returns an empty vector. The returned vector clones the stored
/// strings so the caller may hold them after the buffer advances.
#[must_use]
pub fn last_lines(n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    last_output().lock().map_or_else(
        |_| Vec::new(),
        |buf| {
            let len = buf.len();
            let start = len.saturating_sub(n);
            buf.iter().skip(start).cloned().collect()
        },
    )
}

/// Print a line with an optional `[HH:MM:SS]` timestamp prefix and
/// record it in the history buffer.
///
/// When [`TIMESTAMPS`] is enabled, prepends a time prefix to every
/// output line so blind operators can track when events occurred
/// without checking a clock. The time shown is UTC by default, or
/// local time if [`UTC_OFFSET_SECS`] has been set from
/// `--local-time` / `--utc-offset`.
///
/// Every printed line is also appended to the rolling history buffer
/// so the `last` REPL command can replay recent output. Plain
/// `println!` calls bypass this recording — use `aprintln!` whenever
/// a line is user-facing and should be replayable.
#[macro_export]
macro_rules! aprintln {
    ($($arg:tt)*) => {{
        let body = format!($($arg)*);
        let line = if $crate::TIMESTAMPS.load(
            ::std::sync::atomic::Ordering::Relaxed,
        ) {
            #[expect(
                clippy::cast_possible_wrap,
                reason = "u64 seconds since the epoch will not exceed i64::MAX for billions of \
                          years, so the cast to i64 is safe for all realistic clock values."
            )]
            let utc_secs = ::std::time::SystemTime::now()
                .duration_since(::std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let offset = i64::from(
                $crate::UTC_OFFSET_SECS.load(::std::sync::atomic::Ordering::Relaxed),
            );
            let local_secs_signed = utc_secs + offset;
            #[expect(
                clippy::cast_sign_loss,
                reason = "The branch explicitly gates on `local_secs_signed < 0`, so the `as u64` \
                          cast only runs when the value is non-negative."
            )]
            let local_secs = if local_secs_signed < 0 {
                0u64
            } else {
                local_secs_signed as u64
            };
            let h = (local_secs / 3600) % 24;
            let m = (local_secs / 60) % 60;
            let s = local_secs % 60;
            format!("[{h:02}:{m:02}:{s:02}] {body}")
        } else {
            body
        };
        println!("{line}");
        $crate::record_output(line);
    }};
}

#[cfg(test)]
mod lib_tests {
    //! Tests for the shared library state: history buffer mechanics.
    //!
    //! These tests mutate global state ([`HISTORY_CAPACITY`] and the
    //! `OnceLock` buffer), so they must run serially. A file-local
    //! mutex guards each test.
    use super::{
        HISTORY_CAPACITY_DEFAULT, last_lines, last_output, record_output, set_history_capacity,
    };
    use std::sync::Mutex;

    /// Test guard: every test in this module locks this mutex first,
    /// mutates global state, then drops the lock. Prevents interleaved
    /// test executions from corrupting each other.
    static TEST_GUARD: Mutex<()> = Mutex::new(());

    fn reset() {
        set_history_capacity(HISTORY_CAPACITY_DEFAULT);
        if let Ok(mut buf) = last_output().lock() {
            buf.clear();
        }
    }

    #[test]
    fn records_and_retrieves_lines() {
        let _g = TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        record_output("alpha".to_string());
        record_output("bravo".to_string());
        record_output("charlie".to_string());
        assert_eq!(last_lines(3), vec!["alpha", "bravo", "charlie"]);
        assert_eq!(last_lines(1), vec!["charlie"]);
        assert_eq!(last_lines(2), vec!["bravo", "charlie"]);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let _g = TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        set_history_capacity(3);
        record_output("one".to_string());
        record_output("two".to_string());
        record_output("three".to_string());
        record_output("four".to_string());
        assert_eq!(last_lines(10), vec!["two", "three", "four"]);
    }

    #[test]
    fn last_lines_zero_returns_empty() {
        let _g = TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        record_output("one".to_string());
        assert!(last_lines(0).is_empty());
    }

    #[test]
    fn set_capacity_zero_disables_recording() {
        let _g = TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        set_history_capacity(0);
        record_output("ignored".to_string());
        assert!(last_lines(10).is_empty());
        // Restore default so other tests see the expected state.
        set_history_capacity(HISTORY_CAPACITY_DEFAULT);
    }

    #[test]
    fn shrink_capacity_truncates_front() {
        let _g = TEST_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset();
        set_history_capacity(5);
        record_output("a".to_string());
        record_output("b".to_string());
        record_output("c".to_string());
        record_output("d".to_string());
        set_history_capacity(2);
        assert_eq!(last_lines(10), vec!["c", "d"]);
    }
}
