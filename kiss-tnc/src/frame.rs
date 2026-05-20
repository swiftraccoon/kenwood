//! KISS frame type and one-shot encode/decode helpers.

use alloc::vec::Vec;

use crate::command::{CMD_RETURN, FEND, FESC, KissCommand, KissPort, TFEND, TFESC};
use crate::error::KissError;

/// A decoded KISS frame.
///
/// The wire "type indicator" byte splits into a [`KissPort`] (high
/// nibble) and a [`KissCommand`] (low nibble); the whole-byte value
/// `0xFF` is the [`KissCommand::Return`] command. Both fields are
/// validated types, so a `KissFrame` can never hold an out-of-range
/// port or a command the encoder cannot represent on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KissFrame {
    /// TNC port — the high nibble of the type byte. `0` for the TH-D75.
    pub port: KissPort,
    /// KISS command — the low nibble of the type byte, or the
    /// whole-byte [`KissCommand::Return`].
    pub command: KissCommand,
    /// Frame payload (e.g. an AX.25 frame for [`KissCommand::Data`]).
    ///
    /// Not represented on the wire when `command` is
    /// [`KissCommand::Return`], which is a whole-byte control command.
    pub data: Vec<u8>,
}

impl KissFrame {
    /// Build a TH-D75 data frame (`port = 0`, `command = Data`).
    #[must_use]
    pub const fn data(data: Vec<u8>) -> Self {
        Self {
            port: KissPort::TH_D75,
            command: KissCommand::Data,
            data,
        }
    }

    /// Build a KISS `Return` frame — the exit-KISS-mode command (`0xFF`).
    #[must_use]
    pub const fn return_command() -> Self {
        Self {
            port: KissPort::TH_D75,
            command: KissCommand::Return,
            data: Vec::new(),
        }
    }
}

/// Append `byte` to `out`, byte-stuffing the two KISS special values:
/// `FEND` is emitted as `FESC TFEND` and `FESC` as `FESC TFESC`.
fn push_stuffed(out: &mut Vec<u8>, byte: u8) {
    match byte {
        FEND => {
            out.push(FESC);
            out.push(TFEND);
        }
        FESC => {
            out.push(FESC);
            out.push(TFESC);
        }
        _ => out.push(byte),
    }
}

/// Encode a [`KissFrame`] into wire bytes with FEND delimiters and byte
/// stuffing, appending to an existing buffer.
///
/// Prefer this when reusing a scratch buffer across many encodes; for
/// one-shot use, [`encode_kiss_frame`] is more convenient.
///
/// A [`KissCommand::Return`] frame is emitted as the bare three-byte
/// sequence `FEND 0xFF FEND`: Return is a whole-byte control command, so
/// the frame's `port` and `data` fields are not placed on the wire.
pub fn encode_kiss_frame_into(frame: &KissFrame, out: &mut Vec<u8>) {
    if frame.command.is_return() {
        out.reserve(3);
        out.push(FEND);
        out.push(CMD_RETURN);
        out.push(FEND);
        return;
    }
    // High nibble = port, low nibble = command. A non-Return command's
    // wire byte is always `0x00..=0x06`, so the two nibbles never overlap.
    let type_byte = (frame.port.get() << 4) | frame.command.as_byte();
    // 2 FEND delimiters + worst-case 2x for the type byte and every
    // payload byte (each may expand to a two-byte escape sequence).
    out.reserve(2 + (1 + frame.data.len()) * 2);
    out.push(FEND);
    // The type byte is byte-stuffed along with the payload: a type byte
    // that equals FEND or FESC (e.g. port 12 + command Data => 0xC0)
    // must be escaped, never emitted raw, or the frame is unparseable.
    push_stuffed(out, type_byte);
    for &b in &frame.data {
        push_stuffed(out, b);
    }
    out.push(FEND);
}

/// Encode a [`KissFrame`] into wire bytes with FEND delimiters and byte
/// stuffing.
///
/// The output format is `FEND <type> <escaped-data> FEND`, or the bare
/// `FEND 0xFF FEND` for a [`KissCommand::Return`] frame.
#[must_use]
pub fn encode_kiss_frame(frame: &KissFrame) -> Vec<u8> {
    let mut out = Vec::new();
    encode_kiss_frame_into(frame, &mut out);
    out
}

/// Decode a single KISS frame from raw wire bytes.
///
/// Expects one complete frame delimited by FEND bytes; leading
/// inter-frame FEND fill is tolerated. Performs byte de-stuffing of FESC
/// sequences.
///
/// # Errors
///
/// Returns a [`KissError`] if the frame is malformed: too short, missing
/// a start or end delimiter, empty, carrying an unrecognised command
/// nibble, holding an invalid or truncated escape sequence, or
/// containing an unescaped FEND inside the body.
pub fn decode_kiss_frame(data: &[u8]) -> Result<KissFrame, KissError> {
    // The shortest possible frame is `FEND <type> FEND`; fewer than two
    // bytes cannot hold even the start and end delimiters.
    if data.len() < 2 {
        return Err(KissError::FrameTooShort);
    }
    if data.first().copied() != Some(FEND) {
        return Err(KissError::MissingStartDelimiter);
    }
    if data.last().copied() != Some(FEND) {
        return Err(KissError::MissingEndDelimiter);
    }

    // Strip the leading and trailing FEND delimiter.
    let inner = data.get(1..data.len().saturating_sub(1)).unwrap_or(&[][..]);
    // Skip any extra leading FEND bytes (inter-frame fill).
    let inner = inner
        .iter()
        .position(|&b| b != FEND)
        .map_or(&[][..], |pos| inner.get(pos..).unwrap_or(&[][..]));

    // De-stuff the whole frame body. The type byte is byte-stuffed
    // together with the payload, so a type byte equal to FEND/FESC
    // arrives as a two-byte escape sequence and must be de-stuffed too.
    let mut body = Vec::with_capacity(inner.len());
    let mut iter = inner.iter().copied();
    while let Some(b) = iter.next() {
        match b {
            FESC => match iter.next() {
                Some(TFEND) => body.push(FEND),
                Some(TFESC) => body.push(FESC),
                Some(_) => return Err(KissError::InvalidEscapeSequence),
                None => return Err(KissError::TruncatedEscapeSequence),
            },
            FEND => return Err(KissError::UnexpectedFrameDelimiter),
            _ => body.push(b),
        }
    }

    // The first de-stuffed byte is the type indicator; the rest is payload.
    let Some(&type_byte) = body.first() else {
        return Err(KissError::EmptyFrame);
    };
    let (port, command) = if type_byte == CMD_RETURN {
        (KissPort::TH_D75, KissCommand::Return)
    } else {
        let nibble = type_byte & 0x0F;
        let Some(command) = KissCommand::from_byte(nibble) else {
            return Err(KissError::UnknownCommand(nibble));
        };
        (KissPort::from_type_byte(type_byte), command)
    };
    // Drop the now-decoded type byte; the remainder of `body` is the payload.
    drop(body.drain(..1));

    Ok(KissFrame {
        port,
        command,
        data: body,
    })
}
