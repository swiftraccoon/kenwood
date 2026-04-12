//! Typestate `Session<P, S>` wrapper.
//!
//! Wraps [`SessionCore`] with compile-time state tracking via the
//! `S: ClientState` phantom. Methods are gated by `impl` blocks on
//! specific `Session<P, S>` shapes — calling `send_voice` on a
//! `Session<P, Configured>` is a compile error, not a runtime error.
//!
//! The `Session<P, S>` is a thin wrapper over [`SessionCore`] (the
//! protocol-erased state machine). The phantom types add zero
//! runtime cost — the entire typestate machinery compiles away.

use std::marker::PhantomData;
use std::net::SocketAddr;
use std::time::Instant;

use crate::codec::dplus::HostList;
use crate::error::{Error, StateError};
use crate::header::DStarHeader;
use crate::session::driver::{Driver, Transmit};
use crate::types::{Callsign, StreamId};
use crate::validator::Diagnostic;
use crate::voice::VoiceFrame;

use super::core::SessionCore;
use super::event::Event;
use super::failed::Failed;
use super::protocol::{DPlus, NoAuthRequired, Protocol};
use super::state::{
    Authenticated, ClientState, ClientStateKind, Closed, Configured, Connected, Connecting,
    Disconnecting,
};

/// A typed reflector session.
///
/// `P` is the protocol marker ([`DPlus`], [`super::DExtra`],
/// [`super::Dcs`]). `S` is the connection state marker
/// ([`Configured`], [`Connecting`], etc.). Methods are gated by
/// `impl` blocks on specific `Session<P, S>` shapes — calling
/// `send_voice` on a `Session<P, Configured>` is a compile error, not
/// a runtime error.
///
/// The `Session<P, S>` is a thin wrapper over [`SessionCore`] (the
/// protocol-erased state machine). The phantom types add zero
/// runtime cost — the entire typestate machinery compiles away.
#[derive(Debug)]
pub struct Session<P: Protocol, S: ClientState> {
    pub(crate) inner: SessionCore,
    pub(crate) _protocol: PhantomData<P>,
    pub(crate) _state: PhantomData<S>,
}

// ─── Universal: state inspection works in any state ────────────

impl<P: Protocol, S: ClientState> Session<P, S> {
    /// Runtime discriminator for the current state.
    #[must_use]
    pub const fn state_kind(&self) -> ClientStateKind {
        self.inner.state_kind()
    }

    /// The reflector address this session was built to talk to.
    #[must_use]
    pub const fn peer(&self) -> SocketAddr {
        self.inner.peer()
    }

    /// The local station callsign.
    #[must_use]
    pub const fn local_callsign(&self) -> Callsign {
        self.inner.callsign()
    }

    /// Drain accumulated diagnostics, if any.
    ///
    /// Returns a `Vec` (not an iterator) because the underlying sink
    /// lives inside the session; returning a borrowed iterator would
    /// force the caller to hold a `&mut Session` for the lifetime of
    /// the iterator. Most consumers call this periodically and
    /// process the batch.
    pub fn diagnostics(&mut self) -> Vec<Diagnostic> {
        self.inner.drain_diagnostics()
    }
}

// ─── Universal `Driver` impl — the typestate wraps the same state
//     machine regardless of which `(P, S)` it's in. ────────────

impl<P: Protocol, S: ClientState> Driver for Session<P, S> {
    type Event = Event<P>;
    type Error = Error;

    fn handle_input(
        &mut self,
        now: Instant,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> Result<(), Self::Error> {
        self.inner.handle_input(now, peer, bytes)
    }

    fn handle_timeout(&mut self, now: Instant) {
        self.inner.handle_timeout(now);
    }

    fn poll_transmit(&mut self, now: Instant) -> Option<Transmit<'_>> {
        self.inner.pop_transmit(now)
    }

    fn poll_event(&mut self) -> Option<Self::Event> {
        self.inner.pop_event::<P>()
    }

