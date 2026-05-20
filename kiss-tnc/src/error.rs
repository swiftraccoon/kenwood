//! KISS protocol error type.

use thiserror::Error;

/// Errors that can occur during KISS frame processing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum KissError {
    /// Fewer than two bytes — too short to hold even the start and end
    /// FEND delimiters.
    #[error("KISS frame too short")]
    FrameTooShort,
    /// Frame does not start with FEND.
    #[error("KISS frame missing start FEND")]
    MissingStartDelimiter,
    /// Frame does not end with FEND.
    #[error("KISS frame missing end FEND")]
    MissingEndDelimiter,
    /// Delimiters are present but the frame body has no type indicator
    /// byte.
    #[error("empty KISS frame (no type byte)")]
    EmptyFrame,
    /// The type byte's command nibble is not an assigned KISS command.
    /// Carries the unrecognized nibble (`0x00..=0x0F`).
    #[error("unknown KISS command nibble {0:#04x}")]
    UnknownCommand(u8),
    /// A frame-escape (FESC) byte is followed by a byte other than
    /// TFEND or TFESC.
    #[error("invalid KISS escape sequence")]
    InvalidEscapeSequence,
    /// A frame-escape (FESC) byte is the final body byte, with no
    /// transposed byte following it.
    #[error("truncated KISS escape sequence")]
    TruncatedEscapeSequence,
    /// A raw, unescaped FEND byte appeared inside the frame body, where
    /// it should have been stuffed as `FESC TFEND`.
    #[error("unexpected FEND inside KISS frame body")]
    UnexpectedFrameDelimiter,
    /// A frame — or a run of bytes with no usable delimiter — exceeded
    /// the streaming decoder's configured maximum length and was
    /// discarded to bound memory use.
    #[error("KISS frame exceeds maximum length")]
    FrameTooLong,
}
