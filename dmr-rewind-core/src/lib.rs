// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later
//
// This is an independent implementation of public wire behavior. Protocol
// facts were cross-checked against BrandMeister go-brandmeister, DigestPlay's
// Rewind.h, and DJ4CK's pyspot_rx; no source code from those projects is
// incorporated here.

#![doc = include_str!("../README.md")]

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

use sha2::{Digest, Sha256};
use thiserror::Error;

pub use wire::{
    ControlOctet, FeatureId, PollFlags, PollKind, PollState, RemoteId, ServiceCode, ServiceOptions,
    TransportSequence, WireDmrId,
};

mod codec;
mod wire;

/// REWIND protocol signature.
pub const SIGNATURE: [u8; 8] = *b"REWIND01";
/// Encoded envelope size before the payload.
pub const HEADER_LEN: usize = 18;
/// Default UDP port for the self-service Open DMR Terminal service.
pub const DEFAULT_OPEN_TERMINAL_PORT: u16 = 54_006;
/// Maximum UDP payload accepted by this codec, in bytes.
///
/// This uses the IPv4 limit with a minimum-length IP header; it is also the
/// codec's conservative limit for IPv6 datagrams.
pub const MAX_DATAGRAM_LEN: usize = 65_507;
/// Maximum payload accepted by this codec.
pub const MAX_PAYLOAD_LEN: usize = MAX_DATAGRAM_LEN - HEADER_LEN;
/// Size of a SHA-256 authentication response.
pub const AUTHENTICATION_LEN: usize = 32;
/// Size of a DMR header or terminator full-link-control payload.
pub const FULL_LINK_CONTROL_LEN: usize = 12;
/// Size of one REWIND DMR audio payload.
pub const DMR_AUDIO_LEN: usize = 27;
/// Size of a REWIND super-header payload.
pub const SUPER_HEADER_LEN: usize = 32;
/// Service identifier for a self-service Open DMR Terminal.
pub const SERVICE_OPEN_TERMINAL: ServiceCode = ServiceCode::OPEN_TERMINAL;

/// Packet-envelope flags, including unknown future bits.
///
/// Standard bitwise operators combine or mask flags without discarding unknown
/// bits. See the [packet workflow](crate#construct-and-round-trip-a-packet).
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

impl From<u16> for PacketFlags {
    fn from(bits: u16) -> Self {
        Self::from_bits(bits)
    }
}

impl From<PacketFlags> for u16 {
    fn from(flags: PacketFlags) -> Self {
        flags.bits()
    }
}

impl BitOr for PacketFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for PacketFlags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for PacketFlags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for PacketFlags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl BitXor for PacketFlags {
    type Output = Self;

    fn bitxor(self, rhs: Self) -> Self {
        Self(self.0 ^ rhs.0)
    }
}

impl BitXorAssign for PacketFlags {
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

impl Not for PacketFlags {
    type Output = Self;

    fn not(self) -> Self {
        Self(!self.0)
    }
}

impl fmt::Binary for PacketFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }
}

impl fmt::Octal for PacketFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }
}

impl fmt::LowerHex for PacketFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl fmt::UpperHex for PacketFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }
}

/// A wire session code other than the named private and group voice codes.
///
/// See the [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnknownSessionType(u32);

impl UnknownSessionType {
    /// Preserve an unrecognized session code.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::KnownSessionType`] for the named codes 5 and 7.
    pub const fn new(raw: u32) -> Result<Self, CodecError> {
        if matches!(raw, 5 | 7) {
            Err(CodecError::KnownSessionType { raw })
        } else {
            Ok(Self(raw))
        }
    }

    /// Return the exact unrecognized code.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for UnknownSessionType {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its named-code error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(raw: u32) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<UnknownSessionType> for u32 {
    fn from(value: UnknownSessionType) -> Self {
        value.get()
    }
}

/// DMR session type carried by subscriptions and super-headers.
///
/// See the [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionType {
    /// Private voice, wire value 5.
    PrivateVoice,
    /// Group voice, wire value 7.
    GroupVoice,
    /// A future or unsupported wire value.
    Unknown(UnknownSessionType),
}

