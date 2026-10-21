//! Protocol-erased server-side state machine.
//!
//! [`ServerSessionCore`] is the runtime-erased state machine that
//! drives a single server-side client session. It handles one client
//! at a time; the server's fan-out engine in `dstar-gateway-server`
//! spawns a `ServerSessionCore` per inbound peer and routes datagrams
//! through [`ServerSessionCore::handle_input`].
//!
//! **Supported protocols: `DExtra`, `DPlus`, `DCS`.** All three
//! protocols drive through the same `handle_input` dispatcher; the
//! protocol-specific handshakes are implemented as private helpers
//! (`handle_dextra_input`, `handle_dplus_input`, `handle_dcs_input`).
//!
//! The server core does NOT authenticate clients; that's the
//! `dstar-gateway-server` shell's `ClientAuthorizer` job. The core
//! only manages wire decoding + state transitions + event emission.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Instant;

use crate::codec::dcs::{
    self as dcs_codec, ClientPacket as DcsClientPacket,
    decode_client_to_server as decode_dcs_client_to_server,
};
use crate::codec::dextra::{
    ClientPacket, decode_client_to_server, encode_connect_ack, encode_poll_echo,
};
use crate::codec::dplus::{
    ClientPacket as DPlusClientPacket, Link2Result,
    decode_client_to_server as decode_dplus_client_to_server, encode_link1_ack, encode_link2_reply,
    encode_poll_echo as dplus_encode_poll_echo, encode_unlink_ack,
};
use crate::error::{Error, ProtocolError};
use crate::header::DstarHeader;
use crate::session::client::Protocol;
use crate::session::driver::Transmit;
use crate::session::outbox::{OutboundPacket, Outbox};
use crate::types::{
    Callsign, Module, ModuleSlot, ProtocolKind, RepeaterIdentity, RoutingCallsign, StreamId,
};
use crate::validator::VecSink;
use crate::voice::VoiceFrame;

use super::event::ServerEvent;
use super::state::ServerStateKind;

/// Internal state for the server session machine.
///
/// Kept as a private enum rather than reusing [`ServerStateKind`]
/// so private-only transitional states can be added without leaking
/// them through the public runtime discriminator. [`Link1Received`]
/// is a DPlus-specific transient state between LINK1 and LINK2 that
/// the public view collapses into `Unknown` via [`Self::kind`].
///
/// [`Link1Received`]: Self::Link1Received
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalState {
    Unknown,
    /// `DPlus`-specific: LINK1 seen and acknowledged, waiting for LINK2.
    Link1Received,
    Linked,
    Streaming,
    Unlinking,
    Closed,
}

impl InternalState {
    const fn kind(self) -> ServerStateKind {
        match self {
            // Link1Received is a DPlus-private transitional state;
            // the public view sees "not linked yet", same as Unknown.
            Self::Unknown | Self::Link1Received => ServerStateKind::Unknown,
            Self::Linked => ServerStateKind::Linked,
            Self::Streaming => ServerStateKind::Streaming,
            Self::Unlinking => ServerStateKind::Unlinking,
            Self::Closed => ServerStateKind::Closed,
        }
    }
}

/// Internal protocol-erased server event record.
///
/// [`ServerSessionCore::pop_event`] is generic over `P: Protocol` and
/// converts each `RawServerEvent` into a [`ServerEvent<P>`] at drain
/// time; the queue itself is protocol-erased.
#[derive(Debug, Clone)]
enum RawServerEvent {
    Linked {
        peer: SocketAddr,
        callsign: Callsign,
        module: Module,
    },
    Unlinked {
        peer: SocketAddr,
    },
    StreamStarted {
        peer: SocketAddr,
        stream_id: StreamId,
        header: DstarHeader,
    },
    StreamFrame {
        peer: SocketAddr,
        stream_id: StreamId,
        seq: u8,
        frame: VoiceFrame,
    },
    StreamEnded {
        peer: SocketAddr,
        stream_id: StreamId,
    },
}

/// Protocol-erased server-side session machine.
///
/// Each instance tracks one client. The `dstar-gateway-server` shell
/// spawns a `ServerSessionCore` per inbound peer and routes datagrams
/// through [`Self::handle_input`]. A linked peer keeps its callsign and
/// module until disconnect. During a voice stream, only its active stream
/// identifier can deliver frames or end the stream.
pub struct ServerSessionCore {
    /// Which protocol this client speaks.
    kind: ProtocolKind,
    /// Client peer address.
    peer: SocketAddr,
    /// Reflector module selected by the endpoint.
    reflector_module: Module,
    /// Runtime state.
    state: InternalState,
    /// Callsign of the linked client (populated once LINK arrives).
    client_callsign: Option<Callsign>,
    /// Local module letter of the linked client.
    client_module: Option<Module>,
    /// Active stream identifier, cleared by its EOT or disconnect.
    last_stream_id: Option<StreamId>,
    /// Outbound packet queue.
    outbox: Outbox,
    /// Queued raw events awaiting [`Self::pop_event`] drain.
    events: VecDeque<RawServerEvent>,
    /// Diagnostic sink for lenient parser warnings.
    diagnostics: VecSink,
    /// Most-recently-popped outbound packet, held so
    /// [`Self::pop_transmit`] can return a borrow into the owned
    /// payload across multiple calls.
    current_tx: Option<OutboundPacket>,
}

impl std::fmt::Debug for ServerSessionCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerSessionCore")
            .field("kind", &self.kind)
            .field("peer", &self.peer)
            .field("reflector_module", &self.reflector_module)
            .field("state", &self.state)
            .field("client_callsign", &self.client_callsign)
            .field("client_module", &self.client_module)
            .field("last_stream_id", &self.last_stream_id)
            .field("outbox", &self.outbox)
            .field("events", &self.events)
            .field("diagnostics", &self.diagnostics)
            .field("current_tx", &self.current_tx)
            .finish()
    }
}

impl ServerSessionCore {
    /// Create a new server session for a client with the given
    /// protocol, peer, and reflector module.
    ///
    /// `reflector_module` is the module this reflector endpoint is
    /// bound to. `DExtra` and `DCS` LINK requests for another module
    /// are ignored.
    #[must_use]
    pub fn new(kind: ProtocolKind, peer: SocketAddr, reflector_module: Module) -> Self {
        Self {
            kind,
            peer,
            reflector_module,
            state: InternalState::Unknown,
            client_callsign: None,
            client_module: None,
            last_stream_id: None,
            outbox: Outbox::new(),
            events: VecDeque::new(),
            diagnostics: VecSink::default(),
            current_tx: None,
        }
    }

