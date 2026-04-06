//! MMDVM frame codec: encode, decode, builders, and response parsing.
//!
//! Implements the binary frame protocol defined in the MMDVM Specification
//! (20150922). Each frame is a length-prefixed binary message starting with
//! the `0xE0` marker byte.

use std::fmt;

use super::dstar::DStarHeader;

/// Start byte for all MMDVM frames (MMDVM Specification 20150922).
pub const START_BYTE: u8 = 0xE0;

/// Minimum frame length: start + length + command (MMDVM Specification 20150922).
pub const MIN_FRAME_LEN: u8 = 3;

// ---------------------------------------------------------------------------
// Command constants (MMDVM Specification 20150922)
// ---------------------------------------------------------------------------

/// Request firmware version information.
pub const CMD_GET_VERSION: u8 = 0x00;
/// Request current modem status.
pub const CMD_GET_STATUS: u8 = 0x01;
/// Set modem configuration parameters.
pub const CMD_SET_CONFIG: u8 = 0x02;
/// Set modem operating mode.
pub const CMD_SET_MODE: u8 = 0x03;

/// D-STAR header frame (41-byte header with CRC).
pub const CMD_DSTAR_HEADER: u8 = 0x10;
/// D-STAR voice data frame (9 AMBE + 3 slow data).
pub const CMD_DSTAR_DATA: u8 = 0x11;
/// D-STAR signal lost indication.
pub const CMD_DSTAR_LOST: u8 = 0x12;
/// D-STAR end-of-transmission marker.
pub const CMD_DSTAR_EOT: u8 = 0x13;

/// Acknowledgement response.
pub const CMD_ACK: u8 = 0x70;
/// Negative acknowledgement response.
pub const CMD_NAK: u8 = 0x7F;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors specific to MMDVM frame encoding and decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdvmError {
    /// Frame is shorter than the minimum 3 bytes.
    FrameTooShort {
        /// Number of bytes available.
        len: usize,
    },
    /// Frame does not start with the expected `0xE0` marker.
    InvalidStartByte {
        /// The byte found at position 0.
        got: u8,
    },
    /// The length field is less than the minimum of 3.
    InvalidLength {
        /// The length field value.
        len: u8,
    },
    /// The CRC in a D-STAR header did not match the computed value.
    CrcMismatch {
        /// The CRC stored in the header.
        expected: u16,
        /// The CRC computed over the header data.
        computed: u16,
    },
    /// A callsign field contained bytes that are not valid ASCII.
    InvalidCallsign {
        /// Name of the field (e.g. "rpt1", "`ur_call`").
        field: &'static str,
    },
    /// A D-STAR header payload has the wrong size (expected 41 bytes).
    InvalidHeaderSize {
        /// Actual payload size.
        len: usize,
    },
    /// An unknown NAK reason code was received.
    UnknownNakReason {
        /// The raw reason byte.
        code: u8,
    },
    /// An unknown modem state was received.
    UnknownModemState {
        /// The raw state byte.
        code: u8,
    },
    /// A D-STAR data payload has the wrong size (expected 12 bytes).
    InvalidDataSize {
        /// Actual payload size.
        len: usize,
    },
}

impl fmt::Display for MmdvmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooShort { len } => {
                write!(f, "frame too short: {len} bytes (minimum 3)")
            }
            Self::InvalidStartByte { got } => {
                write!(f, "invalid start byte: 0x{got:02X} (expected 0xE0)")
            }
            Self::InvalidLength { len } => {
                write!(f, "invalid length field: {len} (minimum 3)")
            }
            Self::CrcMismatch { expected, computed } => {
                write!(
                    f,
                    "CRC mismatch: header contains 0x{expected:04X}, computed 0x{computed:04X}"
                )
            }
            Self::InvalidCallsign { field } => {
                write!(f, "invalid callsign in field \"{field}\": not valid ASCII")
            }
            Self::InvalidHeaderSize { len } => {
                write!(f, "invalid D-STAR header size: {len} bytes (expected 41)")
            }
            Self::UnknownNakReason { code } => {
                write!(f, "unknown NAK reason code: 0x{code:02X}")
            }
            Self::UnknownModemState { code } => {
                write!(f, "unknown modem state: 0x{code:02X}")
            }
            Self::InvalidDataSize { len } => {
                write!(f, "invalid D-STAR data size: {len} bytes (expected 12)")
            }
        }
    }
}

