//! `ProtocolEndpoint<P>`: per-protocol reflector RX shell.
//!
//! Holds the client pool, active stream cache, and protocol
//! discriminator for one of the three D-STAR reflector protocols.
//! [`ProtocolEndpoint::handle_inbound`] is the sans-io entry point;
//! [`ProtocolEndpoint::run`] is the UDP pump plus the voice fan-out path.

use std::collections::{HashMap, HashSet, VecDeque};
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::sync::{broadcast, watch};

use dstar_gateway_core::ServerSessionCore;
use dstar_gateway_core::codec::dcs::{
    ClientPacket as DcsClientPacket, decode_client_to_server as decode_dcs_client_to_server,
    encode_connect_nak as encode_dcs_connect_nak,
};
use dstar_gateway_core::codec::dextra::{
    ClientPacket, decode_client_to_server, encode_connect_nak, encode_poll as encode_dextra_poll,
};
use dstar_gateway_core::codec::dplus::{
    ClientPacket as DPlusClientPacket, Link2Result,
    decode_client_to_server as decode_dplus_client_to_server, encode_link2_reply,
    encode_poll_echo as encode_dplus_poll_echo,
};
use dstar_gateway_core::error::Error as CoreError;
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::Protocol;
use dstar_gateway_core::session::server::ServerEvent;
use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId};
use dstar_gateway_core::validator::NullSink;

use crate::client_pool::{ClientHandle, ClientPool, SweepEntry, UnhealthyOutcome};
use crate::reflector::{
    AccessPolicy, ClientAuthorizer, LinkAttempt, ReflectorConfig, RejectReason, StreamCache,
};
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
    /// Outbound datagrams: each `(bytes, destination)`.
    pub txs: Vec<(Vec<u8>, SocketAddr)>,
    /// Consumer-visible server events.
    pub events: Vec<ServerEvent<P>>,
    /// Cached voice-header bytes to rebroadcast to the rest of the
    /// module on this tick.
    ///
    /// Populated by the stream cache every 21 voice frames to match
    /// the `xlxd` / `MMDVMHost` cadence; the run loop fans these
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
/// carry the final seq; downstream encoders OR the 0x40 end-bit in
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

/// Shell-level tunables for one [`ProtocolEndpoint`].
///
/// [`crate::reflector::Reflector`] derives these from its
/// [`ReflectorConfig`] via [`From`]; the plain constructors
/// ([`ProtocolEndpoint::new`] / [`ProtocolEndpoint::new_with_voice_bus`])
/// fall back to [`EndpointSettings::default`], which mirrors the
/// `ReflectorConfig` builder defaults.
#[derive(Debug, Clone)]
pub struct EndpointSettings {
    /// Reflector callsign, used in server-initiated keepalives
    /// (the `DExtra` and `DCS` keepalive wire forms carry it).
    ///
    /// Defaults to `NOCALL`, a placeholder that only becomes
    /// visible on the wire if an endpoint is constructed without a
    /// reflector identity, which production paths never do.
    pub reflector_callsign: Callsign,
    /// Per-client TX rate limit in voice frames per second.
    ///
    /// Applied to every new [`ClientHandle`]'s token bucket: refill
    /// runs at this rate and the burst capacity is one second of
    /// traffic at this rate (see [`crate::client_pool::TokenBucket::from_rate`]).
    pub tx_rate_limit_frames_per_sec: f64,
    /// Maximum clients across all modules.
    ///
    /// A LINK attempt from a NEW address when the pool already holds
    /// this many entries is rejected with the protocol's reject
    /// reply and a [`ServerEvent::ClientRejected`] event
    /// ([`crate::reflector::RejectReason::Busy`]).
    pub max_total_clients: usize,
    /// Maximum clients per module.
    ///
    /// A LINK attempt that would make a module's membership exceed
    /// this count is rejected with the protocol's reject reply
    /// ([`crate::reflector::RejectReason::MaxClients`]).
    pub max_clients_per_module: usize,
    /// Interval between server-initiated keepalives.
    ///
    /// Each maintenance sweep (driven on this interval by
    /// [`ProtocolEndpoint::run`]) that observes at least this much
    /// time since the previous keepalive send emits one
    /// protocol-appropriate keepalive per linked client.
    pub keepalive_interval: Duration,
    /// Idle window after which a silent client is evicted.
    ///
    /// The sweep evicts every pool entry whose `last_heard` is at
    /// least this old (any inbound datagram from the peer refreshes
    /// it).
    pub keepalive_inactivity_timeout: Duration,
    /// Idle window after which a stalled voice stream's cache entry
    /// is dropped.
    ///
    /// The sweep removes stream-cache entries via
    /// [`StreamCache::should_evict`], freeing the module's cache for
    /// the next talker when a stream dies without an EOT.
    pub voice_inactivity_timeout: Duration,
}

impl Default for EndpointSettings {
    /// Mirrors the [`ReflectorConfig`] builder defaults (with the
    /// `NOCALL` placeholder callsign; the config builder has no
    /// callsign default).
    fn default() -> Self {
        Self {
            reflector_callsign: Callsign::from_wire_bytes(*b"NOCALL  "),
            tx_rate_limit_frames_per_sec: 60.0,
            max_total_clients: 250,
            max_clients_per_module: 50,
            keepalive_interval: Duration::from_secs(1),
            keepalive_inactivity_timeout: Duration::from_secs(30),
            voice_inactivity_timeout: Duration::from_secs(2),
        }
    }
}

impl From<&ReflectorConfig> for EndpointSettings {
    fn from(config: &ReflectorConfig) -> Self {
        Self {
            reflector_callsign: config.callsign,
            tx_rate_limit_frames_per_sec: config.tx_rate_limit_frames_per_sec,
            max_total_clients: config.max_total_clients,
            max_clients_per_module: config.max_clients_per_module,
            keepalive_interval: config.keepalive_interval,
            keepalive_inactivity_timeout: config.keepalive_inactivity_timeout,
            voice_inactivity_timeout: config.voice_inactivity_timeout,
        }
    }
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
    /// Modules admitted by this endpoint.
    configured_modules: HashSet<Module>,
    /// Per-module active stream cache: populated on voice header,
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
    /// Cross-protocol voice bus: `Some` iff the reflector was
    /// constructed with `cross_protocol_forwarding = true`. Published
    /// to after each inbound voice event so other protocols'
    /// endpoints can transcode and fan out the frame on their own
    /// wire format.
    voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
    /// Shell-level tunables (rate limit, client caps, keepalive and
    /// inactivity windows); see [`EndpointSettings`].
    settings: EndpointSettings,
    /// Instant of the most recent [`Self::handle_tick`] sweep that
    /// sent keepalives; `None` until the first sweep (which always
    /// sends).
    last_keepalive_sent: Mutex<Option<Instant>>,
    _protocol: PhantomData<fn() -> P>,
}

