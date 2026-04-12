//! Consumer-visible events from the client session.
//!
//! [`Event`] is the consumer-visible enum surfaced by the typestate
//! session. Each variant carries the same data regardless of
//! protocol — the `P: Protocol` parameter is carried through a
//! hidden phantom variant so the type is generic without bloating
//! the other variants with a `PhantomData` field each.

use std::convert::Infallible;
use std::marker::PhantomData;
use std::net::SocketAddr;

use crate::header::DStarHeader;
use crate::types::StreamId;
use crate::validator::Diagnostic;
use crate::voice::VoiceFrame;

use super::protocol::Protocol;

/// One event surfaced by the client session machine.
///
/// The `P: Protocol` parameter is a phantom — every variant carries
/// the same data regardless of protocol. The phantom is for
/// compile-time discrimination only, confined to a hidden
/// [`Event::__Phantom`] variant that cannot be constructed (its
/// payload is an uninhabited [`Infallible`]).
///
/// All variants are populated: [`Event::Connected`], [`Event::Disconnected`],
/// [`Event::PollEcho`], and the voice events.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Event<P: Protocol> {
    /// Session has transitioned to `Connected`.
    Connected {
        /// Peer address of the reflector.
        peer: SocketAddr,
    },

    /// Session has transitioned to `Disconnected`.
    Disconnected {
        /// Why the session disconnected.
        reason: DisconnectReason,
    },

    /// Reflector keepalive echo received.
    PollEcho {
        /// Peer that sent the echo.
        peer: SocketAddr,
    },

    /// A new voice stream started.
    VoiceStart {
        /// Stream id.
        stream_id: StreamId,
        /// Decoded D-STAR header.
        header: DStarHeader,
        /// Diagnostics observed during header parsing.
        diagnostics: Vec<Diagnostic>,
    },

    /// A voice data frame within an active stream.
    VoiceFrame {
        /// Stream id.
        stream_id: StreamId,
        /// Frame seq.
        seq: u8,
        /// Voice frame.
        frame: VoiceFrame,
    },

    /// Voice stream ended (real EOT or synthesized after timeout).
    VoiceEnd {
        /// Stream id.
        stream_id: StreamId,
        /// Real EOT vs synthesized after inactivity.
        reason: VoiceEndReason,
    },

    /// Hidden phantom variant that carries the `P` type parameter.
    ///
    /// This variant cannot be constructed because its payload is
    /// [`Infallible`]. It exists only so the `Event<P>` type is
    /// generic over `P` without embedding a `PhantomData` field in
    /// every public variant.
    #[doc(hidden)]
    __Phantom {
        /// Uninhabited — prevents construction of this variant.
        never: Infallible,
        /// Phantom marker for `P`.
        _protocol: PhantomData<P>,
    },
}

/// Why the session disconnected.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// Reflector explicitly NAK'd the connection.
    Rejected,
    /// Reflector acknowledged the unlink.
    UnlinkAcked,
    /// Local timeout — keepalive inactivity.
    KeepaliveInactivity,
    /// Local timeout — disconnect ACK never arrived.
    DisconnectTimeout,
}

/// Why a voice stream ended.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceEndReason {
    /// Real EOT packet received.
    Eot,
    /// No voice frames for the protocol's inactivity window —
    /// synthesized end.
    Inactivity,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::client::protocol::{DExtra, DPlus, Dcs};
    use std::net::{IpAddr, Ipv4Addr};

    const ADDR_DEXTRA: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const ADDR_DPLUS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const ADDR_DCS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30051);

    #[test]
    fn event_connected_constructible_dextra() {
        let e: Event<DExtra> = Event::Connected { peer: ADDR_DEXTRA };
        assert!(matches!(e, Event::Connected { .. }));
    }

    #[test]
    fn event_connected_constructible_dplus() {
        let e: Event<DPlus> = Event::Connected { peer: ADDR_DPLUS };
        assert!(matches!(e, Event::Connected { .. }));
    }

    #[test]
    fn event_connected_constructible_dcs() {
        let e: Event<Dcs> = Event::Connected { peer: ADDR_DCS };
        assert!(matches!(e, Event::Connected { .. }));
    }

    #[test]
    fn event_disconnected_carries_reason() {
        let e: Event<DExtra> = Event::Disconnected {
            reason: DisconnectReason::KeepaliveInactivity,
        };
        assert!(
            matches!(e, Event::Disconnected { reason } if reason == DisconnectReason::KeepaliveInactivity),
            "expected Disconnected/KeepaliveInactivity, got {e:?}"
        );
    }

    #[test]
    fn event_poll_echo_carries_peer() {
        let e: Event<DExtra> = Event::PollEcho { peer: ADDR_DEXTRA };
        assert!(
            matches!(e, Event::PollEcho { peer } if peer == ADDR_DEXTRA),
            "expected PollEcho, got {e:?}"
        );
    }

    #[test]
    fn disconnect_reason_variants_distinct() {
        assert_ne!(DisconnectReason::Rejected, DisconnectReason::UnlinkAcked);
        assert_ne!(
            DisconnectReason::KeepaliveInactivity,
            DisconnectReason::DisconnectTimeout
        );
    }

    #[test]
    fn voice_end_reason_variants_distinct() {
        assert_ne!(VoiceEndReason::Eot, VoiceEndReason::Inactivity);
    }
}
