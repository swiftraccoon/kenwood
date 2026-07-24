// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later

//! Connected UDP driver for the receive-only Open DMR Terminal flow.

use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, SystemTime};

use dmr_rewind_core::{
    FullLinkControl, MAX_DATAGRAM_LEN, MAX_PAYLOAD_LEN, Packet, PacketFlags, PacketType, Payload,
    SERVICE_OPEN_TERMINAL, SessionType, Subscription as WireSubscription, VersionData,
    authentication_digest, decode, encode,
};
use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

/// Default maximum interval without a valid server datagram.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);
/// Default periodic Open DMR Terminal keepalive interval.
pub const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
/// Maximum operational events retained while authentication/subscription
/// recovery temporarily prevents delivery to the caller.
pub const MAX_PENDING_EVENTS: usize = 1_024;
/// Maximum exact datagram bytes retained by the handshake/recovery backlog.
pub const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_DESCRIPTION_LEN: usize = MAX_PAYLOAD_LEN - 5;
const MIN_DMR_ID: u32 = 1_000_000;
const MAX_DMR_ID: u32 = 9_999_999;
const MAX_DESTINATION_ID: u32 = 0x00ff_ffff;

/// One group- or private-voice destination requested from a master.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Subscription {
    /// Kind of DMR voice session.
    pub session_type: SessionType,
    /// Talkgroup or private DMR ID.
    pub destination_id: u32,
}

impl Subscription {
    /// Create a group-voice subscription.
    #[must_use]
    pub const fn group(destination_id: u32) -> Self {
        Self {
            session_type: SessionType::GroupVoice,
            destination_id,
        }
    }

    /// Create a private-voice subscription.
    #[must_use]
    pub const fn private(destination_id: u32) -> Self {
        Self {
            session_type: SessionType::PrivateVoice,
            destination_id,
        }
    }
}

/// Settings for one Open DMR Terminal connection.
///
/// Hostname resolution deliberately belongs to the caller. Supplying an exact
/// [`SocketAddr`] lets a supervisor choose IPv4 or IPv6 and makes reconnect
/// behavior explicit.
#[derive(Clone)]
pub struct ClientConfig {
    server: SocketAddr,
    bind: SocketAddr,
    dmr_id: u32,
    password: String,
    description: String,
    subscriptions: Vec<Subscription>,
    timeout: Duration,
    keepalive_interval: Duration,
}

