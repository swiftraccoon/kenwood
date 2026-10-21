//! Shared synchronous connect handshake with one total deadline.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use dstar_gateway_core::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Connected, Connecting, Protocol, Session,
};

use crate::tokio_shell::ConnectError;

/// Drive a connecting client to connected using a synchronous UDP socket.
///
/// Sets blocking mode and socket timeouts inside one total deadline. Ready
/// timers run before each receive; foreign source addresses are ignored by
/// the typed core driver. Socket settings remain changed after return.
///
/// # Errors
///
/// Returns `DeadlineOverflow` before socket changes for an unrepresentable
/// duration, `TimedOut` at the total deadline, `Rejected` on core closure, or
/// a source-preserving I/O/protocol/promotion error.
pub fn drive_connecting<P: Protocol>(
    mut session: Session<P, Connecting>,
    socket: &UdpSocket,
    timeout: Duration,
) -> Result<Session<P, Connected>, ConnectError> {
    let cutoff = Instant::now()
        .checked_add(timeout)
        .ok_or(ConnectError::DeadlineOverflow(timeout))?;
    socket.set_nonblocking(false)?;
    let mut buffer = [0_u8; 2048];
    loop {
        let now = Instant::now();
        session.handle_timeout(now);
        match session.state_kind() {
            ClientStateKind::Connected => {
                return session
                    .promote()
                    .map_err(|failed| ConnectError::Promote(failed.error));
            }
            ClientStateKind::Closed => return Err(ConnectError::Rejected),
            _ => {}
        }
        let remaining = cutoff
            .checked_duration_since(now)
            .filter(|span| !span.is_zero())
            .ok_or(ConnectError::TimedOut(timeout))?;
        socket.set_write_timeout(Some(remaining))?;
        while let Some(transmit) = session.poll_transmit(Instant::now()) {
            let remaining = cutoff
                .checked_duration_since(Instant::now())
                .filter(|span| !span.is_zero())
                .ok_or(ConnectError::TimedOut(timeout))?;
            socket.set_write_timeout(Some(remaining))?;
            let _sent = socket.send_to(transmit.payload, transmit.dst)?;
        }
        let wake = session
            .poll_timeout()
            .map_or(cutoff, |wake| wake.min(cutoff));
        let wait = wake
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(1));
        socket.set_read_timeout(Some(wait))?;
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => session
                .handle_input(Instant::now(), source, buffer.get(..length).unwrap_or(&[]))
                .map_err(ConnectError::Protocol)?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
}
