//! Protocol-erased client session core.
//!
//! [`SessionCore`] is the runtime-erased state machine that drives a
//! single D-STAR reflector client session. It is wrapped by the
//! typestate [`Session<P, S>`][session]: the typestate sits on top
//! of this struct and forwards every method. Keeping the state machine
//! monomorphization-free avoids duplicating the machine body for each
//! protocol.
//!
//! Handles connect / keepalive / disconnect and voice TX/RX.
//!
//! [session]: crate::session::client::Protocol

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::codec::{dcs, dextra, dplus};
use crate::error::{DcsError, Error, ProtocolError, StateError, StateOperation};
use crate::header::DstarHeader;
use crate::session::driver::Transmit;
use crate::session::outbox::{OutboundPacket, Outbox};
use crate::session::timer_wheel::{TimerId, TimerWheel};
use crate::types::{
    Callsign, DcsReflector, FrameSequence, Module, ModuleSlot, ProtocolKind, RepeaterIdentity,
    RoutingCallsign, StreamId,
};
use crate::validator::{Diagnostic, VecSink};
use crate::voice::VoiceFrame;

use super::event::{DisconnectReason, Event, VoiceEndReason};
use super::protocol::Protocol;
use super::state::ClientStateKind;

/// Named timer: keepalive poll.
const TIMER_KEEPALIVE: TimerId = TimerId::Keepalive;
/// Named timer: keepalive inactivity (peer silent for too long).
const TIMER_KEEPALIVE_INACTIVITY: TimerId = TimerId::KeepaliveInactivity;
/// Named timer: waiting for disconnect ACK from reflector.
const TIMER_DISCONNECT_DEADLINE: TimerId = TimerId::DisconnectDeadline;
/// Named timer: voice frames stopped flowing on the active stream.
///
/// Re-armed on every voice frame / header for the active stream id.
/// On expiry (`now >= last_frame + VOICE_INACTIVITY_TIMEOUT`),
/// [`SessionCore::handle_timeout`] synthesizes a
/// [`VoiceEndReason::Inactivity`] event and clears
/// [`SessionCore::active_rx_stream`]. Without this timer, a stream
/// whose source drops the EOT packet (or whose reflector keeps
/// retransmitting the header after voice data stopped) leaves the
/// session stuck reporting "still talking" forever and suppressing
/// every subsequent header retransmit as a duplicate.
const TIMER_VOICE_INACTIVITY: TimerId = TimerId::VoiceInactivity;

/// Local receive policy: 500 ms without an active-stream voice frame before
/// another stream identifier may take over. Duplicate headers do not renew
/// that activity. The interval is independent of the voice-inactivity timer.
const STREAM_TAKEOVER_THRESHOLD: Duration = Duration::from_millis(500);

/// Internal protocol-erased event record.
///
/// [`SessionCore::pop_event`] is generic over `P: Protocol` and
/// converts each `RawEvent` into an [`Event<P>`] at drain time.
#[derive(Debug, Clone)]
enum RawEvent {
    /// Transitioned to `Connected`.
    Connected {
        /// Peer that accepted the link.
        peer: SocketAddr,
    },
    /// Transitioned to `Closed`.
    Disconnected {
        /// Why.
        reason: DisconnectReason,
    },
    /// Reflector poll echo received.
    PollEcho {
        /// Peer that sent the echo.
        peer: SocketAddr,
    },
    /// Voice stream started: header arrived from the reflector.
    VoiceStart {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: DstarHeader,
    },
    /// Voice data frame within an active stream.
    VoiceFrame {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Frame sequence number.
        seq: u8,
        /// 9 AMBE bytes + 3 slow data bytes.
        frame: VoiceFrame,
    },
    /// Voice stream ended.
    VoiceEnd {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Reason the stream ended.
        reason: VoiceEndReason,
    },
}

/// Protocol-erased session core.
///
/// Holds all mutable state for one reflector session. The typestate
/// `Session<P, S>` wrapper forwards most calls straight through; the
/// core does not itself enforce state transitions at compile time;
/// that discipline is the wrapper's job.
pub struct SessionCore {
    /// Which protocol this session speaks.
    kind: ProtocolKind,
    /// Logged-in client callsign.
    callsign: RoutingCallsign,
    /// Client local module letter.
    local_module: Module,
    /// Reflector module letter linked (or linking) to.
    reflector_module: Module,
    /// Protocol-specific configuration, including a required DCS target.
    protocol: super::ClientProtocol,
    /// Peer address of the reflector.
    peer: SocketAddr,
    /// Runtime state discriminator.
    state: ClientStateKind,
    /// Outbound packet queue.
    outbox: Outbox,
    /// Timer wheel.
    timers: TimerWheel,
    /// Most-recently-popped outbound packet, held so
    /// [`SessionCore::pop_transmit`] can return a borrow into
    /// the owned payload across multiple calls.
    current_tx: Option<OutboundPacket>,
    /// Queued raw events awaiting [`SessionCore::pop_event`].
    events: VecDeque<RawEvent>,
    /// Cached `DPlus` host list (only populated after TCP auth).
    ///
    /// `None` for `DExtra`/`Dcs`; `Some` after
    /// [`SessionCore::attach_host_list`] transitions a `DPlus`
    /// session from `Configured` to `Authenticated`.
    host_list: Option<dplus::HostList>,
    /// Most recent TX voice header, populated by [`Self::enqueue_send_header`].
    ///
    /// DCS embeds the full D-STAR header in every 100-byte voice
    /// frame, so [`Self::enqueue_send_voice`] / [`Self::enqueue_send_eot`]
    /// must be able to retrieve the header that started the stream.
    /// `DPlus` and `DExtra` do not embed the header in voice frames,
    /// so the cache is not consulted on those protocols; it is still
    /// populated for symmetry and future header retransmit support.
    cached_tx_header: Option<DstarHeader>,
    /// Stream id of the currently-active incoming voice stream, or
    /// `None` when no stream is active.
    ///
    /// D-STAR protocols retransmit the voice header periodically
    /// (DExtra/DPlus: once per ~21-frame superframe; DCS: embedded in
    /// every voice frame, so every `seq == 0` frame carries a fresh
    /// copy). This is a protocol feature so that late-joining listeners
    /// can decode the stream even if they missed the initial header.
    ///
    /// Without tracking the "active stream" here, every retransmitted
    /// header would surface as a fresh [`RawEvent::VoiceStart`] to the
    /// consumer, which resets any decoder state kept per stream and
    /// sounds like the first few frames of the stream repeating over
    /// and over (because those are the only frames the decoder ever
    /// converges on before being reset again).
    ///
    /// Populated by the header-emitting branch of
    /// [`Self::handle_input`]; cleared on [`RawEvent::VoiceEnd`]. A
    /// mid-stream change in `stream_id` (new talker on the same
    /// module) triggers a synthesized `VoiceEnd` for the outgoing
    /// stream id followed by a `VoiceStart` for the new one.
    active_rx_stream: Option<StreamId>,
    /// Wall-clock instant of the most recent voice frame or header
    /// that belonged to [`Self::active_rx_stream`].
    ///
    /// Used by [`Self::emit_voice_start_if_new`] to gate takeover after
    /// [`STREAM_TAKEOVER_THRESHOLD`]. Cleared on stream end or session reset;
    /// frames from concurrent streams never refresh this timestamp.
    last_voice_activity_at: Option<Instant>,
    /// Diagnostic sink for lenient parser warnings.
    ///
    /// Concrete [`VecSink`] owned by the core so [`Self::drain_diagnostics`]
    /// can return them as a `Vec`. The `Session<P, S>` wrapper exposes
    /// these via `Session::diagnostics()`.
    diagnostics: VecSink,
}

impl std::fmt::Debug for SessionCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCore")
            .field("kind", &self.kind)
            .field("callsign", &self.callsign)
            .field("local_module", &self.local_module)
            .field("reflector_module", &self.reflector_module)
            .field("protocol", &self.protocol)
            .field("peer", &self.peer)
            .field("state", &self.state)
            .field("outbox", &self.outbox)
            .field("timers", &self.timers)
            .field("current_tx", &self.current_tx)
            .field("events", &self.events)
            .field("host_list", &self.host_list)
            .field("cached_tx_header", &self.cached_tx_header)
            .field("active_rx_stream", &self.active_rx_stream)
            .field("last_voice_activity_at", &self.last_voice_activity_at)
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

impl SessionCore {
    // ── Stream bookkeeping helpers ────────────────────────────

    /// Emit [`RawEvent::VoiceStart`] for a new incoming stream,
    /// suppressing the event for retransmitted headers of the
    /// currently-active stream.
    ///
    /// D-STAR reflectors retransmit the voice header (DExtra/DPlus:
    /// per super-frame, DCS: embedded in every voice frame) so
    /// late-joining clients can decode. Without the
    /// [`Self::active_rx_stream`] check, every retransmit would
    /// surface as a fresh `VoiceStart`, which typically resets the
    /// consumer's per-stream AMBE decoder state, so the user hears the
    /// first few frames of the stream looping indefinitely.
    ///
    /// If `stream_id` differs from the current active stream, an
    /// [`RawEvent::VoiceEnd`] with
    /// [`VoiceEndReason::Inactivity`] is synthesized for the old
    /// stream before the new `VoiceStart`. That covers the case where a
    /// new talker takes the module mid-flight without an EOT from
    /// the previous one.
    fn emit_voice_start_if_new(&mut self, now: Instant, stream_id: StreamId, header: DstarHeader) {
        match self.active_rx_stream {
            Some(current) if current == stream_id => {
                tracing::trace!(
                    target: "dstar_gateway_core::session::client",
                    stream_id = format_args!("{:#06X}", stream_id.get()),
                    "suppressing retransmitted voice header for active stream"
                );
                // Header retransmits alone do not show that voice data
                // is still flowing (the reflector keeps re-sending the
                // header for late joiners even after the source has
                // stopped). Only the VoiceFrame dispatch re-arms the
                // inactivity timer.
            }
            Some(old_sid) => {
                // A different identifier may take over only after the
                // active stream's voice-activity threshold expires.
                let active_stale = self
                    .last_voice_activity_at
                    .is_none_or(|t| now.saturating_duration_since(t) >= STREAM_TAKEOVER_THRESHOLD);
                if !active_stale {
                    tracing::debug!(
                        target: "dstar_gateway_core::session::client",
                        active_sid = format_args!("{:#06X}", old_sid.get()),
                        concurrent_sid = format_args!("{:#06X}", stream_id.get()),
                        "suppressing concurrent stream header (active stream still live)"
                    );
                    return;
                }
                tracing::debug!(
                    target: "dstar_gateway_core::session::client",
                    old_sid = format_args!("{:#06X}", old_sid.get()),
                    new_sid = format_args!("{:#06X}", stream_id.get()),
                    "mid-stream sid change: synthesizing VoiceEnd for old + VoiceStart for new"
                );
                self.events.push_back(RawEvent::VoiceEnd {
                    stream_id: old_sid,
                    reason: VoiceEndReason::Inactivity,
                });
                self.active_rx_stream = Some(stream_id);
                self.events
                    .push_back(RawEvent::VoiceStart { stream_id, header });
                self.arm_voice_inactivity(now);
                self.last_voice_activity_at = Some(now);
            }
            None => {
                tracing::debug!(
                    target: "dstar_gateway_core::session::client",
                    stream_id = format_args!("{:#06X}", stream_id.get()),
                    "new voice stream: emitting VoiceStart"
                );
                self.active_rx_stream = Some(stream_id);
                self.events
                    .push_back(RawEvent::VoiceStart { stream_id, header });
                self.arm_voice_inactivity(now);
                self.last_voice_activity_at = Some(now);
            }
        }
    }

