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
    /// The first byte is not `0xE0`.
    #[error("invalid start byte: 0x{got:02X} (expected 0xE0)")]
    InvalidStartByte {
        /// The byte found at position 0.
        got: u8,
    },
    /// The length field is 1 or 2: shorter than any frame can be,
    /// and not the extended-form marker (0).
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
