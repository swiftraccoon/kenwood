// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! MMDVM framing primitives and radio-mode detection helpers.
//!
//! The TH-D75 in Reflector Terminal Mode (Menu 650 = 1) speaks MMDVM
//! binary framing on its BT/USB channel rather than CAT ASCII. The
//! short wire format is `[0xE0, len, cmd, payload...]`; extended frames use
//! `[0xE0, 0, total_length_minus_255, cmd, payload...]`. This module exposes
//! the minimum FFI surface Lodestar needs to:
//!
//! - Build MMDVM frames (for example, `GetVersion` for mode probing and
//!   D-STAR header, data, and end-of-transmission frames for voice relay).
//! - Decode arbitrary MMDVM bytes coming off the radio.
//! - Prove MMDVM mode by decoding a complete response to `GetVersion`.
//!   A leading `0xE0` without a complete frame is never sufficient proof.
//!
//! Heavy lifting lives in the `mmdvm-core` crate; this module is a
//! thin `UniFFI` wrapper.

use mmdvm_core::{
    Command, MMDVM_FRAME_START, MmdvmFrame as CoreFrame, VersionResponse as CoreVersionResponse,
};
use thiserror::Error;

/// The MMDVM frame-start byte (`0xE0`).
///
/// This byte identifies a candidate frame. Callers must decode the complete
/// frame before treating a transport as MMDVM.
pub const MMDVM_START_BYTE: u8 = MMDVM_FRAME_START;

/// MMDVM `GetVersion` command byte (`0x00`).
pub const MMDVM_CMD_GET_VERSION: u8 = Command::GET_VERSION.get();

/// Errors surfaced by the MMDVM primitives.
#[derive(Debug, Clone, Error, PartialEq, Eq, uniffi::Error)]
#[non_exhaustive]
pub enum MmdvmFrameError {
    /// Payload exceeds the extended frame's 506-byte capacity.
    #[error("payload too large: {len} bytes (max {max})")]
    PayloadTooLarge {
        /// Supplied payload length.
        len: u32,
        /// Maximum payload length (506).
        max: u32,
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
    /// `GetVersion` payload was not a proved protocol v1/v2 response.
    #[error("invalid MMDVM GetVersion response payload")]
    InvalidVersionResponse,
    /// A codec failure outside the framing and version categories above.
    #[error("MMDVM codec failed: {message}")]
    Codec {
        /// Original codec error description.
        message: String,
    },
}

impl From<mmdvm_core::MmdvmError> for MmdvmFrameError {
    fn from(err: mmdvm_core::MmdvmError) -> Self {
        match err {
            mmdvm_core::MmdvmError::PayloadTooLarge { len } => Self::PayloadTooLarge {
                len: u32::try_from(len).unwrap_or(u32::MAX),
                max: u32::try_from(mmdvm_core::MAX_PAYLOAD_LEN).unwrap_or(u32::MAX),
            },
            mmdvm_core::MmdvmError::InvalidLength { len } => Self::ShortLength {
                got: u32::from(len),
            },
            mmdvm_core::MmdvmError::InvalidStartByte { got } => Self::BadStart { actual: got },
            mmdvm_core::MmdvmError::InvalidVersionResponse
            | mmdvm_core::MmdvmError::UnsupportedProtocolVersion { .. }
            | mmdvm_core::MmdvmError::InvalidVersionText { .. }
            | mmdvm_core::MmdvmError::VersionDescriptionControl { .. } => {
                Self::InvalidVersionResponse
            }
            error => Self::Codec {
                message: error.to_string(),
            },
        }
    }
}

/// FFI frame fields, checked when encoding or converting to [`mmdvm_core::MmdvmFrame`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MmdvmFrame {
    /// MMDVM command / response byte.
    pub command: u8,
    /// Candidate payload bytes; encoding admits at most 506 bytes.
    pub payload: Vec<u8>,
}

impl MmdvmFrame {
    /// Validate and encode this foreign candidate using short or extended framing.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmFrameError::PayloadTooLarge`] above 506 payload bytes.
    pub fn encode(self) -> Result<Vec<u8>, MmdvmFrameError> {
        Ok(CoreFrame::try_from(self)?.to_bytes())
    }

    /// Decode one frame at the beginning of a borrowed buffer.
    ///
    /// A partial frame returns no frame and zero consumed bytes. Complete
    /// decoding leaves trailing bytes for the caller's next invocation.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmFrameError::BadStart`] without `0xE0`, or
    /// [`MmdvmFrameError::ShortLength`] for an invalid total length.
    pub fn decode(bytes: &[u8]) -> Result<MmdvmDecodeResult, MmdvmFrameError> {
        match CoreFrame::from_bytes(bytes)? {
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
}

impl From<CoreFrame> for MmdvmFrame {
    fn from(f: CoreFrame) -> Self {
        Self {
            command: f.command().get(),
            payload: f.into_payload(),
        }
    }
}

impl TryFrom<MmdvmFrame> for CoreFrame {
    type Error = mmdvm_core::MmdvmError;

    /// Validate the candidate payload through [`CoreFrame::with_payload`].
    ///
    /// # Errors
    ///
    /// Returns [`mmdvm_core::MmdvmError::PayloadTooLarge`] above 506 bytes.
    fn try_from(f: MmdvmFrame) -> Result<Self, Self::Error> {
        Self::with_payload(Command::from(f.command), f.payload)
    }
}

/// Decoder outcome from [`decode_mmdvm_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MmdvmDecodeResult {
    /// The decoded frame, if one was complete at the start of the buffer.
    frame: Option<MmdvmFrame>,
    /// Number of bytes consumed. `0` means the buffer holds a partial
    /// frame; the caller should keep accumulating and try again.
    bytes_consumed: u32,
}

impl MmdvmDecodeResult {
    /// Borrow the complete decoded frame, if present.
    #[must_use]
    pub const fn frame(&self) -> Option<&MmdvmFrame> {
        self.frame.as_ref()
    }
    /// Bytes belonging to the complete frame, or zero for incomplete input.
    #[must_use]
    pub const fn bytes_consumed(&self) -> u32 {
        self.bytes_consumed
    }
}

/// Proved protocol and description from an MMDVM `GetVersion` payload.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MmdvmVersionResponse {
    /// MMDVM protocol version. Lodestar accepts only version 1 or 2.
    protocol: u8,
    /// Nonempty modem description supplied by the radio.
    description: String,
}

