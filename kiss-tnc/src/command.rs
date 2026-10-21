//! KISS wire constants and typed command/port representations.

use crate::error::KissError;

// ---------------------------------------------------------------------------
// KISS constants
// ---------------------------------------------------------------------------

/// Frame End marker. Delimits KISS frames.
pub const FEND: u8 = 0xC0;

/// Frame Escape. Signals that the next byte is a transposed special character.
pub const FESC: u8 = 0xDB;

/// Transposed Frame End. Represents `FEND` inside a frame body.
pub const TFEND: u8 = 0xDC;

/// Transposed Frame Escape. Represents `FESC` inside a frame body.
pub const TFESC: u8 = 0xDD;

// ---------------------------------------------------------------------------
// KISS command types (type indicator byte, high nibble = port, low = cmd)
// ---------------------------------------------------------------------------

/// Data frame command. Payload is an AX.25 frame.
pub const CMD_DATA: u8 = 0x00;

/// Set TX delay, in units of 10 ms.
pub const CMD_TX_DELAY: u8 = 0x01;

/// Set persistence parameter for CSMA. Range: 0-255.
pub const CMD_PERSISTENCE: u8 = 0x02;

/// Set the CSMA slot interval, in units of 10 ms, using one payload byte.
pub const CMD_SLOT_TIME: u8 = 0x03;

/// Set TX tail time (units of 10 ms). Range: 0-255.
pub const CMD_TX_TAIL: u8 = 0x04;

/// Set full/half duplex. 0 = half duplex, nonzero = full duplex.
pub const CMD_FULL_DUPLEX: u8 = 0x05;

/// Set a TNC-defined hardware parameter. The KISS spec leaves the
/// payload semantics entirely up to the TNC.
pub const CMD_SET_HARDWARE: u8 = 0x06;

/// Exit KISS mode and return to command/normal mode.
pub const CMD_RETURN: u8 = 0xFF;

/// Typed KISS command enum for host → TNC messages.
///
/// Per the KISS spec (Chepponis & Karn 1987), the low nibble of a KISS
/// frame's type byte identifies the command, and `0xFF` is a special
/// full-byte command meaning "return to command/normal mode." This enum
/// lets the library classify frames at parse time and construct frames
/// without reaching for raw byte constants.
///
/// The port's high nibble is represented separately by [`KissPort`].
/// See the [command conversion example](crate#ports-and-commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KissCommand {
    /// `0x00`: data frame (payload is an AX.25 frame).
    Data,
    /// `0x01`: TX delay in 10 ms units.
    TxDelay,
    /// `0x02`: CSMA persistence (0-255).
    Persistence,
    /// `0x03`: CSMA slot interval in 10 ms units, encoded in one byte.
    SlotTime,
    /// `0x04`: TX tail time in 10 ms units (0-255).
    TxTail,
    /// `0x05`: full duplex (0 = half, nonzero = full).
    FullDuplex,
    /// `0x06`: `SetHardware`, a TNC-defined hardware parameter.
    SetHardware,
    /// `0xFF`: full-byte return command (exit KISS mode).
    Return,
}

impl KissCommand {
    /// Convert a raw low-nibble or full-byte value to a [`KissCommand`].
    ///
    /// Returns `None` for unrecognised values.
    /// See the [command conversion example](crate#ports-and-commands).
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            CMD_DATA => Some(Self::Data),
            CMD_TX_DELAY => Some(Self::TxDelay),
            CMD_PERSISTENCE => Some(Self::Persistence),
            CMD_SLOT_TIME => Some(Self::SlotTime),
            CMD_TX_TAIL => Some(Self::TxTail),
            CMD_FULL_DUPLEX => Some(Self::FullDuplex),
            CMD_SET_HARDWARE => Some(Self::SetHardware),
            CMD_RETURN => Some(Self::Return),
            _ => None,
        }
    }

    /// Convert back to the wire byte value (low-nibble of the type byte,
    /// or `0xFF` for `Return`).
    /// See the [command conversion example](crate#ports-and-commands).
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Data => CMD_DATA,
            Self::TxDelay => CMD_TX_DELAY,
            Self::Persistence => CMD_PERSISTENCE,
            Self::SlotTime => CMD_SLOT_TIME,
            Self::TxTail => CMD_TX_TAIL,
            Self::FullDuplex => CMD_FULL_DUPLEX,
            Self::SetHardware => CMD_SET_HARDWARE,
            Self::Return => CMD_RETURN,
        }
    }

    /// Returns whether this is the whole-byte [`Self::Return`] command.
    /// See the [return command example](crate#return-command).
    #[must_use]
    pub const fn is_return(self) -> bool {
        matches!(self, Self::Return)
    }
}

impl From<KissCommand> for u8 {
    fn from(command: KissCommand) -> Self {
        command.as_byte()
    }
}

impl TryFrom<u8> for KissCommand {
    type Error = KissError;

    /// Classifies an exact command byte, including the whole-byte `0xFF`.
    ///
    /// # Errors
    ///
    /// Returns [`KissError::UnknownCommand`] with the input byte when it is
    /// neither an assigned command code in `0x00..=0x06` nor `0xFF`.
    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::from_byte(byte).ok_or(KissError::UnknownCommand(byte))
    }
}

/// A KISS TNC port number, validated to `0..=15` (a 4-bit nibble).
///
/// Per the KISS spec, the high nibble of the type byte addresses one of
/// up to 16 TNC ports. [`Self::ZERO`] is the default port.
/// See the [port conversion example](crate#ports-and-commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[doc(alias = "tnc-port")]
#[doc(alias = "kiss-port")]
pub struct KissPort(u8);

impl KissPort {
    /// Port zero, the default port.
    pub const ZERO: Self = Self(0);

    /// The maximum valid port; the type byte's port nibble holds 4 bits.
    pub const MAX: Self = Self(15);

    /// Create a port from a raw `u8`, validating the `0..=15` range.
    ///
    /// Returns `None` for values outside the nibble range.
    /// See the [port conversion example](crate#ports-and-commands).
    #[must_use]
    pub const fn new(n: u8) -> Option<Self> {
        if n <= Self::MAX.get() {
            Some(Self(n))
        } else {
            None
        }
    }

    /// Extract the port from a KISS type byte (its high nibble).
    ///
    /// Always succeeds: a 4-bit nibble is intrinsically within `0..=15`.
    /// Callers classify the whole-byte return command before extracting a
    /// frame's port. See the [port conversion example](crate#ports-and-commands).
    #[must_use]
    pub const fn from_type_byte(type_byte: u8) -> Self {
        Self(type_byte >> 4)
    }

    /// Return the raw port value.
    /// See the [port conversion example](crate#ports-and-commands).
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<KissPort> for u8 {
    fn from(port: KissPort) -> Self {
        port.get()
    }
}

impl TryFrom<u8> for KissPort {
    type Error = KissError;

    /// Validates a port byte against the `0..=15` address range.
    ///
    /// # Errors
    ///
    /// Returns [`KissError::InvalidPort`] with the input byte when it exceeds
    /// [`Self::MAX`].
    fn try_from(byte: u8) -> Result<Self, Self::Error> {
        Self::new(byte).ok_or(KissError::InvalidPort(byte))
    }
}

impl Default for KissPort {
    fn default() -> Self {
        Self::ZERO
    }
}
