//! Shell-level error type wrapping the sans-io core error + adding
//! tokio-specific failure modes (channel closed).

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
    ///
    /// Never returned by the current shell: [`AsyncSession::disconnect`]
    /// takes no deadline and resolves as soon as the session loop
    /// acknowledges the request. A disconnect that times out is reported
    /// by the core as `Event::Disconnected(DisconnectReason::DisconnectTimeout)`
    /// on the event stream instead.
    ///
    /// [`AsyncSession::disconnect`]: crate::tokio_shell::AsyncSession::disconnect
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