impl SessionType {
    /// Decode a raw wire value without losing unknown values.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            5 => Self::PrivateVoice,
            7 => Self::GroupVoice,
            value => Self::Unknown(UnknownSessionType(value)),
        }
    }

    /// Encode this session type.
    #[must_use]
    pub const fn as_raw(self) -> u32 {
        match self {
            Self::PrivateVoice => 5,
            Self::GroupVoice => 7,
            Self::Unknown(value) => value.get(),
        }
    }
}

impl From<u32> for SessionType {
    fn from(raw: u32) -> Self {
        Self::from_raw(raw)
    }
}

impl From<SessionType> for u32 {
    fn from(value: SessionType) -> Self {
        value.as_raw()
    }
}

/// An unrecognized six-bit full-link-control opcode.
///
/// See the [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnknownFullLinkControlType(u8);

impl UnknownFullLinkControlType {
    /// Validate an opcode other than group voice (0) or private voice (3).
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidFlco`] for 0, 3 or values above 63.
    pub const fn new(flco: u8) -> Result<Self, CodecError> {
        if flco > 0x3f || matches!(flco, 0 | 3) {
            Err(CodecError::InvalidFlco { flco })
        } else {
            Ok(Self(flco))
        }
    }

    /// Return the six-bit opcode.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for UnknownFullLinkControlType {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its opcode error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(flco: u8) -> Result<Self, Self::Error> {
        Self::new(flco)
    }
}

impl From<UnknownFullLinkControlType> for u8 {
    fn from(value: UnknownFullLinkControlType) -> Self {
        value.get()
    }
}

/// Classification derived from the low six FLCO bits of full link control.
///
/// See the [link-control example](crate#full-link-control).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FullLinkControlType {
    /// Group voice, FLCO value 0.
    Group,
    /// Private voice, FLCO value 3.
    Private,
    /// An unknown FLCO value, preserved exactly.
    Unknown(UnknownFullLinkControlType),
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
            value => Self::Unknown(UnknownFullLinkControlType(value)),
        }
    }

    /// Return the six-bit opcode, excluding protection and reserved flags.
    #[must_use]
    pub const fn as_flco(self) -> u8 {
        match self {
            Self::Group => 0,
            Self::Private => 3,
            Self::Unknown(value) => value.get(),
        }
    }
}

impl From<FullLinkControlType> for u8 {
    fn from(value: FullLinkControlType) -> Self {
        value.as_flco()
    }
}

/// A DMR identifier that fits the full-link-control wire's 24-bit field.
///
/// Zero is preserved as a wire value. This does not validate assignment of an
/// identifier to a subscriber. See the
/// [link-control example](crate#full-link-control).
/// Decimal display and binary, octal and hexadecimal formatting support wire
/// inspection, including standard width, padding and prefix options.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DmrId(u32);

impl DmrId {
    /// Largest identifier representable by full link control.
    pub const MAX: Self = Self(0x00ff_ffff);

    /// Validate a full-link-control identifier.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::DmrIdOutOfRange`] above [`Self::MAX`].
    pub const fn new(value: u32) -> Result<Self, CodecError> {
        if value <= Self::MAX.get() {
            Ok(Self(value))
        } else {
            Err(CodecError::DmrIdOutOfRange { value })
        }
    }

    /// Return the identifier as an unsigned integer.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for DmrId {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its range error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<DmrId> for u32 {
    fn from(value: DmrId) -> Self {
        value.get()
    }
}

impl fmt::Display for DmrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Binary for DmrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.0, formatter)
    }
}

impl fmt::Octal for DmrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.0, formatter)
    }
}

impl fmt::LowerHex for DmrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, formatter)
    }
}

impl fmt::UpperHex for DmrId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.0, formatter)
    }
}

