//! Script mode: read commands from a file and execute them.
//!
//! Script mode is a dual-use feature. For users, it provides a way to
//! pre-configure the radio by saving a sequence of commands in a file
//! and replaying them (contest prep, net startup, morning checks).
//! For tests, it is the vehicle that lets integration tests drive the
//! full REPL loop from a file and capture output via subprocess
//! spawning.
//!
//! ## Format
//!
//! - `#` at line start: comment, skipped
//! - Blank lines: skipped
//! - Leading and trailing whitespace on each command line is trimmed
//! - Everything else: sent through the same dispatcher as interactive
//!   input
//! - `exit` or `quit` ends the script and exits the REPL

use std::io::BufRead;
use std::path::Path;

/// Parsed lines from a script file, with comments and blanks removed.
#[derive(Debug, Default, Clone)]
pub struct Script {
    /// The non-comment, non-blank command lines in the order they
    /// appeared in the source file.
    pub commands: Vec<String>,
}

impl Script {
    /// Parse a script from any reader.
    ///
    /// Returns a [`Script`] containing every non-blank, non-comment
    /// line from `reader`, with leading and trailing whitespace
    /// stripped. Order is preserved.
    ///
    /// # Errors
    ///
    /// Propagates any I/O error from reading the underlying reader.
    pub fn from_reader<R: BufRead>(reader: R) -> std::io::Result<Self> {
        let mut commands = Vec::new();
        for line in reader.lines() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            commands.push(trimmed.to_string());
        }
        Ok(Self { commands })
    }

    /// Parse a script from a file path.
    ///
    /// Pass `-` to read from standard input instead of a regular file.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened or read.
    pub fn from_path(path: &Path) -> std::io::Result<Self> {
        if path.as_os_str() == "-" {
            let stdin = std::io::stdin();
            let reader = stdin.lock();
            Self::from_reader(reader)
        } else {
            let file = std::fs::File::open(path)?;
            Self::from_reader(std::io::BufReader::new(file))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(text: &str) -> Script {
        Script::from_reader(Cursor::new(text.as_bytes())).expect("parse script")
    }

    #[test]
    fn skips_comments() {
        let s = parse("# first\nid\n# second\nbattery\n");
        assert_eq!(s.commands, vec!["id".to_string(), "battery".to_string()]);
    }

    #[test]
    fn skips_blank_lines() {
        let s = parse("id\n\n\nbattery\n");
        assert_eq!(s.commands, vec!["id".to_string(), "battery".to_string()]);
    }

    #[test]
    fn trims_whitespace() {
        let s = parse("  id  \n\tbattery\n");
        assert_eq!(s.commands, vec!["id".to_string(), "battery".to_string()]);
    }

    #[test]
    fn preserves_order() {
        let s = parse("one\ntwo\nthree\n");
        assert_eq!(s.commands, vec!["one", "two", "three"]);
    }

    #[test]
    fn empty_file_is_valid() {
        let s = parse("");
        assert!(s.commands.is_empty());
    }

    #[test]
    fn only_comments_is_valid() {
        let s = parse("# foo\n# bar\n\n");
        assert!(s.commands.is_empty());
    }
}
