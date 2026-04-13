// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! NAK response reason codes.
//!
//! The MMDVM firmware rejects a command with `[0xE0, len, 0x7F, cmd,
//! reason]`. The reason codes below are approximated from
//! `ref/MMDVMHost/Modem.cpp:970-971` and the historical MMDVM
//! specification — unknown codes are preserved in [`NakReason::Unknown`]
//! rather than turning into a hard error, so callers can log and
//! continue.

/// NAK response reason code.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NakReason {
    /// The command byte is not recognised.
    InvalidCommand,
    /// The modem is in the wrong mode for this command.
    WrongMode,
    /// The command frame exceeds the maximum length.
    CommandTooLong,
    /// The command payload contains incorrect data.
    DataIncorrect,
    /// The TX buffer is full.
    BufferFull,
    /// Unknown NAK reason byte — the raw code is preserved.
    Unknown {
        /// Raw byte as received from the modem.
        code: u8,
    },
}

impl NakReason {
    /// Parse a raw reason byte. Never fails — unknown codes are
    /// captured as [`NakReason::Unknown`].
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::InvalidCommand,
            2 => Self::WrongMode,
            3 => Self::CommandTooLong,
            4 => Self::DataIncorrect,
            5 => Self::BufferFull,
            _ => Self::Unknown { code: b },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_reasons() {
        assert_eq!(NakReason::from_byte(1), NakReason::InvalidCommand);
        assert_eq!(NakReason::from_byte(2), NakReason::WrongMode);
        assert_eq!(NakReason::from_byte(3), NakReason::CommandTooLong);
        assert_eq!(NakReason::from_byte(4), NakReason::DataIncorrect);
        assert_eq!(NakReason::from_byte(5), NakReason::BufferFull);
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
