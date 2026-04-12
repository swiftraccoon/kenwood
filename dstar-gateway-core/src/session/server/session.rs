//! Typestate `ServerSession<P, S>` wrapper.
//!
//! Thin wrapper over [`ServerSessionCore`] that adds compile-time
//! state discrimination via the `S: ServerState` phantom. The real
//! state transitions happen on the erased core, and this wrapper
//! provides universal accessors that work in any state plus
//! state-gated methods that only resolve in the correct state.

use std::marker::PhantomData;
use std::net::SocketAddr;

use super::core::ServerSessionCore;
use super::event::ServerEvent;
use super::state::{Closed, Link1Received, Linked, ServerState, ServerStateKind, Streaming};

use crate::error::Error;
use crate::session::client::{DPlus, Protocol};
use crate::types::Callsign;

/// A typed server-side reflector session.
///
/// `P` is the protocol marker ([`crate::session::client::DExtra`],
/// [`crate::session::client::DPlus`], [`crate::session::client::Dcs`]).
/// `S` is the server state marker ([`super::Unknown`],
/// [`super::Link1Received`], [`super::Linked`], etc.).
#[derive(Debug)]
pub struct ServerSession<P: Protocol, S: ServerState> {
    pub(crate) inner: ServerSessionCore,
    pub(crate) _protocol: PhantomData<P>,
    pub(crate) _state: PhantomData<S>,
}

// ─── Universal: state inspection works in any state ──────────────

impl<P: Protocol, S: ServerState> ServerSession<P, S> {
    /// Runtime state discriminator.
    #[must_use]
    pub const fn state_kind(&self) -> ServerStateKind {
        self.inner.state_kind()
    }

    /// Peer address.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.inner.peer()
    }

    /// Drain the next server event.
    pub fn pop_event(&mut self) -> Option<ServerEvent<P>> {
        self.inner.pop_event::<P>()
    }
}

// ─── State-gated methods ──────────────────────────────────────────
//
// Each of the following `impl` blocks bounds one state marker. A
// call site in the wrong state will fail to compile with an
// `E0599: no method found` error — the compile-fail trybuild tests
// prove the gates are tight.
//
// The bodies here are intentionally thin wrappers over the
// protocol-erased core; the real state machine lives in `core.rs`.
// Each method's purpose is to assert which state it's callable in
// and to consume the session on terminal transitions.

impl<P: Protocol> ServerSession<P, Streaming> {
    /// Process a voice data packet on a streaming client.
    ///
    /// Only available when `S = Streaming` — calling this in any
    /// other state is a compile error.
    ///
    /// # Errors
    ///
    /// Returns any [`Error`] the core propagates while processing
    /// the event.
    pub fn handle_voice_data(
        mut self,
        now: std::time::Instant,
        bytes: &[u8],
    ) -> Result<Self, Error> {
        self.inner.handle_input(now, bytes)?;
        Ok(self)
    }
}

impl ServerSession<DPlus, Link1Received> {
    /// Process a `DPlus` LINK2 packet after a LINK1 has been
    /// received.
    ///
    /// Only available on `ServerSession<DPlus, Link1Received>` —
    /// calling this before LINK1 (in the [`super::Unknown`] state)
    /// is a compile error.
    ///
    /// # Errors
    ///
    /// Returns any [`Error`] the core propagates.
    pub fn handle_link2(
        mut self,
        now: std::time::Instant,
        _callsign: Callsign,
        bytes: &[u8],
    ) -> Result<ServerSession<DPlus, Linked>, Error> {
        self.inner.handle_input(now, bytes)?;
        Ok(ServerSession {
            inner: self.inner,
            _protocol: PhantomData,
            _state: PhantomData,
        })
    }
}

impl<P: Protocol> ServerSession<P, Linked> {
    /// Process an UNLINK packet on a linked client.
    ///
    /// Only available on `ServerSession<P, Linked>` — calling this
    /// after the session has already reached [`Closed`] is a
    /// compile error.
    ///
    /// # Errors
    ///
    /// Returns any [`Error`] the core propagates.
    pub fn handle_unlink(
        mut self,
        now: std::time::Instant,
        bytes: &[u8],
    ) -> Result<ServerSession<P, Closed>, Error> {
        self.inner.handle_input(now, bytes)?;
        Ok(ServerSession {
            inner: self.inner,
            _protocol: PhantomData,
            _state: PhantomData,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::core::ServerSessionCore;
    use super::super::state::{ServerStateKind, Unknown};
    use super::ServerSession;
    use crate::session::client::DExtra;
    use crate::types::{Module, ProtocolKind};
    use std::marker::PhantomData;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn universal_accessors_work() {
        let inner = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let session: ServerSession<DExtra, Unknown> = ServerSession {
            inner,
            _protocol: PhantomData,
            _state: PhantomData,
        };
        assert_eq!(session.state_kind(), ServerStateKind::Unknown);
        assert_eq!(session.peer(), PEER);
    }

    #[test]
    fn pop_event_returns_none_on_fresh_session() {
        let inner = ServerSessionCore::new(ProtocolKind::DExtra, PEER, Module::C);
        let mut session: ServerSession<DExtra, Unknown> = ServerSession {
            inner,
            _protocol: PhantomData,
            _state: PhantomData,
        };
        assert!(session.pop_event().is_none());
    }
}