impl MmdvmVersionResponse {
    /// The proved protocol version, either one or two.
    #[must_use]
    pub const fn protocol(&self) -> u8 {
        self.protocol
    }
    /// The nonempty checked description, without trailing padding.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Parse a supported `GetVersion` payload with a nonempty description.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmFrameError::InvalidVersionResponse`] for missing fields,
    /// unsupported protocol, invalid UTF-8, empty text or embedded controls.
    pub fn parse(payload: &[u8]) -> Result<Self, MmdvmFrameError> {
        let version = CoreVersionResponse::parse(payload)
            .map_err(|_| MmdvmFrameError::InvalidVersionResponse)?;
        if version.description().is_empty() {
            return Err(MmdvmFrameError::InvalidVersionResponse);
        }
        Ok(Self {
            protocol: version.protocol().get(),
            description: version.into_description(),
        })
    }
}

/// Build the 3-byte `GetVersion` probe frame: `[0xE0, 0x03, 0x00]`.
#[must_use]
#[uniffi::export]
pub fn mmdvm_get_version_probe() -> Vec<u8> {
    CoreFrame::new(Command::GET_VERSION).to_bytes()
}

/// Build any MMDVM frame.
///
/// # Errors
///
/// - [`MmdvmFrameError::PayloadTooLarge`] if `payload.len() > 506`.
#[uniffi::export]
pub fn build_mmdvm_frame(command: u8, payload: Vec<u8>) -> Result<Vec<u8>, MmdvmFrameError> {
    MmdvmFrame { command, payload }.encode()
}

/// Decode one MMDVM frame from the start of `bytes`.
///
/// Returns the decoded frame plus the number of bytes consumed.
/// If the buffer holds a partial frame, `frame` is `None` and
/// `bytes_consumed` is `0`; the caller should keep buffering.
///
/// # Errors
///
/// - [`MmdvmFrameError::BadStart`] if the first byte isn't `0xE0`.
/// - [`MmdvmFrameError::ShortLength`] if the length byte is 1 or 2.
///   Zero starts an extended header.
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary: `sequence<u8>` in UDL maps to owned `Vec<u8>`."
)]
#[uniffi::export]
pub fn decode_mmdvm_bytes(bytes: Vec<u8>) -> Result<MmdvmDecodeResult, MmdvmFrameError> {
    MmdvmFrame::decode(&bytes)
}

/// Parse and validate one MMDVM `GetVersion` response payload.
///
/// This rejects an echoed three-byte request because that frame has no
/// payload. A response proves terminal mode only when it reports protocol 1
/// or 2 and includes a nonempty description.
///
/// # Errors
///
/// Returns [`MmdvmFrameError::InvalidVersionResponse`] for an empty,
/// truncated, unsupported, description-free or invalid UTF-8 payload, or a
/// description containing a control character before its trailing padding.
#[expect(
    clippy::needless_pass_by_value,
    reason = "UniFFI FFI boundary: `sequence<u8>` in UDL maps to owned `Vec<u8>`."
)]
#[uniffi::export]
pub fn parse_mmdvm_version_payload(
    payload: Vec<u8>,
) -> Result<MmdvmVersionResponse, MmdvmFrameError> {
    MmdvmVersionResponse::parse(&payload)
}

#[cfg(test)]
mod tests {
    use super::{
        MMDVM_CMD_GET_VERSION, MMDVM_START_BYTE, MmdvmFrameError, build_mmdvm_frame,
        decode_mmdvm_bytes, mmdvm_get_version_probe, parse_mmdvm_version_payload,
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
        let big = vec![0u8; 507];
        let result = build_mmdvm_frame(0x10, big);
        assert!(
            matches!(
                result,
                Err(MmdvmFrameError::PayloadTooLarge { len: 507, max: 506 })
            ),
            "got {result:?}"
        );
    }

    #[test]
    fn extended_frames_round_trip_across_the_ffi_boundary() -> TestResult {
        for length in [252, 253, 506] {
            let payload = vec![0xA5; length];
            let wire = build_mmdvm_frame(0x65, payload.clone())?;
            let expected_header = if length == 252 { 3 } else { 4 };
            assert_eq!(wire.len(), payload.len() + expected_header);
            let decoded = decode_mmdvm_bytes(wire)?;
            let frame = decoded.frame.ok_or("extended frame missing")?;
            assert_eq!(frame.command, 0x65);
            assert_eq!(frame.payload, payload);
        }
        Ok(())
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
    fn version_payload_requires_supported_protocol_and_description() -> TestResult {
        let parsed = parse_mmdvm_version_payload(b"\x01MMDVM 2018".to_vec())?;
        assert_eq!(parsed.protocol, 1);
        assert_eq!(parsed.description, "MMDVM 2018");

        for invalid in [vec![], vec![1], b"\x03MMDVM".to_vec()] {
            assert_eq!(
                parse_mmdvm_version_payload(invalid),
                Err(MmdvmFrameError::InvalidVersionResponse)
            );
        }
        Ok(())
    }
}
