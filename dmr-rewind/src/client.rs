// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Connected UDP driver for the receive-only Open DMR Terminal flow.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, SystemTime};

use dmr_rewind_core::{
    DmrAudioSubtype, FullLinkControl, MAX_DATAGRAM_LEN, MAX_PAYLOAD_LEN, Packet, PacketFlags,
    PacketType, Payload, SERVICE_OPEN_TERMINAL, Subscription as WireSubscription,
    TransportSequence, VersionData, authentication_digest,
};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

use crate::config::{ConfigError, DestinationId, SubscriberId, SubscriptionKind};

/// Default maximum interval without a valid server datagram.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Default periodic Open DMR Terminal keepalive interval.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
/// Default deadline for one socket setup operation or control datagram send.
pub const DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum operational events retained while authentication/subscription
/// recovery temporarily prevents delivery to the caller.
pub const MAX_PENDING_EVENTS: usize = 1_024;
/// Maximum exact datagram bytes retained by the handshake/recovery backlog.
pub const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_DESCRIPTION_LEN: usize = MAX_PAYLOAD_LEN - 5;

/// One group- or private-voice destination requested from a master.
///
/// See the [connection example](crate#example).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Subscription {
    /// Kind of DMR voice session.
    kind: SubscriptionKind,
    /// Talkgroup or private DMR ID.
    destination_id: DestinationId,
}

impl Subscription {
    /// Create a group-voice subscription.
    #[must_use]
    pub const fn group(destination_id: DestinationId) -> Self {
        Self {
            kind: SubscriptionKind::Group,
            destination_id,
        }
    }

    /// Create a private-voice subscription.
    #[must_use]
    pub const fn private(destination_id: DestinationId) -> Self {
        Self {
            kind: SubscriptionKind::Private,
            destination_id,
        }
    }

    /// Construct one subscription from its validated kind and destination.
    #[must_use]
    pub const fn new(kind: SubscriptionKind, destination_id: DestinationId) -> Self {
        Self {
            kind,
            destination_id,
        }
    }

    /// Return the outbound voice-session kind.
    #[must_use]
    pub const fn kind(self) -> SubscriptionKind {
        self.kind
    }

    /// Return the validated destination.
    #[must_use]
    pub const fn destination_id(self) -> DestinationId {
        self.destination_id
    }
}

/// Settings for one Open DMR Terminal connection.
///
/// Hostname resolution deliberately belongs to the caller. Supplying an exact
/// [`SocketAddr`] lets a supervisor choose IPv4 or IPv6 and makes reconnect
/// behavior explicit.
/// See the [connection example](crate#example).
#[derive(Clone)]
pub struct ClientConfig {
    server: SocketAddr,
    bind: SocketAddr,
    dmr_id: SubscriberId,
    password: String,
    description: String,
    subscriptions: Vec<Subscription>,
    timeout: Duration,
    keepalive_interval: Duration,
    send_timeout: Duration,
}

impl ClientConfig {
    /// Create a configuration with protocol-default timers and no
    /// subscriptions.
    ///
    /// `dmr_id` is the operator's seven-digit `RadioID` allocation and
    /// `password` is that ID's `BrandMeister` hotspot-security password.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ZeroServerPort`] or [`ConfigError::EmptyPassword`]
    /// before retaining a usable configuration.
    pub fn new(
        server: SocketAddr,
        dmr_id: SubscriberId,
        password: String,
    ) -> Result<Self, ConfigError> {
        let bind_ip = match server.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        let config = Self {
            server,
            bind: SocketAddr::new(bind_ip, 0),
            dmr_id,
            password,
            description: format!("dmr-rewind/{}", env!("CARGO_PKG_VERSION")),
            subscriptions: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
            send_timeout: DEFAULT_SEND_TIMEOUT,
        };
        config.validate()?;
        Ok(config)
    }

    /// Set the local UDP address.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::AddressFamily`] when local and remote families differ.
    pub fn with_bind(mut self, bind: SocketAddr) -> Result<Self, ConfigError> {
        self.bind = bind;
        self.validate()?;
        Ok(self)
    }

    /// Set the UTF-8 client description sent in keepalives.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::EmptyDescription`] or [`ConfigError::DescriptionTooLong`].
    pub fn with_description(mut self, description: String) -> Result<Self, ConfigError> {
        self.description = description;
        self.validate()?;
        Ok(self)
    }

    /// Replace the group/private subscription list.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::DuplicateSubscription`] for duplicate kind/destination pairs.
    pub fn with_subscriptions(
        mut self,
        subscriptions: Vec<Subscription>,
    ) -> Result<Self, ConfigError> {
        self.subscriptions = subscriptions;
        self.validate()?;
        Ok(self)
    }

    /// Set the maximum interval without a valid server datagram.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::TimerRelationship`] when the timeout does not exceed
    /// the keepalive, or [`ConfigError::ClockRange`] if it cannot form a deadline.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self, ConfigError> {
        self.timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    /// Set the periodic keepalive interval.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::TimerRelationship`] for zero or an interval not below
    /// the timeout, or [`ConfigError::ClockRange`] for an unrepresentable deadline.
    pub fn with_keepalive_interval(mut self, interval: Duration) -> Result<Self, ConfigError> {
        self.keepalive_interval = interval;
        self.validate()?;
        Ok(self)
    }

    /// Set a positive deadline for each socket setup operation and control send.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::ZeroSendTimeout`] for zero, or [`ConfigError::ClockRange`]
    /// when the runtime cannot represent its deadline.
    pub fn with_send_timeout(mut self, timeout: Duration) -> Result<Self, ConfigError> {
        self.send_timeout = timeout;
        self.validate()?;
        Ok(self)
    }

    /// Return the socket setup and control-send deadline interval.
    #[must_use]
    pub const fn send_timeout(&self) -> Duration {
        self.send_timeout
    }

    /// Return the configured master endpoint.
    #[must_use]
    pub const fn server(&self) -> SocketAddr {
        self.server
    }

    /// Return the configured local bind address.
    #[must_use]
    pub const fn bind(&self) -> SocketAddr {
        self.bind
    }

    /// Return the seven-digit DMR ID.
    #[must_use]
    pub const fn dmr_id(&self) -> SubscriberId {
        self.dmr_id
    }

    /// Return the client description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Return the requested subscriptions.
    #[must_use]
    pub fn subscriptions(&self) -> &[Subscription] {
        &self.subscriptions
    }

