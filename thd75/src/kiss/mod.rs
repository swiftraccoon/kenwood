//! KISS TNC (Terminal Node Controller) protocol for AX.25 packet operations.
//!
//! The TH-D75's built-in TNC supports KISS framing for sending and
//! receiving APRS packets. When KISS mode is enabled (via `[F], [LIST]`
//! on the radio, cycling to "KISS mode ON"), the radio accepts and
//! produces standard KISS frames over USB or Bluetooth.
//!
//! # TH-D75 KISS TNC specifications (per Operating Tips §2.7.2)
//!
//! - TX buffer: 3 KB, RX buffer: 4 KB
//! - Speeds: 1200 bps (AFSK) and 9600 bps (GMSK)
//! - KISS commands: Data Frame (`0x00`), TXDELAY (`0x01`, 0–120, units
//!   of 10 ms), Persistence (`0x02`, 0–255, default 128), `SlotTime`
//!   (`0x03`, 0–250, units of 10 ms, default 10), `TXtail` (`0x04`,
//!   0–255, default 3), `FullDuplex` (`0x05`, 0=half/default, nonzero=
//!   full), `SetHardware` (`0x06`, 0 or 0x23=1200 bps, 0x05 or 0x26=9600 bps),
//!   Return (`0xFF`)
//! - When KISS is active, all APRS menu configs are inactive except
//!   Menu 505 (Data Speed) and Menu 506 (Data Band)
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
    let type_byte = (frame.port << 4) | (frame.command & 0x0F);
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
    let port = type_byte >> 4;
    let command = type_byte & 0x0F;

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
}

impl fmt::Display for AprsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFormat => write!(f, "invalid APRS format"),
            Self::InvalidCoordinates => write!(f, "invalid APRS coordinates"),
        }
    }
}

impl std::error::Error for AprsError {}

/// A parsed APRS position report.
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
/// Supports the two most common APRS position formats:
/// - `!` / `=` — Position without/with messaging (uncompressed)
/// - `/` / `@` — Position with timestamp (uncompressed)
///
/// Compressed positions and Mic-E encoding are not yet supported.
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
            if info.len() < 20 {
                return Err(AprsError::InvalidFormat);
            }
            &info[1..]
        }
        // Position with timestamp: / or @
        // Timestamp is 7 characters after the type byte
        b'/' | b'@' => {
            if info.len() < 27 {
                return Err(AprsError::InvalidFormat);
            }
            &info[8..]
        }
        _ => return Err(AprsError::InvalidFormat),
    };

    // Uncompressed format: lat(8) sym_table(1) lon(9) sym_code(1) [comment]
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
        comment,
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
        let frame = KissFrame {
            port: 0,
            // CMD_RETURN is 0xFF; low nibble = 0x0F
            command: CMD_RETURN & 0x0F,
            data: vec![],
        };
        let encoded = encode_kiss_frame(&frame);
        assert_eq!(encoded, vec![FEND, 0x0F, FEND]);
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
}
