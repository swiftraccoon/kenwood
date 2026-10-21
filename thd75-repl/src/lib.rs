#![doc = include_str!("../README.md")]

use aprs_is as _;
use clap as _;
use dirs_next as _;
use dstar_gateway_core as _;
use kenwood_transport as _;
use mmdvm as _;
use rustyline as _;
use tokio as _;
use tracing as _;
use tracing_appender as _;
use tracing_subscriber as _;

#[cfg(test)]
use proptest as _;

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

use time::{OffsetDateTime, Time, UtcOffset};

/// Checked pure APRS command parsing.
pub mod aprs_args;
/// Hardware-free sample output checks and their actual rule coverage.
pub mod check;
/// Transmit confirmation and noninteractive refusal.
pub mod confirm;
/// Per-command and mode help text.
pub mod help_text;
/// Local accessibility output rules.
pub mod lint;
/// Feature-selected hardware-free integration scenarios.
#[cfg(feature = "testing")]
pub mod mock_scenarios;
/// Named user-message formatting.
pub mod output;
/// Ordered script parsing.
pub mod script;

/// Default number of complete output lines retained for replay.
pub const HISTORY_CAPACITY_DEFAULT: usize = 30;

/// Timestamp presentation selected before or during a session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampMode {
    /// Output without a timestamp.
    #[default]
    Disabled,
    /// Prefix UTC time of day.
    Utc,
    /// Prefix time of day at a checked fixed UTC offset.
    Offset(UtcOffset),
}

#[derive(Debug)]
struct OutputState {
    capacity: usize,
    history: VecDeque<String>,
    timestamps: TimestampMode,
    verbose: bool,
    first_error: Option<io::Error>,
}

impl Default for OutputState {
    fn default() -> Self {
        Self {
            capacity: HISTORY_CAPACITY_DEFAULT,
            history: VecDeque::new(),
            timestamps: TimestampMode::Disabled,
            verbose: true,
            first_error: None,
        }
    }
}

impl OutputState {
    fn record(&mut self, line: String) {
        if self.capacity == 0 {
            return;
        }
        while self.history.len() >= self.capacity {
            drop(self.history.pop_front());
        }
        self.history.push_back(line);
    }

    fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        while self.history.len() > capacity {
            drop(self.history.pop_front());
        }
    }

    fn write_to(
        &mut self,
        writer: &mut impl Write,
        message: &str,
        timestamp: Option<Time>,
    ) -> io::Result<()> {
        for line in render_message(message, timestamp) {
            writeln!(writer, "{line}")?;
            self.record(line);
        }
        writer.flush()
    }

    fn emit_to(&mut self, writer: &mut impl Write, message: &str, timestamp: Option<Time>) {
        if self.first_error.is_none()
            && let Err(error) = self.write_to(writer, message, timestamp)
        {
            self.first_error = Some(error);
        }
    }

    fn replay_to(&mut self, writer: &mut impl Write, lines: &[String]) {
        if self.first_error.is_some() {
            return;
        }
        let result = (|| {
            for message in lines {
                for line in render_message(message, None) {
                    writeln!(writer, "{line}")?;
                }
            }
            writer.flush()
        })();
        if let Err(error) = result {
            self.first_error = Some(error);
        }
    }
}

