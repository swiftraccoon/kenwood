//! KISS TNC (Terminal Node Controller) protocol for AX.25 packet operations.
//!
//! The TH-D75's built-in TNC supports KISS framing for sending and
//! receiving APRS packets. When KISS mode is enabled (via `[F], [LIST]`
//! on the radio, cycling to "KISS mode ON"), the radio accepts and
//! produces standard KISS frames over USB or Bluetooth.
//!
//! # Activation (per Operating Tips §2.7)
//!
//! KISS mode is activated from the radio front panel: press `[F]`, then
//! `[LIST]`, then cycle to "KISS mode ON". When active, the display shows
//! "KISS12" (1200 bps) or "KISS96" (9600 bps) depending on the data speed
//! setting.
//!
//! From a PC application (e.g. UI-View32), KISS mode is entered by sending
//! the `TN 2,0` command. The radio then accepts standard KISS frames.
//!
//! # Menu interaction while in KISS mode
//!
//! While KISS mode is active, **all APRS menu configurations are inactive**
//! except Menu No. 505 (Data Speed) and Menu No. 506 (Data Band). The radio
//! ignores changes to other APRS settings until KISS mode is exited.
//!
//! # TH-D75 KISS TNC specifications (per Operating Tips §2.7.2, User Manual Chapter 15)
//!
//! - TX buffer: 4 KB, RX buffer: 4 KB (per User Manual Chapter 15)
//! - Speeds: 1200 bps (AFSK) and 9600 bps (GMSK)
//! - The built-in TNC does NOT support Command mode or Converse mode;
//!   it enters KISS mode directly.
//! - KISS commands: Data Frame (`0x00`), TXDELAY (`0x01`, 0-120, units
//!   of 10 ms, default from Menu No. 508), Persistence (`0x02`, 0-255,
//!   default 128), `SlotTime` (`0x03`, 0-250, units of 10 ms, default 10),
//!   `TXtail` (`0x04`, 0-255, default 3), `FullDuplex` (`0x05`, 0=half/
//!   default, nonzero=full), `SetHardware` (`0x06`, 0 or 0x23=1200 bps,
//!   0x05 or 0x26=9600 bps, default from Menu No. 505), Return (`0xFF`)
//! - The data band frequency defaults to Band A; changeable via Menu No. 506.
//! - USB or Bluetooth interface is selectable via Menu No. 983.
//! - Transfer rates: USB up to 12 Mbps, Bluetooth up to 128 kbps.
//! - To exit KISS mode: send KISS command `C0,FF,C0` (192,255,192).
//!   To re-enter KISS mode from PC: send CAT command `TN 2,0` (Band A)
//!   or `TN 2,1` (Band B).
//! - Display indicators: `KISS` (KISS mode active), `12` (1200 bps),
//!   `96` (9600 bps), `STA` (TX packets remaining in buffer).
//!
//! This module provides:
//! - KISS frame encoding and decoding with proper byte stuffing
//! - AX.25 UI frame parsing and construction
//! - Basic APRS position report parsing
//!
//! # References
//!
//! - KISS protocol: <http://www.ka9q.net/papers/kiss.html>
//! - AX.25 v2.2: <http://www.ax25.net/AX25.2.2-Jul%2098-2.pdf>
//! - APRS spec: <http://www.aprs.org/doc/APRS101.PDF>
//! - TH-D75 User Manual, Chapter 15: Built-In KISS TNC

pub mod aprs_client;
pub mod aprs_is;
pub mod aprs_is_client;
pub mod aprs_messaging;
pub mod digipeater;
pub mod smart_beaconing;
pub mod station_list;

use std::fmt;

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

// ---------------------------------------------------------------------------
// KISS errors
// ---------------------------------------------------------------------------

/// Errors that can occur during KISS frame processing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KissError {
    /// Frame is too short to contain a valid KISS header.
    #[error("KISS frame too short")]
    FrameTooShort,
    /// Frame does not start with FEND.
    #[error("KISS frame missing start FEND")]
    MissingStartDelimiter,
    /// Frame does not end with FEND.
    #[error("KISS frame missing end FEND")]
    MissingEndDelimiter,
    /// Invalid escape sequence (FESC not followed by TFEND or TFESC).
    #[error("invalid KISS escape sequence")]
    InvalidEscapeSequence,
    /// Frame body is empty (no type indicator byte).
    #[error("empty KISS frame (no type byte)")]
    EmptyFrame,
}

// ---------------------------------------------------------------------------
// KISS frame
// ---------------------------------------------------------------------------

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

/// Encode a [`KissFrame`] into wire bytes and write to an
/// [`std::io::Write`] sink.
///
/// # Errors
///
/// Propagates any I/O error from the sink.
pub fn encode_kiss_frame_to_writer<W: std::io::Write>(
    frame: &KissFrame,
    writer: &mut W,
) -> std::io::Result<()> {
    let mut buf = Vec::new();
    encode_kiss_frame_into(frame, &mut buf);
    writer.write_all(&buf)
}

/// Incremental KISS stream decoder.
///
/// Most of the library works on whole KISS frames (via
/// [`decode_kiss_frame`]), but a stream transport like a serial port
/// delivers bytes in arbitrary chunks. `KissDecoder` buffers partial
/// frames and emits complete ones as they become available.
///
/// Usage:
///
/// ```
/// use kenwood_thd75::kiss::{KissDecoder, FEND, CMD_DATA};
/// let mut decoder = KissDecoder::new();
/// // Two data frames back-to-back:
/// let bytes = &[FEND, 0x00, 0x01, FEND, FEND, 0x00, 0x02, FEND];
/// decoder.push(bytes);
/// let first = decoder.next_frame().unwrap().unwrap();
/// let second = decoder.next_frame().unwrap().unwrap();
/// assert_eq!(first.command, CMD_DATA);
/// assert_eq!(second.command, CMD_DATA);
/// assert!(decoder.next_frame().unwrap().is_none());
/// ```
#[derive(Debug, Default)]
pub struct KissDecoder {
    /// Accumulated bytes since the last complete frame.
    buffer: Vec<u8>,
    /// `true` once we've seen a leading FEND and are inside a frame.
    in_frame: bool,
}

impl KissDecoder {
    /// Create a new empty decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            in_frame: false,
        }
    }

    /// Feed bytes from the transport into the decoder.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// Try to extract the next complete frame from the buffer.
    ///
    /// Returns `Ok(None)` if the buffer does not yet contain a full
    /// frame, `Ok(Some(frame))` when one is available, and `Err(...)`
    /// on malformed input.
    ///
    /// # Errors
    ///
    /// Returns [`KissError`] on invalid escape sequences or other
    /// frame-level errors.
    pub fn next_frame(&mut self) -> Result<Option<KissFrame>, KissError> {
        loop {
            // Find the first FEND.
            let Some(first) = self.buffer.iter().position(|&b| b == FEND) else {
                // No FEND at all — nothing to decode yet.
                return Ok(None);
            };
            if !self.in_frame {
                // Discard any pre-FEND garbage and start a new frame.
                drop(self.buffer.drain(..first));
                self.in_frame = true;
            }
            // We're now positioned with a leading FEND at buffer[0].
            // Look for the next FEND to close the frame.
            let Some(end) = self.buffer[1..].iter().position(|&b| b == FEND) else {
                return Ok(None);
            };
            let end_idx = end + 1;
            // Empty frame (`FEND FEND`)? Skip and stay in-frame.
            if end_idx == 1 {
                drop(self.buffer.drain(..1));
                continue;
            }
            // Slice the complete frame including both FENDs.
            let frame_bytes = self.buffer[..=end_idx].to_vec();
            drop(self.buffer.drain(..=end_idx));
            self.in_frame = false;
            return decode_kiss_frame(&frame_bytes).map(Some);
        }
    }
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
    if data[0] != FEND {
        return Err(KissError::MissingStartDelimiter);
    }
    if data[data.len() - 1] != FEND {
        return Err(KissError::MissingEndDelimiter);
    }

    // Strip leading/trailing FEND, also skip any consecutive FENDs at start
    let inner = &data[1..data.len() - 1];

    // Skip any extra FEND bytes at the start (inter-frame fill)
    let inner = inner
        .iter()
        .position(|&b| b != FEND)
        .map_or(&[][..], |pos| &inner[pos..]);

    if inner.is_empty() {
        return Err(KissError::EmptyFrame);
    }

    let type_byte = inner[0];
    // CMD_RETURN (0xFF) is a special full-byte command, not nibble-split.
    let (port, command) = if type_byte == CMD_RETURN {
        (0, CMD_RETURN)
    } else {
        (type_byte >> 4, type_byte & 0x0F)
    };

    // De-stuff the payload
    let payload_raw = &inner[1..];
    let mut payload = Vec::with_capacity(payload_raw.len());
    let mut i = 0;
    while i < payload_raw.len() {
        if payload_raw[i] == FESC {
            i += 1;
            if i >= payload_raw.len() {
                return Err(KissError::InvalidEscapeSequence);
            }
            match payload_raw[i] {
                TFEND => payload.push(FEND),
                TFESC => payload.push(FESC),
                _ => return Err(KissError::InvalidEscapeSequence),
            }
        } else {
            payload.push(payload_raw[i]);
        }
        i += 1;
    }

    Ok(KissFrame {
        port,
        command,
        data: payload,
    })
}

// ---------------------------------------------------------------------------
// AX.25 types and errors
// ---------------------------------------------------------------------------

/// Errors that can occur during AX.25 packet parsing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Ax25Error {
    /// Packet is too short to contain required AX.25 fields.
    #[error("AX.25 packet too short")]
    PacketTooShort,
    /// Address field has invalid length (must be multiple of 7).
    #[error("AX.25 address field has invalid length (not a multiple of 7)")]
    InvalidAddressLength,
    /// Control/protocol fields are missing after the address block.
    #[error("AX.25 missing control/protocol fields after address block")]
    MissingControlFields,
    /// Packet carries more than 8 digipeater addresses (the AX.25 v2.2
    /// §2.2.13 maximum).
    #[error("AX.25 packet has more than 8 digipeater addresses (AX.25 v2.2 §2.2.13 max)")]
    TooManyDigipeaters,
    /// A callsign byte decoded to something other than an ASCII
    /// alphanumeric character or space padding.
    #[error("AX.25 callsign byte decoded to non-alphanumeric character {0:#04x}")]
    InvalidCallsignByte(u8),
    /// The protocol identifier (PID) byte is unknown or unsupported.
    #[error("AX.25 unknown PID byte {0:#04x}")]
    UnknownPid(u8),
}

/// Maximum number of digipeater addresses in an AX.25 v2.2 frame (§2.2.13).
pub const MAX_DIGIPEATERS: usize = 8;

/// Compute the AX.25 Frame Check Sequence (CRC-16-CCITT, polynomial
/// `0x1021`, initial value `0xFFFF`, reflected, `xorout = 0xFFFF`) over
/// a byte slice.
///
/// KISS frames do not carry the FCS — the TNC computes and strips it —
/// but this function is provided for callers working with raw AX.25
/// over a transport that does expect the FCS (e.g. a software modem,
/// SDR, or packet capture tool). The byte order on the wire is
/// little-endian: emit `(crc & 0xFF)` then `(crc >> 8)`.
#[must_use]
pub fn ax25_fcs(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in bytes {
        let mut b = b;
        for _ in 0..8 {
            let carry = (crc & 0x0001) != (u16::from(b) & 0x0001);
            crc >>= 1;
            b >>= 1;
            if carry {
                crc ^= 0x8408;
            }
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// AX.25 control + PID enums
// ---------------------------------------------------------------------------

/// AX.25 protocol identifier (PID) byte.
///
/// Per AX.25 v2.2 §2.2.4 Table 2. Only a small subset is observed on APRS
/// (`0xF0` = no layer 3) but the full enum lets the library parse and
/// build any AX.25 frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ax25Pid {
    /// ISO 8208 / X.25 PLP (layer 3).
    Iso8208,
    /// Compressed TCP/IP packet (Van Jacobson, RFC 1144).
    CompressedTcpIp,
    /// Uncompressed TCP/IP packet (Van Jacobson, RFC 1144).
    UncompressedTcpIp,
    /// Segmentation fragment (AX.25 §4.3.2.10).
    SegmentationFragment,
    /// TEXNET datagram protocol.
    TexNet,
    /// Link Quality Protocol.
    LinkQuality,
    /// Appletalk.
    Appletalk,
    /// Appletalk ARP.
    AppletalkArp,
    /// Internet protocol (RFC 791).
    Ip,
    /// Address Resolution Protocol.
    Arp,
    /// `FlexNet`.
    FlexNet,
    /// `NET/ROM` protocol.
    NetRom,
    /// No layer-3 protocol (the APRS case, `0xF0`).
    NoLayer3,
    /// Escape character: next byte defines the protocol.
    Escape,
    /// Any other raw byte the library does not classify.
    Other(u8),
}

impl Ax25Pid {
    /// Parse a single PID byte into an enum value.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            0x01 => Self::Iso8208,
            0x06 => Self::CompressedTcpIp,
            0x07 => Self::UncompressedTcpIp,
            0x08 => Self::SegmentationFragment,
            0xC3 => Self::TexNet,
            0xC4 => Self::LinkQuality,
            0xCA => Self::Appletalk,
            0xCB => Self::AppletalkArp,
            0xCC => Self::Ip,
            0xCD => Self::Arp,
            0xCE => Self::FlexNet,
            0xCF => Self::NetRom,
            0xF0 => Self::NoLayer3,
            0xFF => Self::Escape,
            other => Self::Other(other),
        }
    }

    /// Convert back to the raw PID byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Iso8208 => 0x01,
            Self::CompressedTcpIp => 0x06,
            Self::UncompressedTcpIp => 0x07,
            Self::SegmentationFragment => 0x08,
            Self::TexNet => 0xC3,
            Self::LinkQuality => 0xC4,
            Self::Appletalk => 0xCA,
            Self::AppletalkArp => 0xCB,
            Self::Ip => 0xCC,
            Self::Arp => 0xCD,
            Self::FlexNet => 0xCE,
            Self::NetRom => 0xCF,
            Self::NoLayer3 => 0xF0,
            Self::Escape => 0xFF,
            Self::Other(b) => b,
        }
    }
}

/// AX.25 control-field frame-type family.
///
/// Per AX.25 v2.2 §4.2, the control byte identifies one of three frame
/// families:
/// - **Information (I)** — numbered data transfer frames
/// - **Supervisory (S)** — flow-control frames (RR, RNR, REJ, SREJ)
/// - **Unnumbered (U)** — link-setup, disconnection, and **UI** frames
///   used by APRS
///
/// The APRS protocol uses the `UI` subtype with control byte `0x03`.
/// Only UI is commonly seen in practice, but we parse the full family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ax25Control {
    /// Information frame (I).
    Information {
        /// Numbered send sequence (N(S)).
        ns: u8,
        /// Numbered receive sequence (N(R)).
        nr: u8,
        /// Poll/final bit.
        pf: bool,
    },
    /// Supervisory frame (S) with sub-kind.
    Supervisory {
        /// Supervisory sub-kind (RR / RNR / REJ / SREJ).
        kind: SupervisoryKind,
        /// Numbered receive sequence (N(R)).
        nr: u8,
        /// Poll/final bit.
        pf: bool,
    },
    /// Unnumbered frame (U) with sub-kind.
    Unnumbered {
        /// Unnumbered sub-kind (UI / SABM / DISC / DM / UA / FRMR / XID / TEST).
        kind: UnnumberedKind,
        /// Poll/final bit.
        pf: bool,
    },
}

impl Ax25Control {
    /// Parse a single control byte into an [`Ax25Control`] value.
    ///
    /// This covers modulo-8 control bytes. Modulo-128 extended control
    /// (2-byte) is not yet supported.
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        // Bit 0 = 0 → Information frame
        if b & 0x01 == 0 {
            return Self::Information {
                ns: (b >> 1) & 0x07,
                nr: (b >> 5) & 0x07,
                pf: (b & 0x10) != 0,
            };
        }
        // Bits 0-1 = 01 → Supervisory frame
        if b & 0x03 == 0x01 {
            let kind = match (b >> 2) & 0x03 {
                0 => SupervisoryKind::ReceiveReady,
                1 => SupervisoryKind::ReceiveNotReady,
                2 => SupervisoryKind::Reject,
                _ => SupervisoryKind::SelectiveReject,
            };
            return Self::Supervisory {
                kind,
                nr: (b >> 5) & 0x07,
                pf: (b & 0x10) != 0,
            };
        }
        // Otherwise Unnumbered (bits 0-1 = 11)
        let pf = (b & 0x10) != 0;
        let kind_bits = b & 0xEF; // mask off P/F bit
        let kind = match kind_bits {
            0x03 => UnnumberedKind::UnnumberedInformation,
            0x2F => UnnumberedKind::SetAsyncBalancedMode,
            0x43 => UnnumberedKind::Disconnect,
            0x0F => UnnumberedKind::DisconnectedMode,
            0x63 => UnnumberedKind::UnnumberedAcknowledge,
            0x87 => UnnumberedKind::FrameReject,
            0xAF => UnnumberedKind::ExchangeIdentification,
            0xE3 => UnnumberedKind::Test,
            other => UnnumberedKind::Other(other),
        };
        Self::Unnumbered { kind, pf }
    }

    /// Returns `true` for the UI (Unnumbered Information) subtype used
    /// by APRS.
    #[must_use]
    pub const fn is_ui(self) -> bool {
        matches!(
            self,
            Self::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation,
                ..
            }
        )
    }
}

/// Supervisory (S) frame sub-kinds (AX.25 v2.2 §4.3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupervisoryKind {
    /// Receive Ready (RR).
    ReceiveReady,
    /// Receive Not Ready (RNR).
    ReceiveNotReady,
    /// Reject (REJ).
    Reject,
    /// Selective Reject (SREJ, AX.25 v2.2 addition).
    SelectiveReject,
}

/// Unnumbered (U) frame sub-kinds (AX.25 v2.2 §4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnnumberedKind {
    /// Unnumbered Information (UI) — used by APRS.
    UnnumberedInformation,
    /// Set Asynchronous Balanced Mode (SABM).
    SetAsyncBalancedMode,
    /// Disconnect (DISC).
    Disconnect,
    /// Disconnected Mode (DM).
    DisconnectedMode,
    /// Unnumbered Acknowledge (UA).
    UnnumberedAcknowledge,
    /// Frame Reject (FRMR).
    FrameReject,
    /// Exchange Identification (XID).
    ExchangeIdentification,
    /// Test (TEST).
    Test,
    /// Any other pattern the parser does not classify.
    Other(u8),
}

/// Command/Response classification of an AX.25 address pair.
///
/// Per AX.25 v2.2 §4.3.1.2, bit 7 of the destination SSID byte and bit 7
/// of the source SSID byte together encode whether the frame is a command
/// or response. APRS only sends commands, but we parse both for
/// completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandResponse {
    /// AX.25 v2.2 Command frame (dest C-bit=1, source C-bit=0).
    Command,
    /// AX.25 v2.2 Response frame (dest C-bit=0, source C-bit=1).
    Response,
    /// Legacy AX.25 v2.0 or unknown (both C-bits equal).
    Legacy,
}

/// An AX.25 address (callsign + SSID).
///
/// In AX.25, each address is 7 bytes: 6 bytes of callsign (ASCII shifted
/// left by 1 bit) plus 1 SSID byte.
///
/// Both fields use the validated newtypes from
/// [`crate::types::aprs_wire`]. `Callsign` derefs to `&str` and
/// compares against `&str`/`String`, so most existing code that reads
/// `addr.callsign` continues to work. `Ssid` compares against `u8` and
/// provides `.get()` for arithmetic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Address {
    /// Station callsign (1-6 uppercase ASCII alphanumerics).
    pub callsign: crate::types::aprs_wire::Callsign,
    /// Secondary Station Identifier (0-15).
    pub ssid: crate::types::aprs_wire::Ssid,
    /// Has-been-repeated flag (H-bit).
    ///
    /// For digipeater addresses, indicates this hop has already been
    /// consumed. Encoded as bit 7 of the SSID byte in AX.25 wire format.
    pub repeated: bool,
    /// AX.25 v2.2 Command/Response bit (bit 7 of the SSID byte for
    /// destination/source addresses; the H-bit for digipeaters). Stored
    /// at parse time so callers can reconstruct the
    /// [`CommandResponse`] of the original frame; ignored when building
    /// a frame (build always emits 0).
    pub c_bit: bool,
}

impl Ax25Address {
    /// Create a new address with the H-bit unset (not yet repeated).
    ///
    /// # Panics
    ///
    /// Panics if `callsign` is empty, longer than 6 characters, contains
    /// non-alphanumeric characters, or if `ssid > 15`. Use
    /// [`Self::try_new`] for fallible construction from untrusted input.
    /// This infallible constructor exists for test helpers and internal
    /// code paths that already know the values are well-formed.
    #[must_use]
    pub fn new(callsign: &str, ssid: u8) -> Self {
        Self::try_new(callsign, ssid).expect("Ax25Address::new called with invalid callsign/ssid")
    }

    /// Create a new address with validation.
    ///
    /// Rejects empty or malformed callsigns (must be 1-6 uppercase ASCII
    /// alphanumeric characters) and out-of-range SSIDs (must be 0-15).
    /// Accepts mixed-case input and uppercases internally.
    ///
    /// # Errors
    ///
    /// Returns [`crate::error::ValidationError::AprsWireOutOfRange`] if either field
    /// fails its validation rules.
    pub fn try_new(callsign: &str, ssid: u8) -> Result<Self, crate::error::ValidationError> {
        Ok(Self {
            callsign: crate::types::aprs_wire::Callsign::new_case_insensitive(callsign)?,
            ssid: crate::types::aprs_wire::Ssid::new(ssid)?,
            repeated: false,
            c_bit: false,
        })
    }
}

impl fmt::Display for Ax25Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ssid == 0 {
            if self.repeated {
                write!(f, "{}*", self.callsign)
            } else {
                write!(f, "{}", self.callsign)
            }
        } else if self.repeated {
            write!(f, "{}-{}*", self.callsign, self.ssid)
        } else {
            write!(f, "{}-{}", self.callsign, self.ssid)
        }
    }
}

/// A parsed AX.25 UI (Unnumbered Information) frame.
///
/// APRS uses UI frames exclusively. The control field is `0x03` and
/// the protocol ID is `0xF0` (no layer 3) for standard APRS packets.
///
/// To inspect the AX.25 v2.2 Command/Response bits encoded in the C
/// bits of the source and destination SSID bytes, use
/// [`Self::command_response`] (re-derived on demand from the wire bytes
/// preserved by the parser).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Packet {
    /// Source station address.
    pub source: Ax25Address,
    /// Destination address (often an APRS "tocall" like `APxxxx`).
    pub destination: Ax25Address,
    /// Digipeater path (0-8 via addresses).
    pub digipeaters: Vec<Ax25Address>,
    /// Control field (0x03 for UI frames).
    pub control: u8,
    /// Protocol identifier (0xF0 = no layer 3, standard for APRS).
    pub protocol: u8,
    /// Information field (the APRS payload).
    pub info: Vec<u8>,
}

impl Ax25Packet {
    /// Decode the raw [`Self::control`] byte into a typed [`Ax25Control`]
    /// value.
    #[must_use]
    pub const fn control_typed(&self) -> Ax25Control {
        Ax25Control::from_byte(self.control)
    }

    /// Decode the raw [`Self::protocol`] byte into a typed [`Ax25Pid`]
    /// value.
    #[must_use]
    pub const fn pid(&self) -> Ax25Pid {
        Ax25Pid::from_byte(self.protocol)
    }

    /// `true` if this is a UI frame (APRS standard).
    #[must_use]
    pub const fn is_ui(&self) -> bool {
        self.control_typed().is_ui()
    }

    /// AX.25 v2.2 Command/Response classification of this frame, derived
    /// from the C-bits captured at parse time on the source and
    /// destination addresses (per AX.25 v2.2 §4.3.1.2).
    ///
    /// - `dest C-bit = 1, source C-bit = 0` → Command (APRS frames)
    /// - `dest C-bit = 0, source C-bit = 1` → Response
    /// - both equal → Legacy (AX.25 v2.0 or unknown)
    #[must_use]
    pub const fn command_response(&self) -> CommandResponse {
        match (self.destination.c_bit, self.source.c_bit) {
            (true, false) => CommandResponse::Command,
            (false, true) => CommandResponse::Response,
            _ => CommandResponse::Legacy,
        }
    }
}

/// Decode a single AX.25 address from a 7-byte slice.
///
/// The H-bit (has-been-repeated) is extracted from bit 7 of the SSID
/// byte. For digipeater addresses, this indicates the hop has been used.
///
/// Per AX.25 v2.2 §3.2, callsign characters must be ASCII alphanumeric.
/// Any byte that decodes to a character outside `A-Z`, `a-z`, `0-9`, or
/// space (the pad character) is rejected.
///
/// # Errors
///
/// Returns [`Ax25Error::InvalidCallsignByte`] if any callsign byte
/// decodes to a non-alphanumeric character. Space is treated as padding
/// and stripped.
fn decode_ax25_address(bytes: &[u8]) -> Result<Ax25Address, Ax25Error> {
    let mut callsign = String::with_capacity(6);
    for &b in &bytes[..6] {
        let ch = b >> 1;
        if ch == b' ' {
            // Pad; skip.
            continue;
        }
        if !ch.is_ascii_alphanumeric() {
            return Err(Ax25Error::InvalidCallsignByte(ch));
        }
        // AX.25 callsigns are stored uppercase; the parser tolerates
        // lowercase on the wire by uppercasing during construction.
        callsign.push(ch.to_ascii_uppercase() as char);
    }
    let ssid_raw = (bytes[6] >> 1) & 0x0F;
    let repeated = bytes[6] & 0x80 != 0;
    // The "C bit" is bit 7 of the SSID byte for source/destination
    // addresses (parsed independently from the H-bit which only applies
    // to digipeater entries — but the wire bit is the same position;
    // higher layers know which interpretation applies based on the
    // address slot).
    let c_bit = bytes[6] & 0x80 != 0;
    let callsign = crate::types::aprs_wire::Callsign::new(&callsign)
        .map_err(|_| Ax25Error::InvalidCallsignByte(0))?;
    let ssid = crate::types::aprs_wire::Ssid::new(ssid_raw)
        .map_err(|_| Ax25Error::InvalidCallsignByte(ssid_raw))?;
    Ok(Ax25Address {
        callsign,
        ssid,
        repeated,
        c_bit,
    })
}

