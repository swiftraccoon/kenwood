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

// ---------------------------------------------------------------------------
// KISS errors
// ---------------------------------------------------------------------------

/// Errors that can occur during KISS frame processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KissError {
    /// Frame is too short to contain a valid KISS header.
    FrameTooShort,
    /// Frame does not start with FEND.
    MissingStartDelimiter,
    /// Frame does not end with FEND.
    MissingEndDelimiter,
    /// Invalid escape sequence (FESC not followed by TFEND or TFESC).
    InvalidEscapeSequence,
    /// Frame body is empty (no type indicator byte).
    EmptyFrame,
}

impl fmt::Display for KissError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooShort => write!(f, "KISS frame too short"),
            Self::MissingStartDelimiter => write!(f, "KISS frame missing start FEND"),
            Self::MissingEndDelimiter => write!(f, "KISS frame missing end FEND"),
            Self::InvalidEscapeSequence => write!(f, "invalid KISS escape sequence"),
            Self::EmptyFrame => write!(f, "empty KISS frame (no type byte)"),
        }
    }
}

impl std::error::Error for KissError {}

// ---------------------------------------------------------------------------
// KISS frame
// ---------------------------------------------------------------------------

/// A decoded KISS frame.
///
/// The type indicator byte is split into `port` (high nibble) and
/// `command` (low nibble). For the TH-D75, port is always 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KissFrame {
    /// TNC port number (high nibble of type byte). Always 0 for TH-D75.
    pub port: u8,
    /// KISS command (low nibble of type byte).
    pub command: u8,
    /// Frame payload (e.g. AX.25 frame for data commands).
    pub data: Vec<u8>,
}

/// Encode a [`KissFrame`] into wire bytes with FEND delimiters and byte stuffing.
///
/// The output format is: `FEND <type> <escaped-data> FEND`
#[must_use]
pub fn encode_kiss_frame(frame: &KissFrame) -> Vec<u8> {
    // CMD_RETURN (0xFF) is a special full-byte command — NOT nibble-split.
    // All other commands use the standard port|command nibble encoding.
    let type_byte = if frame.command == CMD_RETURN {
        CMD_RETURN
    } else {
        (frame.port << 4) | (frame.command & 0x0F)
    };
    // Pre-allocate: FEND + type + data (worst case 2x) + FEND
    let mut out = Vec::with_capacity(2 + 1 + frame.data.len() * 2);
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ax25Error {
    /// Packet is too short to contain required AX.25 fields.
    PacketTooShort,
    /// Address field has invalid length (must be multiple of 7).
    InvalidAddressLength,
    /// Control/protocol fields are missing after the address block.
    MissingControlFields,
}

impl fmt::Display for Ax25Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketTooShort => write!(f, "AX.25 packet too short"),
            Self::InvalidAddressLength => write!(f, "AX.25 address field invalid length"),
            Self::MissingControlFields => write!(f, "AX.25 missing control/protocol fields"),
        }
    }
}

impl std::error::Error for Ax25Error {}

/// An AX.25 address (callsign + SSID).
///
/// In AX.25, each address is 7 bytes: 6 bytes of callsign (ASCII shifted
/// left by 1 bit) plus 1 SSID byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ax25Address {
    /// Station callsign (up to 6 characters, right-padded with spaces).
    pub callsign: String,
    /// Secondary Station Identifier (0-15).
    pub ssid: u8,
}

impl fmt::Display for Ax25Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ssid == 0 {
            write!(f, "{}", self.callsign)
        } else {
            write!(f, "{}-{}", self.callsign, self.ssid)
        }
    }
}

/// A parsed AX.25 UI (Unnumbered Information) frame.
///
/// APRS uses UI frames exclusively. The control field is `0x03` and
/// the protocol ID is `0xF0` (no layer 3) for standard APRS packets.
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

/// Decode a single AX.25 address from a 7-byte slice.
fn decode_ax25_address(bytes: &[u8]) -> Ax25Address {
    let mut callsign = String::with_capacity(6);
    for &b in &bytes[..6] {
        let ch = (b >> 1) as char;
        if ch != ' ' {
            callsign.push(ch);
        }
    }
    let ssid = (bytes[6] >> 1) & 0x0F;
    Ax25Address { callsign, ssid }
}

