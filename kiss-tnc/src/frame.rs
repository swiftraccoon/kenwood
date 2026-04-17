//! KISS frame type and one-shot encode/decode helpers.

use alloc::vec::Vec;

use crate::command::{CMD_RETURN, FEND, FESC, KissCommand, KissPort, TFEND, TFESC};
use crate::error::KissError;

/// A decoded KISS frame.
///
/// The type indicator byte is split into `port` (high nibble) and
/// `command` (low nibble). For the TH-D75, port is always 0. Typed
/// accessors are available via [`Self::port_typed`] and
/// [`Self::command_typed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KissFrame {
    /// TNC port number (high nibble of type byte). Always 0 for TH-D75.
    pub port: u8,
    /// KISS command (low nibble of type byte).
    pub command: u8,
    /// Frame payload (e.g. AX.25 frame for data commands).
    pub data: Vec<u8>,
}

impl KissFrame {
    /// Return the port as a validated [`KissPort`] newtype.
    ///
    /// Returns `None` if the stored port is outside the nibble range
    /// (should never happen for frames produced by [`decode_kiss_frame`]).
    #[must_use]
    pub const fn port_typed(&self) -> Option<KissPort> {
        KissPort::new(self.port)
    }

    /// Return the command as a typed [`KissCommand`] if recognised.
    #[must_use]
    pub const fn command_typed(&self) -> Option<KissCommand> {
        KissCommand::from_byte(self.command)
    }

    /// Build a new frame with typed parameters.
    ///
    /// Convenience constructor equivalent to setting `port` and
    /// `command` via their raw-byte equivalents.
    #[must_use]
    pub const fn with_typed(port: KissPort, command: KissCommand, data: Vec<u8>) -> Self {
        Self {
            port: port.get(),
            command: command.as_byte(),
            data,
        }
    }

    /// Build a TH-D75 data frame (`port = 0`, `command = Data`).
    #[must_use]
    pub const fn data(data: Vec<u8>) -> Self {
        Self::with_typed(KissPort::TH_D75, KissCommand::Data, data)
    }

    /// Build a KISS `Return` frame (exit command, `0xFF`).
    #[must_use]
    pub const fn return_command() -> Self {
        Self::with_typed(KissPort::TH_D75, KissCommand::Return, Vec::new())
    }
}

/// Encode a [`KissFrame`] into wire bytes with FEND delimiters and byte
/// stuffing, appending to an existing buffer.
///
/// Use this when you already have a scratch buffer you want to reuse
/// across many encodes. For one-shot use, [`encode_kiss_frame`] is
/// more convenient.
pub fn encode_kiss_frame_into(frame: &KissFrame, out: &mut Vec<u8>) {
    let type_byte = if frame.command == CMD_RETURN {
        CMD_RETURN
    } else {
        (frame.port << 4) | (frame.command & 0x0F)
    };
    out.reserve(2 + 1 + frame.data.len() * 2);
    out.push(FEND);
    out.push(type_byte);
    for &b in &frame.data {
        match b {
            FEND => {
                out.push(FESC);
                out.push(TFEND);
            }
            FESC => {
                out.push(FESC);
                out.push(TFESC);
            }
            _ => out.push(b),
        }
    }
    out.push(FEND);
}

/// Encode a [`KissFrame`] into wire bytes with FEND delimiters and byte stuffing.
///
/// The output format is: `FEND <type> <escaped-data> FEND`
#[must_use]
pub fn encode_kiss_frame(frame: &KissFrame) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + 1 + frame.data.len() * 2);
    encode_kiss_frame_into(frame, &mut out);
    out
}

/// Decode a KISS frame from raw wire bytes.
///
/// Expects the input to be a complete frame delimited by FEND bytes.
/// Performs byte de-stuffing of FESC sequences.
///
/// # Errors
///
/// Returns [`KissError`] if the frame is malformed.
pub fn decode_kiss_frame(data: &[u8]) -> Result<KissFrame, KissError> {
    if data.len() < 3 {
        return Err(KissError::FrameTooShort);
    }
    if data.first().copied() != Some(FEND) {
        return Err(KissError::MissingStartDelimiter);
    }
    if data.last().copied() != Some(FEND) {
        return Err(KissError::MissingEndDelimiter);
    }

    // Strip leading/trailing FEND, also skip any consecutive FENDs at start
    let inner = data.get(1..data.len().saturating_sub(1)).unwrap_or(&[][..]);

    // Skip any extra FEND bytes at the start (inter-frame fill)
    let inner = inner
        .iter()
        .position(|&b| b != FEND)
        .map_or(&[][..], |pos| inner.get(pos..).unwrap_or(&[][..]));

    let Some((&type_byte, payload_raw)) = inner.split_first() else {
        return Err(KissError::EmptyFrame);
    };

    // CMD_RETURN (0xFF) is a special full-byte command, not nibble-split.
    let (port, command) = if type_byte == CMD_RETURN {
        (0, CMD_RETURN)
    } else {
        (type_byte >> 4, type_byte & 0x0F)
    };

    // De-stuff the payload
    let mut payload = Vec::with_capacity(payload_raw.len());
    let mut iter = payload_raw.iter().copied();
    while let Some(b) = iter.next() {
        if b == FESC {
            match iter.next() {
                Some(TFEND) => payload.push(FEND),
                Some(TFESC) => payload.push(FESC),
                _ => return Err(KissError::InvalidEscapeSequence),
            }
        } else {
            payload.push(b);
        }
    }

    Ok(KissFrame {
        port,
        command,
        data: payload,
    })
}
