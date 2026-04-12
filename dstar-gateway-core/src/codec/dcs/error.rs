//! `DCS` wire-format errors returned by the codec.

use crate::error::EncodeError;

/// Errors returned by the `DCS` codec functions.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DcsError {
    /// `DCS` packet length is not one of the known sizes.
    #[error("DCS packet length {got} not valid for any known type")]
    UnknownPacketLength {
        /// Observed length.
        got: usize,
    },

    /// Voice frame magic at `[0..4]` is not `b"0001"`.
    #[error("DCS voice magic missing at offset 0..4, got {got:02X?}")]
    VoiceMagicMissing {
        /// Observed 4-byte magic.
        got: [u8; 4],
    },

    /// Stream id at `[43..45]` is zero.
    #[error("DCS stream id is zero (reserved)")]
    StreamIdZero,

    /// Connect reply tag at `[10..13]` is neither `ACK` nor `NAK`.
    #[error("DCS connect reply has unknown tag {tag:02X?}")]
    UnknownConnectTag {
        /// The 3-byte tag observed.
        tag: [u8; 3],
    },

    /// UNLINK byte `[9]` is not the expected space (`0x20`).
    #[error("DCS UNLINK has invalid module byte 0x{byte:02X} at offset 9 (expected 0x20)")]
    UnlinkModuleByteInvalid {
        /// Rejected byte.
        byte: u8,
    },

    /// LINK or UNLINK packet has a non-A-Z module byte at `[8]` or `[9]`.
    #[error("DCS connect packet has invalid module byte 0x{byte:02X} at offset {offset}")]
    InvalidModuleByte {
        /// Byte offset within the 19- or 519-byte packet.
        offset: usize,
        /// Rejected byte.
        byte: u8,
    },

    /// An encoder was called with an undersized output buffer.
    ///
    /// Programming error inside [`crate::session::client::SessionCore`];
    /// surfaced as a variant rather than swallowed so callers can
    /// still observe the fault.
    #[error("DCS encode buffer too small: need {need}, have {have}")]
    EncodeBufferTooSmall {
        /// How many bytes the encoder needed.
        need: usize,
        /// How many bytes the buffer actually held.
        have: usize,
    },

    /// `send_voice` / `send_eot` called before `send_header` cached the TX header.
    ///
    /// The DCS wire format embeds the D-STAR header in every 100-byte
    /// voice frame, so [`crate::session::client::SessionCore`] must
    /// have a cached header before it can encode voice data or EOT.
    /// Call `send_header` first.
    #[error("DCS send_voice called before send_header cached the TX header")]
    NoTxHeader,
}

impl From<EncodeError> for DcsError {
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
        let err = DcsError::UnknownPacketLength { got: 42 };
        assert!(err.to_string().contains("42"));
    }

    #[test]
    fn voice_magic_missing_display() {
        let err = DcsError::VoiceMagicMissing {
            got: [b'X', b'X', b'X', b'X'],
        };
        let s = err.to_string();
        assert!(s.contains("58"), "display should contain hex of 'X' (0x58)");
    }

    #[test]
    fn stream_id_zero_display() {
        let err = DcsError::StreamIdZero;
        assert_eq!(err.to_string(), "DCS stream id is zero (reserved)");
    }

    #[test]
    fn unknown_connect_tag_display() {
        let err = DcsError::UnknownConnectTag {
            tag: [b'F', b'O', b'O'],
        };
        assert!(
            err.to_string().contains("46"),
            "display should contain hex of 'F'"
        );
    }

    #[test]
    fn unlink_module_byte_invalid_display() {
        let err = DcsError::UnlinkModuleByteInvalid { byte: 0x41 };
        let s = err.to_string();
        assert!(s.contains("41"));
        assert!(s.contains('9'));
    }

    #[test]
    fn invalid_module_byte_display() {
        let err = DcsError::InvalidModuleByte {
            offset: 8,
            byte: 0x40,
        };
        let s = err.to_string();
        assert!(s.contains('8'));
        assert!(s.contains("40"));
    }

    #[test]
    fn no_tx_header_display_mentions_send_header() {
        let err = DcsError::NoTxHeader;
        let s = err.to_string();
        assert!(s.contains("send_header"));
    }
}