    /// Return the valid-packet timeout.
    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Return the keepalive interval.
    #[must_use]
    pub const fn keepalive_interval(&self) -> Duration {
        self.keepalive_interval
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.server.port() == 0 {
            return Err(ConfigError::ZeroServerPort);
        }
        if self.bind.is_ipv4() != self.server.is_ipv4() {
            return Err(ConfigError::AddressFamily);
        }
        if self.password.is_empty() {
            return Err(ConfigError::EmptyPassword);
        }
        if self.description.is_empty() {
            return Err(ConfigError::EmptyDescription);
        }
        if self.description.len() > MAX_DESCRIPTION_LEN {
            return Err(ConfigError::DescriptionTooLong {
                actual: self.description.len(),
                maximum: MAX_DESCRIPTION_LEN,
            });
        }
        if self.keepalive_interval.is_zero() || self.timeout <= self.keepalive_interval {
            return Err(ConfigError::TimerRelationship {
                timeout: self.timeout,
                keepalive: self.keepalive_interval,
            });
        }
        if self.send_timeout.is_zero() {
            return Err(ConfigError::ZeroSendTimeout);
        }
        let validation_instant = Instant::now();
        for duration in [self.timeout, self.keepalive_interval, self.send_timeout] {
            if validation_instant.checked_add(duration).is_none() {
                return Err(ConfigError::ClockRange { duration });
            }
        }

        let mut unique = HashSet::with_capacity(self.subscriptions.len());
        for subscription in &self.subscriptions {
            if !unique.insert(*subscription) {
                return Err(ConfigError::DuplicateSubscription {
                    kind: subscription.kind,
                    destination: subscription.destination_id,
                });
            }
        }
        Ok(())
    }
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientConfig")
            .field("server", &self.server)
            .field("bind", &self.bind)
            .field("dmr_id", &self.dmr_id)
            .field("password", &"[REDACTED]")
            .field("description", &self.description)
            .field("subscriptions", &self.subscriptions)
            .field("timeout", &self.timeout)
            .field("keepalive_interval", &self.keepalive_interval)
            .field("send_timeout", &self.send_timeout)
            .finish()
    }
}

/// Transport metadata shared by every received event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMetadata {
    /// REWIND transport sequence.
    sequence: TransportSequence,
    /// REWIND envelope flags, including unknown future bits.
    flags: PacketFlags,
    /// Payload length declared by the REWIND envelope.
    payload_len: u16,
    /// Exact UDP datagram received from the connected master.
    raw_datagram: Vec<u8>,
    /// Local wall-clock receipt time.
    received_at: SystemTime,
}

/// Exact DMR voice-header event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoiceHeaderEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// The authoritative full-link-control fields; raw data is derived from them.
    link_control: FullLinkControl,
}

/// Exact DMR terminator event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminatorEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// The authoritative full-link-control fields, or an observed empty body.
    link_control: Option<FullLinkControl>,
}

/// Exact packed AMBE+2 audio event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// REWIND audio subtype in `0..=6`.
    subtype: DmrAudioSubtype,
    /// Three exact nine-byte AMBE+2 mode-33 codewords.
    data: [u8; 27],
}

/// Uninterpreted DMR embedded-data event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedDataEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// Exact body bytes, bounded by the outer REWIND datagram limit.
    data: Vec<u8>,
}

/// Parsed 32-byte call-metadata superheader event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuperHeaderEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// Parsed source, destination, session type, and callsigns.
    data: dmr_rewind_core::SuperHeader,
}

/// Opaque server report, failure, or busy-notice event.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoticeEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// Exact notice payload bytes.
    data: Vec<u8>,
}

/// Any decoded packet without a dedicated [`Event`] variant.
///
/// See the [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OtherEvent {
    /// Transport and receipt metadata.
    metadata: EventMetadata,
    /// Known or future REWIND packet type.
    packet_type: PacketType,
    /// Typed or opaque decoded payload.
    payload: Payload,
}

impl EventMetadata {
    /// Return the exact transport sequence word.
    #[must_use]
    pub const fn sequence(&self) -> TransportSequence {
        self.sequence
    }

    /// Return every envelope flag bit.
    #[must_use]
    pub const fn flags(&self) -> PacketFlags {
        self.flags
    }

    /// Return the validated envelope's declared payload byte count.
    #[must_use]
    pub const fn payload_len(&self) -> u16 {
        self.payload_len
    }

    /// Borrow the exact datagram, including its envelope.
    #[must_use]
    pub fn raw_datagram(&self) -> &[u8] {
        &self.raw_datagram
    }

    /// Return the local wall-clock receipt timestamp.
    #[must_use]
    pub const fn received_at(&self) -> SystemTime {
        self.received_at
    }

    /// Transfer the exact datagram without cloning its allocation.
    #[must_use]
    pub fn into_raw_datagram(self) -> Vec<u8> {
        self.raw_datagram
    }
}

// Every concrete event retains the same immutable envelope view.
macro_rules! metadata_accessors {
    ($($event:ty),+ $(,)?) => { $(
        impl $event {
            /// Borrow the immutable transport and receipt metadata.
            #[must_use]
            pub const fn metadata(&self) -> &EventMetadata { &self.metadata }
        }
    )+ };
}

metadata_accessors!(
    VoiceHeaderEvent,
    TerminatorEvent,
    AudioEvent,
    EmbeddedDataEvent,
    SuperHeaderEvent,
    NoticeEvent,
    OtherEvent
);

impl VoiceHeaderEvent {
    /// Return the authoritative full-link-control fields.
    #[must_use]
    pub const fn link_control(&self) -> FullLinkControl {
        self.link_control
    }

    /// Return the exact twelve wire bytes derived from full link control.
    #[must_use]
    pub const fn data(&self) -> [u8; 12] {
        self.link_control.to_bytes()
    }
}

impl TerminatorEvent {
    /// Return full link control, or `None` for an observed empty terminator.
    #[must_use]
    pub const fn link_control(&self) -> Option<FullLinkControl> {
        self.link_control
    }

    /// Return the exact observed twelve-byte body, or `None` for an empty body.
    #[must_use]
    pub const fn data(&self) -> Option<[u8; 12]> {
        match self.link_control {
            Some(control) => Some(control.to_bytes()),
            None => None,
        }
    }
}

impl AudioEvent {
    /// Return the validated audio subtype in zero through six.
    #[must_use]
    pub const fn subtype(&self) -> DmrAudioSubtype {
        self.subtype
    }

    /// Return the three exact nine-byte AMBE+2 codewords.
    #[must_use]
    pub const fn data(&self) -> [u8; 27] {
        self.data
    }
}

impl EmbeddedDataEvent {
    /// Borrow the uninterpreted body under the outer datagram limit.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Transfer the uninterpreted body without cloning its allocation.
    #[must_use]
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

impl SuperHeaderEvent {
    /// Return the parsed call metadata with all four-byte identity bits intact.
    #[must_use]
    pub const fn data(&self) -> dmr_rewind_core::SuperHeader {
        self.data
    }
}

impl NoticeEvent {
    /// Borrow the exact server notice bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Transfer the notice bytes without cloning their allocation.
    #[must_use]
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

impl OtherEvent {
    /// Return the known or unmodeled packet code.
    #[must_use]
    pub const fn packet_type(&self) -> PacketType {
        self.packet_type
    }

    /// Borrow the parsed payload.
    #[must_use]
    pub const fn payload(&self) -> &Payload {
        &self.payload
    }

