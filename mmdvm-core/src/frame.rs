// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Incremental MMDVM framing in the short and extended wire forms.
//!
//! A short frame is `[0xE0, total_length, command, payload...]`. An extended
//! frame is `[0xE0, 0, total_length_minus_255, command, payload...]`.
//! `MMDVMHost/Modem.cpp` uses the extended form in `writeFMData` and decodes
//! both forms in `getResponse`.
//! See the [frame example](crate#encode-and-decode-a-frame).

use crate::command::{Command, MMDVM_FRAME_START};
use crate::error::MmdvmError;

/// Bytes in a short frame header: start, length and command.
pub const MIN_FRAME_LEN: u8 = 3;

/// Payload bytes representable in the short wire form.
pub const MAX_SHORT_PAYLOAD_LEN: usize = 252;

/// Payload bytes representable in the extended wire form: `510 - 4`.
pub const MAX_PAYLOAD_LEN: usize = 506;

/// A command identifier and an owned payload of at most 506 bytes.
///
/// Construction bounds the payload so every frame can be encoded. Known and
/// unrecognized command identifiers use the same framing; command-specific
/// payload validation belongs to its parser or modem operation.
/// See the [frame example](crate#encode-and-decode-a-frame).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MmdvmFrame {
    command: Command,
    payload: Vec<u8>,
}

impl MmdvmFrame {
    /// Construct an empty command or response frame.
    #[must_use]
    pub const fn new(command: Command) -> Self {
        Self {
            command,
            payload: Vec::new(),
        }
    }

    /// Construct a frame, retaining the supplied payload allocation.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::PayloadTooLarge`] when the payload exceeds
    /// [`MAX_PAYLOAD_LEN`] bytes. No frame is produced on rejection.
    pub fn with_payload(command: Command, payload: Vec<u8>) -> Result<Self, MmdvmError> {
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(MmdvmError::PayloadTooLarge { len: payload.len() });
        }
        Ok(Self { command, payload })
    }

    /// Return the command or response identifier.
    #[must_use]
    pub const fn command(&self) -> Command {
        self.command
    }

    /// Borrow the complete payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consume the frame and transfer its payload allocation.
    #[must_use]
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    /// Encode this frame using the shortest representable wire form.
    ///
    /// Payloads through 252 bytes use a three-byte header. Larger payloads
    /// use a four-byte header; the total frame occupies at most 510 bytes.
    /// Decoding an extended frame with a shorter payload and re-encoding it
    /// preserves the command and payload while choosing the short header.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut wire = Vec::with_capacity(self.payload.len() + 4);
        wire.push(MMDVM_FRAME_START);
        if self.payload.len() <= MAX_SHORT_PAYLOAD_LEN {
            wire.push(length_byte(self.payload.len() + 3));
        } else {
            wire.push(0);
            wire.push(length_byte(self.payload.len() + 4 - 255));
        }
        wire.push(self.command.get());
        wire.extend_from_slice(&self.payload);
        wire
    }

    /// Decode one frame at the beginning of a borrowed byte buffer.
    ///
    /// Returns `Ok(None)` for every incomplete prefix, including an empty
    /// buffer. A complete frame returns its owned command/payload and the
    /// number of consumed bytes; subsequent bytes remain with the caller.
    /// Both wire forms are accepted, including extended total lengths
    /// `255..=510`. A complete frame allocates only its payload copy.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmError::InvalidStartByte`] when the first byte is not
    /// `0xE0`, or [`MmdvmError::InvalidLength`] for short length bytes 1 or 2.
    /// A zero length byte starts an extended header and requires a third byte.
    pub fn from_bytes(data: &[u8]) -> Result<Option<(Self, usize)>, MmdvmError> {
        let Some(&first) = data.first() else {
            return Ok(None);
        };
        if first != MMDVM_FRAME_START {
            return Err(MmdvmError::InvalidStartByte { got: first });
        }
        let Some(&length) = data.get(1) else {
            return Ok(None);
        };
        let (frame_len, header_len) = if length == 0 {
            let Some(&length2) = data.get(2) else {
                return Ok(None);
            };
            (usize::from(length2) + 255, 4)
        } else {
            if length < MIN_FRAME_LEN {
                return Err(MmdvmError::InvalidLength { len: length });
            }
            (usize::from(length), 3)
        };
        let Some((&command, payload)) = data
            .get(header_len - 1..frame_len)
            .and_then(<[u8]>::split_first)
        else {
            return Ok(None);
        };
        let frame = Self::with_payload(Command::from(command), payload.to_vec())?;
        Ok(Some((frame, frame_len)))
    }
}