/// Twelve-byte DMR full link control carried by voice headers and terminators.
///
/// Identifier fields validate their 24-bit width at construction; other fields
/// preserve their full wire bytes. See the
/// [link-control example](crate#full-link-control).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FullLinkControl {
    /// Raw control octet from byte 0.
    ///
    /// Bit 7 is the protection flag, bit 6 is reserved, and the low six bits
    /// are the Full Link Control Opcode (FLCO). The complete octet is retained.
    pub flco: ControlOctet,
    /// Feature-set identifier from byte 1.
    pub feature_id: FeatureId,
    /// Service-options byte from byte 2.
    pub service_options: ServiceOptions,
    /// Big-endian 24-bit destination DMR ID from bytes 3 through 5.
    pub destination_id: DmrId,
    /// Big-endian 24-bit source DMR ID from bytes 6 through 8.
    pub source_id: DmrId,
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
            flco: ControlOctet::new(flco),
            feature_id: FeatureId::new(feature_id),
            service_options: ServiceOptions::from_bits(service_options),
            destination_id: DmrId(u32::from_be_bytes([
                0,
                destination_1,
                destination_2,
                destination_3,
            ])),
            source_id: DmrId(u32::from_be_bytes([0, source_1, source_2, source_3])),
            tail: [tail_1, tail_2, tail_3],
        }
    }

    /// Classify the low six FLCO bits while preserving the raw octet in `flco`.
    #[must_use]
    pub const fn call_type(self) -> FullLinkControlType {
        self.flco.call_type()
    }

    /// Encode this value as an exact twelve-byte full-link-control payload.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; FULL_LINK_CONTROL_LEN] {
        let [_, destination_1, destination_2, destination_3] =
            self.destination_id.get().to_be_bytes();
        let [_, source_1, source_2, source_3] = self.source_id.get().to_be_bytes();
        let [tail_1, tail_2, tail_3] = self.tail;

        [
            self.flco.get(),
            self.feature_id.get(),
            self.service_options.bits(),
            destination_1,
            destination_2,
            destination_3,
            source_1,
            source_2,
            source_3,
            tail_1,
            tail_2,
            tail_3,
        ]
    }
}

impl From<[u8; FULL_LINK_CONTROL_LEN]> for FullLinkControl {
    fn from(bytes: [u8; FULL_LINK_CONTROL_LEN]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl From<FullLinkControl> for [u8; FULL_LINK_CONTROL_LEN] {
    fn from(value: FullLinkControl) -> Self {
        value.to_bytes()
    }
}

/// An unmodeled DMR data subtype within the four-bit REWIND data family.
///
/// Voice-header and terminator codes have named [`PacketType`] variants.
/// See the [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DmrDataSubtype(u8);

impl DmrDataSubtype {
    /// Validate a data subtype in `0..=15`, excluding named codes 1 and 2.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidDataSubtype`] outside this domain.
    pub const fn new(subtype: u8) -> Result<Self, CodecError> {
        if subtype > 15 || matches!(subtype, 1 | 2) {
            Err(CodecError::InvalidDataSubtype { subtype })
        } else {
            Ok(Self(subtype))
        }
    }

    /// Return the four-bit subtype.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DmrDataSubtype {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its data-subtype error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(subtype: u8) -> Result<Self, Self::Error> {
        Self::new(subtype)
    }
}

impl From<DmrDataSubtype> for u8 {
    fn from(value: DmrDataSubtype) -> Self {
        value.get()
    }
}

/// A DMR audio subtype in `0..=6`.
///
/// Subtype 7 is the separately modeled embedded-data packet. See the
/// [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DmrAudioSubtype(u8);

impl DmrAudioSubtype {
    /// Validate a DMR audio subtype.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::InvalidAudioSubtype`] for values above 6.
    pub const fn new(subtype: u8) -> Result<Self, CodecError> {
        if subtype <= 6 {
            Ok(Self(subtype))
        } else {
            Err(CodecError::InvalidAudioSubtype { subtype })
        }
    }

    /// Return the audio subtype.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for DmrAudioSubtype {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its audio-subtype error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(subtype: u8) -> Result<Self, Self::Error> {
        Self::new(subtype)
    }
}

impl From<DmrAudioSubtype> for u8 {
    fn from(value: DmrAudioSubtype) -> Self {
        value.get()
    }
}

