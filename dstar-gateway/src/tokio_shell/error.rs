//! Shell-level error type wrapping the sans-io core error + adding
//! tokio-specific failure modes (channel closed, disconnect timeout).

use dstar_gateway_core::error::Error as CoreError;

/// Errors raised by the tokio shell.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    /// Underlying core error (I/O, protocol, type validation, timeout).
    #[error(transparent)]
    Core(#[from] CoreError),

    /// The session task has stopped — handle is no longer valid.
    #[error("session task closed")]
    SessionClosed,

    /// Disconnect did not complete within the timeout.
    #[error("disconnect timed out")]
    DisconnectTimeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_error_session_closed_display() {
        let err = ShellError::SessionClosed;
        assert_eq!(err.to_string(), "session task closed");
    }

    #[test]
    fn shell_error_disconnect_timeout_display() {
        let err = ShellError::DisconnectTimeout;
        assert_eq!(err.to_string(), "disconnect timed out");
    }
}