fn length_byte(length: usize) -> u8 {
    u8::try_from(length)
        .unwrap_or_else(|_| unreachable!("validated frame lengths fit their wire field"))
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn constructor_enforces_the_complete_wire_payload_domain() -> TestResult {
        for length in [0, 251, 252, 253, MAX_PAYLOAD_LEN] {
            let frame = MmdvmFrame::with_payload(Command::FM_DATA, vec![0xAA; length])?;
            let wire = frame.to_bytes();
            let (decoded, consumed) =
                MmdvmFrame::from_bytes(&wire)?.ok_or("complete frame missing")?;
            assert_eq!(decoded, frame);
            assert_eq!(consumed, wire.len());
            if length <= MAX_SHORT_PAYLOAD_LEN {
                assert_eq!(wire.get(1).copied(), Some(u8::try_from(length + 3)?));
            } else {
                assert_eq!(wire.get(1).copied(), Some(0));
                assert_eq!(wire.get(2).copied(), Some(u8::try_from(length + 4 - 255)?));
            }
        }
        let rejected = MmdvmFrame::with_payload(Command::FM_DATA, vec![0; MAX_PAYLOAD_LEN + 1]);
        assert!(
            matches!(rejected, Err(MmdvmError::PayloadTooLarge { len: 507 })),
            "{rejected:?}"
        );
        Ok(())
    }

    #[test]
    fn every_extended_length_and_prefix_preserves_incremental_framing() -> TestResult {
        for length2 in 0..=u8::MAX {
            let total = usize::from(length2) + 255;
            let mut wire = vec![MMDVM_FRAME_START, 0, length2, Command::FM_DATA.get()];
            wire.resize(total, 0xA5);
            for prefix in 0..wire.len() {
                assert_eq!(
                    MmdvmFrame::from_bytes(wire.get(..prefix).ok_or("prefix")?)?,
                    None
                );
            }
            wire.extend_from_slice(&[0xAA, 0xBB]);
            let (frame, consumed) =
                MmdvmFrame::from_bytes(&wire)?.ok_or("extended frame missing")?;
            assert_eq!(consumed, total);
            assert_eq!(frame.command(), Command::FM_DATA);
            assert_eq!(frame.payload(), vec![0xA5; total - 4]);
            assert_eq!(wire.get(consumed..), Some([0xAA, 0xBB].as_slice()));
        }
        Ok(())
    }

    #[test]
    fn malformed_prefixes_are_errors_before_payload_allocation() -> TestResult {
        let _valid = MmdvmFrame::from_bytes(&[MMDVM_FRAME_START, 3, 0])?;
        for length in [1, 2] {
            let result = MmdvmFrame::from_bytes(&[MMDVM_FRAME_START, length]);
            assert!(
                matches!(result, Err(MmdvmError::InvalidLength { len }) if len == length),
                "{result:?}"
            );
        }
        let result = MmdvmFrame::from_bytes(&[0xFF, 0, 0]);
        assert!(
            matches!(result, Err(MmdvmError::InvalidStartByte { got: 0xFF })),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn owned_payload_extraction_reuses_the_callers_allocation() -> TestResult {
        let payload = vec![1, 2, 3];
        let allocation = payload.as_ptr();
        let frame = MmdvmFrame::with_payload(Command::DSTAR_DATA, payload)?;
        let extracted = frame.into_payload();
        assert_eq!(extracted.as_ptr(), allocation);
        Ok(())
    }
}