impl std::error::Error for MmdvmError {}

// ---------------------------------------------------------------------------
// Frame struct
// ---------------------------------------------------------------------------

/// A decoded MMDVM frame with command byte and payload.
///
/// Corresponds to the wire format `[0xE0, length, command, payload...]`
/// defined in the MMDVM Specification (20150922).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MmdvmFrame {
    /// Command or response type byte.
    pub command: u8,
    /// Payload bytes (may be empty).
    pub payload: Vec<u8>,
}

/// Encode a frame to wire bytes: `[0xE0, length, command, payload...]`.
///
/// The length byte accounts for the start byte, length byte, command byte,
/// and all payload bytes (MMDVM Specification 20150922).
///
/// # Panics
///
/// Panics if the payload exceeds 252 bytes (the maximum that fits in a
/// single-byte length field minus the 3-byte header).
#[must_use]
pub fn encode_frame(frame: &MmdvmFrame) -> Vec<u8> {
    let length = u8::try_from(3 + frame.payload.len())
        .expect("payload must fit in a single MMDVM frame (max 252 bytes)");
    let mut buf = Vec::with_capacity(usize::from(length));
    buf.push(START_BYTE);
    buf.push(length);
    buf.push(frame.command);
    buf.extend_from_slice(&frame.payload);
    buf
}

/// Decode one frame from a byte buffer.
///
/// Returns `Some((frame, bytes_consumed))` if a complete frame is available
/// at the start of `data`, or `None` if more bytes are needed. Returns an
/// error if the frame is structurally invalid (MMDVM Specification 20150922).
///
/// # Errors
///
/// Returns [`MmdvmError::InvalidStartByte`] if the first byte is not `0xE0`,
/// or [`MmdvmError::InvalidLength`] if the length field is less than 3.
pub fn decode_frame(data: &[u8]) -> Result<Option<(MmdvmFrame, usize)>, MmdvmError> {
    if data.is_empty() {
        return Ok(None);
    }
    if data[0] != START_BYTE {
        return Err(MmdvmError::InvalidStartByte { got: data[0] });
    }
    if data.len() < 2 {
        return Ok(None);
    }
    let length = data[1];
    if length < MIN_FRAME_LEN {
        return Err(MmdvmError::InvalidLength { len: length });
    }
    let frame_len = usize::from(length);
    if data.len() < frame_len {
        return Ok(None);
    }
    let command = data[2];
    let payload = data[3..frame_len].to_vec();
    Ok(Some((MmdvmFrame { command, payload }, frame_len)))
}

// ---------------------------------------------------------------------------
// Builder functions
// ---------------------------------------------------------------------------

/// Build a `GET_VERSION` request frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x03, 0x00]`.
#[must_use]
pub fn build_get_version() -> Vec<u8> {
    vec![START_BYTE, 3, CMD_GET_VERSION]
}

/// Build a `GET_STATUS` request frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x03, 0x01]`.
#[must_use]
pub fn build_get_status() -> Vec<u8> {
    vec![START_BYTE, 3, CMD_GET_STATUS]
}

/// Build a `SET_CONFIG` request frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x09, 0x02, <6 config bytes>]`.
#[must_use]
pub fn build_set_config(config: &MmdvmConfig) -> Vec<u8> {
    vec![
        START_BYTE,
        9,
        CMD_SET_CONFIG,
        config.invert,
        config.mode_flags,
        config.tx_delay,
        config.state as u8,
        config.rx_level,
        config.tx_level,
    ]
}

/// Build a `SET_MODE` request frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x04, 0x03, mode]`.
#[must_use]
pub fn build_set_mode(mode: ModemMode) -> Vec<u8> {
    vec![START_BYTE, 4, CMD_SET_MODE, mode as u8]
}

/// Build a D-STAR header frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x2C, 0x10, <41 header bytes>]`.
#[must_use]
pub fn build_dstar_header(header: &DStarHeader) -> Vec<u8> {
    let encoded = header.encode();
    let mut buf = Vec::with_capacity(44);
    buf.push(START_BYTE);
    buf.push(44);
    buf.push(CMD_DSTAR_HEADER);
    buf.extend_from_slice(&encoded);
    buf
}

