//! Drive a sans-io client session's connect handshake over a real socket.
//!
//! Every reflector client repeats the same pump: drain `poll_transmit` to
//! the UDP socket, await an inbound datagram (or a short timer tick so the
//! core can fire keepalives and retransmits), feed `handle_input`, and
//! promote the typestate as soon as the state machine reports `Connected`.
//! [`drive_connecting`] owns that loop with typed failures; callers keep
//! only their protocol-specific preludes (`DPlus` authentication, session
//! configuration).

use std::time::{Duration, Instant};

use dstar_gateway_core::session::Driver as _;
use dstar_gateway_core::session::client::{
    ClientStateKind, Connected, Connecting, Protocol, Session,
};
use tokio::net::UdpSocket;

/// Receive-poll granularity: how often the core gets a timer tick while no
/// datagram arrives, so keepalives and retransmits still fire.
const POLL_TICK: Duration = Duration::from_millis(100);

/// Why a reflector connect handshake did not reach `Connected`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The state machine closed the link: the reflector rejected it.
    #[error("the reflector rejected the connection")]
    Rejected,

    /// No acknowledgement arrived within the caller's deadline.
    #[error("timed out after {0:?} waiting for reflector acknowledgement")]
    TimedOut(Duration),

    /// Socket I/O failed during the handshake.
    #[error("handshake I/O failed")]
    Io(#[from] std::io::Error),

    /// An inbound datagram could not be decoded.
    #[error("handshake decode failed: {0}")]
    Protocol(dstar_gateway_core::error::Error),

    /// The state machine reported `Connected` but promotion failed.
    #[error("promotion to Connected failed: {0}")]
    Promote(dstar_gateway_core::error::Error),
}

/// Drive a `Connecting` session to `Connected` over `socket`.
///
/// `deadline` bounds the whole handshake. On success the promoted session
/// is ready for [`AsyncSession::spawn`](crate::tokio_shell::AsyncSession).
///
/// # Errors
///
/// [`ConnectError::Rejected`] when the state machine closes,
/// [`ConnectError::TimedOut`] at the deadline, and the I/O, decode, or
/// promotion failures otherwise.
pub async fn drive_connecting<P: Protocol>(
    mut session: Session<P, Connecting>,
    socket: &UdpSocket,
    deadline: Duration,
) -> Result<Session<P, Connected>, ConnectError> {
    let cutoff = Instant::now() + deadline;
    let mut buf = [0_u8; 2048];

    loop {
        match session.state_kind() {
            ClientStateKind::Connected => break,
            ClientStateKind::Closed => return Err(ConnectError::Rejected),
            _ => {}
        }

        if Instant::now() >= cutoff {
            return Err(ConnectError::TimedOut(deadline));
        }

        // Drain any outbound packets the core wants on the wire.
        while let Some(tx) = session.poll_transmit(Instant::now()) {
            let _sent = socket.send_to(tx.payload, tx.dst).await?;
        }

        // Wait for an inbound datagram, or give the core a timer tick.
        match tokio::time::timeout(POLL_TICK, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, src))) => {
                let Some(bytes) = buf.get(..n) else {
                    continue;
                };
                session
                    .handle_input(Instant::now(), src, bytes)
                    .map_err(ConnectError::Protocol)?;
            }
            Ok(Err(error)) => return Err(ConnectError::Io(error)),
            Err(_elapsed) => session.handle_timeout(Instant::now()),
        }
    }

    session
        .promote()
        .map_err(|failed| ConnectError::Promote(failed.error))
}