    /// Transfer the parsed payload without cloning its storage.
    #[must_use]
    pub fn into_payload(self) -> Payload {
        self.payload
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PacketEnvelope {
    packet: Packet,
    raw_datagram: Vec<u8>,
    received_at: SystemTime,
}

/// One datagram with immutable, mutually consistent wire and parsed views.
///
/// Live clients construct events from received datagrams. [`Self::from_datagram`]
/// supports offline captures and does not authenticate their sender. See the
/// [event example](crate#example) and
/// [fidelity contract](crate#event-fidelity).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    kind: EventKind,
}

/// The owned packet-specific view of a validated [`Event`].
///
/// Inspect through [`Event::kind`] or consume through [`Event::into_kind`].
/// This enum is an inspection value, not an event-construction boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EventKind {
    /// Twelve-byte DMR full-link-control voice header.
    VoiceHeader(VoiceHeaderEvent),
    /// Twelve-byte DMR full-link-control terminator.
    Terminator(TerminatorEvent),
    /// Twenty-seven-byte, three-codeword AMBE+2 burst.
    Audio(AudioEvent),
    /// Uninterpreted DMR embedded-data payload.
    EmbeddedData(EmbeddedDataEvent),
    /// Optional source/destination metadata supplied by the master.
    SuperHeader(SuperHeaderEvent),
    /// Server diagnostic report.
    Report(NoticeEvent),
    /// Server application failure notice.
    Failure(NoticeEvent),
    /// Server busy notice.
    BusyNotice(NoticeEvent),
    /// The master closed this terminal session.
    Close(EventMetadata),
    /// Any other known control/data packet or future extension.
    Other(OtherEvent),
}

impl Event {
    /// Parse exact datagram bytes and retain them with a caller-supplied receipt time.
    ///
    /// # Errors
    ///
    /// Returns the malformed-envelope/body errors from [`Packet::from_bytes`].
    pub fn from_datagram(
        raw_datagram: Vec<u8>,
        received_at: SystemTime,
    ) -> Result<Self, dmr_rewind_core::CodecError> {
        let packet = Packet::from_bytes(&raw_datagram)?;
        Ok(Self::from_envelope(PacketEnvelope {
            packet,
            raw_datagram,
            received_at,
        }))
    }

    /// Borrow the packet-specific view without exposing mutable state.
    #[must_use]
    pub const fn kind(&self) -> &EventKind {
        &self.kind
    }

    /// Transfer the packet-specific view and its existing payload storage.
    #[must_use]
    pub fn into_kind(self) -> EventKind {
        self.kind
    }

    /// Borrow transport and receipt metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadata {
        match &self.kind {
            EventKind::VoiceHeader(event) => &event.metadata,
            EventKind::Terminator(event) => &event.metadata,
            EventKind::Audio(event) => &event.metadata,
            EventKind::EmbeddedData(event) => &event.metadata,
            EventKind::SuperHeader(event) => &event.metadata,
            EventKind::Report(event) | EventKind::Failure(event) | EventKind::BusyNotice(event) => {
                &event.metadata
            }
            EventKind::Close(metadata) => metadata,
            EventKind::Other(event) => &event.metadata,
        }
    }

    fn from_envelope(envelope: PacketEnvelope) -> Self {
        let PacketEnvelope {
            packet,
            raw_datagram,
            received_at,
        } = envelope;
        let (header, payload) = packet.into_parts();
        let metadata = EventMetadata {
            sequence: header.sequence,
            flags: header.flags,
            payload_len: header.payload_len,
            raw_datagram,
            received_at,
        };
        let kind = match (header.packet_type, payload) {
            (PacketType::DmrVoiceHeader, Payload::DmrVoiceHeader(link_control)) => {
                EventKind::VoiceHeader(VoiceHeaderEvent {
                    metadata,
                    link_control,
                })
            }
            (PacketType::DmrTerminator, Payload::DmrTerminator(link_control)) => {
                EventKind::Terminator(TerminatorEvent {
                    metadata,
                    link_control,
                })
            }
            (PacketType::DmrAudio(subtype), Payload::DmrAudio(data)) => {
                EventKind::Audio(AudioEvent {
                    metadata,
                    subtype,
                    data,
                })
            }
            (PacketType::DmrEmbeddedData, Payload::DmrEmbeddedData(data)) => {
                EventKind::EmbeddedData(EmbeddedDataEvent { metadata, data })
            }
            (PacketType::SuperHeader, Payload::SuperHeader(data)) => {
                EventKind::SuperHeader(SuperHeaderEvent { metadata, data })
            }
            (PacketType::Report, Payload::Report(data)) => {
                EventKind::Report(NoticeEvent { metadata, data })
            }
            (PacketType::Failure, Payload::Failure(data)) => {
                EventKind::Failure(NoticeEvent { metadata, data })
            }
            (PacketType::BusyNotice, Payload::Busy(data)) => {
                EventKind::BusyNotice(NoticeEvent { metadata, data })
            }
            (PacketType::Close, Payload::Close) => EventKind::Close(metadata),
            (packet_type, payload) => EventKind::Other(OtherEvent {
                metadata,
                packet_type,
                payload,
            }),
        };
        Self { kind }
    }
}

/// Open DMR Terminal socket, protocol, or configuration failure.
///
/// See the [fallible connection example](crate#example).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A setting cannot produce a valid terminal session.
    #[error("invalid Open DMR Terminal configuration: {0}")]
    InvalidConfig(#[from] ConfigError),
    /// The local UDP socket could not be created.
    #[error("bind UDP socket {address}: {source}")]
    Bind {
        /// Requested local address.
        address: SocketAddr,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// The UDP socket could not be connected to the selected master.
    #[error("connect UDP socket to {address}: {source}")]
    Connect {
        /// Selected master address.
        address: SocketAddr,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// A UDP datagram could not be sent.
    #[error("{operation}: {source}")]
    Send {
        /// Protocol operation being attempted.
        operation: ControlOperation,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// A UDP datagram could not be received.
    #[error("receive from Open DMR Terminal master: {0}")]
    Receive(#[source] io::Error),
    /// A datagram was malformed or an outbound packet could not be constructed.
    #[error("REWIND codec: {0}")]
    Codec(#[from] dmr_rewind_core::CodecError),
    /// A handshake phase received no valid response before its deadline.
    #[error("Open DMR Terminal {phase} timed out after {timeout:?}")]
    HandshakeTimeout {
        /// Human-readable handshake phase.
        phase: &'static str,
        /// Configured per-phase timeout.
        timeout: Duration,
    },
    /// A valid but phase-inappropriate control packet was received.
    #[error("unexpected {packet_type:?} packet while waiting for {phase}")]
    UnexpectedHandshakePacket {
        /// Human-readable handshake phase.
        phase: &'static str,
        /// Received packet type.
        packet_type: PacketType,
    },
    /// The master rejected authentication or another handshake operation.
    #[error("Open DMR Terminal handshake rejected: {message}")]
    HandshakeRejected {
        /// Lossy, display-oriented server message.
        message: String,
    },
    /// An authentication challenge had an unsupported length.
    #[error("Open DMR Terminal challenge has {actual} bytes; exactly 4 required")]
    InvalidChallengeLength {
        /// Received challenge byte count.
        actual: usize,
    },
    /// No valid datagram arrived during the configured session timeout.
    #[error("Open DMR Terminal session timed out after {timeout:?}")]
    SessionTimeout {
        /// Configured valid-packet timeout.
        timeout: Duration,
    },
    /// A requested deadline interval cannot be represented by the runtime clock.
    #[error("deadline interval {duration:?} is too large")]
    InvalidWaitDuration {
        /// Requested interval from the current runtime instant.
        duration: Duration,
    },
    /// Operational traffic exceeded the bounded handshake/recovery backlog.
    #[error(
        "Open DMR Terminal handshake backlog exceeded its limit: {events} events/{bytes} bytes (maximum {max_events}/{max_bytes})"
    )]
    PendingQueueOverflow {
        /// Event count the rejected insertion would have produced.
        events: usize,
        /// Exact datagram bytes the rejected insertion would have retained.
        bytes: usize,
        /// Configured event-count ceiling.
        max_events: usize,
        /// Configured byte ceiling.
        max_bytes: usize,
    },
    /// The local client was already closed.
    #[error("Open DMR Terminal client is closed")]
    ClientClosed,
    /// A socket setup operation or control send exceeded its configured deadline.
    #[error("{operation} exceeded its {timeout:?} deadline")]
    ControlTimeout {
        /// Operation whose deadline expired.
        operation: ControlOperation,
        /// Configured budget for that operation.
        timeout: Duration,
    },
    /// A caller dropped a future while a control datagram send was pending.
    #[error("a control send was interrupted; drop this client and reconnect")]
    ControlSendInterrupted,
}

/// One bounded socket setup or outbound control operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlOperation {
    /// Create the local UDP endpoint.
    Bind,
    /// Connect it to the selected remote endpoint.
    Connect,
    /// Send the terminal description or keepalive.
    Keepalive,
    /// Send the challenge response.
    Authentication,
    /// Install one group/private subscription.
    Subscription,
    /// Send a graceful Close datagram.
    Close,
}

impl fmt::Display for ControlOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bind => "bind UDP socket",
            Self::Connect => "connect UDP socket",
            Self::Keepalive => "send keepalive",
            Self::Authentication => "send authentication",
            Self::Subscription => "send subscription",
            Self::Close => "send close",
        })
    }
}

