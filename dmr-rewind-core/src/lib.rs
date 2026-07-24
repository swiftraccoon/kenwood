// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// This is an independent implementation of public wire behavior. Protocol
// facts were cross-checked against BrandMeister go-brandmeister, DigestPlay's
// Rewind.h, and DJ4CK's pyspot_rx; no source code from those projects is
// incorporated here.

//! Sans-I/O codec for the `BrandMeister` REWIND protocol.
//!
//! The crate parses and emits complete UDP datagrams for the self-service Open
//! DMR Terminal service. It performs no DNS, socket, clock, retry, or
//! filesystem I/O.

use sha2::{Digest, Sha256};
use thiserror::Error;

mod codec;

/// REWIND protocol signature.
pub const SIGNATURE: [u8; 8] = *b"REWIND01";
/// Encoded envelope size before the payload.
pub const HEADER_LEN: usize = 18;
/// Default UDP port for the self-service Open DMR Terminal service.
pub const DEFAULT_OPEN_TERMINAL_PORT: u16 = 54_006;
/// Maximum legal UDP datagram size.
pub const MAX_DATAGRAM_LEN: usize = 65_507;
/// Maximum payload accepted by this codec.
pub const MAX_PAYLOAD_LEN: usize = MAX_DATAGRAM_LEN - HEADER_LEN;
/// Size of a SHA-256 authentication response.
pub const AUTHENTICATION_LEN: usize = 32;
/// Size of a DMR header or terminator full-link-control payload.
pub const FULL_LINK_CONTROL_LEN: usize = 12;
/// Size of one REWIND DMR audio payload.
pub const DMR_AUDIO_LEN: usize = 27;
/// Size of a DMR embedded-data payload.
pub const DMR_EMBEDDED_DATA_LEN: usize = 10;
/// Size of a REWIND super-header payload.
pub const SUPER_HEADER_LEN: usize = 32;
/// Service identifier for a self-service Open DMR Terminal.
pub const SERVICE_OPEN_TERMINAL: u8 = 0x21;

/// Packet-envelope flags, including unknown future bits.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PacketFlags(u16);

impl PacketFlags {
    /// No flags.
    pub const NONE: Self = Self(0);
    /// Primary real-time sequence space.
    pub const REAL_TIME_1: Self = Self(1);
    /// Secondary real-time sequence space.
    pub const REAL_TIME_2: Self = Self(2);
    /// Buffered-delivery marker.
    pub const BUFFERING: Self = Self(4);

    /// Preserve raw wire bits.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Return raw wire bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Test whether every bit in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// DMR session type carried by subscriptions and super-headers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionType {
    /// Private voice, wire value 5.
    PrivateVoice,
    /// Group voice, wire value 7.
    GroupVoice,
    /// A future or unsupported wire value.
    Unknown(u32),
}

impl SessionType {
    /// Decode a raw wire value without losing unknown values.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            5 => Self::PrivateVoice,
            7 => Self::GroupVoice,
            value => Self::Unknown(value),
        }
    }

    /// Encode this session type.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        match self {
            Self::PrivateVoice => 5,
            Self::GroupVoice => 7,
            Self::Unknown(value) => value,
        }
    }
}

/// Classification derived from the low six FLCO bits of full link control.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FullLinkControlType {
    /// Group voice, FLCO value 0.
    Group,
    /// Private voice, FLCO value 3.
    Private,
    /// An unknown FLCO value, preserved exactly.
    Unknown(u8),
}

impl FullLinkControlType {
    /// Classify a DMR control octet by its low six FLCO bits.
    ///
    /// Bit 7 is the protection flag and bit 6 is reserved. Callers that need
    /// those bits must retain the original [`FullLinkControl::flco`] field.
    #[must_use]
    pub const fn from_flco(flco: u8) -> Self {
        match flco & 0x3f {
            0 => Self::Group,
            3 => Self::Private,
            value => Self::Unknown(value),
        }
    }

    /// Return the FLCO byte for this classification.
    #[must_use]
    pub const fn as_flco(self) -> u8 {
        match self {
            Self::Group => 0,
            Self::Private => 3,
            Self::Unknown(value) => value,
        }
    }
}

