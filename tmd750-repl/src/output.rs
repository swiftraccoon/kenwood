//! Plain, optionally timestamped terminal output.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

static TIMESTAMPS: AtomicBool = AtomicBool::new(false);
const LINE_WIDTH: usize = 80;

/// Enable or disable UTC timestamps for subsequent output.
pub(super) fn set_timestamps(enabled: bool) {
    TIMESTAMPS.store(enabled, Ordering::Relaxed);
}

/// Print one screen-reader-friendly output line.
pub(super) fn line(arguments: fmt::Arguments<'_>) {
    println!("{}", render(arguments));
}

/// Print one screen-reader-friendly error line.
pub(super) fn error(arguments: fmt::Arguments<'_>) {
    eprintln!("{}", render(arguments));
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