#[derive(Debug, Default)]
struct SequenceCounters {
    routine: u32,
    real_time_one: u32,
}

#[derive(Debug, Default)]
struct PendingEvents {
    events: VecDeque<Event>,
    datagram_bytes: usize,
}

impl PendingEvents {
    fn push(&mut self, event: Event) -> Result<(), Error> {
        let events = self.events.len().saturating_add(1);
        let bytes = self
            .datagram_bytes
            .saturating_add(event.metadata().raw_datagram.len());
        if events > MAX_PENDING_EVENTS || bytes > MAX_PENDING_BYTES {
            return Err(Error::PendingQueueOverflow {
                events,
                bytes,
                max_events: MAX_PENDING_EVENTS,
                max_bytes: MAX_PENDING_BYTES,
            });
        }
        self.datagram_bytes = bytes;
        self.events.push_back(event);
        Ok(())
    }

    fn pop(&mut self) -> Option<Event> {
        let event = self.events.pop_front()?;
        self.datagram_bytes = self
            .datagram_bytes
            .saturating_sub(event.metadata().raw_datagram.len());
        Some(event)
    }

    fn len(&self) -> usize {
        self.events.len()
    }

    const fn bytes(&self) -> usize {
        self.datagram_bytes
    }
}

impl SequenceCounters {
    const fn next(&mut self, flags: PacketFlags) -> TransportSequence {
        let counter = if flags.contains(PacketFlags::REAL_TIME_1) {
            &mut self.real_time_one
        } else {
            &mut self.routine
        };
        let sequence = *counter;
        *counter = counter.wrapping_add(1);
        TransportSequence::new(sequence)
    }
}

#[derive(Clone, Copy, Debug)]
enum RuntimeState {
    Operational,
    Authentication {
        deadline: Instant,
    },
    Subscription {
        deadline: Instant,
        acknowledged_index: usize,
    },
}

/// Authenticated, receive-only `BrandMeister` Open DMR Terminal client.
///
/// The client intentionally has no DMR transmit method. Reconnection and DNS
/// policies belong to its caller.
/// See the [connection example](crate#example) and
/// [event contract](crate#event-fidelity).
pub struct Client {
    socket: UdpSocket,
    server: SocketAddr,
    dmr_id: SubscriberId,
    password: String,
    description: Vec<u8>,
    subscriptions: Vec<Subscription>,
    timeout: Duration,
    keepalive_interval: Duration,
    send_timeout: Duration,
    sequences: SequenceCounters,
    pending: PendingEvents,
    last_valid_packet: Instant,
    next_keepalive: Instant,
    receive_buffer: Vec<u8>,
    runtime_state: RuntimeState,
    closed: bool,
    control_pending: bool,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("server", &self.server)
            .field("dmr_id", &self.dmr_id)
            .field("password", &"[REDACTED]")
            .field("description", &self.description)
            .field("subscriptions", &self.subscriptions)
            .field("timeout", &self.timeout)
            .field("keepalive_interval", &self.keepalive_interval)
            .field("receive_buffer_capacity", &self.receive_buffer.capacity())
            .field("pending_events", &self.pending.len())
            .field("pending_datagram_bytes", &self.pending.bytes())
            .field("runtime_state", &self.runtime_state)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Bind, authenticate, and install all configured subscriptions.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for socket failures, malformed
    /// packets, a rejected login, a phase timeout, or a bounded pending-event
    /// backlog overflow.
    ///
    /// Each handshake receive phase uses the configured packet timeout. Each
    /// socket bind, connect and control send has its own configured send budget;
    /// these are per-operation deadlines, not one total handshake deadline.
    ///
    /// # Cancellation
    ///
    /// Dropping this future closes its local socket without sending Close.
    /// Already sent handshake datagrams remain visible to the master.
    pub async fn connect(config: ClientConfig) -> Result<Self, Error> {
        let socket = bounded_control(
            UdpSocket::bind(config.bind),
            config.send_timeout,
            ControlOperation::Bind,
        )
        .await
        .map_err(|error| match error {
            ControlFailure::Io(source) => Error::Bind {
                address: config.bind,
                source,
            },
            ControlFailure::Deadline(error) => error,
        })?;
        bounded_control(
            socket.connect(config.server),
            config.send_timeout,
            ControlOperation::Connect,
        )
        .await
        .map_err(|error| match error {
            ControlFailure::Io(source) => Error::Connect {
                address: config.server,
                source,
            },
            ControlFailure::Deadline(error) => error,
        })?;

        let now = Instant::now();
        let mut client = Self {
            socket,
            server: config.server,
            dmr_id: config.dmr_id,
            password: config.password,
            description: config.description.into_bytes(),
            subscriptions: config.subscriptions,
            timeout: config.timeout,
            keepalive_interval: config.keepalive_interval,
            send_timeout: config.send_timeout,
            sequences: SequenceCounters::default(),
            pending: PendingEvents::default(),
            last_valid_packet: now,
            next_keepalive: deadline_after(now, config.keepalive_interval)?,
            receive_buffer: vec![0_u8; MAX_DATAGRAM_LEN + 1],
            runtime_state: RuntimeState::Operational,
            closed: false,
            control_pending: false,
        };

        client.send_keepalive().await?;
        let challenge = client.await_challenge().await?;
        client.finish_authentication(challenge).await?;
        client.subscribe_all().await?;
        client.next_keepalive = deadline_after(Instant::now(), client.keepalive_interval)?;
        Ok(client)
    }