impl ClientConfig {
    /// Create a configuration with protocol-default timers and no
    /// subscriptions.
    ///
    /// `dmr_id` is the operator's seven-digit `RadioID` allocation and
    /// `password` is that ID's `BrandMeister` hotspot-security password.
    #[must_use]
    pub fn new(server: SocketAddr, dmr_id: u32, password: String) -> Self {
        let bind_ip = match server.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        };
        Self {
            server,
            bind: SocketAddr::new(bind_ip, 0),
            dmr_id,
            password,
            description: format!("dmr-rewind/{}", env!("CARGO_PKG_VERSION")),
            subscriptions: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            keepalive_interval: DEFAULT_KEEPALIVE_INTERVAL,
        }
    }

    /// Set the local UDP address.
    #[must_use]
    pub const fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// Set the UTF-8 client description sent in keepalives.
    #[must_use]
    pub fn with_description(mut self, description: String) -> Self {
        self.description = description;
        self
    }

    /// Replace the group/private subscription list.
    #[must_use]
    pub fn with_subscriptions(mut self, subscriptions: Vec<Subscription>) -> Self {
        self.subscriptions = subscriptions;
        self
    }

    /// Set the maximum interval without a valid server datagram.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the periodic keepalive interval.
    #[must_use]
    pub const fn with_keepalive_interval(mut self, interval: Duration) -> Self {
        self.keepalive_interval = interval;
        self
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
    pub const fn dmr_id(&self) -> u32 {
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

    fn validate(&self) -> Result<(), Error> {
        if self.server.port() == 0 {
            return Err(Error::InvalidConfig(
                "server port must be greater than zero".to_owned(),
            ));
        }
        if !(MIN_DMR_ID..=MAX_DMR_ID).contains(&self.dmr_id) {
            return Err(Error::InvalidConfig(
                "dmr_id must be a seven-digit RadioID allocation".to_owned(),
            ));
        }
        if self.password.is_empty() {
            return Err(Error::InvalidConfig(
                "hotspot-security password must not be empty".to_owned(),
            ));
        }
        if self.description.is_empty() {
            return Err(Error::InvalidConfig(
                "client description must not be empty".to_owned(),
            ));
        }
        if self.description.len() > MAX_DESCRIPTION_LEN {
            return Err(Error::InvalidConfig(format!(
                "client description is {} bytes; maximum is {MAX_DESCRIPTION_LEN}",
                self.description.len()
            )));
        }
        if self.timeout.is_zero() {
            return Err(Error::InvalidConfig(
                "timeout must be greater than zero".to_owned(),
            ));
        }
        if self.keepalive_interval.is_zero() {
            return Err(Error::InvalidConfig(
                "keepalive interval must be greater than zero".to_owned(),
            ));
        }
        if self.timeout <= self.keepalive_interval {
            return Err(Error::InvalidConfig(
                "timeout must be greater than the keepalive interval".to_owned(),
            ));
        }
        let validation_instant = Instant::now();
        if validation_instant.checked_add(self.timeout).is_none() {
            return Err(Error::InvalidConfig(
                "timeout is too large for the runtime clock".to_owned(),
            ));
        }
        if validation_instant
            .checked_add(self.keepalive_interval)
            .is_none()
        {
            return Err(Error::InvalidConfig(
                "keepalive interval is too large for the runtime clock".to_owned(),
            ));
        }

        let mut unique = HashSet::with_capacity(self.subscriptions.len());
        for subscription in &self.subscriptions {
            if subscription.destination_id == 0 || subscription.destination_id > MAX_DESTINATION_ID
            {
                return Err(Error::InvalidConfig(format!(
                    "subscription destination {} is outside the 24-bit DMR ID range",
                    subscription.destination_id
                )));
            }
            if matches!(subscription.session_type, SessionType::Unknown(_)) {
                return Err(Error::InvalidConfig(
                    "subscription session type must be group or private voice".to_owned(),
                ));
            }
            if !unique.insert(*subscription) {
                return Err(Error::InvalidConfig(format!(
                    "duplicate {:?} subscription for {}",
                    subscription.session_type, subscription.destination_id
                )));
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
            .finish()
    }
}

/// Transport metadata shared by every received event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMetadata {
    /// REWIND transport sequence.
    pub sequence: u32,
    /// REWIND envelope flags, including unknown future bits.
    pub flags: PacketFlags,
    /// Payload length declared by the REWIND envelope.
    pub payload_len: u16,
    /// Exact UDP datagram received from the connected master.
    pub raw_datagram: Vec<u8>,
    /// Local wall-clock receipt time.
    pub received_at: SystemTime,
}

/// Exact DMR voice-header event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoiceHeaderEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// REWIND DMR-data subtype (`1` for a voice header).
    pub subtype: u8,
    /// Exact 12-byte DMR voice header with Full Link Control.
    pub data: [u8; 12],
    /// Parsed fields from `data`.
    pub link_control: FullLinkControl,
}

/// Exact DMR terminator event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminatorEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// REWIND DMR-data subtype (`2` for a terminator).
    pub subtype: u8,
    /// Exact 12-byte DMR terminator with Full Link Control, when supplied.
    ///
    /// Some Open Terminal implementations send an empty terminator.
    pub data: Option<[u8; 12]>,
    /// Parsed fields from `data`, when supplied.
    pub link_control: Option<FullLinkControl>,
}

/// Exact packed AMBE+2 audio event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// REWIND audio subtype in `0..=6`.
    pub subtype: u8,
    /// Three exact nine-byte AMBE+2 mode-33 codewords.
    pub data: [u8; 27],
}

/// Exact embedded-link-control event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddedDataEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// REWIND audio/data subtype (`7` for embedded data).
    pub subtype: u8,
    /// Exact 10-byte embedded-link-control payload.
    pub data: [u8; 10],
}

/// Parsed 32-byte call-metadata superheader event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SuperHeaderEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// Parsed source, destination, session type, and callsigns.
    pub data: dmr_rewind_core::SuperHeader,
}

/// Opaque server report, failure, or busy-notice event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NoticeEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// Exact notice payload bytes.
    pub data: Vec<u8>,
}

