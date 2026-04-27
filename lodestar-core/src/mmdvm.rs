// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! MMDVM framing primitives and radio-mode detection helpers.
//!
//! The TH-D75 in Reflector Terminal Mode (Menu 650 = 1) speaks MMDVM
//! binary framing on its BT/USB channel rather than CAT ASCII. The
//! wire format is `[0xE0, len, cmd, payload...]`. This module exposes
//! the minimum FFI surface Lodestar needs to:
//!
//! - Build MMDVM frames (e.g. `GetVersion` for mode probing,
//!   `DStarHeader`/`DStarData`/`DStarEot` for voice relay).
//! - Decode arbitrary MMDVM bytes coming off the radio.
//! - Detect whether the radio is currently in MMDVM mode by looking
//!   at the first byte of a response: `0xE0` means MMDVM, anything
//!   else (or silence / `'?'` / `'N'`) means CAT.
//!
//! Heavy lifting lives in the `mmdvm-core` crate; this module is a
//! thin `UniFFI` wrapper.

use mmdvm_core::frame::{MmdvmFrame as CoreFrame, decode_frame, encode_frame};
use mmdvm_core::{MMDVM_FRAME_START, MMDVM_GET_VERSION};
use thiserror::Error;

/// The MMDVM frame-start byte (`0xE0`). A response from the radio
/// starting with this byte means it's in MMDVM (Reflector Terminal)
/// mode; CAT mode won't produce this byte.
pub const MMDVM_START_BYTE: u8 = MMDVM_FRAME_START;

/// MMDVM `GetVersion` command byte (`0x00`).
pub const MMDVM_CMD_GET_VERSION: u8 = MMDVM_GET_VERSION;

/// Errors surfaced by the MMDVM primitives.
#[derive(Debug, Clone, Error, PartialEq, Eq, uniffi::Error)]
#[non_exhaustive]
pub enum MmdvmFrameError {
    /// Payload is longer than a single-byte length field can address.
    #[error("payload too large: {len} bytes (max {max})")]
    PayloadTooLarge {
        /// Supplied payload length.
        len: u32,
        /// Maximum payload length (252).
        max: u32,
    },
    /// Decoder consumed bytes but the frame is still incomplete.
    #[error("need more bytes to complete the frame (have {got}, need {need})")]
    Incomplete {
        /// Bytes present in the buffer so far.
        got: u32,
        /// Minimum bytes required for the current in-progress frame.
        need: u32,
    },
    /// Length byte was below the 3-byte minimum.
    #[error("length byte {got} below minimum 3")]
    ShortLength {
        /// The bad length byte.
        got: u32,
    },
    /// First byte wasn't `0xE0`.
    #[error("expected 0xE0 frame-start, got 0x{actual:02x}")]
    BadStart {
        /// The unexpected byte.
        actual: u8,
    },
}

impl From<mmdvm_core::MmdvmError> for MmdvmFrameError {
    fn from(err: mmdvm_core::MmdvmError) -> Self {
        match err {
            mmdvm_core::MmdvmError::PayloadTooLarge { len } => Self::PayloadTooLarge {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                max: u32::try_from(mmdvm_core::frame::MAX_PAYLOAD_LEN).unwrap_or(u32::MAX),
            },
            mmdvm_core::MmdvmError::InvalidLength { len } => Self::ShortLength {
                got: u32::from(len),
            },
            mmdvm_core::MmdvmError::FrameTooShort { len } => Self::ShortLength {
                got: u32::try_from(len).unwrap_or(u32::MAX),
            },
            mmdvm_core::MmdvmError::InvalidStartByte { got } => Self::BadStart { actual: got },
            // Response-parsing errors (wrong status layout, version payload,
            // unknown NAK reason) aren't reachable from `encode_frame` or
            // `decode_frame`, but enum is `#[non_exhaustive]` so cover them.
            _ => Self::BadStart { actual: 0 },
        }
    }
}

/// FFI-friendly mirror of [`mmdvm_core::frame::MmdvmFrame`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MmdvmFrame {
    /// MMDVM command / response byte.
    pub command: u8,
    /// Payload bytes (0-252 bytes).
    pub payload: Vec<u8>,
}

impl From<CoreFrame> for MmdvmFrame {
    fn from(f: CoreFrame) -> Self {
        Self {
            command: f.command,
            payload: f.payload,
        }
    }
}

impl From<MmdvmFrame> for CoreFrame {
    fn from(f: MmdvmFrame) -> Self {
        Self {
            command: f.command,
            payload: f.payload,
        }
    }
}

/// Decoder outcome from [`decode_mmdvm_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MmdvmDecodeResult {
    /// The decoded frame, if one was complete at the start of the buffer.
    pub frame: Option<MmdvmFrame>,
    /// Number of bytes consumed. `0` means the buffer holds a partial
    /// frame — caller should keep accumulating and try again.
    pub bytes_consumed: u32,
}

/// Build the 3-byte `GetVersion` probe frame: `[0xE0, 0x03, 0x00]`.
#[must_use]
#[uniffi::export]
pub fn mmdvm_get_version_probe() -> Vec<u8> {
    vec![MMDVM_FRAME_START, 0x03, MMDVM_GET_VERSION]
}