    fn poll_timeout(&self) -> Option<Instant> {
        self.inner.next_deadline()
    }
}

// ─── Configured -> Connecting (no-auth protocols only) ─────────

impl<P: Protocol + NoAuthRequired> Session<P, Configured> {
    /// Send the LINK/connect packet and transition to [`Connecting`].
    ///
    /// # Errors
    ///
    /// Returns the original session in the error position if the
    /// state machine refuses the transition (e.g., codec encoder
    /// failure). This lets the caller retry without rebuilding.
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn connect(mut self, now: Instant) -> Result<Session<P, Connecting>, Failed<Self, Error>> {
        match self.inner.enqueue_connect(now) {
            Ok(()) => Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            }),
            Err(error) => Err(Failed {
                session: self,
                error,
            }),
        }
    }
}

// ─── DPlus has an extra hop: Configured -> Authenticated ────────

impl Session<DPlus, Configured> {
    /// Mark the session as authenticated using a previously-fetched host list.
    ///
    /// The actual TCP auth happens in the shell crate — the
    /// sans-io core takes the resulting [`HostList`] here as input.
    ///
    /// # Errors
    ///
    /// Returns the original session in the error position if the
    /// state machine rejects the host list attachment.
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn authenticate(
        mut self,
        hosts: HostList,
    ) -> Result<Session<DPlus, Authenticated>, Failed<Self, Error>> {
        match self.inner.attach_host_list(hosts) {
            Ok(()) => Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            }),
            Err(error) => Err(Failed {
                session: self,
                error,
            }),
        }
    }
}

// ─── DPlus Authenticated -> Connecting ─────────────────────────

impl Session<DPlus, Authenticated> {
    /// Send LINK1 and transition to [`Connecting`].
    ///
    /// # Errors
    ///
    /// Returns the original session in the error position if the
    /// state machine refuses the transition.
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn connect(
        mut self,
        now: Instant,
    ) -> Result<Session<DPlus, Connecting>, Failed<Self, Error>> {
        match self.inner.enqueue_connect(now) {
            Ok(()) => Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            }),
            Err(error) => Err(Failed {
                session: self,
                error,
            }),
        }
    }

    /// Get the cached host list from the TCP auth step.
    ///
    /// The [`Authenticated`] state is entered only after
    /// [`SessionCore::attach_host_list`] succeeds, so the host list is
    /// always present here. A `None` would indicate a bug in
    /// [`SessionCore`]; we fall back to a module-level empty
    /// sentinel rather than panic so lib code stays
    /// `expect_used`-clean.
    #[must_use]
    pub fn host_list(&self) -> &HostList {
        self.inner.host_list().unwrap_or(&EMPTY_HOST_LIST)
    }
}

/// Module-level sentinel returned by
/// [`Session::<DPlus, Authenticated>::host_list`] when the core's
/// internal host list is (impossibly) `None`. Defined as a `static`
/// so callers can hold a `&'static HostList` without lifetime
/// gymnastics.
static EMPTY_HOST_LIST: HostList = HostList::new();

// ─── Connecting -> Connected (promote) ─────────────────────────

impl<P: Protocol> Session<P, Connecting> {
    /// Promote a session that has reached [`Connected`] state.
    ///
    /// The shell calls this after observing [`Event::Connected`] from
    /// the event stream. `promote` only succeeds if the core is
    /// already in [`ClientStateKind::Connected`]; any other state
    /// returns a [`Failed`] with a [`StateError::WrongState`] error.
    /// If the session was rejected mid-handshake the caller should
    /// inspect the failed session's [`Session::state_kind`] — it may
    /// already be in [`ClientStateKind::Closed`].
    ///
    /// # Errors
    ///
    /// Returns `Err(Failed)` if the session is not yet in
    /// [`ClientStateKind::Connected`]. The `session` field of the
    /// [`Failed`] carries the unchanged `Session<P, Connecting>`.
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn promote(self) -> Result<Session<P, Connected>, Failed<Self, Error>> {
        if self.inner.state_kind() == ClientStateKind::Connected {
            Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            })
        } else {
            let error = Error::State(StateError::WrongState {
                operation: "Session::promote",
                state: self.inner.state_kind(),
                protocol: self.inner.protocol_kind(),
            });
            Err(Failed {
                session: self,
                error,
            })
        }
    }
}