/// Twelve-byte DMR full link control carried by voice headers and terminators.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FullLinkControl {
    /// Raw control octet from byte 0.
    ///
    /// Bit 7 is the protection flag, bit 6 is reserved, and the low six bits
    /// are the Full Link Control Opcode (FLCO). The complete octet is retained.
    pub flco: u8,
    /// Feature-set identifier from byte 1.
    pub feature_id: u8,
    /// Service-options byte from byte 2.
    pub service_options: u8,
    /// Big-endian 24-bit destination DMR ID from bytes 3 through 5.
    pub destination_id: u32,
    /// Big-endian 24-bit source DMR ID from bytes 6 through 8.
    pub source_id: u32,
    /// Uninterpreted bytes 9 through 11.
    pub tail: [u8; 3],
}

impl FullLinkControl {
    /// Decode an exact twelve-byte full-link-control payload.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; FULL_LINK_CONTROL_LEN]) -> Self {
        let [
            flco,
            feature_id,
            service_options,
            destination_1,
            destination_2,
            destination_3,
            source_1,
            source_2,
            source_3,
            tail_1,
            tail_2,
            tail_3,
        ] = bytes;

        Self {
            flco,
            feature_id,
            service_options,
            destination_id: u32::from_be_bytes([0, destination_1, destination_2, destination_3]),
            source_id: u32::from_be_bytes([0, source_1, source_2, source_3]),
            tail: [tail_1, tail_2, tail_3],
        }
    }

    /// Classify the low six FLCO bits while preserving the raw octet in `flco`.
    #[must_use]
    pub const fn call_type(self) -> FullLinkControlType {
        FullLinkControlType::from_flco(self.flco)
    }

    /// Encode this value as an exact twelve-byte full-link-control payload.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::FieldOutOfRange`] when either DMR ID does not
    /// fit the protocol's 24-bit field.
    pub const fn to_bytes(self) -> Result<[u8; FULL_LINK_CONTROL_LEN], CodecError> {
        const MAX_DMR_ID: u32 = 0x00ff_ffff;

        if self.destination_id > MAX_DMR_ID {
            return Err(CodecError::FieldOutOfRange {
                field: "full link control destination ID",
                value: self.destination_id,
                maximum: MAX_DMR_ID,
            });
        }
        if self.source_id > MAX_DMR_ID {
            return Err(CodecError::FieldOutOfRange {
                field: "full link control source ID",
                value: self.source_id,
                maximum: MAX_DMR_ID,
            });
        }

        let [_, destination_1, destination_2, destination_3] = self.destination_id.to_be_bytes();
        let [_, source_1, source_2, source_3] = self.source_id.to_be_bytes();
        let [tail_1, tail_2, tail_3] = self.tail;

        Ok([
            self.flco,
            self.feature_id,
            self.service_options,
            destination_1,
            destination_2,
            destination_3,
            source_1,
            source_2,
            source_3,
            tail_1,
            tail_2,
            tail_3,
        ])
    }
}

/// Known REWIND packet types plus lossless unknown values.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PacketType {
    /// Keepalive/version exchange (`0x0000`).
    KeepAlive,
    /// Graceful close (`0x0001`).
    Close,
    /// Authentication challenge (`0x0002`).
    Challenge,
    /// Authentication digest (`0x0003`).
    Authentication,
    /// Server redirection (`0x0008`).
    Redirection,
    /// Text report (`0x0100`).
    Report,
    /// Busy notice (`0x0200`).
    BusyNotice,
    /// Address notice (`0x0201`).
    AddressNotice,
    /// Binding notice (`0x0202`).
    BindingNotice,
    /// Kairos external-server data (`0x0800`).
    ExternalServer,
    /// Kairos remote-control data (`0x0801`).
    RemoteControl,
    /// Kairos SNMP trap (`0x0802`).
    SnmpTrap,
    /// Hytera peer data (`0x0810`).
    PeerData,
    /// Hytera RDAC data (`0x0811`).
    RdacData,
    /// Hytera media data (`0x0812`).
    MediaData,
    /// Application configuration (`0x0900`).
    Configuration,
    /// Add subscription (`0x0901`).
    Subscription,
    /// Cancel subscription (`0x0902`).
    Cancelling,
    /// Session-state poll (`0x0903`).
    SessionPoll,
    /// DMR voice header with full link control (`0x0911`).
    DmrVoiceHeader,
    /// DMR terminator, empty or with full link control (`0x0912`).
    DmrTerminator,
    /// Other DMR data subtype (`0x0910..=0x091f`).
    DmrData(u8),
    /// DMR audio subtype (`0x0920..=0x0926`).
    DmrAudio(u8),
    /// DMR embedded data (`0x0927`).
    DmrEmbeddedData,
    /// Call metadata super-header (`0x0928`).
    SuperHeader,
    /// Application failure (`0x0929`).
    Failure,
    /// Open-terminal idle state (`0x0a00`).
    TerminalIdle,
    /// Open-terminal attachment (`0x0a02`).
    TerminalAttach,
    /// Open-terminal detachment (`0x0a03`).
    TerminalDetach,
    /// Open-terminal wakeup (`0x0a04`).
    TerminalWakeup,
    /// Open-terminal text message (`0x0a10`).
    MessageText,
    /// Open-terminal message status (`0x0a11`).
    MessageStatus,
    /// Open-terminal location report (`0x0a20`).
    LocationReport,
    /// Open-terminal location request (`0x0a21`).
    LocationRequest,
    /// A future packet not known by this codec.
    Unknown(u16),
}