/// A packet code that has no named or modeled-family interpretation.
///
/// See the [wire value example](crate#validated-wire-values).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnknownPacketType(u16);

impl UnknownPacketType {
    /// Validate an unrecognized packet code.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::KnownPacketType`] when [`PacketType::from_raw`]
    /// classifies the code as a named packet or modeled family member.
    pub const fn new(raw: u16) -> Result<Self, CodecError> {
        if matches!(PacketType::from_raw(raw), PacketType::Unknown(_)) {
            Ok(Self(raw))
        } else {
            Err(CodecError::KnownPacketType { raw })
        }
    }

    /// Return the exact unrecognized packet code.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for UnknownPacketType {
    type Error = CodecError;

    /// Validate through [`Self::new`], returning its known-code error.
    ///
    /// # Errors
    ///
    /// Returns the error documented by [`Self::new`].
    fn try_from(raw: u16) -> Result<Self, Self::Error> {
        Self::new(raw)
    }
}

impl From<UnknownPacketType> for u16 {
    fn from(value: UnknownPacketType) -> Self {
        value.get()
    }
}

/// Known REWIND packet types plus lossless unknown values.
///
/// See the [wire value example](crate#validated-wire-values).
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
    DmrData(DmrDataSubtype),
    /// DMR audio subtype (`0x0920..=0x0926`).
    DmrAudio(DmrAudioSubtype),
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
    Unknown(UnknownPacketType),
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
                Self::DmrData(DmrDataSubtype(subtype))
            }
            0x0920..=0x0926 => {
                let [subtype, _] = (raw - 0x0920).to_le_bytes();
                Self::DmrAudio(DmrAudioSubtype(subtype))
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
            value => Self::Unknown(UnknownPacketType(value)),
        }
    }

    /// Encode this packet type.
    #[must_use]
    pub const fn as_raw(self) -> u16 {
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
            Self::DmrData(subtype) => 0x0910 + subtype.get() as u16,
            Self::DmrAudio(subtype) => 0x0920 + subtype.get() as u16,
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
            Self::Unknown(value) => value.get(),
        }
    }
}

impl From<u16> for PacketType {
    fn from(raw: u16) -> Self {
        Self::from_raw(raw)
    }
}

impl From<PacketType> for u16 {
    fn from(value: PacketType) -> Self {
        value.as_raw()
    }
}

/// Parsed packet envelope.
///
/// [`Packet::header`] returns this independent copy. It cannot replace a
/// packet's envelope. See the
/// [packet workflow](crate#construct-and-round-trip-a-packet).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Header {
    /// Packet type.
    pub packet_type: PacketType,
    /// Wire flags.
    pub flags: PacketFlags,
    /// Sequence number.
    pub sequence: TransportSequence,
    /// Declared payload length.
    pub payload_len: u16,
}

/// Ten uninterpreted callsign bytes, preserved exactly.
///
/// Parsing does not validate text encoding or padding. See the
/// [metadata example](crate#call-metadata).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Callsign([u8; 10]);

impl Callsign {
    /// Construct from the exact wire bytes.
    #[must_use]
    pub const fn from_bytes(raw: [u8; 10]) -> Self {
        Self(raw)
    }

    /// Return the exact wire bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 10] {
        self.0
    }

    /// Return text before the first NUL, trimming trailing Unicode whitespace.
    ///
    /// Invalid UTF-8 is replaced with the Unicode replacement character.
    /// The stored bytes are unchanged. See the
    /// [metadata example](crate#call-metadata).
    #[must_use]
    pub fn to_trimmed_lossy(self) -> String {
        let end = self
            .0
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.0.len());
        let bytes = self.0.get(..end).unwrap_or_default();
        String::from_utf8_lossy(bytes).trim_end().to_owned()
    }
}

impl From<[u8; 10]> for Callsign {
    fn from(raw: [u8; 10]) -> Self {
        Self::from_bytes(raw)
    }
}

impl From<Callsign> for [u8; 10] {
    fn from(value: Callsign) -> Self {
        value.to_bytes()
    }
}