// ─── Connected -> Disconnecting + voice TX ─────────────────────

impl<P: Protocol> Session<P, Connected> {
    /// Initiate a graceful disconnect.
    ///
    /// Enqueues the UNLINK packet and transitions to
    /// [`Disconnecting`]. The caller must continue to drive the
    /// [`Driver`] loop (polling `poll_transmit` / `poll_event`) until
    /// either the UNLINK ACK arrives (emitting [`Event::Disconnected`]
    /// with [`super::DisconnectReason::UnlinkAcked`]) or the
    /// disconnect deadline expires (emitting [`Event::Disconnected`]
    /// with [`super::DisconnectReason::DisconnectTimeout`]).
    ///
    /// # Errors
    ///
    /// Returns the original session in the error position if the
    /// state machine refuses the transition (e.g., codec encoder
    /// failure).
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn disconnect(
        mut self,
        now: Instant,
    ) -> Result<Session<P, Disconnecting>, Failed<Self, Error>> {
        match self.inner.enqueue_disconnect(now) {
            Ok(()) => Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            }),
            Err(error) => Err(Failed {
                session: self,
                error,
            }),
        }
    }

    /// Enqueue an UNLINK packet without consuming the session.
    ///
    /// Used by the tokio shell's `SessionLoop` to trigger a disconnect
    /// from within the event loop, where consuming `self` isn't
    /// possible. After this returns, the internal state machine has
    /// transitioned to [`Disconnecting`], but the typestate parameter
    /// on this handle is still [`Connected`]. The caller should exit
    /// the event loop and construct a `Session<P, Disconnecting>` or
    /// wait for the internal state to reach [`Closed`].
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the core's [`SessionCore::enqueue_disconnect`]
    /// call fails (e.g., the codec encoder rejects the UNLINK packet).
    pub fn disconnect_in_place(&mut self, now: Instant) -> Result<(), Error> {
        self.inner.enqueue_disconnect(now)
    }

    /// Send a voice header and start a new outbound voice stream.
    ///
    /// The header is cached internally; subsequent
    /// [`Self::send_voice`] / [`Self::send_eot`] calls will reference
    /// it (DCS embeds the full header in every voice frame, so the
    /// cache is mandatory there).
    ///
    /// This method takes `&mut self`, NOT `self` — voice TX does not
    /// change the typestate. The session stays in [`Connected`] until
    /// `disconnect` or a timeout closes it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails.
    pub fn send_header(
        &mut self,
        now: Instant,
        header: &DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), Error> {
        self.inner.enqueue_send_header(now, header, stream_id)
    }

    /// Send a voice data frame.
    ///
    /// This method takes `&mut self`, NOT `self`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails.
    /// On DCS, returns [`Error::Protocol`] with
    /// [`crate::error::DcsError::NoTxHeader`] if [`Self::send_header`]
    /// has not been called first.
    pub fn send_voice(
        &mut self,
        now: Instant,
        stream_id: StreamId,
        seq: u8,
        frame: &VoiceFrame,
    ) -> Result<(), Error> {
        self.inner.enqueue_send_voice(now, stream_id, seq, frame)
    }

    /// Send a voice EOT packet, ending the outbound stream.
    ///
    /// This method takes `&mut self`, NOT `self`. The session stays
    /// in [`Connected`] after EOT — the caller may begin a new stream
    /// by calling [`Self::send_header`] again.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Protocol`] if the codec encoder fails.
    /// On DCS, returns [`Error::Protocol`] with
    /// [`crate::error::DcsError::NoTxHeader`] if [`Self::send_header`]
    /// has not been called first.
    pub fn send_eot(&mut self, now: Instant, stream_id: StreamId, seq: u8) -> Result<(), Error> {
        self.inner.enqueue_send_eot(now, stream_id, seq)
    }
}

