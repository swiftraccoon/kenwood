//! Shell-level error type wrapping the sans-io core error + adding
//! tokio-specific failure modes (channel closed).

use dstar_gateway_core::error::Error as CoreError;
use dstar_gateway_core::session::client::DisconnectReason;

/// Errors raised by the tokio shell.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    /// Underlying core error (I/O, protocol, type validation, timeout).
    #[error(transparent)]
    Core(#[from] CoreError),

    /// The session task has stopped; the handle is no longer valid.
    #[error("session task closed")]
    SessionClosed,

    /// The session task did not report any disconnect outcome before the
    /// shell deadline.
    #[error("disconnect stalled before the session task reported an outcome")]
    DisconnectStalled,

    /// The core closed the local session after the reflector failed to
    /// acknowledge UNLINK within the protocol deadline.
    #[error("reflector did not acknowledge unlink; local session closed after protocol timeout")]
    DisconnectUnacknowledged,

    /// The session ended for another reason before UNLINK was acknowledged.
    #[error("session ended before unlink was acknowledged: {reason:?}")]
    DisconnectedBeforeUnlink {
        /// Terminal reason reported by the core.
        reason: DisconnectReason,
    },
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
    fn shell_error_disconnect_stalled_display() {
        let err = ShellError::DisconnectStalled;
        assert_eq!(
            err.to_string(),
            "disconnect stalled before the session task reported an outcome"
        );
    }

    #[test]
    fn shell_error_disconnect_unacknowledged_display() {
        let err = ShellError::DisconnectUnacknowledged;
        assert_eq!(
            err.to_string(),
            "reflector did not acknowledge unlink; local session closed after protocol timeout"
        );
    }
}