/// Any decoded packet without a dedicated [`Event`] variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OtherEvent {
    /// Transport and receipt metadata.
    pub metadata: EventMetadata,
    /// Known or future REWIND packet type.
    pub packet_type: PacketType,
    /// Typed or opaque decoded payload.
    pub payload: Payload,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PacketEnvelope {
    packet: Packet,
    raw_datagram: Vec<u8>,
    received_at: SystemTime,
}

/// One operational packet received from an authenticated terminal session.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// Twelve-byte DMR full-link-control voice header.
    VoiceHeader(VoiceHeaderEvent),
    /// Twelve-byte DMR full-link-control terminator.
    Terminator(TerminatorEvent),
    /// Twenty-seven-byte, three-codeword AMBE+2 burst.
    Audio(AudioEvent),
    /// Ten-byte embedded link-control payload.
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
    /// Borrow transport and receipt metadata.
    #[must_use]
    pub const fn metadata(&self) -> &EventMetadata {
        match self {
            Self::VoiceHeader(event) => &event.metadata,
            Self::Terminator(event) => &event.metadata,
            Self::Audio(event) => &event.metadata,
            Self::EmbeddedData(event) => &event.metadata,
            Self::SuperHeader(event) => &event.metadata,
            Self::Report(event) | Self::Failure(event) | Self::BusyNotice(event) => &event.metadata,
            Self::Close(metadata) => metadata,
            Self::Other(event) => &event.metadata,
        }
    }

    fn from_envelope(envelope: PacketEnvelope) -> Result<Self, Error> {
        let PacketEnvelope {
            packet,
            raw_datagram,
            received_at,
        } = envelope;
        let Packet { header, payload } = packet;
        let metadata = EventMetadata {
            sequence: header.sequence,
            flags: header.flags,
            payload_len: header.payload_len,
            raw_datagram,
            received_at,
        };
        let event = match (header.packet_type, payload) {
            (PacketType::DmrVoiceHeader, Payload::DmrVoiceHeader(link_control)) => {
                Self::VoiceHeader(VoiceHeaderEvent {
                    metadata,
                    subtype: 1,
                    data: link_control.to_bytes()?,
                    link_control,
                })
            }
            (PacketType::DmrTerminator, Payload::DmrTerminator(link_control)) => {
                let data = link_control.map(FullLinkControl::to_bytes).transpose()?;
                Self::Terminator(TerminatorEvent {
                    metadata,
                    subtype: 2,
                    data,
                    link_control,
                })
            }
            (PacketType::DmrAudio(subtype), Payload::DmrAudio(data)) => Self::Audio(AudioEvent {
                metadata,
                subtype,
                data,
            }),
            (PacketType::DmrEmbeddedData, Payload::DmrEmbeddedData(data)) => {
                Self::EmbeddedData(EmbeddedDataEvent {
                    metadata,
                    subtype: 7,
                    data,
                })
            }
            (PacketType::SuperHeader, Payload::SuperHeader(data)) => {
                Self::SuperHeader(SuperHeaderEvent { metadata, data })
            }
            (PacketType::Report, Payload::Report(data)) => {
                Self::Report(NoticeEvent { metadata, data })
            }
            (PacketType::Failure, Payload::Failure(data)) => {
                Self::Failure(NoticeEvent { metadata, data })
            }
            (PacketType::BusyNotice, Payload::Busy(data)) => {
                Self::BusyNotice(NoticeEvent { metadata, data })
            }
            (PacketType::Close, Payload::Close) => Self::Close(metadata),
            (packet_type, payload) => Self::Other(OtherEvent {
                metadata,
                packet_type,
                payload,
            }),
        };
        Ok(event)
    }
}