impl AsRef<[u8; 10]> for Callsign {
    fn as_ref(&self) -> &[u8; 10] {
        &self.0
    }
}

/// Variable-length keepalive/version data.
///
/// See the [authentication example](crate#authentication).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct VersionData {
    /// Raw remote-application identifier, encoded in four little-endian bytes.
    pub remote_id: RemoteId,
    /// Application service identifier.
    pub service: ServiceCode,
    /// Opaque software description bytes.
    pub description: Vec<u8>,
}

/// Subscription or cancellation data.
///
/// See the [packet workflow](crate#construct-and-round-trip-a-packet).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Subscription {
    /// Group/private session type.
    pub session_type: SessionType,
    /// Destination DMR ID.
    pub target: WireDmrId,
}

/// Four-word session-poll data.
///
/// Each word preserves its entire 32-bit wire field. See the
/// [session-poll example](crate#session-polls).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionPoll {
    /// Poll tree/type selector.
    pub kind: PollKind,
    /// Poll flags.
    pub flags: PollFlags,
    /// DMR ID being polled.
    pub number: WireDmrId,
    /// Session state.
    pub state: PollState,
}

/// Parsed call metadata.
///
/// Identifiers preserve their full 32-bit protocol fields. See the
/// [metadata example](crate#call-metadata).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SuperHeader {
    /// Group/private session type.
    pub session_type: SessionType,
    /// Source DMR ID, or zero.
    pub source_id: WireDmrId,
    /// Destination DMR ID, or zero.
    pub target_id: WireDmrId,
    /// Source callsign bytes.
    pub source_call: Callsign,
    /// Destination callsign bytes.
    pub target_call: Callsign,
}

/// Typed packet payload.
///
/// [`Packet::new`] checks agreement with the packet type and bounds variable
/// lengths before retaining this value. See the
/// [packet workflow](crate#construct-and-round-trip-a-packet),
/// [link-control example](crate#full-link-control),
/// [media example](crate#media-payloads), and
/// [metadata example](crate#call-metadata).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
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
    /// Uninterpreted DMR embedded-data bytes, bounded by the packet limit.
    DmrEmbeddedData(Vec<u8>),
    /// Parsed super-header metadata.
    SuperHeader(SuperHeader),
    /// Opaque application failure data.
    Failure(Vec<u8>),
    /// Opaque payload for packet types without a modeled structure.
    Opaque(Vec<u8>),
}

/// One complete REWIND datagram.
///
/// Construction fixes the envelope length and checks payload/type agreement.
/// Borrowed accessors preserve those relationships; consuming accessors
/// transfer the existing payload without cloning it. See the
/// [packet workflow](crate#construct-and-round-trip-a-packet).
///
/// Packet fields cannot be changed to bypass validation.
///
/// ```compile_fail
/// use dmr_rewind_core::{Packet, PacketFlags, PacketType, Payload};
/// let mut packet = Packet::new(
///     PacketType::Close, PacketFlags::NONE, 0.into(), Payload::Close,
/// )?;
/// packet.header.payload_len = 1;
/// # Ok::<(), dmr_rewind_core::CodecError>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Packet {
    header: Header,
    payload: Payload,
}

impl Packet {
    /// Construct a packet and derive its declared payload length.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::PayloadTypeMismatch`] when the payload variant
    /// cannot represent `packet_type`, or [`CodecError::PayloadTooLong`] above
    /// [`MAX_PAYLOAD_LEN`]. Validation borrows the owned input and allocates no
    /// temporary payload. See the
    /// [packet workflow](crate#construct-and-round-trip-a-packet).
    pub fn new(
        packet_type: PacketType,
        flags: PacketFlags,
        sequence: TransportSequence,
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

    /// Decode one complete datagram, rejecting malformed envelopes and bodies.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError::DatagramTooShort`] below [`HEADER_LEN`],
    /// [`CodecError::DatagramTooLong`] above [`MAX_DATAGRAM_LEN`],
    /// [`CodecError::InvalidSignature`] for a different signature,
    /// [`CodecError::TruncatedPayload`] or [`CodecError::TrailingBytes`] when
    /// the declared payload length differs, or
    /// [`CodecError::InvalidPayloadLength`] for a modeled body's wrong size.
    /// See the [packet workflow](crate#construct-and-round-trip-a-packet).
    pub fn from_bytes(datagram: &[u8]) -> Result<Self, CodecError> {
        codec::decode(datagram)
    }

    /// Encode the complete datagram into one newly allocated byte vector.
    ///
    /// The validated packet determines its exact output length. The payload is
    /// copied directly into that output, without an intermediate encoding.
    /// See the [packet workflow](crate#construct-and-round-trip-a-packet).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        codec::encode(self)
    }