/// Encode an AX.25 address into 7 bytes.
///
/// `is_last` sets the address-extension bit on the final address.
/// The H-bit (has-been-repeated) is encoded into bit 7 of the SSID byte.
fn encode_ax25_address(addr: &Ax25Address, is_last: bool) -> [u8; 7] {
    let mut bytes = [0x40u8; 7]; // space << 1 = 0x40
    for (i, ch) in addr.callsign.as_bytes().iter().take(6).enumerate() {
        bytes[i] = ch << 1;
    }
    let mut ssid_byte = 0x60 | ((addr.ssid.get() & 0x0F) << 1);
    if is_last {
        ssid_byte |= 0x01; // address-extension bit
    }
    if addr.repeated {
        ssid_byte |= 0x80; // H-bit (has-been-repeated)
    }
    bytes[6] = ssid_byte;
    bytes
}

/// Parse an AX.25 packet from raw bytes (as received in a KISS data frame).
///
/// Handles the standard UI frame format used by APRS:
/// `destination(7) | source(7) | [digipeaters(7 each)] | control(1) | PID(1) | info(N)`
///
/// # Errors
///
/// Returns [`Ax25Error`] if the packet structure is invalid.
pub fn parse_ax25(data: &[u8]) -> Result<Ax25Packet, Ax25Error> {
    // Minimum: dest(7) + src(7) + control(1) + PID(1) = 16
    if data.len() < 16 {
        return Err(Ax25Error::PacketTooShort);
    }

    let destination = decode_ax25_address(&data[0..7])?;
    let source = decode_ax25_address(&data[7..14])?;

    // Find end of address field (bit 0 of last byte in each 7-byte address)
    let mut addr_end = 14;
    let mut digipeaters = Vec::new();

    // Check if source address has the extension bit set
    if data[13] & 0x01 == 0 {
        // More addresses follow (digipeaters). AX.25 v2.2 §2.2.13 caps
        // this at 8 — reject packets claiming more to avoid unbounded
        // allocation from a malformed frame.
        loop {
            if digipeaters.len() >= MAX_DIGIPEATERS {
                return Err(Ax25Error::TooManyDigipeaters);
            }
            if addr_end + 7 > data.len() {
                return Err(Ax25Error::InvalidAddressLength);
            }
            let digi = decode_ax25_address(&data[addr_end..addr_end + 7])?;
            let is_last = data[addr_end + 6] & 0x01 != 0;
            digipeaters.push(digi);
            addr_end += 7;
            if is_last {
                break;
            }
        }
    }

    // After addresses: control + PID
    if addr_end + 2 > data.len() {
        return Err(Ax25Error::MissingControlFields);
    }

    let control = data[addr_end];
    let protocol = data[addr_end + 1];
    let info = data[addr_end + 2..].to_vec();

    Ok(Ax25Packet {
        source,
        destination,
        digipeaters,
        control,
        protocol,
        info,
    })
}

/// Build an AX.25 UI frame from an [`Ax25Packet`].
///
/// Returns the raw bytes suitable for encapsulation in a KISS data frame.
///
/// # Panics
///
/// Panics if the packet has more than [`MAX_DIGIPEATERS`] digipeater
/// addresses, which is the AX.25 v2.2 §2.2.13 cap. Use [`parse_ax25`] to
/// validate packets coming from untrusted sources before building them.
#[must_use]
pub fn build_ax25(packet: &Ax25Packet) -> Vec<u8> {
    assert!(
        packet.digipeaters.len() <= MAX_DIGIPEATERS,
        "AX.25 v2.2 §2.2.13: packet has {} digipeaters, max is {MAX_DIGIPEATERS}",
        packet.digipeaters.len(),
    );
    let no_digis = packet.digipeaters.is_empty();
    let total_len = 14 + packet.digipeaters.len() * 7 + 2 + packet.info.len();
    let mut out = Vec::with_capacity(total_len);

    // Destination (never last unless no source... but source always follows)
    out.extend_from_slice(&encode_ax25_address(&packet.destination, false));

    // Source
    out.extend_from_slice(&encode_ax25_address(&packet.source, no_digis));

    // Digipeaters
    for (i, digi) in packet.digipeaters.iter().enumerate() {
        let is_last = i == packet.digipeaters.len() - 1;
        out.extend_from_slice(&encode_ax25_address(digi, is_last));
    }

    out.push(packet.control);
    out.push(packet.protocol);
    out.extend_from_slice(&packet.info);

    out
}

// ---------------------------------------------------------------------------
// APRS position parsing
// ---------------------------------------------------------------------------

/// Diagnostic context for a parse failure.
///
/// Carries the byte offset within the input where the parser stopped,
/// alongside an error variant. Most parser entry points return the
/// bare error type for backwards compatibility; use
/// [`ParseContext::with_error`] to wrap one when richer diagnostics are
/// useful (e.g. when reporting failures from a fuzz harness or when
/// logging untrusted wire data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseContext<E> {
    /// Underlying error.
    pub error: E,
    /// Byte offset within the input where the parser noticed the
    /// problem (0 if unknown).
    pub offset: usize,
    /// Optional human-readable name for the field that failed.
    pub field: Option<&'static str>,
}

impl<E> ParseContext<E> {
    /// Wrap an error with the given byte offset and optional field name.
    pub const fn with_error(error: E, offset: usize, field: Option<&'static str>) -> Self {
        Self {
            error,
            offset,
            field,
        }
    }
}

impl<E: fmt::Display> fmt::Display for ParseContext<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = self.field {
            write!(
                f,
                "{} (at byte {} in field {field})",
                self.error, self.offset
            )
        } else {
            write!(f, "{} (at byte {})", self.error, self.offset)
        }
    }
}

/// Errors that can occur during APRS data parsing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AprsError {
    /// The info field is too short or has an unrecognized data type.
    #[error("invalid APRS format")]
    InvalidFormat,
    /// The position coordinates could not be parsed.
    #[error("invalid APRS coordinates")]
    InvalidCoordinates,
    /// Mic-E data requires the AX.25 destination address for decoding.
    #[error("Mic-E data requires destination address \u{2014} use parse_aprs_data_full()")]
    MicERequiresDestination,
    /// A digipeater path string could not be parsed.
    #[error("invalid digipeater path: {0}")]
    InvalidPath(String),
    /// The message text is too long (APRS 1.0.1 §14: max 67 characters).
    #[error("APRS message text exceeds 67 characters ({0} bytes)")]
    MessageTooLong(usize),
}

/// APRS position ambiguity level (APRS 1.0.1 §8.1.6).
///
/// Stations can deliberately reduce their reported precision by
/// replacing trailing latitude/longitude digits with spaces. Each level
/// masks one more trailing digit:
///
/// | Level | Example               | Effective precision |
/// |-------|-----------------------|---------------------|
/// | 0     | `4903.50N`            | 0.01 minute         |
/// | 1     | `4903.5 N`            | 0.1 minute          |
/// | 2     | `4903.  N`            | 1 minute            |
/// | 3     | `490 .  N`            | 10 minutes          |
/// | 4     | `49  .  N`            | 1 degree            |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAmbiguity {
    /// No ambiguity — full DDMM.HH precision.
    None,
    /// Last digit of hundredths-of-a-minute masked (0.1' precision).
    OneDigit,
    /// Whole hundredths-of-a-minute masked (1' precision).
    TwoDigits,
    /// Tens of minutes masked (10' precision).
    ThreeDigits,
    /// Whole minutes masked (1° precision).
    FourDigits,
}

/// Mic-E standard message code.
///
/// Per APRS 1.0.1 §10.1, the three message bits (A/B/C) encoded by the
/// "custom" status of destination chars 0-2 select one of 8 standard
/// messages, or the eighth code indicates that a custom status is carried
/// in the comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MiceMessage {
    /// M0 — "Off Duty" (111 standard, 000 custom).
    OffDuty,
    /// M1 — "En Route" (110, 001).
    EnRoute,
    /// M2 — "In Service" (101, 010).
    InService,
    /// M3 — "Returning" (100, 011).
    Returning,
    /// M4 — "Committed" (011, 100).
    Committed,
    /// M5 — "Special" (010, 101).
    Special,
    /// M6 — "Priority" (001, 110).
    Priority,
    /// Emergency — (000, 111) always means emergency.
    Emergency,
}

/// A parsed APRS position report.
///
/// Includes optional speed/course fields populated by Mic-E decoding and
/// optional embedded weather data populated when the station reports with
/// the weather-station symbol code `_`. Data extensions (course/speed,
/// PHG, altitude, DAO) found in the comment field are parsed
/// automatically and exposed via [`Self::extensions`].
#[derive(Debug, Clone, PartialEq)]
pub struct AprsPosition {
    /// Latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = East).
    pub longitude: f64,
    /// APRS symbol table identifier character.
    pub symbol_table: char,
    /// APRS symbol code character.
    pub symbol_code: char,
    /// Speed in knots (from Mic-E or course/speed extension).
    pub speed_knots: Option<u16>,
    /// Course in degrees (from Mic-E or course/speed extension).
    pub course_degrees: Option<u16>,
    /// Optional comment/extension text after the position.
    pub comment: String,
    /// Optional weather data embedded in the position comment.
    ///
    /// Populated when the symbol code is `_` (weather station) and the
    /// comment starts with the `DDD/SSS` wind direction/speed extension,
    /// followed by the remaining weather fields. See APRS 1.0.1 §12.1.
    pub weather: Option<AprsWeather>,
    /// Parsed data extensions (course/speed, PHG, altitude, DAO) found in
    /// the comment field.
    ///
    /// Populated automatically by [`parse_aprs_position`] via
    /// [`parse_aprs_extensions`]. Fields that aren't present in the
    /// comment are `None`.
    pub extensions: AprsDataExtension,
    /// Mic-E standard message code (only populated by
    /// [`parse_mice_position`]).
    pub mice_message: Option<MiceMessage>,
    /// Mic-E altitude in metres, decoded from the comment per APRS 1.0.1
    /// §10.1.1 (three base-91 chars followed by `}`, offset from -10000).
    pub mice_altitude_m: Option<i32>,
    /// Position ambiguity level (APRS 1.0.1 §8.1.6).
    ///
    /// Stations can deliberately reduce their precision by replacing
    /// trailing lat/lon digits with spaces; this field records how many
    /// digits were masked. Mic-E and compressed positions do not use
    /// ambiguity and always report [`PositionAmbiguity::None`].
    pub ambiguity: PositionAmbiguity,
}

/// Parse APRS latitude from the standard `DDMM.HH[N/S]` format.
///
/// Returns `(degrees, ambiguity)` where `degrees` is the decimal-degree
/// value (positive North) and `ambiguity` counts how many trailing
/// digits were replaced with spaces per APRS 1.0.1 §8.1.6. When digits
/// are masked, the decoded position is the "centre" of the ambiguous
/// box (masked digits treated as `0`), which is the convention used by
/// `javAPRS` and the original `APRSdos`.
fn parse_aprs_latitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    if s.len() < 8 {
        return Err(AprsError::InvalidCoordinates);
    }
    let bytes: [u8; 8] = s[..8]
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DD MM . HH H/S
    // Position:     0 1 2 3 4 5 6 7
    // Ambiguity masks trailing digits with spaces (APRS 1.0.1 §8.1.6):
    //   Level 0: DDMM.HH  (no spaces)
    //   Level 1: DDMM.H<sp>
    //   Level 2: DDMM.<sp><sp>
    //   Level 3: DDM<sp>.<sp><sp>
    //   Level 4: DD<sp><sp>.<sp><sp>
    let (digits, ambiguity) = unmask_coord_digits(&bytes[..7], 4)?;
    // digits is now 2 + 5 = 7 ASCII bytes for DDMM.HH (with '.' at idx 4).
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let degrees: f64 = text[..2]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = text[2..7]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = bytes[7];

    let mut lat = degrees + minutes / 60.0;
    if hemisphere == b'S' {
        lat = -lat;
    } else if hemisphere != b'N' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lat, ambiguity))
}

/// Parse APRS longitude from the standard `DDDMM.HH[E/W]` format.
fn parse_aprs_longitude(s: &[u8]) -> Result<(f64, PositionAmbiguity), AprsError> {
    if s.len() < 9 {
        return Err(AprsError::InvalidCoordinates);
    }
    let bytes: [u8; 9] = s[..9]
        .try_into()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    // Field layout: DDD MM . HH E/W
    // Position:     0 1 2 3 4 5 6 7 8
    let (digits, ambiguity) = unmask_coord_digits(&bytes[..8], 5)?;
    // digits = DDDMM.HH (8 ASCII bytes with '.' at idx 5).
    let text = std::str::from_utf8(&digits).map_err(|_| AprsError::InvalidCoordinates)?;
    let degrees: f64 = text[..3]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = text[3..8]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = bytes[8];

    let mut lon = degrees + minutes / 60.0;
    if hemisphere == b'W' {
        lon = -lon;
    } else if hemisphere != b'E' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok((lon, ambiguity))
}

/// Replace space-masked digits with `'0'` and return the masked-count
/// alongside the rebuilt byte sequence. `dot_idx` is the index of the
/// literal `.` in the field (4 for latitude, 5 for longitude).
fn unmask_coord_digits(
    field: &[u8],
    dot_idx: usize,
) -> Result<([u8; 8], PositionAmbiguity), AprsError> {
    if field.len() > 8 || field[dot_idx] != b'.' {
        return Err(AprsError::InvalidCoordinates);
    }
    // Ambiguity is counted by walking the mask-eligible positions from
    // rightmost back to the start until we stop seeing spaces.
    // Mask order (rightmost first): HH tens, HH ones, MM ones, MM tens
    // Positions for latitude:   [0 1 2 3 '.' 5 6]
    //                            DD MM     HH
    // Positions for longitude:  [0 1 2 3 4 '.' 6 7]
    //                            DDD MM    HH
    let mask_order: [usize; 4] = if dot_idx == 4 {
        [6, 5, 3, 2] // lat: HH(6,5), MM(3,2)
    } else {
        [7, 6, 4, 3] // lon: HH(7,6), MM(4,3)
    };
    let mut count: u8 = 0;
    for &pos in &mask_order {
        if field[pos] == b' ' {
            count += 1;
        } else {
            break;
        }
    }
    // Also fail if we see a space at a non-maskable position (outside
    // the trailing run).
    let mut out = [b'0'; 8];
    out[..field.len()].copy_from_slice(field);
    for pos in &mask_order[..count as usize] {
        out[*pos] = b'0';
    }
    for (i, &b) in field.iter().enumerate() {
        if b == b' ' && !mask_order[..count as usize].contains(&i) {
            return Err(AprsError::InvalidCoordinates);
        }
    }
    let ambiguity = match count {
        0 => PositionAmbiguity::None,
        1 => PositionAmbiguity::OneDigit,
        2 => PositionAmbiguity::TwoDigits,
        3 => PositionAmbiguity::ThreeDigits,
        _ => PositionAmbiguity::FourDigits,
    };
    Ok((out, ambiguity))
}

/// Parse an APRS position report from an AX.25 information field.
///
/// Supports three APRS position formats (per APRS101.PDF chapters 8-9):
/// - **Uncompressed**: `!`/`=`/`/`/`@` with ASCII lat/lon (`DDMM.HH`)
/// - **Compressed**: `!`/`=`/`/`/`@` with base-91 encoded lat/lon (13 bytes)
///
/// For **Mic-E** positions (`` ` ``/`'`), use [`parse_mice_position`] which
/// also requires the AX.25 destination address.
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or coordinates are invalid.
pub fn parse_aprs_position(info: &[u8]) -> Result<AprsPosition, AprsError> {
    if info.is_empty() {
        return Err(AprsError::InvalidFormat);
    }

    let data_type = info[0];
    let body = match data_type {
        // Position without timestamp: ! or =
        b'!' | b'=' => {
            if info.len() < 2 {
                return Err(AprsError::InvalidFormat);
            }
            &info[1..]
        }
        // Position with timestamp: / or @
        // Timestamp is 7 characters after the type byte
        b'/' | b'@' => {
            if info.len() < 9 {
                return Err(AprsError::InvalidFormat);
            }
            &info[8..]
        }
        _ => return Err(AprsError::InvalidFormat),
    };

    if body.is_empty() {
        return Err(AprsError::InvalidFormat);
    }

    // Detect compressed vs uncompressed: if the first byte is a digit (0-9),
    // it's uncompressed latitude. Otherwise it's a compressed symbol table char.
    if body[0].is_ascii_digit() {
        parse_uncompressed_body(body)
    } else {
        parse_compressed_body(body)
    }
}

/// Parse uncompressed APRS position body.
///
/// Format: `lat(8) sym_table(1) lon(9) sym_code(1) [comment]` = 19+ bytes.
fn parse_uncompressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    if body.len() < 19 {
        return Err(AprsError::InvalidFormat);
    }

    let (latitude, lat_ambig) = parse_aprs_latitude(&body[..8])?;
    let symbol_table = body[8] as char;
    let (longitude, lon_ambig) = parse_aprs_longitude(&body[9..18])?;
    let symbol_code = body[18] as char;
    // A position's ambiguity is the maximum of the two component
    // ambiguities — whichever field was masked more aggressively wins.
    let ambiguity = std::cmp::max_by_key(lat_ambig, lon_ambig, |a| match a {
        PositionAmbiguity::None => 0,
        PositionAmbiguity::OneDigit => 1,
        PositionAmbiguity::TwoDigits => 2,
        PositionAmbiguity::ThreeDigits => 3,
        PositionAmbiguity::FourDigits => 4,
    });

    let comment = if body.len() > 19 {
        String::from_utf8_lossy(&body[19..]).into_owned()
    } else {
        String::new()
    };

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    // If the comment had a CSE/SPD extension, surface it on speed/course
    // too so callers that only read those fields see the data.
    let (speed_knots, course_degrees) = match extensions.course_speed {
        Some((course, speed)) => (Some(speed), Some(course)),
        None => (None, None),
    };
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions,
        mice_message: None,
        mice_altitude_m: None,
        ambiguity,
    })
}

/// Parse compressed APRS position body (APRS101.PDF Chapter 9).
///
/// Format: `sym_table(1) YYYY(4) XXXX(4) sym_code(1) cs(1) s(1) t(1)` = 13 bytes.
/// YYYY and XXXX are base-91 encoded (each byte = ASCII 33-124, value = byte - 33).
///
/// Latitude:  `90 - (YYYY / 380926.0)` degrees
/// Longitude: `-180 + (XXXX / 190463.0)` degrees
fn parse_compressed_body(body: &[u8]) -> Result<AprsPosition, AprsError> {
    if body.len() < 13 {
        return Err(AprsError::InvalidFormat);
    }

    let symbol_table = body[0] as char;
    let lat_val = decode_base91_4(&body[1..5])?;
    let lon_val = decode_base91_4(&body[5..9])?;
    let symbol_code = body[9] as char;

    let latitude = 90.0 - f64::from(lat_val) / 380_926.0;
    let longitude = -180.0 + f64::from(lon_val) / 190_463.0;

    // Decode the 3-byte cs/s/t tail per APRS 1.0.1 §9.
    // The `t` byte is the Compression Type: bits 3-4 determine what
    // `cs` encodes:
    //   00 = current GGA (course+speed)
    //   01 = old (other) GGA
    //   10 = preset
    //   11 = software flag
    // When cs is space (0x20 == 32 → base-91 value -1), there is no
    // course/speed data.
    let cs_byte = body[10];
    let s_byte = body[11];
    let t_byte = body[12];
    let (compressed_altitude_ft, compressed_course_speed) =
        decode_compressed_tail(cs_byte, s_byte, t_byte);

    let comment = if body.len() > 13 {
        String::from_utf8_lossy(&body[13..]).into_owned()
    } else {
        String::new()
    };

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    // Surface course/speed into the direct fields too.
    let (speed_knots, course_degrees) =
        compressed_course_speed.map_or((None, None), |(course, speed)| (Some(speed), Some(course)));
    let final_extensions = if let Some(alt) = compressed_altitude_ft {
        AprsDataExtension {
            altitude_ft: Some(alt),
            ..extensions
        }
    } else {
        extensions
    };
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions: final_extensions,
        mice_message: None,
        mice_altitude_m: None,
        // Compressed positions do not use APRS §8.1.6 ambiguity.
        ambiguity: PositionAmbiguity::None,
    })
}

/// Decode the 3-byte `cs`/`s`/`t` compression tail of a compressed APRS
/// position report per APRS 1.0.1 §9 Table 10.
///
/// Returns `(altitude_ft, course_speed)` where either may be `None`.
/// The `t` byte selects which flavour of data is encoded in the `cs`/`s`
/// bytes:
/// - `cs == b' '` (space): no course/speed/altitude data
/// - `cs` in `'!'-'z'` and `t` bits 3-4 = 00 or 01: course+speed
/// - `cs` in `'!'-'z'` and `t` bits 3-4 = 10: altitude (feet)
/// - `cs == b'{'`: pre-calculated range (we don't expose it yet)
fn decode_compressed_tail(cs: u8, s: u8, t: u8) -> (Option<i32>, Option<(u16, u16)>) {
    // Space in the `cs` column means "no data."
    if cs == b' ' {
        return (None, None);
    }
    // The `t` byte minus 33 gives a 6-bit compression type value. Bits
    // 3-4 (0x18) select the semantic meaning of `cs`/`s`.
    let t_val = t.saturating_sub(33);
    let type_bits = (t_val >> 3) & 0x03;
    match type_bits {
        // 0b00 / 0b01: course (c) + speed (s). Course is (cs - 33) * 4
        // degrees. Speed is 1.08^(s - 33) - 1 knots.
        0 | 1 => {
            let c = cs.saturating_sub(33);
            let s_val = s.saturating_sub(33);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let speed_knots = (1.08_f64.powi(i32::from(s_val)) - 1.0).round() as u16;
            let course_deg = u16::from(c) * 4;
            // Course 0 == "no data" per spec convention.
            if course_deg == 0 && speed_knots == 0 {
                (None, None)
            } else {
                (None, Some((course_deg, speed_knots)))
            }
        }
        // 0b10: altitude. cs,s = base-91 two-char altitude, value =
        // 1.002^((cs-33)*91 + (s-33)) feet.
        2 => {
            let c = i32::from(cs.saturating_sub(33));
            let s_val = i32::from(s.saturating_sub(33));
            let exponent = c * 91 + s_val;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let alt_ft = 1.002_f64.powi(exponent).round() as i32;
            (Some(alt_ft), None)
        }
        // 0b11 (range): not currently surfaced.
        _ => (None, None),
    }
}

/// Decode a 4-byte base-91 value.
///
/// Each byte is in the ASCII range 33-124. The value is:
/// `b[0]*91^3 + b[1]*91^2 + b[2]*91 + b[3]`
fn decode_base91_4(bytes: &[u8]) -> Result<u32, AprsError> {
    if bytes.len() < 4 {
        return Err(AprsError::InvalidCoordinates);
    }
    let mut val: u32 = 0;
    for &b in &bytes[..4] {
        if !(33..=124).contains(&b) {
            return Err(AprsError::InvalidCoordinates);
        }
        val = val * 91 + u32::from(b - 33);
    }
    Ok(val)
}

/// Parse a Mic-E encoded APRS position (APRS101.PDF Chapter 10).
///
/// Mic-E is a compact encoding used by Kenwood HTs (including the TH-D75)
/// that splits the position across two fields:
/// - **Latitude** is encoded in the 6-character AX.25 destination address
/// - **Longitude** and speed/course are in the info field body
///
/// Data type identifiers: `` ` `` (0x60, current Mic-E) or `'` (0x27, old Mic-E).
/// The TH-D75 uses current Mic-E (`` ` ``).
///
/// # Parameters
///
/// - `destination`: The AX.25 destination callsign (e.g., "T4SP0R")
/// - `info`: The full AX.25 information field (including the type byte)
///
/// # Errors
///
/// Returns [`AprsError`] if the Mic-E encoding is invalid.
pub fn parse_mice_position(destination: &str, info: &[u8]) -> Result<AprsPosition, AprsError> {
    if info.len() < 9 || destination.len() < 6 {
        return Err(AprsError::InvalidFormat);
    }

    let data_type = info[0];
    if data_type != b'`' && data_type != b'\'' && data_type != 0x1C && data_type != 0x1D {
        return Err(AprsError::InvalidFormat);
    }

    // Validate Mic-E longitude bytes are in valid range (28-127 per APRS101).
    for &b in &info[1..4] {
        if b < 28 {
            return Err(AprsError::InvalidCoordinates);
        }
    }

    let dest = destination.as_bytes();

    // --- Latitude from destination address ---
    // Each of the 6 destination chars encodes a latitude digit plus
    // N/S and longitude offset flags. Chars 0-9 and A-L map to digits.
    let mut lat_digits = [0u8; 6];
    let mut north = true;
    let mut lon_offset = 0i16;
    // Bit 2 (char index 3): N/S indicator
    // Bit 1 (char index 4): longitude offset (+100)
    // Bit 0 (char index 5): longitude offset (not used separately)

    for (i, &ch) in dest[..6].iter().enumerate() {
        let (digit, is_custom) = mice_dest_digit(ch)?;
        lat_digits[i] = digit;

        // Chars 0-3: if custom (A-L), set message bits (we don't use them for position)
        // Char 3: N/S flag — custom = North
        if i == 3 {
            north = is_custom;
        }
        // Char 4: longitude offset — custom = +100 degrees
        if i == 4 && is_custom {
            lon_offset = 100;
        }
        // Char 5: W/E flag — custom = West (negate longitude)
    }

    let lat_deg = f64::from(lat_digits[0]).mul_add(10.0, f64::from(lat_digits[1]));
    let lat_min = f64::from(lat_digits[2]).mul_add(10.0, f64::from(lat_digits[3]))
        + f64::from(lat_digits[4]) / 10.0
        + f64::from(lat_digits[5]) / 100.0;
    let mut latitude = lat_deg + lat_min / 60.0;
    if !north {
        latitude = -latitude;
    }

    // --- Longitude from info field ---
    // info[1] = degrees (d+28), info[2] = minutes (m+28), info[3] = hundredths (h+28)
    let d = i16::from(info[1]) - 28;
    let m = i16::from(info[2]) - 28;
    let h = i16::from(info[3]) - 28;

    let mut lon_deg = d + lon_offset;
    if (180..=189).contains(&lon_deg) {
        lon_deg -= 80;
    } else if (190..=199).contains(&lon_deg) {
        lon_deg -= 190;
    }

    let lon_min = if m >= 60 { m - 60 } else { m };
    let longitude_abs = f64::from(lon_deg) + (f64::from(lon_min) + f64::from(h) / 100.0) / 60.0;

    // Char 5 of destination: custom = West
    let west = mice_dest_is_custom(dest[5]);
    let longitude = if west { -longitude_abs } else { longitude_abs };

    // --- Speed and course from info[4..7] (per APRS101 Chapter 10) ---
    // SP+28 = info[4], DC+28 = info[5], SE+28 = info[6]
    // Speed = (SP - 28) * 10 + (DC - 28) / 10  (integer division)
    // Course = ((DC - 28) mod 10) * 100 + (SE - 28)
    let (speed_knots, course_degrees) = if info.len() >= 7 {
        let sp = u16::from(info[4]).saturating_sub(28);
        let dc = u16::from(info[5]).saturating_sub(28);
        let se = u16::from(info[6]).saturating_sub(28);
        let speed = sp * 10 + dc / 10;
        let course_raw = (dc % 10) * 100 + se;
        // Speed 800+ is invalid per spec; course 0 = not known
        let speed_opt = if speed < 800 { Some(speed) } else { None };
        let course_opt = if course_raw > 0 && course_raw <= 360 {
            Some(course_raw)
        } else {
            None
        };
        (speed_opt, course_opt)
    } else {
        (None, None)
    };

    // Symbol: info[7] = symbol code, info[8] = symbol table
    let symbol_code = if info.len() > 7 { info[7] as char } else { '/' };
    let symbol_table = if info.len() > 8 { info[8] as char } else { '/' };

    let comment = if info.len() > 9 {
        String::from_utf8_lossy(&info[9..]).into_owned()
    } else {
        String::new()
    };

    // Decode the Mic-E standard message code from destination chars 0-2
    // (APRS 1.0.1 §10.1 Table 10). `None` if any char is in the custom range.
    let mice_message = mice_decode_message([dest[0], dest[1], dest[2]]);

    // Look for optional altitude in the comment (`<ccc>}` base-91, metres
    // offset from -10000) per APRS 1.0.1 §10.1.1.
    let mice_altitude_m = mice_decode_altitude(&comment);

    let weather = extract_position_weather(symbol_code, &comment);
    let extensions = parse_aprs_extensions(&comment);
    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
        weather,
        extensions,
        mice_message,
        mice_altitude_m,
        // Mic-E positions are not subject to §8.1.6 ambiguity masking.
        ambiguity: PositionAmbiguity::None,
    })
}