impl PacketType {
    /// Decode a raw packet type.
    #[must_use]
    pub const fn from_raw(raw: u16) -> Self {
        match raw {
            0x0000 => Self::KeepAlive,
            0x0001 => Self::Close,
            0x0002 => Self::Challenge,
            0x0003 => Self::Authentication,
            0x0008 => Self::Redirection,
            0x0100 => Self::Report,
            0x0200 => Self::BusyNotice,
            0x0201 => Self::AddressNotice,
            0x0202 => Self::BindingNotice,
            0x0800 => Self::ExternalServer,
            0x0801 => Self::RemoteControl,
            0x0802 => Self::SnmpTrap,
            0x0810 => Self::PeerData,
            0x0811 => Self::RdacData,
            0x0812 => Self::MediaData,
            0x0900 => Self::Configuration,
            0x0901 => Self::Subscription,
            0x0902 => Self::Cancelling,
            0x0903 => Self::SessionPoll,
            0x0911 => Self::DmrVoiceHeader,
            0x0912 => Self::DmrTerminator,
            0x0910..=0x091f => {
                let [subtype, _] = (raw - 0x0910).to_le_bytes();
                Self::DmrData(subtype)
            }
            0x0920..=0x0926 => {
                let [subtype, _] = (raw - 0x0920).to_le_bytes();
                Self::DmrAudio(subtype)
            }
            0x0927 => Self::DmrEmbeddedData,
            0x0928 => Self::SuperHeader,
            0x0929 => Self::Failure,
            0x0a00 => Self::TerminalIdle,
            0x0a02 => Self::TerminalAttach,
            0x0a03 => Self::TerminalDetach,
            0x0a04 => Self::TerminalWakeup,
            0x0a10 => Self::MessageText,
            0x0a11 => Self::MessageStatus,
            0x0a20 => Self::LocationReport,
            0x0a21 => Self::LocationRequest,
            value => Self::Unknown(value),
        }
    }

    /// Encode this packet type.
    #[must_use]
    pub fn as_raw(self) -> u16 {
        match self {
            Self::KeepAlive => 0x0000,
            Self::Close => 0x0001,
            Self::Challenge => 0x0002,
            Self::Authentication => 0x0003,
            Self::Redirection => 0x0008,
            Self::Report => 0x0100,
            Self::BusyNotice => 0x0200,
            Self::AddressNotice => 0x0201,
            Self::BindingNotice => 0x0202,
            Self::ExternalServer => 0x0800,
            Self::RemoteControl => 0x0801,
            Self::SnmpTrap => 0x0802,
            Self::PeerData => 0x0810,
            Self::RdacData => 0x0811,
            Self::MediaData => 0x0812,
            Self::Configuration => 0x0900,
            Self::Subscription => 0x0901,
            Self::Cancelling => 0x0902,
            Self::SessionPoll => 0x0903,
            Self::DmrVoiceHeader => 0x0911,
            Self::DmrTerminator => 0x0912,
            Self::DmrData(subtype) => 0x0910 + u16::from(subtype),
            Self::DmrAudio(subtype) => 0x0920 + u16::from(subtype),
            Self::DmrEmbeddedData => 0x0927,
            Self::SuperHeader => 0x0928,
            Self::Failure => 0x0929,
            Self::TerminalIdle => 0x0a00,
            Self::TerminalAttach => 0x0a02,
            Self::TerminalDetach => 0x0a03,
            Self::TerminalWakeup => 0x0a04,
            Self::MessageText => 0x0a10,
            Self::MessageStatus => 0x0a11,
            Self::LocationReport => 0x0a20,
            Self::LocationRequest => 0x0a21,
            Self::Unknown(value) => value,
        }
    }
}

