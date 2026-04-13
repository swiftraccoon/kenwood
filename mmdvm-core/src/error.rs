//! Error types for the MMDVM codec.
//!
//! All fallible codec operations return `Result<_, MmdvmError>`.
//! Variants carry raw bytes / lengths so callers can pattern-match
//! without parsing error strings.

use thiserror::Error;

/// Errors produced by the MMDVM codec.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum MmdvmError {
    /// Frame is shorter than the minimum 3 bytes.
    #[error("frame too short: {len} bytes (minimum 3)")]
    FrameTooShort {
        /// Number of bytes actually seen.
        len: usize,
    },
    /// The first byte is not `0xE0`.
    #[error("invalid start byte: 0x{got:02X} (expected 0xE0)")]
    InvalidStartByte {
        /// The byte found at position 0.
        got: u8,
    },
    /// The length field is less than 3.
    #[error("invalid length field: {len} (minimum 3)")]
    InvalidLength {
        /// The raw length byte.
        len: u8,
    },
    /// Payload is larger than the single-byte length field can encode.
    #[error("MMDVM payload too large: {len} bytes (maximum 252)")]
    PayloadTooLarge {
        /// Requested payload length in bytes.
        len: usize,
    },
    /// An unknown NAK reason code was received.
    #[error("unknown NAK reason code: 0x{code:02X}")]
    UnknownNakReason {
        /// Raw reason byte.
        code: u8,
    },
    /// Status response was too short to parse.
    #[error("status response too short: {len} bytes (need at least {min})")]
    InvalidStatusLength {
        /// Number of bytes actually seen.
        len: usize,
        /// Minimum required.
        min: usize,
    },
    /// Version response payload was malformed.
    #[error("unexpected version response payload")]
    InvalidVersionResponse,
}