/// Build any MMDVM frame.
///
/// # Errors
///
/// - [`MmdvmFrameError::PayloadTooLarge`] if `payload.len() > 252`.
#[uniffi::export]
pub fn build_mmdvm_frame(command: u8, payload: Vec<u8>) -> Result<Vec<u8>, MmdvmFrameError> {
    let frame = CoreFrame { command, payload };
    encode_frame(&frame).map_err(MmdvmFrameError::from)
}

/// Decode one MMDVM frame from the start of `bytes`.
///
/// Returns the decoded frame plus the number of bytes consumed.
/// If the buffer holds a partial frame, `frame` is `None` and
/// `bytes_consumed` is `0` — caller should keep buffering.
///
/// # Errors
///
/// - [`MmdvmFrameError::BadStart`] if the first byte isn't `0xE0`.
/// - [`MmdvmFrameError::ShortLength`] if the length byte is below 3.
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary — `sequence<u8>` in UDL maps to owned `Vec<u8>`."
)]
#[uniffi::export]
pub fn decode_mmdvm_bytes(bytes: Vec<u8>) -> Result<MmdvmDecodeResult, MmdvmFrameError> {
    match decode_frame(&bytes)? {
        Some((frame, consumed)) => Ok(MmdvmDecodeResult {
            frame: Some(frame.into()),
            bytes_consumed: u32::try_from(consumed).unwrap_or(u32::MAX),
        }),
        None => Ok(MmdvmDecodeResult {
            frame: None,
            bytes_consumed: 0,
        }),
    }
}

/// Quick test used by mode-probe callers: is the first byte `0xE0`?
///
/// Returning `true` is the cheapest possible signal that the radio is
/// in Reflector Terminal Mode. A caller who wants more certainty can
/// feed the same bytes through [`decode_mmdvm_bytes`] to verify a
/// full frame parses out.
#[must_use]
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary — takes owned `Vec<u8>` so Swift can hand it in directly."
)]
#[uniffi::export]
pub fn looks_like_mmdvm_response(bytes: Vec<u8>) -> bool {
    bytes.first().copied() == Some(MMDVM_FRAME_START)
}

#[cfg(test)]
mod tests {
    use super::{
        MMDVM_CMD_GET_VERSION, MMDVM_START_BYTE, MmdvmFrameError, build_mmdvm_frame,
        decode_mmdvm_bytes, looks_like_mmdvm_response, mmdvm_get_version_probe,
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn get_version_probe_is_three_bytes() {
        assert_eq!(mmdvm_get_version_probe(), vec![0xE0, 0x03, 0x00]);
        assert_eq!(MMDVM_START_BYTE, 0xE0);
        assert_eq!(MMDVM_CMD_GET_VERSION, 0x00);
    }

    #[test]
    fn build_frame_no_payload() -> TestResult {
        let out = build_mmdvm_frame(0x01, vec![])?;
        assert_eq!(out, vec![0xE0, 0x03, 0x01]);
        Ok(())
    }

    #[test]
    fn build_frame_with_payload() -> TestResult {
        let out = build_mmdvm_frame(0x10, vec![0x11, 0x22, 0x33])?;
        assert_eq!(out, vec![0xE0, 0x06, 0x10, 0x11, 0x22, 0x33]);
        Ok(())
    }

    #[test]
    fn build_frame_rejects_oversize() {
        let big = vec![0u8; 300];
        let result = build_mmdvm_frame(0x10, big);
        assert!(
            matches!(
                result,
                Err(MmdvmFrameError::PayloadTooLarge { len: 300, .. })
            ),
            "got {result:?}"
        );
    }

    #[test]
    fn decode_round_trip() -> TestResult {
        let wire = build_mmdvm_frame(0x10, vec![0xA0, 0xB0])?;
        let wire_len = u32::try_from(wire.len()).unwrap_or(u32::MAX);
        let decoded = decode_mmdvm_bytes(wire)?;
        assert_eq!(decoded.bytes_consumed, wire_len);
        let frame = decoded.frame.ok_or("frame missing")?;
        assert_eq!(frame.command, 0x10);
        assert_eq!(frame.payload, vec![0xA0, 0xB0]);
        Ok(())
    }

    #[test]
    fn decode_partial_returns_none() -> TestResult {
        // A frame header claims length 10 but we've only fed the first 5 bytes.
        let partial = vec![0xE0, 0x0A, 0x01, 0x00, 0x00];
        let decoded = decode_mmdvm_bytes(partial)?;
        assert!(decoded.frame.is_none());
        assert_eq!(decoded.bytes_consumed, 0);
        Ok(())
    }

    #[test]
    fn decode_rejects_bad_start() {
        let result = decode_mmdvm_bytes(vec![0x7F, 0x03, 0x00]);
        assert!(
            matches!(result, Err(MmdvmFrameError::BadStart { actual: 0x7F })),
            "got {result:?}"
        );
    }

    #[test]
    fn looks_like_mmdvm_happy() {
        assert!(looks_like_mmdvm_response(vec![0xE0, 0x03, 0x00]));
    }

    #[test]
    fn looks_like_mmdvm_rejects_cat() {
        assert!(!looks_like_mmdvm_response(vec![b'?', b'\r']));
        assert!(!looks_like_mmdvm_response(vec![b'I', b'D', b' ']));
    }

    #[test]
    fn looks_like_mmdvm_rejects_empty() {
        assert!(!looks_like_mmdvm_response(vec![]));
    }
}