    /// Return the connected master endpoint.
    #[must_use]
    pub const fn server_addr(&self) -> SocketAddr {
        self.server
    }

    /// Return the socket's effective local address.
    ///
    /// # Errors
    ///
    /// Returns the operating-system socket error, if any.
    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.socket.local_addr()
    }

    /// Return whether close, a failed send or an interrupted send ended this session.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed || self.control_pending
    }

    /// Wait for the next operational packet.
    ///
    /// Keepalive acknowledgements are consumed internally. A fresh challenge
    /// is authenticated and all subscriptions are reinstalled before normal
    /// delivery resumes.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for transport/codec failures, reauthentication
    /// failure, pending-event backlog overflow, session timeout, or use after
    /// close.
    ///
    /// # Cancellation
    ///
    /// Dropping while receiving preserves queued events and recovery state; a
    /// datagram already received can have been consumed. Dropping during a
    /// control send leaves it unconfirmed: the next operation returns
    /// [`Error::ControlSendInterrupted`], after which the caller drops this client
    /// and reconnects. Use [`Self::next_event_for`] to bound receive waiting
    /// without cancelling an in-progress control send.
    pub async fn next_event(&mut self) -> Result<Event, Error> {
        loop {
            if let Some(event) = self.next_event_until(None).await? {
                return Ok(event);
            }
        }
    }

    /// Wait for an operational packet with a receive deadline of `max_wait`.
    ///
    /// Unlike placing an external timeout around [`Self::next_event`], this
    /// method owns its deadline and persists authentication/subscription
    /// recovery state across calls. It continues to send due keepalives and
    /// enforce the configured session timeout while waiting.
    /// Datagrams already queued for delivery are returned immediately. Control
    /// sends use their configured deadline and can extend the call beyond
    /// `max_wait` by at most one send budget. Already started sends are completed
    /// before the receive deadline is reported; they are not cancelled by it.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::next_event`], plus
    /// [`Error::InvalidWaitDuration`] when `max_wait` overflows the runtime
    /// clock.
    ///
    /// # Cancellation
    ///
    /// Returns `Ok(None)` at its receive deadline while retaining recovery state.
    /// Dropping its future has the same effects as [`Self::next_event`].
    pub async fn next_event_for(&mut self, max_wait: Duration) -> Result<Option<Event>, Error> {
        let caller_deadline = Instant::now()
            .checked_add(max_wait)
            .ok_or(Error::InvalidWaitDuration { duration: max_wait })?;
        self.next_event_until(Some(caller_deadline)).await
    }

    async fn next_event_until(
        &mut self,
        caller_deadline: Option<Instant>,
    ) -> Result<Option<Event>, Error> {
        ensure_usable(self.control_pending, &mut self.closed)?;
        if matches!(self.runtime_state, RuntimeState::Operational)
            && let Some(event) = self.pending.pop()
        {
            return Ok(Some(event));
        }

        loop {
            if caller_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(None);
            }
            let timeout_deadline = deadline_after(self.last_valid_packet, self.timeout)?;
            let state_deadline = match self.runtime_state {
                RuntimeState::Operational => self.next_keepalive,
                RuntimeState::Authentication { deadline }
                | RuntimeState::Subscription { deadline, .. } => deadline,
            };
            let protocol_deadline = timeout_deadline.min(state_deadline);
            let deadline =
                caller_deadline.map_or(protocol_deadline, |caller| caller.min(protocol_deadline));
            if let Ok(result) = timeout_at(deadline, self.receive()).await {
                let envelope = result?;
                if let Some(event) = self.handle_runtime_packet(envelope).await? {
                    return Ok(Some(event));
                }
                continue;
            }

            let now = Instant::now();
            if now >= timeout_deadline {
                return Err(Error::SessionTimeout {
                    timeout: self.timeout,
                });
            }
            if let Some(error) = self.runtime_handshake_timeout(now) {
                return Err(error);
            }
            if caller_deadline.is_some_and(|caller| now >= caller) {
                return Ok(None);
            }
            if matches!(self.runtime_state, RuntimeState::Operational) {
                self.send_keepalive().await?;
                self.next_keepalive = deadline_after(now, self.keepalive_interval)?;
            }
        }
    }

    async fn handle_runtime_packet(
        &mut self,
        envelope: PacketEnvelope,
    ) -> Result<Option<Event>, Error> {
        match self.runtime_state {
            RuntimeState::Operational => self.handle_operational_packet(envelope).await,
            RuntimeState::Authentication { deadline } => {
                self.handle_authentication_packet(envelope, deadline).await
            }
            RuntimeState::Subscription {
                acknowledged_index, ..
            } => {
                self.handle_subscription_packet(envelope, acknowledged_index)
                    .await
            }
        }
    }

    async fn handle_operational_packet(
        &mut self,
        envelope: PacketEnvelope,
    ) -> Result<Option<Event>, Error> {
        match envelope.packet.payload() {
            Payload::KeepAlive(_) => Ok(None),
            Payload::Challenge(challenge) => {
                self.begin_runtime_authentication(challenge, None).await?;
                Ok(None)
            }
            Payload::Close => {
                self.closed = true;
                Ok(Some(Event::from_envelope(envelope)))
            }
            _ => Ok(Some(Event::from_envelope(envelope))),
        }
    }

    async fn handle_authentication_packet(
        &mut self,
        envelope: PacketEnvelope,
        deadline: Instant,
    ) -> Result<Option<Event>, Error> {
        let phase = "authentication acknowledgement";
        match envelope.packet.payload() {
            Payload::KeepAlive(_) => {
                self.begin_runtime_subscriptions().await?;
                Ok(self.pending_event_if_recovered())
            }
            Payload::Challenge(challenge) => {
                self.begin_runtime_authentication(challenge, Some(deadline))
                    .await?;
                Ok(None)
            }
            Payload::Close => {
                self.closed = true;
                Ok(Some(Event::from_envelope(envelope)))
            }
            Payload::Failure(message) => Err(handshake_rejection(message)),
            _ if is_operational(&envelope.packet) => {
                self.queue_pending(envelope)?;
                Ok(None)
            }
            _ => Err(Error::UnexpectedHandshakePacket {
                phase,
                packet_type: envelope.packet.header().packet_type,
            }),
        }
    }

    async fn handle_subscription_packet(
        &mut self,
        envelope: PacketEnvelope,
        acknowledged_index: usize,
    ) -> Result<Option<Event>, Error> {
        let phase = "subscription acknowledgement";
        match envelope.packet.payload() {
            Payload::Subscription(acknowledgement)
                if self
                    .subscriptions
                    .get(acknowledged_index)
                    .copied()
                    .is_some_and(|expected| {
                        subscription_acknowledges(*acknowledgement, expected)
                    }) =>
            {
                self.advance_runtime_subscription(acknowledged_index)
                    .await?;
                Ok(self.pending_event_if_recovered())
            }
            Payload::Subscription(_) | Payload::KeepAlive(_) => Ok(None),
            Payload::Challenge(challenge) => {
                self.begin_runtime_authentication(challenge, None).await?;
                Ok(None)
            }
            Payload::Close => {
                self.closed = true;
                Ok(Some(Event::from_envelope(envelope)))
            }
            Payload::Failure(message) => Err(handshake_rejection(message)),
            _ if is_operational(&envelope.packet) => {
                self.queue_pending(envelope)?;
                Ok(None)
            }
            _ => Err(Error::UnexpectedHandshakePacket {
                phase,
                packet_type: envelope.packet.header().packet_type,
            }),
        }
    }

    async fn begin_runtime_authentication(
        &mut self,
        challenge: &[u8],
        existing_deadline: Option<Instant>,
    ) -> Result<(), Error> {
        let challenge = challenge_bytes(challenge)?;
        let deadline = match existing_deadline {
            Some(deadline) => deadline,
            None => deadline_after(Instant::now(), self.timeout)?,
        };
        let digest = authentication_digest(&challenge, self.password.as_bytes());
        self.runtime_state = RuntimeState::Authentication { deadline };
        self.send_payload(
            PacketType::Authentication,
            Payload::Authentication(digest),
            ControlOperation::Authentication,
        )
        .await
    }

    async fn begin_runtime_subscriptions(&mut self) -> Result<(), Error> {
        let Some(subscription) = self.subscriptions.first().copied() else {
            self.finish_runtime_recovery()?;
            return Ok(());
        };
        self.send_runtime_subscription(0, subscription).await
    }

    async fn advance_runtime_subscription(
        &mut self,
        acknowledged_index: usize,
    ) -> Result<(), Error> {
        let next_index = acknowledged_index.saturating_add(1);
        let Some(subscription) = self.subscriptions.get(next_index).copied() else {
            self.finish_runtime_recovery()?;
            return Ok(());
        };
        self.send_runtime_subscription(next_index, subscription)
            .await
    }

    async fn send_runtime_subscription(
        &mut self,
        index: usize,
        subscription: Subscription,
    ) -> Result<(), Error> {
        self.runtime_state = RuntimeState::Subscription {
            deadline: deadline_after(Instant::now(), self.timeout)?,
            acknowledged_index: index,
        };
        self.send_subscription(subscription).await
    }

    fn finish_runtime_recovery(&mut self) -> Result<(), Error> {
        let next_keepalive = deadline_after(Instant::now(), self.keepalive_interval)?;
        self.runtime_state = RuntimeState::Operational;
        self.next_keepalive = next_keepalive;
        Ok(())
    }

    fn pending_event_if_recovered(&mut self) -> Option<Event> {
        if matches!(self.runtime_state, RuntimeState::Operational) {
            self.pending.pop()
        } else {
            None
        }
    }

    fn runtime_handshake_timeout(&self, now: Instant) -> Option<Error> {
        let (phase, deadline) = match self.runtime_state {
            RuntimeState::Operational => return None,
            RuntimeState::Authentication { deadline } => {
                ("authentication acknowledgement", deadline)
            }
            RuntimeState::Subscription { deadline, .. } => {
                ("subscription acknowledgement", deadline)
            }
        };
        (now >= deadline).then_some(Error::HandshakeTimeout {
            phase,
            timeout: self.timeout,
        })
    }

    /// Send the protocol close packet and mark this local client closed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Send`] or [`Error::ControlTimeout`] if the close datagram
    /// could not be sent within its budget, or [`Error::ControlSendInterrupted`]
    /// after a caller-cancelled control send.
    ///
    /// # Cancellation
    ///
    /// The send has the configured send deadline. Dropping it while pending
    /// leaves an unconfirmed send: the next operation reports
    /// [`Error::ControlSendInterrupted`]. Drop the client to release its socket;
    /// no background task continues the send. Successful completion marks it
    /// closed without awaiting a response from the master.
    pub async fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        if self.control_pending {
            self.closed = true;
            return Err(Error::ControlSendInterrupted);
        }
        self.send_payload(PacketType::Close, Payload::Close, ControlOperation::Close)
            .await?;
        self.closed = true;
        Ok(())
    }

    async fn await_challenge(&mut self) -> Result<[u8; 4], Error> {
        let phase = "authentication challenge";
        let deadline = deadline_after(Instant::now(), self.timeout)?;
        loop {
            let envelope = self.receive_handshake(deadline, phase).await?;
            match envelope.packet.payload() {
                Payload::Challenge(challenge) => return challenge_bytes(challenge),
                Payload::Close => return Err(remote_close_rejection()),
                Payload::Failure(message) => {
                    return Err(handshake_rejection(message));
                }
                _ if is_operational(&envelope.packet) => {
                    self.queue_pending(envelope)?;
                }
                _ => {
                    return Err(Error::UnexpectedHandshakePacket {
                        phase,
                        packet_type: envelope.packet.header().packet_type,
                    });
                }
            }
        }
    }

    async fn finish_authentication(&mut self, mut challenge: [u8; 4]) -> Result<(), Error> {
        let phase = "authentication acknowledgement";
        let deadline = deadline_after(Instant::now(), self.timeout)?;
        loop {
            let digest = authentication_digest(&challenge, self.password.as_bytes());
            self.send_payload(
                PacketType::Authentication,
                Payload::Authentication(digest),
                ControlOperation::Authentication,
            )
            .await?;

            loop {
                let envelope = self.receive_handshake(deadline, phase).await?;
                match envelope.packet.payload() {
                    Payload::KeepAlive(_) => return Ok(()),
                    Payload::Challenge(replacement) => {
                        challenge = challenge_bytes(replacement)?;
                        break;
                    }
                    Payload::Close => return Err(remote_close_rejection()),
                    Payload::Failure(message) => {
                        return Err(handshake_rejection(message));
                    }
                    _ if is_operational(&envelope.packet) => {
                        self.queue_pending(envelope)?;
                    }
                    _ => {
                        return Err(Error::UnexpectedHandshakePacket {
                            phase,
                            packet_type: envelope.packet.header().packet_type,
                        });
                    }
                }
            }
        }
    }

    async fn subscribe_all(&mut self) -> Result<(), Error> {
        let subscriptions = self.subscriptions.clone();
        'restart: loop {
            for subscription in &subscriptions {
                self.send_subscription(*subscription).await?;
                if self.await_subscription_ack(*subscription).await? {
                    continue 'restart;
                }
            }
            return Ok(());
        }
    }

    /// Return `true` when reauthentication requires restarting subscriptions.
    async fn await_subscription_ack(&mut self, expected: Subscription) -> Result<bool, Error> {
        let phase = "subscription acknowledgement";
        let deadline = deadline_after(Instant::now(), self.timeout)?;
        loop {
            let envelope = self.receive_handshake(deadline, phase).await?;
            match envelope.packet.payload() {
                Payload::Subscription(acknowledgement)
                    if subscription_acknowledges(*acknowledgement, expected) =>
                {
                    return Ok(false);
                }
                Payload::Subscription(_) | Payload::KeepAlive(_) => {}
                Payload::Challenge(challenge) => {
                    self.finish_authentication(challenge_bytes(challenge)?)
                        .await?;
                    return Ok(true);
                }
                Payload::Close => return Err(remote_close_rejection()),
                Payload::Failure(message) => {
                    return Err(handshake_rejection(message));
                }
                _ if is_operational(&envelope.packet) => {
                    self.queue_pending(envelope)?;
                }
                _ => {
                    return Err(Error::UnexpectedHandshakePacket {
                        phase,
                        packet_type: envelope.packet.header().packet_type,
                    });
                }
            }
        }
    }

    async fn send_subscription(&mut self, subscription: Subscription) -> Result<(), Error> {
        let wire = WireSubscription {
            session_type: subscription.kind.into(),
            target: subscription.destination_id.into(),
        };
        self.send_payload(
            PacketType::Subscription,
            Payload::Subscription(Some(wire)),
            ControlOperation::Subscription,
        )
        .await
    }

    fn queue_pending(&mut self, envelope: PacketEnvelope) -> Result<(), Error> {
        self.pending.push(Event::from_envelope(envelope))
    }

    async fn send_keepalive(&mut self) -> Result<(), Error> {
        let description = self.description.clone();
        self.send_payload(
            PacketType::KeepAlive,
            Payload::KeepAlive(Some(VersionData {
                remote_id: self.dmr_id.get().into(),
                service: SERVICE_OPEN_TERMINAL,
                description,
            })),
            ControlOperation::Keepalive,
        )
        .await
    }

    async fn send_payload(
        &mut self,
        packet_type: PacketType,
        payload: Payload,
        operation: ControlOperation,
    ) -> Result<(), Error> {
        let flags = PacketFlags::NONE;
        let packet = Packet::new(packet_type, flags, self.sequences.next(flags), payload)?;
        let datagram = packet.to_bytes();
        complete_control_send(
            &mut self.control_pending,
            &mut self.closed,
            self.socket.send(&datagram),
            datagram.len(),
            self.send_timeout,
            operation,
        )
        .await
    }

    async fn receive_handshake(
        &mut self,
        deadline: Instant,
        phase: &'static str,
    ) -> Result<PacketEnvelope, Error> {
        match timeout_at(deadline, self.receive()).await {
            Ok(result) => result,
            Err(_) => Err(Error::HandshakeTimeout {
                phase,
                timeout: self.timeout,
            }),
        }
    }

    async fn receive(&mut self) -> Result<PacketEnvelope, Error> {
        let received = self
            .socket
            .recv(&mut self.receive_buffer)
            .await
            .map_err(Error::Receive)?;
        let datagram = self
            .receive_buffer
            .get(..received)
            .ok_or_else(|| {
                Error::Receive(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "UDP receive length exceeded its buffer",
                ))
            })?
            .to_vec();
        let packet = Packet::from_bytes(&datagram)?;
        self.last_valid_packet = Instant::now();
        Ok(PacketEnvelope {
            packet,
            raw_datagram: datagram,
            received_at: SystemTime::now(),
        })
    }
}

