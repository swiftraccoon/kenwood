//! Shell-level error type for the tokio MMDVM shell.

use mmdvm_core::{MmdvmError, ModemMode, NakReason};
use thiserror::Error;

/// Errors surfaced by the async shell.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ShellError {
    /// The session task has exited — consumer handle received a
    /// closed channel.
    #[error("MMDVM session task has exited")]
    SessionClosed,
    /// A frame failed to encode/decode per the MMDVM codec.
    #[error(transparent)]
    Core(#[from] MmdvmError),
    /// Transport-level I/O failure.
    #[error("MMDVM transport I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The consumer submitted a voice frame but the modem's TX buffer
    /// is full; the frame was dropped to protect the FIFO from
    /// overflow.
    #[error("MMDVM TX buffer full for mode {mode:?}; frame dropped")]
    BufferFull {
        /// The mode whose FIFO is saturated.
        mode: ModemMode,
    },
    /// A modem `NAK` response was received.
    #[error("modem rejected command 0x{command:02X}: {reason:?}")]
    Nak {
        /// The command byte that was rejected.
        command: u8,
        /// The rejection reason the modem reported.
        reason: NakReason,
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
            mode: ModemMode::DStar,
        };
        assert!(err.to_string().contains("DStar"));
    }

    #[test]
    fn nak_formats_command_in_hex() {
        let err = ShellError::Nak {
            command: 0x10,
            reason: NakReason::BufferFull,
        };
        assert!(err.to_string().contains("0x10"));
        assert!(err.to_string().contains("BufferFull"));
    }

    #[test]
    fn core_error_transparent() {
        let core_err = MmdvmError::InvalidStartByte { got: 0xFF };
        let shell_err: ShellError = core_err.into();
        assert!(matches!(shell_err, ShellError::Core(_)));
    }
}