/// Extract a digit (0-9) from a Mic-E destination character.
///
/// Returns `(digit, is_custom)` where `is_custom` is true for A-K/L
/// (used for N/S, lon offset, and W/E flags).
const fn mice_dest_digit(ch: u8) -> Result<(u8, bool), AprsError> {
    match ch {
        b'0'..=b'9' => Ok((ch - b'0', false)),
        b'A'..=b'J' => Ok((ch - b'A', true)), // A=0, B=1, ..., J=9
        b'K' | b'L' | b'Z' => Ok((0, true)),  // K, L, Z map to space (0)
        b'P'..=b'Y' => Ok((ch - b'P', true)), // P=0, Q=1, ..., Y=9
        _ => Err(AprsError::InvalidCoordinates),
    }
}

/// Check if a Mic-E destination character is an uppercase letter.
///
/// Used by chars 3-5 for N/S, +100 lon offset, and W/E flag decoding.
const fn mice_dest_is_custom(ch: u8) -> bool {
    matches!(ch, b'A'..=b'L' | b'P'..=b'Z')
}

/// Mic-E message-bit classification for destination chars 0-2.
///
/// Per APRS 1.0.1 §10.1, each of the first three destination characters
/// contributes one bit (A, B, or C) to a 3-bit message code via three
/// categories:
///
/// - `Std0` — character is `0`-`9` or `L`, contributes bit `0`
/// - `Std1` — character is `P`-`Y` or `Z`, contributes bit `1`
/// - `Custom` — character is `A`-`K`, marks the entire message as custom
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MiceMsgClass {
    Std0,
    Std1,
    Custom,
}

const fn mice_msg_class(ch: u8) -> Option<MiceMsgClass> {
    match ch {
        b'0'..=b'9' | b'L' => Some(MiceMsgClass::Std0),
        b'P'..=b'Y' | b'Z' => Some(MiceMsgClass::Std1),
        b'A'..=b'K' => Some(MiceMsgClass::Custom),
        _ => None,
    }
}

/// Decode the 3-bit Mic-E message code from destination chars 0-2.
///
/// Returns `None` if any of the three chars is in the Custom range
/// (`A`-`K`); those encode user-defined messages the library does not
/// currently interpret. Returns `Some(MiceMessage)` for the 8 standard
/// codes (APRS 1.0.1 Table 10).
fn mice_decode_message(chars: [u8; 3]) -> Option<MiceMessage> {
    let c0 = mice_msg_class(chars[0])?;
    let c1 = mice_msg_class(chars[1])?;
    let c2 = mice_msg_class(chars[2])?;
    if matches!(
        (c0, c1, c2),
        (MiceMsgClass::Custom, _, _) | (_, MiceMsgClass::Custom, _) | (_, _, MiceMsgClass::Custom)
    ) {
        return None;
    }
    let bit = |c| u8::from(matches!(c, MiceMsgClass::Std1));
    let idx = (bit(c0) << 2) | (bit(c1) << 1) | bit(c2);
    Some(match idx {
        0b111 => MiceMessage::OffDuty,
        0b110 => MiceMessage::EnRoute,
        0b101 => MiceMessage::InService,
        0b100 => MiceMessage::Returning,
        0b011 => MiceMessage::Committed,
        0b010 => MiceMessage::Special,
        0b001 => MiceMessage::Priority,
        _ => MiceMessage::Emergency, // 0b000
    })
}

/// Map a Mic-E standard message code to its 3-bit (A, B, C) encoding
/// per APRS 1.0.1 §10.1 Table 10. Returns `(bit_a, bit_b, bit_c)` where
/// `true` means "standard-1" (uppercase P-Y in the destination char).
const fn mice_message_bits(msg: MiceMessage) -> (bool, bool, bool) {
    match msg {
        MiceMessage::OffDuty => (true, true, true),      // 111
        MiceMessage::EnRoute => (true, true, false),     // 110
        MiceMessage::InService => (true, false, true),   // 101
        MiceMessage::Returning => (true, false, false),  // 100
        MiceMessage::Committed => (false, true, true),   // 011
        MiceMessage::Special => (false, true, false),    // 010
        MiceMessage::Priority => (false, false, true),   // 001
        MiceMessage::Emergency => (false, false, false), // 000
    }
}

/// Decode Mic-E altitude from the comment field.
///
/// Per APRS 1.0.1 §10.1.1, altitude is optionally encoded as three
/// base-91 characters (33-126, value = byte - 33) followed by a literal
/// `}`. The decoded value is metres, offset from -10000 (so the wire
/// value 10000 = sea level).
///
/// Searches the comment for the first occurrence of the `ccc}` pattern
/// where each `c` is a valid base-91 printable character.
fn mice_decode_altitude(comment: &str) -> Option<i32> {
    let bytes = comment.as_bytes();
    if bytes.len() < 4 {
        return None;
    }
    for i in 0..=bytes.len() - 4 {
        if bytes[i + 3] != b'}' {
            continue;
        }
        let b0 = bytes[i];
        let b1 = bytes[i + 1];
        let b2 = bytes[i + 2];
        if !(33..=126).contains(&b0) || !(33..=126).contains(&b1) || !(33..=126).contains(&b2) {
            continue;
        }
        let val = i32::from(b0 - 33) * 91 * 91 + i32::from(b1 - 33) * 91 + i32::from(b2 - 33);
        return Some(val - 10_000);
    }
    None
}

// ---------------------------------------------------------------------------
// APRS data type parsing (messages, status, objects, items, weather)
// ---------------------------------------------------------------------------

/// A parsed APRS data frame, covering all major APRS data types.
///
/// Per APRS101.PDF, the data type is determined by the first byte of the
/// AX.25 information field. This enum covers the types most relevant to
/// the TH-D75's APRS implementation.
#[derive(Debug, Clone, PartialEq)]
pub enum AprsData {
    /// Position report (uncompressed, compressed, or Mic-E).
    Position(AprsPosition),
    /// APRS message addressed to a specific station.
    Message(AprsMessage),
    /// Status report (free-form text, optionally with Maidenhead grid).
    Status(AprsStatus),
    /// Object report (named, with position and timestamp).
    Object(AprsObject),
    /// Item report (named, with position, no timestamp).
    Item(AprsItem),
    /// Weather report (temperature, wind, rain, pressure, humidity).
    Weather(AprsWeather),
    /// Telemetry report (analog values and digital status).
    Telemetry(AprsTelemetry),
    /// Query (position, status, message, or direction finding).
    Query(AprsQuery),
    /// Third-party traffic — a packet originating elsewhere and
    /// forwarded by an intermediate station (APRS 1.0.1 §17). The
    /// `header` carries the original `source>dest,path` and the
    /// `payload` the original info field.
    ThirdParty {
        /// Raw `source>dest,path` header text from the third-party
        /// wrapper.
        header: String,
        /// Original APRS info field as bytes (no further parsing).
        payload: Vec<u8>,
    },
    /// Maidenhead grid locator (data type `[`). The string form is the
    /// 4-6 character grid square, e.g. `"EM13qc"` or `"FM18lv"`.
    Grid(String),
    /// Raw GPS sentence / Ultimeter 2000 data (data type `$`).
    ///
    /// APRS 1.0.1 §5.2: anything starting with `$GP`, `$GN`, `$GL`,
    /// `$GA` (GPS/GNSS NMEA) or other `$`-prefixed instrument data.
    /// We store the full NMEA sentence minus the leading `$`.
    RawGps(String),
    /// Station capabilities report (data type `<`).
    ///
    /// APRS 1.0.1 §15.2: comma-separated `TOKEN=value` tuples
    /// describing what the station supports (`IGATE`, `MSG_CNT`,
    /// `LOC_CNT`, etc.). We store them as a map.
    StationCapabilities(Vec<(String, String)>),
    /// Agrelo `DFjr` (direction-finding) data (data type `%`).
    ///
    /// The library doesn't interpret the binary format; we preserve
    /// the raw payload bytes for callers that do.
    AgreloDfJr(Vec<u8>),
    /// User-defined APRS data (data type `{`).
    ///
    /// APRS 1.0.1 §18: format is `{<experiment_id><type><data>` where
    /// the experiment ID is one character. We split it out for
    /// convenience; callers that understand the experiment can parse
    /// the rest.
    UserDefined {
        /// One-character experiment identifier (immediately follows `{`).
        experiment: char,
        /// Everything after the experiment ID.
        data: Vec<u8>,
    },
    /// Invalid/test frame (data type `,`).
    ///
    /// Used for test beacons and frames that should be ignored by
    /// normal receivers. We preserve the payload for diagnostics.
    InvalidOrTest(Vec<u8>),
}

/// APRS message kind (per APRS 1.0.1 §14 and bulletin sections).
///
/// Distinguishes direct station-to-station messages from the various
/// bulletin forms based on the addressee prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Direct station-to-station message.
    Direct,
    /// Generic bulletin (addressee `BLN0`-`BLN9`).
    Bulletin {
        /// Bulletin number (0-9).
        number: u8,
    },
    /// Group bulletin (addressee `BLN<group>` where group is an alpha
    /// identifier, e.g. `BLNWX` for weather group).
    GroupBulletin {
        /// Group identifier (1-5 alphanumeric characters).
        group: String,
    },
    /// National Weather Service bulletin (addressee `NWS-*`, `SKY-*`,
    /// `CWA-*`, `BOM-*`).
    NwsBulletin,
    /// An APRS ack/rej control frame (text begins with `ack` or `rej`
    /// followed by 1-5 alnum).
    AckRej,
}

/// Maximum APRS message text length in bytes (APRS 1.0.1 §14).
pub const MAX_APRS_MESSAGE_TEXT_LEN: usize = 67;

/// An APRS message (data type `:`) addressed to a specific station or
/// group.
///
/// Format: `:ADDRESSEE:message text{ID` or, with the APRS 1.2 reply-ack
/// extension, `:ADDRESSEE:message text{MM}AA` where `MM` is this
/// message's ID and `AA` is an ack for a previously-received message.
/// - Addressee is exactly 9 characters, space-padded.
/// - Message text follows the second `:`.
/// - Optional message ID after `{` (for ack/rej).
/// - Optional reply-ack after `}` (APRS 1.2).
///
/// The TH-D75 displays received messages on-screen and can store
/// up to 100 messages in the station list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsMessage {
    /// Destination callsign (up to 9 chars, trimmed).
    pub addressee: String,
    /// Message text content.
    pub text: String,
    /// Optional message sequence number (for ack/rej tracking).
    pub message_id: Option<String>,
    /// Optional APRS 1.2 reply-ack: when the sender bundles an
    /// acknowledgement for a previously-received message into a new
    /// outgoing message. Format on wire is `{MM}AA` where `AA` is the
    /// acknowledged msgno.
    pub reply_ack: Option<String>,
}

impl AprsMessage {
    /// Classify this message by addressee / text pattern per APRS 1.0.1
    /// §14 and bulletin conventions.
    #[must_use]
    pub fn kind(&self) -> MessageKind {
        let addr = self.addressee.trim();
        // Check ack/rej on text first — control frames use a regular
        // addressee.
        if aprs_messaging::classify_ack_rej(&self.text).is_some() {
            return MessageKind::AckRej;
        }
        // NWS bulletins use well-known prefixes.
        if addr.starts_with("NWS-")
            || addr.starts_with("SKY-")
            || addr.starts_with("CWA-")
            || addr.starts_with("BOM-")
        {
            return MessageKind::NwsBulletin;
        }
        // Numeric bulletin: BLN0-BLN9 exactly.
        if let Some(rest) = addr.strip_prefix("BLN") {
            if rest.len() == 1
                && let Some(n) = rest.bytes().next()
                && n.is_ascii_digit()
            {
                return MessageKind::Bulletin { number: n - b'0' };
            }
            // Group bulletin: BLN<group> where group is 1-5 alnum.
            if (1..=5).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return MessageKind::GroupBulletin {
                    group: rest.to_owned(),
                };
            }
        }
        MessageKind::Direct
    }
}

/// An APRS status report (data type `>`).
///
/// Contains free-form text, optionally prefixed with a Maidenhead
/// grid locator (6 chars) or a timestamp (7 chars DHM/HMS).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsStatus {
    /// Status text.
    pub text: String,
}

/// An APRS timestamp as used by object and position-with-timestamp
/// reports (APRS 1.0.1 §6.1).
///
/// Four formats are defined on the wire:
///
/// | Suffix | Meaning | Digits |
/// |--------|---------|--------|
/// | `z`    | Day / hour / minute, zulu | DDHHMM |
/// | `/`    | Day / hour / minute, local| DDHHMM |
/// | `h`    | Hour / minute / second, zulu | HHMMSS |
/// | (none) | Month / day / hour / minute, zulu (11 chars) | MDHM |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsTimestamp {
    /// Day / hour / minute in Zulu (UTC) time. Format `DDHHMMz`.
    DhmZulu {
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
    /// Day / hour / minute in local time. Format `DDHHMM/`.
    DhmLocal {
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
    /// Hour / minute / second in Zulu (UTC) time. Format `HHMMSSh`.
    Hms {
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
        /// Second, 0-59.
        second: u8,
    },
    /// Month / day / hour / minute in Zulu (UTC) time (no suffix).
    /// Format `MMDDHHMM`.
    Mdhm {
        /// Month, 1-12.
        month: u8,
        /// Day of month, 1-31.
        day: u8,
        /// Hour, 0-23.
        hour: u8,
        /// Minute, 0-59.
        minute: u8,
    },
}

impl AprsTimestamp {
    /// Format this timestamp as the exact 7-byte APRS wire representation
    /// (or 8 bytes for `Mdhm`).
    #[must_use]
    pub fn to_wire_string(self) -> String {
        match self {
            Self::DhmZulu { day, hour, minute } => {
                format!("{day:02}{hour:02}{minute:02}z")
            }
            Self::DhmLocal { day, hour, minute } => {
                format!("{day:02}{hour:02}{minute:02}/")
            }
            Self::Hms {
                hour,
                minute,
                second,
            } => {
                format!("{hour:02}{minute:02}{second:02}h")
            }
            Self::Mdhm {
                month,
                day,
                hour,
                minute,
            } => {
                format!("{month:02}{day:02}{hour:02}{minute:02}")
            }
        }
    }
}

/// An APRS object report (data type `;`).
///
/// Objects represent entities that may not have their own radio —
/// hurricanes, marathon runners, event locations. They include a
/// name (9 chars), a live/killed flag, a timestamp, and a position.
///
/// Per User Manual Chapter 14: the TH-D75 can transmit Object
/// information via Menu No. 550 (Object 1-3).
#[derive(Debug, Clone, PartialEq)]
pub struct AprsObject {
    /// Object name (up to 9 characters).
    pub name: String,
    /// Whether the object is live (`true`) or killed (`false`).
    pub live: bool,
    /// DHM or HMS timestamp from the object report (7 characters).
    pub timestamp: String,
    /// Position data.
    pub position: AprsPosition,
}

/// An APRS item report (data type `)` ).
///
/// Items are similar to objects but simpler — no timestamp. They
/// represent static entities like event locations or landmarks.
#[derive(Debug, Clone, PartialEq)]
pub struct AprsItem {
    /// Item name (3-9 characters).
    pub name: String,
    /// Whether the item is live (`true`) or killed (`false`).
    pub live: bool,
    /// Position data.
    pub position: AprsPosition,
}

/// An APRS weather report.
///
/// Weather data can be embedded in a position report or sent as a
/// standalone positionless weather report (data type `_`). The TH-D75
/// displays weather station data in the station list.
///
/// All fields are optional — weather stations may report any subset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AprsWeather {
    /// Wind direction in degrees (0-360).
    pub wind_direction: Option<u16>,
    /// Wind speed in mph.
    pub wind_speed: Option<u16>,
    /// Wind gust in mph (peak in last 5 minutes).
    pub wind_gust: Option<u16>,
    /// Temperature in degrees Fahrenheit.
    pub temperature: Option<i16>,
    /// Rainfall in last hour (hundredths of an inch).
    pub rain_1h: Option<u16>,
    /// Rainfall in last 24 hours (hundredths of an inch).
    pub rain_24h: Option<u16>,
    /// Rainfall since midnight (hundredths of an inch).
    pub rain_since_midnight: Option<u16>,
    /// Humidity in percent (1-100). Raw APRS `00` is converted to 100.
    pub humidity: Option<u8>,
    /// Barometric pressure in tenths of millibars/hPa.
    pub pressure: Option<u32>,
}

/// Telemetry parameter definitions sent as APRS messages.
///
/// Per APRS 1.0.1 §13.2, a station that emits telemetry frames can send
/// four additional parameter-definition messages to tell receivers how
/// to interpret the analog and digital channels. These messages use the
/// standard APRS message format (`:ADDRESSEE:PARM.…`) with a well-known
/// keyword prefix.
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryDefinition {
    /// `PARM.P1,P2,P3,P4,P5,B1,B2,B3,B4,B5,B6,B7,B8` — human-readable
    /// names for 5 analog + 8 digital channels.
    Parameters(TelemetryParameters),
    /// `UNIT.U1,U2,U3,U4,U5,B1,B2,B3,B4,B5,B6,B7,B8` — unit labels.
    Units(TelemetryParameters),
    /// `EQNS.a1,b1,c1,a2,b2,c2,...` — calibration coefficients for the
    /// 5 analog channels (`y = a*x² + b*x + c`, 15 values total).
    Equations([Option<(f64, f64, f64)>; 5]),
    /// `BITS.b1b2b3b4b5b6b7b8,project_title` — active-bit mask plus
    /// project title.
    Bits {
        /// 8-character binary string specifying which digital bits are
        /// "active" (`'1'`) vs "inactive" (`'0'`).
        bits: String,
        /// Free-form project title (up to 23 characters).
        title: String,
    },
}

/// 5 analog + 8 digital channel labels used by both `PARM.` and `UNIT.`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TelemetryParameters {
    /// Analog channel labels (5 entries, `None` when omitted).
    pub analog: [Option<String>; 5],
    /// Digital channel labels (8 entries, `None` when omitted).
    pub digital: [Option<String>; 8],
}