impl<P: Protocol> std::fmt::Debug for ProtocolEndpoint<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `ClientPool<P>` and the stream cache map aren't printed:
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
    /// Pass `None` to disable cross-protocol participation on this
    /// endpoint. Uses [`EndpointSettings::default`]; see
    /// [`Self::new_with_settings`] to tune the shell-level knobs.
    #[must_use]
    pub fn new_with_voice_bus(
        protocol: ProtocolKind,
        default_reflector_module: Module,
        authorizer: Arc<dyn ClientAuthorizer>,
        voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
    ) -> Self {
        Self::new_with_settings(
            protocol,
            default_reflector_module,
            authorizer,
            voice_bus,
            EndpointSettings::default(),
            HashSet::from([default_reflector_module]),
        )
    }

    /// Construct a new endpoint with explicit [`EndpointSettings`].
    ///
    /// This is the constructor [`crate::reflector::Reflector`] uses:
    /// it derives the settings from its [`ReflectorConfig`] so the
    /// configured rate limit, client caps, keepalive interval, and
    /// inactivity timeouts actually govern the endpoint's behavior.
    /// `configured_modules` is the closed set admitted by `DExtra` and
    /// `DCS` LINK packets.
    ///
    /// # Panics
    ///
    /// Panics if `default_reflector_module` is absent from
    /// `configured_modules`.
    #[must_use]
    pub fn new_with_settings(
        protocol: ProtocolKind,
        default_reflector_module: Module,
        authorizer: Arc<dyn ClientAuthorizer>,
        voice_bus: Option<broadcast::Sender<CrossProtocolEvent>>,
        settings: EndpointSettings,
        configured_modules: HashSet<Module>,
    ) -> Self {
        assert!(
            configured_modules.contains(&default_reflector_module),
            "default reflector module must be configured"
        );
        Self {
            protocol,
            clients: ClientPool::<P>::new(),
            default_reflector_module,
            configured_modules,
            stream_cache: Mutex::new(HashMap::new()),
            authorizer,
            pending_events: Mutex::new(VecDeque::new()),
            voice_bus,
            settings,
            last_keepalive_sent: Mutex::new(None),
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
    /// packet, drops non-link traffic from unknown peers (session
    /// state is only allocated for a valid LINK attempt), consults
    /// the authorizer on LINK attempts, gates voice-stream ingress on
    /// [`AccessPolicy`], drives the core via the private `drive_core`
    /// helper, then updates the per-module stream cache and drains
    /// pending background events into the outcome.
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
    /// core drive → `set_module` / `remove` → `record_last_heard`) and cancellation
    /// between any two awaits can leave the pool in a half-updated
    /// state where a session has been created but not yet attached to
    /// its module in the reverse index. The reflector's run loop is
    /// the only expected caller and it never cancels this future
    /// except via shutdown.
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
    /// drives the core, mirrors `ClientLinked` / `ClientUnlinked`
    /// transitions into the pool (module reverse index update / entry
    /// removal), and maintains the per-module stream cache.
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

        // Unknown peers only earn session state via a valid LINK
        // attempt. Garbage (or well-formed non-link traffic that the
        // core would ignore from the `Unknown` state anyway) must not
        // allocate a `ClientHandle`; otherwise every port-scan
        // datagram grows the pool without bound.
        if !self.clients.contains(&peer).await
            && !matches!(pre_decoded, Some(ClientPacket::Link { .. }))
        {
            tracing::debug!(?peer, "dropping non-link datagram from unknown DExtra peer");
            return Ok(EndpointOutcome::<P>::empty());
        }

        // LINK → configured-module, authorizer, and capacity gates.
        // Rejected attempts never materialize a ClientHandle; they
        // produce a NAK + `ClientRejected` event.
        let link_access: Option<AccessPolicy> = if let Some(ClientPacket::Link {
            callsign,
            reflector_module,
            ..
        }) = pre_decoded.clone()
        {
            match self.authorize_link(peer, callsign, reflector_module).await {
                Ok(access_policy) => Some(access_policy),
                Err(reject) => {
                    tracing::info!(
                        ?peer,
                        %callsign,
                        %reflector_module,
                        reason = ?reject,
                        "DExtra LINK attempt rejected"
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

        let created = self.ensure_handle(peer, link_access, now).await;

        // ReadOnly voice drop check: must happen BEFORE drive_core
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

        let mut outcome = self
            .drive_core_for_handle(peer, bytes, now, created, link_access)
            .await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_link_events(&outcome, peer).await;

        // Snapshot the header that was live for this module BEFORE
        // the stream-cache update: the EOT path removes the cache
        // entry, and the cross-protocol StreamEnd publish still needs
        // the header (DCS re-encoders embed it in every packet,
        // including the end frame).
        let pre_update_header = self.module_cached_header_of_peer(peer).await;
        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit =
                self.update_stream_cache_dextra(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer, pre_update_header)
            .await;
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

        // Unknown peers only earn session state via LINK1, the
        // opening packet of the `DPlus` handshake. Anything else from
        // an unknown address (garbage, voice, or a LINK2 that the
        // core would drop for lack of a preceding LINK1) must not
        // allocate a `ClientHandle`.
        if !self.clients.contains(&peer).await {
            if !matches!(pre_decoded, Some(DPlusClientPacket::Link1)) {
                tracing::debug!(?peer, "dropping non-link datagram from unknown DPlus peer");
                return Ok(EndpointOutcome::<P>::empty());
            }
            // `DPlus` allocates its pool entry at LINK1 (the packet
            // carries no callsign, so the authorizer can't run until
            // LINK2); enforce the total-client cap here, where the
            // entry would be created.
            if self.clients.len().await >= self.settings.max_total_clients {
                tracing::info!(?peer, "client capacity limit rejected DPlus LINK1 attempt");
                return Ok(Self::build_dplus_reject_outcome(peer, RejectReason::Busy));
            }
        }

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
                    Ok(access_policy) => {
                        // Per-module capacity gate at the LINK2
                        // verdict. The peer already holds a
                        // transitional LINK1 handle (with no module
                        // yet), so key on "not yet a module member"
                        // rather than pool membership; on a cap hit
                        // the transitional handle is discarded so
                        // the address doesn't linger half-open.
                        if self.clients.module_of(&peer).await.is_none()
                            && self
                                .clients
                                .members_of_module(self.default_reflector_module)
                                .await
                                .len()
                                >= self.settings.max_clients_per_module
                        {
                            tracing::info!(
                                ?peer,
                                %callsign,
                                module = %self.default_reflector_module,
                                "client capacity limit rejected DPlus LINK2 attempt"
                            );
                            drop(self.clients.remove(&peer).await);
                            return Ok(Self::build_dplus_reject_outcome(
                                peer,
                                RejectReason::MaxClients,
                            ));
                        }
                        Some(access_policy)
                    }
                    Err(reject) => {
                        tracing::info!(
                            ?peer,
                            %callsign,
                            reason = ?reject,
                            "authorizer rejected DPlus LINK2 attempt"
                        );
                        // Discard the transitional LINK1 handle, same
                        // as the cap-reject path below: a denied
                        // client looping the handshake must not pin a
                        // total-cap slot (LINK1 refreshes last_heard,
                        // so the idle sweep would never reclaim it).
                        drop(self.clients.remove(&peer).await);
                        return Ok(Self::build_dplus_reject_outcome(peer, reject));
                    }
                }
            } else {
                None
            };

        let created = self.ensure_handle(peer, link_access, now).await;

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

        let mut outcome = self
            .drive_core_for_handle(peer, bytes, now, created, link_access)
            .await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_link_events(&outcome, peer).await;

        // Pre-update header snapshot; see the DExtra sibling for
        // the EOT/StreamEnd rationale.
        let pre_update_header = self.module_cached_header_of_peer(peer).await;
        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit = self.update_stream_cache_dplus(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer, pre_update_header)
            .await;
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

        // Unknown peers only earn session state via a valid LINK
        // attempt; see the DExtra sibling for the rationale.
        if !self.clients.contains(&peer).await
            && !matches!(pre_decoded, Some(DcsClientPacket::Link { .. }))
        {
            tracing::debug!(?peer, "dropping non-link datagram from unknown DCS peer");
            return Ok(EndpointOutcome::<P>::empty());
        }

        let link_access: Option<AccessPolicy> = if let Some(DcsClientPacket::Link {
            callsign,
            reflector_module,
            ..
        }) = pre_decoded.clone()
        {
            match self.authorize_link(peer, callsign, reflector_module).await {
                Ok(access_policy) => Some(access_policy),
                Err(reject) => {
                    tracing::info!(
                        ?peer,
                        %callsign,
                        %reflector_module,
                        reason = ?reject,
                        "DCS LINK attempt rejected"
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

        let created = self.ensure_handle(peer, link_access, now).await;

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

        let mut outcome = self
            .drive_core_for_handle(peer, bytes, now, created, link_access)
            .await?;
        self.clients.record_last_heard(&peer, now).await;
        self.mirror_link_events(&outcome, peer).await;

        // Pre-update header snapshot; see the DExtra sibling for
        // the EOT/StreamEnd rationale.
        let pre_update_header = self.module_cached_header_of_peer(peer).await;
        if let Some(pkt) = pre_decoded.as_ref() {
            outcome.header_retransmit = self.update_stream_cache_dcs(pkt, bytes, peer, now).await;
        }

        self.publish_voice_events(&outcome, peer, pre_update_header)
            .await;
        self.drain_pending_events(&mut outcome).await;
        Ok(outcome)
    }

    /// Ensure a [`ClientHandle`] exists for `peer` in the pool,
    /// creating one if needed.
    ///
    /// `link_access` is the authorizer decision from a fresh LINK
    /// pre-decode. `None` is possible for the first, callsign-free
    /// `DPlus` handshake packet and therefore defaults to
    /// [`AccessPolicy::ReadOnly`] until LINK2 is authorized and
    /// accepted by the core. New handles get a TX
    /// token bucket sized from the configured
    /// [`EndpointSettings::tx_rate_limit_frames_per_sec`].
    async fn ensure_handle(
        &self,
        peer: SocketAddr,
        link_access: Option<AccessPolicy>,
        now: Instant,
    ) -> bool {
        if self.clients.contains(&peer).await {
            return false;
        }
        let access = link_access.unwrap_or(AccessPolicy::ReadOnly);
        let reflector_module = self.default_reflector_module;
        let core = ServerSessionCore::new(self.protocol, peer, reflector_module);
        let handle = ClientHandle::new_with_tx_rate(
            core,
            access,
            now,
            self.settings.tx_rate_limit_frames_per_sec,
        );
        self.clients.insert(peer, handle).await;
        true
    }

    /// Whether `module` belongs to this endpoint's configured module set.
    fn module_is_configured(&self, module: Module) -> bool {
        self.configured_modules.contains(&module)
    }

    /// Apply the common module, authorizer, and capacity gates for a
    /// single-packet reflector LINK attempt.
    async fn authorize_link(
        &self,
        peer: SocketAddr,
        callsign: Callsign,
        module: Module,
    ) -> Result<AccessPolicy, RejectReason> {
        if !self.module_is_configured(module) {
            return Err(RejectReason::UnknownModule);
        }
        let attempt = LinkAttempt {
            protocol: self.protocol,
            callsign,
            peer,
            module,
        };
        let access = self.authorizer.authorize(&attempt)?;
        if let Some(reject) = self.link_capacity_reject(peer, module).await {
            return Err(reject);
        }
        Ok(access)
    }

    /// Drive an existing/new handle and roll back a newly inserted
    /// handle when the core rejects the packet with an error.
    async fn drive_core_for_handle(
        &self,
        peer: SocketAddr,
        bytes: &[u8],
        now: Instant,
        created: bool,
        pending_access: Option<AccessPolicy>,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        match self.drive_core(&peer, bytes, now, pending_access).await {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                if created {
                    drop(self.clients.remove(&peer).await);
                }
                Err(error)
            }
        }
    }

    fn outcome_linked(outcome: &EndpointOutcome<P>) -> bool {
        outcome
            .events
            .iter()
            .any(|event| matches!(event, ServerEvent::ClientLinked { .. }))
    }

    /// `DPlus` emits no second `ClientLinked` event for an idempotent
    /// LINK2, so the accepted OKRW reply is the exact acceptance
    /// signal for both initial and repeated handshakes.
    fn dplus_link2_was_accepted(outcome: &EndpointOutcome<P>) -> bool {
        let mut expected = [0_u8; 16];
        let Ok(length) = encode_link2_reply(&mut expected, Link2Result::Accept) else {
            return false;
        };
        let expected = expected.get(..length).unwrap_or(&[]);
        outcome
            .txs
            .iter()
            .any(|(payload, _)| payload.as_slice() == expected)
    }

    /// Check the configured client-capacity limits for a peer's LINK
    /// attempt against `module`.
    ///
    /// Returns `Some(RejectReason::Busy)` when admitting a NEW peer
    /// would exceed [`EndpointSettings::max_total_clients`], and
    /// `Some(RejectReason::MaxClients)` when the requested module is
    /// already at [`EndpointSettings::max_clients_per_module`]. A
    /// peer already linked to `module` passes unconditionally: an
    /// idempotent re-link changes neither count.
    async fn link_capacity_reject(&self, peer: SocketAddr, module: Module) -> Option<RejectReason> {
        if self.clients.module_of(&peer).await == Some(module) {
            return None;
        }
        if !self.clients.contains(&peer).await
            && self.clients.len().await >= self.settings.max_total_clients
        {
            return Some(RejectReason::Busy);
        }
        if self.clients.members_of_module(module).await.len()
            >= self.settings.max_clients_per_module
        {
            return Some(RejectReason::MaxClients);
        }
        None
    }

    /// Mirror link-lifecycle events into the pool.
    ///
    /// `ClientLinked` writes the module into the pool's reverse index
    /// so fan-out can enumerate module members in O(1).
    /// `ClientUnlinked` removes the peer's pool entry entirely
    /// (forward map + reverse index): the core session is `Closed`
    /// after an unlink and silently ignores any further LINK, so a
    /// retained entry would leave the address unable to reconnect;
    /// removal lets the next LINK from the same address build a
    /// fresh session.
    async fn mirror_link_events(&self, outcome: &EndpointOutcome<P>, peer: SocketAddr) {
        for ev in &outcome.events {
            match ev {
                ServerEvent::ClientLinked { module, .. } => {
                    self.clients.set_module(&peer, *module).await;
                }
                ServerEvent::ClientUnlinked { .. } => {
                    drop(self.clients.remove(&peer).await);
                }
                _ => {}
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
    ///
    /// The published `cached_header` prefers the module's current
    /// stream-cache entry (so a header cached on this very tick
    /// wins), falling back to `fallback_header` (the caller's
    /// snapshot taken before the stream-cache update) when the
    /// update cleared the entry. That keeps the header attached to
    /// the `StreamEnd` publish on EOT ticks, which `DCS` targets
    /// need to encode the 100-byte end frame.
    ///
    /// First-talker-wins extends to this publish path: while the
    /// module tracks a live stream, events belonging to a DIFFERENT
    /// stream are dropped; otherwise a second talker's audio would
    /// go out under the tracked stream's cached header. The collider
    /// recovers after the tracked stream ends (its next periodic
    /// header retransmit re-establishes it as the tracked stream).
    async fn publish_voice_events(
        &self,
        outcome: &EndpointOutcome<P>,
        peer: SocketAddr,
        fallback_header: Option<DstarHeader>,
    ) {
        let Some(bus) = &self.voice_bus else {
            return;
        };
        let Some(module) = self.clients.module_of(&peer).await else {
            return;
        };
        let (live_header, tracked_stream) = {
            let cache = self.stream_cache.lock().await;
            cache.get(&module).map_or((None, None), |entry| {
                (Some(*entry.header()), Some(entry.stream_id()))
            })
        };
        let cached_header = live_header.or(fallback_header);
        for ev in &outcome.events {
            let Some(voice_event) = voice_event_from_server_event(ev) else {
                continue;
            };
            let ev_stream = match &voice_event {
                VoiceEvent::StreamStart { stream_id, .. }
                | VoiceEvent::Frame { stream_id, .. }
                | VoiceEvent::StreamEnd { stream_id, .. } => *stream_id,
            };
            if tracked_stream.is_some_and(|tracked| tracked != ev_stream) {
                continue;
            }
            // `broadcast::Sender::send` errors only when there are
            // no live receivers; that's fine for publish: we don't
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

    /// Look up the cached `DstarHeader` (if any) for the given module.
    ///
    /// Used by [`Self::publish_voice_events`] so `DCS` subscribers
    /// on the other side of the bus receive the header context they
    /// need to re-encode inbound voice data into 100-byte packets.
    async fn cached_header_for_module(&self, module: Module) -> Option<DstarHeader> {
        let cache = self.stream_cache.lock().await;
        cache.get(&module).map(|entry| *entry.header())
    }

    /// Cached header for the module `peer` is linked to, if any.
    ///
    /// Convenience wrapper the inbound handlers use to snapshot the
    /// live header before mutating the stream cache. See
    /// [`Self::publish_voice_events`] for how the snapshot is used.
    async fn module_cached_header_of_peer(&self, peer: SocketAddr) -> Option<DstarHeader> {
        let module = self.clients.module_of(&peer).await?;
        self.cached_header_for_module(module).await
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

    /// Periodic maintenance sweep: idle eviction, stale stream-cache
    /// cleanup, and server-initiated keepalives.
    ///
    /// Driven by [`Self::run`] on a fixed interval with the wall
    /// clock, and by tests directly with synthetic instants. Like
    /// [`Self::handle_inbound`], it never samples a clock itself.
    /// Returns the keepalive datagrams to transmit (`(payload,
    /// destination)` pairs); the caller owns the socket.
    ///
    /// One sweep performs, in order:
    ///
    /// 1. Evict every pool entry whose `last_heard` is at least
    ///    [`EndpointSettings::keepalive_inactivity_timeout`] old.
    ///    Each eviction queues a [`ServerEvent::ClientEvicted`] on
    ///    the pending-event queue (surfaced on the next
    ///    [`Self::handle_inbound`] outcome).
    /// 2. Drop stream-cache entries idle for at least
    ///    [`EndpointSettings::voice_inactivity_timeout`] (via
    ///    [`StreamCache::should_evict`]), so a stream that died
    ///    without an EOT frees its module's cache.
    /// 3. If at least [`EndpointSettings::keepalive_interval`] has
    ///    elapsed since the last keepalive send (the first sweep
    ///    always sends), emit one protocol-appropriate keepalive per
    ///    surviving *linked* client. See `encode_keepalive_for` for
    ///    the wire forms.
    pub(crate) async fn handle_tick(&self, now: Instant) -> Vec<(Vec<u8>, SocketAddr)> {
        // 1. Idle-client eviction.
        let entries = self.clients.sweep_snapshot().await;
        for entry in &entries {
            if now.saturating_duration_since(entry.last_heard)
                >= self.settings.keepalive_inactivity_timeout
            {
                tracing::info!(
                    peer = ?entry.peer,
                    "evicting client after keepalive inactivity timeout"
                );
                self.evict_peer(entry.peer, "keepalive inactivity timeout")
                    .await;
            }
        }

        // 2. Stale voice-stream cleanup.
        {
            let mut cache = self.stream_cache.lock().await;
            cache.retain(|module, entry| {
                let stale = entry.should_evict(now, self.settings.voice_inactivity_timeout);
                if stale {
                    tracing::info!(
                        %module,
                        stream_id = ?entry.stream_id(),
                        "evicting stalled voice stream from cache"
                    );
                }
                !stale
            });
        }

        // 3. Keepalives, one per linked client, when due.
        let due = {
            let mut last = self.last_keepalive_sent.lock().await;
            let due = last.is_none_or(|sent| {
                now.saturating_duration_since(sent) >= self.settings.keepalive_interval
            });
            if due {
                *last = Some(now);
            }
            due
        };
        if !due {
            return Vec::new();
        }
        let survivors = self.clients.sweep_snapshot().await;
        survivors
            .iter()
            .filter_map(|entry| {
                self.encode_keepalive_for(entry)
                    .map(|payload| (payload, entry.peer))
            })
            .collect()
    }

    /// Encode the server-initiated keepalive for one pool entry.
    ///
    /// Only fully linked clients (module assigned) get keepalives;
    /// mid-handshake entries return `None`. The wire forms follow
    /// xlxd's per-protocol keepalive senders:
    ///
    /// - `DExtra`: the 9-byte reflector-callsign poll, an 8-byte
    ///   space-padded callsign plus a NUL (`xlxd/src/cdextraprotocol.cpp`
    ///   `EncodeKeepAlivePacket` / `IsValidKeepAlivePacket`). Same
    ///   wire shape the core's `encode_poll` produces, with the
    ///   REFLECTOR's callsign rather than a client's.
    /// - `DPlus`: the 3-byte `0x03 0x60 0x00` poll
    ///   (`xlxd/src/cdplusprotocol.cpp` `EncodeKeepAlivePacket`),
    ///   byte-identical to the core's `encode_poll_echo`.
    /// - `DCS`: the 22-byte per-client form
    ///   (`xlxd/src/cdcsprotocol.cpp` `EncodeKeepAlivePacket(CBuffer*,
    ///   CClient*)`): reflector callsign (7 bytes) + linked reflector
    ///   module + `' '` + client callsign (7 bytes) + client module
    ///   twice + `0x0A 0x00 0x20 0x20`. The core's 17-byte
    ///   `encode_poll_reply` form is deliberately NOT reused here:
    ///   ircDDBGateway's `CDCSHandler::process(CPollData&)` only
    ///   refreshes an outgoing link's poll-inactivity timer for the
    ///   22-byte form, so a 17-byte server keepalive would let real
    ///   clients time out.
    fn encode_keepalive_for(&self, entry: &SweepEntry) -> Option<Vec<u8>> {
        let module = entry.module?;
        match self.protocol {
            ProtocolKind::DExtra => {
                let mut buf = [0u8; 16];
                let n = encode_dextra_poll(&mut buf, &self.settings.reflector_callsign).ok()?;
                Some(buf.get(..n)?.to_vec())
            }
            ProtocolKind::DPlus => {
                let mut buf = [0u8; 8];
                let n = encode_dplus_poll_echo(&mut buf).ok()?;
                Some(buf.get(..n)?.to_vec())
            }
            ProtocolKind::Dcs => {
                let callsign = entry.client_callsign?;
                let client_module = entry.client_module?;
                let mut payload = Vec::with_capacity(22);
                payload.extend_from_slice(
                    self.settings
                        .reflector_callsign
                        .as_bytes()
                        .as_slice()
                        .get(..7)?,
                );
                payload.push(module.as_byte());
                payload.push(b' ');
                payload.extend_from_slice(callsign.as_bytes().as_slice().get(..7)?);
                payload.push(client_module.as_byte());
                payload.push(client_module.as_byte());
                payload.extend_from_slice(&[0x0A, 0x00, 0x20, 0x20]);
                Some(payload)
            }
            _ => None,
        }
    }

    /// Update the per-module `DExtra` stream cache for this packet.
    ///
    /// The cache tracks ONE stream per module, the first active one,
    /// guarded by stream id (the first-talker-wins semantics all
    /// three protocol variants share), so a second simultaneous
    /// talker cannot corrupt the entry:
    /// - `VoiceHeader`: insert when the module has no entry (or
    ///   refresh when the id matches the tracked stream); a header
    ///   for a DIFFERENT stream is ignored while the tracked stream
    ///   is live.
    /// - `VoiceData`: only a frame of the tracked stream bumps the
    ///   seq counter; if `should_rebroadcast_header` fires return a
    ///   clone of the cached bytes. Frames of other streams are
    ///   ignored.
    /// - `VoiceEot`: only the tracked stream's EOT removes the entry.
    ///
    /// A tracked stream that dies without an EOT is reclaimed by the
    /// voice-inactivity sweep in [`Self::handle_tick`].
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
                if cache_guard
                    .get(&module)
                    .is_some_and(|entry| entry.stream_id() != *stream_id)
                {
                    // A second talker's header must not hijack the
                    // module entry mid-stream.
                    return None;
                }
                let entry =
                    StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
                let _prev = cache_guard.insert(module, entry);
                None
            }
            ClientPacket::VoiceData { stream_id, .. } => {
                let entry = cache_guard.get_mut(&module)?;
                if entry.stream_id() != *stream_id {
                    // A second talker's data must not advance the
                    // tracked stream's retransmit cadence.
                    return None;
                }
                entry.record_frame(now);
                if entry.should_rebroadcast_header() {
                    Some(entry.header_bytes().to_vec())
                } else {
                    None
                }
            }
            ClientPacket::VoiceEot { stream_id, .. } => {
                if cache_guard
                    .get(&module)
                    .is_some_and(|entry| entry.stream_id() == *stream_id)
                {
                    let _prev = cache_guard.remove(&module);
                }
                None
            }
            _ => None,
        }
    }

    /// Update the per-module `DPlus` stream cache for this packet.
    ///
    /// Same lifecycle and stream-id guards as
    /// [`Self::update_stream_cache_dextra`], but operates on the
    /// [`dstar_gateway_core::codec::dplus::ClientPacket`] enum.
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
                if cache_guard
                    .get(&module)
                    .is_some_and(|entry| entry.stream_id() != *stream_id)
                {
                    // A second talker's header must not hijack the
                    // module entry mid-stream.
                    return None;
                }
                let entry =
                    StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
                let _prev = cache_guard.insert(module, entry);
                None
            }
            DPlusClientPacket::VoiceData { stream_id, .. } => {
                let entry = cache_guard.get_mut(&module)?;
                if entry.stream_id() != *stream_id {
                    // A second talker's data must not advance the
                    // tracked stream's retransmit cadence.
                    return None;
                }
                entry.record_frame(now);
                if entry.should_rebroadcast_header() {
                    Some(entry.header_bytes().to_vec())
                } else {
                    None
                }
            }
            DPlusClientPacket::VoiceEot { stream_id, .. } => {
                if cache_guard
                    .get(&module)
                    .is_some_and(|entry| entry.stream_id() == *stream_id)
                {
                    let _prev = cache_guard.remove(&module);
                }
                None
            }
            _ => None,
        }
    }

    /// Update the per-module `DCS` stream cache for this packet.
    ///
    /// `DCS` is a single-packet-per-frame protocol: every `Voice`
    /// packet carries the header + AMBE + optional end marker. With
    /// no live entry, the first sighting of a `stream_id` acts as the
    /// implicit stream-start and is cached. Subsequent packets with
    /// the same `stream_id` are data (and trigger the 21-frame
    /// retransmit cadence); the same stream's `is_end = true` clears
    /// the cache. While an entry is live, packets of a DIFFERENT
    /// stream are ignored (first-talker-wins, the same guard as the
    /// `DExtra`/`DPlus` variants), so a colliding talker can neither
    /// hijack the entry nor clear it with a foreign `is_end`. A
    /// tracked stream that dies without its own `is_end` is reclaimed
    /// by the voice-inactivity sweep in [`Self::handle_tick`].
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

        match cache_guard.get(&module).map(StreamCache::stream_id) {
            // No live stream on this module: first sighting of a
            // stream id is the implicit stream-start. A lone is_end
            // packet has nothing to track.
            None => {
                if !*is_end {
                    let entry =
                        StreamCache::new_with_bytes(*stream_id, *header, bytes.to_vec(), peer, now);
                    let _prev = cache_guard.insert(module, entry);
                }
                None
            }
            // A different stream is live: ignore the collider. Its
            // header must not hijack the entry, its data must not
            // advance the cadence, and its is_end must not clear it.
            Some(tracked) if tracked != *stream_id => None,
            // Same stream id: data frame. Bump the seq counter; if
            // the packet is the end-of-stream marker, clear the cache
            // after checking the retransmit cadence one last time.
            Some(_) => {
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
        }
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
    /// touched: the peer never becomes a handle.
    fn build_dextra_reject_outcome(
        peer: SocketAddr,
        callsign: Callsign,
        reflector_module: Module,
        reject: RejectReason,
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
    /// touched: the peer never becomes a handle.
    fn build_dplus_reject_outcome(peer: SocketAddr, reject: RejectReason) -> EndpointOutcome<P> {
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
    /// touched: the peer never becomes a handle.
    fn build_dcs_reject_outcome(
        peer: SocketAddr,
        callsign: Callsign,
        reflector_module: Module,
        reject: RejectReason,
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

    /// Drive the core's state machine, apply an accepted access verdict, and
    /// drain its outbox + events.
    ///
    /// Held as a private helper so the lock-protected mutation of the
    /// per-peer `ServerSessionCore` stays in one place. We take the pool's
    /// mutex, borrow the handle mutably, feed the core, drain everything into
    /// owned `Vec`s, and apply `pending_access` only when that outcome proves
    /// the link was accepted. The state transition and authorization update
    /// therefore become visible together when the lock is released.
    async fn drive_core(
        &self,
        peer: &SocketAddr,
        bytes: &[u8],
        now: Instant,
        pending_access: Option<AccessPolicy>,
    ) -> Result<EndpointOutcome<P>, ShellError> {
        // We need mutable access to the handle inside the pool's
        // HashMap. `ClientPool` intentionally doesn't expose `&mut`
        // directly, so reach through the private `Mutex<HashMap>` here
        // via a dedicated method on the pool.
        let mut outcome = EndpointOutcome::<P>::empty();

        self.clients
            .with_handle_mut(peer, |handle| -> Result<(), ShellError> {
                handle.session.handle_input(now, bytes)?;
                // The core stamps outbox entries with the SAME
                // injected `now` we just passed to `handle_input`
                // (every `ServerSessionCore` enqueue uses
                // `not_before: now`, and the outbox never samples a
                // clock), so draining with the caller's `now` pops
                // everything enqueued on this tick: `pop_ready`
                // returns packets with `not_before <= now`. Never
                // re-sample the wall clock here: a caller driving a
                // synthetic clock (tests, replay tooling) would see
                // its responses stranded until real time caught up
                // with the injected instant.
                while let Some(tx) = handle.session.pop_transmit(now) {
                    outcome.txs.push((tx.payload.to_vec(), tx.dst));
                }
                while let Some(ev) = handle.session.pop_event::<P>() {
                    outcome.events.push(ev);
                }
                if let Some(access) = pending_access {
                    let accepted = match self.protocol {
                        ProtocolKind::DPlus => Self::dplus_link2_was_accepted(&outcome),
                        ProtocolKind::DExtra | ProtocolKind::Dcs => Self::outcome_linked(&outcome),
                        _ => false,
                    };
                    if accepted {
                        handle.access = access;
                    }
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
    /// resolved from the client pool; the caller has already linked
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
    /// every other peer on the same module. A maintenance timer also
    /// drives the periodic sweep (idle eviction, stale-stream
    /// cleanup, keepalives) on the configured keepalive interval.
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
    /// endpoint task: the enclosing [`tokio::task::JoinSet`] in
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
        // Maintenance sweep timer. The period is the configured
        // keepalive interval, clamped to 100 ms so a zero-duration
        // config can neither panic `tokio::time::interval` nor spin
        // the loop; `handle_tick` re-checks due-ness against the
        // configured interval itself, so a faster timer only costs
        // wakeups.
        let tick_period = self
            .settings
            .keepalive_interval
            .max(Duration::from_millis(100));
        let mut maintenance = tokio::time::interval(tick_period);
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // `interval`'s first tick completes immediately, so consume it
        // here so the first maintenance sweep runs one full period
        // after startup instead of racing the first inbound
        // datagrams with a keepalive burst.
        let _immediate_first_tick = maintenance.tick().await;
        loop {
            // Pattern: "maybe-subscribed optional branch". When
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
                    // `changed` resolves `Err` when all senders drop;
                    // treat that as an implicit shutdown.
                    if change.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
                // The maintenance arm sits ABOVE the socket arm on
                // purpose: with `biased`, arms are polled in order,
                // and a sustained datagram flood keeps `recv_from`
                // perpetually ready; placed after it, the sweep
                // (idle eviction, keepalives) would starve under
                // exactly the load it exists to defend against. The
                // interval is ready at most once per period, so this
                // ordering costs nothing when idle.
                _ = maintenance.tick() => {
                    let txs = self.handle_tick(Instant::now()).await;
                    let mut evicted_peers: Vec<SocketAddr> = Vec::new();
                    self.send_replies(&socket, &txs, &mut evicted_peers).await;
                    for evicted in evicted_peers {
                        self.evict_peer(evicted, "too many send failures").await;
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
                            // Bus has been closed; no more events
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
    /// are dropped; the normal within-protocol fan-out path already
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

        // Remove any peers whose send-failure count crossed the
        // eviction threshold on this tick. The ClientEvicted
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
        // If the stream cache fired a header retransmit on this tick,
        // fan out the cached bytes alongside the normal
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
    use super::{EndpointOutcome, EndpointSettings, ProtocolEndpoint};
    use crate::reflector::{
        AllowAllAuthorizer, ClientAuthorizer, DenyAllAuthorizer, ReadOnlyAuthorizer,
    };
    use dstar_gateway_core::codec::dcs::{
        GatewayType as DcsGatewayType, encode_connect_link as encode_dcs_link,
        encode_connect_nak as encode_dcs_nak, encode_connect_unlink as encode_dcs_unlink,
        encode_voice as encode_dcs_voice,
    };
    use dstar_gateway_core::codec::dextra::{
        encode_connect_link, encode_connect_nak as encode_dextra_nak, encode_poll, encode_unlink,
        encode_voice_data, encode_voice_eot, encode_voice_header,
    };
    use dstar_gateway_core::codec::dplus::{
        Link2Result, encode_link1 as encode_dplus_link1, encode_link2 as encode_dplus_link2,
        encode_link2_reply, encode_unlink as encode_dplus_unlink,
        encode_voice_data as encode_dplus_voice_data, encode_voice_eot as encode_dplus_voice_eot,
        encode_voice_header as encode_dplus_voice_header,
    };
    use dstar_gateway_core::header::DstarHeader;
    use dstar_gateway_core::session::client::{DExtra, DPlus, Dcs};
    use dstar_gateway_core::session::server::{ClientRejectedReason, ServerEvent, ServerStateKind};
    use dstar_gateway_core::types::{Callsign, Module, ProtocolKind, StreamId, Suffix};
    use dstar_gateway_core::voice::VoiceFrame;
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn peer() -> SocketAddr {
        PEER
    }

    fn allow_all() -> Arc<dyn ClientAuthorizer> {
        Arc::new(AllowAllAuthorizer)
    }

    fn configured_modules() -> HashSet<Module> {
        HashSet::from([Module::C])
    }

    struct SwitchableAuthorizer {
        read_only: AtomicBool,
    }

    impl SwitchableAuthorizer {
        const fn new() -> Self {
            Self {
                read_only: AtomicBool::new(false),
            }
        }

        fn set_read_only(&self) {
            self.read_only.store(true, Ordering::SeqCst);
        }
    }

    impl ClientAuthorizer for SwitchableAuthorizer {
        fn authorize(
            &self,
            _request: &crate::reflector::LinkAttempt,
        ) -> Result<crate::reflector::AccessPolicy, crate::reflector::RejectReason> {
            if self.read_only.load(Ordering::SeqCst) {
                Ok(crate::reflector::AccessPolicy::ReadOnly)
            } else {
                Ok(crate::reflector::AccessPolicy::ReadWrite)
            }
        }
    }

    struct PanicAuthorizer;

    impl ClientAuthorizer for PanicAuthorizer {
        #[expect(
            clippy::panic,
            reason = "the test authorizer must prove the call is unreachable"
        )]
        fn authorize(
            &self,
            _request: &crate::reflector::LinkAttempt,
        ) -> Result<crate::reflector::AccessPolicy, crate::reflector::RejectReason> {
            panic!("unconfigured modules must be rejected before authorization")
        }
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
        // offset is asserted by the codec's own golden tests, so we
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

    // ─── synthetic-time contract ──────────────────────────────────
    #[tokio::test]
    async fn dextra_link_with_synthetic_future_now_still_drains_ack() -> TestResult {
        // `handle_inbound` takes an injected `now`; the core stamps
        // its outbox replies with that same instant, so the shell
        // must drain the outbox with the caller's `now`, not a
        // re-sampled wall clock. A caller driving a synthetic clock
        // (tests, replay tooling) would otherwise see its responses
        // stranded until the wall clock catches up.
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let mut buf = [0u8; 16];
        let n = encode_connect_link(
            &mut buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let slice = buf.get(..n).ok_or("encode produced no bytes")?;

        let future_now = Instant::now() + Duration::from_secs(3600);
        let outcome: EndpointOutcome<DExtra> = ep.handle_inbound(slice, peer(), future_now).await?;

        assert_eq!(
            outcome.txs.len(),
            1,
            "LINK ACK must drain with the caller's injected now, not the wall clock"
        );
        Ok(())
    }

    // ─── unlink must release the pool entry ───────────────────────
    #[tokio::test]
    async fn dextra_unlink_releases_pool_entry_and_allows_relink() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let mut link_buf = [0u8; 16];
        let n = encode_connect_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::C,
            Module::B,
        )?;
        let link_slice = link_buf.get(..n).ok_or("link empty")?;

        // LINK: ACK on the wire, pool + module reverse index populated.
        let link_outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;
        assert_eq!(link_outcome.txs.len(), 1, "LINK produces an ACK");
        assert!(ep.clients().contains(&peer()).await, "peer in pool");
        assert_eq!(
            ep.clients().members_of_module(Module::C).await,
            vec![peer()]
        );

        // UNLINK: ClientUnlinked surfaces AND the pool entry is gone.
        let mut unlink_buf = [0u8; 16];
        let n = encode_unlink(
            &mut unlink_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
        )?;
        let unlink_slice = unlink_buf.get(..n).ok_or("unlink empty")?;
        let unlink_outcome = ep
            .handle_inbound(unlink_slice, peer(), Instant::now())
            .await?;
        assert!(
            unlink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientUnlinked { .. })),
            "UNLINK emits ClientUnlinked"
        );
        assert!(
            !ep.clients().contains(&peer()).await,
            "unlinked peer must be removed from the pool"
        );
        assert!(
            ep.clients().members_of_module(Module::C).await.is_empty(),
            "unlinked peer must be removed from the module reverse index"
        );

        // Re-LINK from the same address: a fresh session must accept
        // the LINK and ACK it (a leftover Closed session would
        // silently ignore it).
        let relink_outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;
        assert_eq!(relink_outcome.txs.len(), 1, "re-LINK produces a fresh ACK");
        let (payload, dst) = relink_outcome.txs.first().ok_or("no relink tx")?;
        assert_eq!(*dst, peer());
        assert_eq!(payload.len(), 14, "DExtra ACK is 14 bytes");
        assert!(
            relink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientLinked { .. })),
            "re-LINK emits ClientLinked"
        );
        assert!(ep.clients().contains(&peer()).await, "peer re-admitted");
        Ok(())
    }

    #[tokio::test]
    async fn dplus_unlink_releases_pool_entry_and_allows_relink() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        let link1_slice = link1_buf.get(..n1).ok_or("link1 empty")?;
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        let link2_slice = link2_buf.get(..n2).ok_or("link2 empty")?;

        // Full LINK1 + LINK2 handshake.
        drop(
            ep.handle_inbound(link1_slice, peer(), Instant::now())
                .await?,
        );
        let link_outcome = ep
            .handle_inbound(link2_slice, peer(), Instant::now())
            .await?;
        assert_eq!(link_outcome.txs.len(), 1, "LINK2 produces the OKRW reply");
        assert_eq!(
            ep.clients().members_of_module(Module::C).await,
            vec![peer()]
        );

        // UNLINK: ack + ClientUnlinked + pool entry gone.
        let mut unlink_buf = [0u8; 16];
        let n = encode_dplus_unlink(&mut unlink_buf)?;
        let unlink_slice = unlink_buf.get(..n).ok_or("unlink empty")?;
        let unlink_outcome = ep
            .handle_inbound(unlink_slice, peer(), Instant::now())
            .await?;
        assert!(
            unlink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientUnlinked { .. })),
            "UNLINK emits ClientUnlinked"
        );
        assert!(
            !ep.clients().contains(&peer()).await,
            "unlinked peer must be removed from the pool"
        );
        assert!(
            ep.clients().members_of_module(Module::C).await.is_empty(),
            "unlinked peer must be removed from the module reverse index"
        );

        // Re-link from the same address with a fresh handshake.
        drop(
            ep.handle_inbound(link1_slice, peer(), Instant::now())
                .await?,
        );
        let relink_outcome = ep
            .handle_inbound(link2_slice, peer(), Instant::now())
            .await?;
        assert_eq!(relink_outcome.txs.len(), 1, "re-LINK2 produces a reply");
        let (payload, _dst) = relink_outcome.txs.first().ok_or("no relink tx")?;
        assert!(
            payload.windows(4).any(|w| w == b"OKRW"),
            "re-LINK2 reply is a fresh OKRW accept, got {payload:?}"
        );
        assert!(
            relink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientLinked { .. })),
            "re-LINK emits ClientLinked"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dcs_unlink_releases_pool_entry_and_allows_relink() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());
        let mut link_buf = vec![0u8; 600];
        let link_n = encode_dcs_link(
            &mut link_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Module::C,
            &dcs_reflector_cs(),
            DcsGatewayType::Repeater,
        )?;
        let link_slice = link_buf.get(..link_n).ok_or("link empty")?;

        let link_outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;
        assert_eq!(link_outcome.txs.len(), 1, "LINK produces an ACK");
        assert_eq!(
            ep.clients().members_of_module(Module::C).await,
            vec![peer()]
        );

        // UNLINK: ClientUnlinked + pool entry gone (the DCS core
        // sends no unlink ack, only the event + state transition).
        let mut unlink_buf = [0u8; 32];
        let n = encode_dcs_unlink(
            &mut unlink_buf,
            &Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            &dcs_reflector_cs(),
        )?;
        let unlink_slice = unlink_buf.get(..n).ok_or("unlink empty")?;
        let unlink_outcome = ep
            .handle_inbound(unlink_slice, peer(), Instant::now())
            .await?;
        assert!(
            unlink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientUnlinked { .. })),
            "UNLINK emits ClientUnlinked"
        );
        assert!(
            !ep.clients().contains(&peer()).await,
            "unlinked peer must be removed from the pool"
        );
        assert!(
            ep.clients().members_of_module(Module::C).await.is_empty(),
            "unlinked peer must be removed from the module reverse index"
        );

        // Re-LINK from the same address gets a fresh ACK.
        let relink_outcome = ep
            .handle_inbound(link_slice, peer(), Instant::now())
            .await?;
        assert_eq!(relink_outcome.txs.len(), 1, "re-LINK produces a fresh ACK");
        let (payload, _dst) = relink_outcome.txs.first().ok_or("no relink tx")?;
        assert_eq!(payload.len(), 14, "DCS ACK is 14 bytes");
        assert!(
            relink_outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientLinked { .. })),
            "re-LINK emits ClientLinked"
        );
        Ok(())
    }

    // ─── unknown peers must not allocate pool state ───────────────
    #[tokio::test]
    async fn garbage_datagrams_from_unknown_peers_allocate_no_handles() {
        let dextra = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let dplus = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());
        let dcs = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());

        for i in 0_u16..10 {
            let garbage_peer =
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40100_u16.saturating_add(i));
            // Arbitrary garbage of varying length. Malformed input may
            // surface a decode error from `handle_inbound`; either
            // way it must not allocate session state for the peer.
            let garbage = vec![0xA5_u8; usize::from(i).saturating_add(3)];
            drop(
                dextra
                    .handle_inbound(&garbage, garbage_peer, Instant::now())
                    .await,
            );
            drop(
                dplus
                    .handle_inbound(&garbage, garbage_peer, Instant::now())
                    .await,
            );
            drop(
                dcs.handle_inbound(&garbage, garbage_peer, Instant::now())
                    .await,
            );
        }

        assert_eq!(
            dextra.clients().len().await,
            0,
            "DExtra: garbage from unknown addrs must not allocate pool handles"
        );
        assert_eq!(
            dplus.clients().len().await,
            0,
            "DPlus: garbage from unknown addrs must not allocate pool handles"
        );
        assert_eq!(
            dcs.clients().len().await,
            0,
            "DCS: garbage from unknown addrs must not allocate pool handles"
        );
    }

    #[tokio::test]
    async fn valid_non_link_datagram_from_unknown_peer_allocates_no_handle() -> TestResult {
        // A well-formed voice frame (or poll) from an address that
        // never linked decodes fine but warrants no session state:
        // the core would ignore it from the `Unknown` state anyway.
        let ep = ProtocolEndpoint::<DExtra>::new(ProtocolKind::DExtra, Module::C, allow_all());
        let stranger = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40200);

        let frame = VoiceFrame::silence();
        let mut data_buf = [0u8; 64];
        let data_n = encode_voice_data(&mut data_buf, sid(), 0, &frame)?;
        let data_slice = data_buf.get(..data_n).ok_or("empty")?;
        let outcome = ep
            .handle_inbound(data_slice, stranger, Instant::now())
            .await?;
        assert!(outcome.txs.is_empty(), "no reply to a stranger's voice");
        assert_eq!(
            ep.clients().len().await,
            0,
            "voice from an unknown peer must not allocate a pool handle"
        );
        Ok(())
    }

    // ─── DPlus handshake ─────────────────────────────────────────
    #[tokio::test]
    async fn dplus_link2_after_link1_creates_handle_and_acks_okrw() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());

        // LINK1 is 5 bytes, no callsign. The core transitions to
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
        // LINK1 does not emit a ClientLinked event; the login isn't
        // complete until LINK2 arrives with the callsign.
        assert!(
            outcome1.events.is_empty(),
            "LINK1 emits no events (no callsign yet)"
        );
        // A handle was created (needed to carry the Link1Received state).
        assert_eq!(ep.clients().len().await, 1);

        // LINK2 is 28 bytes carrying the callsign. The core fires
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

        // Voice header: 58 bytes.
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

        // DCS voice is 100 bytes; the first packet for a new stream id
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

        // Now start a NEW stream with a different stream id. This
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

    fn test_header(cs_my: &str) -> DstarHeader {
        let mut my_bytes = *b"        ";
        for (dst, byte) in my_bytes.iter_mut().zip(cs_my.bytes().take(8)) {
            *dst = byte;
        }
        DstarHeader {
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

    // ─── Denied authorization ─────────────────────────────────────
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

        // The pool must be empty: no handle was created.
        assert_eq!(
            ep.clients().len().await,
            0,
            "rejected peer must not be in pool"
        );

        // Exactly one outbound NAK to the same peer. The NAK tag
        // position is asserted by the codec's own golden tests, so we
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

    // ─── StreamCache 21-frame header retransmit ───────────────────
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
        // The header tick itself does NOT trigger a retransmit;
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

        // Voice EOT clears the cache.
        let mut eot_buf = [0u8; 64];
        let eot_n = encode_voice_eot(&mut eot_buf, sid(), 20)?;
        let eot_slice = eot_buf.get(..eot_n).ok_or("empty")?;
        drop(ep.handle_inbound(eot_slice, peer(), Instant::now()).await?);

        // The stream cache is empty, so subsequent data frames from the
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

    // ─── stream-collision cache corruption ────────────────────────
    const fn sid_of(n: u16) -> StreamId {
        match StreamId::new(n) {
            Some(s) => s,
            None => unreachable!(),
        }
    }

    fn drain_bus(
        rx: &mut tokio::sync::broadcast::Receiver<super::CrossProtocolEvent>,
    ) -> Vec<super::CrossProtocolEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        events
    }

    /// Two talkers on one module: the cache must keep tracking the
    /// FIRST active stream until that stream ends: the second
    /// talker's header must not hijack the entry, its data must not
    /// advance the first stream's retransmit cadence, and its EOT
    /// must not clear the entry.
    #[expect(
        clippy::too_many_lines,
        reason = "test walks two interleaved talkers through a full stream lifecycle; splitting it would obscure the linear narrative"
    )]
    #[tokio::test]
    async fn dextra_stream_cache_tracks_first_talker_until_eot() -> TestResult {
        use tokio::sync::broadcast;
        let (tx, mut rx) = broadcast::channel::<super::CrossProtocolEvent>(256);
        let ep = ProtocolEndpoint::<DExtra>::new_with_voice_bus(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            Some(tx),
        );
        let peer_a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40301);
        let peer_b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40302);
        let sid_a = sid_of(0x1111);
        let sid_b = sid_of(0x2222);
        let frame = VoiceFrame::silence();

        // Both peers link to module C.
        for (peer, callsign) in [
            (peer_a, Callsign::from_wire_bytes(*b"W1AW    ")),
            (peer_b, Callsign::from_wire_bytes(*b"K1ABC   ")),
        ] {
            let mut link_buf = [0u8; 16];
            let n = encode_connect_link(&mut link_buf, &callsign, Module::C, Module::B)?;
            drop(
                ep.handle_inbound(link_buf.get(..n).ok_or("empty")?, peer, Instant::now())
                    .await?,
            );
        }

        // A starts a stream, then B tries to start a second one on
        // the same module.
        let mut hdr_a = [0u8; 64];
        let n = encode_voice_header(&mut hdr_a, sid_a, &test_header("W1AW"))?;
        drop(
            ep.handle_inbound(hdr_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        let mut hdr_b = [0u8; 64];
        let n = encode_voice_header(&mut hdr_b, sid_b, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        drop(drain_bus(&mut rx));

        // A's first data frame publishes with A's header still cached:
        // B's header must not have replaced the module entry.
        let mut data_a = [0u8; 64];
        let n = encode_voice_data(&mut data_a, sid_a, 0, &frame)?;
        drop(
            ep.handle_inbound(data_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        let events = drain_bus(&mut rx);
        let frame_event = events
            .iter()
            .find(|ev| matches!(ev.event, super::VoiceEvent::Frame { .. }))
            .ok_or("A's data frame not published")?;
        assert_eq!(
            frame_event.cached_header.map(|h| h.my_call),
            Some(Callsign::from_wire_bytes(*b"W1AW    ")),
            "cached header must still be A's after B's mid-stream header"
        );

        // B's data frames must not advance A's retransmit cadence.
        for seq in 0_u8..25 {
            let mut data_b = [0u8; 64];
            let n = encode_voice_data(&mut data_b, sid_b, seq, &frame)?;
            let outcome = ep
                .handle_inbound(data_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?;
            assert!(
                outcome.header_retransmit.is_none(),
                "B's frame {seq} must not fire the module's header retransmit"
            );
        }

        // B's EOT must not clear A's cache entry.
        let mut eot_b = [0u8; 64];
        let n = encode_voice_eot(&mut eot_b, sid_b, 25)?;
        drop(
            ep.handle_inbound(eot_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );

        // 19 more A frames (20 total) trip the cadence exactly once,
        // and the retransmitted bytes are A's original header.
        let mut retransmits = Vec::new();
        for seq in 1_u8..20 {
            let mut data = [0u8; 64];
            let n = encode_voice_data(&mut data, sid_a, seq, &frame)?;
            let outcome = ep
                .handle_inbound(data.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?;
            if let Some(bytes) = outcome.header_retransmit {
                retransmits.push(bytes);
            }
        }
        assert_eq!(
            retransmits.len(),
            1,
            "exactly one retransmit after A's 20th data frame"
        );
        let retransmitted = retransmits.first().ok_or("no retransmit")?;
        assert!(
            retransmitted.windows(4).any(|w| w == b"W1AW"),
            "retransmitted header must be A's (W1AW), not B's"
        );

        // Once A ends, B may take over the module cache.
        let mut eot_a = [0u8; 64];
        let n = encode_voice_eot(&mut eot_a, sid_a, 20)?;
        drop(
            ep.handle_inbound(eot_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        drop(drain_bus(&mut rx));
        let sid_b2 = sid_of(0x3333);
        let mut hdr_b2 = [0u8; 64];
        let n = encode_voice_header(&mut hdr_b2, sid_b2, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b2.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let mut data_b2 = [0u8; 64];
        let n = encode_voice_data(&mut data_b2, sid_b2, 0, &frame)?;
        drop(
            ep.handle_inbound(data_b2.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let events = drain_bus(&mut rx);
        let frame_event = events
            .iter()
            .find(|ev| matches!(ev.event, super::VoiceEvent::Frame { .. }))
            .ok_or("B's post-takeover frame not published")?;
        assert_eq!(
            frame_event.cached_header.map(|h| h.my_call),
            Some(Callsign::from_wire_bytes(*b"K1ABC   ")),
            "after A's EOT the cache follows B's new stream"
        );
        Ok(())
    }

    /// `DCS` sibling of
    /// [`dextra_stream_cache_tracks_first_talker_until_eot`]. `DCS`
    /// has no separate header/data/EOT packets (every 100-byte voice
    /// packet embeds the header plus an `is_end` flag), so the guard
    /// is asserted directly on the tracked cache entry: a colliding
    /// stream must neither replace the live entry nor clear it with a
    /// foreign `is_end`.
    #[tokio::test]
    async fn dcs_stream_cache_tracks_first_talker_until_eot() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(ProtocolKind::Dcs, Module::C, allow_all());
        let peer_a = test_peer(40321);
        let peer_b = test_peer(40322);
        let sid_a = sid_of(0x1111);
        let sid_b = sid_of(0x2222);
        let frame = VoiceFrame::silence();

        for (peer, callsign) in [
            (peer_a, Callsign::from_wire_bytes(*b"W1AW    ")),
            (peer_b, Callsign::from_wire_bytes(*b"K1ABC   ")),
        ] {
            let mut link_buf = vec![0u8; 600];
            let n = encode_dcs_link(
                &mut link_buf,
                &callsign,
                Module::B,
                Module::C,
                &dcs_reflector_cs(),
                DcsGatewayType::Repeater,
            )?;
            drop(
                ep.handle_inbound(link_buf.get(..n).ok_or("empty")?, peer, Instant::now())
                    .await?,
            );
        }

        // A's first packet tracks A's stream.
        let mut voice_a = [0u8; 128];
        let n = encode_dcs_voice(&mut voice_a, &test_header("W1AW"), sid_a, 0, &frame, false)?;
        drop(
            ep.handle_inbound(voice_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        assert_eq!(
            ep.stream_cache
                .lock()
                .await
                .get(&Module::C)
                .map(super::StreamCache::stream_id),
            Some(sid_a),
            "A's first packet tracks A's stream"
        );

        // B's colliding packet must not replace the live entry.
        let mut voice_b = [0u8; 128];
        let n = encode_dcs_voice(&mut voice_b, &test_header("K1ABC"), sid_b, 0, &frame, false)?;
        drop(
            ep.handle_inbound(voice_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let tracked = {
            let cache = ep.stream_cache.lock().await;
            cache
                .get(&Module::C)
                .map(|entry| (entry.stream_id(), entry.header().my_call))
        };
        let (tracked_sid, tracked_my) = tracked.ok_or("module entry vanished")?;
        assert_eq!(
            tracked_sid, sid_a,
            "B's colliding packet must not replace A's tracked stream"
        );
        assert_eq!(
            tracked_my,
            Callsign::from_wire_bytes(*b"W1AW    "),
            "cached header must remain A's"
        );

        // B's foreign is_end must not clear A's entry.
        let mut eot_b = [0u8; 128];
        let n = encode_dcs_voice(&mut eot_b, &test_header("K1ABC"), sid_b, 1, &frame, true)?;
        drop(
            ep.handle_inbound(eot_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        assert_eq!(
            ep.stream_cache
                .lock()
                .await
                .get(&Module::C)
                .map(super::StreamCache::stream_id),
            Some(sid_a),
            "a foreign is_end must not clear the tracked stream"
        );

        // Only the owning stream's is_end clears the entry.
        let mut eot_a = [0u8; 128];
        let n = encode_dcs_voice(&mut eot_a, &test_header("W1AW"), sid_a, 1, &frame, true)?;
        drop(
            ep.handle_inbound(eot_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        assert!(
            ep.stream_cache.lock().await.get(&Module::C).is_none(),
            "the owning stream's is_end clears the entry"
        );
        Ok(())
    }

    /// Extract the stream id shared by every [`super::VoiceEvent`] variant.
    const fn stream_of(ev: &super::VoiceEvent) -> StreamId {
        match ev {
            super::VoiceEvent::StreamStart { stream_id, .. }
            | super::VoiceEvent::Frame { stream_id, .. }
            | super::VoiceEvent::StreamEnd { stream_id, .. } => *stream_id,
        }
    }

    /// While a module tracks a live stream, a second talker's events
    /// must not be published onto the cross-protocol bus at all;
    /// otherwise `DCS`-side re-encoders emit B's audio framed under
    /// A's cached header.
    #[tokio::test]
    async fn second_talker_events_are_not_published_while_stream_tracked() -> TestResult {
        use tokio::sync::broadcast;
        let (tx, mut rx) = broadcast::channel::<super::CrossProtocolEvent>(256);
        let ep = ProtocolEndpoint::<DExtra>::new_with_voice_bus(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            Some(tx),
        );
        let peer_a = test_peer(40331);
        let peer_b = test_peer(40332);
        let sid_a = sid_of(0x1111);
        let sid_b = sid_of(0x2222);
        let frame = VoiceFrame::silence();

        for (peer, callsign) in [
            (peer_a, Callsign::from_wire_bytes(*b"W1AW    ")),
            (peer_b, Callsign::from_wire_bytes(*b"K1ABC   ")),
        ] {
            let mut link_buf = [0u8; 16];
            let n = encode_connect_link(&mut link_buf, &callsign, Module::C, Module::B)?;
            drop(
                ep.handle_inbound(link_buf.get(..n).ok_or("empty")?, peer, Instant::now())
                    .await?,
            );
        }

        // A starts a stream.
        let mut hdr_a = [0u8; 64];
        let n = encode_voice_header(&mut hdr_a, sid_a, &test_header("W1AW"))?;
        drop(
            ep.handle_inbound(hdr_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        drop(drain_bus(&mut rx));

        // B talks over it: neither B's header nor B's data may reach
        // the bus while A's stream is tracked.
        let mut hdr_b = [0u8; 64];
        let n = encode_voice_header(&mut hdr_b, sid_b, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let mut data_b = [0u8; 64];
        let n = encode_voice_data(&mut data_b, sid_b, 0, &frame)?;
        drop(
            ep.handle_inbound(data_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let events = drain_bus(&mut rx);
        assert!(
            events.iter().all(|ev| stream_of(&ev.event) != sid_b),
            "second talker's events must not reach the bus while A's stream is tracked"
        );

        // A's own data still publishes.
        let mut data_a = [0u8; 64];
        let n = encode_voice_data(&mut data_a, sid_a, 0, &frame)?;
        drop(
            ep.handle_inbound(data_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        let events = drain_bus(&mut rx);
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev.event, super::VoiceEvent::Frame { .. })
                    && stream_of(&ev.event) == sid_a),
            "the tracked stream's own frames still publish"
        );

        // After A's EOT the module is free: B's next stream publishes
        // under B's own header.
        let mut eot_a = [0u8; 64];
        let n = encode_voice_eot(&mut eot_a, sid_a, 1)?;
        drop(
            ep.handle_inbound(eot_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        drop(drain_bus(&mut rx));
        let sid_b2 = sid_of(0x3333);
        let mut hdr_b2 = [0u8; 64];
        let n = encode_voice_header(&mut hdr_b2, sid_b2, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b2.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let mut data_b2 = [0u8; 64];
        let n = encode_voice_data(&mut data_b2, sid_b2, 0, &frame)?;
        drop(
            ep.handle_inbound(data_b2.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let events = drain_bus(&mut rx);
        let frame_event = events
            .iter()
            .find(|ev| {
                matches!(ev.event, super::VoiceEvent::Frame { .. })
                    && stream_of(&ev.event) == sid_b2
            })
            .ok_or("B's post-takeover frame not published")?;
        assert_eq!(
            frame_event.cached_header.map(|h| h.my_call),
            Some(Callsign::from_wire_bytes(*b"K1ABC   ")),
            "after A's EOT, B publishes under B's own header"
        );
        Ok(())
    }

    /// `DPlus` sibling of
    /// [`dextra_stream_cache_tracks_first_talker_until_eot`]. The
    /// cadence and retransmit-byte observables prove the module cache
    /// follows the first active stream.
    #[expect(
        clippy::too_many_lines,
        reason = "test walks two interleaved talkers through a full stream lifecycle; splitting it would obscure the linear narrative"
    )]
    #[tokio::test]
    async fn dplus_stream_cache_tracks_first_talker_until_eot() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());
        let peer_a = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40311);
        let peer_b = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 40312);
        let sid_a = sid_of(0x1111);
        let sid_b = sid_of(0x2222);
        let frame = VoiceFrame::silence();

        // Full LINK1 + LINK2 for both peers.
        for (peer, callsign) in [
            (peer_a, Callsign::from_wire_bytes(*b"W1AW    ")),
            (peer_b, Callsign::from_wire_bytes(*b"K1ABC   ")),
        ] {
            let mut link1_buf = [0u8; 8];
            let n1 = encode_dplus_link1(&mut link1_buf)?;
            drop(
                ep.handle_inbound(link1_buf.get(..n1).ok_or("empty")?, peer, Instant::now())
                    .await?,
            );
            let mut link2_buf = [0u8; 32];
            let n2 = encode_dplus_link2(&mut link2_buf, &callsign)?;
            drop(
                ep.handle_inbound(link2_buf.get(..n2).ok_or("empty")?, peer, Instant::now())
                    .await?,
            );
        }

        // A starts a stream; B tries to hijack with its own header.
        let mut hdr_a = [0u8; 64];
        let n = encode_dplus_voice_header(&mut hdr_a, sid_a, &test_header("W1AW"))?;
        drop(
            ep.handle_inbound(hdr_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        let mut hdr_b = [0u8; 64];
        let n = encode_dplus_voice_header(&mut hdr_b, sid_b, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );

        // B's data frames must not advance A's retransmit cadence.
        for seq in 0_u8..25 {
            let mut data_b = [0u8; 64];
            let n = encode_dplus_voice_data(&mut data_b, sid_b, seq, &frame)?;
            let outcome = ep
                .handle_inbound(data_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?;
            assert!(
                outcome.header_retransmit.is_none(),
                "B's frame {seq} must not fire the module's header retransmit"
            );
        }

        // B's EOT must not clear A's entry.
        let mut eot_b = [0u8; 64];
        let n = encode_dplus_voice_eot(&mut eot_b, sid_b, 25)?;
        drop(
            ep.handle_inbound(eot_b.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );

        // A's 20 data frames trip the cadence exactly once with A's
        // header bytes.
        let mut retransmits = Vec::new();
        for seq in 0_u8..20 {
            let mut data = [0u8; 64];
            let n = encode_dplus_voice_data(&mut data, sid_a, seq, &frame)?;
            let outcome = ep
                .handle_inbound(data.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?;
            if let Some(bytes) = outcome.header_retransmit {
                retransmits.push(bytes);
            }
        }
        assert_eq!(
            retransmits.len(),
            1,
            "exactly one retransmit after A's 20th data frame"
        );
        let retransmitted = retransmits.first().ok_or("no retransmit")?;
        assert!(
            retransmitted.windows(4).any(|w| w == b"W1AW"),
            "retransmitted header must be A's (W1AW), not B's"
        );

        // After A's EOT, B's fresh stream owns the cache: its own 20
        // frames retransmit B's header.
        let mut eot_a = [0u8; 64];
        let n = encode_dplus_voice_eot(&mut eot_a, sid_a, 20)?;
        drop(
            ep.handle_inbound(eot_a.get(..n).ok_or("empty")?, peer_a, Instant::now())
                .await?,
        );
        let sid_b2 = sid_of(0x3333);
        let mut hdr_b2 = [0u8; 64];
        let n = encode_dplus_voice_header(&mut hdr_b2, sid_b2, &test_header("K1ABC"))?;
        drop(
            ep.handle_inbound(hdr_b2.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?,
        );
        let mut takeover_retransmits = Vec::new();
        for seq in 0_u8..20 {
            let mut data = [0u8; 64];
            let n = encode_dplus_voice_data(&mut data, sid_b2, seq, &frame)?;
            let outcome = ep
                .handle_inbound(data.get(..n).ok_or("empty")?, peer_b, Instant::now())
                .await?;
            if let Some(bytes) = outcome.header_retransmit {
                takeover_retransmits.push(bytes);
            }
        }
        assert_eq!(
            takeover_retransmits.len(),
            1,
            "B's stream owns the cadence after A ended"
        );
        let retransmitted = takeover_retransmits.first().ok_or("no retransmit")?;
        assert!(
            retransmitted.windows(5).any(|w| w == b"K1ABC"),
            "post-takeover retransmit carries B's header"
        );
        Ok(())
    }

    // ─── ClientEvicted event path ─────────────────────────────────
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
        // in its outcome. We use peer() again; the previous handle
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
        // LINK itself emits ClientLinked (not voice), so nothing on bus yet.
        assert!(
            rx.try_recv().is_err(),
            "LINK emits no cross-protocol events"
        );

        // Voice header should produce exactly one StreamStart on the bus.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        let hdr_slice = hdr_buf.get(..hdr_n).ok_or("empty")?;
        drop(ep.handle_inbound(hdr_slice, peer(), Instant::now()).await?);
        let event = rx.try_recv()?;
        assert_eq!(event.source_protocol, ProtocolKind::DExtra);
        assert_eq!(event.source_peer, peer());
        assert_eq!(event.module, Module::C);
        assert!(matches!(event.event, super::VoiceEvent::StreamStart { .. }));

        // Voice data should produce one Frame on the bus with cached header.
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

        // Voice EOT should produce one StreamEnd on the bus.
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
        // This must not panic and must not error; publish_voice_events
        // is a silent no-op when voice_bus is None.
        drop(
            ep.handle_inbound(hdr_buf.get(..hdr_n).ok_or("empty")?, peer(), Instant::now())
                .await?,
        );
        Ok(())
    }

    // ─── Read-only voice drop path ────────────────────────────────
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

        // The server session MUST still be in Linked state: the
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

    #[tokio::test]
    async fn dplus_link1_is_fail_closed_and_readonly_link2_stays_readonly() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(
            ProtocolKind::DPlus,
            Module::C,
            Arc::new(ReadOnlyAuthorizer),
        );
        let mut link1 = [0_u8; 8];
        let link1_len = encode_dplus_link1(&mut link1)?;
        drop(
            ep.handle_inbound(
                link1.get(..link1_len).ok_or("empty LINK1")?,
                peer(),
                Instant::now(),
            )
            .await?,
        );
        assert_eq!(
            ep.clients().access_of(&peer()).await,
            Some(crate::reflector::AccessPolicy::ReadOnly),
            "callsign-free transitional handles must start read-only"
        );

        let mut link2 = [0_u8; 32];
        let link2_len = encode_dplus_link2(&mut link2, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        drop(
            ep.handle_inbound(
                link2.get(..link2_len).ok_or("empty LINK2")?,
                peer(),
                Instant::now(),
            )
            .await?,
        );
        assert_eq!(
            ep.clients().access_of(&peer()).await,
            Some(crate::reflector::AccessPolicy::ReadOnly)
        );

        let mut voice = [0_u8; 64];
        let voice_len = encode_dplus_voice_header(&mut voice, sid(), &test_header("W1AW"))?;
        let outcome = ep
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty voice")?,
                peer(),
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dcs_readonly_link_drops_voice_before_core() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new(
            ProtocolKind::Dcs,
            Module::C,
            Arc::new(ReadOnlyAuthorizer),
        );
        let dcs_peer = test_peer(40551);
        drop(
            link_dcs(
                &ep,
                dcs_peer,
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                Instant::now(),
            )
            .await?,
        );
        let mut voice = [0_u8; 128];
        let voice_len = encode_dcs_voice(
            &mut voice,
            &test_header("W1AW"),
            sid(),
            0,
            &VoiceFrame::silence(),
            false,
        )?;
        let outcome = ep
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty DCS voice")?,
                dcs_peer,
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));
        let state = ep
            .clients()
            .with_handle_mut(&dcs_peer, |handle| handle.session.state_kind())
            .await
            .ok_or("DCS handle missing")?;
        assert_eq!(state, ServerStateKind::Linked);
        Ok(())
    }

    #[tokio::test]
    async fn dextra_cross_module_relink_replaces_access_and_drops_voice() -> TestResult {
        let dextra_auth = Arc::new(SwitchableAuthorizer::new());
        let dextra = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            dextra_auth.clone(),
            None,
            EndpointSettings::default(),
            HashSet::from([Module::C, Module::D]),
        );
        let dextra_peer = test_peer(40561);
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        drop(link_dextra(&dextra, dextra_peer, callsign, Module::C, Instant::now()).await?);
        assert_eq!(
            dextra.clients().access_of(&dextra_peer).await,
            Some(crate::reflector::AccessPolicy::ReadWrite)
        );
        assert_eq!(
            dextra.clients().module_of(&dextra_peer).await,
            Some(Module::C)
        );

        dextra_auth.set_read_only();
        let relink = link_dextra(&dextra, dextra_peer, callsign, Module::D, Instant::now()).await?;
        assert!(relink.events.iter().any(|event| {
            matches!(
                event,
                ServerEvent::ClientLinked {
                    module: Module::D,
                    ..
                }
            )
        }));
        assert_eq!(
            dextra.clients().access_of(&dextra_peer).await,
            Some(crate::reflector::AccessPolicy::ReadOnly)
        );
        assert_eq!(
            dextra.clients().module_of(&dextra_peer).await,
            Some(Module::D)
        );
        assert!(
            dextra
                .clients()
                .members_of_module(Module::C)
                .await
                .is_empty()
        );
        assert_eq!(
            dextra.clients().members_of_module(Module::D).await,
            vec![dextra_peer]
        );

        let mut voice = [0_u8; 64];
        let voice_len = encode_voice_header(&mut voice, sid(), &test_header("W1AW"))?;
        let voice_outcome = dextra
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty voice")?,
                dextra_peer,
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            voice_outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dcs_cross_module_relink_replaces_access_and_drops_voice() -> TestResult {
        let dcs_auth = Arc::new(SwitchableAuthorizer::new());
        let dcs = ProtocolEndpoint::<Dcs>::new_with_settings(
            ProtocolKind::Dcs,
            Module::C,
            dcs_auth.clone(),
            None,
            EndpointSettings::default(),
            HashSet::from([Module::C, Module::D]),
        );
        let dcs_peer = test_peer(40562);
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        drop(link_dcs(&dcs, dcs_peer, callsign, Module::C, Instant::now()).await?);
        assert_eq!(
            dcs.clients().access_of(&dcs_peer).await,
            Some(crate::reflector::AccessPolicy::ReadWrite)
        );
        assert_eq!(dcs.clients().module_of(&dcs_peer).await, Some(Module::C));

        dcs_auth.set_read_only();
        let relink = link_dcs(&dcs, dcs_peer, callsign, Module::D, Instant::now()).await?;
        assert!(relink.events.iter().any(|event| {
            matches!(
                event,
                ServerEvent::ClientLinked {
                    module: Module::D,
                    ..
                }
            )
        }));
        assert_eq!(
            dcs.clients().access_of(&dcs_peer).await,
            Some(crate::reflector::AccessPolicy::ReadOnly)
        );
        assert_eq!(dcs.clients().module_of(&dcs_peer).await, Some(Module::D));
        assert!(dcs.clients().members_of_module(Module::C).await.is_empty());
        assert_eq!(
            dcs.clients().members_of_module(Module::D).await,
            vec![dcs_peer]
        );

        let mut voice = [0_u8; 128];
        let voice_len = encode_dcs_voice(
            &mut voice,
            &test_header("W1AW"),
            sid(),
            0,
            &VoiceFrame::silence(),
            false,
        )?;
        let voice_outcome = dcs
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty DCS voice")?,
                dcs_peer,
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            voice_outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dplus_link1_stays_readonly_until_accepted_readwrite_link2() -> TestResult {
        let dplus = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, allow_all());
        let dplus_peer = test_peer(40563);
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        let mut link1 = [0_u8; 8];
        let link1_len = encode_dplus_link1(&mut link1)?;
        drop(
            dplus
                .handle_inbound(
                    link1.get(..link1_len).ok_or("empty LINK1")?,
                    dplus_peer,
                    Instant::now(),
                )
                .await?,
        );
        assert_eq!(
            dplus.clients().access_of(&dplus_peer).await,
            Some(crate::reflector::AccessPolicy::ReadOnly)
        );
        assert_eq!(dplus.clients().module_of(&dplus_peer).await, None);

        let mut link2 = [0_u8; 32];
        let link2_len = encode_dplus_link2(&mut link2, &callsign)?;
        let link2_outcome = dplus
            .handle_inbound(
                link2.get(..link2_len).ok_or("empty LINK2")?,
                dplus_peer,
                Instant::now(),
            )
            .await?;
        assert!(
            link2_outcome
                .txs
                .iter()
                .any(|(payload, _)| { payload.windows(4).any(|window| window == b"OKRW") })
        );
        assert_eq!(
            dplus.clients().access_of(&dplus_peer).await,
            Some(crate::reflector::AccessPolicy::ReadWrite)
        );
        assert_eq!(
            dplus.clients().module_of(&dplus_peer).await,
            Some(Module::C)
        );

        let mut voice = [0_u8; 64];
        let voice_len = encode_dplus_voice_header(&mut voice, sid(), &test_header("W1AW"))?;
        let voice_outcome = dplus
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty voice")?,
                dplus_peer,
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            voice_outcome.events.first(),
            Some(ServerEvent::ClientStreamStarted { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn dplus_accepted_relink_replaces_access_policy() -> TestResult {
        let dplus_auth = Arc::new(SwitchableAuthorizer::new());
        let dplus =
            ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, dplus_auth.clone());
        let dplus_peer = test_peer(40564);
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        let mut link1 = [0_u8; 8];
        let link1_len = encode_dplus_link1(&mut link1)?;
        drop(
            dplus
                .handle_inbound(
                    link1.get(..link1_len).ok_or("empty LINK1")?,
                    dplus_peer,
                    Instant::now(),
                )
                .await?,
        );
        let mut link2 = [0_u8; 32];
        let link2_len = encode_dplus_link2(&mut link2, &callsign)?;
        let link2_bytes = link2.get(..link2_len).ok_or("empty LINK2")?;
        drop(
            dplus
                .handle_inbound(link2_bytes, dplus_peer, Instant::now())
                .await?,
        );
        dplus_auth.set_read_only();
        drop(
            dplus
                .handle_inbound(link2_bytes, dplus_peer, Instant::now())
                .await?,
        );
        assert_eq!(
            dplus.clients().access_of(&dplus_peer).await,
            Some(crate::reflector::AccessPolicy::ReadOnly)
        );

        let mut voice = [0_u8; 64];
        let voice_len = encode_dplus_voice_header(&mut voice, sid(), &test_header("W1AW"))?;
        let voice_outcome = dplus
            .handle_inbound(
                voice.get(..voice_len).ok_or("empty voice")?,
                dplus_peer,
                Instant::now(),
            )
            .await?;
        assert!(matches!(
            voice_outcome.events.first(),
            Some(ServerEvent::VoiceFromReadOnlyDropped { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejected_dplus_relink_does_not_apply_authorizer_policy() -> TestResult {
        let authorizer = Arc::new(SwitchableAuthorizer::new());
        let ep = ProtocolEndpoint::<DPlus>::new(ProtocolKind::DPlus, Module::C, authorizer.clone());
        let dplus_peer = test_peer(40565);
        let mut link1 = [0_u8; 8];
        let link1_len = encode_dplus_link1(&mut link1)?;
        drop(
            ep.handle_inbound(
                link1.get(..link1_len).ok_or("empty LINK1")?,
                dplus_peer,
                Instant::now(),
            )
            .await?,
        );
        let mut initial = [0_u8; 32];
        let initial_len =
            encode_dplus_link2(&mut initial, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        drop(
            ep.handle_inbound(
                initial.get(..initial_len).ok_or("empty LINK2")?,
                dplus_peer,
                Instant::now(),
            )
            .await?,
        );

        authorizer.set_read_only();
        let mut mismatched = [0_u8; 32];
        let mismatch_len =
            encode_dplus_link2(&mut mismatched, &Callsign::from_wire_bytes(*b"K1ABC   "))?;
        let outcome = ep
            .handle_inbound(
                mismatched.get(..mismatch_len).ok_or("empty LINK2")?,
                dplus_peer,
                Instant::now(),
            )
            .await?;
        let mut busy = [0_u8; 16];
        let busy_len = encode_link2_reply(&mut busy, Link2Result::Busy)?;
        assert_eq!(
            outcome.txs.first().map(|tx| tx.0.as_slice()),
            busy.get(..busy_len)
        );
        assert_eq!(
            ep.clients().access_of(&dplus_peer).await,
            Some(crate::reflector::AccessPolicy::ReadWrite),
            "a core-rejected re-link must not change the established policy"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dextra_unconfigured_module_is_rejected_before_authorization() -> TestResult {
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            Arc::new(PanicAuthorizer),
            None,
            EndpointSettings::default(),
            configured_modules(),
        );
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        let outcome = link_dextra(&ep, peer(), callsign, Module::D, Instant::now()).await?;
        let mut nak = [0_u8; 16];
        let nak_len = encode_dextra_nak(&mut nak, &callsign, Module::D)?;
        assert_eq!(
            outcome.txs.first().map(|tx| tx.0.as_slice()),
            nak.get(..nak_len)
        );
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::ClientRejected {
                reason: ClientRejectedReason::UnknownModule,
                ..
            })
        ));
        assert!(ep.clients().is_empty().await);
        Ok(())
    }

    #[tokio::test]
    async fn dcs_unconfigured_module_is_rejected_before_authorization() -> TestResult {
        let ep = ProtocolEndpoint::<Dcs>::new_with_settings(
            ProtocolKind::Dcs,
            Module::C,
            Arc::new(PanicAuthorizer),
            None,
            EndpointSettings::default(),
            configured_modules(),
        );
        let callsign = Callsign::from_wire_bytes(*b"W1AW    ");
        let outcome = link_dcs(&ep, peer(), callsign, Module::D, Instant::now()).await?;
        let mut nak = [0_u8; 32];
        let nak_len = encode_dcs_nak(&mut nak, &callsign, Module::D)?;
        assert_eq!(
            outcome.txs.first().map(|tx| tx.0.as_slice()),
            nak.get(..nak_len)
        );
        assert!(matches!(
            outcome.events.first(),
            Some(ServerEvent::ClientRejected {
                reason: ClientRejectedReason::UnknownModule,
                ..
            })
        ));
        assert!(ep.clients().is_empty().await);
        Ok(())
    }

    // ─── config knobs must have runtime effect ────────────────────

    fn test_peer(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    async fn link_dextra(
        ep: &ProtocolEndpoint<DExtra>,
        peer: SocketAddr,
        callsign: Callsign,
        module: Module,
        now: Instant,
    ) -> Result<EndpointOutcome<DExtra>, Box<dyn std::error::Error>> {
        let mut buf = [0u8; 16];
        let n = encode_connect_link(&mut buf, &callsign, module, Module::B)?;
        Ok(ep
            .handle_inbound(buf.get(..n).ok_or("link empty")?, peer, now)
            .await?)
    }

    async fn link_dcs(
        ep: &ProtocolEndpoint<Dcs>,
        peer: SocketAddr,
        callsign: Callsign,
        module: Module,
        now: Instant,
    ) -> Result<EndpointOutcome<Dcs>, Box<dyn std::error::Error>> {
        let mut buf = vec![0u8; 600];
        let n = encode_dcs_link(
            &mut buf,
            &callsign,
            Module::B,
            module,
            &dcs_reflector_cs(),
            DcsGatewayType::Repeater,
        )?;
        Ok(ep
            .handle_inbound(buf.get(..n).ok_or("link empty")?, peer, now)
            .await?)
    }

    #[tokio::test]
    async fn configured_tx_rate_limit_reaches_client_handle() -> TestResult {
        let settings = EndpointSettings {
            tx_rate_limit_frames_per_sec: 2.0,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let now = Instant::now();
        drop(
            link_dextra(
                &ep,
                peer(),
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                now,
            )
            .await?,
        );

        // Burst capacity is one second at the configured rate: two
        // tokens at the same instant, then the bucket runs dry.
        assert!(ep.clients().try_consume_tx_token(&peer(), now).await);
        assert!(ep.clients().try_consume_tx_token(&peer(), now).await);
        assert!(
            !ep.clients().try_consume_tx_token(&peer(), now).await,
            "third frame at the same instant must be rate-limited"
        );

        // Refill runs at the configured rate.
        let later = now + Duration::from_secs(1);
        assert!(ep.clients().try_consume_tx_token(&peer(), later).await);
        assert!(ep.clients().try_consume_tx_token(&peer(), later).await);
        assert!(
            !ep.clients().try_consume_tx_token(&peer(), later).await,
            "refill is capped at the configured burst"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dextra_total_client_cap_rejects_next_link() -> TestResult {
        let settings = EndpointSettings {
            max_total_clients: 2,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        for port in [41001, 41002] {
            let outcome = link_dextra(&ep, test_peer(port), cs, Module::C, Instant::now()).await?;
            assert_eq!(outcome.txs.len(), 1, "peer {port} links under the cap");
        }
        assert_eq!(ep.clients().len().await, 2);

        let outcome = link_dextra(&ep, test_peer(41003), cs, Module::C, Instant::now()).await?;
        let (payload, dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert_eq!(*dst, test_peer(41003));
        assert_eq!(payload.len(), 14, "DExtra NAK is 14 bytes");
        assert!(
            payload.windows(3).any(|w| w == b"NAK"),
            "cap hit must NAK, got {payload:?}"
        );
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientRejected { .. })),
            "cap hit emits ClientRejected"
        );
        assert_eq!(
            ep.clients().len().await,
            2,
            "pool must not grow past the cap"
        );
        assert!(!ep.clients().contains(&test_peer(41003)).await);
        Ok(())
    }

    #[tokio::test]
    async fn dextra_per_module_cap_rejects_next_link() -> TestResult {
        let settings = EndpointSettings {
            max_clients_per_module: 1,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            HashSet::from([Module::C, Module::D]),
        );
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        drop(link_dextra(&ep, test_peer(41011), cs, Module::C, Instant::now()).await?);
        assert_eq!(ep.clients().members_of_module(Module::C).await.len(), 1);

        // Second peer on the SAME module is rejected…
        let outcome = link_dextra(&ep, test_peer(41012), cs, Module::C, Instant::now()).await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert!(
            payload.windows(3).any(|w| w == b"NAK"),
            "per-module cap hit must NAK"
        );
        assert!(
            !ep.clients().contains(&test_peer(41012)).await,
            "rejected peer must not enter the pool"
        );

        // …but a DIFFERENT module still has room.
        let outcome = link_dextra(&ep, test_peer(41012), cs, Module::D, Instant::now()).await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no tx")?;
        assert!(
            payload.windows(3).any(|w| w == b"ACK"),
            "the cap is per module, not global"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dplus_total_cap_rejects_link1_when_pool_full() -> TestResult {
        let settings = EndpointSettings {
            max_total_clients: 1,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DPlus>::new_with_settings(
            ProtocolKind::DPlus,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        // First client completes the handshake.
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        let link1_slice = link1_buf.get(..n1).ok_or("empty")?;
        drop(
            ep.handle_inbound(link1_slice, test_peer(41021), Instant::now())
                .await?,
        );
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        drop(
            ep.handle_inbound(
                link2_buf.get(..n2).ok_or("empty")?,
                test_peer(41021),
                Instant::now(),
            )
            .await?,
        );
        assert_eq!(ep.clients().len().await, 1);

        // Second client's LINK1 hits the total cap: BUSY, no entry.
        let outcome = ep
            .handle_inbound(link1_slice, test_peer(41022), Instant::now())
            .await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert!(
            payload.windows(4).any(|w| w == b"BUSY"),
            "total-cap hit at LINK1 must reply BUSY, got {payload:?}"
        );
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientRejected { .. })),
            "cap hit emits ClientRejected"
        );
        assert_eq!(ep.clients().len().await, 1, "no pool entry past the cap");
        assert!(!ep.clients().contains(&test_peer(41022)).await);
        Ok(())
    }

    #[tokio::test]
    async fn dplus_per_module_cap_rejects_link2_and_discards_transitional_handle() -> TestResult {
        let settings = EndpointSettings {
            max_clients_per_module: 1,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DPlus>::new_with_settings(
            ProtocolKind::DPlus,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        let link1_slice = link1_buf.get(..n1).ok_or("empty")?;
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        let link2_slice = link2_buf.get(..n2).ok_or("empty")?;

        // First client fills module C.
        drop(
            ep.handle_inbound(link1_slice, test_peer(41031), Instant::now())
                .await?,
        );
        drop(
            ep.handle_inbound(link2_slice, test_peer(41031), Instant::now())
                .await?,
        );
        assert_eq!(ep.clients().members_of_module(Module::C).await.len(), 1);

        // Second client passes LINK1 (total cap has room)…
        drop(
            ep.handle_inbound(link1_slice, test_peer(41032), Instant::now())
                .await?,
        );
        assert_eq!(ep.clients().len().await, 2, "transitional LINK1 handle");
        // …but its LINK2 hits the module cap: BUSY + the transitional
        // handle is discarded so the address doesn't linger half-open.
        let outcome = ep
            .handle_inbound(link2_slice, test_peer(41032), Instant::now())
            .await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert!(
            payload.windows(4).any(|w| w == b"BUSY"),
            "module-cap hit at LINK2 must reply BUSY, got {payload:?}"
        );
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientRejected { .. })),
            "cap hit emits ClientRejected"
        );
        assert!(
            !ep.clients().contains(&test_peer(41032)).await,
            "rejected peer must not keep a pool entry"
        );
        assert_eq!(ep.clients().len().await, 1);
        Ok(())
    }

    /// An authorizer rejection at LINK2 must discard the transitional
    /// LINK1 handle, exactly like the adjacent per-module-cap reject;
    /// otherwise a denied client that loops the handshake pins a
    /// `max_total_clients` slot forever (each LINK1 refreshes
    /// `last_heard`, so the idle sweep never reclaims it).
    #[tokio::test]
    async fn dplus_authorizer_reject_discards_transitional_handle() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new(
            ProtocolKind::DPlus,
            Module::C,
            Arc::new(DenyAllAuthorizer),
        );
        let denied = test_peer(41041);
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        drop(
            ep.handle_inbound(link1_buf.get(..n1).ok_or("empty")?, denied, Instant::now())
                .await?,
        );
        assert_eq!(ep.clients().len().await, 1, "transitional LINK1 handle");

        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        let outcome = ep
            .handle_inbound(link2_buf.get(..n2).ok_or("empty")?, denied, Instant::now())
            .await?;
        assert!(!outcome.txs.is_empty(), "authorizer reject still replies");
        assert!(
            !ep.clients().contains(&denied).await,
            "denied peer must not keep a pool entry"
        );
        assert_eq!(
            ep.clients().len().await,
            0,
            "authorizer reject discards the transitional handle"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dcs_total_client_cap_rejects_next_link() -> TestResult {
        let settings = EndpointSettings {
            max_total_clients: 1,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<Dcs>::new_with_settings(
            ProtocolKind::Dcs,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        drop(link_dcs(&ep, test_peer(41041), cs, Module::C, Instant::now()).await?);
        assert_eq!(ep.clients().len().await, 1);

        let outcome = link_dcs(&ep, test_peer(41042), cs, Module::C, Instant::now()).await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert_eq!(payload.len(), 14, "DCS NAK is 14 bytes");
        assert!(
            payload.windows(3).any(|w| w == b"NAK"),
            "cap hit must NAK, got {payload:?}"
        );
        assert_eq!(ep.clients().len().await, 1, "no pool entry past the cap");
        assert!(!ep.clients().contains(&test_peer(41042)).await);
        Ok(())
    }

    #[tokio::test]
    async fn dcs_per_module_cap_rejects_next_link() -> TestResult {
        let settings = EndpointSettings {
            max_clients_per_module: 1,
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<Dcs>::new_with_settings(
            ProtocolKind::Dcs,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let cs = Callsign::from_wire_bytes(*b"W1AW    ");
        drop(link_dcs(&ep, test_peer(41051), cs, Module::C, Instant::now()).await?);

        let outcome = link_dcs(&ep, test_peer(41052), cs, Module::C, Instant::now()).await?;
        let (payload, _dst) = outcome.txs.first().ok_or("no reject tx")?;
        assert!(
            payload.windows(3).any(|w| w == b"NAK"),
            "per-module cap hit must NAK"
        );
        assert!(!ep.clients().contains(&test_peer(41052)).await);
        Ok(())
    }

    #[tokio::test]
    async fn handle_tick_evicts_idle_clients_and_keeps_active_ones() -> TestResult {
        let settings = EndpointSettings {
            keepalive_inactivity_timeout: Duration::from_secs(30),
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let base = Instant::now();
        let peer_a = test_peer(41061);
        let peer_b = test_peer(41062);
        drop(
            link_dextra(
                &ep,
                peer_a,
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                base,
            )
            .await?,
        );
        drop(
            link_dextra(
                &ep,
                peer_b,
                Callsign::from_wire_bytes(*b"K1ABC   "),
                Module::C,
                base,
            )
            .await?,
        );

        // B polls 20 s in, refreshing its last_heard.
        let mut poll_buf = [0u8; 16];
        let n = encode_poll(&mut poll_buf, &Callsign::from_wire_bytes(*b"K1ABC   "))?;
        drop(
            ep.handle_inbound(
                poll_buf.get(..n).ok_or("empty")?,
                peer_b,
                base + Duration::from_secs(20),
            )
            .await?,
        );

        // Sweep at 35 s: A has been silent past the timeout, B not.
        drop(ep.handle_tick(base + Duration::from_secs(35)).await);
        assert!(
            !ep.clients().contains(&peer_a).await,
            "silent peer must be evicted after the inactivity timeout"
        );
        assert!(
            ep.clients().contains(&peer_b).await,
            "recently active peer must survive the sweep"
        );
        assert!(
            ep.clients().members_of_module(Module::C).await == vec![peer_b],
            "module reverse index follows the eviction"
        );

        // The eviction surfaces as ClientEvicted on the next inbound.
        let outcome = ep
            .handle_inbound(
                poll_buf.get(..n).ok_or("empty")?,
                peer_b,
                base + Duration::from_secs(36),
            )
            .await?;
        assert!(
            outcome
                .events
                .iter()
                .any(|ev| matches!(ev, ServerEvent::ClientEvicted { .. })),
            "sweep eviction emits ClientEvicted"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_tick_drops_stale_stream_cache_entry() -> TestResult {
        let settings = EndpointSettings {
            voice_inactivity_timeout: Duration::from_secs(2),
            keepalive_inactivity_timeout: Duration::from_secs(3600),
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let base = Instant::now();
        drop(
            link_dextra(
                &ep,
                peer(),
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                base,
            )
            .await?,
        );

        // Start a stream and feed 5 frames, then let it stall.
        let mut hdr_buf = [0u8; 64];
        let hdr_n = encode_voice_header(&mut hdr_buf, sid(), &test_header("W1AW"))?;
        drop(
            ep.handle_inbound(hdr_buf.get(..hdr_n).ok_or("empty")?, peer(), base)
                .await?,
        );
        let frame = VoiceFrame::silence();
        for seq in 0_u8..5 {
            let mut data_buf = [0u8; 64];
            let data_n = encode_voice_data(&mut data_buf, sid(), seq, &frame)?;
            drop(
                ep.handle_inbound(data_buf.get(..data_n).ok_or("empty")?, peer(), base)
                    .await?,
            );
        }

        // Sweep past the voice-inactivity timeout evicts the entry.
        drop(ep.handle_tick(base + Duration::from_secs(3)).await);

        // Without the cache entry, 20 further frames can never fire
        // the header-retransmit cadence (the counter sat at 5, so a
        // surviving entry WOULD have fired within these 20).
        for seq in 5_u8..25 {
            let mut data_buf = [0u8; 64];
            let data_n = encode_voice_data(&mut data_buf, sid(), seq, &frame)?;
            let outcome = ep
                .handle_inbound(
                    data_buf.get(..data_n).ok_or("empty")?,
                    peer(),
                    base + Duration::from_secs(3),
                )
                .await?;
            assert!(
                outcome.header_retransmit.is_none(),
                "stale entry must be gone after the sweep (frame {seq})"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn handle_tick_sends_one_keepalive_per_linked_peer_per_due_interval() -> TestResult {
        let settings = EndpointSettings {
            reflector_callsign: Callsign::from_wire_bytes(*b"REF030  "),
            keepalive_interval: Duration::from_secs(10),
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<DExtra>::new_with_settings(
            ProtocolKind::DExtra,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let base = Instant::now();
        let peer_a = test_peer(41071);
        let peer_b = test_peer(41072);
        drop(
            link_dextra(
                &ep,
                peer_a,
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                base,
            )
            .await?,
        );
        drop(
            link_dextra(
                &ep,
                peer_b,
                Callsign::from_wire_bytes(*b"K1ABC   "),
                Module::C,
                base,
            )
            .await?,
        );

        // First sweep is due: exactly one keepalive per linked peer,
        // in the 9-byte reflector-callsign form.
        let txs = ep.handle_tick(base).await;
        assert_eq!(txs.len(), 2, "one keepalive per linked peer");
        for expected_peer in [peer_a, peer_b] {
            let matching: Vec<_> = txs
                .iter()
                .filter(|(_, dst)| *dst == expected_peer)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "exactly one keepalive to {expected_peer}"
            );
            let (payload, _) = matching.first().ok_or("missing keepalive")?;
            assert_eq!(payload.len(), 9, "DExtra keepalive is 9 bytes");
            assert_eq!(payload.get(..8), Some(b"REF030  ".as_slice()));
            assert_eq!(payload.get(8), Some(&0x00));
        }

        // Half an interval later: nothing due.
        let txs = ep.handle_tick(base + Duration::from_secs(5)).await;
        assert!(txs.is_empty(), "keepalive not due yet");

        // A full interval after the first send: due again.
        let txs = ep.handle_tick(base + Duration::from_secs(10)).await;
        assert_eq!(txs.len(), 2, "one keepalive per peer per due tick");
        Ok(())
    }

    #[tokio::test]
    async fn handle_tick_dplus_keepalive_is_poll_echo_form() -> TestResult {
        let ep = ProtocolEndpoint::<DPlus>::new_with_settings(
            ProtocolKind::DPlus,
            Module::C,
            allow_all(),
            None,
            EndpointSettings::default(),
            configured_modules(),
        );
        let base = Instant::now();
        // Fully linked peer A.
        let mut link1_buf = [0u8; 8];
        let n1 = encode_dplus_link1(&mut link1_buf)?;
        let link1_slice = link1_buf.get(..n1).ok_or("empty")?;
        drop(
            ep.handle_inbound(link1_slice, test_peer(41081), base)
                .await?,
        );
        let mut link2_buf = [0u8; 32];
        let n2 = encode_dplus_link2(&mut link2_buf, &Callsign::from_wire_bytes(*b"W1AW    "))?;
        drop(
            ep.handle_inbound(link2_buf.get(..n2).ok_or("empty")?, test_peer(41081), base)
                .await?,
        );
        // Mid-handshake peer B (LINK1 only, no module yet) gets none.
        drop(
            ep.handle_inbound(link1_slice, test_peer(41082), base)
                .await?,
        );

        let txs = ep.handle_tick(base).await;
        assert_eq!(txs.len(), 1, "only fully linked peers get keepalives");
        let (payload, dst) = txs.first().ok_or("no keepalive")?;
        assert_eq!(*dst, test_peer(41081));
        assert_eq!(
            payload.as_slice(),
            &[0x03, 0x60, 0x00],
            "DPlus keepalive is the 3-byte poll form"
        );
        Ok(())
    }

    #[tokio::test]
    async fn handle_tick_dcs_keepalive_is_xlxd_22_byte_form() -> TestResult {
        let settings = EndpointSettings {
            reflector_callsign: Callsign::from_wire_bytes(*b"REF030  "),
            ..EndpointSettings::default()
        };
        let ep = ProtocolEndpoint::<Dcs>::new_with_settings(
            ProtocolKind::Dcs,
            Module::C,
            allow_all(),
            None,
            settings,
            configured_modules(),
        );
        let base = Instant::now();
        // W1AW links its module B to reflector module C.
        drop(
            link_dcs(
                &ep,
                test_peer(41091),
                Callsign::from_wire_bytes(*b"W1AW    "),
                Module::C,
                base,
            )
            .await?,
        );

        let txs = ep.handle_tick(base).await;
        assert_eq!(txs.len(), 1);
        let (payload, dst) = txs.first().ok_or("no keepalive")?;
        assert_eq!(*dst, test_peer(41091));
        assert_eq!(payload.len(), 22, "DCS keepalive is 22 bytes");
        assert_eq!(payload.get(..7), Some(b"REF030 ".as_slice()));
        assert_eq!(payload.get(7), Some(&b'C'), "linked reflector module");
        assert_eq!(payload.get(8), Some(&b' '));
        assert_eq!(payload.get(9..16), Some(b"W1AW   ".as_slice()));
        assert_eq!(payload.get(16), Some(&b'B'), "client module");
        assert_eq!(payload.get(17), Some(&b'B'), "client module repeated");
        assert_eq!(
            payload.get(18..22),
            Some([0x0A, 0x00, 0x20, 0x20].as_slice())
        );
        Ok(())
    }
}
