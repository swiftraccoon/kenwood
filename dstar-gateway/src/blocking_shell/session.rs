//! Synchronous session wrapper over `Session<P, Connected>`.
//!
//! The blocking shell is a one-for-one mirror of
//! [`crate::tokio_shell`] but driven by a plain `std::net::UdpSocket`
//! with read timeouts instead of tokio channels. The caller controls
//! the loop via [`BlockingSession::run_until_event`] — no spawned
//! thread, no background task, no tokio runtime.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use dstar_gateway_core::error::{Error as CoreError, IoOperation};
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{Connected, Event, Protocol, Session};

use crate::tokio_shell::ShellError;

/// Fallback wait window when the session has no pending timer.
///
/// With no deadline in sight the session is idle; we still want to
/// yield to the OS periodically so the caller's loop doesn't spin.
const IDLE_WAIT: Duration = Duration::from_millis(100);

/// Minimum read timeout clamp.
///
/// `UdpSocket::set_read_timeout` rejects a zero `Duration`, so when
/// the next deadline has already elapsed we still set a 1 ms
/// timeout to keep the call legal.
const MIN_WAIT: Duration = Duration::from_millis(1);

/// Synchronous wrapper over a `Session<P, Connected>` + `UdpSocket`.
///
/// Drives the sans-io driver loop one step at a time via
/// [`Self::run_until_event`]. The caller controls the iteration —
/// no spawned task, no channel plumbing.
///
/// # Example
///
/// ```no_run
/// # use std::net::UdpSocket;
/// # use dstar_gateway::blocking_shell::BlockingSession;
/// # use dstar_gateway_core::session::client::{Connected, DExtra, Session};
/// # fn demo(session: Session<DExtra, Connected>, sock: UdpSocket) -> Result<(), Box<dyn std::error::Error>> {
/// let mut shell = BlockingSession::new(session, sock);
/// while let Some(event) = shell.run_until_event()? {
///     // handle event
///     let _ = event;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct BlockingSession<P: Protocol> {
    session: Session<P, Connected>,
    socket: UdpSocket,
}

impl<P: Protocol> BlockingSession<P> {
    /// Wrap a pre-connected session and its bound socket.
    ///
    /// The caller is responsible for driving the typestate transition
    /// from `Configured` → `Connecting` → `Connected` before handing
    /// the session off to the blocking shell. Once `Connected`, the
    /// blocking shell owns the socket exclusively.
    #[must_use]
    pub const fn new(session: Session<P, Connected>, socket: UdpSocket) -> Self {
        Self { session, socket }
    }

    /// Drive the driver loop until an event is available or the
    /// short idle window elapses. Returns `None` if no event
    /// arrived within the window — the caller should then call
    /// again to continue driving the loop.
    ///
    /// The driver loop performs four steps per call:
    ///
    /// 1. Drain outbound datagrams via `session.poll_transmit`.
    /// 2. Compute the next wake deadline via `session.poll_timeout`
    ///    and arm `UdpSocket::set_read_timeout` accordingly.
    /// 3. Attempt a `recv_from` — on success feed the bytes into
    ///    `session.handle_input`, on timeout feed `session.handle_timeout`.
    /// 4. Drain one event via `session.poll_event`.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::Core`] on UDP send/recv failure, read
    /// timeout arming failure, or a protocol-level error surfaced
    /// by the core.
    pub fn run_until_event(&mut self) -> Result<Option<Event<P>>, ShellError> {
        // 1. Drain outbound to the socket.
        while let Some(tx) = self.session.poll_transmit(Instant::now()) {
            let _bytes_sent = self.socket.send_to(tx.payload, tx.dst).map_err(|e| {
                ShellError::Core(CoreError::Io {
                    source: e,
                    operation: IoOperation::UdpSend,
                })
            })?;
        }

        // 2. Set read timeout to the next poll_timeout (or IDLE_WAIT default).
        let next_wake = self.session.poll_timeout();
        let now = Instant::now();
        let wait = next_wake.map_or(IDLE_WAIT, |d| {
            d.checked_duration_since(now)
                .unwrap_or(MIN_WAIT)
                .max(MIN_WAIT)
        });
        self.socket.set_read_timeout(Some(wait)).map_err(|e| {
            ShellError::Core(CoreError::Io {
                source: e,
                operation: IoOperation::UdpRecv,
            })
        })?;

        // 3. Attempt a read.
        let mut buf = [0u8; 2048];
        match self.socket.recv_from(&mut buf) {
            Ok((n, src)) => {
                let slice = buf.get(..n).unwrap_or(&[]);
                self.session
                    .handle_input(Instant::now(), src, slice)
                    .map_err(ShellError::Core)?;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Timeout — let the session know time has advanced.
                self.session.handle_timeout(Instant::now());
            }
            Err(e) => {
                return Err(ShellError::Core(CoreError::Io {
                    source: e,
                    operation: IoOperation::UdpRecv,
                }));
            }
        }

        // 4. Drain an event (if any).
        Ok(self.session.poll_event())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dstar_gateway_core::session::client::DExtra;

    // The blocking shell is gated by the `Session<P, Connected>`
    // typestate, so the type system already proves most invariants
    // at compile time. Behavioral tests would need a synchronous
    // `FakeReflector` — the loopback suites under `tests/` cover the
    // async path and the core codecs are separately tested inside
    // `dstar-gateway-core`.
    //
    // These tests just verify the module surface compiles and the
    // types implement the expected traits.

    /// Compile-time check that `BlockingSession<P>` implements `Debug`.
    const fn require_debug<T: core::fmt::Debug>() {}

    #[test]
    fn blocking_session_is_debug_for_dextra() {
        require_debug::<BlockingSession<DExtra>>();
    }

    #[test]
    fn blocking_session_constructor_signature() {
        // Proves `BlockingSession::new` is callable with the expected
        // argument types. The actual guarantees come from
        // `Session<P, Connected>` being a typestate-gated, well-formed
        // type — if the typestate ever regresses, this test stops
        // compiling.
        let ctor: fn(Session<DExtra, Connected>, UdpSocket) -> BlockingSession<DExtra> =
            BlockingSession::<DExtra>::new;
        // Binding prevents the call from being dropped silently — the
        // `unused_results = "deny"` lint still sees the `fn` item as
        // used because we assigned it into a typed slot.
        let _: fn(Session<DExtra, Connected>, UdpSocket) -> BlockingSession<DExtra> = ctor;
    }
}