/// Build a D-STAR voice data frame (MMDVM Specification 20150922).
///
/// The `data` array contains 9 bytes of AMBE voice data followed by 3 bytes
/// of slow data. Wire format: `[0xE0, 0x0F, 0x11, <12 data bytes>]`.
#[must_use]
pub fn build_dstar_data(data: &[u8; 12]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(15);
    buf.push(START_BYTE);
    buf.push(15);
    buf.push(CMD_DSTAR_DATA);
    buf.extend_from_slice(data);
    buf
}

/// Build a D-STAR end-of-transmission frame (MMDVM Specification 20150922).
///
/// Wire format: `[0xE0, 0x03, 0x13]`.
#[must_use]
pub fn build_dstar_eot() -> Vec<u8> {
    vec![START_BYTE, 3, CMD_DSTAR_EOT]
}

// ---------------------------------------------------------------------------
// Configuration and status types
// ---------------------------------------------------------------------------

/// Modem configuration parameters for `SET_CONFIG` (MMDVM Specification 20150922).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmdvmConfig {
    /// Inversion flags byte.
    pub invert: u8,
    /// Enabled mode bit flags: 0x01 = D-STAR, 0x02 = DMR, 0x04 = Fusion.
    pub mode_flags: u8,
    /// TX delay in 10 ms units.
    pub tx_delay: u8,
    /// Desired modem state after configuration.
    pub state: ModemMode,
    /// RX audio level (0-255).
    pub rx_level: u8,
    /// TX audio level (0-255).
    pub tx_level: u8,
}

/// Modem operating mode (MMDVM Specification 20150922).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ModemMode {
    /// No mode active.
    Idle = 0,
    /// D-STAR digital voice mode.
    DStar = 1,
    /// DMR (Digital Mobile Radio) mode.
    Dmr = 2,
    /// Yaesu System Fusion mode.
    Fusion = 3,
    /// Calibration / test mode.
    Calibration = 4,
}

/// Current modem state reported in `GET_STATUS` (MMDVM Specification 20150922).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModemState {
    /// Modem is idle.
    Idle,
    /// Modem is in D-STAR mode.
    DStar,
    /// Modem is in DMR mode.
    Dmr,
    /// Modem is in Fusion mode.
    Fusion,
    /// Modem is in calibration mode.
    Calibration,
}

impl ModemState {
    /// Parse a raw state byte from the modem (MMDVM Specification 20150922).
    const fn from_byte(b: u8) -> Result<Self, MmdvmError> {
        match b {
            0 => Ok(Self::Idle),
            1 => Ok(Self::DStar),
            2 => Ok(Self::Dmr),
            3 => Ok(Self::Fusion),
            4 => Ok(Self::Calibration),
            _ => Err(MmdvmError::UnknownModemState { code: b }),
        }
    }
}

/// Current modem status from `GET_STATUS` response (MMDVM Specification 20150922).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModemStatus {
    /// Enabled mode bit flags: 0x01 = D-STAR, 0x02 = DMR, 0x04 = Fusion.
    pub enabled_modes: u8,
    /// Current operating state of the modem.
    pub state: ModemState,
    /// Whether the modem is currently transmitting.
    pub tx: bool,
    /// Number of available D-STAR TX buffer slots.
    pub dstar_buffer: u8,
}

/// Reason code for a NAK response (MMDVM Specification 20150922).
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
}

