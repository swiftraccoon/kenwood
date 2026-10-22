//! Error types for the MMDVM codec.
//!
//! All fallible codec operations return `Result<_, MmdvmError>`.
//! Variants carry raw bytes / lengths so callers can pattern-match
//! without parsing error strings.

use thiserror::Error;

/// Errors produced by the MMDVM codec.
///
/// See the [framing](crate#encode-and-decode-a-frame) and
/// [response](crate#parse-version-and-status-responses) examples.
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
    /// Payload exceeds the 506 bytes representable by an extended frame.
    #[error("MMDVM payload too large: {len} bytes (maximum 506)")]
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
    /// Version response lacks its protocol byte or complete fixed header.
    #[error("unexpected version response payload")]
    InvalidVersionResponse,
    /// A version byte names a protocol this codec does not implement.
    #[error("unsupported MMDVM protocol version: {protocol}")]
    UnsupportedProtocolVersion {
        /// Unrecognized protocol version byte.
        protocol: u8,
    },
    /// Firmware description bytes are not valid UTF-8.
    #[error("invalid UTF-8 in version description at byte {valid_up_to}")]
    InvalidVersionText {
        /// Valid prefix length within the description bytes.
        valid_up_to: usize,
        /// Invalid sequence length, or none for an incomplete sequence.
        error_len: Option<usize>,
    },
    /// A description contains a control character before its trailing padding.
    #[error("control character {character:?} in version description at byte {byte_offset}")]
    VersionDescriptionControl {
        /// UTF-8 byte offset within the description.
        byte_offset: usize,
        /// Rejected control character.
        character: char,
    },
    /// A strict mode conversion received an unrecognized mode byte.
    #[error("unknown MMDVM modem mode: 0x{mode:02X}")]
    InvalidModemMode {
        /// Unrecognized mode byte.
        mode: u8,
    },
}
