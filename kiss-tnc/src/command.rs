//! KISS wire constants and typed command/port representations.

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

/// Set TX delay (units of 10 ms). TH-D75 range: 0-120 (0-1200 ms).
pub const CMD_TX_DELAY: u8 = 0x01;

/// Set persistence parameter for CSMA. Range: 0-255.
pub const CMD_PERSISTENCE: u8 = 0x02;

/// Set slot time (units of 10 ms) for CSMA. Range: 0-250.
pub const CMD_SLOT_TIME: u8 = 0x03;

/// Set TX tail time (units of 10 ms). Range: 0-255.
pub const CMD_TX_TAIL: u8 = 0x04;

/// Set full/half duplex. 0 = half duplex, nonzero = full duplex.
pub const CMD_FULL_DUPLEX: u8 = 0x05;

/// Set hardware-specific parameter. TH-D75 uses this for baud rate switching:
/// 0 or 0x23 (35 decimal) = 1200 bps, 0x05 or 0x26 (38 decimal) = 9600 bps.
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
/// Note the TH-D75 is always port 0 on the wire; the high nibble is
/// tracked separately via [`KissPort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KissCommand {
    /// `0x00` — data frame (payload is an AX.25 frame).
    Data,
    /// `0x01` — TX delay in 10 ms units (0-120 on TH-D75).
    TxDelay,
    /// `0x02` — CSMA persistence (0-255).
    Persistence,
    /// `0x03` — CSMA slot time in 10 ms units (0-250).
    SlotTime,
    /// `0x04` — TX tail time in 10 ms units (0-255).
    TxTail,
    /// `0x05` — full duplex (0 = half, nonzero = full).
    FullDuplex,
    /// `0x06` — `SetHardware` (TH-D75 uses this for baud switching).
    SetHardware,
    /// `0xFF` — full-byte return command (exit KISS mode).
    Return,
}

impl KissCommand {
    /// Convert a raw low-nibble or full-byte value to a [`KissCommand`].
    ///
    /// Returns `None` for unrecognised values.
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

    /// `true` if this is the special full-byte `Return` command which
    /// does NOT participate in the port/command nibble encoding.
    #[must_use]
    pub const fn is_return(self) -> bool {
        matches!(self, Self::Return)
    }
}

/// A KISS TNC port number, validated to `0..=15` (the low nibble range).
///
/// Per the KISS spec, the high nibble of the type byte addresses one of
/// up to 16 TNC ports. The TH-D75 is always port 0; [`KissPort::TH_D75`]
/// is a convenience constant for that common case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KissPort(u8);

impl KissPort {
    /// The TH-D75 always uses port 0.
    pub const TH_D75: Self = Self(0);

    /// Maximum valid port (the nibble only holds 4 bits).
    pub const MAX: u8 = 15;

    /// Create a port from a raw `u8`, validating `0..=15`.
    ///
    /// Returns `None` for values outside the nibble range.
    #[must_use]
    pub const fn new(n: u8) -> Option<Self> {
        if n <= Self::MAX { Some(Self(n)) } else { None }
    }

    /// Return the raw port value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for KissPort {
    fn default() -> Self {
        Self::TH_D75
    }
}