impl NakReason {
    /// Parse a raw reason byte from the modem (MMDVM Specification 20150922).
    const fn from_byte(b: u8) -> Result<Self, MmdvmError> {
        match b {
            1 => Ok(Self::InvalidCommand),
            2 => Ok(Self::WrongMode),
            3 => Ok(Self::CommandTooLong),
            4 => Ok(Self::DataIncorrect),
            5 => Ok(Self::BufferFull),
            _ => Err(MmdvmError::UnknownNakReason { code: b }),
        }
    }
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// A parsed MMDVM response (MMDVM Specification 20150922).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdvmResponse {
    /// Firmware version response.
    Version {
        /// Protocol version number.
        protocol: u8,
        /// Firmware description string.
        description: String,
    },
    /// Current modem status.
    Status(ModemStatus),
    /// Command acknowledged.
    Ack {
        /// The command byte that was acknowledged.
        command: u8,
    },
    /// Negative acknowledgement.
    Nak {
        /// The command byte that was rejected.
        command: u8,
        /// Reason for the rejection.
        reason: NakReason,
    },
    /// Received D-STAR header.
    DStarHeader(DStarHeader),
    /// Received D-STAR voice + slow data frame (9 AMBE + 3 slow data).
    DStarData(
        /// The 12-byte voice + slow data payload.
        [u8; 12],
    ),
    /// D-STAR signal was lost.
    DStarLost,
    /// D-STAR end of transmission.
    DStarEot,
}