impl TelemetryDefinition {
    /// Try to parse a telemetry parameter-definition message from the
    /// text portion of an [`AprsMessage`] (everything after the second
    /// `:` in the wire frame).
    ///
    /// Returns `None` when the text doesn't start with a known keyword.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let trimmed = text.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("PARM.") {
            return Some(Self::Parameters(parse_telemetry_labels(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("UNIT.") {
            return Some(Self::Units(parse_telemetry_labels(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("EQNS.") {
            return Some(Self::Equations(parse_telemetry_equations(rest)));
        }
        if let Some(rest) = trimmed.strip_prefix("BITS.") {
            let (bits, title) = rest.split_once(',').unwrap_or((rest, ""));
            return Some(Self::Bits {
                bits: bits.to_owned(),
                title: title.to_owned(),
            });
        }
        None
    }
}

/// Parse a comma-separated label list for `PARM.` / `UNIT.`.
fn parse_telemetry_labels(s: &str) -> TelemetryParameters {
    let mut params = TelemetryParameters::default();
    for (i, field) in s.split(',').enumerate() {
        let field = field.trim();
        if i < 5 {
            if !field.is_empty() {
                params.analog[i] = Some(field.to_owned());
            }
        } else if i < 13 {
            if !field.is_empty() {
                params.digital[i - 5] = Some(field.to_owned());
            }
        } else {
            break;
        }
    }
    params
}

/// Parse a `EQNS.` coefficient list into 5 `(a, b, c)` tuples.
fn parse_telemetry_equations(s: &str) -> [Option<(f64, f64, f64)>; 5] {
    let values: Vec<f64> = s
        .split(',')
        .map(str::trim)
        .map(|v| v.parse::<f64>().unwrap_or(0.0))
        .collect();
    let mut out: [Option<(f64, f64, f64)>; 5] = [None, None, None, None, None];
    for (i, slot) in out.iter_mut().enumerate() {
        let base = i * 3;
        if base + 2 < values.len() {
            *slot = Some((values[base], values[base + 1], values[base + 2]));
        }
    }
    out
}

/// Parsed APRS telemetry report.
///
/// Format: `T#seq,val1,val2,val3,val4,val5,dddddddd`
/// where vals are 0-999 analog values and d's are binary digits (8 bits).
///
/// Per APRS 1.0.1 Chapter 13, telemetry is used to transmit analog and
/// digital sensor readings. Up to 5 analog channels are supported; each
/// channel is stored as `Option<u16>` so callers can distinguish
/// "channel not reported" from "channel reported as 0".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsTelemetry {
    /// Telemetry sequence number (0-999 or "MIC").
    pub sequence: String,
    /// Analog values — exactly 5 channels per APRS 1.0.1 §13.1.
    /// Channels omitted from the wire frame are `None`.
    pub analog: [Option<u16>; 5],
    /// Digital value (8 bits).
    pub digital: u8,
}

/// Parsed APRS query.
///
/// Per APRS 1.0.1 Chapter 15, queries start with `?` and allow stations
/// to request information from other stations.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AprsQuery {
    /// Position query (`?APRSP` or `?APRS?`).
    Position,
    /// Status query (`?APRSS`).
    Status,
    /// Message query for a specific callsign (`?APRSM`).
    Message,
    /// Direction finding query (`?APRSD`).
    DirectionFinding,
    /// Weather query (`?WX`) — request latest weather fields.
    Weather,
    /// Telemetry query (`?APRST` or `?APRST?`).
    Telemetry,
    /// Ping query (`?PING?` or `?PING`).
    Ping,
    /// `IGate` query (`?IGATE?` or `?IGATE`).
    IGate,
    /// Stations-heard-on-RF query (`?APRSH`).
    Heard,
    /// General query with raw text (everything after the leading `?`,
    /// not one of the well-known forms).
    Other(String),
}

/// Build an APRS query frame to send to another station.
///
/// Composes an AX.25 UI frame with the `?APRS…` query format as an
/// addressed message: `:ADDRESSEE:?APRSP`. The addressee is padded to
/// exactly 9 characters. Supply the query via the [`AprsQuery`] enum.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
#[must_use]
pub fn build_aprs_query(
    source: &Ax25Address,
    addressee: &str,
    query: &AprsQuery,
    path: &[Ax25Address],
) -> Vec<u8> {
    let query_text = match query {
        AprsQuery::Position => "?APRSP".to_owned(),
        AprsQuery::Status => "?APRSS".to_owned(),
        AprsQuery::Message => "?APRSM".to_owned(),
        AprsQuery::DirectionFinding => "?APRSD".to_owned(),
        AprsQuery::Telemetry => "?APRST".to_owned(),
        AprsQuery::Heard => "?APRSH".to_owned(),
        AprsQuery::Weather => "?WX".to_owned(),
        AprsQuery::Ping => "?PING?".to_owned(),
        AprsQuery::IGate => "?IGATE?".to_owned(),
        AprsQuery::Other(s) => format!("?{s}"),
    };
    build_aprs_message(source, addressee, &query_text, None, path)
}

/// Build a position query response as a KISS-encoded APRS position report.
///
/// When a station receives a `?APRSP` or `?APRS?` query, it should respond
/// with its current position. This builds that response as a KISS frame
/// ready for transmission.
#[must_use]
pub fn build_query_response_position(
    source: &Ax25Address,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    // A query response is just a normal position report.
    build_aprs_position_report(source, lat, lon, symbol_table, symbol_code, comment, path)
}

/// Parse any APRS data frame from an AX.25 information field.
///
/// Dispatches based on the data type identifier (first byte) to the
/// appropriate parser. For Mic-E positions, use [`parse_mice_position`]
/// directly since it also requires the destination address.
///
/// **Prefer [`parse_aprs_data_full`] when the AX.25 destination address is
/// available** — it handles all data types including Mic-E.
///
/// # Supported data types
///
/// | Byte | Type | Parser |
/// |------|------|--------|
/// | `!`, `=` | Position (no timestamp) | [`parse_aprs_position`] |
/// | `/`, `@` | Position (with timestamp) | [`parse_aprs_position`] |
/// | `:` | Message | Inline |
/// | `>` | Status | Inline |
/// | `;` | Object | Inline |
/// | `)` | Item | Inline |
/// | `_` | Positionless weather | Inline |
/// | `` ` ``, `'` | Mic-E | Returns error (use [`parse_mice_position`]) |
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
pub fn parse_aprs_data(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.is_empty() {
        return Err(AprsError::InvalidFormat);
    }

    match info[0] {
        // Position reports (uncompressed and compressed)
        b'!' | b'=' | b'/' | b'@' => parse_aprs_position(info).map(AprsData::Position),
        // Message
        b':' => parse_aprs_message(info).map(AprsData::Message),
        // Status
        b'>' => parse_aprs_status(info).map(AprsData::Status),
        // Object
        b';' => parse_aprs_object(info).map(AprsData::Object),
        // Item
        b')' => parse_aprs_item(info).map(AprsData::Item),
        // Positionless weather
        b'_' => parse_aprs_weather_positionless(info).map(AprsData::Weather),
        // Telemetry
        b'T' => parse_aprs_telemetry(info).map(AprsData::Telemetry),
        // Query
        b'?' => parse_aprs_query(info).map(AprsData::Query),
        // Third-party traffic (APRS 1.0.1 §17): `}source>dest,path:payload`
        b'}' => parse_aprs_third_party(info),
        // Maidenhead grid locator (APRS 1.0.1 §5.6): `[EM13qc`
        b'[' => parse_aprs_grid(info),
        // Raw GPS / NMEA / Ultimeter (APRS 1.0.1 §5.2): `$GPRMC,...`
        b'$' => parse_aprs_raw_gps(info),
        // Station capabilities (APRS 1.0.1 §15.2): `<IGATE,MSG_CNT=10,LOC_CNT=0`
        b'<' => parse_aprs_capabilities(info),
        // Agrelo DFjr direction-finding data (APRS 1.0.1 §5.5): `%...`
        b'%' => Ok(AprsData::AgreloDfJr(info[1..].to_vec())),
        // User-defined data (APRS 1.0.1 §18): `{<expid><type><data>`
        b'{' => parse_aprs_user_defined(info),
        // Invalid/test data (APRS 1.0.1 §5.7): `,...`
        b',' => Ok(AprsData::InvalidOrTest(info[1..].to_vec())),
        // Mic-E (` ' 0x1C 0x1D) needs destination address — use parse_mice_position().
        b'`' | b'\'' | 0x1C | 0x1D => Err(AprsError::MicERequiresDestination),
        // All other types are unrecognized.
        _ => Err(AprsError::InvalidFormat),
    }
}

/// Parse an APRS third-party traffic frame (data type `}`).
///
/// Format: `}source>dest,path:payload`. The outer envelope identifies
/// the station that forwarded the packet, and the inner fields carry
/// the original packet exactly as it appeared on its origin transport
/// (typically APRS-IS).
fn parse_aprs_third_party(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.is_empty() || info[0] != b'}' {
        return Err(AprsError::InvalidFormat);
    }
    let body = &info[1..];
    let Some(colon) = body.iter().position(|&b| b == b':') else {
        return Err(AprsError::InvalidFormat);
    };
    let header = String::from_utf8_lossy(&body[..colon]).into_owned();
    let payload = body[colon + 1..].to_vec();
    Ok(AprsData::ThirdParty { header, payload })
}

/// Parse an APRS Maidenhead grid locator frame (data type `[`).
///
/// Format: `[<4-6 chars>`. The locator is left-padded / right-trimmed.
fn parse_aprs_grid(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.is_empty() || info[0] != b'[' {
        return Err(AprsError::InvalidFormat);
    }
    let body = String::from_utf8_lossy(&info[1..])
        .trim_end_matches(['\r', '\n', ' '])
        .to_owned();
    if !(4..=6).contains(&body.len()) {
        return Err(AprsError::InvalidFormat);
    }
    let bytes = body.as_bytes();
    // First two: letters A-R. Next two: digits 0-9. Last two (optional):
    // letters a-x.
    if !bytes[0].is_ascii_uppercase()
        || !bytes[1].is_ascii_uppercase()
        || !bytes[2].is_ascii_digit()
        || !bytes[3].is_ascii_digit()
    {
        return Err(AprsError::InvalidFormat);
    }
    if bytes[0] > b'R' || bytes[1] > b'R' {
        return Err(AprsError::InvalidFormat);
    }
    if bytes.len() == 6
        && (!bytes[4].is_ascii_lowercase()
            || !bytes[5].is_ascii_lowercase()
            || bytes[4] > b'x'
            || bytes[5] > b'x')
    {
        return Err(AprsError::InvalidFormat);
    }
    Ok(AprsData::Grid(body))
}

/// Parse an APRS raw GPS / NMEA frame (data type `$`).
///
/// Per APRS 1.0.1 §5.2, the frame is a full NMEA sentence including the
/// leading `$`. We preserve the body without the leading `$` (so the
/// caller still sees `GPRMC,...` etc.).
fn parse_aprs_raw_gps(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.is_empty() || info[0] != b'$' {
        return Err(AprsError::InvalidFormat);
    }
    let body = std::str::from_utf8(&info[1..])
        .map_err(|_| AprsError::InvalidFormat)?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    Ok(AprsData::RawGps(body))
}

/// Parse an APRS station capabilities frame (data type `<`).
///
/// Per APRS 1.0.1 §15.2, the body is a comma-separated list of tokens,
/// each of the form `KEY` (flag) or `KEY=value`. Whitespace around the
/// delimiters is not permitted in the spec but we trim it anyway for
/// tolerance.
fn parse_aprs_capabilities(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.is_empty() || info[0] != b'<' {
        return Err(AprsError::InvalidFormat);
    }
    let body = std::str::from_utf8(&info[1..])
        .map_err(|_| AprsError::InvalidFormat)?
        .trim_end_matches(['\r', '\n']);
    let mut tokens: Vec<(String, String)> = Vec::new();
    for entry in body.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        if let Some((k, v)) = entry.split_once('=') {
            tokens.push((k.trim().to_owned(), v.trim().to_owned()));
        } else {
            tokens.push((entry.to_owned(), String::new()));
        }
    }
    Ok(AprsData::StationCapabilities(tokens))
}

/// Parse an APRS user-defined frame (data type `{`).
///
/// Per APRS 1.0.1 §18, the frame is `{<experiment_id>[<type>]<data>`.
/// The experiment ID is the first character after `{`.
fn parse_aprs_user_defined(info: &[u8]) -> Result<AprsData, AprsError> {
    if info.len() < 2 || info[0] != b'{' {
        return Err(AprsError::InvalidFormat);
    }
    let experiment = info[1] as char;
    let data = info[2..].to_vec();
    Ok(AprsData::UserDefined { experiment, data })
}

/// Parse any APRS data frame, including Mic-E types that require the
/// AX.25 destination address.
///
/// This is the recommended entry point when the full [`Ax25Packet`] is
/// available. For Mic-E data type identifiers (`` ` ``, `'`, `0x1C`,
/// `0x1D`), the destination callsign is used to decode the latitude
/// via [`parse_mice_position`]. All other types delegate to
/// [`parse_aprs_data`].
///
/// # Errors
///
/// Returns [`AprsError`] if the format is unrecognized or data is invalid.
pub fn parse_aprs_data_full(info: &[u8], destination: &str) -> Result<AprsData, AprsError> {
    if info.is_empty() {
        return Err(AprsError::InvalidFormat);
    }

    match info[0] {
        // Mic-E current/old data types
        b'`' | b'\'' | 0x1C | 0x1D => {
            parse_mice_position(destination, info).map(AprsData::Position)
        }
        _ => parse_aprs_data(info),
    }
}

/// Parse an APRS message (`:ADDRESSEE:text{id`).
fn parse_aprs_message(info: &[u8]) -> Result<AprsMessage, AprsError> {
    // Minimum: : + 9 char addressee + : + at least 0 text = 11 bytes
    if info.len() < 11 || info[0] != b':' {
        return Err(AprsError::InvalidFormat);
    }

    // Addressee is exactly 9 characters (space-padded)
    let addressee_raw = &info[1..10];
    let addressee = String::from_utf8_lossy(addressee_raw).trim().to_string();

    if info[10] != b':' {
        return Err(AprsError::InvalidFormat);
    }

    let body = &info[11..];
    let body_str = String::from_utf8_lossy(body).into_owned();
    let trimmed_body = body_str.trim_end_matches(['\r', '\n']);

    // Split on `{` for message ID — but only if what follows is a valid
    // 1-5 character alphanumeric ID and nothing else. Naive `rfind('{')`
    // would misinterpret legitimate message text like "reply {to}" as
    // having a message ID of "to}".
    // Split on `{` for message ID and APRS 1.2 reply-ack extension.
    // Three possible trailer forms to recognise (checked from richest):
    //   1. `text{MM}AA`  — reply-ack: MM is this msg's id, AA is ack
    //   2. `text{MM`     — plain message id
    //   3. `text`        — no trailer
    let (text, message_id, reply_ack) = parse_message_trailer(trimmed_body);

    Ok(AprsMessage {
        addressee,
        text,
        message_id,
        reply_ack,
    })
}

/// Parse the optional `{MM}AA` / `{MM` trailer of an APRS message body.
///
/// Returns `(text, message_id, reply_ack)` where the latter two are
/// `Some` only when the trailer has the exact well-formed shape.
fn parse_message_trailer(body: &str) -> (String, Option<String>, Option<String>) {
    let Some(brace_idx) = body.rfind('{') else {
        return (body.to_owned(), None, None);
    };
    let after_brace = &body[brace_idx + 1..];

    // APRS 1.2 reply-ack: `MM}AA` where MM is 1-5 alnum and AA is 1-5 alnum.
    if let Some(close_idx) = after_brace.find('}') {
        let mm = &after_brace[..close_idx];
        let aa = &after_brace[close_idx + 1..];
        if (1..=5).contains(&mm.len())
            && mm.bytes().all(|b| b.is_ascii_alphanumeric())
            && (1..=5).contains(&aa.len())
            && aa.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return (
                body[..brace_idx].to_owned(),
                Some(mm.to_owned()),
                Some(aa.to_owned()),
            );
        }
    }

    // Plain message id: `MM` (1-5 alnum, end of string).
    if (1..=5).contains(&after_brace.len())
        && after_brace.bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return (
            body[..brace_idx].to_owned(),
            Some(after_brace.to_owned()),
            None,
        );
    }

    // Neither pattern matched — treat whole body as plain text.
    (body.to_owned(), None, None)
}

/// Parse an APRS status report (`>text`).
fn parse_aprs_status(info: &[u8]) -> Result<AprsStatus, AprsError> {
    if info.is_empty() || info[0] != b'>' {
        return Err(AprsError::InvalidFormat);
    }
    let text = String::from_utf8_lossy(&info[1..]).trim().to_string();
    Ok(AprsStatus { text })
}

/// Parse an APRS object report (`;name_____*DDHHMMzpos...`).
fn parse_aprs_object(info: &[u8]) -> Result<AprsObject, AprsError> {
    // ; + 9-char name + * or _ + 7-char timestamp + position
    if info.len() < 27 || info[0] != b';' {
        return Err(AprsError::InvalidFormat);
    }

    let name = String::from_utf8_lossy(&info[1..10]).trim().to_string();
    let live = match info[10] {
        b'*' => true,
        b'_' => false,
        _ => return Err(AprsError::InvalidFormat),
    };

    // After the name and live/killed flag, there's a 7-char timestamp
    // then position data. Build a synthetic position info field.
    let pos_body = &info[11..];
    // Timestamp is 7 chars, then position data follows
    if pos_body.len() < 7 {
        return Err(AprsError::InvalidFormat);
    }
    let timestamp = String::from_utf8_lossy(&pos_body[..7]).to_string();
    let pos_data = &pos_body[7..];

    // Detect compressed vs uncompressed
    let position = if pos_data.is_empty() {
        return Err(AprsError::InvalidFormat);
    } else if pos_data[0].is_ascii_digit() {
        parse_uncompressed_body(pos_data)?
    } else {
        parse_compressed_body(pos_data)?
    };

    Ok(AprsObject {
        name,
        live,
        timestamp,
        position,
    })
}

/// Parse an APRS item report (`)name!pos...` or `)name_pos...`).
///
/// Per APRS 1.0.1 Chapter 11, the name is 3-9 characters terminated by
/// `!` (live) or `_` (killed). The name is restricted to printable
/// ASCII excluding the terminator characters themselves.
fn parse_aprs_item(info: &[u8]) -> Result<AprsItem, AprsError> {
    if info.len() < 2 || info[0] != b')' {
        return Err(AprsError::InvalidFormat);
    }
    let body = &info[1..];

    // Scan the first 9 bytes for a terminator. Anything beyond that is
    // outside the spec-legal range and the frame is malformed.
    let search_len = std::cmp::min(body.len(), 9);
    let terminator_pos = body[..search_len]
        .iter()
        .position(|&b| b == b'!' || b == b'_')
        .ok_or(AprsError::InvalidFormat)?;

    // Names are 3-9 characters inclusive.
    if terminator_pos < 3 {
        return Err(AprsError::InvalidFormat);
    }

    let live = body[terminator_pos] == b'!';
    let name_bytes = &body[..terminator_pos];
    // Reject non-printable ASCII in names.
    if name_bytes.iter().any(|&b| !(0x20..=0x7E).contains(&b)) {
        return Err(AprsError::InvalidFormat);
    }
    let name = std::str::from_utf8(name_bytes)
        .map_err(|_| AprsError::InvalidFormat)?
        .to_owned();
    let pos_data = &body[terminator_pos + 1..];

    if pos_data.is_empty() {
        return Err(AprsError::InvalidFormat);
    }

    let position = if pos_data[0].is_ascii_digit() {
        parse_uncompressed_body(pos_data)?
    } else {
        parse_compressed_body(pos_data)?
    };

    Ok(AprsItem {
        name,
        live,
        position,
    })
}

/// Try to extract weather data embedded in a position report's comment.
///
/// Per APRS 1.0.1 §12.1, a "complete weather report" is a position report
/// with symbol code `_` (weather station) whose comment begins with the
/// CSE/SPD extension format `DDD/SSS` encoding wind direction and speed,
/// followed by the remaining weather fields (`gGGG tTTT rRRR …`) in the
/// standard order.
///
/// Returns `None` if the symbol is not `_` or the comment does not start
/// with a valid `DDD/SSS` extension.
fn extract_position_weather(symbol_code: char, comment: &str) -> Option<AprsWeather> {
    if symbol_code != '_' {
        return None;
    }
    let bytes = comment.as_bytes();
    if bytes.len() < 7 || bytes[3] != b'/' {
        return None;
    }
    if !bytes[..3].iter().all(u8::is_ascii_digit) || !bytes[4..7].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let wind_dir: u16 = comment.get(..3)?.parse().ok()?;
    let wind_spd: u16 = comment.get(4..7)?.parse().ok()?;
    let mut wx = parse_weather_fields(&bytes[7..]);
    wx.wind_direction = Some(wind_dir);
    wx.wind_speed = Some(wind_spd);
    Some(wx)
}

/// Parse a positionless APRS weather report (`_MMDDHHMMdata`).
///
/// Weather data uses single-letter field tags followed by fixed-width
/// numeric values. Common fields:
/// - `c` = wind direction (3 digits, degrees)
/// - `s` = wind speed (3 digits, mph)
/// - `g` = gust (3 digits, mph)
/// - `t` = temperature (3 digits, Fahrenheit, may be negative)
/// - `r` = rain last hour (3 digits, hundredths of inch)
/// - `p` = rain last 24h (3 digits, hundredths of inch)
/// - `P` = rain since midnight (3 digits, hundredths of inch)
/// - `h` = humidity (2 digits, 00=100%)
/// - `b` = barometric pressure (5 digits, tenths of mbar)
fn parse_aprs_weather_positionless(info: &[u8]) -> Result<AprsWeather, AprsError> {
    if info.is_empty() || info[0] != b'_' {
        return Err(AprsError::InvalidFormat);
    }
    // Skip _ and 8-char timestamp (MMDDHHMM)
    let data = if info.len() > 9 { &info[9..] } else { &[] };
    Ok(parse_weather_fields(data))
}

/// Parse APRS weather data fields from a byte slice.
///
/// Per APRS 1.0.1 §12.2, weather fields are a contiguous sequence of
/// `<tag><value>` pairs in a **fixed order** (wind direction, wind speed,
/// gust, temperature, rain 1h, rain 24h, rain since midnight, humidity,
/// pressure, luminosity). Each field is optional and, if present, uses a
/// fixed-width decimal value. A value of all dots or spaces means the
/// station has no data for that field.
///
/// The parser walks the buffer from the start, consumes a known tag +
/// value pair, and advances. It stops on the first unknown byte, leaving
/// any trailing comment / station-type suffix alone.
///
/// This is strictly more correct than a `find()`-based scan, which would
/// false-match tag letters appearing inside comment text (e.g. `"canada"`
/// matching `c` for wind direction).
fn parse_weather_fields(data: &[u8]) -> AprsWeather {
    let mut wx = AprsWeather::default();
    let mut i = 0;
    while i < data.len() {
        let tag = data[i];
        let width = match tag {
            b'c' | b's' | b'g' | b't' | b'r' | b'p' | b'P' | b'L' | b'l' => 3,
            b'h' => 2,
            b'b' => 5,
            // Unknown byte — assume start of comment / type suffix.
            _ => break,
        };
        if i + 1 + width > data.len() {
            break;
        }
        let val_bytes = &data[i + 1..=i + width];
        let parsed_i32 = parse_weather_value(val_bytes);
        match tag {
            b'c' => {
                // Wind direction: 000 is the "true North / no data"
                // convention; most stations encode 360 as 000.
                wx.wind_direction = parsed_i32.and_then(convert_u16);
            }
            b's' => wx.wind_speed = parsed_i32.and_then(convert_u16),
            b'g' => wx.wind_gust = parsed_i32.and_then(convert_u16),
            b't' => wx.temperature = parsed_i32.and_then(convert_i16),
            b'r' => wx.rain_1h = parsed_i32.and_then(convert_u16),
            b'p' => wx.rain_24h = parsed_i32.and_then(convert_u16),
            b'P' => wx.rain_since_midnight = parsed_i32.and_then(convert_u16),
            b'h' => {
                // APRS encodes humidity 100% as "00".
                wx.humidity = parsed_i32.and_then(|v| {
                    if v == 0 {
                        Some(100)
                    } else {
                        u8::try_from(v).ok()
                    }
                });
            }
            b'b' => wx.pressure = parsed_i32.and_then(|v| u32::try_from(v).ok()),
            // Luminosity (L/l): not yet represented in AprsWeather.
            b'L' | b'l' => {}
            _ => unreachable!(),
        }
        i += 1 + width;
    }
    wx
}

/// Parse a fixed-width weather field value. Returns `None` if the bytes
/// are a "no data" placeholder (dots or spaces) or unparseable.
fn parse_weather_value(bytes: &[u8]) -> Option<i32> {
    if bytes.iter().all(|&b| b == b'.' || b == b' ') {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;
    s.trim().parse().ok()
}

/// Lossless widening from `i32` to `u16` for weather values.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
const fn convert_u16(v: i32) -> Option<u16> {
    if v < 0 || v > u16::MAX as i32 {
        None
    } else {
        Some(v as u16)
    }
}

/// Lossless widening from `i32` to `i16` for signed weather values.
#[allow(clippy::cast_possible_truncation)]
const fn convert_i16(v: i32) -> Option<i16> {
    if v < i16::MIN as i32 || v > i16::MAX as i32 {
        None
    } else {
        Some(v as i16)
    }
}

// ---------------------------------------------------------------------------
// APRS Telemetry parser
// ---------------------------------------------------------------------------

/// Parse an APRS telemetry report (`T#seq,v1,v2,v3,v4,v5,dddddddd`).
///
/// Per APRS 1.0.1 §13.1 a telemetry frame has exactly 5 analog channels
/// and 1 digital channel. We tolerate fewer analog channels (missing
/// channels become `None`) but reject frames with more fields than the
/// spec allows — those are almost certainly malformed.
fn parse_aprs_telemetry(info: &[u8]) -> Result<AprsTelemetry, AprsError> {
    // Minimum: T#seq,v (at least 5 bytes)
    if info.len() < 4 || info[0] != b'T' || info[1] != b'#' {
        return Err(AprsError::InvalidFormat);
    }

    let body = String::from_utf8_lossy(&info[2..]);
    let parts: Vec<&str> = body.split(',').collect();
    if parts.is_empty() {
        return Err(AprsError::InvalidFormat);
    }
    // Spec limit: sequence + 5 analog + 1 digital = 7 fields max.
    if parts.len() > 7 {
        return Err(AprsError::InvalidFormat);
    }

    let sequence = parts[0].to_owned();

    // Parse analog values into a fixed-size [Option<u16>; 5].
    let mut analog: [Option<u16>; 5] = [None, None, None, None, None];
    let analog_end = std::cmp::min(parts.len(), 6); // indices 1..=5
    for (i, part) in parts[1..analog_end].iter().enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val: u16 = trimmed.parse().map_err(|_| AprsError::InvalidFormat)?;
        analog[i] = Some(val);
    }

    // Parse digital value (8 binary digits) if present. Per APRS 1.0.1
    // §13.1, the field is exactly 8 binary digits; malformed input is a
    // parse error, not a silent zero.
    let digital = if parts.len() > 6 {
        let digi_str = parts[6].trim();
        let digi_bits = if digi_str.len() >= 8 {
            &digi_str[..8]
        } else {
            digi_str
        };
        u8::from_str_radix(digi_bits, 2).map_err(|_| AprsError::InvalidFormat)?
    } else {
        0
    };

    Ok(AprsTelemetry {
        sequence,
        analog,
        digital,
    })
}

// ---------------------------------------------------------------------------
// APRS Query parser
// ---------------------------------------------------------------------------

/// Parse an APRS query (`?APRSx` or `?text`).
fn parse_aprs_query(info: &[u8]) -> Result<AprsQuery, AprsError> {
    if info.is_empty() || info[0] != b'?' {
        return Err(AprsError::InvalidFormat);
    }

    let body = String::from_utf8_lossy(&info[1..]);
    let text = body.trim_end_matches('\r');

    // Standard APRS queries per APRS 1.0.1 Chapter 15.
    match text {
        "APRSP" | "APRS?" => Ok(AprsQuery::Position),
        "APRSS" => Ok(AprsQuery::Status),
        "APRSM" => Ok(AprsQuery::Message),
        "APRSD" => Ok(AprsQuery::DirectionFinding),
        "APRST" | "APRST?" => Ok(AprsQuery::Telemetry),
        "APRSH" => Ok(AprsQuery::Heard),
        "WX" => Ok(AprsQuery::Weather),
        "PING" | "PING?" => Ok(AprsQuery::Ping),
        "IGATE" | "IGATE?" => Ok(AprsQuery::IGate),
        _ => Ok(AprsQuery::Other(text.to_owned())),
    }
}

// ---------------------------------------------------------------------------
// APRS TX packet builders
// ---------------------------------------------------------------------------

/// APRS tocall for the Kenwood TH-D75 (per APRS tocall registry).
const APRS_TOCALL: &str = "APK005";

impl Ax25Packet {
    /// Build a minimal APRS UI frame with the given source, destination,
    /// path, and info field. Control = 0x03, PID = 0xF0.
    #[must_use]
    pub const fn ui_frame(
        source: Ax25Address,
        destination: Ax25Address,
        path: Vec<Ax25Address>,
        info: Vec<u8>,
    ) -> Self {
        Self {
            source,
            destination,
            digipeaters: path,
            control: 0x03,
            protocol: 0xF0,
            info,
        }
    }

    /// Encode this packet as a KISS-framed data frame ready for the
    /// wire. Equivalent to wrapping [`build_ax25`] in
    /// [`encode_kiss_frame`] with `port = 0` and `command = Data`.
    #[must_use]
    pub fn encode_kiss(&self) -> Vec<u8> {
        let ax25_bytes = build_ax25(self);
        encode_kiss_frame(&KissFrame::data(ax25_bytes))
    }
}

/// Default APRS digipeater path: WIDE1-1, WIDE2-1.
const DEFAULT_DIGIPEATERS: &[(&str, u8)] = &[("WIDE1", 1), ("WIDE2", 1)];

/// Parse a digipeater path string like `"WIDE1-1,WIDE2-2"` into addresses.
///
/// Accepts comma-separated entries, each of the form `CALLSIGN[-SSID]`.
/// Whitespace around entries is trimmed. An empty string returns an empty
/// path (direct transmission with no digipeating).
///
/// # Errors
///
/// Returns [`AprsError::InvalidPath`] if any entry has an SSID that is
/// not a valid 0-15 integer, or if the callsign is empty or longer than
/// 6 characters.
///
/// # Examples
///
/// ```
/// use kenwood_thd75::kiss::parse_digipeater_path;
/// let path = parse_digipeater_path("WIDE1-1,WIDE2-2").unwrap();
/// assert_eq!(path.len(), 2);
/// assert_eq!(path[0].callsign, "WIDE1");
/// assert_eq!(path[0].ssid, 1);
/// ```
pub fn parse_digipeater_path(s: &str) -> Result<Vec<Ax25Address>, AprsError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in trimmed.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err(AprsError::InvalidPath(s.to_owned()));
        }
        let (callsign, ssid) = if let Some((call, ssid_str)) = entry.split_once('-') {
            let ssid: u8 = ssid_str
                .parse()
                .map_err(|_| AprsError::InvalidPath(s.to_owned()))?;
            if ssid > 15 {
                return Err(AprsError::InvalidPath(s.to_owned()));
            }
            (call, ssid)
        } else {
            (entry, 0)
        };
        if callsign.is_empty() || callsign.len() > 6 {
            return Err(AprsError::InvalidPath(s.to_owned()));
        }
        result.push(Ax25Address::new(callsign, ssid));
    }
    Ok(result)
}

/// Format latitude as APRS uncompressed `DDMM.HHN` (8 bytes).
///
/// Clamps out-of-range or non-finite input to `±90.0` so the output is
/// always a well-formed 8-byte APRS latitude field instead of garbage
/// like `"950000.00N"`.
fn format_aprs_latitude(lat: f64) -> String {
    let lat = if lat.is_finite() {
        lat.clamp(-90.0, 90.0)
    } else {
        0.0
    };
    let hemisphere = if lat >= 0.0 { 'N' } else { 'S' };
    let lat_abs = lat.abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let degrees = lat_abs as u32;
    let minutes = (lat_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:02}{minutes:05.2}{hemisphere}")
}