    /// Emit [`RawEvent::VoiceEnd`] and clear the active-stream tracker.
    ///
    /// EOTs for non-active streams (e.g. a concurrent stream whose
    /// `VoiceStart` was suppressed by [`Self::emit_voice_start_if_new`])
    /// are silently ignored, because the consumer never saw a matching
    /// `VoiceStart`, so emitting `VoiceEnd` for that stream would
    /// be a dangling close.
    fn emit_voice_end(&mut self, stream_id: StreamId, reason: VoiceEndReason) {
        if self.active_rx_stream != Some(stream_id) {
            tracing::trace!(
                target: "dstar_gateway_core::session::client",
                stream_id = format_args!("{:#06X}", stream_id.get()),
                active_sid = ?self.active_rx_stream.map(StreamId::get),
                ?reason,
                "ignoring VoiceEnd for non-active stream (concurrent stream closing)"
            );
            return;
        }
        tracing::debug!(
            target: "dstar_gateway_core::session::client",
            stream_id = format_args!("{:#06X}", stream_id.get()),
            ?reason,
            "emitting VoiceEnd, clearing active stream tracker"
        );
        self.active_rx_stream = None;
        self.last_voice_activity_at = None;
        self.timers.clear(TIMER_VOICE_INACTIVITY);
        self.events
            .push_back(RawEvent::VoiceEnd { stream_id, reason });
    }

    // ── Construction ──────────────────────────────────────────

    /// Build a new [`SessionCore`] in [`ClientStateKind::Configured`].
    ///
    /// The session has no host list, no pending packets, and no
    /// active timers. The typestate builder ([`super::SessionBuilder`])
    /// wraps this constructor and enforces per-protocol state-transition
    /// rules.
    #[must_use]
    pub fn new(config: super::SessionConfig) -> Self {
        Self {
            kind: config.protocol.kind(),
            callsign: config.callsign,
            local_module: config.local_module,
            reflector_module: config.reflector_module,
            protocol: config.protocol,
            peer: config.peer,
            state: ClientStateKind::Configured,
            outbox: Outbox::new(),
            timers: TimerWheel::new(),
            current_tx: None,
            events: VecDeque::new(),
            host_list: None,
            cached_tx_header: None,
            active_rx_stream: None,
            last_voice_activity_at: None,
            diagnostics: VecSink::default(),
        }
    }

