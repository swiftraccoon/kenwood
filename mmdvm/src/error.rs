//! Shell-level error type for the tokio MMDVM shell.

use mmdvm_core::{Command, MmdvmError, ModemMode, NakReason, ProtocolVersion};
use thiserror::Error;

/// Errors surfaced by the async shell.
/// See the [runtime example](crate#drive-and-reclaim-a-modem).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShellError {
    /// The session task has exited; the consumer handle received a
    /// closed channel.
    #[error("MMDVM session task has exited")]
    SessionClosed,
    /// The spawned task was cancelled or panicked before returning its stream.
    #[error("MMDVM task did not complete: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// Frame construction or response decoding failed its codec contract.
    #[error(transparent)]
    Core(#[from] MmdvmError),
    /// Transport-level I/O failure.
    #[error("MMDVM transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The host's bounded TX queue refused another frame.
    #[error("MMDVM host TX queue full for mode {mode}; frame not queued")]
    BufferFull {
        /// The mode whose host queue is at capacity.
        mode: ModemMode,
    },
    /// A modem `NAK` response was received.
    #[error("modem rejected command 0x{command:02X}: {reason}")]
    Nak {
        /// The command byte that was rejected.
        command: Command,
        /// The rejection reason the modem reported.
        reason: NakReason,
    },
    /// The modem did not answer a correlated command (`set_mode`)
    /// with an ACK or NAK within the response deadline.
    #[error("modem did not acknowledge the command in time")]
    ResponseTimeout,
    /// A previously written mode change still awaits its ACK or NAK.
    #[error("an earlier modem mode change still awaits a response")]
    ModeChangePending,
    /// A previously written configuration still awaits its ACK or NAK.
    #[error("an earlier modem configuration still awaits a response")]
    ConfigurationPending,
    /// The confirmed version differs from the explicitly selected layout.
    #[error("configuration needs {expected:?}, but the modem reported {got:?}")]
    ConfigurationProtocolMismatch {
        /// Protocol required by the configuration.
        expected: ProtocolVersion,
        /// Supported protocol reported by the modem.
        got: ProtocolVersion,
    },
    /// A raw submission uses a command reserved for a correlated operation.
    #[error("command 0x{command:02X} requires its typed modem operation")]
    ReservedCommand {
        /// The reserved command identifier.
        command: Command,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_closed_display() {
        let err = ShellError::SessionClosed;
        assert_eq!(err.to_string(), "MMDVM session task has exited");
    }

    #[test]
    fn buffer_full_reports_mode() {
        let err = ShellError::BufferFull {
            mode: ModemMode::Dstar,
        };
        assert!(err.to_string().contains("D-STAR"));
    }

    #[test]
    fn nak_formats_command_in_hex() {
        let err = ShellError::Nak {
            command: Command::DSTAR_HEADER,
            reason: NakReason::QueueOrStateRejected,
        };
        assert!(err.to_string().contains("0x10"));
        assert!(err.to_string().contains("TX queue or modem state"));
    }

    #[test]
    fn core_error_transparent() {
        let core_err = MmdvmError::InvalidStartByte { got: 0xFF };
        let shell_err: ShellError = core_err.into();
        assert!(matches!(shell_err, ShellError::Core(_)));
    }
}