    /// Runtime state discriminator.
    #[must_use]
    pub const fn state_kind(&self) -> ServerStateKind {
        self.state.kind()
    }

    /// Peer address of this client.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Runtime protocol discriminator.
    #[must_use]
    pub const fn protocol_kind(&self) -> ProtocolKind {
        self.kind
    }

    /// Reflector module for this session.
    #[must_use]
    pub const fn reflector_module(&self) -> Module {
        self.reflector_module
    }

    /// Callsign of the linked client, if any.
    #[must_use]
    pub const fn client_callsign(&self) -> Option<Callsign> {
        self.client_callsign
    }

    /// Local module letter of the linked client, if any.
    #[must_use]
    pub const fn client_module(&self) -> Option<Module> {
        self.client_module
    }

    /// Feed an inbound datagram into the server session.
    ///
    /// Parses the bytes, updates state, pushes events and outbound
    /// packets as needed. Protocol-erased dispatch: matches on
    /// [`Self::protocol_kind`] and calls the appropriate decoder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] wrapping the codec error if the
    /// datagram cannot be parsed.
    pub fn handle_input(&mut self, now: Instant, bytes: &[u8]) -> Result<(), Error> {
        if self.state == InternalState::Closed {
            return Ok(());
        }
        let result = match self.kind {
            ProtocolKind::DExtra => self.handle_dextra_input(now, bytes),
            ProtocolKind::DPlus => self.handle_dplus_input(now, bytes),
            ProtocolKind::Dcs => self.handle_dcs_input(now, bytes),
        };
        if matches!(
            &result,
            Err(Error::Protocol(
                ProtocolError::DExtra(
                    crate::codec::dextra::DExtraError::UnknownPacketLength { .. }
                ) | ProtocolError::DPlus(
                    crate::codec::dplus::DPlusError::UnknownPacketLength { .. }
                ) | ProtocolError::Dcs(crate::codec::dcs::DcsError::UnknownPacketLength { .. })
            ))
        ) {
            tracing::debug!(protocol = ?self.kind, length = bytes.len(), "ignored unknown server datagram length");
            return Ok(());
        }
        result
    }