const fn is_operational(packet: &Packet) -> bool {
    !matches!(
        packet.header().packet_type,
        PacketType::KeepAlive
            | PacketType::Close
            | PacketType::Challenge
            | PacketType::Authentication
            | PacketType::Configuration
            | PacketType::Subscription
            | PacketType::Cancelling
    )
}

fn deadline_after(start: Instant, duration: Duration) -> Result<Instant, Error> {
    start
        .checked_add(duration)
        .ok_or(Error::InvalidWaitDuration { duration })
}

const fn ensure_usable(control_pending: bool, closed: &mut bool) -> Result<(), Error> {
    if control_pending {
        *closed = true;
        return Err(Error::ControlSendInterrupted);
    }
    if *closed {
        Err(Error::ClientClosed)
    } else {
        Ok(())
    }
}

async fn complete_control_send(
    pending: &mut bool,
    closed: &mut bool,
    future: impl Future<Output = io::Result<usize>>,
    length: usize,
    budget: Duration,
    operation: ControlOperation,
) -> Result<(), Error> {
    *pending = true;
    let result = bounded_control(future, budget, operation).await;
    *pending = false;
    let sent = match result {
        Ok(sent) => sent,
        Err(error) => {
            *closed = true;
            return Err(match error {
                ControlFailure::Io(source) => Error::Send { operation, source },
                ControlFailure::Deadline(error) => error,
            });
        }
    };
    if sent != length {
        *closed = true;
        return Err(Error::Send {
            operation,
            source: io::Error::new(
                io::ErrorKind::WriteZero,
                "UDP socket reported a partial datagram send",
            ),
        });
    }
    Ok(())
}