/// Parsed packet envelope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Header {
    /// Packet type.
    pub packet_type: PacketType,
    /// Wire flags.
    pub flags: PacketFlags,
    /// Sequence number.
    pub sequence: u32,
    /// Declared payload length.
    pub payload_len: u16,
}

/// Zero-padded ten-byte callsign, preserved exactly.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Callsign([u8; 10]);

impl Callsign {
    /// Construct from the exact wire bytes.
    #[must_use]
    pub const fn from_raw(raw: [u8; 10]) -> Self {
        Self(raw)
    }

    /// Return the exact wire bytes.
    #[must_use]
    pub const fn into_raw(self) -> [u8; 10] {
        self.0
    }

    /// Return a display-oriented, zero/space-trimmed lossy string.
    #[must_use]
    pub fn trimmed_lossy(&self) -> String {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.0.len());
        let bytes = self.0.get(..end).unwrap_or_default();
        String::from_utf8_lossy(bytes).trim_end().to_owned()
    }
}

/// Variable-length keepalive/version data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionData {
    /// Seven-digit DMR ID for this Open Terminal.
    pub remote_id: u32,
    /// Application service identifier.
    pub service: u8,
    /// Opaque software description bytes.
    pub description: Vec<u8>,
}

/// Subscription or cancellation data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Subscription {
    /// Group/private session type.
    pub session_type: SessionType,
    /// Destination DMR ID.
    pub target: u32,
}

/// Four-word session-poll data.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionPoll {
    /// Poll tree/type selector.
    pub kind: u32,
    /// Poll flags.
    pub flags: u32,
    /// DMR ID being polled.
    pub number: u32,
    /// Session state.
    pub state: u32,
}

/// Parsed call metadata.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SuperHeader {
    /// Group/private session type.
    pub session_type: SessionType,
    /// Source DMR ID, or zero.
    pub source_id: u32,
    /// Destination DMR ID, or zero.
    pub target_id: u32,
    /// Source callsign bytes.
    pub source_call: Callsign,
    /// Destination callsign bytes.
    pub target_call: Callsign,
}

/// Typed packet payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Payload {
    /// Optional keepalive/version data; `None` is the acknowledgement form.
    KeepAlive(Option<VersionData>),
    /// Empty graceful-close payload.
    Close,
    /// Opaque server challenge bytes.
    Challenge(Vec<u8>),
    /// SHA-256 authentication digest.
    Authentication([u8; AUTHENTICATION_LEN]),
    /// Opaque redirection data.
    Redirection(Vec<u8>),
    /// Opaque text report bytes.
    Report(Vec<u8>),
    /// Opaque busy-notice bytes.
    Busy(Vec<u8>),
    /// Optional subscription data; `None` is an acknowledgement.
    Subscription(Option<Subscription>),
    /// Optional cancellation data; `None` is an acknowledgement or cancel-all.
    Cancelling(Option<Subscription>),
    /// Session-poll words.
    SessionPoll(SessionPoll),
    /// Parsed DMR voice-header full link control.
    DmrVoiceHeader(FullLinkControl),
    /// DMR terminator, optionally carrying full link control.
    ///
    /// Both the empty form and the 12-byte form occur in deployed
    /// implementations.
    DmrTerminator(Option<FullLinkControl>),
    /// Exact 27-byte DMR audio burst.
    DmrAudio([u8; DMR_AUDIO_LEN]),
    /// Exact DMR embedded-data bytes.
    DmrEmbeddedData([u8; DMR_EMBEDDED_DATA_LEN]),
    /// Parsed super-header metadata.
    SuperHeader(SuperHeader),
    /// Opaque application failure data.
    Failure(Vec<u8>),
    /// Opaque payload for packet types without a modeled structure.
    Opaque(Vec<u8>),
}

/// One complete REWIND datagram.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Packet {
    /// Parsed envelope.
    pub header: Header,
    /// Typed or opaque payload.
    pub payload: Payload,
}

