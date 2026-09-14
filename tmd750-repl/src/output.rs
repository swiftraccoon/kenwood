//! Plain, optionally timestamped terminal output.

use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

static TIMESTAMPS: AtomicBool = AtomicBool::new(false);
static OUTPUT: OutputState = OutputState::new();
const LINE_WIDTH: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

impl fmt::Display for Stream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        })
    }
}

/// The first failed output stream and its original I/O cause.
#[derive(Debug, thiserror::Error)]
#[error("{stream} output failed: {source}")]
pub(super) struct OutputFailure {
    stream: Stream,
    source: io::Error,
}

/// Output failures are sticky, without preventing independent stderr diagnostics.
struct OutputState {
    failure: Mutex<Option<Arc<OutputFailure>>>,
}

impl OutputState {
    const fn new() -> Self {
        Self {
            failure: Mutex::new(None),
        }
    }

    fn write(&self, stream: Stream, writer: &mut impl Write, line: &[u8]) {
        if let Err(source) = writer.write_all(line).and_then(|()| writer.flush()) {
            let mut failure = self.failure.lock().unwrap_or_else(PoisonError::into_inner);
            if failure.is_none() {
                *failure = Some(Arc::new(OutputFailure { stream, source }));
            }
        }
    }

    fn check(&self) -> Result<(), Arc<OutputFailure>> {
        self.failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map_or(Ok(()), Err)
    }
}

/// Fail an operation boundary or final success check after any output error.
///
/// Callers must finish already-started protocol exchanges and explicit cleanup
/// before returning this failure. The original cause is retained for the process.
pub(super) fn check() -> Result<(), Arc<OutputFailure>> {
    OUTPUT.check()
}

/// Enable or disable UTC timestamps for subsequent output.
pub(super) fn set_timestamps(enabled: bool) {
    TIMESTAMPS.store(enabled, Ordering::Relaxed);
}

/// Print one screen-reader-friendly output line.
pub(super) fn line(arguments: fmt::Arguments<'_>) {
    let mut line = render(arguments);
    line.push('\n');
    OUTPUT.write(Stream::Stdout, &mut io::stdout().lock(), line.as_bytes());
}

/// Print one screen-reader-friendly error line.
pub(super) fn error(arguments: fmt::Arguments<'_>) {
    let mut line = render(arguments);
    line.push('\n');
    OUTPUT.write(Stream::Stderr, &mut io::stderr().lock(), line.as_bytes());
}

fn render(arguments: fmt::Arguments<'_>) -> String {
    let body = arguments.to_string();
    render_at(
        &body,
        TIMESTAMPS.load(Ordering::Relaxed),
        current_utc_seconds(),
    )
}

fn current_utc_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn render_at(body: &str, timestamps: bool, seconds: u64) -> String {
    let hours = (seconds / 3_600) % 24;
    let minutes = (seconds / 60) % 60;
    let seconds = seconds % 60;
    let prefix = if timestamps {
        format!("[{hours:02}:{minutes:02}:{seconds:02}] ")
    } else {
        String::new()
    };
    let width = LINE_WIDTH.saturating_sub(prefix.len());
    body.split('\n')
        .flat_map(|line| wrap_line(line, width))
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wrap prose without splitting an individual whitespace-free token.
fn wrap_line(line: &str, width: usize) -> Vec<String> {
    if line.chars().count() <= width {
        return vec![line.to_owned()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut length = 0;
    for word in line.split_whitespace() {
        let word_length = word.chars().count();
        if !current.is_empty() && length + 1 + word_length > width {
            lines.push(std::mem::take(&mut current));
            length = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            length += 1;
        }
        current.push_str(word);
        length += word_length;
    }
    lines.push(current);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy)]
    enum Fault {
        Write,
        Flush,
    }

    struct FailingWriter {
        fault: Fault,
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
    }

    impl FailingWriter {
        const fn new(fault: Fault) -> Self {
            Self {
                fault,
                bytes: Vec::new(),
                writes: 0,
                flushes: 0,
            }
        }
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if matches!(self.fault, Fault::Write) {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed output"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Err(io::Error::other("output flush failed"))
        }
    }

    #[test]
    fn output_failure_is_sticky_without_blocking_independent_stderr() -> crate::AppResult<()> {
        let state = OutputState::new();
        let mut stdout = FailingWriter::new(Fault::Write);
        state.write(Stream::Stdout, &mut stdout, b"status\n");
        let first = state.check().err().ok_or("write failure was discarded")?;
        assert_eq!(first.stream, Stream::Stdout);
        assert_eq!(first.source.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(stdout.writes, 1);
        assert_eq!(stdout.flushes, 0);
        let mut stderr = Vec::new();
        state.write(Stream::Stderr, &mut stderr, b"output failed\n");
        assert_eq!(stderr, b"output failed\n");
        state.write(
            Stream::Stderr,
            &mut FailingWriter::new(Fault::Write),
            b"second failure\n",
        );
        let retained = state.check().err().ok_or("sticky failure was cleared")?;
        assert!(Arc::ptr_eq(&first, &retained));
        Ok(())
    }

    #[test]
    fn output_flush_failure_prevents_success_after_a_complete_write() -> crate::AppResult<()> {
        let state = OutputState::new();
        let mut stdout = FailingWriter::new(Fault::Flush);
        state.write(Stream::Stdout, &mut stdout, b"completed text\n");
        assert_eq!(stdout.bytes, b"completed text\n");
        assert_eq!(stdout.writes, 1);
        assert_eq!(stdout.flushes, 1);
        let failure = state.check().err().ok_or("flush failure was discarded")?;
        assert_eq!(failure.stream, Stream::Stdout);
        assert_eq!(failure.source.to_string(), "output flush failed");
        assert!(failure.to_string().contains("stdout"));
        Ok(())
    }

    #[test]
    fn disabled_render_is_plain() {
        assert_eq!(render_at("hello 7", false, 0), "hello 7");
    }

    #[test]
    fn enabled_render_has_a_utc_clock_prefix() {
        assert_eq!(render_at("hello", true, 3_723), "[01:02:03] hello");
    }

    #[test]
    fn multiline_guidance_timestamps_every_line() {
        assert_eq!(
            render_at("First step.\nSecond step.", true, 0),
            "[00:00:00] First step.\n[00:00:00] Second step."
        );
    }

    #[test]
    fn long_prose_wraps_within_eighty_columns_including_timestamps() {
        let body = "Readable radio setup instructions. ".repeat(10);
        let rendered = render_at(&body, true, 0);
        assert!(
            rendered
                .lines()
                .all(|line| line.chars().count() <= LINE_WIDTH)
        );
        let words: Vec<_> = rendered
            .lines()
            .filter_map(|line| line.strip_prefix("[00:00:00] "))
            .flat_map(str::split_whitespace)
            .collect();
        assert_eq!(words, body.split_whitespace().collect::<Vec<_>>());
    }

    #[test]
    fn wrapping_preserves_unicode_and_does_not_split_long_paths() {
        assert_eq!(wrap_line("éé éé éé", 5), ["éé éé", "éé"]);
        let path = format!("/{}", "endpoint".repeat(12));
        assert_eq!(wrap_line(&path, LINE_WIDTH), [path]);
    }
}