fn output_state() -> MutexGuard<'static, OutputState> {
    static STATE: OnceLock<Mutex<OutputState>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(OutputState::default()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Select timestamp presentation atomically with respect to message output.
///
/// Offset values use the validated `time::UtcOffset` domain. Every physical
/// output line receives the selected prefix, including wrapped continuations.
pub fn set_timestamp_mode(mode: TimestampMode) {
    output_state().timestamps = mode;
}

/// Read whether recurring monitor details are enabled.
#[must_use]
pub fn is_verbose() -> bool {
    output_state().verbose
}

/// Enable or disable recurring monitor details.
///
/// Call sites decide which events are recurring; output emission itself is
/// unconditional, so errors and confirmation prompts remain visible.
pub fn set_verbose(enabled: bool) {
    output_state().verbose = enabled;
}

/// Change history capacity and discard excess oldest entries in one transaction.
///
/// Zero clears and disables history. Concurrent output and resizing use the
/// same mutex. Poisoned locks recover their retained state; no mutable storage
/// handle is exposed to callers.
pub fn set_history_capacity(capacity: usize) {
    output_state().set_capacity(capacity);
}

/// Store a message as escaped, bounded lines without printing or timestamps.
///
/// This supports callers that render to another destination. Prefer
/// [`aprintln!`] when stdout and history should advance together.
pub fn record_output(message: &str) {
    let mut state = output_state();
    for line in render_message(message, None) {
        state.record(line);
    }
}

/// Clone at most the most recent count lines, oldest first.
///
/// Zero returns an empty vector. Snapshots release the mutex before returning.
#[must_use]
pub fn last_lines(count: usize) -> Vec<String> {
    let state = output_state();
    state
        .history
        .iter()
        .skip(state.history.len().saturating_sub(count))
        .cloned()
        .collect()
}

/// Escape text into printable ASCII lines of at most 80 bytes.
///
/// LF separates logical lines. All other controls and non-ASCII characters use
/// Rust-style visible escapes. A timestamp prefixes every physical line; wrapped
/// lines add `Continued: `. Escapes are never split. Text is preserved through
/// escaping/wrapping rather than silently truncated.
///
/// ```
/// use thd75_repl::render_message;
/// let lines = render_message("Station: A\u{1b}B\u{e9}", None);
/// assert_eq!(lines, [r"Station: A\u{1b}B\u{e9}"]);
/// assert!(lines.iter().all(|line| line.len() <= 80));
/// ```
#[must_use]
pub fn render_message(message: &str, timestamp: Option<Time>) -> Vec<String> {
    let prefix = timestamp.map_or_else(String::new, |time| {
        let h = time.hour();
        let m = time.minute();
        let s = time.second();
        format!("[{h:02}:{m:02}:{s:02}] ")
    });
    let mut lines = Vec::new();
    for logical in message.split('\n') {
        let mut line = prefix.clone();
        for character in logical.chars() {
            let token = if character.is_ascii() && !character.is_ascii_control() {
                character.to_string()
            } else {
                character.escape_default().to_string()
            };
            if line.len() + token.len() > 80 {
                lines.push(line);
                line = format!("{prefix}Continued: ");
            }
            line.push_str(&token);
        }
        lines.push(line);
    }
    lines
}

/// Emit one message through the shared escaped, bounded stdout/history path.
///
/// Each successfully written complete line enters history. A write or flush
/// failure is retained as the first output error and suppresses later writes
/// until [`take_output_error`] retrieves it. Callers inspect that error before
/// reporting process success. The operation serializes output and configuration;
/// ordinary blocking stdout has no application deadline.
pub fn emit_output(message: &str) {
    let mut state = output_state();
    let timestamp = match state.timestamps {
        TimestampMode::Disabled => None,
        TimestampMode::Utc => Some(OffsetDateTime::now_utc().time()),
        TimestampMode::Offset(offset) => Some(OffsetDateTime::now_utc().to_offset(offset).time()),
    };
    state.emit_to(&mut io::stdout().lock(), message, timestamp);
}

/// Print a history snapshot without adding timestamps or recording it again.
///
/// Existing printable prefixes are retained. Supplied lines still pass through
/// [`render_message`], so arbitrary caller text cannot inject controls or exceed
/// the physical line limit. Write/flush failures follow [`emit_output`]'s
/// first-error contract.
pub fn replay_output(lines: &[String]) {
    output_state().replay_to(&mut io::stdout().lock(), lines);
}

/// Take the first stdout write/flush error and allow later output attempts.
///
/// The binary calls this at completion and returns the original I/O error
/// instead of reporting success after broken output.
#[must_use]
pub fn take_output_error() -> Option<io::Error> {
    output_state().first_error.take()
}

/// Format and emit a message with shared timestamps, escaping and history.
///
/// Macro expansion requires no caller imports.
///
/// ```
/// thd75_repl::aprintln!("Status: {}", "ready");
/// ```
#[macro_export]
macro_rules! aprintln {
    () => {{
        $crate::emit_output("");
    }};
    ($($arg:tt)*) => {{
        $crate::emit_output(&::std::format!($($arg)*));
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn history_resize_and_record_share_one_bounded_state() {
        let mut state = OutputState::default();
        state.set_capacity(3);
        for line in ["alpha", "bravo", "charlie", "delta"] {
            state.record(line.to_owned());
        }
        assert_eq!(state.history, ["bravo", "charlie", "delta"]);
        state.set_capacity(1);
        assert_eq!(state.history, ["delta"]);
        state.set_capacity(0);
        state.record("ignored".to_owned());
        assert!(
            state.history.is_empty(),
            "zero capacity clears and disables storage"
        );
    }

    #[test]
    fn controls_unicode_and_long_text_remain_visible_bounded_lines() -> TestResult {
        let text = format!(
            "Error: {}{}{}{}",
            "x".repeat(200),
            char::from(13),
            char::from(27),
            'é'
        );
        let timestamp = Time::from_hms(12, 34, 56)?;
        let lines = render_message(&text, Some(timestamp));
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.starts_with("[12:34:56] "), "{line:?}");
            assert!(line.len() <= 80, "{line:?}");
            assert!(
                line.bytes().all(|byte| (32..=126).contains(&byte)),
                "{line:?}"
            );
        }
        let joined = lines.join("");
        assert!(joined.contains(r"\r"), "{joined}");
        assert!(joined.contains(r"\u{1b}"), "{joined}");
        assert!(joined.contains(r"\u{e9}"), "{joined}");
        assert_eq!(render_message("one\ntwo", None), ["one", "two"]);
        Ok(())
    }

    #[derive(Debug)]
    struct FailingWriter {
        written: Vec<u8>,
        flush_fails: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.written.len() >= 6 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed sink"));
            }
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.flush_fails {
                Err(io::Error::other("flush failed"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn history_records_only_complete_written_lines_and_reports_flush_failure() {
        let mut state = OutputState::default();
        let mut writer = FailingWriter {
            written: Vec::new(),
            flush_fails: false,
        };
        let result = state.write_to(&mut writer, "first\nsecond", None);
        assert!(
            matches!(result, Err(ref error) if error.kind() == io::ErrorKind::BrokenPipe),
            "{result:?}"
        );
        assert_eq!(state.history, ["first"]);
        let mut writer = FailingWriter {
            written: Vec::new(),
            flush_fails: true,
        };
        let result = state.write_to(&mut writer, "ok", None);
        assert!(
            matches!(result, Err(ref error) if error.to_string() == "flush failed"),
            "{result:?}"
        );
        assert_eq!(state.history.back().map(String::as_str), Some("ok"));
    }

    #[test]
    fn output_failure_stops_later_writes_until_the_original_error_is_taken() -> TestResult {
        let mut state = OutputState::default();
        let mut broken = FailingWriter {
            written: Vec::new(),
            flush_fails: true,
        };
        state.emit_to(&mut broken, "first", None);
        let mut later = Vec::new();
        state.emit_to(&mut later, "second", None);
        state.replay_to(&mut later, &["replayed".to_owned()]);
        assert!(
            later.is_empty(),
            "later writes must stop after the first failure"
        );
        assert_eq!(state.history, ["first"]);
        let error = state.first_error.take().ok_or("missing output error")?;
        assert_eq!(error.to_string(), "flush failed");
        state.emit_to(&mut later, "third", None);
        assert_eq!(later, b"third\n");
        assert_eq!(state.history, ["first", "third"]);
        Ok(())
    }

    #[test]
    fn replay_keeps_original_prefix_and_history_without_duplicate_entries() -> TestResult {
        let mut state = OutputState::default();
        let mut original = Vec::new();
        state.emit_to(
            &mut original,
            "Status: ready",
            Some(Time::from_hms(1, 2, 3)?),
        );
        let snapshot: Vec<_> = state.history.iter().cloned().collect();
        let mut replayed = Vec::new();
        state.replay_to(&mut replayed, &snapshot);
        assert_eq!(replayed, original);
        assert_eq!(state.history.len(), 1);
        assert!(state.first_error.is_none(), "memory output must succeed");
        Ok(())
    }
}
