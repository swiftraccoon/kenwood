//! Protocol-erased client session core.
//!
//! [`SessionCore`] is the runtime-erased state machine that drives a
//! single D-STAR reflector client session. It is wrapped by the
//! typestate [`Session<P, S>`][session] — the typestate sits on top
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
use crate::error::{DcsError, Error, ProtocolError, StateError};
use crate::header::DStarHeader;
use crate::session::driver::Transmit;
use crate::session::outbox::{OutboundPacket, Outbox};
use crate::session::timer_wheel::TimerWheel;
use crate::types::{Callsign, Module, ProtocolKind, StreamId};
use crate::validator::{Diagnostic, VecSink};
use crate::voice::VoiceFrame;

use super::event::{DisconnectReason, Event, VoiceEndReason};
use super::protocol::Protocol;
use super::state::ClientStateKind;

/// Named timer: keepalive poll.
const TIMER_KEEPALIVE: &str = "keepalive";
/// Named timer: keepalive inactivity (peer silent for too long).
const TIMER_KEEPALIVE_INACTIVITY: &str = "keepalive_inactivity";
/// Named timer: waiting for disconnect ACK from reflector.
const TIMER_DISCONNECT_DEADLINE: &str = "disconnect_deadline";
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
const TIMER_VOICE_INACTIVITY: &str = "voice_inactivity";

/// How long the currently-active inbound stream must be silent
/// (no voice frames / no matching-`stream_id` headers) before a
/// different `stream_id` is allowed to take over as the active
/// stream.
///
/// Reflectors that bridge multiple upstream gateways can forward
/// two simultaneous talkers' packets to the same module, producing
/// rapidly-alternating `stream_id` values (observed: ~2-5 swaps
/// per second at REF030 C during a round-table). Swapping
/// `active_rx_stream` on every change tears the radio modem down
/// and re-keys it at that frequency and destroys the audio.
///
/// The threshold is deliberately longer than `DPlus`'s ~420 ms
/// superframe boundary so a normal inter-header gap (during which
/// voice frames still flow) doesn't count as silence, but shorter
/// than a real talker pause (which usually takes >1 s to release
/// PTT cleanly). 500 ms hits the right balance on live reflector
/// traffic.
const STREAM_TAKEOVER_THRESHOLD: Duration = Duration::from_millis(500);