#[derive(Debug)]
enum ControlFailure {
    Io(io::Error),
    Deadline(Error),
}

/// Bound socket setup and datagram sends before polling their I/O future.
async fn bounded_control<T>(
    operation: impl Future<Output = io::Result<T>>,
    budget: Duration,
    kind: ControlOperation,
) -> Result<T, ControlFailure> {
    let deadline = deadline_after(Instant::now(), budget).map_err(ControlFailure::Deadline)?;
    timeout_at(deadline, operation)
        .await
        .map_err(|_| {
            ControlFailure::Deadline(Error::ControlTimeout {
                operation: kind,
                timeout: budget,
            })
        })?
        .map_err(ControlFailure::Io)
}

fn subscription_acknowledges(
    acknowledgement: Option<WireSubscription>,
    expected: Subscription,
) -> bool {
    let wire_expected = WireSubscription {
        session_type: expected.kind.into(),
        target: expected.destination_id.into(),
    };
    acknowledgement.is_none_or(|actual| actual == wire_expected)
}

fn challenge_bytes(challenge: &[u8]) -> Result<[u8; 4], Error> {
    challenge
        .try_into()
        .map_err(|_| Error::InvalidChallengeLength {
            actual: challenge.len(),
        })
}

fn handshake_rejection(message: &[u8]) -> Error {
    let text = String::from_utf8_lossy(message).trim().to_owned();
    Error::HandshakeRejected {
        message: if text.is_empty() {
            "master sent an empty failure notice".to_owned()
        } else {
            text
        },
    }
}

