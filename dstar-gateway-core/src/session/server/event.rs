//! Server-side session events.
//!
//! [`ServerEvent`] is the consumer-visible enum surfaced by the
//! server-side session machine. The `P: Protocol` parameter is a
//! phantom discriminator — every variant carries the same data
//! regardless of protocol, mirroring the client-side `Event<P>`
//! design.

use std::convert::Infallible;
use std::marker::PhantomData;
use std::net::SocketAddr;

use crate::header::DStarHeader;
use crate::session::client::Protocol;
use crate::types::{Callsign, Module, StreamId};
use crate::voice::VoiceFrame;

/// Reason a reflector rejected or evicted a client.
///
/// Stripped of any protocol-specific NAK code. Carries a
/// human-readable reason so consumers (logs, metrics, tests) can
/// explain the decision without needing to know which authorizer or
/// health check fired.
///
/// This mirrors `dstar_gateway_server::RejectReason` at the event
/// layer so the core can surface rejections without a cross-crate
/// dependency on the server-side authorizer types.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ClientRejectedReason {
    /// Reflector at capacity.
    Busy,
    /// Callsign or IP is banlisted.
    Banned {
        /// Human-readable reason.
        reason: String,
    },
    /// The requested module is not configured on this reflector.
    UnknownModule,
    /// Per-module max client count exceeded.
    MaxClients,
    /// Custom rejection.
    Custom {
        /// Numeric code (NOT a protocol code — internal).
        code: u8,
        /// Human-readable message.
        message: String,
    },
}

/// One event surfaced by the server-side session machine.
///
/// The `P: Protocol` parameter is a phantom — every variant carries
/// the same data regardless of protocol. The phantom is for
/// compile-time discrimination only, confined to a hidden
/// [`ServerEvent::__Phantom`] variant that cannot be constructed
/// (its payload is an uninhabited [`Infallible`]).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum ServerEvent<P: Protocol> {
    /// A new client has linked to a module.
    ClientLinked {
        /// Client peer address.
        peer: SocketAddr,
        /// Client callsign.
        callsign: Callsign,
        /// Module the client linked to.
        module: Module,
    },
    /// A client has unlinked.
    ClientUnlinked {
        /// Client peer address.
        peer: SocketAddr,
    },
    /// A client started a voice stream.
    ClientStreamStarted {
        /// Client peer.
        peer: SocketAddr,
        /// Stream id.
        stream_id: StreamId,
        /// The header they sent.
        header: DStarHeader,
    },
    /// A client sent a voice frame.
    ClientStreamFrame {
        /// Client peer.
        peer: SocketAddr,
        /// Stream id.
        stream_id: StreamId,
        /// Frame seq.
        seq: u8,
        /// Voice frame.
        frame: VoiceFrame,
    },
    /// A client ended a voice stream.
    ClientStreamEnded {
        /// Client peer.
        peer: SocketAddr,
        /// Stream id.
        stream_id: StreamId,
    },
    /// A link attempt was refused by the shell authorizer.
    ///
    /// Emitted before any handle is created — the rejected client is
    /// *not* present in the pool. The reflector additionally enqueues
    /// a protocol-appropriate NAK to the peer.
    ClientRejected {
        /// Client peer that was rejected.
        peer: SocketAddr,
        /// Why the authorizer refused the link.
        reason: ClientRejectedReason,
    },
    /// Voice from a read-only client was dropped.
    ///
    /// Emitted when a client whose [`AccessPolicy`] is `ReadOnly`
    /// sends a voice header / data / EOT packet. The reflector drops
    /// the frame silently on the wire so the originator isn't told
    /// the difference — this event lets consumers observe the drop
    /// for metrics and audit purposes without exposing it on-air.
    ///
    /// [`AccessPolicy`]: https://docs.rs/dstar-gateway-server/latest/dstar_gateway_server/enum.AccessPolicy.html
    VoiceFromReadOnlyDropped {
        /// Client peer that attempted to transmit.
        peer: SocketAddr,
        /// Stream id of the dropped frame.
        stream_id: StreamId,
    },
    /// A client was evicted by the reflector.
    ///
    /// Emitted when the shell removes a client for reasons unrelated
    /// to a protocol-level UNLINK — e.g. the send-failure threshold
    /// was exceeded or a health check fired. The peer entry has
    /// already been removed from the pool by the time this event is
    /// observed.
    ClientEvicted {
        /// Client peer that was evicted.
        peer: SocketAddr,
        /// Human-readable reason for eviction.
        reason: String,
    },

    /// Hidden phantom variant that carries the `P` type parameter.
    ///
    /// This variant cannot be constructed because its payload is
    /// [`Infallible`]. It exists only so the `ServerEvent<P>` type
    /// is generic over `P` without embedding a `PhantomData` field in
    /// every public variant.
    #[doc(hidden)]
    __Phantom {
        /// Uninhabited — prevents construction of this variant.
        #[doc(hidden)]
        never: Infallible,
        /// Phantom marker for `P`.
        #[doc(hidden)]
        _p: PhantomData<P>,
    },
}