impl Packet {
    /// Construct a packet and derive its declared payload length.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`] if the payload does not match `packet_type`, a
    /// public subtype is invalid, a full-link-control ID exceeds 24 bits, or
    /// the payload exceeds the protocol limit.
    pub fn new(
        packet_type: PacketType,
        flags: PacketFlags,
        sequence: u32,
        payload: Payload,
    ) -> Result<Self, CodecError> {
        let payload_len = codec::validate_and_measure(packet_type, &payload)?;
        let payload_len = u16::try_from(payload_len).map_err(|_| CodecError::PayloadTooLong {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        })?;

        Ok(Self {
            header: Header {
                packet_type,
                flags,
                sequence,
                payload_len,
            },
            payload,
        })
    }
}

/// Codec or packet-construction failure.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CodecError {
    /// Datagram is shorter than the fixed header.
    #[error("datagram has {actual} bytes; at least {minimum} required")]
    DatagramTooShort {
        /// Observed length.
        actual: usize,
        /// Required length.
        minimum: usize,
    },
    /// Datagram exceeds the UDP maximum accepted by the codec.
    #[error("datagram has {actual} bytes; maximum is {maximum}")]
    DatagramTooLong {
        /// Observed length.
        actual: usize,
        /// Maximum length.
        maximum: usize,
    },
    /// Signature is not `REWIND01`.
    #[error("invalid REWIND signature")]
    InvalidSignature,
    /// Payload ends before its declared length.
    #[error("payload declares {declared} bytes but only {actual} are present")]
    TruncatedPayload {
        /// Declared payload length.
        declared: usize,
        /// Available payload length.
        actual: usize,
    },
    /// Bytes follow the declared payload.
    #[error("{trailing} trailing bytes follow the declared payload")]
    TrailingBytes {
        /// Undeclared byte count.
        trailing: usize,
    },
    /// A fixed-size typed payload has the wrong size.
    #[error("{packet_type:?} payload has {actual} bytes; expected {expected}")]
    InvalidPayloadLength {
        /// Packet type being decoded.
        packet_type: PacketType,
        /// Human-readable expected length.
        expected: &'static str,
        /// Observed length.
        actual: usize,
    },
    /// Header type and typed payload do not agree.
    #[error("payload variant does not match packet type {packet_type:?}")]
    PayloadTypeMismatch {
        /// Header packet type.
        packet_type: PacketType,
    },
    /// A public subtype is outside its wire range.
    #[error("invalid subtype {subtype} for packet type {packet_type:?}")]
    InvalidSubtype {
        /// Packet family.
        packet_type: PacketType,
        /// Invalid subtype.
        subtype: u8,
    },
    /// A public enum value uses the raw code of a different canonical variant.
    #[error("non-canonical packet type {packet_type:?}")]
    NonCanonicalPacketType {
        /// Invalid public value.
        packet_type: PacketType,
    },
    /// Encoded payload cannot fit this protocol.
    #[error("payload has {actual} bytes; maximum is {maximum}")]
    PayloadTooLong {
        /// Attempted length.
        actual: usize,
        /// Maximum length.
        maximum: usize,
    },
    /// Header length does not match the encoded typed payload.
    #[error("header declares {declared} bytes but payload encodes to {actual}")]
    HeaderLengthMismatch {
        /// Header value.
        declared: usize,
        /// Encoded payload size.
        actual: usize,
    },
    /// A public numeric field cannot fit its wire representation.
    #[error("{field} value {value} exceeds maximum {maximum}")]
    FieldOutOfRange {
        /// Name of the invalid field.
        field: &'static str,
        /// Attempted value.
        value: u32,
        /// Maximum wire value.
        maximum: u32,
    },
}

/// Compute `SHA-256(challenge || password)`.
#[must_use]
pub fn authentication_digest(challenge: &[u8], password: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(challenge);
    hasher.update(password);
    hasher.finalize().into()
}

/// Decode and validate one complete REWIND datagram.
///
/// # Errors
///
/// Returns [`CodecError`] for malformed envelopes or typed payloads.
pub fn decode(datagram: &[u8]) -> Result<Packet, CodecError> {
    codec::decode(datagram)
}

/// Encode one complete REWIND datagram.
///
/// # Errors
///
/// Returns [`CodecError`] if public packet fields are inconsistent or exceed
/// protocol limits.
pub fn encode(packet: &Packet) -> Result<Vec<u8>, CodecError> {
    codec::encode(packet)
}
