//! `ProtocolEndpoint<P>` — per-protocol reflector RX shell.
//!
//! Holds the client pool, active stream cache, and protocol
//! discriminator for one of the three D-STAR reflector protocols.
//! [`ProtocolEndpoint::handle_inbound`] is the sans-io entry point;
//! [`ProtocolEndpoint::run`] is the UDP pump plus the voice fan-out path.

use std::collections::{HashMap, VecDeque};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::sync::{broadcast, watch};

use dstar_gateway_core::ServerSessionCore;
use dstar_gateway_core::codec::dcs::{
    ClientPacket as DcsClientPacket, decode_client_to_server as decode_dcs_client_to_server,
    encode_connect_nak as encode_dcs_connect_nak,
};
use dstar_gateway_core::codec::dextra::{
    ClientPacket, decode_client_to_server, encode_connect_nak,
};
use dstar_gateway_core::codec::dplus::{
    ClientPacket as DPlusClientPacket, Link2Result,
    decode_client_to_server as decode_dplus_client_to_server, encode_link2_reply,
};
use dstar_gateway_core::error::Error as CoreError;
use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::session::server::ServerEvent;
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId};
use dstar_gateway_core::validator::NullSink;

use crate::client_pool::{ClientHandle, ClientPool, UnhealthyOutcome};
use crate::reflector::{AccessPolicy, ClientAuthorizer, LinkAttempt, StreamCache};
use crate::tokio_shell::fanout::fan_out_voice;
use crate::tokio_shell::transcode::{
    CrossProtocolEvent, TranscodeError, VoiceEvent, transcode_voice,
};

/// Outbound result from a single [`ProtocolEndpoint::handle_inbound`] call.
///
/// Carries the outbound datagrams the core wants to send plus the
/// server events the core emitted. The run loop consumes this to drive
/// the real `UdpSocket` and to update the fan-out engine's cache.
#[derive(Debug, Clone)]
pub struct EndpointOutcome<P: Protocol> {
    /// Outbound datagrams — each `(bytes, destination)`.
    pub txs: Vec<(Vec<u8>, SocketAddr)>,
    /// Consumer-visible server events.
    pub events: Vec<ServerEvent<P>>,
    /// Cached voice-header bytes to rebroadcast to the rest of the
    /// module on this tick.
    ///
    /// Populated by the stream cache every 21 voice frames to match
    /// the `xlxd` / `MMDVMHost` cadence — the run loop fans these
    /// bytes out to every non-originator peer on the module in
    /// addition to the normal voice frame that triggered the cadence.
    ///
    /// Empty on the vast majority of ticks.
    pub header_retransmit: Option<Vec<u8>>,
}

impl<P: Protocol> EndpointOutcome<P> {
    /// Construct an empty outcome (no txs, no events, no retransmit).
    ///
    /// We cannot derive `Default` because it would require
    /// `P: Default`, which the sealed `Protocol` trait intentionally
    /// doesn't bound. Every protocol marker is a ZST so constructing
    /// an empty outcome has no data-dependent initialization anyway.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            txs: Vec::new(),
            events: Vec::new(),
            header_retransmit: None,
        }
    }
}

/// Derive a cross-protocol [`VoiceEvent`] from a server event.
///
/// Returns `None` for non-voice events (linked/unlinked/rejected/…).
/// The EOT branch reports seq `0` because the server event doesn't
/// carry the final seq — downstream encoders OR the 0x40 end-bit in
/// on their own, so the value doesn't matter for correctness of the
/// encoding, only for bandwidth log parity with the originator.
const fn voice_event_from_server_event<P: Protocol>(ev: &ServerEvent<P>) -> Option<VoiceEvent> {
    match ev {
        ServerEvent::ClientStreamStarted {
            stream_id, header, ..
        } => Some(VoiceEvent::StreamStart {
            header: *header,
            stream_id: *stream_id,
        }),
        ServerEvent::ClientStreamFrame {
            stream_id,
            seq,
            frame,
            ..
        } => Some(VoiceEvent::Frame {
            stream_id: *stream_id,
            seq: *seq,
            frame: *frame,
        }),
        ServerEvent::ClientStreamEnded { stream_id, .. } => Some(VoiceEvent::StreamEnd {
            stream_id: *stream_id,
            seq: 0,
        }),
        _ => None,
    }
}