/// Parse an [`MmdvmFrame`] into a typed [`MmdvmResponse`].
///
/// Returns the parsed response or an error if the frame contents are
/// invalid or unrecognised (MMDVM Specification 20150922).
///
/// # Errors
///
/// Returns an [`MmdvmError`] if the payload is malformed for the given
/// command type (wrong size, invalid CRC, unknown state/reason codes, etc.).
pub fn parse_response(frame: &MmdvmFrame) -> Result<MmdvmResponse, MmdvmError> {
    match frame.command {
        CMD_GET_VERSION => {
            let protocol = if frame.payload.is_empty() {
                0
            } else {
                frame.payload[0]
            };
            let description = if frame.payload.len() > 1 {
                String::from_utf8_lossy(&frame.payload[1..])
                    .trim_end_matches('\0')
                    .to_owned()
            } else {
                String::new()
            };
            Ok(MmdvmResponse::Version {
                protocol,
                description,
            })
        }
        CMD_GET_STATUS => {
            if frame.payload.len() < 4 {
                return Err(MmdvmError::FrameTooShort {
                    len: frame.payload.len(),
                });
            }
            let enabled_modes = frame.payload[0];
            let state = ModemState::from_byte(frame.payload[1])?;
            let tx = frame.payload[2] != 0;
            let dstar_buffer = frame.payload[3];
            Ok(MmdvmResponse::Status(ModemStatus {
                enabled_modes,
                state,
                tx,
                dstar_buffer,
            }))
        }
        CMD_ACK => {
            let command = frame.payload.first().copied().unwrap_or(0);
            Ok(MmdvmResponse::Ack { command })
        }
        CMD_NAK => {
            let command = frame.payload.first().copied().unwrap_or(0);
            let reason_byte = frame.payload.get(1).copied().unwrap_or(1);
            let reason = NakReason::from_byte(reason_byte)?;
            Ok(MmdvmResponse::Nak { command, reason })
        }
        CMD_DSTAR_HEADER => {
            if frame.payload.len() != 41 {
                return Err(MmdvmError::InvalidHeaderSize {
                    len: frame.payload.len(),
                });
            }
            let mut data = [0u8; 41];
            data.copy_from_slice(&frame.payload[..41]);
            let header = DStarHeader::decode(&data)?;
            Ok(MmdvmResponse::DStarHeader(header))
        }
        CMD_DSTAR_DATA => {
            if frame.payload.len() != 12 {
                return Err(MmdvmError::InvalidDataSize {
                    len: frame.payload.len(),
                });
            }
            let mut data = [0u8; 12];
            data.copy_from_slice(&frame.payload);
            Ok(MmdvmResponse::DStarData(data))
        }
        CMD_DSTAR_LOST => Ok(MmdvmResponse::DStarLost),
        CMD_DSTAR_EOT => Ok(MmdvmResponse::DStarEot),
        _ => Err(MmdvmError::FrameTooShort { len: 0 }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let frame = MmdvmFrame {
            command: CMD_GET_VERSION,
            payload: vec![],
        };
        let wire = encode_frame(&frame);
        assert_eq!(wire, [0xE0, 3, 0x00]);
        let (decoded, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(decoded, frame);
    }

    #[test]
    fn encode_decode_with_payload() {
        let frame = MmdvmFrame {
            command: CMD_DSTAR_DATA,
            payload: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        };
        let wire = encode_frame(&frame);
        assert_eq!(wire.len(), 15);
        assert_eq!(wire[0], START_BYTE);
        assert_eq!(wire[1], 15);
        assert_eq!(wire[2], CMD_DSTAR_DATA);
        let (decoded, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, 15);
        assert_eq!(decoded, frame);
    }

    #[test]
    fn decode_incomplete_returns_none() {
        // Only start byte
        assert!(decode_frame(&[0xE0]).unwrap().is_none());
        // Length says 5, but only 3 bytes present
        assert!(decode_frame(&[0xE0, 5, 0x00]).unwrap().is_none());
        // Empty buffer
        assert!(decode_frame(&[]).unwrap().is_none());
    }

    #[test]
    fn decode_invalid_start_byte() {
        let err = decode_frame(&[0xFF, 3, 0x00]).unwrap_err();
        assert_eq!(err, MmdvmError::InvalidStartByte { got: 0xFF });
    }

    #[test]
    fn decode_invalid_length() {
        let err = decode_frame(&[0xE0, 2, 0x00]).unwrap_err();
        assert_eq!(err, MmdvmError::InvalidLength { len: 2 });
    }

    #[test]
    fn build_get_version_frame() {
        let wire = build_get_version();
        assert_eq!(wire, [0xE0, 3, 0x00]);
    }

    #[test]
    fn build_get_status_frame() {
        let wire = build_get_status();
        assert_eq!(wire, [0xE0, 3, 0x01]);
    }

    #[test]
    fn build_set_mode_dstar() {
        let wire = build_set_mode(ModemMode::DStar);
        assert_eq!(wire, [0xE0, 4, 0x03, 1]);
    }

    #[test]
    fn build_dstar_data_frame() {
        let data: [u8; 12] = [0xAA; 12];
        let wire = build_dstar_data(&data);
        assert_eq!(wire.len(), 15);
        assert_eq!(wire[0], START_BYTE);
        assert_eq!(wire[1], 15);
        assert_eq!(wire[2], CMD_DSTAR_DATA);
        assert_eq!(&wire[3..], &[0xAA; 12]);
    }

    #[test]
    fn build_dstar_eot_frame() {
        let wire = build_dstar_eot();
        assert_eq!(wire, [0xE0, 3, 0x13]);
    }

    #[test]
    fn build_set_config_frame() {
        let config = MmdvmConfig {
            invert: 0x00,
            mode_flags: 0x01,
            tx_delay: 10,
            state: ModemMode::DStar,
            rx_level: 128,
            tx_level: 128,
        };
        let wire = build_set_config(&config);
        assert_eq!(wire, [0xE0, 9, 0x02, 0x00, 0x01, 10, 1, 128, 128]);
    }

    #[test]
    fn parse_version_response() {
        let frame = MmdvmFrame {
            command: CMD_GET_VERSION,
            payload: vec![1, b'M', b'M', b'D', b'V', b'M'],
        };
        let resp = parse_response(&frame).unwrap();
        match resp {
            MmdvmResponse::Version {
                protocol,
                description,
            } => {
                assert_eq!(protocol, 1);
                assert_eq!(description, "MMDVM");
            }
            _ => panic!("expected Version response"),
        }
    }

    #[test]
    fn parse_status_response() {
        let frame = MmdvmFrame {
            command: CMD_GET_STATUS,
            payload: vec![0x01, 0x00, 0x00, 10],
        };
        let resp = parse_response(&frame).unwrap();
        match resp {
            MmdvmResponse::Status(status) => {
                assert_eq!(status.enabled_modes, 0x01);
                assert_eq!(status.state, ModemState::Idle);
                assert!(!status.tx);
                assert_eq!(status.dstar_buffer, 10);
            }
            _ => panic!("expected Status response"),
        }
    }

    #[test]
    fn parse_ack_response() {
        let frame = MmdvmFrame {
            command: CMD_ACK,
            payload: vec![CMD_SET_CONFIG],
        };
        let resp = parse_response(&frame).unwrap();
        assert_eq!(
            resp,
            MmdvmResponse::Ack {
                command: CMD_SET_CONFIG
            }
        );
    }

    #[test]
    fn parse_nak_response() {
        let frame = MmdvmFrame {
            command: CMD_NAK,
            payload: vec![CMD_SET_MODE, 2],
        };
        let resp = parse_response(&frame).unwrap();
        assert_eq!(
            resp,
            MmdvmResponse::Nak {
                command: CMD_SET_MODE,
                reason: NakReason::WrongMode,
            }
        );
    }

    #[test]
    fn parse_dstar_data_response() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let frame = MmdvmFrame {
            command: CMD_DSTAR_DATA,
            payload: data.to_vec(),
        };
        let resp = parse_response(&frame).unwrap();
        assert_eq!(resp, MmdvmResponse::DStarData(data));
    }

    #[test]
    fn parse_dstar_data_wrong_size() {
        let frame = MmdvmFrame {
            command: CMD_DSTAR_DATA,
            payload: vec![1, 2, 3],
        };
        let err = parse_response(&frame).unwrap_err();
        assert_eq!(err, MmdvmError::InvalidDataSize { len: 3 });
    }

    #[test]
    fn parse_dstar_lost_response() {
        let frame = MmdvmFrame {
            command: CMD_DSTAR_LOST,
            payload: vec![],
        };
        assert_eq!(parse_response(&frame).unwrap(), MmdvmResponse::DStarLost);
    }

    #[test]
    fn parse_dstar_eot_response() {
        let frame = MmdvmFrame {
            command: CMD_DSTAR_EOT,
            payload: vec![],
        };
        assert_eq!(parse_response(&frame).unwrap(), MmdvmResponse::DStarEot);
    }

    #[test]
    fn nak_reason_all_codes() {
        assert_eq!(NakReason::from_byte(1).unwrap(), NakReason::InvalidCommand);
        assert_eq!(NakReason::from_byte(2).unwrap(), NakReason::WrongMode);
        assert_eq!(NakReason::from_byte(3).unwrap(), NakReason::CommandTooLong);
        assert_eq!(NakReason::from_byte(4).unwrap(), NakReason::DataIncorrect);
        assert_eq!(NakReason::from_byte(5).unwrap(), NakReason::BufferFull);
        assert!(NakReason::from_byte(0).is_err());
        assert!(NakReason::from_byte(6).is_err());
    }

    #[test]
    fn modem_state_all_codes() {
        assert_eq!(ModemState::from_byte(0).unwrap(), ModemState::Idle);
        assert_eq!(ModemState::from_byte(1).unwrap(), ModemState::DStar);
        assert_eq!(ModemState::from_byte(2).unwrap(), ModemState::Dmr);
        assert_eq!(ModemState::from_byte(3).unwrap(), ModemState::Fusion);
        assert_eq!(ModemState::from_byte(4).unwrap(), ModemState::Calibration);
        assert!(ModemState::from_byte(5).is_err());
    }

    #[test]
    fn decode_frame_with_trailing_data() {
        let wire = vec![0xE0, 3, 0x00, 0xFF, 0xFF];
        let (frame, consumed) = decode_frame(&wire).unwrap().unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(frame.command, CMD_GET_VERSION);
        assert!(frame.payload.is_empty());
        // Remaining bytes should be untouched.
        assert_eq!(&wire[consumed..], &[0xFF, 0xFF]);
    }

    #[test]
    fn build_dstar_header_frame() {
        let header = DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: "DIRECT  ".to_owned(),
            rpt1: "DIRECT  ".to_owned(),
            ur_call: "CQCQCQ  ".to_owned(),
            my_call: "W1AW    ".to_owned(),
            my_suffix: "    ".to_owned(),
        };
        let wire = build_dstar_header(&header);
        assert_eq!(wire.len(), 44);
        assert_eq!(wire[0], START_BYTE);
        assert_eq!(wire[1], 44);
        assert_eq!(wire[2], CMD_DSTAR_HEADER);
    }

    #[test]
    fn error_display_messages() {
        let err = MmdvmError::FrameTooShort { len: 1 };
        assert!(err.to_string().contains("too short"));

        let err = MmdvmError::InvalidStartByte { got: 0xFF };
        assert!(err.to_string().contains("0xFF"));

        let err = MmdvmError::CrcMismatch {
            expected: 0x1234,
            computed: 0x5678,
        };
        assert!(err.to_string().contains("0x1234"));
        assert!(err.to_string().contains("0x5678"));
    }
}