#[cfg(test)]
mod tests {
    use super::{Callsign, ClientRejectedReason, Module, ServerEvent, StreamId};
    use crate::session::client::{DExtra, DPlus, Dcs};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const ADDR_DPLUS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const ADDR_DCS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30051);

    #[expect(clippy::unwrap_used, reason = "const-validated: n is non-zero")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    #[test]
    fn client_linked_constructs_dextra() {
        let e: ServerEvent<DExtra> = ServerEvent::ClientLinked {
            peer: ADDR,
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            module: Module::C,
        };
        assert!(matches!(e, ServerEvent::ClientLinked { .. }));
    }

    #[test]
    fn client_rejected_carries_reason() {
        let e: ServerEvent<DExtra> = ServerEvent::ClientRejected {
            peer: ADDR,
            reason: ClientRejectedReason::Banned {
                reason: "bad actor".to_string(),
            },
        };
        assert!(
            matches!(
                &e,
                ServerEvent::ClientRejected {
                    peer,
                    reason: ClientRejectedReason::Banned { .. },
                } if *peer == ADDR
            ),
            "expected ClientRejected/Banned, got {e:?}"
        );
    }

    #[test]
    fn voice_from_readonly_dropped_carries_stream_id() {
        let sid = sid(0xBEEF);
        let e: ServerEvent<DExtra> = ServerEvent::VoiceFromReadOnlyDropped {
            peer: ADDR,
            stream_id: sid,
        };
        assert!(
            matches!(e, ServerEvent::VoiceFromReadOnlyDropped { stream_id, .. } if stream_id == sid),
            "expected VoiceFromReadOnlyDropped, got {e:?}"
        );
    }

    #[test]
    fn client_evicted_carries_reason_string() {
        let e: ServerEvent<DExtra> = ServerEvent::ClientEvicted {
            peer: ADDR,
            reason: "too many send failures".to_string(),
        };
        assert!(
            matches!(
                &e,
                ServerEvent::ClientEvicted { peer, reason }
                    if *peer == ADDR && reason == "too many send failures"
            ),
            "expected ClientEvicted, got {e:?}"
        );
    }

    #[test]
    fn client_linked_constructs_dplus() {
        let e: ServerEvent<DPlus> = ServerEvent::ClientLinked {
            peer: ADDR_DPLUS,
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            module: Module::C,
        };
        assert!(matches!(e, ServerEvent::ClientLinked { .. }));
    }

    #[test]
    fn client_linked_constructs_dcs() {
        let e: ServerEvent<Dcs> = ServerEvent::ClientLinked {
            peer: ADDR_DCS,
            callsign: Callsign::from_wire_bytes(*b"W1AW    "),
            module: Module::C,
        };
        assert!(matches!(e, ServerEvent::ClientLinked { .. }));
    }

    #[test]
    fn client_unlinked_carries_peer() {
        let e: ServerEvent<DExtra> = ServerEvent::ClientUnlinked { peer: ADDR };
        assert!(
            matches!(e, ServerEvent::ClientUnlinked { peer } if peer == ADDR),
            "expected ClientUnlinked, got {e:?}"
        );
    }

    #[test]
    fn client_stream_ended_carries_stream_id() {
        let sid = sid(0xBEEF);
        let e: ServerEvent<DExtra> = ServerEvent::ClientStreamEnded {
            peer: ADDR,
            stream_id: sid,
        };
        assert!(
            matches!(e, ServerEvent::ClientStreamEnded { stream_id, .. } if stream_id == sid),
            "expected ClientStreamEnded, got {e:?}"
        );
    }
}