/// Errors returned by the shell layer.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    /// Decoding or state-machine error bubbled up from the core.
    #[error("core error: {0}")]
    Core(#[from] CoreError),
    /// Protocol-layer error (framing problem, unexpected variant, etc.).
    #[error("protocol error: {0}")]
    Protocol(String),
    /// UDP socket I/O error.
    #[error("socket I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A hint describing an inbound datagram's role in the fan-out path.
///
/// Extracted from the [`EndpointOutcome::events`] list so the run loop
/// can forward voice bytes without re-examining the wire format.
#[derive(Debug, Clone, Copy)]
enum ForwardHint {
    Header { module: Module, stream_id: StreamId },
    Data { module: Module, stream_id: StreamId },
    Eot { module: Module, stream_id: StreamId },
}

/// Per-protocol reflector endpoint.
///
/// Owns the client pool, the per-module stream cache, and the
/// authorizer used to admit LINK attempts for one reflector protocol.
/// Supports all three D-STAR reflector protocols (`DExtra`, `DPlus`,
/// `DCS`); the endpoint's default reflector module is used as the
/// fallback for `DPlus` sessions (which don't carry a module on the
/// wire) and as the seed for `DExtra`/`DCS` sessions before the LINK
/// packet overwrites it.
pub struct ProtocolEndpoint<P: Protocol> {
    protocol: ProtocolKind,
    clients: ClientPool<P>,
    /// Default reflector module for this endpoint.
    ///
    /// Used as the initial `reflector_module` for every
    /// [`ServerSessionCore`] created on this endpoint. `DExtra` and
    /// `DCS` sessions overwrite their `client_module` from the LINK
    /// packet; `DPlus` sessions keep this placeholder because the
    /// `DPlus` LINK2 packet doesn't carry a module on the wire.
    default_reflector_module: Module,
    /// Per-module active stream cache — populated on voice header,
    /// updated on voice data, cleared on voice EOT. Drives the
    /// 21-frame header-retransmit cadence in [`Self::handle_inbound`].
    stream_cache: Mutex<HashMap<Module, StreamCache>>,
    /// Authorizer consulted on every LINK attempt.
    authorizer: Arc<dyn ClientAuthorizer>,
    /// Pending events produced by background work (fan-out eviction,
    /// health checks) that didn't happen during a
    /// [`Self::handle_inbound`] call. Drained into the next outcome
    /// surfaced to the caller so consumers of the event stream see
    /// eviction decisions.
    pending_events: Mutex<VecDeque<ServerEvent<P>>>,
    /// Cross-protocol voice bus — `Some` iff the reflector was
    /// constructed with `cross_protocol_forwarding = true`. Published
    /// to after each inbound voice event so other protocols'
    /// endpoints can transcode and fan out the frame on their own
    /// wire format.
    voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
    _protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> std::fmt::Debug for ProtocolEndpoint<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ClientPool<P>` and the stream cache map aren't printed —
        // `P` doesn't bound `Debug`, and the pool contents are
        // runtime-owned by tokio locks we can't cheaply peek at in a
        // Debug impl.
        f.debug_struct("ProtocolEndpoint")
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

impl<P: Protocol> ProtocolEndpoint<P> {
    /// Construct a new endpoint for the given protocol with the
    /// supplied authorizer.
    ///
    /// `default_reflector_module` is passed to every
    /// [`ServerSessionCore`] created on this endpoint; `DExtra` and
    /// `DCS` sessions overwrite their `client_module` from the LINK
    /// packet but `DPlus` sessions keep the default because the
    /// `DPlus` LINK2 wire packet doesn't carry a module.
    ///
    /// The authorizer is consulted on every inbound LINK attempt;
    /// rejected attempts never materialize a [`ClientHandle`] and
    /// instead produce a protocol-appropriate NAK plus a
    /// [`ServerEvent::ClientRejected`] event.
    #[must_use]
    pub fn new(
        protocol: ProtocolKind,
        default_reflector_module: Module,
        authorizer: Arc<dyn ClientAuthorizer>,
    ) -> Self {
        Self::new_with_voice_bus(protocol, default_reflector_module, authorizer, None)
    }

    /// Construct a new endpoint with an optional cross-protocol voice bus.
    ///
    /// Identical to [`Self::new`] except the caller supplies a
    /// [`broadcast::Sender<CrossProtocolEvent>`] clone; when `Some`,
    /// the endpoint publishes inbound voice events to the bus so
    /// other protocols' endpoints can transcode and re-broadcast.
    ///
    /// Used by [`crate::reflector::Reflector`] when its config has
    /// `cross_protocol_forwarding = true`. Pass `None` to disable
    /// cross-protocol participation on this endpoint.
    #[must_use]
    pub fn new_with_voice_bus(
        protocol: ProtocolKind,
        default_reflector_module: Module,
        authorizer: Arc<dyn ClientAuthorizer>,
        voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
    ) -> Self {
        Self {
            protocol,
            clients: ClientPool::<P>::new(),
            default_reflector_module,
            stream_cache: Mutex::new(HashMap::new()),
            authorizer,
            pending_events: Mutex::new(VecDeque::new()),
            voice_bus,
            _protocol: PhantomData,
        }
    }

    /// Runtime protocol discriminator for this endpoint.
    #[must_use]
    pub const fn protocol_kind(&self) -> ProtocolKind {
        self.protocol
    }

    /// Access the endpoint's client pool (primarily for tests).
    #[must_use]
    pub const fn clients(&self) -> &ClientPool<P> {
        &self.clients
    }

    /// Feed one inbound datagram into the endpoint.
    ///
    /// Dispatches to the protocol-specific handler based on
    /// [`Self::protocol_kind`]. Each handler pre-decodes the inbound
    /// packet, consults the authorizer on LINK attempts, gates
    /// voice-stream ingress on [`AccessPolicy`], drives the core via
    /// the private `drive_core` helper, then updates the per-module
    /// stream cache and drains pending background events into the
    /// outcome.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::Core`] if the core rejects the input
    /// (parse failure, wrong-state, etc.). Returns
    /// [`ShellError::Protocol`] if the endpoint was constructed with
    /// a [`ProtocolKind`] the shell does not recognize.
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe. It takes multiple
    /// [`ClientPool`] locks in sequence (`contains` → `insert` →
    /// `set_module` → `record_last_heard`) and cancellation between
    /// any two awaits can leave the pool in a half-updated state where
    /// a session has been created but not yet attached to its module
    /// in the reverse index. The reflector's run loop is the only
    /// expected caller and it never cancels this future except via
    /// shutdown.
    pub async fn handle_inbound(
        &self,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        match self.protocol {
            ProtocolKind::DExtra => self.handle_inbound_dextra(bytes, peer, now).await,
            ProtocolKind::DPlus => self.handle_inbound_dplus(bytes, peer, now).await,
            ProtocolKind::Dcs => self.handle_inbound_dcs(bytes, peer, now).await,
            _ => Err(ShellError::Protocol(format!(
                "unsupported protocol discriminator: {:?}",
                self.protocol
            ))),
        }
    }

    /// `DExtra`-specific inbound pipeline.
    ///
    /// Pre-decodes the `DExtra` wire packet, consults the authorizer on
    /// `Link`, gates voice-stream ingress on `AccessPolicy::ReadOnly`,
    /// drives the core, mirrors `ClientLinked` module into the pool's
    /// reverse index, and maintains the per-module stream cache.
    async fn handle_inbound_dextra(
        &self,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        // Pre-decode the DExtra packet for dispatch only. The real
        // state transitions happen in `drive_core` via
        // `ServerSessionCore::handle_input`.
        let mut null_sink = NullSink;
        let pre_decoded = decode_client_to_server(bytes, &mut null_sink).ok();

        // LINK → authorizer. Rejected attempts never materialize a
        // ClientHandle; they produce a NAK + `ClientRejected` event.
        let link_access: Option<AccessPolicy> = if let Some(ClientPacket::Link {
            callsign,
            reflector_module,
            ..
        }) = pre_decoded.clone()
        {
            let attempt = LinkAttempt {
                protocol: self.protocol,
                callsign,
                peer,
                module: reflector_module,
            };
            match self.authorizer.authorize(&attempt) {
                Ok(access_policy) => Some(access_policy),
                Err(reject) => {
                    tracing::info!(
                        ?peer,
                        %callsign,
                        %reflector_module,
                        reason = ?reject,
                        "authorizer rejected DExtra LINK attempt"
                    );
                    return Ok(Self::build_dextra_reject_outcome(
                        peer,
                        callsign,
                        reflector_module,
                        reject,
                    ));
                }
            }
        } else {
            None
        };

        self.ensure_handle(peer, link_access, now).await;

        // ReadOnly voice drop check — must happen BEFORE drive_core
        // so the state machine never sees the voice bytes.
        if self
            .read_only_drop_voice_dextra(pre_decoded.as_ref(), peer, now)
            .await
        {
            let mut outcome = EndpointOutcome::<P>::empty();
            if let Some(pkt) = pre_decoded.as_ref()
                && let Some(stream_id) = Self::voice_stream_id_dextra(pkt)
            {
                outcome
                    .events
                    .push(ServerEvent::VoiceFromReadOnlyDropped { peer, stream_id });
            }
            return Ok(outcome);
        }

        let mut outcome = self.drive_core(&peer, bytes, now).await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_linked_module(&outcome, peer).await;

        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit =
                self.update_stream_cache_dextra(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer).await;
        self.drain_pending_events(&mut outcome).await;
        Ok(outcome)
    }

    /// `DPlus`-specific inbound pipeline.
    ///
    /// `DPlus` has a two-step handshake: `Link1` carries no callsign
    /// (pass-through to the core, which transitions to a transitional
    /// `Link1Received` state and enqueues the 5-byte ACK echo), then
    /// `Link2` carries the client's callsign and fires the authorizer.
    /// On a rejected `Link2` we emit an 8-byte `BUSY` reply and a
    /// [`ServerEvent::ClientRejected`] event but do NOT create a
    /// pool handle.
    async fn handle_inbound_dplus(
        &self,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        let mut null_sink = NullSink;
        let pre_decoded = decode_dplus_client_to_server(bytes, &mut null_sink).ok();

        // LINK2 → authorizer. LINK1 passes through unconditionally
        // because it carries no callsign; the core's
        // `handle_dplus_input` walks the state machine from
        // `Unknown → Link1Received` and enqueues the LINK1 ACK.
        let link_access: Option<AccessPolicy> =
            if let Some(DPlusClientPacket::Link2 { callsign }) = pre_decoded.clone() {
                let attempt = LinkAttempt {
                    protocol: self.protocol,
                    callsign,
                    peer,
                    module: self.default_reflector_module,
                };
                match self.authorizer.authorize(&attempt) {
                    Ok(access_policy) => Some(access_policy),
                    Err(reject) => {
                        tracing::info!(
                            ?peer,
                            %callsign,
                            reason = ?reject,
                            "authorizer rejected DPlus LINK2 attempt"
                        );
                        return Ok(Self::build_dplus_reject_outcome(peer, reject));
                    }
                }
            } else {
                None
            };

        self.ensure_handle(peer, link_access, now).await;

        if self
            .read_only_drop_voice_dplus(pre_decoded.as_ref(), peer, now)
            .await
        {
            let mut outcome = EndpointOutcome::<P>::empty();
            if let Some(pkt) = pre_decoded.as_ref()
                && let Some(stream_id) = Self::voice_stream_id_dplus(pkt)
            {
                outcome
                    .events
                    .push(ServerEvent::VoiceFromReadOnlyDropped { peer, stream_id });
            }
            return Ok(outcome);
        }

        let mut outcome = self.drive_core(&peer, bytes, now).await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_linked_module(&outcome, peer).await;

        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit = self.update_stream_cache_dplus(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer).await;
        self.drain_pending_events(&mut outcome).await;
        Ok(outcome)
    }

    /// `DCS`-specific inbound pipeline.
    ///
    /// DCS carries the D-STAR header embedded in every voice packet,
    /// so the stream-cache lifecycle is different from `DExtra`/`DPlus`:
    /// the first voice packet for a new `stream_id` is treated as a
    /// header (and cached), subsequent packets with the same
    /// `stream_id` are data, and a packet with `is_end = true`
    /// clears the cache.
    async fn handle_inbound_dcs(
        &self,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        let mut null_sink = NullSink;
        let pre_decoded = decode_dcs_client_to_server(bytes, &mut null_sink).ok();

        let link_access: Option<AccessPolicy> = if let Some(DcsClientPacket::Link {
            callsign,
            reflector_module,
            ..
        }) = pre_decoded.clone()
        {
            let attempt = LinkAttempt {
                protocol: self.protocol,
                callsign,
                peer,
                module: reflector_module,
            };
            match self.authorizer.authorize(&attempt) {
                Ok(access_policy) => Some(access_policy),
                Err(reject) => {
                    tracing::info!(
                        ?peer,
                        %callsign,
                        %reflector_module,
                        reason = ?reject,
                        "authorizer rejected DCS LINK attempt"
                    );
                    return Ok(Self::build_dcs_reject_outcome(
                        peer,
                        callsign,
                        reflector_module,
                        reject,
                    ));
                }
            }
        } else {
            None
        };

        self.ensure_handle(peer, link_access, now).await;

        if self
            .read_only_drop_voice_dcs(pre_decoded.as_ref(), peer, now)
            .await
        {
            let mut outcome = EndpointOutcome::<P>::empty();
            if let Some(pkt) = pre_decoded.as_ref()
                && let Some(stream_id) = Self::voice_stream_id_dcs(pkt)
            {
                outcome
                    .events
                    .push(ServerEvent::VoiceFromReadOnlyDropped { peer, stream_id });
            }
            return Ok(outcome);
        }

        let mut outcome = self.drive_core(&peer, bytes, now).await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_linked_module(&outcome, peer).await;

        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit = self.update_stream_cache_dcs(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer).await;
        self.drain_pending_events(&mut outcome).await;
        Ok(outcome)
    }

    /// Ensure a [`ClientHandle`] exists for `peer` in the pool,
    /// creating one if needed.
    ///
    /// `link_access` is the authorizer decision from a fresh LINK
    /// pre-decode; if `None` (e.g. for non-LINK packets) the
    /// fallback is [`AccessPolicy::ReadWrite`]. The LINK path above
    /// overwrites the fallback when it fires.
    async fn ensure_handle(
        &self,
        peer: SocketAddr,
        link_access: Option<AccessPolicy>,
        now: Instant,
    ) {
        if self.clients.contains(&peer).await {
            return;
        }
        let access = link_access.unwrap_or(AccessPolicy::ReadWrite);
        let reflector_module = self.default_reflector_module;
        let core = ServerSessionCore::new(self.protocol, peer, reflector_module);
        let handle = ClientHandle::new(core, access, now);
        self.clients.insert(peer, handle).await;
    }

    /// Mirror any `ClientLinked` module transitions into the pool's
    /// reverse index so fan-out can enumerate module members in O(1).
    async fn mirror_linked_module(&self, outcome: &EndpointOutcome<P>, peer: SocketAddr) {
        for ev in &outcome.events {
            if let ServerEvent::ClientLinked { module, .. } = ev {
                self.clients.set_module(&peer, *module).await;
            }
        }
    }

    /// Drain any pending background events (fan-out eviction, etc.)
    /// into the outcome the caller will observe.
    async fn drain_pending_events(&self, outcome: &mut EndpointOutcome<P>) {
        let mut pending = self.pending_events.lock().await;
        while let Some(ev) = pending.pop_front() {
            outcome.events.push(ev);
        }
    }

    /// Publish cross-protocol voice events onto the voice bus, if
    /// configured.
    ///
    /// Scans `outcome.events` for voice-lifecycle events and forwards
    /// each one as a [`CrossProtocolEvent`] so other protocols'
    /// endpoints can transcode and fan out to their own module
    /// members. No-op when the endpoint was constructed with a
    /// `None` voice bus.
    async fn publish_voice_events(&self, outcome: &EndpointOutcome<P>, peer: SocketAddr) {
        let Some(bus) = &self.voice_bus else {
            return;
        };
        let Some(module) = self.clients.module_of(&peer).await else {
            return;
        };
        let cached_header = self.cached_header_for_module(module).await;
        for ev in &outcome.events {
            let Some(voice_event) = voice_event_from_server_event(ev) else {
                continue;
            };
            // `broadcast::Sender::send` errors only when there are
            // no live receivers; that's fine for publish — we don't
            // want to fail the inbound path because nobody listens.
            drop(bus.send(CrossProtocolEvent {
                source_protocol: self.protocol,
                source_peer: peer,
                module,
                event: voice_event,
                cached_header,
            }));
        }
    }

    /// Look up the cached `DStarHeader` (if any) for the given module.
    ///
    /// Used by [`Self::publish_voice_events`] so `DCS` subscribers
    /// on the other side of the bus receive the header context they
    /// need to re-encode inbound voice data into 100-byte packets.
    async fn cached_header_for_module(&self, module: Module) -> Option<DStarHeader> {
        let cache = self.stream_cache.lock().await;
        cache.get(&module).map(|entry| *entry.header())
    }

    /// Evict a peer from the pool and enqueue a
    /// [`ServerEvent::ClientEvicted`] event onto the pending-event
    /// queue.
    ///
    /// The queued event is drained on the next [`Self::handle_inbound`]
    /// call and appears on that tick's outcome. Callers invoke this
    /// from the run loop after `fan_out_voice` reports an eviction
    /// decision.
    async fn evict_peer(&self, peer: SocketAddr, reason: &str) {
        drop(self.clients.remove(&peer).await);
        let mut pending = self.pending_events.lock().await;
        pending.push_back(ServerEvent::ClientEvicted {
            peer,
            reason: reason.to_string(),
        });
    }

    /// Update the per-module `DExtra` stream cache for this packet.
    ///
    /// Lifecycle:
    /// - `VoiceHeader`: insert or replace the module's cache entry,
    ///   store the raw 56-byte header.
    /// - `VoiceData`: bump the seq counter; if `should_rebroadcast_header`
    ///   fires return a clone of the cached bytes.
    /// - `VoiceEot`: remove the module's cache entry.
    ///
    /// Returns `None` on all non-retransmit ticks.
    async fn update_stream_cache_dextra(
        &self,
        pkt: &ClientPacket,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let module = self.clients.module_of(&peer).await?;
        let mut cache_guard = self.stream_cache.lock().await;
        match pkt {
            ClientPacket::VoiceHeader { stream_id, header } => {
                let entry =
                    StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
                let _prev = cache_guard.insert(module, entry);
                None
            }
            ClientPacket::VoiceData { .. } => {
                let entry = cache_guard.get_mut(&module)?;
                entry.record_frame(now);
                if entry.should_rebroadcast_header() {
                    Some(entry.header_bytes().to_vec())
                } else {
                    None
                }
            }
            ClientPacket::VoiceEot { .. } => {
                let _prev = cache_guard.remove(&module);
                None
            }
            _ => None,
        }
    }

    /// Update the per-module `DPlus` stream cache for this packet.
    ///
    /// Same lifecycle as [`Self::update_stream_cache_dextra`], but
    /// operates on the [`dstar_gateway_core::codec::dplus::ClientPacket`]
    /// enum.
    async fn update_stream_cache_dplus(
        &self,
        pkt: &DPlusClientPacket,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let module = self.clients.module_of(&peer).await?;
        let mut cache_guard = self.stream_cache.lock().await;
        match pkt {
            DPlusClientPacket::VoiceHeader { stream_id, header } => {
                let entry =
                    StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
                let _prev = cache_guard.insert(module, entry);
                None
            }
            DPlusClientPacket::VoiceData { .. } => {
                let entry = cache_guard.get_mut(&module)?;
                entry.record_frame(now);
                if entry.should_rebroadcast_header() {
                    Some(entry.header_bytes().to_vec())
                } else {
                    None
                }
            }
            DPlusClientPacket::VoiceEot { .. } => {
                let _prev = cache_guard.remove(&module);
                None
            }
            _ => None,
        }
    }

    /// Update the per-module `DCS` stream cache for this packet.
    ///
    /// `DCS` is a single-packet-per-frame protocol: every `Voice`
    /// packet carries the header + AMBE + optional end marker. The
    /// first sighting of a new `stream_id` acts as the implicit
    /// stream-start and is cached. Subsequent packets with the same
    /// `stream_id` are data (and trigger the 21-frame retransmit
    /// cadence). A packet with `is_end = true` clears the cache.
    async fn update_stream_cache_dcs(
        &self,
        pkt: &DcsClientPacket,
        bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let module = self.clients.module_of(&peer).await?;
        let mut cache_guard = self.stream_cache.lock().await;
        let DcsClientPacket::Voice {
            header,
            stream_id,
            is_end,
            ..
        } = pkt
        else {
            return None;
        };

        // First sighting of this stream id on this module: cache the
        // header + raw bytes and emit no retransmit.
        let existing_stream = cache_guard.get(&module).map(StreamCache::stream_id);
        if existing_stream != Some(*stream_id) {
            if *is_end {
                // A one-packet stream that starts and ends in the
                // same datagram — clear any stale entry and bail.
                let _prev = cache_guard.remove(&module);
                return None;
            }
            let entry = StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
            let _prev = cache_guard.insert(module, entry);
            return None;
        }

        // Same stream id — data frame. Bump the seq counter; if the
        // packet is the end-of-stream marker, clear the cache after
        // checking the retransmit cadence one last time.
        let retransmit_payload = cache_guard.get_mut(&module).and_then(|entry| {
            entry.record_frame(now);
            if entry.should_rebroadcast_header() {
                Some(entry.header_bytes().to_vec())
            } else {
                None
            }
        });
        if *is_end {
            let _prev = cache_guard.remove(&module);
        }
        retransmit_payload
    }

    /// Return the voice stream id if this `DExtra` packet is a voice
    /// header, voice data, or voice EOT frame; otherwise `None`.
    const fn voice_stream_id_dextra(pkt: &ClientPacket) -> Option<StreamId> {
        match pkt {
            ClientPacket::VoiceHeader { stream_id, .. }
            | ClientPacket::VoiceData { stream_id, .. }
            | ClientPacket::VoiceEot { stream_id, .. } => Some(*stream_id),
            _ => None,
        }
    }

    /// Return the voice stream id if this `DPlus` packet is a voice
    /// header, voice data, or voice EOT frame; otherwise `None`.
    const fn voice_stream_id_dplus(pkt: &DPlusClientPacket) -> Option<StreamId> {
        match pkt {
            DPlusClientPacket::VoiceHeader { stream_id, .. }
            | DPlusClientPacket::VoiceData { stream_id, .. }
            | DPlusClientPacket::VoiceEot { stream_id, .. } => Some(*stream_id),
            _ => None,
        }
    }

    /// Return the voice stream id if this `DCS` packet is a voice
    /// frame; otherwise `None`.
    const fn voice_stream_id_dcs(pkt: &DcsClientPacket) -> Option<StreamId> {
        match pkt {
            DcsClientPacket::Voice { stream_id, .. } => Some(*stream_id),
            _ => None,
        }
    }

    /// Check whether a `DExtra` pre-decoded packet should be dropped
    /// because the peer has `AccessPolicy::ReadOnly`, and record the
    /// `last_heard` bookkeeping for the drop path.
    ///
    /// Returns `true` if the caller should short-circuit with a
    /// `VoiceFromReadOnlyDropped` event.
    async fn read_only_drop_voice_dextra(
        &self,
        pkt: Option<&ClientPacket>,
        peer: SocketAddr,
        now: Instant,
    ) -> bool {
        let Some(pkt) = pkt else {
            return false;
        };
        if !matches!(
            self.clients.access_of(&peer).await,
            Some(AccessPolicy::ReadOnly)
        ) {
            return false;
        }
        if Self::voice_stream_id_dextra(pkt).is_none() {
            return false;
        }
        self.clients.record_last_heard(&peer, now).await;
        true
    }

    /// `DPlus` sibling of [`Self::read_only_drop_voice_dextra`].
    async fn read_only_drop_voice_dplus(
        &self,
        pkt: Option<&DPlusClientPacket>,
        peer: SocketAddr,
        now: Instant,
    ) -> bool {
        let Some(pkt) = pkt else {
            return false;
        };
        if !matches!(
            self.clients.access_of(&peer).await,
            Some(AccessPolicy::ReadOnly)
        ) {
            return false;
        }
        if Self::voice_stream_id_dplus(pkt).is_none() {
            return false;
        }
        self.clients.record_last_heard(&peer, now).await;
        true
    }

    /// `DCS` sibling of [`Self::read_only_drop_voice_dextra`].
    async fn read_only_drop_voice_dcs(
        &self,
        pkt: Option<&DcsClientPacket>,
        peer: SocketAddr,
        now: Instant,
    ) -> bool {
        let Some(pkt) = pkt else {
            return false;
        };
        if !matches!(
            self.clients.access_of(&peer).await,
            Some(AccessPolicy::ReadOnly)
        ) {
            return false;
        }
        if Self::voice_stream_id_dcs(pkt).is_none() {
            return false;
        }
        self.clients.record_last_heard(&peer, now).await;
        true
    }

    /// Build an outcome for a rejected `DExtra` link attempt.
    ///
    /// Emits a single 14-byte NAK datagram and a
    /// [`ServerEvent::ClientRejected`] event. The client pool is not
    /// touched — the peer never becomes a handle.
    fn build_dextra_reject_outcome(
        peer: SocketAddr,
        callsign: Callsign,
        reflector_module: Module,
        reject: crate::reflector::RejectReason,
    ) -> EndpointOutcome<P> {
        let mut outcome = EndpointOutcome::<P>::empty();
        let mut buf = [0u8; 16];
        if let Ok(n) = encode_connect_nak(&mut buf, &callsign, reflector_module)
            && let Some(payload) = buf.get(..n)
        {
            outcome.txs.push((payload.to_vec(), peer));
        }
        outcome.events.push(ServerEvent::ClientRejected {
            peer,
            reason: reject.into_core_reason(),
        });
        outcome
    }

    /// Build an outcome for a rejected `DPlus` LINK2 attempt.
    ///
    /// Emits an 8-byte `BUSY` reply (`Link2Result::Busy`) and a
    /// [`ServerEvent::ClientRejected`] event. The client pool is not
    /// touched — the peer never becomes a handle.
    fn build_dplus_reject_outcome(
        peer: SocketAddr,
        reject: crate::reflector::RejectReason,
    ) -> EndpointOutcome<P> {
        let mut outcome = EndpointOutcome::<P>::empty();
        let mut buf = [0u8; 16];
        if let Ok(n) = encode_link2_reply(&mut buf, Link2Result::Busy)
            && let Some(payload) = buf.get(..n)
        {
            outcome.txs.push((payload.to_vec(), peer));
        }
        outcome.events.push(ServerEvent::ClientRejected {
            peer,
            reason: reject.into_core_reason(),
        });
        outcome
    }

    /// Build an outcome for a rejected `DCS` link attempt.
    ///
    /// Emits a single 14-byte DCS NAK datagram and a
    /// [`ServerEvent::ClientRejected`] event. The client pool is not
    /// touched — the peer never becomes a handle.
    fn build_dcs_reject_outcome(
        peer: SocketAddr,
        callsign: Callsign,
        reflector_module: Module,
        reject: crate::reflector::RejectReason,
    ) -> EndpointOutcome<P> {
        let mut outcome = EndpointOutcome::<P>::empty();
        let mut buf = [0u8; 32];
        if let Ok(n) = encode_dcs_connect_nak(&mut buf, &callsign, reflector_module)
            && let Some(payload) = buf.get(..n)
        {
            outcome.txs.push((payload.to_vec(), peer));
        }
        outcome.events.push(ServerEvent::ClientRejected {
            peer,
            reason: reject.into_core_reason(),
        });
        outcome
    }

    /// Drive the core's state machine and drain its outbox + events.
    ///
    /// Held as a private helper so the lock-protected mutation of the
    /// per-peer `ServerSessionCore` stays in one place. We take the
    /// pool's mutex, borrow the handle mutably, feed the core, and
    /// drain everything into owned `Vec`s before releasing the lock.
    async fn drive_core(
        &self,
        peer: &SocketAddr,
        bytes: &[u8],
        now: Instant,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        // We need mutable access to the handle inside the pool's
        // HashMap. `ClientPool` intentionally doesn't expose `&mut`
        // directly — reach through the private `Mutex<HashMap>` here
        // via a dedicated method on the pool.
        let mut outcome = EndpointOutcome::<P>::empty();

        self.clients
            .with_handle_mut(peer, |handle| -> Result<(), ShellError> {
                handle.session.handle_input(now, bytes)?;
                // The core currently stamps outbox entries with a
                // fresh `Instant::now()` rather than the injected
                // `now`. Sampling the wall clock again here ensures
                // `pop_ready` sees a time strictly after the enqueue
                // instant so just-enqueued packets actually drain.
                let drain_now = Instant::now();
                while let Some(tx) = handle.session.pop_transmit(drain_now) {
                    outcome.txs.push((tx.payload.to_vec(), tx.dst));
                }
                while let Some(ev) = handle.session.pop_event::<P>() {
                    outcome.events.push(ev);
                }
                Ok(())
            })
            .await
            .unwrap_or(Ok(()))?;

        Ok(outcome)
    }

    /// Inspect the outcome events to classify the received datagram.
    ///
    /// Returns the first forwardable voice hint found in the event
    /// list. The run loop uses this hint to route the raw inbound
    /// bytes through the fan-out engine without re-decoding.
    ///
    /// The event carries the stream id, and the peer's module is
    /// resolved from the client pool — the caller has already linked
    /// the peer by the time the hint is extracted.
    fn forward_hint(events: &[ServerEvent<P>], peer_module: Option<Module>) -> Option<ForwardHint> {
        let module = peer_module?;
        for ev in events {
            let hint = match ev {
                ServerEvent::ClientStreamStarted { stream_id, .. } => Some(ForwardHint::Header {
                    module,
                    stream_id: *stream_id,
                }),
                ServerEvent::ClientStreamFrame { stream_id, .. } => Some(ForwardHint::Data {
                    module,
                    stream_id: *stream_id,
                }),
                ServerEvent::ClientStreamEnded { stream_id, .. } => Some(ForwardHint::Eot {
                    module,
                    stream_id: *stream_id,
                }),
                // `ServerEvent` is `non_exhaustive`; the wildcard
                // covers `ClientLinked`/`ClientUnlinked` plus any
                // future variants.
                _ => None,
            };
            if hint.is_some() {
                return hint;
            }
        }
        None
    }

    /// Bind-less run loop that owns a pre-bound [`UdpSocket`].
    ///
    /// Each iteration reads one datagram, feeds it to
    /// [`Self::handle_inbound`], writes outbound responses back to
    /// their destination peer, and finally fans voice frames out to
    /// every other peer on the same module.
    ///
    /// Returns when `shutdown` transitions to `true`, or when an
    /// unrecoverable I/O error occurs.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::Io`] if the socket errors during a
    /// `recv_from`. Send-side failures are logged and the offending
    /// peer is marked unhealthy; they do not terminate the loop.
    ///
    /// # Cancellation safety
    ///
    /// Dropping this future is the intended shutdown mechanism for an
    /// endpoint task — the enclosing [`tokio::task::JoinSet`] in
    /// [`crate::reflector::Reflector::run`] will abort the task when the
    /// shutdown watch channel fires, which drops the `run` future
    /// cleanly. Any in-progress `handle_inbound` call for a single
    /// datagram will be abandoned mid-lock-sequence, which is
    /// acceptable during shutdown because the entire [`ClientPool`]
    /// is about to be dropped with it. Do **not** race `run()` against
    /// another future with `tokio::select!` while the endpoint is
    /// expected to remain operational.
    pub async fn run(
        self: Arc<Self>,
        socket: Arc<UdpSocket>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), ShellError> {
        let mut buf = [0u8; 2048];
        let mut voice_rx = self.voice_bus.as_ref().map(broadcast::Sender::subscribe);
        loop {
            // Pattern: "maybe-subscribed optional branch" — when
            // `voice_rx` is None the voice arm must never resolve so
            // the other arms can still drive. `std::future::pending()`
            // returns `!` which we wrap in `Option` so the select
            // arm type-checks against the `Ok` branch below.
            let voice_branch = async {
                match voice_rx.as_mut() {
                    Some(rx) => Some(rx.recv().await),
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                change = shutdown.changed() => {
                    // `changed` resolves `Err` when all senders drop —
                    // treat that as an implicit shutdown.
                    if change.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                result = socket.recv_from(&mut buf) => {
                    let (n, peer) = result?;
                    let recv_slice = buf.get(..n).unwrap_or(&[]);
                    let owned_bytes = recv_slice.to_vec();
                    let now = Instant::now();
                    self.run_one_tick(&socket, &owned_bytes, peer, now).await?;
                }
                Some(result) = voice_branch => {
                    match result {
                        Ok(event) => {
                            self.handle_cross_protocol_event(&socket, event).await;
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            tracing::warn!(
                                skipped,
                                "cross-protocol bus lagged; catching up"
                            );
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            // Bus has been closed — no more events
                            // will arrive. Drop the subscription so
                            // the voice arm goes quiet forever and
                            // `run` keeps servicing UDP + shutdown.
                            voice_rx = None;
                        }
                    }
                }
            }
        }
    }

    /// Handle a cross-protocol voice event delivered via the
    /// broadcast bus.
    ///
    /// Transcodes the event into this endpoint's wire format via
    /// [`transcode_voice`] and fans the result out to every peer
    /// currently linked to the originator's module on this endpoint.
    /// Same-protocol events (those published by this endpoint itself)
    /// are dropped — the normal within-protocol fan-out path already
    /// handles them.
    async fn handle_cross_protocol_event(
        self: &Arc<Self>,
        socket: &Arc<UdpSocket>,
        event: CrossProtocolEvent,
    ) {
        if event.source_protocol == self.protocol {
            return;
        }
        let mut scratch = [0u8; 2048];
        let len = match transcode_voice(
            self.protocol,
            &event.event,
            event.cached_header.as_ref(),
            &mut scratch,
        ) {
            Ok(n) => n,
            Err(TranscodeError::Encode(e)) => {
                tracing::warn!(
                    target = ?self.protocol,
                    source = ?event.source_protocol,
                    err = ?e,
                    "cross-protocol transcode encode failed"
                );
                return;
            }
            Err(TranscodeError::MissingCachedHeader) => {
                tracing::debug!(
                    target = ?self.protocol,
                    source = ?event.source_protocol,
                    "cross-protocol transcode dropped: target requires cached header"
                );
                return;
            }
        };
        let Some(payload) = scratch.get(..len) else {
            return;
        };
        let members = self.clients.members_of_module(event.module).await;
        for peer in members {
            if peer == event.source_peer {
                // Defensive: the source peer should be on a
                // different protocol's pool, not this one, but the
                // check is cheap.
                continue;
            }
            if let Err(e) = socket.send_to(payload, peer).await {
                tracing::warn!(
                    ?peer,
                    ?e,
                    "cross-protocol send failed; marking peer unhealthy"
                );
                if let UnhealthyOutcome::ShouldEvict { failure_count } =
                    self.clients.mark_unhealthy(&peer).await
                {
                    tracing::warn!(
                        ?peer,
                        failure_count,
                        "cross-protocol send failure threshold exceeded; evicting peer"
                    );
                    self.evict_peer(peer, "too many cross-protocol send failures")
                        .await;
                }
            }
        }
    }

    /// Process one received datagram through the full pipeline:
    /// `handle_inbound` → reply `send_to` → fan-out → eviction.
    ///
    /// Extracted from [`Self::run`] to keep the top-level run loop
    /// readable and within clippy's cognitive complexity budget.
    async fn run_one_tick(
        self: &Arc<Self>,
        socket: &Arc<UdpSocket>,
        owned_bytes: &[u8],
        peer: SocketAddr,
        now: Instant,
    ) -> Result<(), ShellError> {
        let outcome = match self.handle_inbound(owned_bytes, peer, now).await {
            Ok(o) => o,
            Err(ShellError::Core(e)) => {
                tracing::warn!(?peer, ?e, "dropping malformed datagram");
                return Ok(());
            }
            Err(ShellError::Protocol(msg)) => {
                tracing::warn!(?peer, msg, "protocol not supported");
                return Ok(());
            }
            Err(e @ ShellError::Io(_)) => return Err(e),
        };

        let mut evicted_peers: Vec<SocketAddr> = Vec::new();
        self.send_replies(socket, &outcome.txs, &mut evicted_peers)
            .await;
        self.fan_out_outcome(socket, &outcome, peer, owned_bytes, &mut evicted_peers)
            .await;

        // Fix 4: Remove any peers whose send-failure count crossed
        // the eviction threshold on this tick. The ClientEvicted
        // event itself is emitted by `evict_peer` so consumers of
        // the server event stream can observe the eviction.
        for evicted in evicted_peers {
            self.evict_peer(evicted, "too many send failures").await;
        }
        Ok(())
    }

    /// Send all reply datagrams from `outcome.txs`, marking peers
    /// unhealthy on send failure and collecting any that cross the
    /// eviction threshold into `evicted_peers`.
    async fn send_replies(
        self: &Arc<Self>,
        socket: &Arc<UdpSocket>,
        txs: &[(Vec<u8>, SocketAddr)],
        evicted_peers: &mut Vec<SocketAddr>,
    ) {
        for (payload, dst) in txs {
            if let Err(e) = socket.send_to(payload, *dst).await {
                tracing::warn!(?dst, ?e, "reply send_to failed");
                if let UnhealthyOutcome::ShouldEvict { failure_count } =
                    self.clients.mark_unhealthy(dst).await
                {
                    tracing::warn!(
                        ?dst,
                        failure_count,
                        "reply send failure threshold exceeded; evicting peer"
                    );
                    evicted_peers.push(*dst);
                }
            }
        }
    }

    /// Fan out the received datagram (and any cached header
    /// retransmit) to every other peer on the same module.
    async fn fan_out_outcome(
        self: &Arc<Self>,
        socket: &Arc<UdpSocket>,
        outcome: &EndpointOutcome<P>,
        peer: SocketAddr,
        owned_bytes: &[u8],
        evicted_peers: &mut Vec<SocketAddr>,
    ) {
        let peer_module = self.clients.module_of(&peer).await;
        let Some(hint) = Self::forward_hint(&outcome.events, peer_module) else {
            return;
        };
        let (module, _stream_id) = match hint {
            ForwardHint::Header { module, stream_id }
            | ForwardHint::Data { module, stream_id }
            | ForwardHint::Eot { module, stream_id } => (module, stream_id),
        };
        match fan_out_voice(
            socket.as_ref(),
            &self.clients,
            peer,
            module,
            self.protocol,
            owned_bytes,
        )
        .await
        {
            Ok(report) => evicted_peers.extend(report.evicted),
            Err(e) => tracing::warn!(?peer, ?e, "fan_out_voice failed"),
        }
        // Fix 3: If the stream cache fired a header retransmit on
        // this tick, fan out the cached bytes alongside the normal
        // frame. We send the data frame FIRST (above) and the cached
        // header SECOND so listeners who missed the initial header
        // still get refreshed context immediately after decoding the
        // data.
        if let Some(cached) = outcome.header_retransmit.as_ref() {
            match fan_out_voice(
                socket.as_ref(),
                &self.clients,
                peer,
                module,
                self.protocol,
                cached,
            )
            .await
            {
                Ok(report) => evicted_peers.extend(report.evicted),
                Err(e) => tracing::warn!(?peer, ?e, "fan_out_voice header retransmit failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::indexing_slicing,
        reason = "Tests slice fixed-size voice frames and protocol fixtures where bounds \
                  are obvious by construction; out-of-range access correctly fails the test."
    )]

    use super::{EndpointOutcome, ProtocolEndpoint};
    use crate::reflector::{
        AllowAllAuthorizer, ClientAuthorizer, DenyAllAuthorizer, ReadOnlyAuthorizer,
    };
    use dstar_gateway_core::codec::dcs::{
        GatewayType as DcsGatewayType, encode_connect_link as encode_dcs_link,
        encode_voice as encode_dcs_voice,
    };
    use dstar_gateway_core::codec::dextra::{
        encode_connect_link, encode_voice_data, encode_voice_eot, encode_voice_header,
    };
    use dstar_gateway_core::codec::dplus::{
        encode_link1 as encode_dplus_link1, encode_link2 as encode_dplus_link2,
        encode_voice_data as encode_dplus_voice_data, encode_voice_eot as encode_dplus_voice_eot,
        encode_voice_header as encode_dplus_voice_header,
    };
    use dstar_gateway_core::header::DStarHeader;
    use dstar_gateway_core::session::client::{DExtra, DPlus, Dcs};
    use dstar_gateway_core::session::server::{ServerEvent, ServerStateKind};
    use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
    use dstar_gateway_core::voice::VoiceFrame;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Instant;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn peer() -> SocketAddr {
        PEER
    }

    fn allow_all() -> Arc<dyn ClientAuthorizer> {
        Arc::new(AllowAllAuthorizer)
    }

    #[tokio::test]
    async fn new_endpoint_has_empty_pool() {
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        assert_eq!(ep.protocol_kind(), ProtocolKind::DExtra);
        assert_eq!(ep.clients().len().await, 0);
    }

    #[tokio::test]
    async fn dextra_link_produces_ack_and_event() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let mut buf = [0u8; 16];
        let n = encode_connect_link(
            &mut buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let slice = buf.get(..n).ok_or("encode produced no bytes")?;

        let outcome: EndpointOutcome<DExtra> =
            ep.handle_inbound(slice, peer(), Instant::now()).await?;

        // Exactly one outbound ACK to the same peer. The ACK tag
        // offset is asserted by the codec's own golden tests — we
        // just verify one 14-byte datagram was enqueued to the peer
        // and contains the ACK tag somewhere in the payload.
        assert_eq!(outcome.txs.len(), 1);
        let (payload, dst) = outcome.txs.first().ok_or("no tx")?;
        assert_eq!(*dst, peer());
        assert_eq!(payload.len(), 14, "DExtra ACK is 14 bytes");
        assert!(
            payload.windows(3).any(|w| w == b"ACK"),
            "payload must contain ACK tag"
        );

        // Exactly one ClientLinked event.
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::ClientLinked { .. })
        ));

        // Pool now has one entry and has the reverse-index populated.
        assert_eq!(ep.clients().len().await, 1);
        let members = ep.clients().members_of_module(Module::C).await;
        assert_eq!(members, vec![peer()]);
        Ok(())
    }

    // ─── DPlus handshake ─────────────────────────────────────────
    #[tokio::test]
    async fn dplus_link2_after_link1_creates_handle_and_acks_okrw() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());

        // LINK1 — 5 bytes, no callsign. The core transitions to
        // `Link1Received` and enqueues the 5-byte ACK echo.
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        let link1_slice = link1_buf.get(..n1).ok_or("link1 empty")?;
        let outcome1: EndpointOutcome<DPlus> = ep
            .handle_inbound(link1_slice, peer(), Instant::now())
            .await?;
        // LINK1 ACK echo is 5 bytes back to the peer.
        assert_eq!(outcome1.txs.len(), 1);
        let (payload1, dst1) = outcome1.txs.first().ok_or("no tx")?;
        assert_eq!(*dst1, peer());
        assert_eq!(payload1.len(), 5, "DPlus LINK1 ACK is 5 bytes");
        // LINK1 does not emit a ClientLinked event — the login isn't
        // complete until LINK2 arrives with the callsign.
        assert!(
            outcome1.events.is_empty(),
            "LINK1 emits no events (no callsign yet)"
        );
        // A handle was created (needed to carry the Link1Received state).
        assert_eq!(ep.clients().len().await, 1);

        // LINK2 — 28 bytes carrying the callsign. The core fires
        // `ClientLinked` with the fallback reflector module (DPlus
        // LINK2 carries no module), and enqueues the 8-byte OKRW reply.
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        let link2_slice = link2_buf.get(..n2).ok_or("link2 empty")?;
        let outcome2: EndpointOutcome<DPlus> = ep
            .handle_inbound(link2_slice, peer(), Instant::now())
            .await?;
        assert_eq!(outcome2.txs.len(), 1);
        let (payload2, dst2) = outcome2.txs.first().ok_or("no tx")?;
        assert_eq!(*dst2, peer());
        assert_eq!(payload2.len(), 8, "DPlus LINK2 reply is 8 bytes");
        assert!(
            payload2.windows(4).any(|w| w == b"OKRW"),
            "LINK2 ACCEPT reply contains OKRW tag"
        );
        assert_eq!(outcome2.events.len(), 1);
        assert!(matches!(
            outcome2.events.first(),
            Some(ServerEvent::ClientLinked { .. })
        ));
        let members = ep.clients().members_of_module(Module::C).await;
        assert_eq!(members, vec![peer()]);
        Ok(())
    }

    #[tokio::test]
    async fn dplus_voice_header_during_linked_creates_stream_cache() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());
        // LINK1 + LINK2 to establish the session.
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        drop(
            ep.handle_inbound(link1_buf.get(..n1).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        drop(
            ep.handle_inbound(link2_buf.get(..n2).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );

        // Voice header — 58 bytes.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_dplus_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        let hdr_slice = hdr_buf.get(..hdr_n).ok_or("empty")?;
        let outcome = ep.handle_inbound(hdr_slice, peer(), Instant::now()).await?;
        // ClientStreamStarted event must be present.
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientStreamStarted { .. })),
            "voice header emits ClientStreamStarted"
        );
        // Header tick itself does not retransmit.
        assert!(outcome.header_retransmit.is_none());

        // Send 20 data frames to trip the retransmit cadence.
        let frame = VoiceFrame::silence();
        let mut cache_fired = 0_u32;
        for seq in 0_u8..20 {
            let mut data_buf = [0u8; 64];
            let data_n = encode_dplus_voice_data(&mut data_buf, sid(), seq, &frame)?;
            let data_slice = data_buf.get(..data_n).ok_or("empty")?;
            let outcome = ep
                .handle_inbound(data_slice, peer(), Instant::now())
                .await?;
            if outcome.header_retransmit.is_some() {
                cache_fired = cache_fired.saturating_add(1);
            }
        }
        assert_eq!(
            cache_fired, 1,
            "DPlus stream cache retransmits after 20 frames"
        );

        // EOT clears the cache.
        let mut eot_buf = [0u8; 64];
        let eot_n = encode_dplus_voice_eot(&mut eot_buf, sid(), 20)?;
        let eot_slice = eot_buf.get(..eot_n).ok_or("empty")?;
        drop(ep.handle_inbound(eot_slice, peer(), Instant::now()).await?);
        Ok(())
    }

    // ─── DCS handshake ───────────────────────────────────────────
    fn dcs_reflector_cs() -> Callsign {
        Callsign::from_wire_bytes(*b"DCS030  ")
    }

    #[tokio::test]
    async fn dcs_link_creates_handle_and_acks() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());
        // DCS LINK is 519 bytes.
        let mut buf = vec![0u8; 600];
        let n = encode_dcs_link(
            &mut buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Module::C,
            &dcs_reflector_cs(),
            DcsGatewayType::Repeater,
        )?;
        let slice = buf.get(..n).ok_or("empty")?;

        let outcome: EndpointOutcome<Dcs> =
            ep.handle_inbound(slice, peer(), Instant::now()).await?;
        // Exactly one 14-byte ACK datagram.
        assert_eq!(outcome.txs.len(), 1);
        let (payload, dst) = outcome.txs.first().ok_or("no tx")?;
        assert_eq!(*dst, peer());
        assert_eq!(payload.len(), 14, "DCS ACK is 14 bytes");
        assert!(
            payload.windows(3).any(|w| w == b"ACK"),
            "DCS ACK payload contains ACK tag"
        );
        // ClientLinked event present.
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientLinked { .. })),
            "DCS link emits ClientLinked"
        );
        // Pool has one member on module C.
        assert_eq!(ep.clients().len().await, 1);
        let members = ep.clients().members_of_module(Module::C).await;
        assert_eq!(members, vec![peer()]);
        Ok(())
    }

    #[tokio::test]
    async fn dcs_voice_first_packet_starts_stream_and_caches_header() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());
        // LINK first.
        let mut link_buf = vec![0u8; 600];
        let link_n = encode_dcs_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Module::C,
            &dcs_reflector_cs(),
            DcsGatewayType::Repeater,
        )?;
        drop(
            ep.handle_inbound(
                link_buf.get(..link_n).ok_or("empty")?,
                peer(),
                Instant::now(),
            )
            .await?,
        );

        // DCS voice is 100 bytes — first packet for a new stream id
        // is the implicit "header". DCS carries the header in every
        // voice frame so there's no separate VoiceHeader packet type.
        let frame = VoiceFrame::silence();
        let mut voice_buf = [0u8; 128];
        let voice_n = encode_dcs_voice(
            &mut voice_buf,
            &test_header("W1AW"),
            sid(),
            0,
            &frame,
            false,
        )?;
        let voice_slice = voice_buf.get(..voice_n).ok_or("empty")?;
        let outcome = ep
            .handle_inbound(voice_slice, peer(), Instant::now())
            .await?;
        // Both ClientStreamStarted (new stream id) and
        // ClientStreamFrame (the frame itself) are present.
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientStreamStarted { .. })),
            "first DCS voice packet emits ClientStreamStarted"
        );
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientStreamFrame { .. })),
            "first DCS voice packet also emits ClientStreamFrame"
        );
        // First packet never triggers retransmit cadence.
        assert!(outcome.header_retransmit.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn dcs_voice_with_is_end_clears_stream_cache() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());
        // LINK first.
        let mut link_buf = vec![0u8; 600];
        let link_n = encode_dcs_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Module::C,
            &dcs_reflector_cs(),
            DcsGatewayType::Repeater,
        )?;
        drop(
            ep.handle_inbound(
                link_buf.get(..link_n).ok_or("empty")?,
                peer(),
                Instant::now(),
            )
            .await?,
        );

        // Send a voice header-ish packet (first of a new stream).
        let frame = VoiceFrame::silence();
        let mut voice_buf = [0u8; 128];
        let voice_n = encode_dcs_voice(
            &mut voice_buf,
            &test_header("W1AW"),
            sid(),
            0,
            &frame,
            false,
        )?;
        drop(
            ep.handle_inbound(
                voice_buf.get(..voice_n).ok_or("empty")?,
                peer(),
                Instant::now(),
            )
            .await?,
        );

        // Now send the end-of-stream packet.
        let mut eot_buf = [0u8; 128];
        let eot_n = encode_dcs_voice(
            &mut eot_buf,
            &test_header("W1AW"),
            sid(),
            1,
            &frame,
            true, // is_end
        )?;
        drop(
            ep.handle_inbound(eot_buf.get(..eot_n).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );

        // Now start a NEW stream with a different stream id — this
        // must behave as a fresh stream (new ClientStreamStarted
        // event), which can only happen if the DCS EOT cleared the
        // cache on the previous tick.
        let Some(new_sid) = StreamId::new(0x9999) else {
            unreachable!()
        };
        let mut fresh_buf = [0u8; 128];
        let fresh_n = encode_dcs_voice(
            &mut fresh_buf,
            &test_header("W1AW"),
            new_sid,
            0,
            &frame,
            false,
        )?;
        let fresh_outcome = ep
            .handle_inbound(
                fresh_buf.get(..fresh_n).ok_or("empty")?,
                peer(),
                Instant::now(),
            )
            .await?;
        assert!(
            fresh_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientStreamStarted { .. })),
            "fresh stream after is_end must emit ClientStreamStarted"
        );
        Ok(())
    }

    fn test_header(cs_my: &str) -> DStarHeader {
        let mut my_bytes = *b"        ";
        let src = cs_my.as_bytes();
        let len = src.len().min(8);
        my_bytes[..len].copy_from_slice(&src[..len]);
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"REF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"REF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(my_bytes),
            my_suffix: Suffix::EMPTY,
        }
    }

    const fn sid() -> StreamId {
        match StreamId::new(0x4242) {
            Some(s) => s,
            None => unreachable!(),
        }
    }

    // ─── Fix 1: DenyAllAuthorizer path ────────────────────────────
    #[tokio::test]
    async fn dextra_link_rejected_by_deny_all_authorizer() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new(
            ProtocolKind::DExtra,
            Module::C,
            Arc::new(DenyAllAuthorizer),
        );
        let mut buf = [0u8; 16];
        let n = encode_connect_link(
            &mut buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let slice = buf.get(..n).ok_or("empty")?;

        let outcome: EndpointOutcome<DExtra> =
            ep.handle_inbound(slice, peer(), Instant::now()).await?;

        // The pool must be empty — no handle was created.
        assert_eq!(
            ep.clients().len().await,
            0,
            "rejected peer must not be in pool"
        );

        // Exactly one outbound NAK to the same peer. The NAK tag
        // position is asserted by the codec's own golden tests — we
        // just verify one 14-byte datagram was enqueued to the peer
        // that tried to link.
        assert_eq!(outcome.txs.len(), 1);
        let (payload, dst) = outcome.txs.first().ok_or("no tx")?;
        assert_eq!(*dst, peer());
        assert_eq!(payload.len(), 14, "DExtra NAK is 14 bytes");

        // Exactly one ClientRejected event.
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::ClientRejected { .. })
        ));
        Ok(())
    }

    // ─── Fix 3: StreamCache 21-frame header retransmit ────────────
    #[tokio::test]
    async fn dextra_stream_cache_retransmits_header_every_21_frames() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());

        // LINK first so the peer has a module assignment.
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let link_slice = link_buf.get(..n).ok_or("empty")?;
        drop(
            ep.handle_inbound(link_slice, peer(), Instant::now())
                .await?,
        );

        // Voice header.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        let hdr_slice = hdr_buf.get(..hdr_n).ok_or("empty")?;
        let hdr_outcome = ep.handle_inbound(hdr_slice, peer(), Instant::now()).await?;
        // The header tick itself does NOT trigger a retransmit —
        // the first retransmit fires after 20 data frames.
        assert!(
            hdr_outcome.header_retransmit.is_none(),
            "header tick must not trigger retransmit",
        );

        // Send 20 voice data frames; the 20th (seq_counter=20 after
        // bump) fires the retransmit boundary.
        let frame = VoiceFrame::silence();
        let mut cache_fired = 0_u32;
        for seq in 0_u8..20 {
            let mut data_buf = [0u8; 64];
            let data_n = encode_voice_data(&mut data_buf, sid(), seq, &frame)?;
            let data_slice = data_buf.get(..data_n).ok_or("empty")?;
            let outcome = ep
                .handle_inbound(data_slice, peer(), Instant::now())
                .await?;
            if outcome.header_retransmit.is_some() {
                cache_fired = cache_fired.saturating_add(1);
            }
        }
        assert_eq!(cache_fired, 1, "one header retransmit after 20 data frames");

        // Voice EOT — clears the cache.
        let mut eot_buf = [0u8; 64];
        let eot_n = encode_voice_eot(&mut eot_buf, sid(), 20)?;
        let eot_slice = eot_buf.get(..eot_n).ok_or("empty")?;
        drop(ep.handle_inbound(eot_slice, peer(), Instant::now()).await?);

        // The stream cache is empty — subsequent data frames from the
        // same peer (without a fresh header) must not produce a
        // retransmit.
        let mut stale_buf = [0u8; 64];
        let stale_n = encode_voice_data(&mut stale_buf, sid(), 99, &frame)?;
        let stale_slice = stale_buf.get(..stale_n).ok_or("empty")?;
        let stale_outcome = ep
            .handle_inbound(stale_slice, peer(), Instant::now())
            .await?;
        assert!(
            stale_outcome.header_retransmit.is_none(),
            "EOT must clear the cache",
        );
        Ok(())
    }

    // ─── Fix 4: ClientEvicted event path ──────────────────────────
    #[tokio::test]
    async fn dextra_endpoint_surfaces_evict_peer_event_next_tick() -> TestResult {
        // evict_peer is an async helper; we exercise it directly to
        // confirm the event queues and drains correctly on the next
        // handle_inbound call. The real wire trigger comes via the
        // run loop when fan_out_voice reports ShouldEvict.
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        // LINK a peer first so it has a pool entry to be evicted.
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let link_slice = link_buf.get(..n).ok_or("empty")?;
        drop(
            ep.handle_inbound(link_slice, peer(), Instant::now())
                .await?,
        );
        assert_eq!(ep.clients().len().await, 1);

        // Evict the peer out-of-band.
        ep.evict_peer(peer(), "test eviction").await;
        assert_eq!(ep.clients().len().await, 0, "peer removed");

        // A subsequent LINK from a NEW peer on the same port surfaces
        // the queued ClientEvicted event from the previous eviction
        // in its outcome. We use peer() again — the previous handle
        // is gone, so this counts as a fresh LINK.
        let outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;

        // The relink produced its own ClientLinked event, AND the
        // queued ClientEvicted event from the prior tick.
        let events = &outcome.events;
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientLinked { .. })),
            "fresh link still emits ClientLinked"
        );
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientEvicted { .. })),
            "queued ClientEvicted drains on next tick"
        );
        Ok(())
    }

    // ─── Voice bus publish path ───────────────────────────────────
    #[tokio::test]
    async fn dextra_voice_events_publish_to_voice_bus() -> TestResult {
        use tokio::sync::broadcast;
        let (tx, mut rx) = broadcast::channel::<super::CrossProtocolEvent>(32);
        let ep = ProtocolEndpoint::<DExtra>::new_with_voice_bus(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            Some(tx),
        );
        // LINK so the peer has a module assignment in the pool.
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let link_slice = link_buf.get(..n).ok_or("empty")?;
        drop(
            ep.handle_inbound(link_slice, peer(), Instant::now())
                .await?,
        );
        // LINK itself emits ClientLinked (not voice) — nothing on bus yet.
        assert!(
            rx.try_recv().is_err(),
            "LINK emits no cross-protocol events"
        );

        // Voice header — should produce exactly one StreamStart on the bus.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        let hdr_slice = hdr_buf.get(..hdr_n).ok_or("empty")?;
        drop(ep.handle_inbound(hdr_slice, peer(), Instant::now()).await?);
        let event = rx.try_recv()?;
        assert_eq!(event.source_protocol, ProtocolKind::DExtra);
        assert_eq!(event.source_peer, peer());
        assert_eq!(event.module, Module::C);
        assert!(matches!(event.event, super::VoiceEvent::StreamStart { .. }));

        // Voice data — should produce one Frame on the bus with cached header.
        let frame = VoiceFrame::silence();
        let mut data_buf = [0u8; 64];
        let data_n = encode_voice_data(&mut data_buf, sid(), 1, &frame)?;
        let data_slice = data_buf.get(..data_n).ok_or("empty")?;
        drop(
            ep.handle_inbound(data_slice, peer(), Instant::now())
                .await?,
        );
        let event = rx.try_recv()?;
        assert!(matches!(event.event, super::VoiceEvent::Frame { .. }));
        assert!(
            event.cached_header.is_some(),
            "voice data frame carries cached header"
        );

        // Voice EOT — should produce one StreamEnd on the bus.
        let mut eot_buf = [0u8; 64];
        let eot_n = encode_voice_eot(&mut eot_buf, sid(), 1)?;
        let eot_slice = eot_buf.get(..eot_n).ok_or("empty")?;
        drop(ep.handle_inbound(eot_slice, peer(), Instant::now()).await?);
        let event = rx.try_recv()?;
        assert!(matches!(event.event, super::VoiceEvent::StreamEnd { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn no_voice_bus_means_no_publish() -> TestResult {
        // Sanity check: an endpoint constructed without a voice bus
        // MUST NOT attempt to publish (ergo, voice_bus field is None,
        // and handle_inbound's publish_voice_events helper is a no-op).
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        drop(
            ep.handle_inbound(link_buf.get(..n).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        // This must not panic and must not error — publish_voice_events
        // is a silent no-op when voice_bus is None.
        drop(
            ep.handle_inbound(hdr_buf.get(..hdr_n).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );
        Ok(())
    }

    // ─── Fix 2: ReadOnly voice drop path ──────────────────────────
    #[tokio::test]
    async fn dextra_readonly_voice_header_is_dropped() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new(
            ProtocolKind::DExtra,
            Module::C,
            Arc::new(ReadOnlyAuthorizer),
        );
        // First LINK so the peer is admitted with ReadOnly access.
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let link_slice = link_buf.get(..n).ok_or("empty")?;
        let link_outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;
        assert_eq!(ep.clients().len().await, 1, "peer admitted as read-only");
        // The link itself still produced an ACK + ClientLinked event.
        assert_eq!(link_outcome.txs.len(), 1);
        assert_eq!(link_outcome.events.len(), 1);

        // Now send a voice header from the read-only peer.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        let hdr_slice = hdr_buf.get(..hdr_n).ok_or("empty")?;

        let voice_outcome: EndpointOutcome<DExtra> =
            ep.handle_inbound(hdr_slice, peer(), Instant::now()).await?;

        // No fan-out side-effects: zero outbound txs for the voice
        // header (the pool is size-1 anyway so even ReadWrite would
        // produce no fan-out, but we also verify the state below).
        assert!(
            voice_outcome.txs.is_empty(),
            "read-only voice must not emit any outbound datagrams"
        );

        // Exactly one VoiceFromReadOnlyDropped event is surfaced.
        assert_eq!(voice_outcome.events.len(), 1);
        assert!(matches!(
            voice_outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));

        // The server session MUST still be in Linked state — the
        // voice header must NOT have transitioned it to Streaming.
        let state = ep
            .clients()
            .with_handle_mut(&peer(), |h| h.session.state_kind())
            .await
            .ok_or("handle not found")?;
        assert_eq!(
            state,
            ServerStateKind::Linked,
            "read-only voice must not push session into Streaming"
        );
        Ok(())
    }
}
