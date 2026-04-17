//! AX.25 codec error type.

use alloc::string::String;

use thiserror::Error;

/// Errors produced by AX.25 frame encode/decode and address construction.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
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
    /// Callsign is outside the 1-6 ASCII uppercase/digit range per AX.25 v2.2 §3.2.
    #[error("invalid callsign: {0}")]
    InvalidCallsign(String),
    /// SSID is outside the 0-15 range per AX.25 v2.2 §3.2.
    #[error("invalid SSID: {0}")]
    InvalidSsid(u8),
}