// ─── Disconnecting -> Closed (promote) ──────────────────────────

impl<P: Protocol> Session<P, Disconnecting> {
    /// Promote to [`Closed`] once the UNLINK ACK arrives or the deadline fires.
    ///
    /// Same pattern as [`Session::<P, Connecting>::promote`]. The
    /// shell watches for [`Event::Disconnected`] and then calls this.
    ///
    /// # Errors
    ///
    /// Returns `Err(Failed)` if the session is not yet in
    /// [`ClientStateKind::Closed`] (the disconnect ACK hasn't arrived
    /// yet).
    #[expect(
        clippy::result_large_err,
        reason = "Failed<Self, Error> is large because Self wraps the full SessionCore; \
                  boxing would force every caller to unbox on success too"
    )]
    pub fn promote(self) -> Result<Session<P, Closed>, Failed<Self, Error>> {
        if self.inner.state_kind() == ClientStateKind::Closed {
            Ok(Session {
                inner: self.inner,
                _protocol: PhantomData,
                _state: PhantomData,
            })
        } else {
            let error = Error::State(StateError::WrongState {
                operation: "Session::promote",
                state: self.inner.state_kind(),
                protocol: self.inner.protocol_kind(),
            });
            Err(Failed {
                session: self,
                error,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::dextra::encode_connect_ack;
    use crate::session::client::protocol::DExtra;
    use crate::types::{Module, ProtocolKind};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const fn cs(bytes: [u8; 8]) -> Callsign {
        Callsign::from_wire_bytes(bytes)
    }

    fn new_dextra_configured() -> Session<DExtra, Configured> {
        let core = SessionCore::new(
            ProtocolKind::DExtra,
            cs(*b"W1AW    "),
            Module::B,
            Module::C,
            ADDR,
        );
        Session {
            inner: core,
            _protocol: PhantomData,
            _state: PhantomData,
        }
    }

    #[test]
    fn dextra_configured_state_kind() {
        let session = new_dextra_configured();
        assert_eq!(session.state_kind(), ClientStateKind::Configured);
    }

    #[test]
    fn dextra_connect_transitions_to_connecting() -> TestResult {
        let session = new_dextra_configured();
        let now = Instant::now();
        let connecting = session.connect(now)?;
        assert_eq!(connecting.state_kind(), ClientStateKind::Connecting);
        Ok(())
    }

    #[test]
    fn dextra_full_connect_cycle() -> TestResult {
        let now = Instant::now();
        let session = new_dextra_configured();
        let mut connecting = session.connect(now)?;
        assert!(
            connecting.poll_transmit(now).is_some(),
            "LINK transmit ready"
        );

        let mut ack_buf = [0u8; 16];
        let n = encode_connect_ack(&mut ack_buf, &cs(*b"W1AW    "), Module::C)?;
        connecting.handle_input(now, ADDR, ack_buf.get(..n).ok_or("slice")?)?;

        assert_eq!(connecting.state_kind(), ClientStateKind::Connected);
        let connected = connecting.promote()?;
        assert_eq!(connected.state_kind(), ClientStateKind::Connected);
        Ok(())
    }

    #[test]
    fn dextra_promote_fails_if_still_connecting() -> TestResult {
        let now = Instant::now();
        let session = new_dextra_configured();
        let connecting = session.connect(now)?;
        let Err(err) = connecting.promote() else {
            return Err("expected promote to fail".into());
        };
        assert_eq!(err.session.state_kind(), ClientStateKind::Connecting);
        Ok(())
    }

    #[test]
    fn dextra_connected_disconnect_transitions_to_disconnecting() -> TestResult {
        let now = Instant::now();
        let session = new_dextra_configured();
        let mut connecting = session.connect(now)?;
        let mut ack_buf = [0u8; 16];
        let n = encode_connect_ack(&mut ack_buf, &cs(*b"W1AW    "), Module::C)?;
        connecting.handle_input(now, ADDR, ack_buf.get(..n).ok_or("slice")?)?;
        let connected = connecting.promote()?;
        let disconnecting = connected.disconnect(now + Duration::from_secs(1))?;
        assert_eq!(disconnecting.state_kind(), ClientStateKind::Disconnecting);
        Ok(())
    }

    #[test]
    fn peer_accessor_works_in_any_state() {
        let session = new_dextra_configured();
        assert_eq!(session.peer(), ADDR);
    }

    #[test]
    fn local_callsign_accessor_works_in_any_state() {
        let session = new_dextra_configured();
        assert_eq!(session.local_callsign(), cs(*b"W1AW    "));
    }

    #[test]
    fn diagnostics_drain_starts_empty() {
        let mut session = new_dextra_configured();
        assert!(session.diagnostics().is_empty());
    }

    // ─── Voice TX on Session<P, Connected> ─────────────────────

    use crate::types::Suffix;

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

    #[expect(clippy::unwrap_used, reason = "const-validated: n is non-zero")]
    const fn sid(n: u16) -> StreamId {
        StreamId::new(n).unwrap()
    }

    fn dextra_connected() -> Result<Session<DExtra, Connected>, Box<dyn std::error::Error>> {
        let now = Instant::now();
        let session = new_dextra_configured();
        let mut connecting = session.connect(now)?;
        let mut ack_buf = [0u8; 16];
        let n = encode_connect_ack(&mut ack_buf, &cs(*b"W1AW    "), Module::C)?;
        connecting.handle_input(now, ADDR, ack_buf.get(..n).ok_or("slice")?)?;
        Ok(connecting.promote()?)
    }

    #[test]
    fn dextra_connected_send_header_succeeds() -> TestResult {
        let mut session = dextra_connected()?;
        let now = Instant::now();
        session.send_header(now, &test_header(), sid(0x1234))?;
        let _link = session.poll_transmit(now).ok_or("link tx")?;
        let header_tx = session.poll_transmit(now).ok_or("voice header tx")?;
        assert_eq!(header_tx.payload.len(), 56);
        Ok(())
    }

    #[test]
    fn dextra_connected_send_voice_succeeds() -> TestResult {
        let mut session = dextra_connected()?;
        let now = Instant::now();
        let frame = VoiceFrame::silence();
        session.send_voice(now, sid(0x1234), 5, &frame)?;
        let _link = session.poll_transmit(now).ok_or("link tx")?;
        let voice_tx = session.poll_transmit(now).ok_or("voice tx")?;
        assert_eq!(voice_tx.payload.len(), 27);
        Ok(())
    }

    #[test]
    fn dextra_connected_send_eot_succeeds() -> TestResult {
        let mut session = dextra_connected()?;
        let now = Instant::now();
        session.send_eot(now, sid(0x1234), 21)?;
        let _link = session.poll_transmit(now).ok_or("link tx")?;
        let eot_tx = session.poll_transmit(now).ok_or("eot tx")?;
        assert_eq!(eot_tx.payload.len(), 27);
        Ok(())
    }

    #[test]
    fn dextra_connected_send_header_does_not_change_state() -> TestResult {
        let mut session = dextra_connected()?;
        let now = Instant::now();
        session.send_header(now, &test_header(), sid(0x1234))?;
        assert_eq!(session.state_kind(), ClientStateKind::Connected);
        Ok(())
    }

    #[test]
    fn dextra_connected_disconnect_in_place_transitions_internal_state() -> TestResult {
        let mut session = dextra_connected()?;
        assert_eq!(session.state_kind(), ClientStateKind::Connected);
        session.disconnect_in_place(Instant::now())?;
        assert_eq!(session.state_kind(), ClientStateKind::Disconnecting);
        Ok(())
    }
}