/// Format longitude as APRS uncompressed `DDDMM.HHE` (9 bytes).
///
/// Clamps out-of-range or non-finite input to `±180.0`.
fn format_aprs_longitude(lon: f64) -> String {
    let lon = if lon.is_finite() {
        lon.clamp(-180.0, 180.0)
    } else {
        0.0
    };
    let hemisphere = if lon >= 0.0 { 'E' } else { 'W' };
    let lon_abs = lon.abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let degrees = lon_abs as u32;
    let minutes = (lon_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:03}{minutes:05.2}{hemisphere}")
}

/// Build the default digipeater path as [`Ax25Address`] entries.
#[must_use]
pub fn default_digipeater_path() -> Vec<Ax25Address> {
    DEFAULT_DIGIPEATERS
        .iter()
        .map(|(call, ssid)| Ax25Address::new(call, *ssid))
        .collect()
}

/// Build a KISS-encoded APRS uncompressed position report.
///
/// Composes an AX.25 UI frame with:
/// - Destination: `APK005-0` (Kenwood TH-D75 tocall)
/// - Digipeater path: WIDE1-1, WIDE2-1
/// - Info field: `!DDMM.HHN/DDDMM.HHEscomment`
///
/// Returns wire-ready bytes (FEND-delimited KISS frame) suitable for
/// [`KissSession::send_data`](crate::radio::kiss_session::KissSession::send_data)
/// or direct transport write.
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car, `-` for house).
/// - `comment`: Free-form comment text appended after the position.
/// - `path`: Digipeater path. Use [`default_digipeater_path`] for the
///   standard `WIDE1-1,WIDE2-1` path, or an empty slice for direct.
#[must_use]
pub fn build_aprs_position_report(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_position_report_packet(
        source,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
    .encode_kiss()
}

/// Like [`build_aprs_position_report`] but returns the unencoded
/// [`Ax25Packet`] so callers can inspect, log, or route it before
/// wrapping it in KISS framing.
#[must_use]
pub fn build_aprs_position_report_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let info = format!("!{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Build a KISS-encoded APRS message packet.
///
/// Composes an AX.25 UI frame with the APRS message format:
/// `:ADDRESSEE:text{ID`
///
/// The addressee is padded to exactly 9 characters per the APRS spec.
/// Message text that exceeds [`MAX_APRS_MESSAGE_TEXT_LEN`] (67 bytes) is
/// **truncated** — use [`build_aprs_message_checked`] if you want a
/// hard error on overlong input.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `addressee`: Destination station callsign (up to 9 chars).
/// - `text`: Message text content.
/// - `message_id`: Optional message sequence number for ack/rej tracking.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_message(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_message_packet(source, addressee, text, message_id, path).encode_kiss()
}

/// Like [`build_aprs_message`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_message_packet(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[Ax25Address],
) -> Ax25Packet {
    // Pad addressee to exactly 9 characters.
    let padded_addressee = format!("{addressee:<9}");
    let padded_addressee = &padded_addressee[..9];

    // Truncate text to the spec limit on a UTF-8 char boundary.
    let text = if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
        let mut end = MAX_APRS_MESSAGE_TEXT_LEN;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };

    let info = message_id.map_or_else(
        || format!(":{padded_addressee}:{text}"),
        |id| format!(":{padded_addressee}:{text}{{{id}"),
    );

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Like [`build_aprs_message`] but returns an error when the text
/// exceeds the APRS 1.0.1 67-byte limit instead of silently truncating.
///
/// # Errors
///
/// Returns [`AprsError::MessageTooLong`] if `text.len() > 67`.
pub fn build_aprs_message_checked(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
    path: &[Ax25Address],
) -> Result<Vec<u8>, AprsError> {
    if text.len() > MAX_APRS_MESSAGE_TEXT_LEN {
        return Err(AprsError::MessageTooLong(text.len()));
    }
    Ok(build_aprs_message(
        source, addressee, text, message_id, path,
    ))
}

/// Build a KISS-encoded APRS object report.
///
/// Composes an AX.25 UI frame with the APRS object format:
/// `;name_____*DDHHMMzDDMM.HHN/DDDMM.HHEscomment`
///
/// The object name is padded to exactly 9 characters per the APRS spec.
/// The timestamp uses the current UTC time in DHM zulu format.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `name`: Object name (up to 9 characters).
/// - `live`: `true` for a live object (`*`), `false` for killed (`_`).
/// - `latitude`: Decimal degrees, positive = North.
/// - `longitude`: Decimal degrees, positive = East.
/// - `symbol_table`: APRS symbol table character.
/// - `symbol_code`: APRS symbol code character.
/// - `comment`: Free-form comment text.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_object(
    source: &Ax25Address,
    name: &str,
    live: bool,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    // Use a placeholder DHM zulu timestamp `000000z`. Callers needing a
    // real timestamp should use [`build_aprs_object_with_timestamp`].
    build_aprs_object_with_timestamp(
        source,
        name,
        live,
        AprsTimestamp::DhmZulu {
            day: 0,
            hour: 0,
            minute: 0,
        },
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
}

/// Build a KISS-encoded APRS object report with a caller-supplied
/// timestamp.
///
/// Identical to [`build_aprs_object`] but uses the provided
/// [`AprsTimestamp`] instead of the `000000z` placeholder.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_aprs_object_with_timestamp(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_object_with_timestamp_packet(
        source,
        name,
        live,
        timestamp,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
    .encode_kiss()
}

/// Like [`build_aprs_object_with_timestamp`] but returns the unencoded
/// [`Ax25Packet`] for callers that want to inspect or route it.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_aprs_object_with_timestamp_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    timestamp: AprsTimestamp,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let padded_name = format!("{name:<9}");
    let padded_name = &padded_name[..9];
    let live_char = if live { '*' } else { '_' };
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let ts = timestamp.to_wire_string();

    let info = format!(
        ";{padded_name}{live_char}{ts}{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}"
    );

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Build a KISS-encoded APRS item report.
///
/// Composes an AX.25 UI frame with the APRS item format:
/// `)name!DDMM.HHN/DDDMM.HHEscomment` (live) or
/// `)name_DDMM.HHN/DDDMM.HHEscomment` (killed).
///
/// The item name must be 3-9 characters per APRS101 Chapter 11.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `name`: Item name (3-9 characters).
/// - `live`: `true` for a live item (`!`), `false` for killed (`_`).
/// - `lat`: Decimal degrees, positive = North.
/// - `lon`: Decimal degrees, positive = East.
/// - `symbol_table`: APRS symbol table character.
/// - `symbol_code`: APRS symbol code character.
/// - `comment`: Free-form comment text.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_item(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_item_packet(
        source,
        name,
        live,
        lat,
        lon,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
    .encode_kiss()
}

/// Like [`build_aprs_item`] but returns the unencoded [`Ax25Packet`].
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn build_aprs_item_packet(
    source: &Ax25Address,
    name: &str,
    live: bool,
    lat: f64,
    lon: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let live_char = if live { '!' } else { '_' };
    let lat_str = format_aprs_latitude(lat);
    let lon_str = format_aprs_longitude(lon);
    let info = format!("){name}{live_char}{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");
    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Build a KISS-encoded positionless APRS weather report.
///
/// Composes an AX.25 UI frame with the APRS positionless weather format:
/// `_MMDDHHMMcSSSsSSS gSSS tTTT rRRR pRRR PRRR hHH bBBBBB`
///
/// Uses a placeholder timestamp (`00000000`). Callers needing a real
/// timestamp should build the info field manually.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `weather`: Weather data to encode. Missing fields are omitted.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_weather(
    source: &Ax25Address,
    weather: &AprsWeather,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_weather_packet(source, weather, path).encode_kiss()
}

/// Build a combined APRS position + weather report as a single KISS
/// frame, per APRS 1.0.1 §12.1.
///
/// Uses the uncompressed position format with symbol code `_` (weather
/// station), followed by the `DDD/SSS` CSE/SPD wind direction/speed
/// extension, then the remaining weather fields. This is the "complete
/// weather report" wire form used by most fixed weather stations.
#[must_use]
pub fn build_aprs_position_weather(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    weather: &AprsWeather,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_position_weather_packet(source, latitude, longitude, symbol_table, weather, path)
        .encode_kiss()
}

/// Like [`build_aprs_position_weather`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
pub fn build_aprs_position_weather_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    weather: &AprsWeather,
    path: &[Ax25Address],
) -> Ax25Packet {
    use std::fmt::Write as _;

    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    // Symbol code is always `_` (weather station) for this format.
    // Wind direction and speed go into the CSE/SPD slot (`DDD/SSS`),
    // with "..." for missing values.
    let wind_dir = weather
        .wind_direction
        .map_or_else(|| "...".to_owned(), |d| format!("{d:03}"));
    let wind_spd = weather
        .wind_speed
        .map_or_else(|| "...".to_owned(), |s| format!("{s:03}"));

    let mut info = format!("!{lat_str}{symbol_table}{lon_str}_{wind_dir}/{wind_spd}");
    if let Some(gust) = weather.wind_gust {
        let _ = write!(info, "g{gust:03}");
    }
    if let Some(temp) = weather.temperature {
        let _ = write!(info, "t{temp:03}");
    }
    if let Some(rain) = weather.rain_1h {
        let _ = write!(info, "r{rain:03}");
    }
    if let Some(rain) = weather.rain_24h {
        let _ = write!(info, "p{rain:03}");
    }
    if let Some(rain) = weather.rain_since_midnight {
        let _ = write!(info, "P{rain:03}");
    }
    if let Some(hum) = weather.humidity {
        let hum_val = if hum == 100 { 0 } else { hum };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pres) = weather.pressure {
        let _ = write!(info, "b{pres:05}");
    }

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

/// Like [`build_aprs_weather`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_weather_packet(
    source: &Ax25Address,
    weather: &AprsWeather,
    path: &[Ax25Address],
) -> Ax25Packet {
    use std::fmt::Write as _;

    let mut info = String::from("_00000000");

    if let Some(dir) = weather.wind_direction {
        let _ = write!(info, "c{dir:03}");
    }
    if let Some(spd) = weather.wind_speed {
        let _ = write!(info, "s{spd:03}");
    }
    if let Some(gust) = weather.wind_gust {
        let _ = write!(info, "g{gust:03}");
    }
    if let Some(temp) = weather.temperature {
        let _ = write!(info, "t{temp:03}");
    }
    if let Some(rain) = weather.rain_1h {
        let _ = write!(info, "r{rain:03}");
    }
    if let Some(rain) = weather.rain_24h {
        let _ = write!(info, "p{rain:03}");
    }
    if let Some(rain) = weather.rain_since_midnight {
        let _ = write!(info, "P{rain:03}");
    }
    if let Some(hum) = weather.humidity {
        let hum_val = if hum == 100 { 0 } else { hum };
        let _ = write!(info, "h{hum_val:02}");
    }
    if let Some(pres) = weather.pressure {
        let _ = write!(info, "b{pres:05}");
    }

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info.into_bytes(),
    )
}

// ---------------------------------------------------------------------------
// APRS compressed position TX builder
// ---------------------------------------------------------------------------

/// Encode a `u32` value as 4 bytes of base-91.
///
/// Base-91 encoding uses characters 33 (`!`) through 123 (`{`), giving
/// 91 possible values per byte. Four bytes can represent values up to
/// 91^4 - 1 = 68,574,960.
fn encode_base91_4(mut value: u32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for i in (0..4).rev() {
        #[allow(clippy::cast_possible_truncation)]
        let digit = (value % 91) as u8;
        out[i] = digit + 33;
        value /= 91;
    }
    out
}

/// Build a KISS-encoded APRS compressed position report.
///
/// Compressed format uses base-91 encoding for latitude and longitude,
/// producing smaller packets than the uncompressed `DDMM.HH` format.
/// Encoding follows APRS101 Chapter 9.
///
/// The compressed body is 13 bytes:
/// `sym_table(1) YYYY(4) XXXX(4) sym_code(1) cs(1) s(1) t(1)`
///
/// Where `cs`, `s`, and `t` are set to indicate no course/speed/altitude
/// data (space characters).
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car, `-` for house).
/// - `comment`: Free-form comment text appended after the compressed position.
/// - `path`: Digipeater path.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn build_aprs_position_compressed(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_position_compressed_packet(
        source,
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
    .encode_kiss()
}

/// Like [`build_aprs_position_compressed`] but returns the unencoded
/// [`Ax25Packet`].
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn build_aprs_position_compressed_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let lat_val = (380_926.0 * (90.0 - latitude)) as u32;
    let lon_val = (190_463.0 * (longitude + 180.0)) as u32;
    let lat_encoded = encode_base91_4(lat_val);
    let lon_encoded = encode_base91_4(lon_val);

    let mut info = Vec::with_capacity(1 + 13 + comment.len());
    info.push(b'!');
    info.push(symbol_table as u8);
    info.extend_from_slice(&lat_encoded);
    info.extend_from_slice(&lon_encoded);
    info.push(symbol_code as u8);
    info.push(b' '); // cs: no course/speed data
    info.push(b' ');
    info.push(b' '); // t: compression type = no data
    info.extend_from_slice(comment.as_bytes());

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info,
    )
}

// ---------------------------------------------------------------------------
// APRS status TX builder
// ---------------------------------------------------------------------------

/// Build a KISS-encoded APRS status report.
///
/// Composes an AX.25 UI frame with the APRS status format:
/// `>text\r`
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `text`: Status text content.
/// - `path`: Digipeater path.
#[must_use]
pub fn build_aprs_status(source: &Ax25Address, text: &str, path: &[Ax25Address]) -> Vec<u8> {
    build_aprs_status_packet(source, text, path).encode_kiss()
}

/// Like [`build_aprs_status`] but returns the unencoded [`Ax25Packet`].
#[must_use]
pub fn build_aprs_status_packet(
    source: &Ax25Address,
    text: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    let mut info = Vec::with_capacity(1 + text.len() + 1);
    info.push(b'>');
    info.extend_from_slice(text.as_bytes());
    info.push(b'\r');
    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(APRS_TOCALL, 0),
        path.to_vec(),
        info,
    )
}

// ---------------------------------------------------------------------------
// APRS data extensions parser (APRS101 Chapters 6-7)
// ---------------------------------------------------------------------------

/// Parsed APRS data extensions from the position comment field.
///
/// Position reports can carry structured data in the comment string
/// after the coordinates. This struct captures the extensions defined
/// in APRS101 Chapters 6-7.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AprsDataExtension {
    /// Course in degrees (0-360) and speed in knots, from CSE/SPD.
    pub course_speed: Option<(u16, u16)>,
    /// Power, Height, Gain, Directivity (PHG).
    pub phg: Option<Phg>,
    /// Altitude in feet (from `/A=NNNNNN` in comment).
    pub altitude_ft: Option<i32>,
    /// DAO precision extension (`!DAO!` for extra lat/lon digits).
    pub dao: Option<(f64, f64)>,
}

/// Power-Height-Gain-Directivity data (APRS101 Chapter 7).
///
/// PHG provides station RF characteristics for range circle calculations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phg {
    /// Effective radiated power in watts.
    pub power_watts: u32,
    /// Antenna height above average terrain in feet.
    pub height_feet: u32,
    /// Antenna gain in dB.
    pub gain_db: u8,
    /// Antenna directivity in degrees (0 = omni).
    pub directivity_deg: u16,
}

/// Parse data extensions from an APRS position comment string.
///
/// Extracts CSE/SPD, PHG, altitude (`/A=NNNNNN`), and DAO (`!DAO!`)
/// extensions per APRS101 Chapters 6-7.
///
/// # Parameters
///
/// - `comment`: The comment string after the APRS position fields.
///
/// # Returns
///
/// An [`AprsDataExtension`] with each field populated if found.
#[must_use]
pub fn parse_aprs_extensions(comment: &str) -> AprsDataExtension {
    let course_speed = parse_cse_spd(comment);
    let phg = parse_phg(comment);
    let altitude_ft = parse_altitude(comment);
    let dao = parse_dao(comment);

    AprsDataExtension {
        course_speed,
        phg,
        altitude_ft,
        dao,
    }
}

/// Parse CSE/SPD from the first 7 characters of the comment.
///
/// Format: `DDD/SSS` where DDD is 3-digit course (000-360) and SSS is
/// 3-digit speed in knots. Per APRS101 Chapter 7, this must be at the
/// start of the comment and use the exact `NNN/NNN` format.
fn parse_cse_spd(comment: &str) -> Option<(u16, u16)> {
    let bytes = comment.as_bytes();
    if bytes.len() < 7 {
        return None;
    }
    // Must be DDD/SSS at position 0.
    if bytes[3] != b'/' {
        return None;
    }
    // All digits check.
    if !bytes[0..3].iter().all(u8::is_ascii_digit) || !bytes[4..7].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let course: u16 = comment[0..3].parse().ok()?;
    let speed: u16 = comment[4..7].parse().ok()?;
    if course > 360 {
        return None;
    }
    Some((course, speed))
}

/// PHG power codes: index^2 watts. Per APRS101 Table on p.28.
const PHG_POWER: [u32; 10] = [0, 1, 4, 9, 16, 25, 36, 49, 64, 81];
/// PHG height codes: 10 * 2^N feet.
const PHG_HEIGHT: [u32; 10] = [10, 20, 40, 80, 160, 320, 640, 1280, 2560, 5120];
/// PHG directivity codes: 0=omni, then 20, 40, ..., 320 degrees.
const PHG_DIR: [u16; 10] = [0, 20, 40, 60, 80, 100, 120, 140, 160, 180];

/// Parse a PHG extension from the comment string.
///
/// Format: `PHGNhgd` anywhere in the comment, where each of N, h, g, d
/// is a single ASCII digit (0-9).
fn parse_phg(comment: &str) -> Option<Phg> {
    let idx = comment.find("PHG")?;
    let rest = &comment[idx + 3..];
    if rest.len() < 4 {
        return None;
    }
    let chars: Vec<u8> = rest[..4].bytes().collect();
    if !chars.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let p = (chars[0] - b'0') as usize;
    let h = (chars[1] - b'0') as usize;
    let g = chars[2] - b'0';
    let d = (chars[3] - b'0') as usize;

    Some(Phg {
        power_watts: PHG_POWER.get(p).copied().unwrap_or(0),
        height_feet: PHG_HEIGHT.get(h).copied().unwrap_or(10),
        gain_db: g,
        directivity_deg: PHG_DIR.get(d).copied().unwrap_or(0),
    })
}

/// Parse altitude extension from the comment string.
///
/// Format: `/A=NNNNNN` anywhere in the comment (6-digit altitude in feet,
/// can be negative with a leading minus sign in the 6-digit field).
fn parse_altitude(comment: &str) -> Option<i32> {
    let idx = comment.find("/A=")?;
    let rest = &comment[idx + 3..];
    if rest.len() < 6 {
        return None;
    }
    let val_str = &rest[..6];
    val_str.parse::<i32>().ok()
}