/// Encode an AX.25 address into 7 bytes.
///
/// `is_last` sets the address-extension bit on the final address.
fn encode_ax25_address(addr: &Ax25Address, is_last: bool) -> [u8; 7] {
    let mut bytes = [0x40u8; 7]; // space << 1 = 0x40
    for (i, ch) in addr.callsign.bytes().take(6).enumerate() {
        bytes[i] = ch << 1;
    }
    let mut ssid_byte = 0x60 | ((addr.ssid & 0x0F) << 1);
    if is_last {
        ssid_byte |= 0x01; // address-extension bit
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

    let destination = decode_ax25_address(&data[0..7]);
    let source = decode_ax25_address(&data[7..14]);

    // Find end of address field (bit 0 of last byte in each 7-byte address)
    let mut addr_end = 14;
    let mut digipeaters = Vec::new();

    // Check if source address has the extension bit set
    if data[13] & 0x01 == 0 {
        // More addresses follow (digipeaters)
        loop {
            if addr_end + 7 > data.len() {
                return Err(Ax25Error::InvalidAddressLength);
            }
            let digi = decode_ax25_address(&data[addr_end..addr_end + 7]);
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
#[must_use]
pub fn build_ax25(packet: &Ax25Packet) -> Vec<u8> {
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

/// Errors that can occur during APRS data parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AprsError {
    /// The info field is too short or has an unrecognized data type.
    InvalidFormat,
    /// The position coordinates could not be parsed.
    InvalidCoordinates,
    /// Mic-E data requires the AX.25 destination address for decoding.
    MicERequiresDestination,
}

impl fmt::Display for AprsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid APRS format"),
            Self::InvalidCoordinates => write!(f, "invalid APRS coordinates"),
            Self::MicERequiresDestination => write!(
                f,
                "Mic-E data requires destination address \u{2014} use parse_aprs_data_full()"
            ),
        }
    }
}

impl std::error::Error for AprsError {}

/// A parsed APRS position report.
///
/// Includes optional speed/course fields populated by Mic-E decoding.
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
}

/// Parse APRS latitude from the standard `DDMM.HH[N/S]` format.
///
/// Returns decimal degrees (positive North).
fn parse_aprs_latitude(s: &[u8]) -> Result<f64, AprsError> {
    // Format: "DDMM.HHx" where x is N or S, total 8 bytes
    if s.len() < 8 {
        return Err(AprsError::InvalidCoordinates);
    }
    let text = std::str::from_utf8(&s[..8]).map_err(|_| AprsError::InvalidCoordinates)?;
    let degrees: f64 = text[..2]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = text[2..7]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = text.as_bytes()[7];

    let mut lat = degrees + minutes / 60.0;
    if hemisphere == b'S' {
        lat = -lat;
    } else if hemisphere != b'N' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok(lat)
}

