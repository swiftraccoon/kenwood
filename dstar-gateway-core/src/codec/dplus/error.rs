//! `DPlus` wire-format errors returned by the codec.
//!
//! This is the codec-layer error type. It composes into
//! [`crate::error::ProtocolError`] via `From` impls.

use crate::error::EncodeError;
use crate::validator::CallsignField;

/// Errors returned by the `DPlus` codec functions.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DPlusError {
    /// `DPlus` packet length is not one of the known sizes.
    ///
    /// Valid non-DSVT lengths: 3 (poll), 5 (LINK1/UNLINK ACK), 8 (LINK2 reply),
    /// 28 (LINK2 / echo). Valid DSVT lengths: 29 (voice data), 32 (voice EOT),
    /// 58 (voice header).
    #[error("DPlus packet length {got} not valid for any known type")]
    UnknownPacketLength {
        /// Observed length.
        got: usize,
    },

    /// Packet expected to be DSVT-framed but the magic at `[2..6]` is wrong.
    #[error("expected DSVT magic at offset 2..6, got {got:02X?}")]
    DsvtMagicMissing {
        /// The 4 bytes at offsets 2..6.
        got: [u8; 4],
    },

    /// Stream id at offsets `[14..16]` is zero (reserved per D-STAR spec).
    #[error("DPlus stream id is zero (reserved)")]
    StreamIdZero,

    /// The 5-byte non-DSVT packet has an unknown control byte at offset 4.
    ///
    /// Valid values: 0x00 (UNLINK), 0x01 (LINK1).
    #[error("DPlus 5-byte packet has invalid control byte 0x{byte:02X}")]
    InvalidShortControlByte {
        /// The rejected byte.
        byte: u8,
    },

    /// `DPlus` auth chunk truncated at offset {offset}: needed {need} bytes, have {have}.
    #[error("DPlus auth chunk truncated at offset {offset}: needed {need} bytes, have {have}")]
    AuthChunkTruncated {
        /// Byte offset where truncation occurred.
        offset: usize,
        /// Bytes needed to complete the chunk.
        need: usize,
        /// Bytes actually present.
        have: usize,
    },

    /// Auth chunk flag byte `[1]` failed validation — `(b1 & 0xC0) != 0xC0`.
    #[error("DPlus auth chunk has invalid flag byte 0x{byte:02X} at offset {offset}")]
    AuthChunkFlagsInvalid {
        /// Byte offset of the chunk header.
        offset: usize,
        /// The rejected flag byte.
        byte: u8,
    },

    /// Auth chunk type byte `[2]` is not `0x01`.
    #[error("DPlus auth chunk has invalid type byte 0x{byte:02X} at offset {offset}")]
    AuthChunkTypeInvalid {
        /// Byte offset of the chunk header.
        offset: usize,
        /// The rejected type byte.
        byte: u8,
    },

    /// Auth chunk length `{claimed}` is smaller than the 8-byte chunk header.
    #[error("DPlus auth chunk length {claimed} smaller than 8-byte header at offset {offset}")]
    AuthChunkUndersized {
        /// Byte offset of the chunk header.
        offset: usize,
        /// The too-small length value.
        claimed: usize,
    },

    /// Reserved for callsign parsing errors on received packets.
    ///
    /// Currently unused — the lenient RX path uses
    /// `Callsign::from_wire_bytes` which stores bytes verbatim and
    /// cannot fail. This variant exists so that if a future strict
    /// mode rejects non-printable wire callsigns at the codec
    /// level, the error type already carries the right shape
    /// without a breaking API change (the enum is `#[non_exhaustive]`).
    #[error("DPlus callsign field {field:?} has invalid bytes")]
    CallsignFieldInvalid {
        /// Which callsign field.
        field: CallsignField,
    },

    /// An encoder was called with an undersized output buffer.
    ///
    /// This is a programming error inside
    /// [`crate::session::client::SessionCore`] — it should never
    /// fire in production because the core sizes its own scratch
    /// buffers. Propagated as a variant rather than swallowed so
    /// callers can still surface the fault.
    #[error("DPlus encode buffer too small: need {need}, have {have}")]
    EncodeBufferTooSmall {
        /// How many bytes the encoder needed.
        need: usize,
        /// How many bytes the buffer actually held.
        have: usize,
    },
}

impl From<EncodeError> for DPlusError {
    fn from(value: EncodeError) -> Self {
        match value {
            EncodeError::BufferTooSmall { need, have } => Self::EncodeBufferTooSmall { need, have },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_packet_length_display() {
        let err = DPlusError::UnknownPacketLength { got: 42 };
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn dsvt_magic_missing_display() {
        let err = DPlusError::DsvtMagicMissing {
            got: [0x01, 0x02, 0x03, 0x04],
        };
        assert!(err.to_string().contains("01"));
    }

    #[test]
    fn stream_id_zero_display() {
        let err = DPlusError::StreamIdZero;
        assert_eq!(err.to_string(), "DPlus stream id is zero (reserved)");
    }

    #[test]
    fn auth_chunk_truncated_display() {
        let err = DPlusError::AuthChunkTruncated {
            offset: 16,
            need: 26,
            have: 8,
        };
        let s = err.to_string();
        assert!(s.contains("16"));
        assert!(s.contains("26"));
        assert!(s.contains('8'));
    }
}