/// Parse a DAO extension from the comment string.
///
/// Format: `!DAO!` where D and O are extra precision digits for latitude
/// and longitude respectively. The middle character indicates the encoding:
/// - Uppercase letter (W): human-readable. D and O are ASCII digits (0-9)
///   representing hundredths of a minute increment (divide by 60 for degrees).
/// - Lowercase letter (w): base-91 encoded. D and O are base-91 characters
///   giving finer precision.
///
/// Returns `(lat_correction, lon_correction)` in decimal degrees.
fn parse_dao(comment: &str) -> Option<(f64, f64)> {
    // Find `!` followed by 3 chars and another `!`.
    let bytes = comment.as_bytes();
    for i in 0..bytes.len().saturating_sub(4) {
        if bytes[i] == b'!' && bytes[i + 4] == b'!' {
            let d = bytes[i + 1];
            let a = bytes[i + 2];
            let o = bytes[i + 3];

            if a.is_ascii_uppercase() {
                // Human-readable: D and O are ASCII digits.
                if d.is_ascii_digit() && o.is_ascii_digit() {
                    let lat_extra = f64::from(d - b'0') / 600.0;
                    let lon_extra = f64::from(o - b'0') / 600.0;
                    return Some((lat_extra, lon_extra));
                }
            } else if a.is_ascii_lowercase() {
                // Base-91: D and O are base-91 chars (33-123).
                if (33..=123).contains(&d) && (33..=123).contains(&o) {
                    let lat_extra = f64::from(d - 33) / (91.0 * 60.0);
                    let lon_extra = f64::from(o - 33) / (91.0 * 60.0);
                    return Some((lat_extra, lon_extra));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Mic-E TX builder (APRS101 Chapter 10)
// ---------------------------------------------------------------------------

/// Build a Mic-E encoded APRS position report for KISS transmission.
///
/// Mic-E is the most compact position format and the native format
/// used by Kenwood HTs including the TH-D75. The latitude is encoded
/// in the AX.25 destination address, and longitude + speed/course
/// are in the info field.
///
/// Encoding per APRS101 Chapter 10:
/// - Destination address: 6 chars encoding latitude digits + N/S + lon offset + W/E flags
/// - Info field: type byte (`0x60` for current Mic-E) + 3 lon bytes + 3 speed/course bytes
///   + symbol code + symbol table + comment
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `latitude`: Decimal degrees, positive = North, negative = South.
/// - `longitude`: Decimal degrees, positive = East, negative = West.
/// - `speed_knots`: Speed in knots (0-799).
/// - `course_deg`: Course in degrees (0-360; 0 = unknown).
/// - `symbol_table`: APRS symbol table character (`/` for primary, `\\` for alternate).
/// - `symbol_code`: APRS symbol code character (e.g., `>` for car).
/// - `comment`: Free-form comment text.
///
/// # Panics
///
/// Panics if the produced Mic-E destination bytes fail the `from_utf8`
/// round-trip, which by construction cannot happen — the encoder only
/// writes ASCII bytes in the ranges `0x30-0x39` ('0'-'9') and
/// `0x50-0x59` ('P'-'Y').
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names
)]
pub fn build_aprs_mice(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    // Default to Off Duty for backwards compat with the old signature.
    build_aprs_mice_with_message(
        source,
        latitude,
        longitude,
        speed_knots,
        course_deg,
        MiceMessage::OffDuty,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
}

/// Build a Mic-E encoded APRS position report with a specific
/// [`MiceMessage`] status code.
///
/// Per APRS 1.0.1 §10.1 Table 10, the 8 standard codes are encoded in
/// the message bits of the first three destination characters. The
/// other Mic-E encoder entrypoint, [`build_aprs_mice`], uses Off Duty
/// for backwards compatibility.
///
/// # Panics
///
/// Panics only if the produced destination bytes fail ASCII validation
/// — which cannot happen because the encoder only writes bytes in
/// `0x30-0x39` ('0'-'9') or `0x50-0x59` ('P'-'Y').
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    clippy::too_many_arguments
)]
pub fn build_aprs_mice_with_message(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    message: MiceMessage,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Vec<u8> {
    build_aprs_mice_with_message_packet(
        source,
        latitude,
        longitude,
        speed_knots,
        course_deg,
        message,
        symbol_table,
        symbol_code,
        comment,
        path,
    )
    .encode_kiss()
}

/// Like [`build_aprs_mice_with_message`] but returns the unencoded
/// [`Ax25Packet`] for callers that want to inspect or route it.
///
/// # Panics
///
/// Same as [`build_aprs_mice_with_message`] — the Mic-E destination
/// callsign is constructed from bytes that are always ASCII by
/// construction, so the `from_utf8` conversion cannot fail.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::similar_names,
    clippy::too_many_arguments
)]
pub fn build_aprs_mice_with_message_packet(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    speed_knots: u16,
    course_deg: u16,
    message: MiceMessage,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
    path: &[Ax25Address],
) -> Ax25Packet {
    // Clamp position so the wire fields never overflow.
    let latitude = latitude.clamp(-90.0, 90.0);
    let longitude = longitude.clamp(-180.0, 180.0);
    let north = latitude >= 0.0;
    let west = longitude < 0.0;
    let lat_abs = latitude.abs();
    let lon_abs = longitude.abs();

    // Decompose latitude into digits: DD MM.HH. Clamp the rounding so
    // hundredths == 100 rolls into minutes correctly.
    let lat_deg = lat_abs as u32;
    let lat_min_f = (lat_abs - f64::from(lat_deg)) * 60.0;
    let lat_min = lat_min_f as u32;
    let lat_hundredths_f = ((lat_min_f - f64::from(lat_min)) * 100.0).round();
    let lat_hundredths = (lat_hundredths_f as u32).min(99);

    let d0 = (lat_deg / 10).min(9) as u8;
    let d1 = (lat_deg % 10) as u8;
    let d2 = (lat_min / 10).min(9) as u8;
    let d3 = (lat_min % 10) as u8;
    let d4 = (lat_hundredths / 10) as u8;
    let d5 = (lat_hundredths % 10) as u8;

    // Message bits (A, B, C) from the 3-bit index. Per APRS 1.0.1 §10.1
    // Table 10, bit = 1 (Std1, uppercase P-Y range) when set.
    let (msg_a, msg_b, msg_c) = mice_message_bits(message);

    // Encode destination address characters. Chars 0-2 carry message
    // bits A/B/C: if the bit is 1, pick from P-Y; otherwise 0-9.
    let lon_offset = lon_abs >= 100.0;
    let dest_chars: [u8; 6] = [
        if msg_a { b'P' + d0 } else { b'0' + d0 },
        if msg_b { b'P' + d1 } else { b'0' + d1 },
        if msg_c { b'P' + d2 } else { b'0' + d2 },
        if north { b'P' + d3 } else { b'0' + d3 },
        if lon_offset { b'P' + d4 } else { b'0' + d4 },
        if west { b'P' + d5 } else { b'0' + d5 },
    ];
    // SAFETY: every byte in `dest_chars` is in the range 0x30-0x59 (P-Y
    // for custom, 0-9 for standard) by construction above, all valid ASCII.
    let dest_callsign = std::str::from_utf8(&dest_chars)
        .expect("Mic-E destination chars are ASCII by construction");

    // Longitude degrees encoding per APRS 1.0.1 §10.3.3:
    //   No offset (0-99°):    d = degrees
    //   Offset set (≥100°):
    //     100-109°:           d = degrees - 20    (decoder hits 180-189 → subtract 80)
    //     110-179°:           d = degrees - 100   (decoder passes through)
    //
    // Byte on the wire is always d + 28.
    let lon_deg_raw = lon_abs as u16;
    let d = if lon_offset {
        if lon_deg_raw >= 110 {
            (lon_deg_raw - 100) as u8
        } else {
            (lon_deg_raw - 20) as u8
        }
    } else {
        lon_deg_raw as u8
    };

    let lon_min_f = (lon_abs - f64::from(lon_abs as u32)) * 60.0;
    let lon_min_int = lon_min_f as u8;
    let lon_hundredths = ((lon_min_f - f64::from(lon_min_int)) * 100.0).round() as u8;

    // Minutes encoding: if < 10, add 60.
    let m = if lon_min_int < 10 {
        lon_min_int + 60
    } else {
        lon_min_int
    };

    // Speed/course encoding per APRS101.
    // SP = speed / 10, remainder from DC.
    // DC = (speed % 10) * 10 + course / 100
    // SE = course % 100
    let sp = (speed_knots / 10) as u8;
    let dc = ((speed_knots % 10) * 10 + course_deg / 100) as u8;
    let se = (course_deg % 100) as u8;

    // Build info field.
    let mut info = Vec::with_capacity(9 + comment.len());
    info.push(0x60); // Current Mic-E data type.
    info.push(d + 28);
    info.push(m + 28);
    info.push(lon_hundredths + 28);
    info.push(sp + 28);
    info.push(dc + 28);
    info.push(se + 28);
    info.push(symbol_code as u8);
    info.push(symbol_table as u8);
    info.extend_from_slice(comment.as_bytes());

    Ax25Packet::ui_frame(
        source.clone(),
        Ax25Address::new(dest_callsign, 0),
        path.to_vec(),
        info,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- KISS frame tests ----

    #[test]
    fn encode_simple_data_frame() {
        let frame = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: vec![0x01, 0x02, 0x03],
        };
        let encoded = encode_kiss_frame(&frame);
        assert_eq!(encoded, vec![FEND, 0x00, 0x01, 0x02, 0x03, FEND]);
    }

    #[test]
    fn encode_frame_with_fend_in_data() {
        let frame = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: vec![0xC0], // FEND in data
        };
        let encoded = encode_kiss_frame(&frame);
        assert_eq!(encoded, vec![FEND, 0x00, FESC, TFEND, FEND]);
    }

    #[test]
    fn encode_frame_with_fesc_in_data() {
        let frame = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: vec![0xDB], // FESC in data
        };
        let encoded = encode_kiss_frame(&frame);
        assert_eq!(encoded, vec![FEND, 0x00, FESC, TFESC, FEND]);
    }

    #[test]
    fn encode_frame_with_port() {
        let frame = KissFrame {
            port: 1,
            command: CMD_TX_DELAY,
            data: vec![0x28], // 400ms TX delay
        };
        let encoded = encode_kiss_frame(&frame);
        // type byte = (1 << 4) | 0x01 = 0x11
        assert_eq!(encoded, vec![FEND, 0x11, 0x28, FEND]);
    }

    #[test]
    fn encode_return_command() {
        // CMD_RETURN (0xFF) is a special full-byte command, NOT nibble-split.
        // Wire format: C0 FF C0 (per TH-D75 Operating Tips §2.7.3)
        let frame = KissFrame {
            port: 0,
            command: CMD_RETURN,
            data: vec![],
        };
        let encoded = encode_kiss_frame(&frame);
        assert_eq!(encoded, vec![FEND, 0xFF, FEND]);
    }

    #[test]
    fn return_command_roundtrip() {
        let original = KissFrame {
            port: 0,
            command: CMD_RETURN,
            data: vec![],
        };
        let encoded = encode_kiss_frame(&original);
        let decoded = decode_kiss_frame(&encoded).unwrap();
        assert_eq!(decoded.port, 0);
        assert_eq!(decoded.command, CMD_RETURN);
        assert!(decoded.data.is_empty());
    }

    #[test]
    fn decode_simple_data_frame() {
        let raw = vec![FEND, 0x00, 0x01, 0x02, 0x03, FEND];
        let frame = decode_kiss_frame(&raw).unwrap();
        assert_eq!(frame.port, 0);
        assert_eq!(frame.command, CMD_DATA);
        assert_eq!(frame.data, vec![0x01, 0x02, 0x03]);
    }

    #[test]
    fn decode_frame_with_escaped_fend() {
        let raw = vec![FEND, 0x00, FESC, TFEND, FEND];
        let frame = decode_kiss_frame(&raw).unwrap();
        assert_eq!(frame.data, vec![FEND]);
    }

    #[test]
    fn decode_frame_with_escaped_fesc() {
        let raw = vec![FEND, 0x00, FESC, TFESC, FEND];
        let frame = decode_kiss_frame(&raw).unwrap();
        assert_eq!(frame.data, vec![FESC]);
    }

    #[test]
    fn decode_roundtrip() {
        let original = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: vec![0xC0, 0xDB, 0x00, 0xFF, 0xC0, 0xDB],
        };
        let encoded = encode_kiss_frame(&original);
        let decoded = decode_kiss_frame(&encoded).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn decode_too_short() {
        assert_eq!(
            decode_kiss_frame(&[FEND, FEND]),
            Err(KissError::FrameTooShort)
        );
    }

    #[test]
    fn decode_missing_start() {
        assert_eq!(
            decode_kiss_frame(&[0x00, 0x01, FEND]),
            Err(KissError::MissingStartDelimiter)
        );
    }

    #[test]
    fn decode_missing_end() {
        assert_eq!(
            decode_kiss_frame(&[FEND, 0x00, 0x01]),
            Err(KissError::MissingEndDelimiter)
        );
    }

    #[test]
    fn decode_invalid_escape() {
        let raw = vec![FEND, 0x00, FESC, 0x42, FEND]; // FESC not followed by TFEND/TFESC
        assert_eq!(
            decode_kiss_frame(&raw),
            Err(KissError::InvalidEscapeSequence)
        );
    }

    #[test]
    fn decode_truncated_escape() {
        let raw = vec![FEND, 0x00, FESC, FEND]; // FESC at end of data
        assert_eq!(
            decode_kiss_frame(&raw),
            Err(KissError::InvalidEscapeSequence)
        );
    }

    #[test]
    fn kiss_decoder_emits_complete_frames_from_stream() {
        let mut dec = KissDecoder::new();
        // Feed bytes in chunks that split a frame across pushes.
        dec.push(&[FEND, 0x00, 0x01]);
        assert!(dec.next_frame().unwrap().is_none());
        dec.push(&[0x02, 0x03, FEND, FEND, 0x00]);
        let first = dec.next_frame().unwrap().unwrap();
        assert_eq!(first.data, vec![0x01, 0x02, 0x03]);
        // Second frame is incomplete until final FEND arrives.
        assert!(dec.next_frame().unwrap().is_none());
        dec.push(&[0xAA, FEND]);
        let second = dec.next_frame().unwrap().unwrap();
        assert_eq!(second.data, vec![0xAA]);
    }

    #[test]
    fn kiss_decoder_skips_garbage_before_fend() {
        let mut dec = KissDecoder::new();
        dec.push(b"junkdata before");
        assert!(dec.next_frame().unwrap().is_none());
        dec.push(&[FEND, 0x00, 0x42, FEND]);
        let frame = dec.next_frame().unwrap().unwrap();
        assert_eq!(frame.data, vec![0x42]);
    }

    #[test]
    fn encode_kiss_frame_into_reuses_buffer() {
        let frame = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: vec![1, 2, 3],
        };
        let mut buf = Vec::with_capacity(16);
        encode_kiss_frame_into(&frame, &mut buf);
        let expected = encode_kiss_frame(&frame);
        assert_eq!(buf, expected);
    }

    #[test]
    fn decode_extra_leading_fends() {
        // Multiple FENDs before the type byte (inter-frame fill)
        let raw = vec![FEND, FEND, FEND, 0x00, 0xAA, FEND];
        let frame = decode_kiss_frame(&raw).unwrap();
        assert_eq!(frame.command, CMD_DATA);
        assert_eq!(frame.data, vec![0xAA]);
    }

    // ---- AX.25 tests ----

    /// Build a minimal AX.25 UI frame for testing.
    fn make_test_ax25_bytes() -> Vec<u8> {
        // Destination: "APRS  " SSID 0
        // Source: "N0CALL" SSID 7
        let mut frame = Vec::new();

        // Destination address: "APRS  " (6 chars, shifted left)
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // SSID 0, not last

        // Source address: "N0CALL" shifted left
        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (7 << 1) | 0x01); // SSID 7, last address

        // Control: UI frame
        frame.push(0x03);
        // PID: no layer 3
        frame.push(0xF0);
        // Info: position report
        frame.extend_from_slice(b"!4903.50N/07201.75W-Test");

        frame
    }

    #[test]
    fn parse_ax25_basic() {
        let data = make_test_ax25_bytes();
        let packet = parse_ax25(&data).unwrap();
        assert_eq!(packet.destination.callsign, "APRS");
        assert_eq!(packet.destination.ssid, 0);
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.source.ssid, 7);
        assert!(packet.digipeaters.is_empty());
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);
        assert_eq!(&packet.info, b"!4903.50N/07201.75W-Test");
    }

    #[test]
    fn parse_ax25_with_digipeaters() {
        let mut frame = Vec::new();

        // Destination: "APRS  "
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // not last

        // Source: "W6DJY "
        for &ch in b"W6DJY " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (9 << 1)); // SSID 9, not last (has digis)

        // Digipeater 1: "WIDE1 "
        for &ch in b"WIDE1 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1)); // SSID 1, not last

        // Digipeater 2: "WIDE2 "
        for &ch in b"WIDE2 " {
            frame.push(ch << 1);
        }
        frame.push(0x60 | (1 << 1) | 0x01); // SSID 1, last address

        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"=test data");

        let packet = parse_ax25(&frame).unwrap();
        assert_eq!(packet.source.callsign, "W6DJY");
        assert_eq!(packet.source.ssid, 9);
        assert_eq!(packet.digipeaters.len(), 2);
        assert_eq!(packet.digipeaters[0].callsign, "WIDE1");
        assert_eq!(packet.digipeaters[0].ssid, 1);
        assert_eq!(packet.digipeaters[1].callsign, "WIDE2");
        assert_eq!(packet.digipeaters[1].ssid, 1);
    }

    #[test]
    fn ax25_roundtrip() {
        let original = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7),
            destination: Ax25Address::new("APRS", 0),
            digipeaters: vec![Ax25Address::new("WIDE1", 1), Ax25Address::new("WIDE2", 1)],
            control: 0x03,
            protocol: 0xF0,
            info: b"!4903.50N/07201.75W-Test 73".to_vec(),
        };

        let bytes = build_ax25(&original);
        let parsed = parse_ax25(&bytes).unwrap();

        assert_eq!(parsed.source.callsign, original.source.callsign);
        assert_eq!(parsed.source.ssid, original.source.ssid);
        assert_eq!(parsed.destination.callsign, original.destination.callsign);
        assert_eq!(parsed.destination.ssid, original.destination.ssid);
        assert_eq!(parsed.digipeaters.len(), original.digipeaters.len());
        for (a, b) in parsed.digipeaters.iter().zip(&original.digipeaters) {
            assert_eq!(a.callsign, b.callsign);
            assert_eq!(a.ssid, b.ssid);
        }
        assert_eq!(parsed.control, original.control);
        assert_eq!(parsed.protocol, original.protocol);
        assert_eq!(parsed.info, original.info);
    }

    #[test]
    fn parse_ax25_too_short() {
        assert!(parse_ax25(&[0; 10]).is_err());
    }

    #[test]
    fn ax25_address_display() {
        let addr = Ax25Address::new("N0CALL", 0);
        assert_eq!(format!("{addr}"), "N0CALL");

        let addr_ssid = Ax25Address::new("N0CALL", 7);
        assert_eq!(format!("{addr_ssid}"), "N0CALL-7");

        // H-bit (repeated) shows asterisk.
        let mut addr_repeated = Ax25Address::new("WIDE1", 1);
        addr_repeated.repeated = true;
        assert_eq!(format!("{addr_repeated}"), "WIDE1-1*");

        let mut addr_repeated_no_ssid = Ax25Address::new("MYDIGI", 0);
        addr_repeated_no_ssid.repeated = true;
        assert_eq!(format!("{addr_repeated_no_ssid}"), "MYDIGI*");
    }

    // ---- APRS position tests ----

    #[test]
    fn parse_aprs_position_no_timestamp() {
        let info = b"!4903.50N/07201.75W-Test comment";
        let pos = parse_aprs_position(info).unwrap();
        // 49 degrees 3.50 minutes N = 49.058333...
        assert!((pos.latitude - 49.058_333).abs() < 0.001);
        // 72 degrees 1.75 minutes W = -72.029166...
        assert!((pos.longitude - (-72.029_166)).abs() < 0.001);
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '-');
        assert_eq!(pos.comment, "Test comment");
    }

    #[test]
    fn parse_aprs_position_with_timestamp() {
        // '@' type with DHM timestamp "092345z"
        let info = b"@092345z4903.50N/07201.75W-";
        let pos = parse_aprs_position(info).unwrap();
        assert!((pos.latitude - 49.058_333).abs() < 0.001);
        assert!((pos.longitude - (-72.029_166)).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_position_south_east() {
        let info = b"!3356.65S/15113.72E>";
        let pos = parse_aprs_position(info).unwrap();
        assert!(pos.latitude < 0.0); // South
        assert!(pos.longitude > 0.0); // East
    }

    #[test]
    fn parse_aprs_position_messaging_enabled() {
        let info = b"=4903.50N/07201.75W-";
        let pos = parse_aprs_position(info).unwrap();
        assert!((pos.latitude - 49.058_333).abs() < 0.001);
    }

    #[test]
    fn parse_aprs_position_invalid_type() {
        let info = b"X4903.50N/07201.75W-";
        assert!(parse_aprs_position(info).is_err());
    }

    #[test]
    fn parse_aprs_position_too_short() {
        assert!(parse_aprs_position(b"!short").is_err());
    }

    #[test]
    fn parse_aprs_position_empty() {
        assert!(parse_aprs_position(b"").is_err());
    }

    // ---- APRS compressed position tests ----

    #[test]
    fn parse_aprs_compressed_position() {
        // Example from APRS101.PDF chapter 9: 49°30'N, 72°45'W
        // Lat: 90 - (49.5 * 380926 / 90) ≈ ... let me compute the base-91 bytes.
        // lat_val = (90 - 49.5) * 380926 = 40.5 * 380926 = 15427503
        // But easier to use known test vector.
        //
        // APRS101 example: /YYYY XXXX $csT
        // Let's use a computed example:
        // Latitude 49.5°N → val = (90 - 49.5) * 380926 = 15_427_503
        //   15427503 / 91^3 = 20.46 → b[0] = 20 + 33 = 53 = '5'
        //   remainder: 15427503 - 20*753571 = 15427503 - 15071420 = 356083
        //   356083 / 91^2 = 42.98 → b[1] = 42 + 33 = 75 = 'K'
        //   remainder: 356083 - 42*8281 = 356083 - 347802 = 8281
        //   8281 / 91 = 90.99 → b[2] = 90 + 33 = 123 = '{'
        //   remainder: 8281 - 90*91 = 8281 - 8190 = 91 → b[3] = 91... wait, max is 90
        // Let me use exact integer math instead.

        // Use simpler known values: lat=49.5, lon=-72.75
        // lat_val = (90 - 49.5) * 380926.0 = 15_427_503 (round to u32)
        // lon_val = (-72.75 + 180) * 190463.0 = 107.25 * 190463 = 20_427_156 (round)

        // Actually, let me just construct valid base-91 bytes and verify the decode.
        // lat_val = 3493929 → lat = 90 - 3493929/380926 = 90 - 9.172 = 80.828
        // Encode 3493929 in base-91:
        //   3493929 / 753571 = 4, rem 3493929 - 4*753571 = 479645
        //   479645 / 8281 = 57, rem 479645 - 57*8281 = 7628
        //   7628 / 91 = 83, rem 7628 - 83*91 = 75
        //   bytes: (4+33, 57+33, 83+33, 75+33) = (37, 90, 116, 108) = ('%', 'Z', 't', 'l'

        // lon_val = 4567890 → lon = -180 + 4567890/190463 = -180 + 23.982 = -156.018
        //   4567890 / 753571 = 6, rem 4567890 - 6*753571 = 46464
        //   46464 / 8281 = 5, rem 46464 - 5*8281 = 5059
        //   5059 / 91 = 55, rem 5059 - 55*91 = 54
        //   bytes: (6+33, 5+33, 55+33, 54+33) = (39, 38, 88, 87) = (''', '&', 'X', 'W')

        // Full compressed body: sym_table YYYY XXXX sym_code cs s t
        let body: &[u8] = b"/%Ztl'&XW> sT";
        //                    ^     ^       ^  symbol table '/'
        //                     ^^^^  lat     ^^^^  lon
        //                                 ^  symbol code '>'
        //                                  ^^^ cs, s, t (ignored for position)
        let mut info = vec![b'!'];
        info.extend_from_slice(body);

        let pos = parse_aprs_position(&info).unwrap();
        // Verify latitude: 90 - 3493929/380926 ≈ 80.828
        assert!((pos.latitude - 80.828).abs() < 0.01, "lat={}", pos.latitude);
        // Verify longitude: -180 + 4567890/190463 ≈ -156.018
        assert!(
            (pos.longitude - (-156.018)).abs() < 0.01,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '>');
    }

    #[test]
    fn parse_aprs_compressed_with_timestamp() {
        // '@' type with timestamp, then compressed body
        let mut info = Vec::new();
        info.push(b'@');
        info.extend_from_slice(b"092345z"); // 7-char timestamp
        info.extend_from_slice(b"/%Ztl'&XW> sT"); // compressed body
        let pos = parse_aprs_position(&info).unwrap();
        assert!((pos.latitude - 80.828).abs() < 0.01);
    }

    #[test]
    fn parse_aprs_compressed_too_short() {
        // Compressed needs at least 13 bytes in body
        let info = b"!/short";
        assert!(parse_aprs_position(info).is_err());
    }

    #[test]
    fn base91_decode_zero() {
        // All '!' (33) = value 0
        assert_eq!(decode_base91_4(b"!!!!").unwrap(), 0);
    }

    #[test]
    fn base91_decode_max() {
        // All '|' (124) = value 91 for each digit
        // 91*91^3 + 91*91^2 + 91*91 + 91 would overflow, but max char is 124=91+33
        // so max digit value is 124-33=91. Let's just verify the computation.
        let val = decode_base91_4(b"||||").unwrap();
        // 91*(91^3 + 91^2 + 91 + 1) — but actually the encoding is:
        // b[0]*91^3 + b[1]*91^2 + b[2]*91 + b[3] where each b[i] = 124-33 = 91
        // = 91*753571 + 91*8281 + 91*91 + 91
        let expected = 91_u32 * 753_571 + 91 * 8281 + 91 * 91 + 91;
        assert_eq!(val, expected);
    }

    #[test]
    fn base91_decode_invalid_char() {
        // Space (32) is below valid range
        assert!(decode_base91_4(b" !!!").is_err());
    }

    // ---- Mic-E position tests ----

    #[test]
    fn parse_mice_basic() {
        // Mic-E example: destination "T4SP0R" encodes latitude
        // T=4(custom), 4=4, S=3(custom), P=0(custom), 0=0, R=2(custom)
        // Wait, let me use the APRS101 encoding table:
        // P=0, Q=1, R=2, S=3, T=4, U=5, V=6, W=7, X=8, Y=9 (all custom=true)
        // 0-9 = 0-9 (custom=false)
        //
        // Destination "S32N0R":
        // S=3(custom), 3=3, 2=2, N=? — N is not in the table...
        //
        // Let me use a proper example.
        // For lat 35°16.52'N, lon 97°46.35'W:
        // Destination chars encode lat digits: 3,5,1,6,5,2
        // With message bits: chars 0-2 custom for msg type
        // Char 3 custom = North, char 4 custom = +100, char 5 custom = West
        //
        // Using "S5QN5W" as destination:
        // S=3(custom), 5=5(std), Q=1(custom), N→invalid
        //
        // Let me use all-P-Y range:
        // Destination "RUPPV2": R=2(c), U=5(c), P=0(c), P=0(c), V=6(c), 2=2(std)
        // Lat digits: 2,5,0,0,6,2 → lat = 25°00.62'
        // Char 3 (P) custom=true → North
        // Char 4 (V) custom=true → lon_offset = +100
        // Char 5 (2) custom=false → East
        //
        // Info: ` d m h speed_course symbol
        // Lon: deg=d-28+offset, min=m-28, hundredths=h-28
        // For lon 172°45.23'E: deg=172-100=72, d=72+28=100='d',
        //   min=45, m=45+28=73='I', h=23, h=23+28=51='3'
        //
        // But with offset=100, lon_deg = d-28+100 = 100-28+100 = 172.
        // If d=100-28=72, wait: d = (lon_deg - lon_offset) + 28 = (172-100)+28 = 100 = 'd'
        // m = 45 + 28 = 73 = 'I'
        // h = 23 + 28 = 51 = '3'
        //
        // Speed/course (3 bytes): just use placeholder values
        // info[4]=28+0=28 (might be below printable)... this gets complex.
        //
        // Let me use a simpler real-world-like example.
        // Lat 35°15.50'N, Lon -97°45.30'W (Oklahoma City area, synthetic)
        //
        // Destination encodes lat: digits 3,5,1,5,5,0
        // Char 0-2: message type bits (use custom=true for "standard" msg)
        //   S=3(c), U=5(c), Q=1(c) → digits 3,5,1
        // Char 3: digit 5, North → T=4... no, need 5+custom.
        //   For digit 5 custom: U=5(custom) → North
        // Char 4: digit 5, lon_offset 0 → 5(std, not custom) → no offset
        // Char 5: digit 0, West → P=0(custom) → West
        //
        // Destination: "SUQ5UP" → no wait, "SUQU5P"
        // S=3(c), U=5(c), Q=1(c) → digits 3,5,1 — message type 111 = Custom
        // U=5(c) → digit 5, North (char 3 custom=true)
        // 5=5(std) → digit 5, no lon offset (char 4 custom=false)
        // P=0(c) → digit 0, West (char 5 custom=true)
        //
        // Lat: 35°15.50'N = 35 + 15.50/60 = 35.2583
        //
        // Lon: 97°45.30'W. lon_offset=0 (char4 not custom).
        // d = 97 + 28 = 125 = '}' (but max printable is 126='~', ok)
        // Wait, d-28 must equal degrees. If lon_offset=0 and degrees=97:
        //   If 97 >= 180 → subtract 80. If 97 >= 190 → subtract 190. Neither applies.
        //   So d = 97 + 28 = 125 = '}'
        // m = 45 + 28 = 73 = 'I'
        // h = 30 + 28 = 58 = ':'
        //
        // Speed/course: info[4..7], let's use nulls (32+28=60, etc.)
        // Actually just pad with reasonable values.
        //
        // Info bytes: ` (type) } I : <speed> <course> <sym_code> <sym_table>
        // Let's use: type=0x60, d=125, m=73, h=58, then 3 speed/course bytes,
        //            sym_code='>', sym_table='/'

        let dest = "SUQU5P";
        let info: &[u8] = &[
            0x60, // Mic-E current data type
            125,  // longitude degrees + 28 = 97+28
            73,   // longitude minutes + 28 = 45+28
            58,   // longitude hundredths + 28 = 30+28
            40,   // speed/course byte 1
            40,   // speed/course byte 2
            40,   // speed/course byte 3
            b'>', // symbol code
            b'/', // symbol table
        ];

        let pos = parse_mice_position(dest, info).unwrap();
        // Lat should be ~35.258
        assert!((pos.latitude - 35.258).abs() < 0.01, "lat={}", pos.latitude);
        // Lon should be ~-97.755
        assert!(
            (pos.longitude - (-97.755)).abs() < 0.01,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_code, '>');
        assert_eq!(pos.symbol_table, '/');

        // Speed/course from info[4..7] = [40, 40, 40]:
        // SP = 40-28 = 12, DC = 40-28 = 12, SE = 40-28 = 12
        // speed = 12*10 + 12/10 = 121
        // course = (12%10)*100 + 12 = 212
        assert_eq!(pos.speed_knots, Some(121));
        assert_eq!(pos.course_degrees, Some(212));
    }

    #[test]
    fn parse_mice_invalid_type() {
        assert!(parse_mice_position("SUQU5P", b"!test data").is_err());
    }

    #[test]
    fn parse_mice_too_short() {
        assert!(parse_mice_position("SHORT", &[0x60, 1, 2]).is_err());
    }

    #[test]
    fn parse_mice_speed_ge_800_rejected() {
        // SP = 108-28 = 80, DC = 28-28 = 0, SE = 28-28 = 0
        // speed = 80*10 + 0/10 = 800 → should be rejected (>= 800)
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 108, 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info).unwrap();
        assert_eq!(pos.speed_knots, None);
    }

    #[test]
    fn mice_decode_message_off_duty() {
        // Chars "PPP" — all Std1 → idx 111 → Off Duty
        assert_eq!(mice_decode_message(*b"PPP"), Some(MiceMessage::OffDuty));
    }

    #[test]
    fn mice_decode_message_emergency() {
        // Chars "000" — all Std0 → idx 000 → Emergency
        assert_eq!(mice_decode_message(*b"000"), Some(MiceMessage::Emergency));
    }

    #[test]
    fn mice_decode_message_in_service() {
        // "P0P" → Std1, Std0, Std1 → idx 101 → In Service
        assert_eq!(mice_decode_message(*b"P0P"), Some(MiceMessage::InService));
    }

    #[test]
    fn mice_decode_message_custom_returns_none() {
        // Any A-K char → custom → None
        assert_eq!(mice_decode_message(*b"APP"), None);
        assert_eq!(mice_decode_message(*b"PKP"), None);
    }

    #[test]
    fn mice_decode_altitude_sea_level() {
        // Sea level = 0 m → wire value 10000 + 10000 = … wait, the
        // spec's offset is that the decoded metres = wire - 10000, so
        // sea level requires wire = 10000. Base-91 of 10000:
        //   10000 / 91^2 = 1  (rem 10000 - 8281 = 1719)
        //   1719 / 91 = 18    (rem 1719 - 1638 = 81)
        //   81
        // Bytes: (1+33, 18+33, 81+33) = ('"', '3', 'r'), then '}'.
        let altitude = mice_decode_altitude("\"3r}").unwrap();
        assert_eq!(altitude, 0);
    }

    #[test]
    fn mice_decode_altitude_absent() {
        assert_eq!(mice_decode_altitude("no altitude here"), None);
        assert_eq!(mice_decode_altitude(""), None);
        assert_eq!(mice_decode_altitude("abc"), None);
    }

    #[test]
    fn parse_mice_populates_message_and_altitude() {
        // Destination "SUQU5P" → chars 0-2 = S, U, Q — all Std1 → 111
        // → Off Duty. Comment contains `"3r}` which decodes to sea level.
        let mut info = vec![0x60u8, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        info.extend_from_slice(b"\"3r}");
        let pos = parse_mice_position("SUQU5P", &info).unwrap();
        assert_eq!(pos.mice_message, Some(MiceMessage::OffDuty));
        assert_eq!(pos.mice_altitude_m, Some(0));
    }

    #[test]
    fn parse_mice_course_zero_is_none() {
        // SP = 28-28 = 0, DC = 28-28 = 0, SE = 28-28 = 0
        // speed = 0*10 + 0/10 = 0, course = (0%10)*100 + 0 = 0
        // course 0 = not known → None
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 28, 28, 28, b'>', b'/'];
        let pos = parse_mice_position(dest, info).unwrap();
        assert_eq!(pos.speed_knots, Some(0));
        assert_eq!(pos.course_degrees, None);
    }

    // ---- APRS message tests ----

    #[test]
    fn parse_message_basic() {
        let info = b":N0CALL   :Hello World{123";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.addressee, "N0CALL");
        assert_eq!(msg.text, "Hello World");
        assert_eq!(msg.message_id, Some("123".to_string()));
    }

    #[test]
    fn parse_message_no_id() {
        let info = b":KQ4NIT   :Test message";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.addressee, "KQ4NIT");
        assert_eq!(msg.text, "Test message");
        assert_eq!(msg.message_id, None);
    }

    #[test]
    fn parse_message_ack() {
        let info = b":N0CALL   :ack123";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.text, "ack123");
    }

    #[test]
    fn parse_message_too_short() {
        assert!(parse_aprs_message(b":SHORT:hi").is_err());
    }

    #[test]
    fn parse_message_does_not_misinterpret_brace_in_text() {
        // Regression: "reply {soon}" should NOT produce message_id="soon}"
        // because "soon}" is not 1-5 alphanumerics at end of string.
        let info = b":N0CALL   :reply {soon}";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.text, "reply {soon}");
        assert_eq!(msg.message_id, None);
    }

    #[test]
    fn parse_message_accepts_valid_id_with_text_containing_brace() {
        // "json stuff {foo}{42" has a trailing "{42" that IS a valid ID.
        let info = b":N0CALL   :json {foo}{42";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.text, "json {foo}");
        assert_eq!(msg.message_id, Some("42".to_owned()));
    }

    #[test]
    fn message_kind_direct() {
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "hello".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::Direct);
    }

    #[test]
    fn message_kind_numeric_bulletin() {
        let msg = AprsMessage {
            addressee: "BLN3".to_owned(),
            text: "event update".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::Bulletin { number: 3 });
    }

    #[test]
    fn message_kind_group_bulletin() {
        let msg = AprsMessage {
            addressee: "BLNWX".to_owned(),
            text: "weather watch".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(
            msg.kind(),
            MessageKind::GroupBulletin {
                group: "WX".to_owned()
            }
        );
    }

    #[test]
    fn message_kind_nws_bulletin() {
        let msg = AprsMessage {
            addressee: "NWS-TOR".to_owned(),
            text: "tornado warning".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::NwsBulletin);
    }

    #[test]
    fn message_kind_ack_rej_frame() {
        let msg = AprsMessage {
            addressee: "N0CALL".to_owned(),
            text: "ack42".to_owned(),
            message_id: None,
            reply_ack: None,
        };
        assert_eq!(msg.kind(), MessageKind::AckRej);
    }

    #[test]
    fn parse_message_reply_ack() {
        // APRS 1.2 reply-ack: text contains `{MM}AA` where MM is this
        // msg's ID and AA is the ack for a previously-received message.
        let info = b":N0CALL   :Hello back{3}7";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.text, "Hello back");
        assert_eq!(msg.message_id, Some("3".to_owned()));
        assert_eq!(msg.reply_ack, Some("7".to_owned()));
    }

    #[test]
    fn parse_message_plain_id_no_reply_ack() {
        let info = b":N0CALL   :Hello{3";
        let msg = parse_aprs_message(info).unwrap();
        assert_eq!(msg.text, "Hello");
        assert_eq!(msg.message_id, Some("3".to_owned()));
        assert_eq!(msg.reply_ack, None);
    }

    #[test]
    fn build_aprs_message_truncates_long_text() {
        let source = test_source();
        let long_text = "x".repeat(100);
        let wire = build_aprs_message(
            &source,
            "W1AW",
            &long_text,
            None,
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_aprs_message(&packet.info).unwrap();
        assert_eq!(msg.text.len(), MAX_APRS_MESSAGE_TEXT_LEN);
    }

    #[test]
    fn build_aprs_message_checked_rejects_long_text() {
        let source = test_source();
        let long_text = "y".repeat(100);
        let result = build_aprs_message_checked(
            &source,
            "W1AW",
            &long_text,
            None,
            &default_digipeater_path(),
        );
        assert!(matches!(result, Err(AprsError::MessageTooLong(100))));
    }

    // ---- APRS status tests ----

    #[test]
    fn parse_status_basic() {
        let info = b">Operating on 144.390";
        let status = parse_aprs_status(info).unwrap();
        assert_eq!(status.text, "Operating on 144.390");
    }

    #[test]
    fn parse_status_empty() {
        let info = b">";
        let status = parse_aprs_status(info).unwrap();
        assert_eq!(status.text, "");
    }

    // ---- APRS object tests ----

    #[test]
    fn parse_object_live() {
        // ; + 9-char name + * + 7-char timestamp + uncompressed position
        let info = b";TORNADO  *092345z4903.50N/07201.75W-Tornado warning";
        let obj = parse_aprs_object(info).unwrap();
        assert_eq!(obj.name, "TORNADO");
        assert!(obj.live);
        assert_eq!(obj.timestamp, "092345z");
        assert!((obj.position.latitude - 49.058_333).abs() < 0.001);
    }

    #[test]
    fn parse_object_killed() {
        let info = b";MARATHON _092345z4903.50N/07201.75W-Event over";
        let obj = parse_aprs_object(info).unwrap();
        assert_eq!(obj.name, "MARATHON");
        assert!(!obj.live);
    }

    // ---- APRS item tests ----

    #[test]
    fn parse_item_live() {
        let info = b")AID#2!4903.50N/07201.75W-First aid";
        let item = parse_aprs_item(info).unwrap();
        assert_eq!(item.name, "AID#2");
        assert!(item.live);
        assert!((item.position.latitude - 49.058_333).abs() < 0.001);
    }

    #[test]
    fn parse_item_killed() {
        let info = b")AID#2_4903.50N/07201.75W-Closed";
        let item = parse_aprs_item(info).unwrap();
        assert!(!item.live);
    }

    #[test]
    fn parse_item_short_name_rejected() {
        // "AB" is only 2 characters — APRS101 requires 3-9.
        let info = b")AB!4903.50N/07201.75W-";
        assert!(matches!(
            parse_aprs_item(info),
            Err(AprsError::InvalidFormat)
        ));
    }

    // ---- APRS weather tests ----

    #[test]
    fn parse_weather_positionless() {
        let info = b"_01011234c180s005g010t075r001p010P020h55b10135";
        let wx = parse_aprs_weather_positionless(info).unwrap();
        assert_eq!(wx.wind_direction, Some(180));
        assert_eq!(wx.wind_speed, Some(5));
        assert_eq!(wx.wind_gust, Some(10));
        assert_eq!(wx.temperature, Some(75));
        assert_eq!(wx.rain_1h, Some(1));
        assert_eq!(wx.rain_24h, Some(10));
        assert_eq!(wx.rain_since_midnight, Some(20));
        assert_eq!(wx.humidity, Some(55));
        assert_eq!(wx.pressure, Some(10135));
    }

    #[test]
    fn parse_weather_missing_fields() {
        let info = b"_01011234c...s...t072";
        let wx = parse_aprs_weather_positionless(info).unwrap();
        assert_eq!(wx.wind_direction, None); // dots = missing
        assert_eq!(wx.wind_speed, None);
        assert_eq!(wx.temperature, Some(72));
    }

    #[test]
    fn parse_weather_humidity_zero_means_100() {
        let info = b"_01011234h00";
        let wx = parse_aprs_weather_positionless(info).unwrap();
        assert_eq!(wx.humidity, Some(100));
    }

    #[test]
    fn parse_weather_stops_on_comment_text() {
        // Regression: the old find('c')-based parser would match 'c' in
        // the word "canada" inside a comment. The new position-based
        // parser stops on the first unknown byte.
        let info = b"_01011234t072canada";
        let wx = parse_aprs_weather_positionless(info).unwrap();
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.wind_direction, None); // must NOT be Some(nad)
    }

    #[test]
    fn parse_weather_fields_in_order_with_gaps() {
        // Temperature only — other fields omitted entirely.
        let wx = parse_weather_fields(b"t072");
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.wind_direction, None);
    }

    #[test]
    fn parse_weather_rejects_trailing_garbage() {
        // The old parser would still find 'b' anywhere. The new parser
        // stops at the first unknown byte.
        let wx = parse_weather_fields(b"t072 b is not pressure");
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.pressure, None);
    }

    #[test]
    fn parse_position_embedded_weather() {
        // Weather station position: symbol code '_' + DDD/SSS + fields.
        let info = b"!3515.00N/09745.00W_090/010g015t072r001P020h55b10135";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.symbol_code, '_');
        let wx = pos.weather.expect("embedded weather");
        assert_eq!(wx.wind_direction, Some(90));
        assert_eq!(wx.wind_speed, Some(10));
        assert_eq!(wx.wind_gust, Some(15));
        assert_eq!(wx.temperature, Some(72));
        assert_eq!(wx.rain_1h, Some(1));
        assert_eq!(wx.rain_since_midnight, Some(20));
        assert_eq!(wx.humidity, Some(55));
        assert_eq!(wx.pressure, Some(10135));
    }

    #[test]
    fn parse_position_without_weather_symbol_has_no_weather() {
        let info = b"!3515.00N/09745.00W>mobile comment";
        let pos = parse_aprs_position(info).unwrap();
        assert!(pos.weather.is_none());
    }

    #[test]
    fn parse_position_weather_symbol_bad_format_has_no_weather() {
        // Symbol '_' but comment not in DDD/SSS form.
        let info = b"!3515.00N/09745.00W_hello";
        let pos = parse_aprs_position(info).unwrap();
        assert!(pos.weather.is_none());
    }

    #[test]
    fn parse_position_with_one_digit_ambiguity() {
        // Last hundredths digit replaced with space.
        let info = b"!4903.5 N/07201.75W-";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.ambiguity, PositionAmbiguity::OneDigit);
        // The space is replaced with 0 for decoding, so the position
        // should decode to 4903.50 ≈ 49.058.
        assert!((pos.latitude - 49.0583).abs() < 0.001);
    }

    #[test]
    fn parse_position_with_two_digit_ambiguity() {
        let info = b"!4903.  N/07201.75W-";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.ambiguity, PositionAmbiguity::TwoDigits);
    }

    #[test]
    fn parse_position_with_four_digit_ambiguity() {
        let info = b"!49  .  N/072  .  W-";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.ambiguity, PositionAmbiguity::FourDigits);
    }

    #[test]
    fn parse_position_full_precision_has_no_ambiguity() {
        let info = b"!4903.50N/07201.75W-";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.ambiguity, PositionAmbiguity::None);
    }

    #[test]
    fn parse_position_populates_extensions_from_comment() {
        // Comment contains CSE/SPD and altitude extensions.
        let info = b"!3515.00N/09745.00W>088/036/A=001234hello";
        let pos = parse_aprs_position(info).unwrap();
        assert_eq!(pos.extensions.course_speed, Some((88, 36)));
        assert_eq!(pos.extensions.altitude_ft, Some(1234));
        // Course/speed also surfaced as direct fields.
        assert_eq!(pos.speed_knots, Some(36));
        assert_eq!(pos.course_degrees, Some(88));
    }

    // ---- parse_aprs_data dispatch tests ----

    #[test]
    fn dispatch_position() {
        let info = b"!4903.50N/07201.75W-Test";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Position(_))));
    }

    #[test]
    fn dispatch_message() {
        let info = b":N0CALL   :Hello{1";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Message(_))));
    }

    #[test]
    fn dispatch_status() {
        let info = b">Status text";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Status(_))));
    }

    #[test]
    fn dispatch_object() {
        let info = b";OBJNAME  *092345z4903.50N/07201.75W-";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Object(_))));
    }

    #[test]
    fn dispatch_item() {
        let info = b")ITEM!4903.50N/07201.75W-";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Item(_))));
    }

    #[test]
    fn dispatch_weather() {
        let info = b"_01011234c180s005t072";
        assert!(matches!(parse_aprs_data(info), Ok(AprsData::Weather(_))));
    }

    #[test]
    fn dispatch_third_party() {
        let info = b"}W1AW>APK005,TCPIP:!4903.50N/07201.75W-from IS";
        match parse_aprs_data(info).unwrap() {
            AprsData::ThirdParty { header, payload } => {
                assert_eq!(header, "W1AW>APK005,TCPIP");
                assert_eq!(payload, b"!4903.50N/07201.75W-from IS");
            }
            other => panic!("expected ThirdParty, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_grid_locator() {
        let info = b"[EM13qc";
        match parse_aprs_data(info).unwrap() {
            AprsData::Grid(grid) => assert_eq!(grid, "EM13qc"),
            other => panic!("expected Grid, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_grid_4char() {
        let info = b"[FM18";
        match parse_aprs_data(info).unwrap() {
            AprsData::Grid(grid) => assert_eq!(grid, "FM18"),
            other => panic!("expected Grid, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_grid_invalid_rejected() {
        assert!(parse_aprs_data(b"[XX12").is_err()); // X > R
        assert!(parse_aprs_data(b"[AB").is_err()); // too short
    }

    #[test]
    fn dispatch_raw_gps() {
        let info = b"$GPRMC,123519,A,4807.038,N,01131.000,E,022.4,084.4,230394,003.1,W";
        match parse_aprs_data(info).unwrap() {
            AprsData::RawGps(s) => {
                assert!(s.starts_with("GPRMC,"));
                assert!(s.contains("4807.038"));
            }
            other => panic!("expected RawGps, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_capabilities_parses_tokens() {
        let info = b"<IGATE,MSG_CNT=10,LOC_CNT=42";
        match parse_aprs_data(info).unwrap() {
            AprsData::StationCapabilities(tokens) => {
                assert_eq!(tokens.len(), 3);
                assert_eq!(tokens[0], ("IGATE".to_owned(), String::new()));
                assert_eq!(tokens[1], ("MSG_CNT".to_owned(), "10".to_owned()));
                assert_eq!(tokens[2], ("LOC_CNT".to_owned(), "42".to_owned()));
            }
            other => panic!("expected StationCapabilities, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_agrelo_df() {
        let info = b"%\x01\x02\x03\x04";
        match parse_aprs_data(info).unwrap() {
            AprsData::AgreloDfJr(bytes) => assert_eq!(bytes, vec![1, 2, 3, 4]),
            other => panic!("expected AgreloDfJr, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_user_defined() {
        let info = b"{Adata payload";
        match parse_aprs_data(info).unwrap() {
            AprsData::UserDefined { experiment, data } => {
                assert_eq!(experiment, 'A');
                assert_eq!(data, b"data payload");
            }
            other => panic!("expected UserDefined, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_invalid_or_test() {
        let info = b",test frame";
        match parse_aprs_data(info).unwrap() {
            AprsData::InvalidOrTest(bytes) => assert_eq!(bytes, b"test frame"),
            other => panic!("expected InvalidOrTest, got {other:?}"),
        }
    }

    #[test]
    fn parse_object_rejects_bad_live_flag() {
        let info = b";TORNADO  X092345z4903.50N/07201.75W-";
        assert!(parse_aprs_object(info).is_err());
    }

    #[test]
    fn dispatch_mice_returns_error() {
        // Mic-E needs destination address, can't parse from info alone
        let info = &[0x60u8, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        assert!(matches!(
            parse_aprs_data(info),
            Err(AprsError::MicERequiresDestination)
        ));
    }

    // ---- parse_aprs_data_full tests ----

    #[test]
    fn full_dispatch_mice_current() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest).unwrap();
        assert!(matches!(result, AprsData::Position(_)));
    }

    #[test]
    fn full_dispatch_mice_old() {
        let dest = "SUQU5P";
        let info: &[u8] = &[b'\'', 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest).unwrap();
        assert!(matches!(result, AprsData::Position(_)));
    }

    #[test]
    fn full_dispatch_mice_0x1c() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x1C, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest).unwrap();
        assert!(matches!(result, AprsData::Position(_)));
    }

    #[test]
    fn full_dispatch_mice_0x1d() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x1D, 125, 73, 58, 40, 40, 40, b'>', b'/'];
        let result = parse_aprs_data_full(info, dest).unwrap();
        assert!(matches!(result, AprsData::Position(_)));
    }

    #[test]
    fn full_dispatch_non_mice_delegates() {
        let info = b"!4903.50N/07201.75W-Test";
        let result = parse_aprs_data_full(info, "APRS").unwrap();
        assert!(matches!(result, AprsData::Position(_)));
    }

    #[test]
    fn full_dispatch_empty_info() {
        assert!(parse_aprs_data_full(b"", "APRS").is_err());
    }

    // ---- Mic-E byte range validation tests ----

    #[test]
    fn mice_rejects_low_longitude_bytes() {
        // info[1] = 27 (below valid Mic-E range of 28)
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 27, 73, 58, 40, 40, 40, b'>', b'/'];
        assert_eq!(
            parse_mice_position(dest, info),
            Err(AprsError::InvalidCoordinates)
        );
    }

    #[test]
    fn mice_rejects_zero_longitude_byte() {
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 125, 0, 58, 40, 40, 40, b'>', b'/'];
        assert_eq!(
            parse_mice_position(dest, info),
            Err(AprsError::InvalidCoordinates)
        );
    }

    #[test]
    fn mice_accepts_minimum_valid_byte() {
        // info[1..4] all = 28, the minimum valid value
        let dest = "SUQU5P";
        let info: &[u8] = &[0x60, 28, 28, 28, 40, 40, 40, b'>', b'/'];
        assert!(parse_mice_position(dest, info).is_ok());
    }

    // ---- Full integration: KISS -> AX.25 -> APRS ----

    #[test]
    fn full_kiss_to_aprs_pipeline() {
        // Build an AX.25 APRS packet
        let ax25_data = make_test_ax25_bytes();

        // Wrap in KISS
        let kiss_frame = KissFrame {
            port: 0,
            command: CMD_DATA,
            data: ax25_data,
        };
        let wire = encode_kiss_frame(&kiss_frame);

        // Decode KISS
        let decoded_kiss = decode_kiss_frame(&wire).unwrap();
        assert_eq!(decoded_kiss.command, CMD_DATA);

        // Parse AX.25
        let packet = parse_ax25(&decoded_kiss.data).unwrap();
        assert_eq!(packet.source.callsign, "N0CALL");

        // Parse APRS position from info field
        let pos = parse_aprs_position(&packet.info).unwrap();
        assert!((pos.latitude - 49.058_333).abs() < 0.001);
    }

    // ---- APRS TX builder tests ----

    fn test_source() -> Ax25Address {
        Ax25Address::new("N0CALL", 7)
    }

    #[test]
    fn format_latitude_north() {
        let s = format_aprs_latitude(49.058_333);
        // 49 degrees, 3.50 minutes North
        assert_eq!(s.len(), 8);
        assert!(s.ends_with('N'));
        assert!(s.starts_with("49"));
    }

    #[test]
    fn format_latitude_south() {
        let s = format_aprs_latitude(-33.856);
        assert!(s.ends_with('S'));
        assert!(s.starts_with("33"));
    }

    #[test]
    fn format_longitude_east() {
        let s = format_aprs_longitude(151.209);
        assert_eq!(s.len(), 9);
        assert!(s.ends_with('E'));
        assert!(s.starts_with("151"));
    }

    #[test]
    fn format_longitude_west() {
        let s = format_aprs_longitude(-72.029_166);
        assert!(s.ends_with('W'));
        assert!(s.starts_with("072"));
    }

    #[test]
    fn build_position_report_roundtrip() {
        let source = test_source();
        let wire = build_aprs_position_report(
            &source,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Test",
            &default_digipeater_path(),
        );

        // Decode the KISS frame.
        let kiss = decode_kiss_frame(&wire).unwrap();
        assert_eq!(kiss.command, CMD_DATA);

        // Decode the AX.25 packet.
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.source.callsign, "N0CALL");
        assert_eq!(packet.source.ssid, 7);
        assert_eq!(packet.destination.callsign, "APK005");
        assert_eq!(packet.destination.ssid, 0);
        assert_eq!(packet.digipeaters.len(), 2);
        assert_eq!(packet.digipeaters[0].callsign, "WIDE1");
        assert_eq!(packet.digipeaters[0].ssid, 1);
        assert_eq!(packet.digipeaters[1].callsign, "WIDE2");
        assert_eq!(packet.digipeaters[1].ssid, 1);
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);

        // Parse the APRS position from the info field.
        let pos = parse_aprs_position(&packet.info).unwrap();
        assert!((pos.latitude - 49.058_333).abs() < 0.01);
        assert!((pos.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '-');
        assert!(pos.comment.contains("Test"));
    }

    // ---- Timestamp tests ----

    #[test]
    fn aprs_timestamp_dhm_zulu_format() {
        let ts = AprsTimestamp::DhmZulu {
            day: 9,
            hour: 23,
            minute: 45,
        };
        assert_eq!(ts.to_wire_string(), "092345z");
    }

    #[test]
    fn aprs_timestamp_hms_format() {
        let ts = AprsTimestamp::Hms {
            hour: 12,
            minute: 0,
            second: 1,
        };
        assert_eq!(ts.to_wire_string(), "120001h");
    }

    #[test]
    fn build_aprs_object_with_real_timestamp() {
        let source = test_source();
        let wire = build_aprs_object_with_timestamp(
            &source,
            "EVENT",
            true,
            AprsTimestamp::DhmZulu {
                day: 15,
                hour: 14,
                minute: 30,
            },
            35.0,
            -97.0,
            '/',
            '-',
            "real",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let obj = parse_aprs_object(&packet.info).unwrap();
        assert_eq!(obj.timestamp, "151430z");
    }

    // ---- Query builder tests ----

    #[test]
    fn build_aprs_query_position() {
        let source = test_source();
        let wire = build_aprs_query(
            &source,
            "W1AW",
            &AprsQuery::Position,
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_aprs_message(&packet.info).unwrap();
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "?APRSP");
    }

    #[test]
    fn build_message_roundtrip() {
        let source = test_source();
        let wire = build_aprs_message(
            &source,
            "KQ4NIT",
            "Hello 73!",
            Some("42"),
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");

        let msg = parse_aprs_message(&packet.info).unwrap();
        assert_eq!(msg.addressee, "KQ4NIT");
        assert_eq!(msg.text, "Hello 73!");
        assert_eq!(msg.message_id, Some("42".to_string()));
    }

    #[test]
    fn build_message_no_id() {
        let source = test_source();
        let wire = build_aprs_message(
            &source,
            "W1AW",
            "Test msg",
            None,
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let msg = parse_aprs_message(&packet.info).unwrap();
        assert_eq!(msg.addressee, "W1AW");
        assert_eq!(msg.text, "Test msg");
        assert_eq!(msg.message_id, None);
    }

    #[test]
    fn build_message_pads_short_addressee() {
        let source = test_source();
        let wire = build_aprs_message(&source, "AB", "Hi", None, &default_digipeater_path());

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        // The info field should have the addressee padded to 9 chars.
        let info_str = String::from_utf8_lossy(&packet.info);
        // Format: :ADDRESSEE:text — addressee is bytes 1..10.
        assert_eq!(&info_str[1..10], "AB       ");
    }

    #[test]
    fn build_object_roundtrip() {
        let source = test_source();
        let wire = build_aprs_object(
            &source,
            "TORNADO",
            true,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Wrn",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");

        let obj = parse_aprs_object(&packet.info).unwrap();
        assert_eq!(obj.name, "TORNADO");
        assert!(obj.live);
        assert!((obj.position.latitude - 49.058_333).abs() < 0.01);
        assert!((obj.position.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(obj.position.symbol_table, '/');
        assert_eq!(obj.position.symbol_code, '-');
        assert!(obj.position.comment.contains("Wrn"));
    }

    #[test]
    fn build_object_killed() {
        let source = test_source();
        let wire = build_aprs_object(
            &source,
            "EVENT",
            false,
            35.0,
            -97.0,
            '/',
            'E',
            "Done",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let obj = parse_aprs_object(&packet.info).unwrap();
        assert_eq!(obj.name, "EVENT");
        assert!(!obj.live);
    }

    // ---- APRS item TX builder tests ----

    #[test]
    fn build_item_live_roundtrip() {
        let source = test_source();
        let wire = build_aprs_item(
            &source,
            "MARKER",
            true,
            49.058_333,
            -72.029_166,
            '/',
            '-',
            "Test item",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");

        let item = parse_aprs_item(&packet.info).unwrap();
        assert_eq!(item.name, "MARKER");
        assert!(item.live);
        assert!((item.position.latitude - 49.058_333).abs() < 0.01);
        assert!((item.position.longitude - (-72.029_166)).abs() < 0.01);
        assert_eq!(item.position.symbol_table, '/');
        assert_eq!(item.position.symbol_code, '-');
        assert!(item.position.comment.contains("Test item"));
    }

    #[test]
    fn build_item_killed() {
        let source = test_source();
        let wire = build_aprs_item(
            &source,
            "GONE",
            false,
            35.0,
            -97.0,
            '/',
            'E',
            "Removed",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let item = parse_aprs_item(&packet.info).unwrap();
        assert_eq!(item.name, "GONE");
        assert!(!item.live);
    }

    // ---- APRS weather TX builder tests ----

    #[test]
    fn build_weather_full_roundtrip() {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: Some(180),
            wind_speed: Some(10),
            wind_gust: Some(25),
            temperature: Some(72),
            rain_1h: Some(5),
            rain_24h: Some(50),
            rain_since_midnight: Some(100),
            humidity: Some(55),
            pressure: Some(10132),
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");

        // Parse it back.
        let parsed = parse_aprs_weather_positionless(&packet.info).unwrap();
        assert_eq!(parsed.wind_direction, Some(180));
        assert_eq!(parsed.wind_speed, Some(10));
        assert_eq!(parsed.wind_gust, Some(25));
        assert_eq!(parsed.temperature, Some(72));
        assert_eq!(parsed.rain_1h, Some(5));
        assert_eq!(parsed.rain_24h, Some(50));
        assert_eq!(parsed.rain_since_midnight, Some(100));
        assert_eq!(parsed.humidity, Some(55));
        assert_eq!(parsed.pressure, Some(10132));
    }

    #[test]
    fn build_weather_partial_fields() {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: None,
            wind_speed: None,
            wind_gust: None,
            temperature: Some(32),
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: None,
            pressure: Some(10200),
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        let parsed = parse_aprs_weather_positionless(&packet.info).unwrap();
        assert_eq!(parsed.temperature, Some(32));
        assert_eq!(parsed.pressure, Some(10200));
        assert_eq!(parsed.wind_direction, None);
        assert_eq!(parsed.humidity, None);
    }

    #[test]
    fn build_aprs_position_weather_roundtrip() {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: Some(90),
            wind_speed: Some(10),
            wind_gust: Some(15),
            temperature: Some(72),
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: Some(20),
            humidity: Some(55),
            pressure: Some(10135),
        };
        let wire = build_aprs_position_weather(
            &test_source(),
            35.25,
            -97.75,
            '/',
            &wx,
            &default_digipeater_path(),
        );
        let _ = source;
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_aprs_position(&packet.info).unwrap();
        assert_eq!(pos.symbol_code, '_');
        let weather = pos.weather.expect("embedded weather");
        assert_eq!(weather.wind_direction, Some(90));
        assert_eq!(weather.wind_speed, Some(10));
        assert_eq!(weather.wind_gust, Some(15));
        assert_eq!(weather.temperature, Some(72));
        assert_eq!(weather.humidity, Some(55));
        assert_eq!(weather.pressure, Some(10135));
    }

    #[test]
    fn build_weather_humidity_100_encodes_as_00() {
        let source = test_source();
        let wx = AprsWeather {
            wind_direction: None,
            wind_speed: None,
            wind_gust: None,
            temperature: None,
            rain_1h: None,
            rain_24h: None,
            rain_since_midnight: None,
            humidity: Some(100),
            pressure: None,
        };

        let wire = build_aprs_weather(&source, &wx, &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        let parsed = parse_aprs_weather_positionless(&packet.info).unwrap();
        // APRS encodes humidity 100% as "h00", parser converts back to 100.
        assert_eq!(parsed.humidity, Some(100));
    }

    // ---- Compressed position builder tests ----

    #[test]
    fn build_compressed_position_round_trip() {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            35.3,
            -84.233,
            '/',
            '>',
            "test",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");
        assert_eq!(packet.control, 0x03);
        assert_eq!(packet.protocol, 0xF0);

        // Parse it back through the existing compressed parser.
        let data = parse_aprs_data(&packet.info).unwrap();
        if let AprsData::Position(pos) = data {
            // Compressed encoding has some rounding; check within tolerance.
            assert!((pos.latitude - 35.3).abs() < 0.01, "lat: {}", pos.latitude);
            assert!(
                (pos.longitude - (-84.233)).abs() < 0.01,
                "lon: {}",
                pos.longitude
            );
            assert_eq!(pos.symbol_table, '/');
            assert_eq!(pos.symbol_code, '>');
            assert!(pos.comment.contains("test"));
        } else {
            panic!("expected Position, got {data:?}");
        }
    }

    #[test]
    fn build_compressed_position_equator_prime_meridian() {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            0.0,
            0.0,
            '/',
            '-',
            "",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        let data = parse_aprs_data(&packet.info).unwrap();
        if let AprsData::Position(pos) = data {
            assert!(pos.latitude.abs() < 0.01, "lat: {}", pos.latitude);
            assert!(pos.longitude.abs() < 0.01, "lon: {}", pos.longitude);
        } else {
            panic!("expected Position, got {data:?}");
        }
    }

    #[test]
    fn build_compressed_position_southern_hemisphere() {
        let source = test_source();
        let wire = build_aprs_position_compressed(
            &source,
            -33.86,
            151.21,
            '/',
            '>',
            "sydney",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        let data = parse_aprs_data(&packet.info).unwrap();
        if let AprsData::Position(pos) = data {
            assert!(
                (pos.latitude - (-33.86)).abs() < 0.01,
                "lat: {}",
                pos.latitude
            );
            assert!(
                (pos.longitude - 151.21).abs() < 0.01,
                "lon: {}",
                pos.longitude
            );
        } else {
            panic!("expected Position, got {data:?}");
        }
    }

    #[test]
    fn base91_encoding_known_value() {
        // APRS101 example: 90 degrees latitude encodes as "!!!!".
        let encoded = encode_base91_4(0);
        assert_eq!(encoded, [b'!', b'!', b'!', b'!']);
    }

    // ---- Status builder tests ----

    #[test]
    fn build_status_round_trip() {
        let source = test_source();
        let wire = build_aprs_status(&source, "On the air in FM18", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        assert_eq!(packet.destination.callsign, "APK005");

        let data = parse_aprs_data(&packet.info).unwrap();
        if let AprsData::Status(status) = data {
            assert_eq!(status.text, "On the air in FM18");
        } else {
            panic!("expected Status, got {data:?}");
        }
    }

    #[test]
    fn build_status_empty_text() {
        let source = test_source();
        let wire = build_aprs_status(&source, "", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        let data = parse_aprs_data(&packet.info).unwrap();
        if let AprsData::Status(status) = data {
            assert_eq!(status.text, "");
        } else {
            panic!("expected Status, got {data:?}");
        }
    }

    #[test]
    fn build_status_info_field_format() {
        let source = test_source();
        let wire = build_aprs_status(&source, "Hello", &default_digipeater_path());
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        // Info field should be: >Hello\r
        assert_eq!(packet.info[0], b'>');
        assert_eq!(&packet.info[1..6], b"Hello");
        assert_eq!(packet.info[6], b'\r');
    }

    // ---- APRS data extension parser tests ----

    #[test]
    fn parse_extensions_cse_spd() {
        let ext = parse_aprs_extensions("088/036");
        assert_eq!(ext.course_speed, Some((88, 36)));
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
    }

    #[test]
    fn parse_extensions_cse_spd_with_comment() {
        let ext = parse_aprs_extensions("270/015via Mic-E");
        assert_eq!(ext.course_speed, Some((270, 15)));
    }

    #[test]
    fn parse_extensions_cse_spd_invalid_course() {
        // Course 999 > 360 is invalid.
        let ext = parse_aprs_extensions("999/050");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_cse_spd_not_at_start() {
        // CSE/SPD must be at position 0.
        let ext = parse_aprs_extensions("xx088/036");
        assert!(ext.course_speed.is_none());
    }

    #[test]
    fn parse_extensions_phg() {
        // PHG5132 = power 25W, height 20ft (10*2^1), gain 3dB, directivity 40deg
        let ext = parse_aprs_extensions("PHG5132");
        let phg = ext.phg.unwrap();
        assert_eq!(phg.power_watts, 25);
        assert_eq!(phg.height_feet, 20);
        assert_eq!(phg.gain_db, 3);
        assert_eq!(phg.directivity_deg, 40);
    }

    #[test]
    fn parse_extensions_phg_omni() {
        // PHG2360 = power 4W, height 80ft, gain 6dB, omni
        let ext = parse_aprs_extensions("PHG2360");
        let phg = ext.phg.unwrap();
        assert_eq!(phg.power_watts, 4);
        assert_eq!(phg.height_feet, 80);
        assert_eq!(phg.gain_db, 6);
        assert_eq!(phg.directivity_deg, 0);
    }

    #[test]
    fn parse_extensions_phg_in_comment() {
        let ext = parse_aprs_extensions("some text PHG5132 more text");
        assert!(ext.phg.is_some());
        assert_eq!(ext.phg.unwrap().power_watts, 25);
    }

    #[test]
    fn parse_extensions_altitude() {
        let ext = parse_aprs_extensions("some comment /A=001234 more");
        assert_eq!(ext.altitude_ft, Some(1234));
    }

    #[test]
    fn parse_extensions_altitude_negative() {
        let ext = parse_aprs_extensions("/A=-00100");
        assert_eq!(ext.altitude_ft, Some(-100));
    }

    #[test]
    fn parse_extensions_altitude_zeros() {
        let ext = parse_aprs_extensions("/A=000000");
        assert_eq!(ext.altitude_ft, Some(0));
    }

    #[test]
    fn parse_extensions_dao_human_readable() {
        // !W5! — W is uppercase, so digits 5 and 5.
        let ext = parse_aprs_extensions("text !5W5! more");
        let (lat, lon) = ext.dao.unwrap();
        let expected = 5.0 / 600.0;
        assert!((lat - expected).abs() < 1e-9, "lat={lat}");
        assert!((lon - expected).abs() < 1e-9, "lon={lon}");
    }

    #[test]
    fn parse_extensions_dao_base91() {
        // !w"! — w is lowercase, " is char 34, so base-91 value = 34-33 = 1
        let ext = parse_aprs_extensions("!\"w\"!");
        let (lat, lon) = ext.dao.unwrap();
        let expected = 1.0 / (91.0 * 60.0);
        assert!((lat - expected).abs() < 1e-9, "lat={lat}");
        assert!((lon - expected).abs() < 1e-9, "lon={lon}");
    }

    #[test]
    fn parse_extensions_combined() {
        let ext = parse_aprs_extensions("088/036PHG5132/A=001234");
        assert_eq!(ext.course_speed, Some((88, 36)));
        assert!(ext.phg.is_some());
        assert_eq!(ext.altitude_ft, Some(1234));
    }

    #[test]
    fn parse_extensions_empty() {
        let ext = parse_aprs_extensions("");
        assert!(ext.course_speed.is_none());
        assert!(ext.phg.is_none());
        assert!(ext.altitude_ft.is_none());
        assert!(ext.dao.is_none());
    }

    // ---- Mic-E TX builder tests ----

    #[test]
    fn build_mice_roundtrip_oklahoma() {
        // 35.258 N, 97.755 W — matches the existing parse_mice test case.
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.258,
            -97.755,
            121,
            212,
            '/',
            '>',
            "test",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();

        // Destination should encode the latitude.
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert!((pos.latitude - 35.258).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-97.755)).abs() < 0.02,
            "lon={}",
            pos.longitude
        );
        assert_eq!(pos.symbol_table, '/');
        assert_eq!(pos.symbol_code, '>');
        assert!(pos.comment.contains("test"));
    }

    #[test]
    fn build_mice_roundtrip_north_east() {
        // 51.5 N, 0.1 W (London area)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            51.5,
            -0.1,
            0,
            0,
            '/',
            '-',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert!((pos.latitude - 51.5).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - (-0.1)).abs() < 0.02,
            "lon={}",
            pos.longitude
        );
    }

    #[test]
    fn build_mice_roundtrip_southern_hemisphere() {
        // -33.86 S, 151.21 E (Sydney)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            -33.86,
            151.21,
            50,
            180,
            '/',
            '>',
            "sydney",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert!(
            (pos.latitude - (-33.86)).abs() < 0.02,
            "lat={}",
            pos.latitude
        );
        assert!(
            (pos.longitude - 151.21).abs() < 0.02,
            "lon={}",
            pos.longitude
        );
    }

    #[test]
    fn build_mice_speed_course_roundtrip() {
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -97.0,
            55,
            270,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert_eq!(pos.speed_knots, Some(55));
        assert_eq!(pos.course_degrees, Some(270));
    }

    #[test]
    fn build_mice_zero_speed_course() {
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            40.0,
            -74.0,
            0,
            0,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert_eq!(pos.speed_knots, Some(0));
        // Course 0 = unknown → None in the decoder.
        assert_eq!(pos.course_degrees, None);
    }

    #[test]
    fn build_mice_high_longitude() {
        // 35.0 N, 140.0 E (Tokyo area)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            140.0,
            10,
            90,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert!((pos.latitude - 35.0).abs() < 0.02, "lat={}", pos.latitude);
        assert!(
            (pos.longitude - 140.0).abs() < 0.02,
            "lon={}",
            pos.longitude
        );
    }

    #[test]
    fn build_mice_with_message_roundtrip() {
        // Encode each standard message code, decode it back, verify.
        let cases = [
            MiceMessage::OffDuty,
            MiceMessage::EnRoute,
            MiceMessage::InService,
            MiceMessage::Returning,
            MiceMessage::Committed,
            MiceMessage::Special,
            MiceMessage::Priority,
            MiceMessage::Emergency,
        ];
        for msg in cases {
            let source = test_source();
            let wire = build_aprs_mice_with_message(
                &source,
                35.25,
                -97.75,
                10,
                90,
                msg,
                '/',
                '>',
                "",
                &default_digipeater_path(),
            );
            let kiss = decode_kiss_frame(&wire).unwrap();
            let packet = parse_ax25(&kiss.data).unwrap();
            let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
            assert_eq!(pos.mice_message, Some(msg), "round trip for {msg:?}");
        }
    }

    #[test]
    fn build_mice_lon_100_109() {
        // 35.0 N, 105.5 W (New Mexico)
        let source = test_source();
        let wire = build_aprs_mice(
            &source,
            35.0,
            -105.5,
            0,
            0,
            '/',
            '>',
            "",
            &default_digipeater_path(),
        );

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let pos = parse_mice_position(&packet.destination.callsign, &packet.info).unwrap();
        assert!(
            (pos.longitude - (-105.5)).abs() < 0.02,
            "lon={}",
            pos.longitude
        );
    }

    // ---- Telemetry tests ----

    #[test]
    fn parse_telemetry_full() {
        let info = b"T#123,100,200,300,400,500,10101010";
        let data = parse_aprs_data(info).unwrap();
        match data {
            AprsData::Telemetry(t) => {
                assert_eq!(t.sequence, "123");
                assert_eq!(
                    t.analog,
                    [Some(100), Some(200), Some(300), Some(400), Some(500)]
                );
                assert_eq!(t.digital, 0b1010_1010);
            }
            other => panic!("expected Telemetry, got {other:?}"),
        }
    }

    #[test]
    fn parse_telemetry_mic_sequence() {
        let info = b"T#MIC,001,002,003,004,005,11111111";
        let data = parse_aprs_data(info).unwrap();
        match data {
            AprsData::Telemetry(t) => {
                assert_eq!(t.sequence, "MIC");
                assert_eq!(t.analog, [Some(1), Some(2), Some(3), Some(4), Some(5)]);
                assert_eq!(t.digital, 0xFF);
            }
            other => panic!("expected Telemetry, got {other:?}"),
        }
    }

    #[test]
    fn parse_telemetry_partial_analog() {
        // Only 3 analog values, no digital.
        let info = b"T#001,10,20,30";
        let data = parse_aprs_data(info).unwrap();
        match data {
            AprsData::Telemetry(t) => {
                assert_eq!(t.sequence, "001");
                assert_eq!(t.analog, [Some(10), Some(20), Some(30), None, None]);
                assert_eq!(t.digital, 0);
            }
            other => panic!("expected Telemetry, got {other:?}"),
        }
    }

    #[test]
    fn parse_telemetry_zero_values() {
        let info = b"T#000,0,0,0,0,0,00000000";
        let data = parse_aprs_data(info).unwrap();
        match data {
            AprsData::Telemetry(t) => {
                assert_eq!(t.sequence, "000");
                assert_eq!(t.analog, [Some(0), Some(0), Some(0), Some(0), Some(0)]);
                assert_eq!(t.digital, 0);
            }
            other => panic!("expected Telemetry, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_definition_parm() {
        let def =
            TelemetryDefinition::from_text("PARM.Volts,Temp,Humid,Wind,Rain,Door,Light,Heat,,,,,")
                .unwrap();
        match def {
            TelemetryDefinition::Parameters(p) => {
                assert_eq!(p.analog[0].as_deref(), Some("Volts"));
                assert_eq!(p.analog[4].as_deref(), Some("Rain"));
                assert_eq!(p.digital[0].as_deref(), Some("Door"));
                assert_eq!(p.digital[2].as_deref(), Some("Heat"));
            }
            other => panic!("expected Parameters, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_definition_unit() {
        let def = TelemetryDefinition::from_text("UNIT.Vdc,C,%,mph,in,open,lit,on,,,,,").unwrap();
        match def {
            TelemetryDefinition::Units(p) => {
                assert_eq!(p.analog[1].as_deref(), Some("C"));
            }
            other => panic!("expected Units, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_definition_eqns() {
        let def = TelemetryDefinition::from_text("EQNS.0,0.1,0,0,0.5,0,0,1,0,0,2,0,0,3,0").unwrap();
        match def {
            TelemetryDefinition::Equations(eqs) => {
                assert_eq!(eqs[0], Some((0.0, 0.1, 0.0)));
                assert_eq!(eqs[1], Some((0.0, 0.5, 0.0)));
                assert_eq!(eqs[4], Some((0.0, 3.0, 0.0)));
            }
            other => panic!("expected Equations, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_definition_bits() {
        let def = TelemetryDefinition::from_text("BITS.11111111,WX station telemetry").unwrap();
        match def {
            TelemetryDefinition::Bits { bits, title } => {
                assert_eq!(bits, "11111111");
                assert_eq!(title, "WX station telemetry");
            }
            other => panic!("expected Bits, got {other:?}"),
        }
    }

    #[test]
    fn telemetry_definition_unknown_returns_none() {
        assert!(TelemetryDefinition::from_text("hello world").is_none());
    }

    #[test]
    fn parse_telemetry_rejects_too_many_fields() {
        // 6 analog + 1 digital = 7 after the sequence = 8 fields total.
        let info = b"T#001,1,2,3,4,5,6,00000000";
        assert!(matches!(
            parse_aprs_data(info),
            Err(AprsError::InvalidFormat)
        ));
    }

    #[test]
    fn parse_telemetry_invalid_no_hash() {
        let info = b"T123,1,2,3,4,5,00000000";
        assert!(parse_aprs_data(info).is_err());
    }

    #[test]
    fn parse_telemetry_invalid_digital_field_is_error() {
        // Digital field must be exactly 8 binary digits — non-binary
        // characters must fail parsing, not silently return 0.
        let info = b"T#123,1,2,3,4,5,XXXXXXXX";
        assert!(matches!(
            parse_aprs_data(info),
            Err(AprsError::InvalidFormat)
        ));
    }

    #[test]
    fn parse_ax25_rejects_more_than_8_digipeaters() {
        // Build a packet claiming 9 digipeaters. Each address is 7 bytes,
        // and the source extension bit is 0, so the parser follows the
        // digi chain. We set the E-bit only on the 9th digi.
        let mut frame = Vec::new();
        for &ch in b"APRS  " {
            frame.push(ch << 1);
        }
        frame.push(0x60); // dest ssid 0, not last

        for &ch in b"N0CALL" {
            frame.push(ch << 1);
        }
        frame.push(0x60); // source ssid 0, not last → digis follow

        // 9 digipeater addresses, last one has E-bit.
        for i in 0..9 {
            for &ch in b"WIDE1 " {
                frame.push(ch << 1);
            }
            let ssid_byte = 0x60 | (1 << 1) | u8::from(i == 8);
            frame.push(ssid_byte);
        }
        frame.push(0x03);
        frame.push(0xF0);
        frame.extend_from_slice(b"!test");

        let result = parse_ax25(&frame);
        assert_eq!(result, Err(Ax25Error::TooManyDigipeaters));
    }

    // ---- Query tests ----

    #[test]
    fn parse_query_position_aprsp() {
        let info = b"?APRSP";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Position));
    }

    #[test]
    fn parse_query_position_aprs_question() {
        let info = b"?APRS?";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Position));
    }

    #[test]
    fn parse_query_status() {
        let info = b"?APRSS";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Status));
    }

    #[test]
    fn parse_query_message() {
        let info = b"?APRSM";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Message));
    }

    #[test]
    fn parse_query_direction_finding() {
        let info = b"?APRSD";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::DirectionFinding));
    }

    #[test]
    fn parse_query_igate() {
        let info = b"?IGATE";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::IGate));
    }

    #[test]
    fn parse_query_ping() {
        let info = b"?PING?";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Ping));
    }

    #[test]
    fn parse_query_weather() {
        let info = b"?WX";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Weather));
    }

    #[test]
    fn parse_query_telemetry() {
        let info = b"?APRST";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Telemetry));
    }

    #[test]
    fn parse_query_heard() {
        let info = b"?APRSH";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Heard));
    }

    #[test]
    fn parse_query_other() {
        let info = b"?FOOBAR";
        let data = parse_aprs_data(info).unwrap();
        match data {
            AprsData::Query(AprsQuery::Other(s)) => assert_eq!(s, "FOOBAR"),
            other => panic!("expected Query::Other, got {other:?}"),
        }
    }

    #[test]
    fn parse_query_with_trailing_cr() {
        let info = b"?APRSP\r";
        let data = parse_aprs_data(info).unwrap();
        assert_eq!(data, AprsData::Query(AprsQuery::Position));
    }

    // ---- Typed enums (Ax25Pid, Ax25Control) ----

    #[test]
    fn parse_context_display_with_field() {
        let ctx = ParseContext::with_error(AprsError::InvalidFormat, 17, Some("addressee"));
        let s = format!("{ctx}");
        assert!(s.contains("byte 17"));
        assert!(s.contains("addressee"));
    }

    #[test]
    fn parse_context_display_without_field() {
        let ctx = ParseContext::with_error(AprsError::InvalidCoordinates, 4, None);
        let s = format!("{ctx}");
        assert!(s.contains("byte 4"));
    }

    #[test]
    fn ax25_fcs_known_value() {
        // Known test vector: CRC-CCITT of "123456789" in reflected form
        // equals 0x906E. (See CRC-16/X-25, matches AX.25 FCS.)
        assert_eq!(ax25_fcs(b"123456789"), 0x906E);
    }

    #[test]
    fn ax25_fcs_empty_matches_init_xor() {
        // Initial value 0xFFFF with xorout 0xFFFF → 0x0000 for empty.
        assert_eq!(ax25_fcs(&[]), 0x0000);
    }

    #[test]
    fn ax25_pid_round_trip_no_layer_3() {
        assert_eq!(Ax25Pid::from_byte(0xF0), Ax25Pid::NoLayer3);
        assert_eq!(Ax25Pid::NoLayer3.as_byte(), 0xF0);
    }

    #[test]
    fn ax25_pid_round_trip_common_values() {
        for b in [0x01u8, 0x06, 0xCC, 0xCF, 0xF0, 0xFF] {
            assert_eq!(Ax25Pid::from_byte(b).as_byte(), b);
        }
    }

    #[test]
    fn ax25_pid_unknown_byte_is_other() {
        let pid = Ax25Pid::from_byte(0x42);
        assert_eq!(pid, Ax25Pid::Other(0x42));
        assert_eq!(pid.as_byte(), 0x42);
    }

    #[test]
    fn ax25_control_ui_frame_parses() {
        let control = Ax25Control::from_byte(0x03);
        assert!(control.is_ui());
        assert!(matches!(
            control,
            Ax25Control::Unnumbered {
                kind: UnnumberedKind::UnnumberedInformation,
                pf: false,
            }
        ));
    }

    #[test]
    fn ax25_control_information_frame() {
        // I-frame: bit 0 = 0. Use control = 0b0010_0000 (NR=1, NS=0, P=0).
        let control = Ax25Control::from_byte(0b0010_0000);
        assert!(matches!(
            control,
            Ax25Control::Information {
                ns: 0,
                nr: 1,
                pf: false,
            }
        ));
    }

    #[test]
    fn ax25_control_receive_ready() {
        // RR: bits 0-1 = 01, kind bits 2-3 = 00.
        let control = Ax25Control::from_byte(0x01);
        assert!(matches!(
            control,
            Ax25Control::Supervisory {
                kind: SupervisoryKind::ReceiveReady,
                nr: 0,
                pf: false,
            }
        ));
    }

    #[test]
    fn ax25_packet_command_response_classification() {
        // APRS frames have dest C-bit=1, src C-bit=0 → Command.
        let mut packet = Ax25Packet::ui_frame(
            Ax25Address::new("N0CALL", 7),
            Ax25Address::new("APRS", 0),
            vec![],
            b"!test".to_vec(),
        );
        packet.destination.c_bit = true;
        packet.source.c_bit = false;
        assert_eq!(packet.command_response(), CommandResponse::Command);

        packet.destination.c_bit = false;
        packet.source.c_bit = true;
        assert_eq!(packet.command_response(), CommandResponse::Response);

        packet.destination.c_bit = false;
        packet.source.c_bit = false;
        assert_eq!(packet.command_response(), CommandResponse::Legacy);
    }

    #[test]
    fn ax25_packet_typed_accessors() {
        let packet = Ax25Packet {
            source: Ax25Address::new("N0CALL", 7),
            destination: Ax25Address::new("APRS", 0),
            digipeaters: vec![],
            control: 0x03,
            protocol: 0xF0,
            info: b"!test".to_vec(),
        };
        assert!(packet.is_ui());
        assert_eq!(packet.pid(), Ax25Pid::NoLayer3);
    }

    // ---- Ax25Address typed accessors ----

    #[test]
    fn ax25_address_try_new_rejects_invalid_callsign() {
        assert!(Ax25Address::try_new("", 0).is_err());
        assert!(Ax25Address::try_new("TOOLONG", 0).is_err());
        assert!(Ax25Address::try_new("N0-CAL", 0).is_err());
    }

    #[test]
    fn ax25_address_try_new_rejects_invalid_ssid() {
        assert!(Ax25Address::try_new("N0CALL", 16).is_err());
    }

    #[test]
    fn ax25_address_try_new_accepts_mixed_case() {
        let addr = Ax25Address::try_new("n0call", 7).unwrap();
        assert_eq!(addr.callsign, "N0CALL");
        assert_eq!(addr.ssid, 7);
    }

    #[test]
    fn decode_ax25_address_rejects_non_ascii() {
        // Byte 0x24 >> 1 = 0x12 is a control char, not alphanumeric.
        let bytes = [
            b'N' << 1,
            b'0' << 1,
            0x24,
            b'A' << 1,
            b'L' << 1,
            b'L' << 1,
            0x60,
        ];
        let err = decode_ax25_address(&bytes);
        assert!(matches!(err, Err(Ax25Error::InvalidCallsignByte(0x12))));
    }

    #[test]
    fn build_query_response_roundtrip() {
        let source = test_source();
        let wire = build_query_response_position(
            &source,
            35.258,
            -97.755,
            '/',
            '>',
            "QRY resp",
            &default_digipeater_path(),
        );
        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let data = parse_aprs_data(&packet.info).unwrap();
        match data {
            AprsData::Position(pos) => {
                assert!((pos.latitude - 35.258).abs() < 0.01);
                assert!((pos.longitude - (-97.755)).abs() < 0.01);
                assert!(pos.comment.contains("QRY resp"));
            }
            other => panic!("expected Position, got {other:?}"),
        }
    }
}