    /// Drain accumulated parser diagnostics.
    ///
    /// Returns everything the internal [`VecSink`] has captured since
    /// the previous drain (or since construction). The sink is empty
    /// on return. Called by `Session::diagnostics()` from the
    /// typestate wrapper.
    pub fn drain_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.diagnostics.drain().collect()
    }

    // ── Accessors ─────────────────────────────────────────────

    /// Current runtime state.
    #[must_use]
    pub const fn state_kind(&self) -> ClientStateKind {
        self.state
    }

    /// Peer address of the reflector.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Client callsign.
    #[must_use]
    pub const fn callsign(&self) -> Callsign {
        self.callsign.callsign()
    }

    /// Client local module letter.
    #[must_use]
    pub const fn local_module(&self) -> Module {
        self.local_module
    }

    /// Reflector module letter.
    #[must_use]
    pub const fn reflector_module(&self) -> Module {
        self.reflector_module
    }

    /// Runtime protocol discriminator.
    #[must_use]
    pub const fn protocol_kind(&self) -> ProtocolKind {
        self.kind
    }

    /// Cached `DPlus` host list (`None` unless authenticated).
    #[must_use]
    pub const fn host_list(&self) -> Option<&dplus::HostList> {
        self.host_list.as_ref()
    }

    // ── DPlus host list / auth ────────────────────────────────

    /// Attach a `DPlus` host list, transitioning the session from
    /// [`ClientStateKind::Configured`] to
    /// [`ClientStateKind::Authenticated`].
    ///
    /// Only valid for `DPlus` sessions. The host list is what the
    /// `dstar-gateway` shell would obtain from the
    /// `auth.dstargateway.org` TCP handshake; the core does not
    /// itself perform any I/O.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::WrongState`] if the session is not a
    /// `DPlus` session or is not in [`ClientStateKind::Configured`].
    /// The typestate wrapper prevents both cases at compile time;
    /// this runtime check is the residual safety net for direct
    /// `SessionCore` users (tests + the protocol-erased fallback path).
    pub fn attach_host_list(&mut self, list: dplus::HostList) -> Result<(), Error> {
        if self.kind != ProtocolKind::DPlus || self.state != ClientStateKind::Configured {
            return Err(Error::State(StateError::WrongState {
                operation: StateOperation::AttachHostList,
                state: self.state,
                protocol: self.kind,
            }));
        }
        self.host_list = Some(list);
        self.state = ClientStateKind::Authenticated;
        Ok(())
    }

    // ── Connect / disconnect ──────────────────────────────────

    /// Enqueue the protocol-appropriate LINK packet and transition
    /// the session to [`ClientStateKind::Connecting`].
    ///
    /// # Errors
    ///
    /// Returns [`StateError::WrongState`] outside the protocol's initial
    /// configured/authenticated state, [`StateError::DeadlineOverflow`] for
    /// an unrepresentable timer span, or [`Error::Protocol`] if a codec reports a
    /// buffer-too-small (should never happen, since the scratch
    /// buffers in this core are oversized for every known packet).
    pub fn enqueue_connect(&mut self, now: Instant) -> Result<(), Error> {
        self.validate_time(now)?;
        let expected = if self.kind == ProtocolKind::DPlus {
            ClientStateKind::Authenticated
        } else {
            ClientStateKind::Configured
        };
        if self.state != expected {
            return Err(StateError::WrongState {
                operation: StateOperation::Connect,
                state: self.state,
                protocol: self.kind,
            }
            .into());
        }
        let packet = match self.kind {
            ProtocolKind::DPlus => {
                let mut buf = [0u8; 32];
                let n = dplus::encode_link1(&mut buf)
                    .map_err(dplus::DPlusError::from)
                    .map_err(ProtocolError::DPlus)?;
                OutboundPacket {
                    dst: self.peer,
                    payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                    not_before: now,
                }
            }
            ProtocolKind::DExtra => {
                let mut buf = [0u8; 16];
                let n = dextra::encode_connect_link(
                    &mut buf,
                    self.local_identity(),
                    self.reflector_module,
                )
                .map_err(dextra::DExtraError::from)
                .map_err(ProtocolError::DExtra)?;
                OutboundPacket {
                    dst: self.peer,
                    payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                    not_before: now,
                }
            }
            ProtocolKind::Dcs => {
                let mut buf = vec![0u8; 600];
                let reflector_callsign = self.dcs_reflector_callsign();
                let n = dcs::encode_connect_link(
                    &mut buf,
                    self.local_identity(),
                    self.reflector_module,
                    reflector_callsign,
                    dcs::GatewayType::Dongle,
                )
                .map_err(DcsError::from)
                .map_err(ProtocolError::Dcs)?;
                buf.truncate(n);
                OutboundPacket {
                    dst: self.peer,
                    payload: buf,
                    not_before: now,
                }
            }
        };
        self.outbox.enqueue(packet);
        self.state = ClientStateKind::Connecting;
        self.arm_keepalive_inactivity(now);
        Ok(())
    }

    /// Enqueue the protocol-appropriate UNLINK packet and transition
    /// the session to [`ClientStateKind::Disconnecting`].
    ///
    /// # Errors
    ///
    /// Returns [`StateError::WrongState`] outside connecting/connected state,
    /// [`StateError::DeadlineOverflow`] for an unrepresentable timer span, or
    /// [`Error::Protocol`] if a codec encoder fails.
    pub fn enqueue_disconnect(&mut self, now: Instant) -> Result<(), Error> {
        self.validate_time(now)?;
        if !matches!(
            self.state,
            ClientStateKind::Connecting | ClientStateKind::Connected
        ) {
            return Err(StateError::WrongState {
                operation: StateOperation::Disconnect,
                state: self.state,
                protocol: self.kind,
            }
            .into());
        }
        let packet = match self.kind {
            ProtocolKind::DPlus => {
                let mut buf = [0u8; 16];
                let n = dplus::encode_unlink(&mut buf)
                    .map_err(dplus::DPlusError::from)
                    .map_err(ProtocolError::DPlus)?;
                OutboundPacket {
                    dst: self.peer,
                    payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                    not_before: now,
                }
            }
            ProtocolKind::DExtra => {
                let mut buf = [0u8; 16];
                let n = dextra::encode_unlink(&mut buf, self.local_identity())
                    .map_err(dextra::DExtraError::from)
                    .map_err(ProtocolError::DExtra)?;
                OutboundPacket {
                    dst: self.peer,
                    payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                    not_before: now,
                }
            }
            ProtocolKind::Dcs => {
                let mut buf = [0u8; 32];
                let reflector_callsign = self.dcs_reflector_callsign();
                let n =
                    dcs::encode_connect_unlink(&mut buf, self.local_identity(), reflector_callsign)
                        .map_err(DcsError::from)
                        .map_err(ProtocolError::Dcs)?;
                OutboundPacket {
                    dst: self.peer,
                    payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                    not_before: now,
                }
            }
        };
        self.outbox.enqueue(packet);
        self.state = ClientStateKind::Disconnecting;
        self.arm_disconnect_deadline(now);
        Ok(())
    }

    // ── Voice TX ──────────────────────────────────────────────

    /// Enqueue a voice header packet for transmission.
    ///
    /// Caches the header internally so subsequent
    /// [`Self::enqueue_send_voice`] and [`Self::enqueue_send_eot`]
    /// calls can use it (required by DCS, which embeds the full
    /// header in every voice frame).
    ///
    /// For DCS, the protocol does NOT have a separate header packet:
    /// the first frame (seq=0) carries the embedded header. This
    /// method emits a synthetic silence frame at seq=0 to start the
    /// stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::State`] with [`StateError::WrongState`] if
    /// the session is not in [`ClientStateKind::Connected`]. The
    /// typestate wrapper prevents this at compile time, but the runtime
    /// check is the residual safety net for direct [`SessionCore`] users.
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails
    /// (buffer too small, etc.).
    pub fn enqueue_send_header(
        &mut self,
        now: Instant,
        header: &DstarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: StateOperation::SendHeader,
                state: self.state,
                protocol: self.kind,
            }));
        }
        self.cached_tx_header = Some(*header);
        let mut buf = [0u8; 256];
        let n = match self.kind {
            ProtocolKind::DPlus => dplus::encode_voice_header(&mut buf, stream_id, header)
                .map_err(dplus::DPlusError::from)
                .map_err(ProtocolError::DPlus)?,
            ProtocolKind::DExtra => dextra::encode_voice_header(&mut buf, stream_id, header)
                .map_err(dextra::DExtraError::from)
                .map_err(ProtocolError::DExtra)?,
            ProtocolKind::Dcs => {
                // DCS has no separate header packet: the first frame
                // (seq=0) carries the embedded header. Emit a silence
                // frame at seq=0 to start the stream.
                let silence = VoiceFrame::silence();
                dcs::encode_voice(
                    &mut buf,
                    header,
                    stream_id,
                    FrameSequence::ZERO,
                    &silence,
                    false,
                )
                .map_err(DcsError::from)
                .map_err(ProtocolError::Dcs)?
            }
        };
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload: buf.get(..n).unwrap_or(&[]).to_vec(),
            not_before: now,
        });
        Ok(())
    }

    /// Enqueue a voice data frame for transmission.
    ///
    /// On DCS, the cached header from [`Self::enqueue_send_header`]
    /// is required: DCS embeds the full header in every voice frame.
    /// On `DPlus` and `DExtra`, the cache is consulted but not
    /// strictly required for voice data.
    ///
    /// # Errors
    ///
    /// Returns [`Error::State`] with [`StateError::WrongState`] if
    /// the session is not in [`ClientStateKind::Connected`].
    ///
    /// Returns [`Error::Protocol`] with
    /// [`ProtocolError::Dcs`]([`crate::error::DcsError::NoTxHeader`])
    /// if called on a DCS session before [`Self::enqueue_send_header`]
    /// has cached a TX header.
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails.
    pub fn enqueue_send_voice(
        &mut self,
        now: Instant,
        stream_id: StreamId,
        seq: FrameSequence,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: StateOperation::SendVoice,
                state: self.state,
                protocol: self.kind,
            }));
        }
        let mut buf = [0u8; 256];
        let n = match self.kind {
            ProtocolKind::DPlus => dplus::encode_voice_data(&mut buf, stream_id, seq, frame)
                .map_err(dplus::DPlusError::from)
                .map_err(ProtocolError::DPlus)?,
            ProtocolKind::DExtra => dextra::encode_voice_data(&mut buf, stream_id, seq, frame)
                .map_err(dextra::DExtraError::from)
                .map_err(ProtocolError::DExtra)?,
            ProtocolKind::Dcs => {
                let header = self
                    .cached_tx_header
                    .as_ref()
                    .ok_or(Error::Protocol(ProtocolError::Dcs(DcsError::NoTxHeader)))?;
                dcs::encode_voice(&mut buf, header, stream_id, seq, frame, false)
                    .map_err(DcsError::from)
                    .map_err(ProtocolError::Dcs)?
            }
        };
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload: buf.get(..n).unwrap_or(&[]).to_vec(),
            not_before: now,
        });
        Ok(())
    }

    /// Enqueue a voice EOT packet for transmission.
    ///
    /// On DCS, the cached header from [`Self::enqueue_send_header`]
    /// is required: DCS embeds the full header in every voice frame
    /// (including the EOT). On `DPlus` and `DExtra`, the cache is not
    /// consulted.
    ///
    /// # Errors
    ///
    /// Returns [`Error::State`] with [`StateError::WrongState`] if
    /// the session is not in [`ClientStateKind::Connected`].
    ///
    /// Returns [`Error::Protocol`] with
    /// [`ProtocolError::Dcs`]([`crate::error::DcsError::NoTxHeader`])
    /// if called on a DCS session before [`Self::enqueue_send_header`]
    /// has cached a TX header.
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails.
    pub fn enqueue_send_eot(
        &mut self,
        now: Instant,
        stream_id: StreamId,
        seq: FrameSequence,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: StateOperation::SendEot,
                state: self.state,
                protocol: self.kind,
            }));
        }
        let mut buf = [0u8; 256];
        let n = match self.kind {
            ProtocolKind::DPlus => dplus::encode_voice_eot(&mut buf, stream_id, seq)
                .map_err(dplus::DPlusError::from)
                .map_err(ProtocolError::DPlus)?,
            ProtocolKind::DExtra => dextra::encode_voice_eot(&mut buf, stream_id, seq)
                .map_err(dextra::DExtraError::from)
                .map_err(ProtocolError::DExtra)?,
            ProtocolKind::Dcs => {
                let header = self
                    .cached_tx_header
                    .as_ref()
                    .ok_or(Error::Protocol(ProtocolError::Dcs(DcsError::NoTxHeader)))?;
                let silence = VoiceFrame::silence();
                dcs::encode_voice(&mut buf, header, stream_id, seq, &silence, true)
                    .map_err(DcsError::from)
                    .map_err(ProtocolError::Dcs)?
            }
        };
        self.outbox.enqueue(OutboundPacket {
            dst: self.peer,
            payload: buf.get(..n).unwrap_or(&[]).to_vec(),
            not_before: now,
        });
        Ok(())
    }

    // ── Input dispatch ────────────────────────────────────────

    /// Feed an inbound UDP datagram.
    ///
    /// Parses the bytes, updates state, pushes events and outbound
    /// packets as needed. Protocol-erased dispatch: matches on
    /// [`Self::protocol_kind`] and calls the appropriate codec.
    ///
    /// `peer` is the source address. Foreign peers and input after closure
    /// are ignored before parsing or updating timers.
    ///
    /// # Errors
    ///
    /// Returns [`StateError::DeadlineOverflow`] before processing when a
    /// protocol timer cannot be represented. Malformed wire datagrams are
    /// logged and ignored by the protocol-specific dispatchers.
    pub fn handle_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Error> {
        if peer != self.peer || self.state == ClientStateKind::Closed {
            return Ok(());
        }
        self.validate_time(now)?;
        match self.kind {
            ProtocolKind::DPlus => self.handle_dplus_input(now, peer, bytes),
            ProtocolKind::DExtra => self.handle_dextra_input(now, peer, bytes),
            ProtocolKind::Dcs => self.handle_dcs_input(now, peer, bytes),
        }
    }

    fn handle_dplus_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Error> {
        // Lenient decode: unknown-length, magic-missing, or otherwise
        // unparseable datagrams must NOT tear down an active session.
        // Real DPlus reflectors emit unrecognized traffic (status
        // heartbeats, variable-length control, legacy framing); a
        // propagated decode error would kill the tokio shell's run
        // loop. Record a diagnostic and keep going.
        let pkt = match dplus::decode_server_to_client(bytes, &mut self.diagnostics) {
            Ok(pkt) => pkt,
            Err(e) => {
                tracing::debug!(
                    target: "dstar_gateway_core::codec",
                    error = %e,
                    peer = %peer,
                    bytes_len = bytes.len(),
                    "DPlus decoder rejected datagram; dropping (session stays alive)"
                );
                return Ok(());
            }
        };
        if self.state != ClientStateKind::Connected
            && matches!(
                pkt,
                dplus::ServerPacket::PollEcho
                    | dplus::ServerPacket::VoiceHeader { .. }
                    | dplus::ServerPacket::VoiceData { .. }
                    | dplus::ServerPacket::VoiceEot { .. }
            )
        {
            return Ok(());
        }
        match pkt {
            dplus::ServerPacket::Link1Ack => {
                if self.state == ClientStateKind::Connecting {
                    // First half of the two-step DPlus handshake:
                    // reply with LINK2 immediately.
                    let mut buf = [0u8; 32];
                    let n = dplus::encode_link2(&mut buf, &self.callsign.callsign())
                        .map_err(dplus::DPlusError::from)
                        .map_err(ProtocolError::DPlus)?;
                    self.outbox.enqueue(OutboundPacket {
                        dst: self.peer,
                        payload: buf.get(..n).unwrap_or(&[]).to_vec(),
                        not_before: now,
                    });
                    self.arm_keepalive_inactivity(now);
                } else if self.state == ClientStateKind::Disconnecting {
                    // DPlus servers echo the 5-byte packet on unlink too.
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
            }
            dplus::ServerPacket::Link2Reply { result } => {
                if self.state == ClientStateKind::Connecting {
                    match result {
                        dplus::Link2Result::Accept => {
                            self.finalize_connected(peer, now);
                        }
                        dplus::Link2Result::Busy | dplus::Link2Result::Unknown { .. } => {
                            self.finalize_rejected();
                        }
                    }
                }
            }
            dplus::ServerPacket::Link2Echo { callsign } => {
                // Some DPlus servers echo the full 28-byte LINK2 instead
                // of OKRW. Treat it as an accept.
                if self.state == ClientStateKind::Connecting && callsign == self.callsign.callsign()
                {
                    self.finalize_connected(peer, now);
                }
            }
            dplus::ServerPacket::UnlinkAck => {
                if self.state == ClientStateKind::Disconnecting {
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
            }
            dplus::ServerPacket::PollEcho => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
            }
            dplus::ServerPacket::VoiceHeader { stream_id, header } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_start_if_new(now, stream_id, header);
            }
            dplus::ServerPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_frame(now, stream_id, seq, frame);
            }
            dplus::ServerPacket::VoiceEot { stream_id, seq: _ } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_end(stream_id, VoiceEndReason::Eot);
            }
        }
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "uniform signature with handle_dplus_input / handle_dcs_input; top-level dispatcher returns Result<(), Error>"
    )]
    fn handle_dextra_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Error> {
        // Lenient decode: see `handle_dplus_input` for the rationale.
        // An unknown datagram must not kill an established session.
        let pkt = match dextra::decode_server_to_client(bytes, &mut self.diagnostics) {
            Ok(pkt) => pkt,
            Err(e) => {
                tracing::debug!(
                    target: "dstar_gateway_core::codec",
                    error = %e,
                    peer = %peer,
                    bytes_len = bytes.len(),
                    "DExtra decoder rejected datagram; dropping (session stays alive)"
                );
                return Ok(());
            }
        };
        if self.state != ClientStateKind::Connected
            && matches!(
                pkt,
                dextra::ServerPacket::PollEcho { .. }
                    | dextra::ServerPacket::VoiceHeader { .. }
                    | dextra::ServerPacket::VoiceData { .. }
                    | dextra::ServerPacket::VoiceEot { .. }
            )
        {
            return Ok(());
        }
        match pkt {
            dextra::ServerPacket::ConnectAck {
                callsign,
                client_module,
                reflector_module,
            } => {
                if callsign == self.callsign.callsign()
                    && client_module == self.local_module
                    && reflector_module == self.reflector_module
                    && self.state == ClientStateKind::Connecting
                {
                    self.finalize_connected(peer, now);
                }
            }
            dextra::ServerPacket::ConnectNak {
                callsign,
                client_module,
                reflector_module,
            } => {
                if callsign != self.callsign.callsign()
                    || client_module != self.local_module
                    || (reflector_module != ModuleSlot::Empty
                        && reflector_module != ModuleSlot::Module(self.reflector_module))
                {
                    return Ok(());
                }
                if self.state == ClientStateKind::Connecting {
                    self.finalize_rejected();
                } else if self.state == ClientStateKind::Disconnecting
                    && reflector_module == ModuleSlot::Empty
                {
                    // DExtra reflectors echo the unlink as a NAK on
                    // module-space. Treat it as the unlink ACK.
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
            }
            dextra::ServerPacket::PollEcho { .. } => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
            }
            dextra::ServerPacket::VoiceHeader { stream_id, header } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_start_if_new(now, stream_id, header);
            }
            dextra::ServerPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_frame(now, stream_id, seq, frame);
            }
            dextra::ServerPacket::VoiceEot { stream_id, seq: _ } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_end(stream_id, VoiceEndReason::Eot);
            }
        }
        Ok(())
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "uniform signature with handle_dplus_input / handle_dextra_input; top-level dispatcher returns Result<(), Error>"
    )]
    fn handle_dcs_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Error> {
        // Lenient decode: see `handle_dplus_input` for the rationale.
        // An unknown datagram must not kill an established session.
        let pkt = match dcs::decode_server_to_client(bytes, &mut self.diagnostics) {
            Ok(pkt) => pkt,
            Err(e) => {
                tracing::debug!(
                    target: "dstar_gateway_core::codec",
                    error = %e,
                    peer = %peer,
                    bytes_len = bytes.len(),
                    "DCS decoder rejected datagram; dropping (session stays alive)"
                );
                return Ok(());
            }
        };
        if self.state != ClientStateKind::Connected
            && matches!(
                pkt,
                dcs::ServerPacket::PollEcho { .. } | dcs::ServerPacket::Voice { .. }
            )
        {
            return Ok(());
        }
        match pkt {
            dcs::ServerPacket::ConnectAck {
                callsign,
                client_module,
                reflector_module,
            } => {
                if callsign == self.callsign.callsign()
                    && client_module == self.local_module
                    && reflector_module == self.reflector_module
                    && self.state == ClientStateKind::Connecting
                {
                    self.finalize_connected(peer, now);
                }
            }
            dcs::ServerPacket::ConnectNak {
                callsign,
                client_module,
                reflector_module,
            } => {
                if callsign != self.callsign.callsign()
                    || client_module != self.local_module
                    || (reflector_module != ModuleSlot::Empty
                        && reflector_module != ModuleSlot::Module(self.reflector_module))
                {
                    return Ok(());
                }
                if self.state == ClientStateKind::Connecting {
                    self.finalize_rejected();
                } else if self.state == ClientStateKind::Disconnecting
                    && reflector_module == ModuleSlot::Empty
                {
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
            }
            dcs::ServerPacket::PollEcho { .. } => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
            }
            dcs::ServerPacket::Voice {
                header,
                stream_id,
                seq,
                frame,
                is_end,
            } => {
                self.arm_keepalive_inactivity(now);
                if is_end {
                    self.emit_voice_end(stream_id, VoiceEndReason::Eot);
                } else {
                    // DCS embeds the D-STAR header in every voice
                    // frame. Treat `seq == 0` (or a fresh stream id)
                    // as the stream-start trigger via
                    // [`Self::emit_voice_start_if_new`], but ALWAYS
                    // surface the frame as `VoiceFrame` too, because the
                    // `seq == 0` frame carries real voice data plus
                    // the superframe sync pattern in slow-data.
                    //
                    self.emit_voice_start_if_new(now, stream_id, header);
                    self.emit_voice_frame(now, stream_id, seq, frame);
                }
            }
        }
        Ok(())
    }

    /// Queue only the active stream's frame and renew its activity timer.
    fn emit_voice_frame(&mut self, now: Instant, stream_id: StreamId, seq: u8, frame: VoiceFrame) {
        if self.active_rx_stream == Some(stream_id) {
            self.events.push_back(RawEvent::VoiceFrame {
                stream_id,
                seq,
                frame,
            });
            self.arm_voice_inactivity(now);
            self.last_voice_activity_at = Some(now);
        } else {
            tracing::trace!(
                target: "dstar_gateway_core::session::client",
                concurrent_sid = format_args!("{:#06X}", stream_id.get()),
                active_sid = ?self.active_rx_stream.map(StreamId::get),
                seq,
                "dropping voice frame for concurrent stream"
            );
        }
    }

    // ── Timer handling ────────────────────────────────────────

    /// Advance the session timers using `now` as the current
    /// instant.
    ///
    /// Checks each named timer:
    ///
    /// - `keepalive` expired → enqueue poll packet, re-arm
    /// - `keepalive_inactivity` expired → transition to Closed,
    ///   emit [`Event::Disconnected`] with reason
    ///   [`DisconnectReason::KeepaliveInactivity`]
    /// - `disconnect_deadline` expired (in Disconnecting) →
    ///   transition to Closed, emit [`Event::Disconnected`] with
    ///   reason [`DisconnectReason::DisconnectTimeout`]
    pub fn handle_timeout(&mut self, now: Instant) {
        if self.validate_time(now).is_err() {
            if self.state != ClientStateKind::Closed {
                self.outbox = Outbox::new();
                self.active_rx_stream = None;
                self.last_voice_activity_at = None;
                self.finalize_disconnect(DisconnectReason::ClockRange);
            }
            return;
        }
        if self.state == ClientStateKind::Connected && self.timers.is_expired(TIMER_KEEPALIVE, now)
        {
            self.enqueue_poll(now);
            self.timers.set(
                TIMER_KEEPALIVE,
                timer_deadline(now, self.keepalive_interval()),
            );
        }

        // Voice-inactivity expiry: the active inbound stream has
        // produced no voice frames for `VOICE_INACTIVITY_TIMEOUT`
        // (2 s on all three protocols). Synthesize a
        // `VoiceEnd(Inactivity)` so the consumer isn't left with a
        // dangling "in progress" stream when the reflector drops
        // an EOT packet or the source stops without forwarding an EOT.
        //
        // The timer is armed on each voice frame, so the deadline
        // always sits `VOICE_INACTIVITY_TIMEOUT` past the latest real
        // activity; the event loop must keep draining `next_event` or
        // a stalled loop fires this deadline mid-transmission.
        if self.state == ClientStateKind::Connected
            && self.timers.is_expired(TIMER_VOICE_INACTIVITY, now)
        {
            if let Some(stream_id) = self.active_rx_stream {
                tracing::debug!(
                    target: "dstar_gateway_core::session::client",
                    stream_id = format_args!("{:#06X}", stream_id.get()),
                    "voice inactivity expired: synthesizing VoiceEnd(Inactivity)"
                );
                self.emit_voice_end(stream_id, VoiceEndReason::Inactivity);
            } else {
                // Defensive: timer fired but no active stream.
                // Remove the expired timer outside the disconnect state.
                self.timers.clear(TIMER_VOICE_INACTIVITY);
            }
        }

        if (self.state == ClientStateKind::Connecting || self.state == ClientStateKind::Connected)
            && self.timers.is_expired(TIMER_KEEPALIVE_INACTIVITY, now)
        {
            self.finalize_disconnect(DisconnectReason::KeepaliveInactivity);
            return;
        }

        if self.state == ClientStateKind::Disconnecting
            && self.timers.is_expired(TIMER_DISCONNECT_DEADLINE, now)
        {
            self.finalize_disconnect(DisconnectReason::DisconnectTimeout);
        }
    }

    // ── Poll / event drain ────────────────────────────────────

    /// Pop the next outbound datagram, if any.
    ///
    /// The returned [`Transmit`] borrows from `self.current_tx`,
    /// which holds the most-recently-popped packet until the next
    /// call replaces it. Callers must consume the borrow before
    /// calling this method again.
    #[must_use]
    pub fn pop_transmit(&mut self, now: Instant) -> Option<Transmit<'_>> {
        let next = self.outbox.pop_ready(now)?;
        self.current_tx = Some(next);
        let held = self.current_tx.as_ref()?;
        Some(Transmit {
            dst: held.dst,
            payload: held.payload.as_slice(),
        })
    }

    /// Pop the next consumer-visible event.
    ///
    /// The `P` type parameter re-attaches the protocol phantom at
    /// drain time; the event queue itself is protocol-erased. Returns `None`
    /// without draining when `P` does not match [`Self::protocol_kind`].
    pub fn pop_event<P: Protocol>(&mut self) -> Option<Event<P>> {
        if P::KIND != self.kind {
            return None;
        }
        let raw = self.events.pop_front()?;
        Some(match raw {
            RawEvent::Connected { peer } => Event::Connected { peer },
            RawEvent::Disconnected { reason } => Event::Disconnected { reason },
            RawEvent::PollEcho { peer } => Event::PollEcho { peer },
            RawEvent::VoiceStart { stream_id, header } => Event::VoiceStart {
                stream_id,
                header,
                // Per-event diagnostics are not populated here;
                // consumers drain them via `Session::diagnostics()`.
                diagnostics: Vec::new(),
            },
            RawEvent::VoiceFrame {
                stream_id,
                seq,
                frame,
            } => Event::VoiceFrame {
                stream_id,
                seq,
                frame,
            },
            RawEvent::VoiceEnd { stream_id, reason } => Event::VoiceEnd { stream_id, reason },
        })
    }

    /// Earliest instant at which this core wants to be woken up.
    ///
    /// Combines the outbox's next release instant with the timer
    /// wheel's next deadline.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        match (
            self.outbox.peek_next_deadline(),
            self.timers.next_deadline(),
        ) {
            (None, None) => None,
            (Some(o), None) => Some(o),
            (None, Some(t)) => Some(t),
            (Some(o), Some(t)) => Some(o.min(t)),
        }
    }

    // ── Internal helpers ──────────────────────────────────────

    /// Check all protocol-specific timer spans before mutating the session.
    fn validate_time(&self, now: Instant) -> Result<(), Error> {
        let longest = self
            .keepalive_interval()
            .max(self.keepalive_inactivity_timeout())
            .max(self.voice_inactivity_timeout())
            .max(self.disconnect_timeout());
        let _deadline = now
            .checked_add(longest)
            .ok_or(StateError::DeadlineOverflow)?;
        Ok(())
    }

    /// Enqueue the protocol-appropriate keepalive poll packet.
    ///
    /// Encoder failures are swallowed: the scratch buffers in this
    /// method are always big enough for the smallest packet in each
    /// protocol, so the error path is unreachable in practice. A
    /// failure would simply mean no poll is sent this tick and the
    /// next timer expiry will try again.
    fn enqueue_poll(&mut self, now: Instant) {
        let encoded: Option<Vec<u8>> = match self.kind {
            ProtocolKind::DPlus => {
                let mut buf = [0u8; 8];
                dplus::encode_poll(&mut buf)
                    .ok()
                    .and_then(|n| buf.get(..n).map(<[u8]>::to_vec))
            }
            ProtocolKind::DExtra => {
                let mut buf = [0u8; 16];
                dextra::encode_poll(&mut buf, &self.callsign.callsign())
                    .ok()
                    .and_then(|n| buf.get(..n).map(<[u8]>::to_vec))
            }
            ProtocolKind::Dcs => {
                let mut buf = [0u8; 32];
                let reflector_callsign = self.dcs_reflector_callsign();
                dcs::encode_poll_request(&mut buf, self.callsign, reflector_callsign)
                    .ok()
                    .and_then(|n| buf.get(..n).map(<[u8]>::to_vec))
            }
        };
        if let Some(payload) = encoded {
            self.outbox.enqueue(OutboundPacket {
                dst: self.peer,
                payload,
                not_before: now,
            });
        }
    }

    /// Transition to Connected, arm keepalive timers, emit event.
    fn finalize_connected(&mut self, peer: SocketAddr, now: Instant) {
        self.state = ClientStateKind::Connected;
        self.timers.set(
            TIMER_KEEPALIVE,
            timer_deadline(now, self.keepalive_interval()),
        );
        self.arm_keepalive_inactivity(now);
        self.events.push_back(RawEvent::Connected { peer });
    }

    /// Transition to Closed with `Rejected`, emit event.
    fn finalize_rejected(&mut self) {
        self.state = ClientStateKind::Closed;
        self.clear_timers();
        self.events.push_back(RawEvent::Disconnected {
            reason: DisconnectReason::Rejected,
        });
    }

    /// Transition to Closed with the given reason, emit event.
    fn finalize_disconnect(&mut self, reason: DisconnectReason) {
        self.state = ClientStateKind::Closed;
        self.clear_timers();
        self.events.push_back(RawEvent::Disconnected { reason });
    }

    const fn clear_timers(&mut self) {
        self.timers.clear(TIMER_KEEPALIVE);
        self.timers.clear(TIMER_KEEPALIVE_INACTIVITY);
        self.timers.clear(TIMER_DISCONNECT_DEADLINE);
        self.timers.clear(TIMER_VOICE_INACTIVITY);
    }

    fn arm_keepalive_inactivity(&mut self, now: Instant) {
        self.timers.set(
            TIMER_KEEPALIVE_INACTIVITY,
            timer_deadline(now, self.keepalive_inactivity_timeout()),
        );
    }

    /// Re-arm [`TIMER_VOICE_INACTIVITY`] for the currently-active
    /// incoming voice stream.
    ///
    /// Called on every voice frame push and on each fresh
    /// [`RawEvent::VoiceStart`] synthesized by
    /// [`Self::emit_voice_start_if_new`]. The deadline lives in
    /// [`Self::timers`], so [`Self::next_deadline`] (consumed by the
    /// tokio shell's `next_wake`) surfaces it as the earliest wake
    /// automatically. Not armed by bare header retransmits of an
    /// already-active stream: reflectors keep re-sending the header
    /// after voice data stops, so treating those as liveness would
    /// mask exactly the case this timer exists to catch.
    fn arm_voice_inactivity(&mut self, now: Instant) {
        self.timers.set(
            TIMER_VOICE_INACTIVITY,
            timer_deadline(now, self.voice_inactivity_timeout()),
        );
    }

    fn arm_disconnect_deadline(&mut self, now: Instant) {
        self.timers.set(
            TIMER_DISCONNECT_DEADLINE,
            timer_deadline(now, self.disconnect_timeout()),
        );
    }

    const fn keepalive_interval(&self) -> Duration {
        match self.kind {
            ProtocolKind::DPlus => dplus::consts::KEEPALIVE_INTERVAL,
            ProtocolKind::DExtra => dextra::consts::KEEPALIVE_INTERVAL,
            ProtocolKind::Dcs => dcs::consts::KEEPALIVE_INTERVAL,
        }
    }

    const fn keepalive_inactivity_timeout(&self) -> Duration {
        match self.kind {
            ProtocolKind::DPlus => dplus::consts::KEEPALIVE_INACTIVITY_TIMEOUT,
            ProtocolKind::DExtra => dextra::consts::KEEPALIVE_INACTIVITY_TIMEOUT,
            ProtocolKind::Dcs => dcs::consts::KEEPALIVE_INACTIVITY_TIMEOUT,
        }
    }

    const fn voice_inactivity_timeout(&self) -> Duration {
        match self.kind {
            ProtocolKind::DPlus => dplus::consts::VOICE_INACTIVITY_TIMEOUT,
            ProtocolKind::DExtra => dextra::consts::VOICE_INACTIVITY_TIMEOUT,
            ProtocolKind::Dcs => dcs::consts::VOICE_INACTIVITY_TIMEOUT,
        }
    }

    const fn disconnect_timeout(&self) -> Duration {
        match self.kind {
            ProtocolKind::DPlus => dplus::consts::DISCONNECT_TIMEOUT,
            ProtocolKind::DExtra => dextra::consts::DISCONNECT_TIMEOUT,
            ProtocolKind::Dcs => dcs::consts::DISCONNECT_TIMEOUT,
        }
    }

    /// Return the stored reflector callsign for DCS codec paths.
    ///
    /// The constructor requires this value in [`super::ClientProtocol::Dcs`].
    const fn dcs_reflector_callsign(&self) -> DcsReflector {
        let super::ClientProtocol::Dcs(target) = self.protocol else {
            unreachable!();
        };
        target
    }

    const fn local_identity(&self) -> RepeaterIdentity {
        RepeaterIdentity {
            callsign: self.callsign,
            module: self.local_module,
        }
    }
}

