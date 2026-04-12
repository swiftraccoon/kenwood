//! `AnySession<P>` — storage-friendly enum wrapping a session in any state.

use super::protocol::{DPlus, Protocol};
use super::session::Session;
use super::state::{
    Authenticated, ClientStateKind, Closed, Configured, Connected, Connecting, Disconnecting,
};

/// Storage-friendly enum wrapping a [`Session<P, S>`] in any state.
///
/// Use this when you need to keep a session in a struct field that
/// might be in any state (e.g., a long-lived REPL state). For
/// individual transitions, use the typed [`Session<P, S>`] directly.
///
/// Note: [`AnySession<P>`] is generic over the protocol. The
/// [`Self::Authenticated`] variant is hard-coded to [`DPlus`] because
/// the typestate guarantees only `DPlus` reaches that state. This is
/// a known wart of full typestate that we accept.
#[non_exhaustive]
#[derive(Debug)]
pub enum AnySession<P: Protocol> {
    /// [`Configured`] state — session built but no I/O has happened.
    Configured(Session<P, Configured>),
    /// [`Authenticated`] state — `DPlus` only, TCP auth completed.
    Authenticated(Session<DPlus, Authenticated>),
    /// [`Connecting`] state — LINK packet sent, waiting for ACK.
    Connecting(Session<P, Connecting>),
    /// [`Connected`] state — operational.
    Connected(Session<P, Connected>),
    /// [`Disconnecting`] state — UNLINK sent, waiting for ACK.
    Disconnecting(Session<P, Disconnecting>),
    /// [`Closed`] state — terminal.
    Closed(Session<P, Closed>),
}

impl<P: Protocol> AnySession<P> {
    /// Runtime state discriminator for whichever state the session is in.
    #[must_use]
    pub const fn state_kind(&self) -> ClientStateKind {
        match self {
            Self::Configured(s) => s.state_kind(),
            Self::Authenticated(s) => s.state_kind(),
            Self::Connecting(s) => s.state_kind(),
            Self::Connected(s) => s.state_kind(),
            Self::Disconnecting(s) => s.state_kind(),
            Self::Closed(s) => s.state_kind(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::client::core::SessionCore;
    use crate::session::client::protocol::DExtra;
    use crate::types::{Callsign, Module, ProtocolKind};
    use std::marker::PhantomData;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    fn dextra_configured_any() -> AnySession<DExtra> {
        let core = SessionCore::new(
            ProtocolKind::DExtra,
            Callsign::from_wire_bytes(*b"W1AW    "),
            Module::B,
            Module::C,
            PEER,
        );
        AnySession::Configured(Session {
            inner: core,
            _protocol: PhantomData,
            _state: PhantomData,
        })
    }

    #[test]
    fn any_session_configured_state_kind() {
        let s = dextra_configured_any();
        assert_eq!(s.state_kind(), ClientStateKind::Configured);
    }
}
