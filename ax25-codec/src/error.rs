//! AX.25 codec error type.

use alloc::string::String;

use thiserror::Error;

/// Errors produced by AX.25 frame encode/decode and address construction.
///
/// See the [packet example](crate#encoding-and-decoding-a-packet) and
/// [construction errors](crate#frame-validation).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum Ax25Error {
    /// Packet is too short to contain required AX.25 fields.
    #[error("AX.25 packet too short")]
    PacketTooShort,
    /// A repeater address ends before all seven octets are available.
    #[error("AX.25 repeater address is shorter than seven octets")]
    InvalidAddressLength,
    /// An address-extension bit is set before the final SSID octet.
    #[error("AX.25 address-extension bit precedes the final address octet")]
    InvalidAddressExtension,
    /// A textual AX.25 address is not in canonical `CALLSIGN` or
    /// `CALLSIGN-SSID` form.
    #[error("invalid AX.25 address: {0}")]
    InvalidAddress(String),
    /// A required control field is missing after the address block.
    #[error("AX.25 missing a required control field after the address block")]
    MissingControlFields,
    /// A frame family that does not carry a PID was given one.
    #[error("AX.25 control byte {control:#04x} does not permit a protocol identifier")]
    UnexpectedProtocolIdentifier {
        /// The control byte that determines the frame family.
        control: u8,
    },
    /// A frame family that requires a PID was not given one.
    #[error("AX.25 control byte {control:#04x} requires a protocol identifier")]
    MissingProtocolIdentifier {
        /// The control byte that determines the frame family.
        control: u8,
    },
    /// A `0xFF` PID escape prefix is missing its required following octet.
    #[error("AX.25 escaped protocol identifier is missing its required extension octet")]
    MissingProtocolIdentifierExtension,
    /// A frame type that forbids an information field was given one.
    #[error(
        "AX.25 control byte {control:#04x} does not permit an information field ({length} bytes supplied)"
    )]
    UnexpectedInformationField {
        /// The control byte that determines the frame type.
        control: u8,
        /// Number of unexpected information bytes.
        length: usize,
    },
    /// A path exceeds the codec's eight-digipeater capacity.
    #[error("AX.25 path has more than eight digipeater addresses")]
    TooManyDigipeaters,
    /// A requested insertion position is beyond the end of the current
    /// digipeater path. Insertion at `len` is valid and appends an entry.
    #[error("AX.25 digipeater insertion index {index} exceeds path length {len}")]
    DigipeaterIndexOutOfBounds {
        /// The requested insertion index.
        index: usize,
        /// The path length when insertion was attempted.
        len: usize,
    },
    /// A callsign byte is neither ASCII alphanumeric nor space padding,
    /// or an alphanumeric character occurs after padding has started.
    #[error("AX.25 callsign contains invalid or misplaced character {0:#04x}")]
    InvalidCallsignByte(u8),
    /// A modulo-8 send or receive sequence number is outside 0..=7.
    #[error("invalid AX.25 modulo-8 sequence number: {0}")]
    InvalidSequenceNumber(u8),
    /// A raw unnumbered control pattern is not an unknown canonical U-frame kind.
    #[error("invalid unknown AX.25 unnumbered control pattern: {0:#04x}")]
    InvalidUnknownUnnumberedKind(u8),
    /// Callsign is outside the 1-6 ASCII uppercase/digit range in AX.25 §3.12.
    #[error("invalid callsign: {0}")]
    InvalidCallsign(String),
    /// SSID is outside the 0-15 range in AX.25 v2.2 §3.12.
    #[error("invalid SSID: {0}")]
    InvalidSsid(u8),
}