    /// Return a copy of the envelope; changes to it leave this packet intact.
    ///
    /// See the [packet workflow](crate#construct-and-round-trip-a-packet).
    #[must_use]
    pub const fn header(&self) -> Header {
        self.header
    }

    /// Borrow the interpreted payload without copying its owned storage.
    ///
    /// See the [metadata example](crate#call-metadata).
    #[must_use]
    pub const fn payload(&self) -> &Payload {
        &self.payload
    }

    /// Consume the packet, transferring its envelope and payload without cloning.
    ///
    /// See the [metadata example](crate#call-metadata).
    #[must_use]
    pub fn into_parts(self) -> (Header, Payload) {
        (self.header, self.payload)
    }

    /// Consume the packet and transfer its payload without cloning.
    ///
    /// See the [metadata example](crate#call-metadata).
    #[must_use]
    pub fn into_payload(self) -> Payload {
        self.payload
    }
}

impl TryFrom<&[u8]> for Packet {
    type Error = CodecError;

    /// Parse a datagram using [`Self::from_bytes`].
    ///
    /// # Errors
    ///
    /// Returns the envelope and body errors documented by [`Self::from_bytes`].
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::from_bytes(bytes)
    }
}

/// Codec or packet-construction failure.
///
/// Errors preserve rejected codes, lengths or fields without including
/// password bytes. See the [validation example](crate#rejected-input).
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
    /// Datagram exceeds the codec's configured byte limit.
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
    /// A data subtype is outside `0..=15` or uses named codes 1 or 2.
    #[error("DMR data subtype {subtype} is outside 0..=15 or uses a named packet code")]
    InvalidDataSubtype {
        /// Rejected subtype.
        subtype: u8,
    },
    /// An audio subtype exceeds 6.
    #[error("DMR audio subtype {subtype} exceeds 6")]
    InvalidAudioSubtype {
        /// Rejected subtype.
        subtype: u8,
    },
    /// A named packet code was supplied as an unknown code.
    #[error("packet code 0x{raw:04x} has a named interpretation")]
    KnownPacketType {
        /// Rejected packet code.
        raw: u16,
    },
    /// A named session code was supplied as an unknown code.
    #[error("session code {raw} has a named interpretation")]
    KnownSessionType {
        /// Rejected session code.
        raw: u32,
    },
    /// An unknown FLCO exceeds six bits or uses the named codes 0 or 3.
    #[error("unknown FLCO {flco} exceeds six bits or uses a named voice code")]
    InvalidFlco {
        /// Rejected opcode.
        flco: u8,
    },
    /// Encoded payload cannot fit this protocol.
    #[error("payload has {actual} bytes; maximum is {maximum}")]
    PayloadTooLong {
        /// Attempted length.
        actual: usize,
        /// Maximum length.
        maximum: usize,
    },
    /// A DMR identifier exceeds its unsigned 24-bit wire representation.
    #[error("DMR ID value {value} exceeds maximum {}", DmrId::MAX.get())]
    DmrIdOutOfRange {
        /// Attempted value.
        value: u32,
    },
}

/// Compute `SHA-256(challenge || password)`.
///
/// Both inputs are exact bytes with no terminator or text conversion. See the
/// [authentication example](crate#authentication).
#[must_use]
pub fn authentication_digest(challenge: &[u8], password: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(challenge);
    hasher.update(password);
    hasher.finalize().into()
}