/// Public time-bearing entry points validate every timer span first.
/// A direct internal call at the limit schedules an immediately due timer.
fn timer_deadline(now: Instant, duration: Duration) -> Instant {
    now.checked_add(duration).unwrap_or(now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dplus::HostList;
    use crate::codec::{dcs as dcs_codec, dextra as dextra_codec, dplus as dplus_codec};
    use crate::error::DcsError;
    use crate::header::DstarHeader;
    use crate::session::client::protocol::{DExtra, DPlus, Dcs};
    use crate::types::{StreamId, Suffix};
    use crate::voice::VoiceFrame;
    use crate::{ClientProtocol, DcsReflector, RoutingCallsign, SessionConfig};
    use std::net::{IpAddr, Ipv4Addr};

    fn clock_limit() -> Instant {
        let mut limit = Instant::now();
        for bit in (0..64).rev() {
            if let Some(later) = limit.checked_add(Duration::from_secs(1_u64 << bit)) {
                limit = later;
            }
        }
        limit
    }

    #[test]
    fn clock_limit_rejects_partial_transitions_and_timeout_closes_once() -> TestResult {
        let limit = clock_limit();
        for mut core in [new_dextra(), new_dplus(), new_dcs()] {
            let result = core.enqueue_connect(limit);
            assert!(
                matches!(result, Err(Error::State(StateError::DeadlineOverflow))),
                "unrepresentable deadline must be typed: {result:?}"
            );
            assert_eq!(core.state_kind(), ClientStateKind::Configured);
            assert!(
                core.next_deadline().is_none(),
                "failed connect cannot enqueue packets or timers"
            );
            if core.kind == ProtocolKind::DPlus {
                core.attach_host_list(HostList::new())?;
            }
            core.enqueue_connect(Instant::now())?;
            core.handle_timeout(limit);
            assert_eq!(core.state_kind(), ClientStateKind::Closed);
            assert!(
                core.next_deadline().is_none(),
                "clock-range failure retires every queued deadline"
            );
            assert!(
                matches!(
                    core.pop_event::<DExtra>(),
                    Some(Event::Disconnected {
                        reason: DisconnectReason::ClockRange
                    })
                ) || matches!(
                    core.pop_event::<DPlus>(),
                    Some(Event::Disconnected {
                        reason: DisconnectReason::ClockRange
                    })
                ) || matches!(
                    core.pop_event::<Dcs>(),
                    Some(Event::Disconnected {
                        reason: DisconnectReason::ClockRange
                    })
                ),
                "one clock-range completion must remain observable"
            );
            core.handle_timeout(limit);
            assert!(
                core.events.is_empty(),
                "repeated timeout cannot duplicate completion"
            );
        }
        Ok(())
    }
    const fn cs() -> Callsign {
        Callsign::from_wire_bytes(*b"W1AW    ")
    }

    const fn sequence(index: u8) -> FrameSequence {
        match FrameSequence::new(index) {
            Some(index) => index,
            None => unreachable!(),
        }
    }

    const ADDR_DEXTRA: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const ADDR_DPLUS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const ADDR_DCS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30051);
    const LOCAL: RoutingCallsign = match RoutingCallsign::from_padded_bytes(*b"W1AW   ") {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };
    const DCS_PREFIX: RoutingCallsign = match RoutingCallsign::from_padded_bytes(*b"DCS001 ") {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };
    const DCS_TARGET: DcsReflector = match DcsReflector::try_from_routing(DCS_PREFIX) {
        Ok(value) => value,
        Err(_) => unreachable!(),
    };
    const LOCAL_IDENTITY: RepeaterIdentity = RepeaterIdentity {
        callsign: LOCAL,
        module: Module::B,
    };

    fn new_dextra() -> SessionCore {
        SessionCore::new(SessionConfig {
            protocol: ClientProtocol::DExtra,
            callsign: LOCAL,
            local_module: Module::B,
            reflector_module: Module::C,
            peer: ADDR_DEXTRA,
        })
    }

    fn new_dplus() -> SessionCore {
        SessionCore::new(SessionConfig {
            protocol: ClientProtocol::DPlus,
            callsign: LOCAL,
            local_module: Module::B,
            reflector_module: Module::C,
            peer: ADDR_DPLUS,
        })
    }

    fn new_dcs() -> SessionCore {
        SessionCore::new(SessionConfig {
            protocol: ClientProtocol::Dcs(DCS_TARGET),
            callsign: LOCAL,
            local_module: Module::B,
            reflector_module: Module::C,
            peer: ADDR_DCS,
        })
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Drive a `DExtra` core through the full connect handshake,
    /// returning it in Connected state with the Connected event
    /// already drained.
    fn connected_dextra() -> Result<(SessionCore, Instant), Box<dyn std::error::Error>> {
        let t0 = Instant::now();
        let mut core = new_dextra();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link")?;
        let mut ack = [0u8; 16];
        let n = dextra_codec::encode_connect_ack(&mut ack, LOCAL_IDENTITY, Module::C)?;
        core.handle_input(t0, ADDR_DEXTRA, ack.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DExtra>().ok_or("no connected event")?);
        Ok((core, t0))
    }

    /// Drive a `DPlus` core through the full 2-step handshake.
    fn connected_dplus() -> Result<(SessionCore, Instant), Box<dyn std::error::Error>> {
        let t0 = Instant::now();
        let mut core = new_dplus();
        core.attach_host_list(HostList::new())?;
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link1")?;
        let mut ack = [0u8; 8];
        let n = dplus_codec::encode_link1_ack(&mut ack)?;
        core.handle_input(t0, ADDR_DPLUS, ack.get(..n).ok_or("n > buf")?)?;
        let _ = core.pop_transmit(t0).ok_or("no link2")?;
        let mut reply = [0u8; 16];
        let n = dplus_codec::encode_link2_reply(&mut reply, dplus_codec::Link2Result::Accept)?;
        core.handle_input(t0, ADDR_DPLUS, reply.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DPlus>().ok_or("no connected event")?);
        Ok((core, t0))
    }

    /// Drive a DCS core through the connect handshake.
    fn connected_dcs() -> Result<(SessionCore, Instant), Box<dyn std::error::Error>> {
        let t0 = Instant::now();
        let mut core = new_dcs();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link")?;
        let mut ack = [0u8; 16];
        let n = dcs_codec::encode_connect_ack(&mut ack, LOCAL_IDENTITY, Module::C)?;
        core.handle_input(t0, ADDR_DCS, ack.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<Dcs>().ok_or("no connected event")?);
        Ok((core, t0))
    }

    #[test]
    fn receive_activity_requires_a_connected_client() -> TestResult {
        let now = Instant::now();
        for mut core in [new_dplus(), new_dextra(), new_dcs()] {
            let mut voice = [0_u8; 100];
            let mut poll = [0_u8; 32];
            let (voice_len, poll_len) = match core.kind {
                ProtocolKind::DPlus => (
                    dplus_codec::encode_voice_header(&mut voice, sid(0x1234), &test_header())?,
                    dplus_codec::encode_poll_echo(&mut poll)?,
                ),
                ProtocolKind::DExtra => (
                    dextra_codec::encode_voice_header(&mut voice, sid(0x1234), &test_header())?,
                    dextra_codec::encode_poll_echo(&mut poll, &cs())?,
                ),
                ProtocolKind::Dcs => (
                    dcs_codec::encode_voice(
                        &mut voice,
                        &test_header(),
                        sid(0x1234),
                        FrameSequence::ZERO,
                        &VoiceFrame::silence(),
                        false,
                    )?,
                    dcs_codec::encode_poll_reply(&mut poll, &cs(), &DCS_TARGET.callsign())?,
                ),
            };
            let voice = voice.get(..voice_len).ok_or("invalid voice length")?;
            let poll = poll.get(..poll_len).ok_or("invalid poll length")?;
            for state in [
                ClientStateKind::Configured,
                ClientStateKind::Authenticated,
                ClientStateKind::Connecting,
                ClientStateKind::Disconnecting,
            ] {
                core.state = state;
                core.handle_input(now, core.peer, voice)?;
                core.handle_input(now, core.peer, poll)?;
                assert!(core.events.is_empty(), "{state:?}: {:?}", core.events);
                assert_eq!(core.active_rx_stream, None, "{state:?}");
                assert_eq!(core.next_deadline(), None, "{state:?}");
            }
            core.state = ClientStateKind::Connected;
            core.handle_input(now, core.peer, voice)?;
            assert!(
                matches!(core.events.front(), Some(RawEvent::VoiceStart { .. })),
                "the same packet must be usable after linking"
            );
        }
        Ok(())
    }

    #[test]
    fn dplus_link_echo_requires_the_requested_identity() -> TestResult {
        let mut core = new_dplus();
        core.attach_host_list(HostList::new())?;
        let now = Instant::now();
        core.enqueue_connect(now)?;
        let mut packet = [0_u8; 28];
        let foreign = Callsign::from_wire_bytes(*b"OTHER   ");
        let n = dplus_codec::encode_link2(&mut packet, &foreign)?;
        core.handle_input(
            now,
            ADDR_DPLUS,
            packet.get(..n).ok_or("invalid echo length")?,
        )?;
        assert_eq!(core.state_kind(), ClientStateKind::Connecting);
        let n = dplus_codec::encode_link2(&mut packet, &cs())?;
        core.handle_input(
            now,
            ADDR_DPLUS,
            packet.get(..n).ok_or("invalid echo length")?,
        )?;
        assert_eq!(core.state_kind(), ClientStateKind::Connected);
        Ok(())
    }

    // ── DExtra happy path ─────────────────────────────────────

    #[test]
    fn dextra_connect_success() -> TestResult {
        let mut core = new_dextra();
        assert_eq!(core.state_kind(), ClientStateKind::Configured);
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        assert_eq!(core.state_kind(), ClientStateKind::Connecting);

        // Expect an 11-byte LINK packet in the outbox.
        let tx = core.pop_transmit(t0).ok_or("no link packet in outbox")?;
        assert_eq!(tx.payload.len(), 11);
        assert_eq!(tx.dst, ADDR_DEXTRA);

        // Build the server-side ACK using the codec.
        let mut ack = [0u8; 16];
        let n = dextra_codec::encode_connect_ack(&mut ack, LOCAL_IDENTITY, Module::C)?;
        core.handle_input(
            t0,
            ADDR_DEXTRA,
            ack.get(..n).ok_or("encode returned n > buf")?,
        )?;
        assert_eq!(core.state_kind(), ClientStateKind::Connected);

        // Should have emitted a Connected event.
        let ev = core.pop_event::<DExtra>().ok_or("no Connected event")?;
        assert!(matches!(ev, Event::Connected { .. }));
        Ok(())
    }

    #[test]
    fn control_replies_require_correlated_identity_module_and_phase() -> TestResult {
        let now = Instant::now();
        for mut core in [new_dextra(), new_dcs()] {
            core.enqueue_connect(now)?;
            let _link = core.pop_transmit(now).ok_or("LINK request")?;
            for invalid in [b"N0CALL  BCACK\0", b"W1AW    ACACK\0", b"W1AW    BDACK\0"] {
                core.handle_input(now, core.peer(), invalid)?;
                assert_eq!(
                    core.state_kind(),
                    ClientStateKind::Connecting,
                    "an unrelated identity or module cannot finish the link"
                );
            }
            core.handle_input(now, core.peer(), b"W1AW    BCACK\0")?;
            assert_eq!(core.state_kind(), ClientStateKind::Connected);
            core.enqueue_disconnect(now)?;
            let _unlink = core.pop_transmit(now).ok_or("UNLINK request")?;
            core.handle_input(now, core.peer(), b"W1AW    BCACK\0")?;
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Disconnecting,
                "a delayed LINK acknowledgment cannot complete UNLINK"
            );
            core.handle_input(now, core.peer(), b"W1AW    BCNAK\0")?;
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Disconnecting,
                "a link rejection with a selected module cannot finish UNLINK"
            );
            core.handle_input(now, core.peer(), b"W1AW    B NAK\0")?;
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Closed,
                "the matched space-slot NAK acknowledges disconnect"
            );
        }
        Ok(())
    }

    #[test]
    fn dextra_connect_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut core = new_dextra();
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link packet")?;

        let mut nak = [0u8; 16];
        let n = dextra_codec::encode_connect_nak(
            &mut nak,
            LOCAL_IDENTITY,
            ModuleSlot::Module(Module::C),
        )?;
        core.handle_input(t0, ADDR_DEXTRA, nak.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Closed);

        let ev = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(
                ev,
                Event::Disconnected {
                    reason: DisconnectReason::Rejected
                }
            ),
            "expected Disconnected(Rejected), got {ev:?}"
        );
        Ok(())
    }

    #[test]
    fn dextra_keepalive_fires_poll() -> TestResult {
        let (mut core, t0) = connected_dextra()?;

        let t1 = t0 + dextra_codec::consts::KEEPALIVE_INTERVAL + Duration::from_millis(1);
        core.handle_timeout(t1);

        let tx = core.pop_transmit(t1).ok_or("no poll packet")?;
        assert_eq!(tx.payload.len(), 9);
        Ok(())
    }

    #[test]
    fn dextra_keepalive_inactivity_closes() -> TestResult {
        let (mut core, t0) = connected_dextra()?;

        let t1 = t0 + dextra_codec::consts::KEEPALIVE_INACTIVITY_TIMEOUT + Duration::from_secs(1);
        core.handle_timeout(t1);
        assert_eq!(core.state_kind(), ClientStateKind::Closed);

        let ev = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(
                ev,
                Event::Disconnected {
                    reason: DisconnectReason::KeepaliveInactivity
                }
            ),
            "expected Disconnected(KeepaliveInactivity), got {ev:?}"
        );
        Ok(())
    }

    #[test]
    fn dextra_disconnect_success() -> TestResult {
        let (mut core, t0) = connected_dextra()?;

        core.enqueue_disconnect(t0)?;
        assert_eq!(core.state_kind(), ClientStateKind::Disconnecting);

        let tx = core.pop_transmit(t0).ok_or("no unlink packet")?;
        assert_eq!(tx.payload.len(), 11);

        // DExtra echoes the unlink as a NAK on space module.
        let mut nak = [0u8; 16];
        let n = dextra_codec::encode_connect_nak(&mut nak, LOCAL_IDENTITY, ModuleSlot::Empty)?;
        core.handle_input(t0, ADDR_DEXTRA, nak.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Closed);

        let ev = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(
                ev,
                Event::Disconnected {
                    reason: DisconnectReason::UnlinkAcked
                }
            ),
            "expected Disconnected(UnlinkAcked), got {ev:?}"
        );
        Ok(())
    }

    // ── DPlus happy path (two-step handshake) ─────────────────

    #[test]
    fn dplus_two_step_connect_success() -> TestResult {
        let mut core = new_dplus();
        assert_eq!(core.state_kind(), ClientStateKind::Configured);

        core.attach_host_list(HostList::new())?;
        assert_eq!(core.state_kind(), ClientStateKind::Authenticated);
        assert!(core.host_list().is_some());

        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        assert_eq!(core.state_kind(), ClientStateKind::Connecting);

        let tx = core.pop_transmit(t0).ok_or("no link1")?;
        assert_eq!(tx.payload.len(), 5);

        let mut ack = [0u8; 8];
        let n = dplus_codec::encode_link1_ack(&mut ack)?;
        core.handle_input(t0, ADDR_DPLUS, ack.get(..n).ok_or("n > buf")?)?;
        assert_eq!(
            core.state_kind(),
            ClientStateKind::Connecting,
            "still connecting after LINK1 ACK"
        );

        let tx = core.pop_transmit(t0).ok_or("no link2")?;
        assert_eq!(tx.payload.len(), 28);

        let mut reply = [0u8; 16];
        let n = dplus_codec::encode_link2_reply(&mut reply, dplus_codec::Link2Result::Accept)?;
        core.handle_input(t0, ADDR_DPLUS, reply.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Connected);

        let ev = core.pop_event::<DPlus>().ok_or("no event")?;
        assert!(matches!(ev, Event::Connected { .. }));
        Ok(())
    }

    #[test]
    fn dplus_rejected_on_busy() -> TestResult {
        let mut core = new_dplus();
        core.attach_host_list(HostList::new())?;
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link1")?;

        let mut ack = [0u8; 8];
        let n = dplus_codec::encode_link1_ack(&mut ack)?;
        core.handle_input(t0, ADDR_DPLUS, ack.get(..n).ok_or("n > buf")?)?;
        let _ = core.pop_transmit(t0).ok_or("no link2")?;

        let mut reply = [0u8; 16];
        let n = dplus_codec::encode_link2_reply(&mut reply, dplus_codec::Link2Result::Busy)?;
        core.handle_input(t0, ADDR_DPLUS, reply.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Closed);

        let ev = core.pop_event::<DPlus>().ok_or("no event")?;
        assert!(
            matches!(
                ev,
                Event::Disconnected {
                    reason: DisconnectReason::Rejected
                }
            ),
            "expected Disconnected(Rejected), got {ev:?}"
        );
        Ok(())
    }

    // ── DCS ───────────────────────────────────────────────────

    #[test]
    fn dcs_connect_success() -> TestResult {
        let mut core = new_dcs();
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;

        let tx = core.pop_transmit(t0).ok_or("no link packet")?;
        assert_eq!(tx.payload.len(), 519);

        let mut ack = [0u8; 16];
        let n = dcs_codec::encode_connect_ack(&mut ack, LOCAL_IDENTITY, Module::C)?;
        core.handle_input(t0, ADDR_DCS, ack.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Connected);

        let ev = core.pop_event::<Dcs>().ok_or("no event")?;
        assert!(matches!(ev, Event::Connected { .. }));
        Ok(())
    }

    #[test]
    fn dcs_rejected_on_nak() -> TestResult {
        let mut core = new_dcs();
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link")?;

        let mut nak = [0u8; 16];
        let n =
            dcs_codec::encode_connect_nak(&mut nak, LOCAL_IDENTITY, ModuleSlot::Module(Module::C))?;
        core.handle_input(t0, ADDR_DCS, nak.get(..n).ok_or("n > buf")?)?;
        assert_eq!(core.state_kind(), ClientStateKind::Closed);

        let ev = core.pop_event::<Dcs>().ok_or("no event")?;
        assert!(
            matches!(
                ev,
                Event::Disconnected {
                    reason: DisconnectReason::Rejected
                }
            ),
            "expected Disconnected(Rejected), got {ev:?}"
        );
        Ok(())
    }

    // ── pop_transmit / next_deadline ──────────────────────────

    #[test]
    fn pop_transmit_holds_current_tx_across_calls() -> TestResult {
        let mut core = new_dextra();
        let t0 = Instant::now();

        core.outbox.enqueue(OutboundPacket {
            dst: ADDR_DEXTRA,
            payload: vec![1, 2, 3],
            not_before: t0,
        });
        core.outbox.enqueue(OutboundPacket {
            dst: ADDR_DEXTRA,
            payload: vec![4, 5, 6],
            not_before: t0 + Duration::from_millis(1),
        });

        {
            let tx = core
                .pop_transmit(t0 + Duration::from_secs(1))
                .ok_or("no tx1")?;
            assert_eq!(tx.payload, &[1, 2, 3]);
        }
        {
            let tx = core
                .pop_transmit(t0 + Duration::from_secs(1))
                .ok_or("no tx2")?;
            assert_eq!(tx.payload, &[4, 5, 6]);
        }
        assert!(core.pop_transmit(t0 + Duration::from_secs(1)).is_none());
        Ok(())
    }

    #[test]
    fn next_deadline_combines_sources() -> TestResult {
        let mut core = new_dextra();
        let t0 = Instant::now();
        core.timers
            .set(TIMER_KEEPALIVE, t0 + Duration::from_secs(5));
        core.outbox.enqueue(OutboundPacket {
            dst: ADDR_DEXTRA,
            payload: vec![],
            not_before: t0 + Duration::from_secs(2),
        });

        let dl = core.next_deadline().ok_or("no deadline")?;
        assert_eq!(dl, t0 + Duration::from_secs(2));
        Ok(())
    }

    #[test]
    fn next_deadline_none_when_idle() {
        let core = new_dextra();
        assert!(core.next_deadline().is_none());
    }

    // ── drain_diagnostics ────────────────────────────────────

    #[test]
    fn drain_diagnostics_is_empty_on_fresh_core() {
        let mut core = new_dextra();
        assert_eq!(core.drain_diagnostics(), []);
    }

    // ── Error paths ───────────────────────────────────────────

    #[test]
    fn attach_host_list_rejects_non_dplus() {
        let mut core = new_dextra();
        let result = core.attach_host_list(HostList::new());
        assert!(
            matches!(result, Err(Error::State(StateError::WrongState { .. }))),
            "DExtra must reject host list, got {result:?}"
        );
    }

    #[test]
    fn attach_host_list_rejects_wrong_state() -> TestResult {
        let mut core = new_dplus();
        core.attach_host_list(HostList::new())?;
        let result = core.attach_host_list(HostList::new());
        assert!(
            matches!(result, Err(Error::State(StateError::WrongState { .. }))),
            "second attach must fail, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn event_queue_empty_returns_none() {
        let mut core = new_dextra();
        assert!(core.pop_event::<DExtra>().is_none());
    }

    #[test]
    fn wrong_protocol_marker_preserves_queued_client_event() -> TestResult {
        let (mut core, now) = connected_dextra()?;
        let mut bytes = [0_u8; 9];
        let length = dextra_codec::encode_poll_echo(&mut bytes, &cs())?;
        core.handle_input(now, ADDR_DEXTRA, bytes.get(..length).ok_or("poll length")?)?;
        assert!(
            core.pop_event::<DPlus>().is_none(),
            "a DPlus marker cannot claim a DExtra event"
        );
        assert!(
            matches!(core.pop_event::<DExtra>(), Some(Event::PollEcho { .. })),
            "refused marker must leave the original event queued"
        );
        Ok(())
    }

    // ── Voice TX / RX ────────────────────────────────────────

    #[expect(
        clippy::unwrap_used,
        reason = "test helper: every call site passes a non-zero literal, so the \
                  unwrap cannot fire; a zero would panic only this test, at runtime"
    )]
    const fn sid(n: u16) -> StreamId {
        // Runtime call sites evaluate this at runtime, so a zero
        // argument would panic the test, and every call site passes
        // a non-zero literal.
        StreamId::new(n).unwrap()
    }

    const fn test_header() -> DstarHeader {
        DstarHeader {
            flags: crate::HeaderFlags::NONE,
            rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
    }

    // ── Junk datagrams must not kill a live session ───────────

    /// Unrecognized datagrams leave a connected session usable: after the junk,
    /// a real voice header still produces a `VoiceStart`.
    ///
    /// Note the codecs dispatch on datagram LENGTH, so a payload that
    /// is junk to one protocol can be a valid packet in another (a
    /// 3-byte datagram is a `DPlus` poll). Events the probes
    /// legitimately produce are drained before the voice assertion.
    /// What is under test is that the session survives and keeps
    /// working, not that every probe is rejected.
    #[test]
    fn dextra_junk_datagram_does_not_kill_a_connected_session() -> TestResult {
        let (mut core, t0) = connected_dextra()?;

        // The REF030 shape (10 bytes) plus a couple of other
        // unknown-length datagrams.
        for junk in [&[0u8; 10][..], &[0xFF; 3][..], &[0x42; 37][..]] {
            let result = core.handle_input(t0, ADDR_DEXTRA, junk);
            assert!(
                result.is_ok(),
                "junk datagram must be swallowed, not propagated: {result:?}"
            );
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Connected,
                "junk datagram must not tear down the session"
            );
        }
        while core.pop_event::<DExtra>().is_some() {}

        // The session still processes real traffic.
        let mut hdr = [0u8; 64];
        let n = dextra_codec::encode_voice_header(&mut hdr, sid(0x1234), &test_header())?;
        core.handle_input(t0, ADDR_DEXTRA, hdr.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DExtra>().ok_or("no event after junk")?;
        assert!(
            matches!(event, Event::VoiceStart { .. }),
            "voice must still flow after junk datagrams: {event:?}"
        );
        Ok(())
    }

    /// `DPlus` sibling of
    /// [`dextra_junk_datagram_does_not_kill_a_connected_session`].
    #[test]
    fn dplus_junk_datagram_does_not_kill_a_connected_session() -> TestResult {
        let (mut core, t0) = connected_dplus()?;
        for junk in [&[0u8; 10][..], &[0xFF; 3][..], &[0x42; 37][..]] {
            let result = core.handle_input(t0, ADDR_DPLUS, junk);
            assert!(
                result.is_ok(),
                "junk datagram must be swallowed, not propagated: {result:?}"
            );
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Connected,
                "junk datagram must not tear down the session"
            );
        }
        while core.pop_event::<DPlus>().is_some() {}
        let mut hdr = [0u8; 64];
        let n = dplus_codec::encode_voice_header(&mut hdr, sid(0x1234), &test_header())?;
        core.handle_input(t0, ADDR_DPLUS, hdr.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DPlus>().ok_or("no event after junk")?;
        assert!(
            matches!(event, Event::VoiceStart { .. }),
            "voice must still flow after junk datagrams: {event:?}"
        );
        Ok(())
    }

    /// `DCS` sibling of
    /// [`dextra_junk_datagram_does_not_kill_a_connected_session`].
    #[test]
    fn dcs_junk_datagram_does_not_kill_a_connected_session() -> TestResult {
        let (mut core, t0) = connected_dcs()?;
        for junk in [&[0u8; 10][..], &[0xFF; 3][..], &[0x42; 37][..]] {
            let result = core.handle_input(t0, ADDR_DCS, junk);
            assert!(
                result.is_ok(),
                "junk datagram must be swallowed, not propagated: {result:?}"
            );
            assert_eq!(
                core.state_kind(),
                ClientStateKind::Connected,
                "junk datagram must not tear down the session"
            );
        }
        while core.pop_event::<Dcs>().is_some() {}
        let mut voice = [0u8; 128];
        let n = dcs_codec::encode_voice(
            &mut voice,
            &test_header(),
            sid(0x1234),
            FrameSequence::ZERO,
            &VoiceFrame::silence(),
            false,
        )?;
        core.handle_input(t0, ADDR_DCS, voice.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<Dcs>().ok_or("no event after junk")?;
        assert!(
            matches!(event, Event::VoiceStart { .. }),
            "voice must still flow after junk datagrams: {event:?}"
        );
        Ok(())
    }

    // ── Voice TX: header sizes ────────────────────────────────

    #[test]
    fn dextra_connected_enqueue_send_header_produces_56_bytes() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        core.enqueue_send_header(now, &test_header(), sid(0x1234))?;
        let tx = core.pop_transmit(now).ok_or("no header tx")?;
        assert_eq!(tx.payload.len(), 56, "DExtra voice header is 56 bytes");
        Ok(())
    }

    #[test]
    fn dplus_connected_enqueue_send_header_produces_58_bytes() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        core.enqueue_send_header(now, &test_header(), sid(0x1234))?;
        let tx = core.pop_transmit(now).ok_or("no header tx")?;
        assert_eq!(tx.payload.len(), 58, "DPlus voice header is 58 bytes");
        Ok(())
    }

    #[test]
    fn dcs_connected_enqueue_send_header_produces_100_bytes() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        core.enqueue_send_header(now, &test_header(), sid(0x1234))?;
        let tx = core.pop_transmit(now).ok_or("no header tx")?;
        assert_eq!(tx.payload.len(), 100, "DCS voice frame is always 100 bytes");
        Ok(())
    }

    // ── Voice TX: data frame sizes ────────────────────────────

    #[test]
    fn dplus_connected_enqueue_send_voice_produces_29_bytes() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        core.enqueue_send_voice(now, sid(0x1234), sequence(5), &VoiceFrame::silence())?;
        let tx = core.pop_transmit(now).ok_or("no voice tx")?;
        assert_eq!(tx.payload.len(), 29, "DPlus voice data is 29 bytes");
        Ok(())
    }

    #[test]
    fn dextra_connected_enqueue_send_voice_produces_27_bytes() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        core.enqueue_send_voice(now, sid(0x1234), sequence(5), &VoiceFrame::silence())?;
        let tx = core.pop_transmit(now).ok_or("no voice tx")?;
        assert_eq!(tx.payload.len(), 27, "DExtra voice data is 27 bytes");
        Ok(())
    }

    // ── Voice TX: EOT sizes ───────────────────────────────────

    #[test]
    fn dplus_connected_enqueue_send_eot_produces_32_bytes() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        core.enqueue_send_eot(now, sid(0x1234), FrameSequence::ZERO)?;
        let tx = core.pop_transmit(now).ok_or("no eot tx")?;
        assert_eq!(tx.payload.len(), 32, "DPlus voice EOT is 32 bytes");
        Ok(())
    }

    #[test]
    fn dextra_connected_enqueue_send_eot_produces_27_bytes() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        core.enqueue_send_eot(now, sid(0x1234), FrameSequence::ZERO)?;
        let tx = core.pop_transmit(now).ok_or("no eot tx")?;
        assert_eq!(tx.payload.len(), 27, "DExtra voice EOT is 27 bytes");
        Ok(())
    }

    // ── DCS NoTxHeader error path ─────────────────────────────

    #[test]
    fn dcs_send_voice_without_header_errors() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let result = core.enqueue_send_voice(now, sid(0x1234), sequence(1), &VoiceFrame::silence());
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::Dcs(DcsError::NoTxHeader)))
            ),
            "expected NoTxHeader, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn dcs_send_eot_without_header_errors() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let result = core.enqueue_send_eot(now, sid(0x1234), sequence(1));
        assert!(
            matches!(
                result,
                Err(Error::Protocol(ProtocolError::Dcs(DcsError::NoTxHeader)))
            ),
            "expected NoTxHeader, got {result:?}"
        );
        Ok(())
    }

    #[test]
    fn dcs_send_voice_after_header_succeeds() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        core.enqueue_send_header(now, &test_header(), sid(0x1234))?;
        let _ = core.pop_transmit(now).ok_or("no header tx")?;
        core.enqueue_send_voice(now, sid(0x1234), sequence(1), &VoiceFrame::silence())?;
        let voice_tx = core.pop_transmit(now).ok_or("no voice tx")?;
        assert_eq!(voice_tx.payload.len(), 100);
        Ok(())
    }

    #[test]
    fn dcs_send_eot_after_header_succeeds() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        core.enqueue_send_header(now, &test_header(), sid(0x1234))?;
        let _ = core.pop_transmit(now).ok_or("no header tx")?;
        core.enqueue_send_eot(now, sid(0x1234), FrameSequence::ZERO)?;
        let eot_tx = core.pop_transmit(now).ok_or("no eot tx")?;
        assert_eq!(eot_tx.payload.len(), 100);
        Ok(())
    }

    // ── Wrong-state error path ───────────────────────────────

    #[test]
    fn enqueue_send_header_in_configured_state_errors() {
        let mut core = new_dextra();
        let now = Instant::now();
        let result = core.enqueue_send_header(now, &test_header(), sid(0x1234));
        assert!(matches!(
            result,
            Err(Error::State(StateError::WrongState { .. }))
        ));
    }

    #[test]
    fn enqueue_send_voice_in_configured_state_errors() {
        let mut core = new_dextra();
        let now = Instant::now();
        let result = core.enqueue_send_voice(now, sid(0x1234), sequence(1), &VoiceFrame::silence());
        assert!(matches!(
            result,
            Err(Error::State(StateError::WrongState { .. }))
        ));
    }

    #[test]
    fn enqueue_send_eot_in_configured_state_errors() {
        let mut core = new_dextra();
        let now = Instant::now();
        let result = core.enqueue_send_eot(now, sid(0x1234), sequence(1));
        assert!(matches!(
            result,
            Err(Error::State(StateError::WrongState { .. }))
        ));
    }

    // ── Voice RX: handle_input emits voice events ────────────

    #[test]
    fn dextra_handle_voice_header_emits_voice_start() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(matches!(event, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x1234));
        Ok(())
    }

    #[test]
    fn dextra_retransmitted_voice_header_does_not_re_emit_voice_start() -> TestResult {
        // Retransmitted headers retain the active stream and its decoder state.
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let first = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(matches!(first, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x1234));
        assert!(
            core.pop_event::<DExtra>().is_none(),
            "retransmitted headers must not emit additional events"
        );
        Ok(())
    }

    #[test]
    fn dextra_mid_stream_sid_change_synthesizes_voice_end() -> TestResult {
        // If a new talker takes the module without an explicit EOT
        // from the previous one, the client surfaces a synthetic
        // VoiceEnd(Inactivity) for the old stream before emitting
        // VoiceStart for the new one. This is only done once the
        // old stream has been silent for `STREAM_TAKEOVER_THRESHOLD`
        // (see `emit_voice_start_if_new`). The previous test sent
        // both headers at the same `now`, which under the new
        // guard (suppress if active stream still live) would be
        // treated as a concurrent stream and suppressed, so we
        // advance `now` past the threshold between the two
        // headers to exercise the takeover branch.
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let later = now + STREAM_TAKEOVER_THRESHOLD + Duration::from_millis(1);
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x5678), &test_header())?;
        core.handle_input(later, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let first = core.pop_event::<DExtra>().ok_or("no event 1")?;
        assert!(matches!(first, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x1234));
        let second = core.pop_event::<DExtra>().ok_or("no event 2")?;
        assert!(
            matches!(second, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0x1234 && reason == VoiceEndReason::Inactivity),
            "expected synthesized VoiceEnd for old stream, got {second:?}"
        );
        let third = core.pop_event::<DExtra>().ok_or("no event 3")?;
        assert!(
            matches!(third, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x5678),
            "expected VoiceStart for new stream, got {third:?}"
        );
        Ok(())
    }

    #[test]
    fn dextra_concurrent_stream_header_suppressed_while_active_live() -> TestResult {
        // Two simultaneous talkers being forwarded by a reflector:
        // stream A's header, then a voice frame for A (re-arms
        // `last_voice_activity_at`), then stream B's header arrives
        // only a few ms later. The new guard must suppress B's
        // header so the radio modem isn't re-keyed mid-audio.
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0xAAAA), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let n = dextra_codec::encode_voice_data(
            &mut buf,
            sid(0xAAAA),
            sequence(1),
            &VoiceFrame::silence(),
        )?;
        core.handle_input(
            now + Duration::from_millis(20),
            ADDR_DEXTRA,
            buf.get(..n).ok_or("n > buf")?,
        )?;
        // Drain stream A's start and frame events.
        drop(core.pop_event::<DExtra>().ok_or("no VoiceStart")?);
        drop(core.pop_event::<DExtra>().ok_or("no VoiceFrame")?);
        // B's header arrives only 50 ms after A's last activity,
        // well inside STREAM_TAKEOVER_THRESHOLD (500 ms). Must be
        // suppressed silently.
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0xBBBB), &test_header())?;
        core.handle_input(
            now + Duration::from_millis(70),
            ADDR_DEXTRA,
            buf.get(..n).ok_or("n > buf")?,
        )?;
        assert!(
            core.pop_event::<DExtra>().is_none(),
            "concurrent stream's header must not emit any event"
        );
        // And a voice frame for B is dropped silently too.
        let n = dextra_codec::encode_voice_data(
            &mut buf,
            sid(0xBBBB),
            sequence(2),
            &VoiceFrame::silence(),
        )?;
        core.handle_input(
            now + Duration::from_millis(80),
            ADDR_DEXTRA,
            buf.get(..n).ok_or("n > buf")?,
        )?;
        assert!(
            core.pop_event::<DExtra>().is_none(),
            "concurrent stream's voice frame must be dropped"
        );
        // A's EOT ends the active stream normally; B's EOT (if it
        // arrived) would be ignored as non-active.
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0xAAAA), FrameSequence::ZERO)?;
        core.handle_input(
            now + Duration::from_millis(100),
            ADDR_DEXTRA,
            buf.get(..n).ok_or("n > buf")?,
        )?;
        let eot = core.pop_event::<DExtra>().ok_or("no VoiceEnd")?;
        assert!(
            matches!(eot, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0xAAAA && reason == VoiceEndReason::Eot),
            "expected VoiceEnd for stream A, got {eot:?}"
        );
        Ok(())
    }

    #[test]
    fn dextra_handle_voice_data_emits_voice_frame() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        // Voice frames are gated on `active_rx_stream` matching
        // (see `handle_dextra_input`'s VoiceData branch), so
        // establish the stream with a VoiceHeader first.
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DExtra>().ok_or("no VoiceStart")?);
        let n = dextra_codec::encode_voice_data(
            &mut buf,
            sid(0x1234),
            sequence(7),
            &VoiceFrame::silence(),
        )?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(event, Event::VoiceFrame { stream_id, seq, .. } if stream_id.get() == 0x1234 && seq == 7)
        );
        Ok(())
    }

    #[test]
    fn dextra_handle_voice_eot_emits_voice_end() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        // Establish the stream first; `emit_voice_end` ignores EOTs
        // for non-active streams (concurrent stream closing).
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DExtra>().ok_or("no VoiceStart")?);
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0x1234), FrameSequence::ZERO)?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(event, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0x1234 && reason == VoiceEndReason::Eot)
        );
        Ok(())
    }

    /// Regression scenario: replay the exact wire sequence a running
    /// reflector sends (one header every superframe interleaved
    /// with 21 voice-data frames) and verify that exactly ONE
    /// `VoiceStart` is emitted for the whole stream, followed by all
    /// the `VoiceFrame`s in order, then `VoiceEnd` on the EOT packet.
    #[test]
    fn dextra_superframe_header_retransmit_emits_single_voice_start() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];

        // Simulate 3 superframes: each is 1 header + 21 voice-data
        // frames. The reflector retransmits the header at the start
        // of each superframe so late-joiners can decode.
        for _sf in 0..3 {
            let n = dextra_codec::encode_voice_header(&mut buf, sid(0x19C9), &test_header())?;
            core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
            for seq in 0..21_u8 {
                let n = dextra_codec::encode_voice_data(
                    &mut buf,
                    sid(0x19C9),
                    sequence(seq),
                    &VoiceFrame::silence(),
                )?;
                core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
            }
        }
        // Then EOT.
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0x19C9), FrameSequence::ZERO)?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;

        // Drain events and count per variant.
        let mut voice_starts = 0_usize;
        let mut voice_frames = 0_usize;
        let mut voice_ends = 0_usize;
        while let Some(ev) = core.pop_event::<DExtra>() {
            match ev {
                Event::VoiceStart { stream_id, .. } => {
                    assert_eq!(stream_id.get(), 0x19C9);
                    voice_starts += 1;
                }
                Event::VoiceFrame { stream_id, .. } => {
                    assert_eq!(stream_id.get(), 0x19C9);
                    voice_frames += 1;
                }
                Event::VoiceEnd { stream_id, reason } => {
                    assert_eq!(stream_id.get(), 0x19C9);
                    assert_eq!(reason, VoiceEndReason::Eot);
                    voice_ends += 1;
                }
                other => unreachable!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            voice_starts, 1,
            "expected exactly 1 VoiceStart across 3 superframes of header retransmits, got {voice_starts}"
        );
        assert_eq!(
            voice_frames,
            3 * 21,
            "expected all 63 voice frames surfaced"
        );
        assert_eq!(voice_ends, 1, "expected exactly 1 VoiceEnd");
        Ok(())
    }

    /// The voice-inactivity timer is armed on each voice-frame
    /// arrival and fires `VoiceEnd(Inactivity)` when the active
    /// stream has been silent for `VOICE_INACTIVITY_TIMEOUT` without
    /// an explicit EOT. This covers reflector-side EOT loss (remote
    /// source de-keys too fast for the EOT packet to propagate, or
    /// a UDP drop in transit) so the consumer doesn't see a
    /// dangling "in progress" stream.
    ///
    /// This test verifies:
    ///   * voice frames arm the timer (deadline reflects last frame
    ///     + `VOICE_INACTIVITY_TIMEOUT`)
    ///   * advancing time past the deadline and calling
    ///     `handle_timeout` synthesizes a `VoiceEnd(Inactivity)`
    ///   * `active_rx_stream` is cleared so the next header can
    ///     start a fresh stream.
    #[test]
    fn dextra_voice_inactivity_synthesizes_voice_end() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let t0 = Instant::now();
        let mut buf = [0u8; 64];

        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x2BE9), &test_header())?;
        core.handle_input(t0, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        for seq in 0..5_u8 {
            let n = dextra_codec::encode_voice_data(
                &mut buf,
                sid(0x2BE9),
                sequence(seq),
                &VoiceFrame::silence(),
            )?;
            core.handle_input(
                t0 + Duration::from_millis(u64::from(seq) * 20),
                ADDR_DEXTRA,
                buf.get(..n).ok_or("n > buf")?,
            )?;
        }

        // Drain events from the in-progress stream.
        let mut seen_start = false;
        let mut seen_frames = 0_usize;
        while let Some(ev) = core.pop_event::<DExtra>() {
            match ev {
                Event::VoiceStart { .. } => seen_start = true,
                Event::VoiceFrame { .. } => seen_frames += 1,
                other => unreachable!("unexpected pre-timeout event: {other:?}"),
            }
        }
        assert!(seen_start, "expected VoiceStart before inactivity");
        assert_eq!(seen_frames, 5, "expected 5 VoiceFrames before inactivity");

        // Timer infrastructure: next_deadline picks up the
        // voice-inactivity deadline.
        let next = core.next_deadline().ok_or("no next_deadline")?;
        let voice_timeout = dextra::consts::VOICE_INACTIVITY_TIMEOUT;
        let expected = t0 + Duration::from_millis(80) + voice_timeout;
        assert!(
            next <= expected,
            "voice-inactivity deadline should be at or before last_voice_frame + timeout"
        );

        // Advance past the deadline and tick: expiry branch fires
        // and synthesizes VoiceEnd(Inactivity) for the orphaned
        // stream, clearing `active_rx_stream`.
        let t_fire = t0 + Duration::from_millis(80) + voice_timeout + Duration::from_millis(1);
        core.handle_timeout(t_fire);

        let ev = core.pop_event::<DExtra>().ok_or("no VoiceEnd on timeout")?;
        assert!(
            matches!(ev, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0x2BE9 && reason == VoiceEndReason::Inactivity),
            "expected VoiceEnd(Inactivity) for orphaned stream, got {ev:?}"
        );
        assert!(
            core.pop_event::<DExtra>().is_none(),
            "no further events expected after the inactivity VoiceEnd"
        );
        Ok(())
    }

    /// Same regression for DCS: its header is embedded in every
    /// voice frame, so the dedup check has to fire on every
    /// `seq == 0` after the first one.
    #[test]
    fn dcs_superframe_boundary_emits_single_voice_start() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let mut buf = [0u8; 128];
        // 3 superframes = 63 voice frames, each carrying the header.
        for sf in 0..3 {
            for seq in 0..21_u8 {
                let n = dcs_codec::encode_voice(
                    &mut buf,
                    &test_header(),
                    sid(0x789A),
                    sequence(seq),
                    &non_silence_frame(),
                    false,
                )?;
                core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;
                // Silence `sf` unused warning on non-debug builds.
                let _ = sf;
            }
        }
        let n = dcs_codec::encode_voice(
            &mut buf,
            &test_header(),
            sid(0x789A),
            FrameSequence::ZERO,
            &VoiceFrame::silence(),
            true,
        )?;
        core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;

        let mut voice_starts = 0_usize;
        let mut voice_frames = 0_usize;
        let mut voice_ends = 0_usize;
        while let Some(ev) = core.pop_event::<Dcs>() {
            match ev {
                Event::VoiceStart { stream_id, .. } => {
                    assert_eq!(stream_id.get(), 0x789A);
                    voice_starts += 1;
                }
                Event::VoiceFrame { stream_id, .. } => {
                    assert_eq!(stream_id.get(), 0x789A);
                    voice_frames += 1;
                }
                Event::VoiceEnd { stream_id, reason } => {
                    assert_eq!(stream_id.get(), 0x789A);
                    assert_eq!(reason, VoiceEndReason::Eot);
                    voice_ends += 1;
                }
                other => unreachable!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            voice_starts, 1,
            "DCS: expected 1 VoiceStart, got {voice_starts}"
        );
        assert_eq!(voice_frames, 3 * 21, "DCS: expected 63 voice frames");
        assert_eq!(voice_ends, 1, "DCS: expected 1 VoiceEnd");
        Ok(())
    }

    #[test]
    fn dplus_handle_voice_header_emits_voice_start() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        let n = dplus_codec::encode_voice_header(&mut buf, sid(0x4567), &test_header())?;
        core.handle_input(now, ADDR_DPLUS, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DPlus>().ok_or("no event")?;
        assert!(matches!(event, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x4567));
        Ok(())
    }

    #[test]
    fn dplus_handle_voice_eot_emits_voice_end() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        let mut buf = [0u8; 64];
        // Establish the stream first so the EOT's stream_id matches
        // `active_rx_stream`. Without the VoiceHeader, `emit_voice_end`
        // ignores the EOT as a dangling close of a stream the
        // consumer never saw start; see the mid-stream sid-change
        // guard in `emit_voice_start_if_new`.
        let n = dplus_codec::encode_voice_header(&mut buf, sid(0x4567), &test_header())?;
        core.handle_input(now, ADDR_DPLUS, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DPlus>().ok_or("no VoiceStart")?);
        let n = dplus_codec::encode_voice_eot(&mut buf, sid(0x4567), FrameSequence::ZERO)?;
        core.handle_input(now, ADDR_DPLUS, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DPlus>().ok_or("no event")?;
        assert!(matches!(event, Event::VoiceEnd { .. }));
        Ok(())
    }

    fn non_silence_frame() -> VoiceFrame {
        VoiceFrame {
            ambe: [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09],
            slow_data: [0xAA, 0xBB, 0xCC],
        }
    }

    #[test]
    fn dcs_handle_first_voice_frame_emits_voice_start() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let mut buf = [0u8; 128];
        let n = dcs_codec::encode_voice(
            &mut buf,
            &test_header(),
            sid(0x789A),
            FrameSequence::ZERO,
            &non_silence_frame(),
            false,
        )?;
        core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<Dcs>().ok_or("no event")?;
        assert!(matches!(event, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x789A));
        Ok(())
    }

    #[test]
    fn dcs_handle_subsequent_voice_frame_emits_voice_frame() -> TestResult {
        // A DCS voice frame with `seq > 0` arriving without a prior
        // `seq == 0` frame (lost initial-header packet or late-join)
        // now synthesizes a VoiceStart first so consumers always see
        // a stream-start event before any VoiceFrame. The frame data
        // is then surfaced as VoiceFrame.
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let mut buf = [0u8; 128];
        let n = dcs_codec::encode_voice(
            &mut buf,
            &test_header(),
            sid(0x789A),
            sequence(5),
            &non_silence_frame(),
            false,
        )?;
        core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;
        let first = core.pop_event::<Dcs>().ok_or("no first event")?;
        assert!(
            matches!(first, Event::VoiceStart { stream_id, .. } if stream_id.get() == 0x789A),
            "expected VoiceStart, got {first:?}"
        );
        let second = core.pop_event::<Dcs>().ok_or("no second event")?;
        assert!(
            matches!(second, Event::VoiceFrame { stream_id, seq, .. } if stream_id.get() == 0x789A && seq == 5),
            "expected VoiceFrame(seq=5), got {second:?}"
        );
        Ok(())
    }

    #[test]
    fn dcs_handle_voice_end_emits_voice_end() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let mut buf = [0u8; 128];
        // Establish the stream with a non-end voice packet first.
        // DCS embeds the header in every frame, so this is also the
        // VoiceStart trigger. `emit_voice_end` now ignores EOTs
        // for non-active streams. Using `non_silence_frame()` so
        // the frame isn't mistaken for a DCS keepalive payload by
        // the codec (silence-patterned voice packets are treated
        // as keepalive elsewhere).
        let n = dcs_codec::encode_voice(
            &mut buf,
            &test_header(),
            sid(0x789A),
            FrameSequence::ZERO,
            &non_silence_frame(),
            false,
        )?;
        core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<Dcs>().ok_or("no VoiceStart")?);
        drop(core.pop_event::<Dcs>().ok_or("no VoiceFrame")?);
        let n = dcs_codec::encode_voice(
            &mut buf,
            &test_header(),
            sid(0x789A),
            FrameSequence::ZERO,
            &VoiceFrame::silence(),
            true,
        )?;
        core.handle_input(now, ADDR_DCS, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<Dcs>().ok_or("no event")?;
        assert!(
            matches!(event, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0x789A && reason == VoiceEndReason::Eot)
        );
        Ok(())
    }
}
