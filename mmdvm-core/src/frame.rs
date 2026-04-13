//! Wire frame codec for MMDVM.
//!
//! Each frame is `[0xE0, length, command, payload...]` where `length`
//! is the total frame length (start + length + command + payload).
//! Since the length field is a single byte, a frame can carry at most
//! `255 - 3 = 252` payload bytes.

use crate::command::MMDVM_FRAME_START;
use crate::error::MmdvmError;

/// Minimum frame length (start + length + command).
pub const MIN_FRAME_LEN: u8 = 3;

/// Maximum payload length that fits in a single-byte length field.
pub const MAX_PAYLOAD_LEN: usize = 252;

/// A decoded MMDVM frame.
///
/// Corresponds to the on-wire layout `[0xE0, length, command, payload...]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdvmFrame {
    /// Command or response byte.
    pub command: u8,
    /// Payload bytes (may be empty).
    pub payload: Vec<u8>,
}

impl MmdvmFrame {
    /// Build a frame with no payload.
    #[must_use]
    pub const fn new(command: u8) -> Self {
        Self {
            command,
            payload: Vec::new(),
        }
    }

    /// Build a frame carrying the given payload.
    #[must_use]
    pub const fn with_payload(command: u8, payload: Vec<u8>) -> Self {
        Self { command, payload }
    }
}

/// Encode a frame to wire bytes: `[0xE0, length, command, payload...]`.
///
/// # Errors
///
/// Returns [`MmdvmError::PayloadTooLarge`] if the payload would overflow
/// the single-byte length field (payload longer than
/// [`MAX_PAYLOAD_LEN`] bytes).
pub fn encode_frame(frame: &MmdvmFrame) -> Result<Vec<u8>, MmdvmError> {
    if frame.payload.len() > MAX_PAYLOAD_LEN {
        return Err(MmdvmError::PayloadTooLarge {
            len: frame.payload.len(),
        });
    }
    // Cast is infallible because `payload.len() <= 252` and
    // `3 + 252 == 255 < 256`, which fits in u8.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bounded by MAX_PAYLOAD_LEN check above"
    )]
    let length = (3 + frame.payload.len()) as u8;
    let mut buf = Vec::with_capacity(usize::from(length));
    buf.push(MMDVM_FRAME_START);
    buf.push(length);
    buf.push(frame.command);
    buf.extend_from_slice(&frame.payload);
    Ok(buf)
}

/// Decode one frame from a byte buffer.
///
/// Returns `Ok(Some((frame, bytes_consumed)))` if a complete frame is
/// available at the start of `data`, or `Ok(None)` if more bytes are
/// needed. Trailing bytes beyond `bytes_consumed` are left untouched
/// for the caller to hand to the next `decode_frame` call.
///
/// # Errors
///
/// - [`MmdvmError::InvalidStartByte`] if the first byte is not `0xE0`.
/// - [`MmdvmError::InvalidLength`] if the length field is less than 3.
pub fn decode_frame(data: &[u8]) -> Result<Option<(MmdvmFrame, usize)>, MmdvmError> {
    let Some(&first) = data.first() else {
        return Ok(None);
    };
    if first != MMDVM_FRAME_START {
        return Err(MmdvmError::InvalidStartByte { got: first });
    }
    let Some(&length) = data.get(1) else {
        return Ok(None);
    };
    if length < MIN_FRAME_LEN {
        return Err(MmdvmError::InvalidLength { len: length });
    }
    let frame_len = usize::from(length);
    if data.len() < frame_len {
        return Ok(None);
    }
    let Some(&command) = data.get(2) else {
        // Impossible because frame_len >= 3 and data.len() >= frame_len,
        // but the lint-safe get() path is cheap.
        return Ok(None);
    };
    let payload = data
        .get(3..frame_len)
        .ok_or(MmdvmError::FrameTooShort { len: data.len() })?
        .to_vec();
    Ok(Some((MmdvmFrame { command, payload }, frame_len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{MMDVM_DSTAR_DATA, MMDVM_GET_VERSION};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn new_frame_has_empty_payload() {
        let f = MmdvmFrame::new(MMDVM_GET_VERSION);
        assert_eq!(f.command, MMDVM_GET_VERSION);
        assert!(f.payload.is_empty());
    }

    #[test]
    fn encode_minimal_frame() -> TestResult {
        let f = MmdvmFrame::new(MMDVM_GET_VERSION);
        let wire = encode_frame(&f)?;
        assert_eq!(wire, [0xE0, 3, 0x00]);
        Ok(())
    }

    #[test]
    fn encode_with_payload() -> TestResult {
        let f = MmdvmFrame::with_payload(MMDVM_DSTAR_DATA, vec![0xAA; 12]);
        let wire = encode_frame(&f)?;
        assert_eq!(wire.len(), 15);
        assert_eq!(wire.first().copied(), Some(0xE0));
        assert_eq!(wire.get(1).copied(), Some(15));
        assert_eq!(wire.get(2).copied(), Some(MMDVM_DSTAR_DATA));
        Ok(())
    }

    #[test]
    fn encode_rejects_oversized_payload() {
        let f = MmdvmFrame::with_payload(0x00, vec![0u8; MAX_PAYLOAD_LEN + 1]);
        let err = encode_frame(&f);
        assert!(
            matches!(err, Err(MmdvmError::PayloadTooLarge { len }) if len == MAX_PAYLOAD_LEN + 1),
            "expected PayloadTooLarge, got {err:?}"
        );
    }

    #[test]
    fn encode_accepts_max_size_payload() -> TestResult {
        let f = MmdvmFrame::with_payload(0x00, vec![0u8; MAX_PAYLOAD_LEN]);
        let wire = encode_frame(&f)?;
        assert_eq!(wire.len(), 255);
        assert_eq!(wire.get(1).copied(), Some(255));
        Ok(())
    }

    #[test]
    fn decode_empty_returns_none() -> TestResult {
        assert!(decode_frame(&[])?.is_none());
        Ok(())
    }

    #[test]
    fn decode_start_only_returns_none() -> TestResult {
        assert!(decode_frame(&[0xE0])?.is_none());
        Ok(())
    }

    #[test]
    fn decode_invalid_start_byte() {
        let err = decode_frame(&[0xFF, 3, 0x00]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidStartByte { got: 0xFF })),
            "expected InvalidStartByte(0xFF), got {err:?}"
        );
    }

    #[test]
    fn decode_invalid_length() {
        let err = decode_frame(&[0xE0, 2, 0x00]);
        assert!(
            matches!(err, Err(MmdvmError::InvalidLength { len: 2 })),
            "expected InvalidLength(2), got {err:?}"
        );
    }

    #[test]
    fn decode_incomplete_returns_none() -> TestResult {
        // Length says 5, only 3 bytes available.
        assert!(decode_frame(&[0xE0, 5, 0x00])?.is_none());
        Ok(())
    }
}
