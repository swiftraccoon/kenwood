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

    /// The requested total deadline exceeds the platform's Instant range.
    #[error("handshake deadline {0:?} cannot be represented")]
    DeadlineOverflow(Duration),

    /// Socket I/O failed during the handshake.
    #[error("handshake I/O failed")]
    Io(#[from] std::io::Error),

    /// An inbound datagram could not be decoded.
    #[error("handshake decode failed: {0}")]
    Protocol(#[source] dstar_gateway_core::error::Error),

    /// The state machine reported `Connected` but promotion failed.
    #[error("promotion to Connected failed: {0}")]
    Promote(#[source] dstar_gateway_core::error::Error),
}

/// Drive a `Connecting` session to `Connected` over `socket`.
///
/// `deadline` bounds the whole handshake. On success the promoted session
/// is ready for [`AsyncSession::spawn`](crate::tokio_shell::AsyncSession).
///
/// # Errors
///
/// [`ConnectError::Rejected`] when the state machine closes,
/// [`ConnectError::TimedOut`] at the total deadline,
/// [`ConnectError::DeadlineOverflow`] before I/O for an unrepresentable span,
/// and typed I/O, protocol or promotion failures otherwise.
///
/// # Cancellation
///
/// The function owns the session; cancellation drops it. Datagrams already
/// sent may have reached the peer, and the caller retains the socket.
pub async fn drive_connecting<P: Protocol>(
    mut session: Session<P, Connecting>,
    socket: &UdpSocket,
    deadline: Duration,
) -> Result<Session<P, Connected>, ConnectError> {
    let cutoff = tokio::time::Instant::now()
        .checked_add(deadline)
        .ok_or(ConnectError::DeadlineOverflow(deadline))?;
    let mut buf = [0_u8; 2048];

    loop {
        let now = Instant::now();
        session.handle_timeout(now);
        match session.state_kind() {
            ClientStateKind::Connected => break,
            ClientStateKind::Closed => return Err(ConnectError::Rejected),
            _ => {}
        }

        if tokio::time::Instant::now() >= cutoff {
            return Err(ConnectError::TimedOut(deadline));
        }

        // Drain any outbound packets the core wants on the wire.
        while let Some(tx) = session.poll_transmit(Instant::now()) {
            let _sent = tokio::time::timeout_at(cutoff, socket.send_to(tx.payload, tx.dst))
                .await
                .map_err(|_| ConnectError::TimedOut(deadline))??;
        }

        let wake = session.poll_timeout().map_or(cutoff, |wake| {
            tokio::time::Instant::from_std(wake).min(cutoff)
        });
        match tokio::time::timeout_at(wake, socket.recv_from(&mut buf)).await {
            Ok(Ok((n, src))) => {
                let Some(bytes) = buf.get(..n) else {
                    continue;
                };
                session
                    .handle_input(Instant::now(), src, bytes)
                    .map_err(ConnectError::Protocol)?;
            }
            Ok(Err(error)) => return Err(ConnectError::Io(error)),
            Err(_elapsed) => {}
        }
    }

    session
        .promote()
        .map_err(|failed| ConnectError::Promote(failed.error))
}