fn remote_close_rejection() -> Error {
    Error::HandshakeRejected {
        message: "master closed the session".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmr_rewind_core::SessionType;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn queued_event(raw_datagram_len: usize) -> Event {
        Event {
            kind: EventKind::Report(NoticeEvent {
                metadata: EventMetadata {
                    sequence: TransportSequence::new(0),
                    flags: PacketFlags::NONE,
                    payload_len: 0,
                    raw_datagram: vec![0; raw_datagram_len],
                    received_at: SystemTime::UNIX_EPOCH,
                },
                data: Vec::new(),
            }),
        }
    }

    #[test]
    fn pending_events_enforce_event_count_without_mutating_on_rejection() {
        let mut pending = PendingEvents::default();
        for _ in 0..MAX_PENDING_EVENTS {
            assert!(
                pending.push(queued_event(0)).is_ok(),
                "every insertion through the count limit must succeed"
            );
        }

        let Err(error) = pending.push(queued_event(0)) else {
            unreachable!("one event beyond the limit must be rejected");
        };
        assert!(
            matches!(
                error,
                Error::PendingQueueOverflow {
                    events,
                    bytes: 0,
                    max_events: MAX_PENDING_EVENTS,
                    max_bytes: MAX_PENDING_BYTES,
                } if events == MAX_PENDING_EVENTS + 1
            ),
            "overflow must report the rejected count and configured limits"
        );
        assert_eq!(
            pending.len(),
            MAX_PENDING_EVENTS,
            "rejection must leave the event count unchanged"
        );
        assert_eq!(pending.bytes(), 0, "zero-byte events must not add bytes");
    }

    #[test]
    fn pending_events_enforce_exact_datagram_byte_count() {
        let mut pending = PendingEvents::default();
        let full_datagrams = MAX_PENDING_BYTES / MAX_DATAGRAM_LEN;
        let remainder = MAX_PENDING_BYTES % MAX_DATAGRAM_LEN;

        for _ in 0..full_datagrams {
            assert!(
                pending.push(queued_event(MAX_DATAGRAM_LEN)).is_ok(),
                "whole datagrams within the byte limit must succeed"
            );
        }
        if remainder > 0 {
            assert!(
                pending.push(queued_event(remainder)).is_ok(),
                "the remainder through the exact byte limit must succeed"
            );
        }
        assert_eq!(
            pending.bytes(),
            MAX_PENDING_BYTES,
            "queue accounting must reach the exact byte limit"
        );

        let event_count = pending.len();
        let Err(error) = pending.push(queued_event(1)) else {
            unreachable!("one byte beyond the limit must be rejected");
        };
        assert!(
            matches!(
                error,
                Error::PendingQueueOverflow {
                    events,
                    bytes,
                    max_events: MAX_PENDING_EVENTS,
                    max_bytes: MAX_PENDING_BYTES,
                } if events == event_count + 1 && bytes == MAX_PENDING_BYTES + 1
            ),
            "overflow must report the rejected byte and event totals"
        );
        assert_eq!(
            pending.len(),
            event_count,
            "byte-limit rejection must leave the count unchanged"
        );
        assert_eq!(
            pending.bytes(),
            MAX_PENDING_BYTES,
            "byte-limit rejection must leave byte accounting unchanged"
        );

        let Some(popped) = pending.pop() else {
            unreachable!("queue must contain a datagram");
        };
        assert_eq!(
            popped.metadata().raw_datagram.len(),
            MAX_DATAGRAM_LEN,
            "the queue must preserve FIFO event order"
        );
        assert_eq!(
            pending.bytes(),
            MAX_PENDING_BYTES - MAX_DATAGRAM_LEN,
            "popping must release the exact retained datagram bytes"
        );
    }

    #[test]
    fn subscription_ack_must_be_generic_or_match_the_outstanding_request() -> TestResult {
        let expected = Subscription::group(DestinationId::new(91)?);
        assert!(
            subscription_acknowledges(None, expected),
            "a generic acknowledgement is protocol-compatible"
        );
        assert!(
            subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::GroupVoice,
                    target: 91.into(),
                }),
                expected,
            ),
            "an exact echoed acknowledgement must be accepted"
        );
        assert!(
            !subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::PrivateVoice,
                    target: 91.into(),
                }),
                expected,
            ),
            "an acknowledgement for another session type must be ignored"
        );
        assert!(
            !subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::GroupVoice,
                    target: 92.into(),
                }),
                expected,
            ),
            "an acknowledgement for another target must be ignored"
        );
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn blocked_control_send_expires_and_retires_the_session() -> TestResult {
        let mut pending = false;
        let mut closed = false;
        let budget = Duration::from_millis(10);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            complete_control_send(
                &mut pending,
                &mut closed,
                std::future::pending(),
                18,
                budget,
                ControlOperation::Close,
            ),
        )
        .await?;
        assert!(
            matches!(result, Err(Error::ControlTimeout { operation: ControlOperation::Close, timeout }) if timeout == budget),
            "result {result:?}"
        );
        assert!(!pending, "completed timeout clears the in-flight marker");
        assert!(closed, "an unconfirmed control send ends the local session");
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_control_send_poisoned_session_cannot_resume() {
        let mut pending = false;
        let mut closed = false;
        let result = tokio::time::timeout(
            Duration::from_millis(1),
            complete_control_send(
                &mut pending,
                &mut closed,
                std::future::pending(),
                18,
                Duration::from_secs(5),
                ControlOperation::Authentication,
            ),
        )
        .await;
        assert!(
            result.is_err(),
            "the outer caller cancelled an unfinished send"
        );
        assert!(
            pending,
            "cancellation must retain the unfinished-send marker"
        );
        assert!(matches!(
            ensure_usable(pending, &mut closed),
            Err(Error::ControlSendInterrupted)
        ));
        assert!(
            closed,
            "interrupted control sends require a fresh connection"
        );
        let valid = bounded_control(
            std::future::ready(Ok(7)),
            Duration::from_secs(1),
            ControlOperation::Bind,
        )
        .await;
        assert!(matches!(valid, Ok(7)), "ordinary completion remains usable");
    }

    #[tokio::test]
    async fn failed_or_partial_control_send_cannot_resume() {
        for result in [
            Ok(17),
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, "fixture")),
        ] {
            let mut pending = false;
            let mut closed = false;
            let outcome = complete_control_send(
                &mut pending,
                &mut closed,
                std::future::ready(result),
                18,
                Duration::from_secs(1),
                ControlOperation::Keepalive,
            )
            .await;
            assert!(
                matches!(
                    outcome,
                    Err(Error::Send {
                        operation: ControlOperation::Keepalive,
                        ..
                    })
                ),
                "{outcome:?}"
            );
            assert!(!pending, "returned sends clear their in-flight marker");
            assert!(
                closed,
                "a send without a complete datagram ends the session"
            );
            assert!(matches!(
                ensure_usable(pending, &mut closed),
                Err(Error::ClientClosed)
            ));
        }
    }
}