/// Open DMR Terminal socket, protocol, or configuration failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A setting cannot produce a valid terminal session.
    #[error("invalid Open DMR Terminal configuration: {0}")]
    InvalidConfig(String),
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
        operation: &'static str,
        /// Operating-system error.
        #[source]
        source: io::Error,
    },
    /// A UDP datagram could not be received.
    #[error("receive from Open DMR Terminal master: {0}")]
    Receive(#[source] io::Error),
    /// A datagram was malformed or an outbound packet could not be encoded.
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
    /// A bounded wait duration cannot be represented by the runtime clock.
    #[error("event wait duration {duration:?} is too large")]
    InvalidWaitDuration {
        /// Requested maximum wait.
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
    const fn next(&mut self, flags: PacketFlags) -> u32 {
        let counter = if flags.contains(PacketFlags::REAL_TIME_1) {
            &mut self.real_time_one
        } else {
            &mut self.routine
        };
        let sequence = *counter;
        *counter = counter.wrapping_add(1);
        sequence
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
pub struct Client {
    socket: UdpSocket,
    server: SocketAddr,
    dmr_id: u32,
    password: String,
    description: Vec<u8>,
    subscriptions: Vec<Subscription>,
    timeout: Duration,
    keepalive_interval: Duration,
    sequences: SequenceCounters,
    pending: PendingEvents,
    last_valid_packet: Instant,
    next_keepalive: Instant,
    receive_buffer: Vec<u8>,
    runtime_state: RuntimeState,
    closed: bool,
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
    /// Returns [`Error`] for invalid settings, socket failures, malformed
    /// packets, a rejected login, a phase timeout, or a bounded pending-event
    /// backlog overflow.
    pub async fn connect(config: ClientConfig) -> Result<Self, Error> {
        config.validate()?;
        let socket = UdpSocket::bind(config.bind)
            .await
            .map_err(|source| Error::Bind {
                address: config.bind,
                source,
            })?;
        socket
            .connect(config.server)
            .await
            .map_err(|source| Error::Connect {
                address: config.server,
                source,
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
            sequences: SequenceCounters::default(),
            pending: PendingEvents::default(),
            last_valid_packet: now,
            next_keepalive: now + config.keepalive_interval,
            receive_buffer: vec![0_u8; MAX_DATAGRAM_LEN],
            runtime_state: RuntimeState::Operational,
            closed: false,
        };

        client.send_keepalive().await?;
        let challenge = client.await_challenge().await?;
        client.finish_authentication(challenge).await?;
        client.subscribe_all().await?;
        client.next_keepalive = Instant::now() + client.keepalive_interval;
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

    /// Return whether this client has sent or received a graceful close.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
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
    pub async fn next_event(&mut self) -> Result<Event, Error> {
        loop {
            if let Some(event) = self.next_event_until(None).await? {
                return Ok(event);
            }
        }
    }

    /// Wait at most `max_wait` for an operational packet.
    ///
    /// Unlike placing an external timeout around [`Self::next_event`], this
    /// method owns its deadline and persists authentication/subscription
    /// recovery state across calls. It continues to send due keepalives and
    /// enforce the configured session timeout while waiting.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::next_event`], plus
    /// [`Error::InvalidWaitDuration`] when `max_wait` overflows the runtime
    /// clock.
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
        if matches!(self.runtime_state, RuntimeState::Operational)
            && let Some(event) = self.pending.pop()
        {
            return Ok(Some(event));
        }
        if self.closed {
            return Err(Error::ClientClosed);
        }

        loop {
            let timeout_deadline = self.last_valid_packet + self.timeout;
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
                self.next_keepalive = now + self.keepalive_interval;
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
        match envelope.packet.payload.clone() {
            Payload::KeepAlive(_) => Ok(None),
            Payload::Challenge(challenge) => {
                self.begin_runtime_authentication(challenge, None).await?;
                Ok(None)
            }
            Payload::Close => {
                self.closed = true;
                Event::from_envelope(envelope).map(Some)
            }
            _ => Event::from_envelope(envelope).map(Some),
        }
    }

    async fn handle_authentication_packet(
        &mut self,
        envelope: PacketEnvelope,
        deadline: Instant,
    ) -> Result<Option<Event>, Error> {
        let phase = "authentication acknowledgement";
        match envelope.packet.payload.clone() {
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
                Event::from_envelope(envelope).map(Some)
            }
            Payload::Failure(message) => Err(handshake_rejection(&message)),
            _ if is_operational(&envelope.packet) => {
                self.queue_pending(envelope)?;
                Ok(None)
            }
            _ => Err(Error::UnexpectedHandshakePacket {
                phase,
                packet_type: envelope.packet.header.packet_type,
            }),
        }
    }

    async fn handle_subscription_packet(
        &mut self,
        envelope: PacketEnvelope,
        acknowledged_index: usize,
    ) -> Result<Option<Event>, Error> {
        let phase = "subscription acknowledgement";
        match envelope.packet.payload.clone() {
            Payload::Subscription(acknowledgement)
                if self
                    .subscriptions
                    .get(acknowledged_index)
                    .copied()
                    .is_some_and(|expected| {
                        subscription_acknowledges(acknowledgement, expected)
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
                Event::from_envelope(envelope).map(Some)
            }
            Payload::Failure(message) => Err(handshake_rejection(&message)),
            _ if is_operational(&envelope.packet) => {
                self.queue_pending(envelope)?;
                Ok(None)
            }
            _ => Err(Error::UnexpectedHandshakePacket {
                phase,
                packet_type: envelope.packet.header.packet_type,
            }),
        }
    }

    async fn begin_runtime_authentication(
        &mut self,
        challenge: Vec<u8>,
        existing_deadline: Option<Instant>,
    ) -> Result<(), Error> {
        if challenge.len() != 4 {
            return Err(Error::InvalidChallengeLength {
                actual: challenge.len(),
            });
        }
        let deadline = existing_deadline.unwrap_or_else(|| Instant::now() + self.timeout);
        let digest = authentication_digest(&challenge, self.password.as_bytes());
        self.runtime_state = RuntimeState::Authentication { deadline };
        self.send_payload(
            PacketType::Authentication,
            Payload::Authentication(digest),
            "send authentication",
        )
        .await
    }

    async fn begin_runtime_subscriptions(&mut self) -> Result<(), Error> {
        let Some(subscription) = self.subscriptions.first().copied() else {
            self.finish_runtime_recovery();
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
            self.finish_runtime_recovery();
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
            deadline: Instant::now() + self.timeout,
            acknowledged_index: index,
        };
        self.send_subscription(subscription).await
    }

    fn finish_runtime_recovery(&mut self) {
        self.runtime_state = RuntimeState::Operational;
        self.next_keepalive = Instant::now() + self.keepalive_interval;
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
    /// Returns [`Error::Send`] if the close datagram could not be sent.
    pub async fn close(&mut self) -> Result<(), Error> {
        if self.closed {
            return Ok(());
        }
        self.send_payload(PacketType::Close, Payload::Close, "send close")
            .await?;
        self.closed = true;
        Ok(())
    }

    async fn await_challenge(&mut self) -> Result<Vec<u8>, Error> {
        let phase = "authentication challenge";
        let deadline = Instant::now() + self.timeout;
        loop {
            let envelope = self.receive_handshake(deadline, phase).await?;
            match envelope.packet.payload.clone() {
                Payload::Challenge(challenge) => return Ok(challenge),
                Payload::Close => return Err(remote_close_rejection()),
                Payload::Failure(message) => {
                    return Err(handshake_rejection(&message));
                }
                _ if is_operational(&envelope.packet) => {
                    self.queue_pending(envelope)?;
                }
                _ => {
                    return Err(Error::UnexpectedHandshakePacket {
                        phase,
                        packet_type: envelope.packet.header.packet_type,
                    });
                }
            }
        }
    }

    async fn finish_authentication(&mut self, mut challenge: Vec<u8>) -> Result<(), Error> {
        let phase = "authentication acknowledgement";
        let deadline = Instant::now() + self.timeout;
        loop {
            if challenge.len() != 4 {
                return Err(Error::InvalidChallengeLength {
                    actual: challenge.len(),
                });
            }
            let digest = authentication_digest(&challenge, self.password.as_bytes());
            self.send_payload(
                PacketType::Authentication,
                Payload::Authentication(digest),
                "send authentication",
            )
            .await?;

            loop {
                let envelope = self.receive_handshake(deadline, phase).await?;
                match envelope.packet.payload.clone() {
                    Payload::KeepAlive(_) => return Ok(()),
                    Payload::Challenge(replacement) => {
                        challenge = replacement;
                        break;
                    }
                    Payload::Close => return Err(remote_close_rejection()),
                    Payload::Failure(message) => {
                        return Err(handshake_rejection(&message));
                    }
                    _ if is_operational(&envelope.packet) => {
                        self.queue_pending(envelope)?;
                    }
                    _ => {
                        return Err(Error::UnexpectedHandshakePacket {
                            phase,
                            packet_type: envelope.packet.header.packet_type,
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
        let deadline = Instant::now() + self.timeout;
        loop {
            let envelope = self.receive_handshake(deadline, phase).await?;
            match envelope.packet.payload.clone() {
                Payload::Subscription(acknowledgement)
                    if subscription_acknowledges(acknowledgement, expected) =>
                {
                    return Ok(false);
                }
                Payload::Subscription(_) | Payload::KeepAlive(_) => {}
                Payload::Challenge(challenge) => {
                    self.finish_authentication(challenge).await?;
                    return Ok(true);
                }
                Payload::Close => return Err(remote_close_rejection()),
                Payload::Failure(message) => {
                    return Err(handshake_rejection(&message));
                }
                _ if is_operational(&envelope.packet) => {
                    self.queue_pending(envelope)?;
                }
                _ => {
                    return Err(Error::UnexpectedHandshakePacket {
                        phase,
                        packet_type: envelope.packet.header.packet_type,
                    });
                }
            }
        }
    }

    async fn send_subscription(&mut self, subscription: Subscription) -> Result<(), Error> {
        let wire = WireSubscription {
            session_type: subscription.session_type,
            target: subscription.destination_id,
        };
        self.send_payload(
            PacketType::Subscription,
            Payload::Subscription(Some(wire)),
            "send subscription",
        )
        .await
    }

    fn queue_pending(&mut self, envelope: PacketEnvelope) -> Result<(), Error> {
        self.pending.push(Event::from_envelope(envelope)?)
    }

    async fn send_keepalive(&mut self) -> Result<(), Error> {
        let description = self.description.clone();
        self.send_payload(
            PacketType::KeepAlive,
            Payload::KeepAlive(Some(VersionData {
                remote_id: self.dmr_id,
                service: SERVICE_OPEN_TERMINAL,
                description,
            })),
            "send keepalive",
        )
        .await
    }

    async fn send_payload(
        &mut self,
        packet_type: PacketType,
        payload: Payload,
        operation: &'static str,
    ) -> Result<(), Error> {
        let flags = PacketFlags::NONE;
        let packet = Packet::new(packet_type, flags, self.sequences.next(flags), payload)?;
        let datagram = encode(&packet)?;
        let sent = self
            .socket
            .send(&datagram)
            .await
            .map_err(|source| Error::Send { operation, source })?;
        if sent != datagram.len() {
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
        let packet = decode(&datagram)?;
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
        packet.header.packet_type,
        PacketType::KeepAlive
            | PacketType::Close
            | PacketType::Challenge
            | PacketType::Authentication
            | PacketType::Configuration
            | PacketType::Subscription
            | PacketType::Cancelling
    )
}

fn subscription_acknowledges(
    acknowledgement: Option<WireSubscription>,
    expected: Subscription,
) -> bool {
    let wire_expected = WireSubscription {
        session_type: expected.session_type,
        target: expected.destination_id,
    };
    acknowledgement.is_none_or(|actual| actual == wire_expected)
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

    fn queued_event(raw_datagram_len: usize) -> Event {
        Event::Report(NoticeEvent {
            metadata: EventMetadata {
                sequence: 0,
                flags: PacketFlags::NONE,
                payload_len: 0,
                raw_datagram: vec![0; raw_datagram_len],
                received_at: SystemTime::UNIX_EPOCH,
            },
            data: Vec::new(),
        })
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
    fn subscription_ack_must_be_generic_or_match_the_outstanding_request() {
        let expected = Subscription::group(91);
        assert!(
            subscription_acknowledges(None, expected),
            "a generic acknowledgement is protocol-compatible"
        );
        assert!(
            subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::GroupVoice,
                    target: 91,
                }),
                expected,
            ),
            "an exact echoed acknowledgement must be accepted"
        );
        assert!(
            !subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::PrivateVoice,
                    target: 91,
                }),
                expected,
            ),
            "an acknowledgement for another session type must be ignored"
        );
        assert!(
            !subscription_acknowledges(
                Some(WireSubscription {
                    session_type: SessionType::GroupVoice,
                    target: 92,
                }),
                expected,
            ),
            "an acknowledgement for another target must be ignored"
        );
    }
}
