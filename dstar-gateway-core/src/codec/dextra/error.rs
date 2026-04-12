//! `DExtra` wire-format errors returned by the codec.

use crate::error::EncodeError;

/// Errors returned by the `DExtra` codec functions.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DExtraError {
    /// `DExtra` packet length is not one of the known sizes.
    #[error("DExtra packet length {got} not valid for any known type")]
    UnknownPacketLength {
        /// Observed length.
        got: usize,
    },

    /// Packet expected to be DSVT-framed but the magic at `[0..4]` is wrong.
    #[error("expected DSVT magic at offset 0..4, got {got:02X?}")]
    DsvtMagicMissing {
        /// Observed bytes.
        got: [u8; 4],
    },

    /// Stream id at offsets `[12..14]` is zero.
    #[error("DExtra stream id is zero (reserved)")]
    StreamIdZero,

    /// LINK packet has a non-A-Z reflector or client module byte.
    #[error("DExtra LINK has invalid module byte 0x{byte:02X} at offset {offset}")]
    InvalidModuleByte {
        /// Byte offset within the 11-byte packet.
        offset: usize,
        /// Rejected byte.
        byte: u8,
    },

    /// Connect reply tag at `[10..13]` is neither `ACK` nor `NAK`.
    #[error("DExtra connect reply has unknown tag {tag:02X?}")]
    UnknownConnectTag {
        /// The 3-byte tag observed.
        tag: [u8; 3],
    },

    /// An encoder was called with an undersized output buffer.
    ///
    /// Programming error inside [`crate::session::client::SessionCore`];
    /// surfaced as a variant rather than swallowed so callers can
    /// still observe the fault.
    #[error("DExtra encode buffer too small: need {need}, have {have}")]
    EncodeBufferTooSmall {
        /// How many bytes the encoder needed.
        need: usize,
        /// How many bytes the buffer actually held.
        have: usize,
    },
}

impl From<EncodeError> for DExtraError {
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
    fn unknown_length_display() {
        let err = DExtraError::UnknownPacketLength { got: 42 };
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn stream_id_zero_display() {
        let err = DExtraError::StreamIdZero;
        assert_eq!(err.to_string(), "DExtra stream id is zero (reserved)");
    }

    #[test]
    fn invalid_module_byte_display() {
        let err = DExtraError::InvalidModuleByte {
            offset: 9,
            byte: 0x40,
        };
        let s = err.to_string();
        assert!(s.contains('9'));
        assert!(s.contains("40"));
    }

    #[test]
    fn unknown_connect_tag_display() {
        let err = DExtraError::UnknownConnectTag {
            tag: [b'F', b'O', b'O'],
        };
        assert!(err.to_string().contains("46"));
    }
}