/// Internal protocol-erased event record.
///
/// [`SessionCore::pop_event`] is generic over `P: Protocol` and
/// converts each `RawEvent` into an [`Event<P>`] at drain time.
#[derive(Debug, Clone)]
enum RawEvent {
    /// Transitioned to `Connected`.
    Connected {
        /// Peer that accepted us.
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
    /// Voice stream started — header arrived from the reflector.
    VoiceStart {
        /// D-STAR stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: DStarHeader,
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
/// core does not itself enforce state transitions at compile time —
/// that discipline is the wrapper's job.
pub struct SessionCore {
    /// Which protocol this session speaks.
    kind: ProtocolKind,
    /// Logged-in client callsign.
    callsign: Callsign,
    /// Client local module letter.
    local_module: Module,
    /// Reflector module letter we are linked (or linking) to.
    reflector_module: Module,
    /// Reflector's own callsign (e.g. `REF030`, `XLX307`, `DCS030`).
    ///
    /// Required by the DCS wire format — the 519-byte LINK packet,
    /// the 19-byte UNLINK packet, and the 17-byte POLL packet all
    /// embed the target reflector's callsign at specific offsets.
    /// If the DCS client sends the wrong reflector callsign the
    /// target reflector will drop the packet with no response.
    ///
    /// `None` means the caller did not supply one. For `DPlus` and
    /// `DExtra` this is harmless — neither protocol embeds the
    /// reflector callsign on the wire. For `DCS` the session falls
    /// back to a `DCS001  ` default and emits a warning at construction
    /// time so the operator can see why connections to
    /// non-DCS001 reflectors fail.
    reflector_callsign: Option<Callsign>,
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
    /// so the cache is not consulted on those protocols — it is still
    /// populated for symmetry and future header retransmit support.
    cached_tx_header: Option<DStarHeader>,
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
    /// consumer — which resets any decoder state kept per stream and
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
    /// Used by [`Self::emit_voice_start_if_new`] to distinguish a
    /// legitimate mid-stream takeover (new talker picks up where
    /// the previous one stopped) from a concurrent stream (reflector
    /// interleaves frames from two simultaneous talkers). A header
    /// for a different `stream_id` that arrives while the active
    /// stream is still producing frames indicates the latter, and
    /// must not swap `active_rx_stream` — doing so tears down and
    /// re-keys the radio modem 2-4 times per second and produces
    /// audible chopping between talkers. Observed live at REF030 C
    /// when two gateways forwarded the same QSO to the reflector,
    /// 252 mid-stream sid changes in a single session log before
    /// this guard was added.
    ///
    /// Cleared on `VoiceEnd` and on session reset. Updated on every
    /// voice frame / header push for the active stream (and only
    /// the active stream — concurrent streams must not keep the
    /// liveness timer alive).
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
            .field("reflector_callsign", &self.reflector_callsign)
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
    /// consumer's per-stream AMBE decoder state — the user hears the
    /// first few frames of the stream looping indefinitely.
    ///
    /// If `stream_id` differs from the current active stream, an
    /// [`RawEvent::VoiceEnd`] with
    /// [`VoiceEndReason::Inactivity`] is synthesized for the old
    /// stream before the new `VoiceStart` — covers the case where a
    /// new talker takes the module mid-flight without an EOT from
    /// the previous one.
    fn emit_voice_start_if_new(&mut self, now: Instant, stream_id: StreamId, header: DStarHeader) {
        match self.active_rx_stream {
            Some(current) if current == stream_id => {
                tracing::trace!(
                    target: "dstar_gateway_core::session::client",
                    stream_id = format_args!("{:#06X}", stream_id.get()),
                    "suppressing retransmitted voice header for active stream"
                );
                // Header retransmits by themselves are NOT evidence
                // that voice data is still flowing (the reflector
                // keeps re-sending the header for late joiners even
                // after the source has stopped). Only the VoiceFrame
                // dispatch re-arms the inactivity timer.
            }
            Some(old_sid) => {
                // Different stream_id arrived while another is
                // active. Only honour this as a takeover if the
                // active stream has been silent for longer than
                // [`STREAM_TAKEOVER_THRESHOLD`]. Otherwise the
                // reflector is forwarding two simultaneous talkers
                // and swapping `active_rx_stream` would re-key the
                // radio modem 2-4x per second.
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
                    "mid-stream sid change — synthesizing VoiceEnd for old + VoiceStart for new"
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
                    "new voice stream — emitting VoiceStart"
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
    /// are silently ignored — the consumer never saw a matching
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
    pub fn new(
        kind: ProtocolKind,
        callsign: Callsign,
        local_module: Module,
        reflector_module: Module,
        peer: SocketAddr,
    ) -> Self {
        Self::new_with_reflector_callsign(
            kind,
            callsign,
            local_module,
            reflector_module,
            peer,
            None,
        )
    }

    /// Build a new [`SessionCore`] with an explicit reflector
    /// callsign.
    ///
    /// Required for `DCS` sessions that target a non-`DCS001`
    /// reflector — the DCS codec embeds the reflector callsign in
    /// every LINK/UNLINK/POLL packet, and the default fallback is
    /// `DCS001  `. Optional for `DPlus` and `DExtra`, which do not
    /// carry the reflector callsign on the wire.
    ///
    /// Emits a `tracing::warn!` at construction time if the
    /// protocol is `DCS` and `reflector_callsign` is `None`, so
    /// operators can see why connections to non-DCS001 reflectors
    /// fail without the real callsign.
    #[must_use]
    pub fn new_with_reflector_callsign(
        kind: ProtocolKind,
        callsign: Callsign,
        local_module: Module,
        reflector_module: Module,
        peer: SocketAddr,
        reflector_callsign: Option<Callsign>,
    ) -> Self {
        if kind == ProtocolKind::Dcs && reflector_callsign.is_none() {
            tracing::warn!(
                target: "dstar_gateway_core::session",
                %callsign,
                %peer,
                "DCS session constructed without reflector_callsign — \
                 falling back to \"DCS001  \" default. Connections to \
                 any other DCS reflector will fail silently because the \
                 target reflector reads the callsign field from the \
                 LINK packet and drops mismatched traffic. Call \
                 SessionBuilder::reflector_callsign to fix."
            );
        }
        Self {
            kind,
            callsign,
            local_module,
            reflector_module,
            reflector_callsign,
            peer,
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
        self.callsign
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
    /// The typestate wrapper prevents both cases at compile time —
    /// this runtime check is the residual safety net for direct
    /// `SessionCore` users (tests + the protocol-erased fallback path).
    pub fn attach_host_list(&mut self, list: dplus::HostList) -> Result<(), Error> {
        if self.kind != ProtocolKind::DPlus || self.state != ClientStateKind::Configured {
            return Err(Error::State(StateError::WrongState {
                operation: "attach_host_list",
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
    /// Returns [`Error::Protocol`] if a codec encoder reports a
    /// buffer-too-small (should never happen — the scratch
    /// buffers in this core are oversized for every known packet).
    pub fn enqueue_connect(&mut self, now: Instant) -> Result<(), Error> {
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
                    &self.callsign,
                    self.reflector_module,
                    self.local_module,
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
                    &self.callsign,
                    self.local_module,
                    self.reflector_module,
                    &reflector_callsign,
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
    /// Returns [`Error::Protocol`] if a codec encoder fails.
    pub fn enqueue_disconnect(&mut self, now: Instant) -> Result<(), Error> {
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
                let n = dextra::encode_unlink(&mut buf, &self.callsign, self.local_module)
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
                let n = dcs::encode_connect_unlink(
                    &mut buf,
                    &self.callsign,
                    self.local_module,
                    &reflector_callsign,
                )
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
    /// For DCS, the protocol does NOT have a separate header packet
    /// — the first frame (seq=0) carries the embedded header. This
    /// method emits a synthetic silence frame at seq=0 to start the
    /// stream and matches the legacy
    /// [`crate`]-internal behavior.
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
        header: &DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: "enqueue_send_header",
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
                // DCS has no separate header packet — the first frame
                // (seq=0) carries the embedded header. Emit a silence
                // frame at seq=0 to start the stream.
                let silence = VoiceFrame::silence();
                dcs::encode_voice(&mut buf, header, stream_id, 0, &silence, false)
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
    /// is required — DCS embeds the full header in every voice frame.
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
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: "enqueue_send_voice",
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
    /// is required — DCS embeds the full header in every voice frame
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
        seq: u8,
    ) -> Result<(), Error> {
        if self.state != ClientStateKind::Connected {
            return Err(Error::State(StateError::WrongState {
                operation: "enqueue_send_eot",
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
    /// The `peer` argument is the source address of the datagram.
    /// The typestate wrapper filters out datagrams whose source does
    /// not match the expected reflector; the core accepts whatever
    /// the shell passes it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] wrapping the codec error if the
    /// datagram cannot be parsed.
    pub fn handle_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Error> {
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
        // heartbeats, variable-length control, legacy framing) and
        // any of those would previously propagate through `?` and
        // kill the tokio shell's run loop. Record a diagnostic and
        // keep going.
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
        match pkt {
            dplus::ServerPacket::Link1Ack => {
                if self.state == ClientStateKind::Connecting {
                    // First half of the two-step DPlus handshake —
                    // reply with LINK2 immediately.
                    let mut buf = [0u8; 32];
                    let n = dplus::encode_link2(&mut buf, &self.callsign)
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
                Ok(())
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
                Ok(())
            }
            dplus::ServerPacket::Link2Echo { .. } => {
                // Some DPlus servers echo the full 28-byte LINK2 instead
                // of OKRW. Treat it as an accept.
                if self.state == ClientStateKind::Connecting {
                    self.finalize_connected(peer, now);
                }
                Ok(())
            }
            dplus::ServerPacket::UnlinkAck => {
                if self.state == ClientStateKind::Disconnecting {
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
                Ok(())
            }
            dplus::ServerPacket::PollEcho => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
                Ok(())
            }
            dplus::ServerPacket::VoiceHeader { stream_id, header } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_start_if_new(now, stream_id, header);
                Ok(())
            }
            dplus::ServerPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.arm_keepalive_inactivity(now);
                // Only emit frames for the currently-active stream.
                // When the reflector forwards a second concurrent
                // talker's packets, this drops them cleanly rather
                // than mixing both streams' audio into the modem's
                // single-stream FIFO.
                if self.active_rx_stream == Some(stream_id) {
                    self.events.push_back(RawEvent::VoiceFrame {
                        stream_id,
                        seq,
                        frame,
                    });
                    self.arm_voice_inactivity(now);
                    self.last_voice_activity_at = Some(now);
                } else {
                    // Concurrent stream's voice frame dropped. The
                    // corresponding header suppression logs at
                    // debug in `emit_voice_start_if_new`; this trace
                    // makes every single dropped frame visible so
                    // operators can confirm the magnitude of a
                    // doubling event without having to estimate
                    // from the (superframe-cadence) header log.
                    let _ = frame;
                    tracing::trace!(
                        target: "dstar_gateway_core::session::client",
                        concurrent_sid = format_args!("{:#06X}", stream_id.get()),
                        active_sid = ?self.active_rx_stream.map(StreamId::get),
                        seq,
                        "dropping voice frame for concurrent stream"
                    );
                }
                Ok(())
            }
            dplus::ServerPacket::VoiceEot { stream_id, seq: _ } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_end(stream_id, VoiceEndReason::Eot);
                Ok(())
            }
        }
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
        match pkt {
            dextra::ServerPacket::ConnectAck { .. } => {
                if self.state == ClientStateKind::Connecting {
                    self.finalize_connected(peer, now);
                }
                Ok(())
            }
            dextra::ServerPacket::ConnectNak { .. } => {
                if self.state == ClientStateKind::Connecting {
                    self.finalize_rejected();
                } else if self.state == ClientStateKind::Disconnecting {
                    // DExtra reflectors echo the unlink as a NAK on
                    // module-space. Treat it as the unlink ACK.
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
                Ok(())
            }
            dextra::ServerPacket::PollEcho { .. } => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
                Ok(())
            }
            dextra::ServerPacket::VoiceHeader { stream_id, header } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_start_if_new(now, stream_id, header);
                Ok(())
            }
            dextra::ServerPacket::VoiceData {
                stream_id,
                seq,
                frame,
            } => {
                self.arm_keepalive_inactivity(now);
                // See the DPlus branch above — emit only for the
                // active stream so concurrent talkers' frames don't
                // mix into the modem FIFO.
                if self.active_rx_stream == Some(stream_id) {
                    self.events.push_back(RawEvent::VoiceFrame {
                        stream_id,
                        seq,
                        frame,
                    });
                    self.arm_voice_inactivity(now);
                    self.last_voice_activity_at = Some(now);
                } else {
                    let _ = frame;
                    tracing::trace!(
                        target: "dstar_gateway_core::session::client",
                        concurrent_sid = format_args!("{:#06X}", stream_id.get()),
                        active_sid = ?self.active_rx_stream.map(StreamId::get),
                        seq,
                        "dropping voice frame for concurrent stream"
                    );
                }
                Ok(())
            }
            dextra::ServerPacket::VoiceEot { stream_id, seq: _ } => {
                self.arm_keepalive_inactivity(now);
                self.emit_voice_end(stream_id, VoiceEndReason::Eot);
                Ok(())
            }
        }
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
        match pkt {
            dcs::ServerPacket::ConnectAck { .. } => {
                if self.state == ClientStateKind::Connecting {
                    self.finalize_connected(peer, now);
                } else if self.state == ClientStateKind::Disconnecting {
                    self.finalize_disconnect(DisconnectReason::UnlinkAcked);
                }
                Ok(())
            }
            dcs::ServerPacket::ConnectNak { .. } => {
                if self.state == ClientStateKind::Connecting {
                    self.finalize_rejected();
                }
                Ok(())
            }
            dcs::ServerPacket::PollEcho { .. } => {
                self.arm_keepalive_inactivity(now);
                self.events.push_back(RawEvent::PollEcho { peer });
                Ok(())
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
                    // surface the frame as `VoiceFrame` too — the
                    // `seq == 0` frame carries real voice data plus
                    // the superframe sync pattern in slow-data, so
                    // dropping it (as the pre-fix code did) left
                    // audible gaps every 420 ms.
                    //
                    // Frames for a concurrent stream whose header
                    // was suppressed by
                    // [`Self::emit_voice_start_if_new`] are dropped
                    // here — see the DPlus branch for rationale.
                    self.emit_voice_start_if_new(now, stream_id, header);
                    if self.active_rx_stream == Some(stream_id) {
                        self.events.push_back(RawEvent::VoiceFrame {
                            stream_id,
                            seq,
                            frame,
                        });
                        self.arm_voice_inactivity(now);
                        self.last_voice_activity_at = Some(now);
                    } else {
                        let _ = frame;
                        tracing::trace!(
                            target: "dstar_gateway_core::session::client",
                            concurrent_sid = format_args!("{:#06X}", stream_id.get()),
                            active_sid = ?self.active_rx_stream.map(StreamId::get),
                            seq,
                            "dropping voice frame for concurrent stream"
                        );
                    }
                }
                Ok(())
            }
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
        if self.state == ClientStateKind::Connected && self.timers.is_expired(TIMER_KEEPALIVE, now)
        {
            self.enqueue_poll(now);
            self.timers
                .set(TIMER_KEEPALIVE, now + self.keepalive_interval());
        }

        // Voice-inactivity expiry: the active inbound stream has
        // produced no voice frames for `VOICE_INACTIVITY_TIMEOUT`
        // (2 s on all three protocols). Synthesize a
        // `VoiceEnd(Inactivity)` so the consumer isn't left with a
        // dangling "in progress" stream when the reflector drops
        // an EOT packet or the remote source de-keys too fast for
        // the EOT to be forwarded (observed at REF030 C: 15 frames
        // of M6JBE audio relayed cleanly, then silence with no
        // EOT, stream stuck "active" indefinitely).
        //
        // This branch was previously disabled because the
        // mmdvm-event-channel backpressure (now fixed by the
        // next_event noise drain) would stall the session loop
        // long enough that any wall-clock deadline fired as a
        // false positive mid-transmission. With the deadlock
        // fixed the loop reliably drains, and the timer is armed
        // on each voice frame so the deadline always sits
        // `VOICE_INACTIVITY_TIMEOUT` past the latest real activity.
        if self.state == ClientStateKind::Connected
            && self.timers.is_expired(TIMER_VOICE_INACTIVITY, now)
        {
            if let Some(stream_id) = self.active_rx_stream {
                tracing::debug!(
                    target: "dstar_gateway_core::session::client",
                    stream_id = format_args!("{:#06X}", stream_id.get()),
                    "voice inactivity expired — synthesizing VoiceEnd(Inactivity)"
                );
                self.emit_voice_end(stream_id, VoiceEndReason::Inactivity);
            } else {
                // Defensive: timer fired but no active stream.
                // Clear it so we don't spin-loop in a stuck state.
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
    /// drain time — the event queue itself is protocol-erased.
    pub fn pop_event<P: Protocol>(&mut self) -> Option<Event<P>> {
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

    /// Enqueue the protocol-appropriate keepalive poll packet.
    ///
    /// Encoder failures are swallowed — the scratch buffers in this
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
                dextra::encode_poll(&mut buf, &self.callsign)
                    .ok()
                    .and_then(|n| buf.get(..n).map(<[u8]>::to_vec))
            }
            ProtocolKind::Dcs => {
                let mut buf = [0u8; 32];
                let reflector_callsign = self.dcs_reflector_callsign();
                dcs::encode_poll_request(&mut buf, &self.callsign, &reflector_callsign)
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
        self.timers
            .set(TIMER_KEEPALIVE, now + self.keepalive_interval());
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

    fn clear_timers(&mut self) {
        self.timers.clear(TIMER_KEEPALIVE);
        self.timers.clear(TIMER_KEEPALIVE_INACTIVITY);
        self.timers.clear(TIMER_DISCONNECT_DEADLINE);
        self.timers.clear(TIMER_VOICE_INACTIVITY);
    }

    fn arm_keepalive_inactivity(&mut self, now: Instant) {
        self.timers.set(
            TIMER_KEEPALIVE_INACTIVITY,
            now + self.keepalive_inactivity_timeout(),
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
            now + self.voice_inactivity_timeout(),
        );
    }

    fn arm_disconnect_deadline(&mut self, now: Instant) {
        self.timers
            .set(TIMER_DISCONNECT_DEADLINE, now + self.disconnect_timeout());
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
    /// Returns the value supplied via
    /// [`Self::new_with_reflector_callsign`] when present, or a
    /// `DCS001  ` fallback when the caller did not supply one. The
    /// fallback is only correct for sessions targeting the DCS001
    /// reflector; for any other target the caller MUST supply the
    /// real reflector callsign via the builder or the DCS server
    /// will drop the LINK packet silently.
    fn dcs_reflector_callsign(&self) -> Callsign {
        self.reflector_callsign
            .unwrap_or_else(|| Callsign::from_wire_bytes(*b"DCS001  "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dplus::HostList;
    use crate::codec::{dcs as dcs_codec, dextra as dextra_codec, dplus as dplus_codec};
    use crate::session::client::protocol::{DExtra, DPlus, Dcs};
    use std::net::{IpAddr, Ipv4Addr};

    const fn cs() -> Callsign {
        Callsign::from_wire_bytes(*b"W1AW    ")
    }

    const ADDR_DEXTRA: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const ADDR_DPLUS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const ADDR_DCS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30051);

    fn new_dextra() -> SessionCore {
        SessionCore::new(
            ProtocolKind::DExtra,
            cs(),
            Module::B,
            Module::C,
            ADDR_DEXTRA,
        )
    }

    fn new_dplus() -> SessionCore {
        SessionCore::new(ProtocolKind::DPlus, cs(), Module::B, Module::C, ADDR_DPLUS)
    }

    fn new_dcs() -> SessionCore {
        SessionCore::new(ProtocolKind::Dcs, cs(), Module::B, Module::C, ADDR_DCS)
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
        let n = dextra_codec::encode_connect_ack(&mut ack, &cs(), Module::C)?;
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
        let n = dcs_codec::encode_connect_ack(
            &mut ack,
            &Callsign::from_wire_bytes(*b"DCS001  "),
            Module::C,
        )?;
        core.handle_input(t0, ADDR_DCS, ack.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<Dcs>().ok_or("no connected event")?);
        Ok((core, t0))
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
        let n = dextra_codec::encode_connect_ack(&mut ack, &cs(), Module::C)?;
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
    fn dextra_connect_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let mut core = new_dextra();
        let t0 = Instant::now();
        core.enqueue_connect(t0)?;
        let _ = core.pop_transmit(t0).ok_or("no link packet")?;

        let mut nak = [0u8; 16];
        let n = dextra_codec::encode_connect_nak(&mut nak, &cs(), Module::C)?;
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
        let n = dextra_codec::encode_connect_nak(&mut nak, &cs(), Module::C)?;
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
        let n = dcs_codec::encode_connect_ack(
            &mut ack,
            &Callsign::from_wire_bytes(*b"DCS001  "),
            Module::C,
        )?;
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
        let n = dcs_codec::encode_connect_nak(
            &mut nak,
            &Callsign::from_wire_bytes(*b"DCS001  "),
            Module::C,
        )?;
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
        assert!(core.drain_diagnostics().is_empty());
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

    // ── Voice TX / RX ────────────────────────────────────────

    use crate::error::DcsError;
    use crate::header::DStarHeader;
    use crate::types::{StreamId, Suffix};
    use crate::voice::VoiceFrame;

    #[expect(clippy::unwrap_used, reason = "const-validated: n is non-zero")]
    const fn sid(n: u16) -> StreamId {
        // Option::unwrap is const since 1.83 — panics at compile
        // time on zero, never at runtime with a non-zero literal.
        StreamId::new(n).unwrap()
    }

    const fn test_header() -> DStarHeader {
        DStarHeader {
            flag1: 0,
            flag2: 0,
            flag3: 0,
            rpt2: Callsign::from_wire_bytes(*b"XRF030 G"),
            rpt1: Callsign::from_wire_bytes(*b"XRF030 C"),
            ur_call: Callsign::from_wire_bytes(*b"CQCQCQ  "),
            my_call: Callsign::from_wire_bytes(*b"W1AW    "),
            my_suffix: Suffix::from_wire_bytes(*b"D75 "),
        }
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
        core.enqueue_send_voice(now, sid(0x1234), 5, &VoiceFrame::silence())?;
        let tx = core.pop_transmit(now).ok_or("no voice tx")?;
        assert_eq!(tx.payload.len(), 29, "DPlus voice data is 29 bytes");
        Ok(())
    }

    #[test]
    fn dextra_connected_enqueue_send_voice_produces_27_bytes() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        core.enqueue_send_voice(now, sid(0x1234), 5, &VoiceFrame::silence())?;
        let tx = core.pop_transmit(now).ok_or("no voice tx")?;
        assert_eq!(tx.payload.len(), 27, "DExtra voice data is 27 bytes");
        Ok(())
    }

    // ── Voice TX: EOT sizes ───────────────────────────────────

    #[test]
    fn dplus_connected_enqueue_send_eot_produces_32_bytes() -> TestResult {
        let (mut core, _) = connected_dplus()?;
        let now = Instant::now();
        core.enqueue_send_eot(now, sid(0x1234), 21)?;
        let tx = core.pop_transmit(now).ok_or("no eot tx")?;
        assert_eq!(tx.payload.len(), 32, "DPlus voice EOT is 32 bytes");
        Ok(())
    }

    #[test]
    fn dextra_connected_enqueue_send_eot_produces_27_bytes() -> TestResult {
        let (mut core, _) = connected_dextra()?;
        let now = Instant::now();
        core.enqueue_send_eot(now, sid(0x1234), 21)?;
        let tx = core.pop_transmit(now).ok_or("no eot tx")?;
        assert_eq!(tx.payload.len(), 27, "DExtra voice EOT is 27 bytes");
        Ok(())
    }

    // ── DCS NoTxHeader error path ─────────────────────────────

    #[test]
    fn dcs_send_voice_without_header_errors() -> TestResult {
        let (mut core, _) = connected_dcs()?;
        let now = Instant::now();
        let result = core.enqueue_send_voice(now, sid(0x1234), 1, &VoiceFrame::silence());
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
        let result = core.enqueue_send_eot(now, sid(0x1234), 1);
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
        core.enqueue_send_voice(now, sid(0x1234), 1, &VoiceFrame::silence())?;
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
        core.enqueue_send_eot(now, sid(0x1234), 21)?;
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
        let result = core.enqueue_send_voice(now, sid(0x1234), 1, &VoiceFrame::silence());
        assert!(matches!(
            result,
            Err(Error::State(StateError::WrongState { .. }))
        ));
    }

    #[test]
    fn enqueue_send_eot_in_configured_state_errors() {
        let mut core = new_dextra();
        let now = Instant::now();
        let result = core.enqueue_send_eot(now, sid(0x1234), 1);
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
        // Regression test: D-STAR voice headers are retransmitted by
        // reflectors every superframe (~21 frames / 420 ms) so late
        // joiners can decode. The previous client emitted VoiceStart
        // on every header packet, which reset per-stream decoder
        // state in the consumer (heard as "first ~80 ms of the
        // stream repeating over and over"). The fix is to track the
        // currently-active stream and suppress duplicate headers.
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
        // — see `emit_voice_start_if_new`. The previous test sent
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
        let n = dextra_codec::encode_voice_data(&mut buf, sid(0xAAAA), 1, &VoiceFrame::silence())?;
        core.handle_input(
            now + Duration::from_millis(20),
            ADDR_DEXTRA,
            buf.get(..n).ok_or("n > buf")?,
        )?;
        // Drain the two events we expect for stream A.
        drop(core.pop_event::<DExtra>().ok_or("no VoiceStart")?);
        drop(core.pop_event::<DExtra>().ok_or("no VoiceFrame")?);
        // B's header arrives only 50 ms after A's last activity —
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
        let n = dextra_codec::encode_voice_data(&mut buf, sid(0xBBBB), 2, &VoiceFrame::silence())?;
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
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0xAAAA), 21)?;
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
        // Voice frames are gated on `active_rx_stream` matching —
        // see `handle_dextra_input`'s VoiceData branch — so
        // establish the stream with a VoiceHeader first.
        let n = dextra_codec::encode_voice_header(&mut buf, sid(0x1234), &test_header())?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DExtra>().ok_or("no VoiceStart")?);
        let n = dextra_codec::encode_voice_data(&mut buf, sid(0x1234), 7, &VoiceFrame::silence())?;
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
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0x1234), 21)?;
        core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
        let event = core.pop_event::<DExtra>().ok_or("no event")?;
        assert!(
            matches!(event, Event::VoiceEnd { stream_id, reason } if stream_id.get() == 0x1234 && reason == VoiceEndReason::Eot)
        );
        Ok(())
    }

    /// Regression scenario: replay the exact wire sequence a running
    /// reflector sends — one header every superframe interleaved
    /// with 21 voice-data frames — and verify that exactly ONE
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
                    seq,
                    &VoiceFrame::silence(),
                )?;
                core.handle_input(now, ADDR_DEXTRA, buf.get(..n).ok_or("n > buf")?)?;
            }
        }
        // Then EOT.
        let n = dextra_codec::encode_voice_eot(&mut buf, sid(0x19C9), 21)?;
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
                seq,
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
                    seq,
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
            21,
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
        // consumer never saw start — see the mid-stream sid-change
        // guard in `emit_voice_start_if_new`.
        let n = dplus_codec::encode_voice_header(&mut buf, sid(0x4567), &test_header())?;
        core.handle_input(now, ADDR_DPLUS, buf.get(..n).ok_or("n > buf")?)?;
        drop(core.pop_event::<DPlus>().ok_or("no VoiceStart")?);
        let n = dplus_codec::encode_voice_eot(&mut buf, sid(0x4567), 21)?;
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
            0,
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
            5,
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
        // Establish the stream with a non-end voice packet first —
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
            0,
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
            21,
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