    fn handle_dextra_input(&mut self, now: Instant, bytes: &[u8]) -> Result<(), Error> {
        let packet = decode_client_to_server(bytes, &mut self.diagnostics)
            .map_err(|e| Error::Protocol(ProtocolError::DExtra(e)))?;
        match packet {
            ClientPacket::Link {
                callsign,
                reflector_module,
                client_module,
            } => self.on_dextra_link(now, callsign, reflector_module, client_module),
            ClientPacket::Unlink {
                callsign,
                client_module,
            } => {
                self.on_dextra_unlink(callsign, client_module);
                Ok(())
            }
            ClientPacket::Poll { callsign } => self.on_dextra_poll(now, callsign),
            ClientPacket::VoiceHeader { stream_id, header } => {
                self.on_dextra_voice_header(stream_id, header);
                Ok(())
            }
            ClientPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.on_dextra_voice_data(stream_id, seq, frame);
                Ok(())
            }
            ClientPacket::VoiceEot { stream_id, .. } => {
                self.on_dextra_voice_eot(stream_id);
                Ok(())
            }
        }
    }

    fn on_dextra_link(
        &mut self,
        now: Instant,
        callsign: Callsign,
        reflector_module: Module,
        client_module: Module,
    ) -> Result<(), Error> {
        if !matches!(
            self.state,
            InternalState::Unknown | InternalState::Linked | InternalState::Streaming
        ) || reflector_module != self.reflector_module
            || self
                .client_callsign
                .is_some_and(|existing| existing != callsign)
            || self
                .client_module
                .is_some_and(|existing| existing != client_module)
        {
            return Ok(());
        }
        let identity = RepeaterIdentity {
            callsign: RoutingCallsign::try_from(callsign)?,
            module: client_module,
        };
        self.client_callsign = Some(callsign);
        self.client_module = Some(client_module);
        if self.state != InternalState::Streaming {
            self.state = InternalState::Linked;
        }
        self.events.push_back(RawServerEvent::Linked {
            peer: self.peer,
            callsign,
            module: reflector_module,
        });
        // Enqueue a 14-byte ACK to send back.
        let mut buf = [0u8; 32];
        let n = encode_connect_ack(&mut buf, identity, reflector_module)
            .map_err(|e| Error::Protocol(ProtocolError::DExtra(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dextra_unlink(&mut self, callsign: Callsign, client_module: Module) {
        if self.client_callsign != Some(callsign) || self.client_module != Some(client_module) {
            return;
        }
        self.state = InternalState::Unlinking;
        self.events
            .push_back(RawServerEvent::Unlinked { peer: self.peer });
        // Revision-zero DExtra UNLINK closes without a reply.
        self.state = InternalState::Closed;
        self.last_stream_id = None;
    }

    fn on_dextra_poll(&mut self, now: Instant, callsign: Callsign) -> Result<(), Error> {
        if self.client_callsign != Some(callsign) {
            return Ok(());
        }
        // Echo the poll back.
        let mut buf = [0u8; 16];
        let n = encode_poll_echo(&mut buf, &callsign)
            .map_err(|e| Error::Protocol(ProtocolError::DExtra(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dextra_voice_header(&mut self, stream_id: StreamId, header: DstarHeader) {
        if self.state != InternalState::Linked {
            return;
        }
        self.state = InternalState::Streaming;
        self.last_stream_id = Some(stream_id);
        self.events.push_back(RawServerEvent::StreamStarted {
            peer: self.peer,
            stream_id,
            header,
        });
    }

    fn on_dextra_voice_data(&mut self, stream_id: StreamId, seq: u8, frame: VoiceFrame) {
        if self.state != InternalState::Streaming || self.last_stream_id != Some(stream_id) {
            return;
        }
        self.events.push_back(RawServerEvent::StreamFrame {
            peer: self.peer,
            stream_id,
            seq,
            frame,
        });
    }

    fn on_dextra_voice_eot(&mut self, stream_id: StreamId) {
        if self.state != InternalState::Streaming || self.last_stream_id != Some(stream_id) {
            return;
        }
        self.state = InternalState::Linked;
        self.last_stream_id = None;
        self.events.push_back(RawServerEvent::StreamEnded {
            peer: self.peer,
            stream_id,
        });
    }

    // ─── DPlus server handshake ───────────────────────────────────

    fn handle_dplus_input(&mut self, now: Instant, bytes: &[u8]) -> Result<(), Error> {
        let packet = decode_dplus_client_to_server(bytes, &mut self.diagnostics)
            .map_err(|e| Error::Protocol(ProtocolError::DPlus(e)))?;
        match packet {
            DPlusClientPacket::Link1 => self.on_dplus_link1(now),
            DPlusClientPacket::Link2 { callsign } => self.on_dplus_link2(now, callsign),
            DPlusClientPacket::Unlink => self.on_dplus_unlink(now),
            DPlusClientPacket::Poll => self.on_dplus_poll(now),
            DPlusClientPacket::VoiceHeader { stream_id, header } => {
                self.on_dplus_voice_header(stream_id, header);
                Ok(())
            }
            DPlusClientPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.on_dplus_voice_data(stream_id, seq, frame);
                Ok(())
            }
            DPlusClientPacket::VoiceEot { stream_id, .. } => {
                self.on_dplus_voice_eot(stream_id);
                Ok(())
            }
        }
    }

    fn on_dplus_link1(&mut self, now: Instant) -> Result<(), Error> {
        match self.state {
            InternalState::Unknown => {
                self.state = InternalState::Link1Received;
            }
            InternalState::Link1Received | InternalState::Linked => {
                // Real clients retransmit LINK1, so just re-enqueue the
                // ACK idempotently. If we're already Linked the client
                // is badly lagged; echoing LINK1 is still the safest
                // response.
            }
            InternalState::Streaming | InternalState::Unlinking | InternalState::Closed => {
                // Ignore: can't drop back to LINK1 handshake.
                return Ok(());
            }
        }
        // Enqueue a 5-byte LINK1 ACK echo.
        let mut buf = [0u8; 8];
        let n = encode_link1_ack(&mut buf)
            .map_err(|e| Error::Protocol(ProtocolError::DPlus(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dplus_link2(&mut self, now: Instant, callsign: Callsign) -> Result<(), Error> {
        match self.state {
            InternalState::Link1Received => {
                self.client_callsign = Some(callsign);
                self.client_module = Some(self.reflector_module);
                self.state = InternalState::Linked;
                self.events.push_back(RawServerEvent::Linked {
                    peer: self.peer,
                    callsign,
                    module: self.reflector_module,
                });
                // Enqueue an 8-byte OKRW reply.
                self.enqueue_dplus_link2_reply(now, Link2Result::Accept)?;
                Ok(())
            }
            InternalState::Linked | InternalState::Streaming => {
                // Already linked. If it's the same callsign, idempotent
                // re-ACK. If it's a different callsign, return BUSY.
                if self.client_callsign == Some(callsign) {
                    self.enqueue_dplus_link2_reply(now, Link2Result::Accept)?;
                } else {
                    self.enqueue_dplus_link2_reply(now, Link2Result::Busy)?;
                }
                Ok(())
            }
            InternalState::Unknown | InternalState::Unlinking | InternalState::Closed => {
                // LINK2 without LINK1: drop silently. The real client
                // will retransmit LINK1 on its own retry timer. Safest
                // path: do nothing.
                Ok(())
            }
        }
    }

    fn enqueue_dplus_link2_reply(
        &mut self,
        now: Instant,
        result: Link2Result,
    ) -> Result<(), Error> {
        let mut buf = [0u8; 16];
        let n = encode_link2_reply(&mut buf, result)
            .map_err(|e| Error::Protocol(ProtocolError::DPlus(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dplus_unlink(&mut self, now: Instant) -> Result<(), Error> {
        if !matches!(
            self.state,
            InternalState::Linked | InternalState::Streaming | InternalState::Link1Received
        ) {
            return Ok(());
        }
        // Enqueue the 5-byte UNLINK ACK echo.
        let mut buf = [0u8; 8];
        let n = encode_unlink_ack(&mut buf)
            .map_err(|e| Error::Protocol(ProtocolError::DPlus(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        self.events
            .push_back(RawServerEvent::Unlinked { peer: self.peer });
        self.state = InternalState::Closed;
        self.last_stream_id = None;
        Ok(())
    }

    fn on_dplus_poll(&mut self, now: Instant) -> Result<(), Error> {
        if !matches!(self.state, InternalState::Linked | InternalState::Streaming) {
            return Ok(());
        }
        let mut buf = [0u8; 8];
        let n = dplus_encode_poll_echo(&mut buf)
            .map_err(|e| Error::Protocol(ProtocolError::DPlus(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dplus_voice_header(&mut self, stream_id: StreamId, header: DstarHeader) {
        if self.state != InternalState::Linked {
            return;
        }
        self.state = InternalState::Streaming;
        self.last_stream_id = Some(stream_id);
        self.events.push_back(RawServerEvent::StreamStarted {
            peer: self.peer,
            stream_id,
            header,
        });
    }

    fn on_dplus_voice_data(&mut self, stream_id: StreamId, seq: u8, frame: VoiceFrame) {
        if self.state != InternalState::Streaming || self.last_stream_id != Some(stream_id) {
            return;
        }
        self.events.push_back(RawServerEvent::StreamFrame {
            peer: self.peer,
            stream_id,
            seq,
            frame,
        });
    }

    fn on_dplus_voice_eot(&mut self, stream_id: StreamId) {
        if self.state != InternalState::Streaming || self.last_stream_id != Some(stream_id) {
            return;
        }
        self.state = InternalState::Linked;
        self.last_stream_id = None;
        self.events.push_back(RawServerEvent::StreamEnded {
            peer: self.peer,
            stream_id,
        });
    }

    // ─── DCS server handshake ─────────────────────────────────────

    fn handle_dcs_input(&mut self, now: Instant, bytes: &[u8]) -> Result<(), Error> {
        let packet = decode_dcs_client_to_server(bytes, &mut self.diagnostics)
            .map_err(|e| Error::Protocol(ProtocolError::Dcs(e)))?;
        match packet {
            DcsClientPacket::Link {
                callsign,
                client_module,
                reflector_module,
                ..
            } => self.on_dcs_link(now, callsign, client_module, reflector_module),
            DcsClientPacket::Unlink {
                callsign,
                client_module,
                ..
            } => self.on_dcs_unlink(now, callsign, client_module),
            DcsClientPacket::Poll {
                callsign,
                reflector_callsign,
            } => self.on_dcs_poll(now, callsign, reflector_callsign),
            DcsClientPacket::Voice {
                header,
                stream_id,
                seq,
                frame,
                is_end,
            } => {
                self.on_dcs_voice(header, stream_id, seq, frame, is_end);
                Ok(())
            }
        }
    }

    fn on_dcs_link(
        &mut self,
        now: Instant,
        callsign: Callsign,
        client_module: Module,
        reflector_module: Module,
    ) -> Result<(), Error> {
        if !matches!(
            self.state,
            InternalState::Unknown | InternalState::Linked | InternalState::Streaming
        ) || reflector_module != self.reflector_module
            || self
                .client_callsign
                .is_some_and(|existing| existing != callsign)
            || self
                .client_module
                .is_some_and(|existing| existing != client_module)
        {
            return Ok(());
        }
        let identity = RepeaterIdentity {
            callsign: RoutingCallsign::try_from(callsign)?,
            module: client_module,
        };
        self.client_callsign = Some(callsign);
        self.client_module = Some(client_module);
        if self.state != InternalState::Streaming {
            self.state = InternalState::Linked;
        }
        self.events.push_back(RawServerEvent::Linked {
            peer: self.peer,
            callsign,
            module: reflector_module,
        });
        let mut buf = [0u8; 32];
        let n = dcs_codec::encode_connect_ack(&mut buf, identity, reflector_module)
            .map_err(|e| Error::Protocol(ProtocolError::Dcs(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        // DCS ACK is enqueued without a rate-limit delay; the caller
        // drives `now` through `pop_transmit`.
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dcs_unlink(
        &mut self,
        now: Instant,
        callsign: Callsign,
        client_module: Module,
    ) -> Result<(), Error> {
        if self.client_callsign != Some(callsign) || self.client_module != Some(client_module) {
            return Ok(());
        }
        let identity = RepeaterIdentity {
            callsign: RoutingCallsign::try_from(callsign)?,
            module: client_module,
        };
        let mut bytes = [0_u8; 14];
        let length = dcs_codec::encode_connect_nak(&mut bytes, identity, ModuleSlot::Empty)
            .map_err(|error| Error::Protocol(ProtocolError::Dcs(error.into())))?;
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload: bytes.get(..length).unwrap_or(&[]).to_vec(),
            not_before: now,
        });
        self.events
            .push_back(RawServerEvent::Unlinked { peer: self.peer });
        self.state = InternalState::Closed;
        self.last_stream_id = None;
        Ok(())
    }

    fn on_dcs_poll(
        &mut self,
        now: Instant,
        callsign: Callsign,
        reflector_callsign: Callsign,
    ) -> Result<(), Error> {
        if !matches!(self.state, InternalState::Linked | InternalState::Streaming) {
            return Ok(());
        }
        if self.client_callsign != Some(callsign) {
            return Ok(());
        }
        let mut buf = [0u8; 32];
        let n = dcs_codec::encode_poll_reply(&mut buf, &callsign, &reflector_callsign)
            .map_err(|e| Error::Protocol(ProtocolError::Dcs(e.into())))?;
        let payload = buf.get(..n).unwrap_or(&[]).to_vec();
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload,
            not_before: now,
        });
        Ok(())
    }

    fn on_dcs_voice(
        &mut self,
        header: DstarHeader,
        stream_id: StreamId,
        seq: u8,
        frame: VoiceFrame,
        is_end: bool,
    ) {
        if !matches!(self.state, InternalState::Linked | InternalState::Streaming) {
            return;
        }
        if self
            .last_stream_id
            .is_some_and(|active| active != stream_id)
        {
            return;
        }
        // DCS embeds the header in every voice packet, so detect
        // stream-start by observing a new stream id.
        let is_new_stream = self.last_stream_id != Some(stream_id);
        if is_new_stream {
            self.state = InternalState::Streaming;
            self.last_stream_id = Some(stream_id);
            self.events.push_back(RawServerEvent::StreamStarted {
                peer: self.peer,
                stream_id,
                header,
            });
        }
        self.events.push_back(RawServerEvent::StreamFrame {
            peer: self.peer,
            stream_id,
            seq,
            frame,
        });
        if is_end {
            self.state = InternalState::Linked;
            self.last_stream_id = None;
            self.events.push_back(RawServerEvent::StreamEnded {
                peer: self.peer,
                stream_id,
            });
        }
    }

    /// Pop the next outbound packet (from the outbox).
    ///
    /// Holds the popped packet in `current_tx` so the returned
    /// [`Transmit`] can borrow from it across calls.
    #[must_use]
    pub fn pop_transmit(&mut self, now: Instant) -> Option<Transmit<'_>> {
        let next = self.outbox.pop_ready(now)?;
        self.current_tx = Some(next);
        let held = self.current_tx.as_ref()?;
        Some(Transmit::new(held.dst, held.payload.as_slice()))
    }

    /// Drain the next event, typed with the correct protocol marker.
    ///
    /// The `P` type parameter re-attaches the protocol phantom at
    /// drain time; the event queue itself is protocol-erased. Returns `None`
    /// without draining when `P` does not match [`Self::protocol_kind`].
    pub fn pop_event<P: Protocol>(&mut self) -> Option<ServerEvent<P>> {
        if P::KIND != self.kind {
            return None;
        }
        let raw = self.events.pop_front()?;
        Some(match raw {
            RawServerEvent::Linked {
                peer,
                callsign,
                module,
            } => ServerEvent::ClientLinked {
                peer,
                callsign,
                module,
            },
            RawServerEvent::Unlinked { peer } => ServerEvent::ClientUnlinked { peer },
            RawServerEvent::StreamStarted {
                peer,
                stream_id,
                header,
            } => ServerEvent::ClientStreamStarted {
                peer,
                stream_id,
                header,
            },
            RawServerEvent::StreamFrame {
                peer,
                stream_id,
                seq,
                frame,
            } => ServerEvent::ClientStreamFrame {
                peer,
                stream_id,
                seq,
                frame,
            },
            RawServerEvent::StreamEnded { peer, stream_id } => {
                ServerEvent::ClientStreamEnded { peer, stream_id }
            }
        })
    }

    /// Earliest time the loop needs to re-enter.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.outbox.peek_next_deadline()
    }

    /// Drain accumulated parser diagnostics.
    pub fn drain_diagnostics(&mut self) -> Vec<crate::validator::Diagnostic> {
        self.diagnostics.drain().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{ProtocolKind, ServerEvent, ServerSessionCore, ServerStateKind};
    use crate::codec::dcs::{
        encode_connect_link as dcs_encode_link, encode_connect_unlink as dcs_encode_unlink,
        encode_poll_request as dcs_encode_poll, encode_voice as dcs_encode_voice,
    };
    use crate::codec::dextra::{encode_connect_link, encode_poll, encode_unlink};
    use crate::codec::dplus::{
        encode_link1, encode_link2, encode_poll as dplus_encode_poll,
        encode_unlink as dplus_encode_unlink,
    };
    use crate::header::DstarHeader;
    use crate::session::client::{DExtra, DPlus, Dcs};
    use crate::types::{
        Callsign, FrameSequence, Module, RepeaterIdentity, RoutingCallsign, StreamId, Suffix,
    };
    use crate::voice::VoiceFrame;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Instant;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const DPLUS_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const DCS_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30051);
    const LOCAL: RepeaterIdentity = RepeaterIdentity {
        callsign: match RoutingCallsign::from_padded_bytes(*b"W1AW   ") {
            Ok(value) => value,
            Err(_) => unreachable!(),
        },
        module: Module::B,
    };
    const DCS_TARGET: crate::DcsReflector = match crate::DcsReflector::try_from_routing(
        match RoutingCallsign::from_padded_bytes(*b"DCS030 ") {
            Ok(value) => value,
            Err(_) => unreachable!(),
        },
    ) {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };

    const fn cs(bytes: [u8; 8]) -> Callsign {
        Callsign::from_wire_bytes(bytes)
    }

    const fn test_header(my: [u8; 8]) -> DstarHeader {
        DstarHeader {
            flags: crate::HeaderFlags::NONE,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(my),
            my_suffix: Suffix::EMPTY,
        }
    }

    #[expect(
        clippy::unwrap_used,
        reason = "test helper: every call site passes a non-zero literal, so the \
                  unwrap cannot fire; a zero would panic only this test, at runtime"
    )]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    fn linked_server<P: crate::Protocol>() -> Result<ServerSessionCore, Box<dyn std::error::Error>>
    {
        let mut core = ServerSessionCore::new(P::KIND, PEER, Module::C);
        let mut bytes = [0_u8; 600];
        let length = match P::KIND {
            ProtocolKind::DExtra => encode_connect_link(&mut bytes, LOCAL, Module::C)?,
            ProtocolKind::DPlus => {
                let length = encode_link1(&mut bytes)?;
                core.handle_input(Instant::now(), bytes.get(..length).ok_or("LINK1 length")?)?;
                encode_link2(&mut bytes, &cs(*b"W1AW    "))?
            }
            ProtocolKind::Dcs => dcs_encode_link(
                &mut bytes,
                LOCAL,
                Module::C,
                DCS_TARGET,
                crate::codec::dcs::GatewayType::Repeater,
            )?,
        };
        core.handle_input(Instant::now(), bytes.get(..length).ok_or("LINK length")?)?;
        assert!(
            matches!(
                core.pop_event::<P>(),
                Some(ServerEvent::ClientLinked { .. })
            ),
            "complete protocol handshake must publish a linked client"
        );
        Ok(core)
    }

    fn stream_id_guard<P: crate::Protocol>() -> TestResult {
        let mut core = linked_server::<P>()?;
        let active = sid(0x1234);
        let unrelated = sid(0x5678);
        let header = test_header(*b"W1AW    ");
        let frame = VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        };
        match P::KIND {
            ProtocolKind::DExtra => core.on_dextra_voice_header(active, header),
            ProtocolKind::DPlus => core.on_dplus_voice_header(active, header),
            ProtocolKind::Dcs => core.on_dcs_voice(header, active, 0, frame, false),
        }
        while core.pop_event::<P>().is_some() {}
        match P::KIND {
            ProtocolKind::DExtra => {
                core.on_dextra_voice_data(unrelated, 1, frame);
                core.on_dextra_voice_eot(unrelated);
            }
            ProtocolKind::DPlus => {
                core.on_dplus_voice_data(unrelated, 1, frame);
                core.on_dplus_voice_eot(unrelated);
            }
            ProtocolKind::Dcs => core.on_dcs_voice(header, unrelated, 1, frame, true),
        }
        assert_eq!(
            core.state_kind(),
            ServerStateKind::Streaming,
            "an unrelated stream cannot end the active stream"
        );
        assert!(
            core.pop_event::<P>().is_none(),
            "unrelated voice must never reach fan-out events"
        );
        match P::KIND {
            ProtocolKind::DExtra => core.on_dextra_voice_eot(active),
            ProtocolKind::DPlus => core.on_dplus_voice_eot(active),
            ProtocolKind::Dcs => core.on_dcs_voice(header, active, 2, frame, true),
        }
        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        let mut ended = false;
        while let Some(event) = core.pop_event::<P>() {
            if matches!(event, ServerEvent::ClientStreamEnded { stream_id, .. } if stream_id == active)
            {
                ended = true;
            }
        }
        assert!(ended, "only the active EOT publishes completion");
        Ok(())
    }

    #[test]
    fn every_server_protocol_preserves_the_active_stream() -> TestResult {
        stream_id_guard::<DExtra>()?;
        stream_id_guard::<DPlus>()?;
        stream_id_guard::<Dcs>()
    }

    #[test]
    fn linked_identity_cannot_be_reassigned_and_closed_input_is_inert() -> TestResult {
        let now = Instant::now();
        let mut dextra = linked_server::<DExtra>()?;
        dextra.on_dextra_link(now, cs(*b"N0CALL  "), Module::C, Module::B)?;
        dextra.on_dextra_link(now, cs(*b"W1AW    "), Module::C, Module::A)?;
        dextra.on_dextra_link(now, cs(*b"W1AW    "), Module::D, Module::B)?;
        assert_eq!(dextra.client_callsign(), Some(cs(*b"W1AW    ")));
        assert_eq!(dextra.client_module(), Some(Module::B));
        assert!(dextra.pop_event::<DExtra>().is_none());
        let mut dcs = linked_server::<Dcs>()?;
        dcs.on_dcs_link(now, cs(*b"N0CALL  "), Module::B, Module::C)?;
        dcs.on_dcs_link(now, cs(*b"W1AW    "), Module::A, Module::C)?;
        dcs.on_dcs_link(now, cs(*b"W1AW    "), Module::B, Module::D)?;
        assert_eq!(dcs.client_callsign(), Some(cs(*b"W1AW    ")));
        assert_eq!(dcs.client_module(), Some(Module::B));
        assert!(dcs.pop_event::<Dcs>().is_none());
        dextra.on_dextra_unlink(cs(*b"W1AW    "), Module::B);
        let _unlinked = dextra.pop_event::<DExtra>();
        dextra.handle_input(now, &[0xFF; 10])?;
        assert_eq!(dextra.state_kind(), ServerStateKind::Closed);
        assert!(dextra.pop_event::<DExtra>().is_none());
        Ok(())
    }

    #[test]
    fn unknown_datagram_lengths_leave_server_sessions_usable() -> TestResult {
        let now = Instant::now();
        for protocol in [ProtocolKind::DExtra, ProtocolKind::DPlus, ProtocolKind::Dcs] {
            let mut core = ServerSessionCore::new(protocol, PEER, Module::C);
            core.handle_input(now, &[0xEE; 10])?;
            assert_eq!(core.state_kind(), ServerStateKind::Unknown);
        }
        Ok(())
    }

    #[test]
    fn control_replies_echo_the_linked_client_module_and_match_unlink_identity() -> TestResult {
        for mut core in [linked_server::<DExtra>()?, linked_server::<Dcs>()?] {
            let now = Instant::now();
            let ack = core.pop_transmit(now).ok_or("LINK acknowledgment")?;
            assert_eq!(
                ack.payload, b"W1AW    BCACK\0",
                "independent reply vector retains client B separately from target C"
            );
            let (wrong, matching): (&[u8], &[u8]) = match core.protocol_kind() {
                ProtocolKind::DExtra => (b"W1AW    A \0", b"W1AW    B \0"),
                ProtocolKind::Dcs => (b"W1AW    A \0DCS030  ", b"W1AW    B \0DCS030  "),
                ProtocolKind::DPlus => unreachable!(),
            };
            core.handle_input(now, wrong)?;
            assert_eq!(
                core.state_kind(),
                ServerStateKind::Linked,
                "another client module cannot unlink the stored identity"
            );
            core.handle_input(now, matching)?;
            assert_eq!(core.state_kind(), ServerStateKind::Closed);
            if core.protocol_kind() == ProtocolKind::Dcs {
                let unlink = core.pop_transmit(now).ok_or("DCS unlink acknowledgment")?;
                assert_eq!(
                    unlink.payload, b"W1AW    B NAK\0",
                    "DCS disconnect uses the primary-source space target slot"
                );
            }
        }
        Ok(())
    }

    // ─── DExtra tests ────────────────────────────────────────────

    #[test]
    fn dextra_starts_in_unknown_state() {
        let core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        assert_eq!(core.state_kind(), ServerStateKind::Unknown);
        assert_eq!(core.peer(), PEER);
        assert_eq!(core.protocol_kind(), ProtocolKind::DExtra);
        assert_eq!(core.reflector_module(), Module::C);
        assert!(core.client_callsign().is_none());
        assert!(core.client_module().is_none());
    }

    #[test]
    fn dextra_link_transitions_to_linked() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        assert_eq!(core.state_kind(), ServerStateKind::Unknown);

        let mut buf = [0u8; 16];
        let n = encode_connect_link(&mut buf, LOCAL, Module::C)?;
        let slice = buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        assert_eq!(core.client_callsign(), Some(cs(*b"W1AW    ")));
        assert_eq!(core.client_module(), Some(Module::B));

        let event: Option<ServerEvent<DExtra>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientLinked { .. })));
        Ok(())
    }

    #[test]
    fn wrong_protocol_marker_preserves_queued_server_event() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut bytes = [0_u8; 16];
        let length = encode_connect_link(&mut bytes, LOCAL, Module::C)?;
        core.handle_input(
            Instant::now(),
            bytes.get(..length).ok_or("encoded LINK length")?,
        )?;
        assert!(
            core.pop_event::<DPlus>().is_none(),
            "a DPlus marker cannot claim a DExtra event"
        );
        assert!(
            matches!(
                core.pop_event::<DExtra>(),
                Some(ServerEvent::ClientLinked { .. })
            ),
            "refused marker must leave the original event queued"
        );
        Ok(())
    }

    #[test]
    fn dextra_link_enqueues_ack() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut buf = [0u8; 16];
        let n = encode_connect_link(&mut buf, LOCAL, Module::C)?;
        let slice = buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        let tx = core.pop_transmit(Instant::now()).ok_or("tx")?;
        assert_eq!(tx.payload.len(), 14, "DExtra ACK is 14 bytes");
        assert_eq!(tx.dst, PEER);
        assert_eq!(tx.payload.get(10..13), Some(b"ACK".as_slice()));
        assert_eq!(tx.payload.get(13), Some(&0x00));
        Ok(())
    }

    #[test]
    fn dextra_poll_from_linked_client_is_echoed() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(&mut link_buf, LOCAL, Module::C)?;
        let slice = link_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;
        let _ack = core.pop_transmit(Instant::now()).ok_or("ack")?;
        let _event: Option<ServerEvent<DExtra>> = core.pop_event();

        let mut poll_buf = [0u8; 16];
        let n = encode_poll(&mut poll_buf, &cs(*b"W1AW    "))?;
        let slice = poll_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        let tx = core.pop_transmit(Instant::now()).ok_or("echo")?;
        assert_eq!(tx.payload.len(), 9, "DExtra poll echo is 9 bytes");
        assert_eq!(tx.dst, PEER);
        Ok(())
    }

    #[test]
    fn dextra_poll_from_unknown_callsign_is_ignored() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(&mut link_buf, LOCAL, Module::C)?;
        let slice = link_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;
        let _ack = core.pop_transmit(Instant::now()).ok_or("ack")?;
        let _event: Option<ServerEvent<DExtra>> = core.pop_event();

        let mut poll_buf = [0u8; 16];
        let n = encode_poll(&mut poll_buf, &cs(*b"N0CALL  "))?;
        let slice = poll_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        assert!(
            core.pop_transmit(Instant::now()).is_none(),
            "poll from wrong callsign ignored"
        );
        Ok(())
    }

    #[test]
    fn dextra_unlink_transitions_to_closed() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(&mut link_buf, LOCAL, Module::C)?;
        let slice = link_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;
        let _ack = core.pop_transmit(Instant::now()).ok_or("ack")?;
        let _event: Option<ServerEvent<DExtra>> = core.pop_event();

        let mut ulink_buf = [0u8; 16];
        let n = encode_unlink(&mut ulink_buf, LOCAL)?;
        let slice = ulink_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        assert_eq!(core.state_kind(), ServerStateKind::Closed);
        let event: Option<ServerEvent<DExtra>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientUnlinked { .. })));
        Ok(())
    }

    #[test]
    fn dextra_unlink_from_wrong_callsign_is_ignored() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(&mut link_buf, LOCAL, Module::C)?;
        let slice = link_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;
        let _ack = core.pop_transmit(Instant::now()).ok_or("ack")?;
        let _event: Option<ServerEvent<DExtra>> = core.pop_event();

        let mut ulink_buf = [0u8; 16];
        let n = encode_unlink(
            &mut ulink_buf,
            RepeaterIdentity {
                callsign: RoutingCallsign::new("N0CALL")?,
                module: Module::B,
            },
        )?;
        let slice = ulink_buf.get(..n).ok_or("n bytes")?;
        core.handle_input(Instant::now(), slice)?;

        assert_eq!(
            core.state_kind(),
            ServerStateKind::Linked,
            "state unchanged when wrong callsign tries to unlink"
        );
        Ok(())
    }

    // ─── DPlus tests ─────────────────────────────────────────────

    #[test]
    fn dplus_link1_transitions_to_link1_received() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf = [0u8; 16];
        let n = encode_link1(&mut buf)?;
        core.handle_input(Instant::now(), buf.get(..n).ok_or("bytes")?)?;
        assert_eq!(core.state_kind(), ServerStateKind::Unknown);
        let tx = core.pop_transmit(Instant::now()).ok_or("ack")?;
        assert_eq!(tx.payload.len(), 5, "DPlus LINK1 ACK is 5 bytes");
        assert_eq!(tx.payload, &[0x05, 0x00, 0x18, 0x00, 0x01]);
        Ok(())
    }

    #[test]
    fn dplus_link2_after_link1_transitions_to_linked_and_accepts() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ack1 = core.pop_transmit(Instant::now()).ok_or("ack1")?;

        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        assert_eq!(core.client_callsign(), Some(cs(*b"W1AW    ")));
        assert_eq!(core.client_module(), Some(Module::C));

        let tx = core.pop_transmit(Instant::now()).ok_or("okrw")?;
        assert_eq!(tx.payload.len(), 8, "DPlus LINK2 reply is 8 bytes");
        assert_eq!(tx.payload.get(4..8), Some(b"OKRW".as_slice()));

        let event: Option<ServerEvent<DPlus>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientLinked { .. })));
        Ok(())
    }

    #[test]
    fn dplus_link2_without_link1_is_ignored() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf = [0u8; 32];
        let n = encode_link2(&mut buf, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf.get(..n).ok_or("bytes")?)?;
        assert_eq!(core.state_kind(), ServerStateKind::Unknown);
        assert!(core.pop_transmit(Instant::now()).is_none());
        Ok(())
    }

    #[test]
    fn dplus_already_linked_link2_from_different_callsign_returns_busy() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ack1 = core.pop_transmit(Instant::now()).ok_or("ack1")?;
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _ok = core.pop_transmit(Instant::now()).ok_or("okrw")?;
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let mut buf3 = [0u8; 32];
        let n = encode_link2(&mut buf3, &cs(*b"N0CALL  "))?;
        core.handle_input(Instant::now(), buf3.get(..n).ok_or("bytes")?)?;

        let tx = core.pop_transmit(Instant::now()).ok_or("busy")?;
        assert_eq!(tx.payload.len(), 8);
        assert_eq!(tx.payload.get(4..8), Some(b"BUSY".as_slice()));
        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        assert_eq!(core.client_callsign(), Some(cs(*b"W1AW    ")));
        Ok(())
    }

    #[test]
    fn dplus_voice_header_after_link2_transitions_to_streaming() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _a1 = core.pop_transmit(Instant::now());
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _a2 = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let mut hdr_buf = [0u8; 128];
        let n = crate::codec::dplus::encode_voice_header(
            &mut hdr_buf,
            sid(0xCAFE),
            &test_header(*b"W1AW    "),
        )?;
        core.handle_input(Instant::now(), hdr_buf.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Streaming);
        let event: Option<ServerEvent<DPlus>> = core.pop_event();
        assert!(matches!(
            event,
            Some(ServerEvent::ClientStreamStarted { .. })
        ));
        Ok(())
    }

    #[test]
    fn dplus_voice_data_during_streaming_emits_frame_event() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();
        let mut hdr_buf = [0u8; 128];
        let n = crate::codec::dplus::encode_voice_header(
            &mut hdr_buf,
            sid(0xCAFE),
            &test_header(*b"W1AW    "),
        )?;
        core.handle_input(Instant::now(), hdr_buf.get(..n).ok_or("bytes")?)?;
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let frame = VoiceFrame::silence();
        let mut data_buf = [0u8; 128];
        let n = crate::codec::dplus::encode_voice_data(
            &mut data_buf,
            sid(0xCAFE),
            FrameSequence::try_from(3)?,
            &frame,
        )?;
        core.handle_input(Instant::now(), data_buf.get(..n).ok_or("bytes")?)?;

        let event: Option<ServerEvent<DPlus>> = core.pop_event();
        assert!(matches!(
            event,
            Some(ServerEvent::ClientStreamFrame { stream_id, seq, .. })
            if stream_id == sid(0xCAFE) && seq == 3
        ));
        Ok(())
    }

    #[test]
    fn dplus_voice_eot_returns_to_linked() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();
        let mut hdr_buf = [0u8; 128];
        let n = crate::codec::dplus::encode_voice_header(
            &mut hdr_buf,
            sid(0xCAFE),
            &test_header(*b"W1AW    "),
        )?;
        core.handle_input(Instant::now(), hdr_buf.get(..n).ok_or("bytes")?)?;
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let mut eot_buf = [0u8; 128];
        let n = crate::codec::dplus::encode_voice_eot(
            &mut eot_buf,
            sid(0xCAFE),
            FrameSequence::try_from(20)?,
        )?;
        core.handle_input(Instant::now(), eot_buf.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        let event: Option<ServerEvent<DPlus>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientStreamEnded { .. })));
        Ok(())
    }

    #[test]
    fn dplus_unlink_transitions_to_closed_and_acks() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let mut ub = [0u8; 16];
        let n = dplus_encode_unlink(&mut ub)?;
        core.handle_input(Instant::now(), ub.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Closed);
        let tx = core.pop_transmit(Instant::now()).ok_or("ack")?;
        assert_eq!(tx.payload.len(), 5);
        assert_eq!(tx.payload.get(4), Some(&0x00));
        let event: Option<ServerEvent<DPlus>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientUnlinked { .. })));
        Ok(())
    }

    #[test]
    fn dplus_poll_emits_echo() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::DPlus, DPLUS_PEER, Module::C);
        let mut buf1 = [0u8; 16];
        let n = encode_link1(&mut buf1)?;
        core.handle_input(Instant::now(), buf1.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let mut buf2 = [0u8; 32];
        let n = encode_link2(&mut buf2, &cs(*b"W1AW    "))?;
        core.handle_input(Instant::now(), buf2.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<DPlus>> = core.pop_event();

        let mut pb = [0u8; 16];
        let n = dplus_encode_poll(&mut pb)?;
        core.handle_input(Instant::now(), pb.get(..n).ok_or("bytes")?)?;

        let tx = core.pop_transmit(Instant::now()).ok_or("echo")?;
        assert_eq!(tx.payload.len(), 3, "DPlus poll echo is 3 bytes");
        assert_eq!(tx.payload, &[0x03, 0x60, 0x00]);
        Ok(())
    }

    // ─── DCS tests ───────────────────────────────────────────────

    #[test]
    fn dcs_link_transitions_to_linked_and_acks() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut buf = [0u8; 520];
        let n = dcs_encode_link(
            &mut buf,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), buf.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        assert_eq!(core.client_callsign(), Some(cs(*b"W1AW    ")));
        assert_eq!(core.client_module(), Some(Module::B));

        let tx = core.pop_transmit(Instant::now()).ok_or("ack")?;
        assert_eq!(tx.payload.len(), 14, "DCS ACK is 14 bytes");
        assert_eq!(tx.payload.get(10..13), Some(b"ACK".as_slice()));

        let event: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientLinked { .. })));
        Ok(())
    }

    /// Non-silence frame: the DCS decoder flags EOT when `slow_data`
    /// is `[0x55, 0x55, 0x55]` (the D-STAR sync pattern), which is
    /// what `VoiceFrame::silence` returns. Mid-stream tests use this
    /// constructor to avoid triggering the EOT heuristic.
    fn non_eot_frame() -> VoiceFrame {
        VoiceFrame {
            ambe: [0x11; 9],
            slow_data: [0x22; 3],
        }
    }

    #[test]
    fn dcs_voice_from_linked_state_starts_stream() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut lb = [0u8; 520];
        let n = dcs_encode_link(
            &mut lb,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), lb.get(..n).ok_or("bytes")?)?;
        let _ack = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let frame = non_eot_frame();
        let mut vb = [0u8; 128];
        let n = dcs_encode_voice(
            &mut vb,
            &test_header(*b"W1AW    "),
            sid(0xBEEF),
            FrameSequence::ZERO,
            &frame,
            false,
        )?;
        core.handle_input(Instant::now(), vb.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Streaming);
        let ev1: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(ev1, Some(ServerEvent::ClientStreamStarted { .. })));
        let ev2: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(ev2, Some(ServerEvent::ClientStreamFrame { .. })));
        Ok(())
    }

    #[test]
    fn dcs_voice_mid_stream_emits_frame() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut lb = [0u8; 520];
        let n = dcs_encode_link(
            &mut lb,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), lb.get(..n).ok_or("bytes")?)?;
        let _a = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let frame = non_eot_frame();
        let mut vb = [0u8; 128];
        let n = dcs_encode_voice(
            &mut vb,
            &test_header(*b"W1AW    "),
            sid(0xBEEF),
            FrameSequence::ZERO,
            &frame,
            false,
        )?;
        core.handle_input(Instant::now(), vb.get(..n).ok_or("bytes")?)?;
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let mut vb2 = [0u8; 128];
        let n = dcs_encode_voice(
            &mut vb2,
            &test_header(*b"W1AW    "),
            sid(0xBEEF),
            FrameSequence::try_from(1)?,
            &frame,
            false,
        )?;
        core.handle_input(Instant::now(), vb2.get(..n).ok_or("bytes")?)?;

        let ev: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(
            ev,
            Some(ServerEvent::ClientStreamFrame { seq: 1, .. })
        ));
        assert!(core.pop_event::<Dcs>().is_none());
        Ok(())
    }

    #[test]
    fn dcs_voice_with_is_end_transitions_back_to_linked() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut lb = [0u8; 520];
        let n = dcs_encode_link(
            &mut lb,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), lb.get(..n).ok_or("bytes")?)?;
        let _ = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let frame = VoiceFrame::silence();
        let mut vb = [0u8; 128];
        let n = dcs_encode_voice(
            &mut vb,
            &test_header(*b"W1AW    "),
            sid(0xBEEF),
            FrameSequence::try_from(5)?,
            &frame,
            true,
        )?;
        core.handle_input(Instant::now(), vb.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Linked);
        let _a: Option<ServerEvent<Dcs>> = core.pop_event();
        let _b: Option<ServerEvent<Dcs>> = core.pop_event();
        let end: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(end, Some(ServerEvent::ClientStreamEnded { .. })));
        Ok(())
    }

    #[test]
    fn dcs_poll_emits_echo() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut lb = [0u8; 520];
        let n = dcs_encode_link(
            &mut lb,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), lb.get(..n).ok_or("bytes")?)?;
        let _ack = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let mut pb = [0u8; 32];
        let n = dcs_encode_poll(&mut pb, LOCAL.callsign, DCS_TARGET)?;
        core.handle_input(Instant::now(), pb.get(..n).ok_or("bytes")?)?;

        let tx = core.pop_transmit(Instant::now()).ok_or("echo")?;
        assert_eq!(tx.payload.len(), 17, "DCS poll echo is 17 bytes");
        Ok(())
    }

    #[test]
    fn dcs_unlink_transitions_to_closed() -> TestResult {
        let mut core = ServerSessionCore::new(ProtocolKind::Dcs, DCS_PEER, Module::C);
        let mut lb = [0u8; 520];
        let n = dcs_encode_link(
            &mut lb,
            LOCAL,
            Module::C,
            DCS_TARGET,
            crate::codec::dcs::GatewayType::Repeater,
        )?;
        core.handle_input(Instant::now(), lb.get(..n).ok_or("bytes")?)?;
        let _ack = core.pop_transmit(Instant::now());
        let _ev: Option<ServerEvent<Dcs>> = core.pop_event();

        let mut ub = [0u8; 32];
        let n = dcs_encode_unlink(&mut ub, LOCAL, DCS_TARGET)?;
        core.handle_input(Instant::now(), ub.get(..n).ok_or("bytes")?)?;

        assert_eq!(core.state_kind(), ServerStateKind::Closed);
        let event: Option<ServerEvent<Dcs>> = core.pop_event();
        assert!(matches!(event, Some(ServerEvent::ClientUnlinked { .. })));
        Ok(())
    }

    // ─── Misc ────────────────────────────────────────────────────

    #[test]
    fn next_deadline_is_none_on_empty_outbox() {
        let core = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        assert!(core.next_deadline().is_none());
    }
}
