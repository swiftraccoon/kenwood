//! KISS protocol error type.

use thiserror::Error;

/// Errors that can occur during KISS frame processing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum KissError {
    /// Frame is too short to contain a valid KISS header.
    #[error("KISS frame too short")]
    FrameTooShort,
    /// Frame does not start with FEND.
    #[error("KISS frame missing start FEND")]
    MissingStartDelimiter,
    /// Frame does not end with FEND.
    #[error("KISS frame missing end FEND")]
    MissingEndDelimiter,
    /// Invalid escape sequence (FESC not followed by TFEND or TFESC).
    #[error("invalid KISS escape sequence")]
    InvalidEscapeSequence,
    /// Frame body is empty (no type indicator byte).
    #[error("empty KISS frame (no type byte)")]
    EmptyFrame,
}