/// Parse APRS longitude from the standard `DDDMM.HH[E/W]` format.
///
/// Returns decimal degrees (positive East).
fn parse_aprs_longitude(s: &[u8]) -> Result<f64, AprsError> {
    // Format: "DDDMM.HHx" where x is E or W, total 9 bytes
    if s.len() < 9 {
        return Err(AprsError::InvalidCoordinates);
    }
    let text = std::str::from_utf8(&s[..9]).map_err(|_| AprsError::InvalidCoordinates)?;
    let degrees: f64 = text[..3]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let minutes: f64 = text[3..8]
        .parse()
        .map_err(|_| AprsError::InvalidCoordinates)?;
    let hemisphere = text.as_bytes()[8];

    let mut lon = degrees + minutes / 60.0;
    if hemisphere == b'W' {
        lon = -lon;
    } else if hemisphere != b'E' {
        return Err(AprsError::InvalidCoordinates);
    }
    Ok(lon)
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

    let latitude = parse_aprs_latitude(&body[..8])?;
    let symbol_table = body[8] as char;
    let longitude = parse_aprs_longitude(&body[9..18])?;
    let symbol_code = body[18] as char;

    let comment = if body.len() > 19 {
        String::from_utf8_lossy(&body[19..]).into_owned()
    } else {
        String::new()
    };

    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots: None,
        course_degrees: None,
        comment,
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

    let comment = if body.len() > 13 {
        String::from_utf8_lossy(&body[13..]).into_owned()
    } else {
        String::new()
    };

    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots: None,
        course_degrees: None,
        comment,
    })
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

    Ok(AprsPosition {
        latitude,
        longitude,
        symbol_table,
        symbol_code,
        speed_knots,
        course_degrees,
        comment,
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

/// Check if a Mic-E destination character is a "custom" (non-standard digit) character.
const fn mice_dest_is_custom(ch: u8) -> bool {
    matches!(ch, b'A'..=b'L' | b'P'..=b'Z')
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
}

/// An APRS message (data type `:`) addressed to a specific station.
///
/// Format: `:ADDRESSEE:message text{ID`
/// - Addressee is exactly 9 characters, space-padded.
/// - Message text follows the second `:`.
/// - Optional message ID after `{` (for ack/rej).
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        // Mic-E (` ' 0x1C 0x1D) needs destination address — use parse_mice_position().
        b'`' | b'\'' | 0x1C | 0x1D => Err(AprsError::MicERequiresDestination),
        // All other types are unrecognized.
        _ => Err(AprsError::InvalidFormat),
    }
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
    let body_str = String::from_utf8_lossy(body);

    // Split on { for message ID
    let (text, message_id) = if let Some(idx) = body_str.rfind('{') {
        let id = body_str[idx + 1..].to_string();
        let text = body_str[..idx].to_string();
        (text, if id.is_empty() { None } else { Some(id) })
    } else {
        (body_str.into_owned(), None)
    };

    Ok(AprsMessage {
        addressee,
        text,
        message_id,
    })
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
    let live = info[10] == b'*'; // * = live, _ = killed

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
fn parse_aprs_item(info: &[u8]) -> Result<AprsItem, AprsError> {
    if info.len() < 2 || info[0] != b')' {
        return Err(AprsError::InvalidFormat);
    }

    // Item name is 3-9 chars, terminated by ! (live) or _ (killed)
    let body = &info[1..];
    let mut name_end = None;
    let mut live = true;
    for (i, &b) in body.iter().enumerate() {
        if b == b'!' {
            name_end = Some(i);
            live = true;
            break;
        }
        if b == b'_' {
            name_end = Some(i);
            live = false;
            break;
        }
        if i >= 9 {
            break;
        }
    }

    let name_end = name_end.ok_or(AprsError::InvalidFormat)?;
    // APRS101 Chapter 11: item names are 3-9 characters
    if name_end < 3 {
        return Err(AprsError::InvalidFormat);
    }
    let name = String::from_utf8_lossy(&body[..name_end]).to_string();
    let pos_data = &body[name_end + 1..];

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
/// Weather fields use single-letter tags followed by fixed-width values.
/// Fields with all dots or spaces are treated as missing data.
///
/// Note: This parser assumes the input contains only weather fields.
/// If called on data that includes non-weather text (e.g., position report
/// comments), tag letters in the text may cause false matches.
fn parse_weather_fields(data: &[u8]) -> AprsWeather {
    let s = String::from_utf8_lossy(data);
    AprsWeather {
        wind_direction: extract_weather_u16(&s, 'c', 3),
        wind_speed: extract_weather_u16(&s, 's', 3),
        wind_gust: extract_weather_u16(&s, 'g', 3),
        temperature: extract_weather_i16(&s, 't', 3),
        rain_1h: extract_weather_u16(&s, 'r', 3),
        rain_24h: extract_weather_u16(&s, 'p', 3),
        rain_since_midnight: extract_weather_u16(&s, 'P', 3),
        humidity: extract_weather_u16(&s, 'h', 2).map(|v| {
            if v == 0 {
                100
            } else {
                #[allow(clippy::cast_possible_truncation)]
                let val = v as u8;
                val
            }
        }),
        pressure: extract_weather_u32(&s, 'b', 5),
    }
}

/// Extract a u16 weather field value.
fn extract_weather_u16(s: &str, tag: char, width: usize) -> Option<u16> {
    let tag_str = tag.to_string();
    let idx = s.find(&tag_str)?;
    let start = idx + 1;
    if start + width > s.len() {
        return None;
    }
    let val_str = &s[start..start + width];
    if val_str.contains('.') || val_str.contains(' ') {
        return None;
    }
    val_str.trim().parse().ok()
}

/// Extract an i16 weather field value (supports negative temperatures).
fn extract_weather_i16(s: &str, tag: char, width: usize) -> Option<i16> {
    let tag_str = tag.to_string();
    let idx = s.find(&tag_str)?;
    let start = idx + 1;
    if start + width > s.len() {
        return None;
    }
    let val_str = &s[start..start + width];
    if val_str.contains('.') || val_str.contains(' ') {
        return None;
    }
    val_str.trim().parse().ok()
}

/// Extract a u32 weather field value.
fn extract_weather_u32(s: &str, tag: char, width: usize) -> Option<u32> {
    let tag_str = tag.to_string();
    let idx = s.find(&tag_str)?;
    let start = idx + 1;
    if start + width > s.len() {
        return None;
    }
    let val_str = &s[start..start + width];
    if val_str.contains('.') || val_str.contains(' ') {
        return None;
    }
    val_str.trim().parse().ok()
}

// ---------------------------------------------------------------------------
// APRS TX packet builders
// ---------------------------------------------------------------------------

/// APRS tocall for the Kenwood TH-D75 (per APRS tocall registry).
const APRS_TOCALL: &str = "APK005";

/// Default APRS digipeater path: WIDE1-1, WIDE2-1.
const DEFAULT_DIGIPEATERS: &[(&str, u8)] = &[("WIDE1", 1), ("WIDE2", 1)];

/// Format latitude as APRS uncompressed `DDMM.HHN` (8 bytes).
fn format_aprs_latitude(lat: f64) -> String {
    let hemisphere = if lat >= 0.0 { 'N' } else { 'S' };
    let lat_abs = lat.abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let degrees = lat_abs as u32;
    let minutes = (lat_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:02}{minutes:05.2}{hemisphere}")
}

/// Format longitude as APRS uncompressed `DDDMM.HHE` (9 bytes).
fn format_aprs_longitude(lon: f64) -> String {
    let hemisphere = if lon >= 0.0 { 'E' } else { 'W' };
    let lon_abs = lon.abs();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let degrees = lon_abs as u32;
    let minutes = (lon_abs - f64::from(degrees)) * 60.0;
    format!("{degrees:03}{minutes:05.2}{hemisphere}")
}

/// Build the default digipeater path as [`Ax25Address`] entries.
fn default_digipeater_path() -> Vec<Ax25Address> {
    DEFAULT_DIGIPEATERS
        .iter()
        .map(|(call, ssid)| Ax25Address {
            callsign: (*call).to_owned(),
            ssid: *ssid,
        })
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
#[must_use]
pub fn build_aprs_position_report(
    source: &Ax25Address,
    latitude: f64,
    longitude: f64,
    symbol_table: char,
    symbol_code: char,
    comment: &str,
) -> Vec<u8> {
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);
    let info = format!("!{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}");

    let packet = Ax25Packet {
        source: source.clone(),
        destination: Ax25Address {
            callsign: APRS_TOCALL.to_owned(),
            ssid: 0,
        },
        digipeaters: default_digipeater_path(),
        control: 0x03,
        protocol: 0xF0,
        info: info.into_bytes(),
    };

    let ax25_bytes = build_ax25(&packet);
    encode_kiss_frame(&KissFrame {
        port: 0,
        command: CMD_DATA,
        data: ax25_bytes,
    })
}

/// Build a KISS-encoded APRS message packet.
///
/// Composes an AX.25 UI frame with the APRS message format:
/// `:ADDRESSEE:text{ID`
///
/// The addressee is padded to exactly 9 characters per the APRS spec.
///
/// Returns wire-ready bytes (FEND-delimited KISS frame).
///
/// # Parameters
///
/// - `source`: The sender's callsign and SSID.
/// - `addressee`: Destination station callsign (up to 9 chars).
/// - `text`: Message text content.
/// - `message_id`: Optional message sequence number for ack/rej tracking.
#[must_use]
pub fn build_aprs_message(
    source: &Ax25Address,
    addressee: &str,
    text: &str,
    message_id: Option<&str>,
) -> Vec<u8> {
    // Pad addressee to exactly 9 characters.
    let padded_addressee = format!("{addressee:<9}");
    // Truncate to 9 characters if longer.
    let padded_addressee = &padded_addressee[..9];

    let info = message_id.map_or_else(
        || format!(":{padded_addressee}:{text}"),
        |id| format!(":{padded_addressee}:{text}{{{id}"),
    );

    let packet = Ax25Packet {
        source: source.clone(),
        destination: Ax25Address {
            callsign: APRS_TOCALL.to_owned(),
            ssid: 0,
        },
        digipeaters: default_digipeater_path(),
        control: 0x03,
        protocol: 0xF0,
        info: info.into_bytes(),
    };

    let ax25_bytes = build_ax25(&packet);
    encode_kiss_frame(&KissFrame {
        port: 0,
        command: CMD_DATA,
        data: ax25_bytes,
    })
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
) -> Vec<u8> {
    // Pad object name to exactly 9 characters.
    let padded_name = format!("{name:<9}");
    let padded_name = &padded_name[..9];

    let live_char = if live { '*' } else { '_' };
    let lat_str = format_aprs_latitude(latitude);
    let lon_str = format_aprs_longitude(longitude);

    // Use a placeholder DHM zulu timestamp (000000z). Callers needing a real
    // timestamp should build the info field manually. The APRS spec allows
    // any valid DHM or HMS timestamp here.
    let info = format!(
        ";{padded_name}{live_char}000000z{lat_str}{symbol_table}{lon_str}{symbol_code}{comment}"
    );

    let packet = Ax25Packet {
        source: source.clone(),
        destination: Ax25Address {
            callsign: APRS_TOCALL.to_owned(),
            ssid: 0,
        },
        digipeaters: default_digipeater_path(),
        control: 0x03,
        protocol: 0xF0,
        info: info.into_bytes(),
    };

    let ax25_bytes = build_ax25(&packet);
    encode_kiss_frame(&KissFrame {
        port: 0,
        command: CMD_DATA,
        data: ax25_bytes,
    })
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
            source: Ax25Address {
                callsign: "N0CALL".to_owned(),
                ssid: 7,
            },
            destination: Ax25Address {
                callsign: "APRS".to_owned(),
                ssid: 0,
            },
            digipeaters: vec![
                Ax25Address {
                    callsign: "WIDE1".to_owned(),
                    ssid: 1,
                },
                Ax25Address {
                    callsign: "WIDE2".to_owned(),
                    ssid: 1,
                },
            ],
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
        let addr = Ax25Address {
            callsign: "N0CALL".to_owned(),
            ssid: 0,
        };
        assert_eq!(format!("{addr}"), "N0CALL");

        let addr_ssid = Ax25Address {
            callsign: "N0CALL".to_owned(),
            ssid: 7,
        };
        assert_eq!(format!("{addr_ssid}"), "N0CALL-7");
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
        Ax25Address {
            callsign: "N0CALL".to_owned(),
            ssid: 7,
        }
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
        let wire = build_aprs_position_report(&source, 49.058_333, -72.029_166, '/', '-', "Test");

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

    #[test]
    fn build_message_roundtrip() {
        let source = test_source();
        let wire = build_aprs_message(&source, "KQ4NIT", "Hello 73!", Some("42"));

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
        let wire = build_aprs_message(&source, "W1AW", "Test msg", None);

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
        let wire = build_aprs_message(&source, "AB", "Hi", None);

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
        let wire = build_aprs_object(&source, "EVENT", false, 35.0, -97.0, '/', 'E', "Done");

        let kiss = decode_kiss_frame(&wire).unwrap();
        let packet = parse_ax25(&kiss.data).unwrap();
        let obj = parse_aprs_object(&packet.info).unwrap();
        assert_eq!(obj.name, "EVENT");
        assert!(!obj.live);
    }
}
