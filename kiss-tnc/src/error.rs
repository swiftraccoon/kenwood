//! KISS protocol error type.

use thiserror::Error;

/// Errors from KISS frame decoding and command or port conversion.
///
/// Errors are value diagnostics: copying preserves the failure kind and any
/// rejected byte, without retaining frame buffers or decoder state.
/// See the [decoding error example](crate#limits-and-errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum KissError {
    /// Fewer than two bytes: too short to hold even the start and end
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
    /// A command byte is not an assigned KISS command code.
    ///
    /// [`crate::KissCommand::try_from`] returns the input byte. Frame
    /// decoding returns the type byte's unrecognized low nibble.
    #[error("unknown KISS command {0:#04x}")]
    UnknownCommand(u8),
    /// A raw port byte exceeds the four-bit `0..=15` port address range.
    #[error("KISS port {0} exceeds 15")]
    InvalidPort(u8),
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
    /// Buffered input exceeded the streaming decoder's wire-length limit.
    ///
    /// A complete frame includes both FEND delimiters. An incomplete
    /// frame or buffer without a FEND is rejected when its buffered length
    /// exceeds the limit. Rejected bytes are discarded by
    /// [`crate::KissDecoder::next_frame`].
    #[error("KISS frame exceeds maximum length")]
    FrameTooLong,
}
