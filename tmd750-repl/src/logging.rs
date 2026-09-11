//! Opt-in tracing for the TM-D750 REPL.

use clap::ValueEnum;

use crate::output;

const MAX_LOG_COLLISIONS: u32 = 1_024;

/// File log verbosity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum LogLevel {
    /// Do not create a log file.
    Off,
    /// Record errors only.
    Error,
    /// Record warnings and errors.
    Warn,
    /// Record informational events and above.
    Info,
    /// Record debug events and above.
    Debug,
    /// Record every trace event.
    Trace,
}

impl LogLevel {
    const fn filter(self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Error => Some("error"),
            Self::Warn => Some("warn"),
            Self::Info => Some("info"),
            Self::Debug => Some("debug"),
            Self::Trace => Some("trace"),
        }
    }
}

/// Keeps the background log writer alive until process shutdown.
#[derive(Debug)]
pub(super) struct Guard {
    _worker: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Configure tracing and return the concrete log path, when enabled.
pub(super) fn init(requested_level: LogLevel, trace: bool) -> (Guard, Option<std::path::PathBuf>) {
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::fmt;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    use tracing_subscriber::{EnvFilter, Registry};

    let file_level = if trace {
        LogLevel::Trace
    } else {
        requested_level
    };
    let stderr_layer = std::env::var("RUST_LOG").ok().map(|filter| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_filter(EnvFilter::new(filter))
    });

    let mut file_layer = None;
    let mut worker = None;
    let mut path = None;
    if let Some(filter) = file_level.filter() {
        match log_path() {
            Some(candidate) => {
                let Some(parent) = candidate.parent() else {
                    output::error(format_args!(
                        "Warning: could not determine the log directory; file logging disabled."
                    ));
                    Registry::default().with(stderr_layer).init();
                    return (Guard { _worker: None }, None);
                };
                if let Err(error) = std::fs::create_dir_all(parent) {
                    output::error(format_args!(
                        "Warning: could not create log directory {}: {error}; file logging disabled.",
                        parent.display()
                    ));
                } else {
                    match create_session_file(&candidate) {
                        Ok((file, created_path)) => {
                            let (writer, guard) = tracing_appender::non_blocking(file);
                            file_layer = Some(
                                fmt::layer()
                                    .with_writer(writer)
                                    .with_ansi(false)
                                    .with_filter(EnvFilter::new(filter)),
                            );
                            worker = Some(guard);
                            path = Some(created_path);
                        }
                        Err(error) => output::error(format_args!(
                            "Warning: could not create log file {}: {error}; file logging disabled.",
                            candidate.display()
                        )),
                    }
                }
            }
            None => {
                output::error(format_args!(
                    "Warning: could not determine a log directory; file logging disabled."
                ));
            }
        }
    }

    Registry::default()
        .with(stderr_layer)
        .with(file_layer)
        .init();
    (Guard { _worker: worker }, path)
}

/// Atomically reserve a new log, appending a numeric suffix on a collision.
///
/// Two sessions can start in the same clock second. Exclusive creation also
/// handles concurrent starts and existing files without truncating either.
fn create_session_file(
    candidate: &std::path::Path,
) -> std::io::Result<(std::fs::File, std::path::PathBuf)> {
    let name = candidate.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "log path has no file name",
        )
    })?;
    for collision in 0..=MAX_LOG_COLLISIONS {
        let path = if collision == 0 {
            candidate.to_owned()
        } else {
            let mut unique_name = name.to_os_string();
            unique_name.push(format!(".{collision}"));
            candidate.with_file_name(unique_name)
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "all session log collision suffixes are occupied",
    ))
}

fn log_path() -> Option<std::path::PathBuf> {
    let suffix = time::OffsetDateTime::now_utc()
        .format(time::macros::format_description!(
            "[year]-[month]-[day]-[hour][minute][second]"
        ))
        .unwrap_or_else(|_| "session".to_owned());
    let file = format!("tmd750-repl.log.{suffix}");

    #[cfg(target_os = "macos")]
    {
        Some(
            dirs_next::home_dir()?
                .join("Library")
                .join("Logs")
                .join("tmd750-repl")
                .join(file),
        )
    }

    #[cfg(target_os = "windows")]
    {
        Some(
            dirs_next::data_local_dir()?
                .join("tmd750-repl")
                .join("logs")
                .join(file),
        )
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| dirs_next::home_dir().map(|home| home.join(".local").join("state")))?;
        Some(base.join("tmd750-repl").join(file))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::create_session_file;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn same_timestamp_sessions_preserve_every_log() -> TestResult {
        let directory = tempfile::tempdir()?;
        let candidate = directory.path().join("tmd750-repl.log.2026-09-04-030128");
        let (mut first, first_path) = create_session_file(&candidate)?;
        first.write_all(b"first session\n")?;
        let (mut second, second_path) = create_session_file(&candidate)?;
        second.write_all(b"second session\n")?;
        let (mut third, third_path) = create_session_file(&candidate)?;
        third.write_all(b"third session\n")?;

        assert_eq!(
            first_path, candidate,
            "the first session keeps the base name"
        );
        assert_ne!(
            second_path, first_path,
            "a collision must reserve a new file"
        );
        assert_ne!(
            third_path, first_path,
            "later sessions must preserve the base file"
        );
        assert_ne!(
            third_path, second_path,
            "collision retries must preserve earlier suffixes"
        );
        assert_eq!(std::fs::read(&first_path)?, b"first session\n");
        assert_eq!(std::fs::read(&second_path)?, b"second session\n");
        assert_eq!(std::fs::read(&third_path)?, b"third session\n");
        Ok(())
    }

    #[test]
    fn unrelated_io_errors_are_returned_without_creating_a_log() -> TestResult {
        let directory = tempfile::tempdir()?;
        let candidate = directory.path().join("missing").join("session.log");
        let result = create_session_file(&candidate);

        assert!(
            matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::NotFound),
            "a missing directory must remain a useful I/O error: {result:?}"
        );
        assert!(
            !candidate.exists(),
            "failed creation must not leave a log behind"
        );
        Ok(())
    }
}
