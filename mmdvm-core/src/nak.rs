// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! NAK response reason codes.
//!
//! A negative acknowledgement is `[0xE0, 5, 0x7F, command, reason]`.
//! Names follow `MMDVM/SerialPort.cpp` `processMessage` and
//! `MMDVM/DStarTX.cpp` `writeHeader`/`writeData`. Firmware state and command
//! context can share one reason byte.
//! See the [NAK example](crate#inspect-negative-acknowledgements).

use std::fmt;

/// NAK response reason code.
///
/// Unrecognized bytes retain their value. Code 5 identifies either a queue
/// capacity refusal or a state refusal; it alone does not identify retry
/// policy.
/// See the [NAK example](crate#inspect-negative-acknowledgements).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NakReason {
    /// The command byte is not recognised.
    InvalidCommand,
    /// The requested mode is disabled or the active state refuses the command.
    CommandUnavailable,
    /// The command payload contains incorrect data.
    DataIncorrect,
    /// A TX queue lacks space or the current state refuses the command.
    QueueOrStateRejected,
    /// Unknown NAK reason byte; the raw code is preserved.
    Unknown {
        /// Raw byte as received from the modem.
        code: u8,
    },
}

impl NakReason {
    /// Parse a raw reason byte. Never fails: unknown codes are
    /// captured as [`NakReason::Unknown`].
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::InvalidCommand,
            2 => Self::CommandUnavailable,
            4 => Self::DataIncorrect,
            5 => Self::QueueOrStateRejected,
            _ => Self::Unknown { code: b },
        }
    }

    /// Return the preserved reason byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::InvalidCommand => 1,
            Self::CommandUnavailable => 2,
            Self::DataIncorrect => 4,
            Self::QueueOrStateRejected => 5,
            Self::Unknown { code } => code,
        }
    }
}

impl From<u8> for NakReason {
    fn from(byte: u8) -> Self {
        Self::from_byte(byte)
    }
}

impl From<NakReason> for u8 {
    fn from(reason: NakReason) -> Self {
        reason.as_byte()
    }
}

impl fmt::Display for NakReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand => formatter.write_str("invalid command"),
            Self::CommandUnavailable => {
                formatter.write_str("command unavailable in the active mode")
            }
            Self::DataIncorrect => formatter.write_str("incorrect command data"),
            Self::QueueOrStateRejected => {
                formatter.write_str("TX queue or modem state refused the command")
            }
            Self::Unknown { code } => write!(formatter, "unknown reason 0x{code:02X}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_reasons() {
        assert_eq!(NakReason::from_byte(1), NakReason::InvalidCommand);
        assert_eq!(NakReason::from_byte(2), NakReason::CommandUnavailable);
        assert_eq!(NakReason::from_byte(3), NakReason::Unknown { code: 3 });
        assert_eq!(NakReason::from_byte(4), NakReason::DataIncorrect);
        assert_eq!(NakReason::from_byte(5), NakReason::QueueOrStateRejected);
    }

    #[test]
    fn unknown_preserves_code() {
        assert_eq!(NakReason::from_byte(0), NakReason::Unknown { code: 0 });
        assert_eq!(
            NakReason::from_byte(0xFF),
            NakReason::Unknown { code: 0xFF }
        );
        assert_eq!(NakReason::from_byte(6), NakReason::Unknown { code: 6 });
    }
}
