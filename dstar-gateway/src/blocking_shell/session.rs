//! Synchronous client driving with bounded idle reads and explicit shutdown.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use dstar_gateway_core::error::{Error as CoreError, IoOperation};
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{
    ClientStateKind, Connected, DisconnectReason, Event, Protocol, Session,
};
use dstar_gateway_core::{DstarHeader, FrameSequence, StreamId, VoiceFrame};

use crate::tokio_shell::ShellError;

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

const IDLE_WAIT: Duration = Duration::from_millis(100);
const MIN_WAIT: Duration = Duration::from_millis(1);
const SEND_WAIT: Duration = Duration::from_secs(2);
const DISCONNECT_WAIT: Duration = Duration::from_secs(5);

/// A connected client and exclusively owned synchronous UDP socket.
///
/// Calls drive I/O on the calling thread. Existing events are returned before
/// waiting; otherwise receive waits total at most 100 ms, subject to OS
/// scheduling and timeout resolution. Socket send calls have a two-second
/// timeout. Use [`Self::is_closed`] to distinguish closure from an idle None.
///
/// ```no_run
/// # use std::net::UdpSocket;
/// # use dstar_gateway::blocking_shell::BlockingSession;
/// # use dstar_gateway_core::{Connected, DExtra, Event, Session};
/// # fn run(session: Session<DExtra, Connected>, socket: UdpSocket)
/// # -> Result<(), Box<dyn std::error::Error>> {
/// let mut client = BlockingSession::new(session, socket)?;
/// while !client.is_closed() {
///     if let Some(event) = client.run_until_event()? {
///         println!("{event:?}");
///     }
/// }
/// let (_session, _socket) = client.into_parts();
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub struct BlockingSession<P> {
    session: Session<P, Connected>,
    socket: UdpSocket,
    disconnect_reason: Option<DisconnectReason>,
}

impl<P: Protocol> BlockingSession<P> {
    /// Take ownership of an already connected session and its bound socket.
    ///
    /// Sets blocking mode and the local write timeout.
    ///
    /// # Errors
    ///
    /// Returns a typed socket error if either setting fails.
    pub fn new(session: Session<P, Connected>, socket: UdpSocket) -> Result<Self, ShellError> {
        socket.set_nonblocking(false).map_err(receive_error)?;
        socket
            .set_write_timeout(Some(SEND_WAIT))
            .map_err(send_error)?;
        Ok(Self {
            session,
            socket,
            disconnect_reason: None,
        })
    }

    /// Whether the runtime state has reached its terminal closed state.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.session.state_kind() == ClientStateKind::Closed
    }

    /// Return both owned resources, preserving the current runtime state.
    ///
    /// Call [`Self::disconnect`] first when a graceful unlink is required.
    #[must_use]
    pub fn into_parts(self) -> (Session<P, Connected>, UdpSocket) {
        (self.session, self.socket)
    }

    /// Send a header; success means its ready datagrams reached the local socket.
    ///
    /// # Errors
    ///
    /// Returns the typed core state/codec or UDP/timeout configuration error.
    pub fn send_header(
        &mut self,
        header: &DstarHeader,
        stream: StreamId,
    ) -> Result<(), ShellError> {
        self.session.send_header(Instant::now(), header, stream)?;
        self.flush_transmit()
    }

    /// Send one checked frame index and voice frame.
    ///
    /// # Errors
    ///
    /// Returns the typed core state/codec or UDP error. Partial prior sends may
    /// already have reached the peer.
    pub fn send_voice(
        &mut self,
        stream: StreamId,
        sequence: FrameSequence,
        frame: &VoiceFrame,
    ) -> Result<(), ShellError> {
        self.session
            .send_voice(Instant::now(), stream, sequence, frame)?;
        self.flush_transmit()
    }

    /// Send an EOT for the outbound stream, with the termination bit encoded.
    ///
    /// # Errors
    ///
    /// Returns the typed core state/codec or UDP error.
    pub fn send_eot(
        &mut self,
        stream: StreamId,
        sequence: FrameSequence,
    ) -> Result<(), ShellError> {
        self.session.send_eot(Instant::now(), stream, sequence)?;
        self.flush_transmit()
    }

    /// Drive until one event is available, closure, or a 100-ms idle window.
    ///
    /// None means no event is currently available; inspect [`Self::is_closed`]
    /// and continue polling while linked. Due timers run before receive,
    /// including under continuous foreign or malformed traffic.
    ///
    /// # Errors
    ///
    /// Returns typed core or socket errors. The socket's read/write timeouts are
    /// adjusted by this driver and remain on the socket returned by `into_parts`.
    pub fn run_until_event(&mut self) -> Result<Option<Event<P>>, ShellError> {
        let cutoff = Instant::now()
            .checked_add(IDLE_WAIT)
            .ok_or(ShellError::DisconnectStalled)?;
        let mut buffer = [0_u8; 2048];
        loop {
            if let Some(event) = self.session.poll_event() {
                if let Event::Disconnected { reason } = &event {
                    self.disconnect_reason = Some(*reason);
                }
                return Ok(Some(event));
            }
            let now = Instant::now();
            self.session.handle_timeout(now);
            self.flush_transmit()?;
            if let Some(event) = self.session.poll_event() {
                if let Event::Disconnected { reason } = &event {
                    self.disconnect_reason = Some(*reason);
                }
                return Ok(Some(event));
            }
            if self.is_closed() || now >= cutoff {
                return Ok(None);
            }
            let wake = self
                .session
                .poll_timeout()
                .map_or(cutoff, |wake| wake.min(cutoff));
            let wait = wake.saturating_duration_since(now).max(MIN_WAIT);
            self.socket
                .set_read_timeout(Some(wait))
                .map_err(receive_error)?;
            match self.socket.recv_from(&mut buffer) {
                Ok((length, source)) => {
                    self.session.handle_input(
                        Instant::now(),
                        source,
                        buffer.get(..length).unwrap_or(&[]),
                    )?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(receive_error(error)),
            }
        }
    }

    /// Unlink, drive terminal events and return the acknowledgement outcome.
    ///
    /// The core permits two seconds for the peer, inside a five-second local
    /// driving deadline. Queued receive events are consumed during shutdown.
    ///
    /// # Errors
    ///
    /// Returns typed I/O/core failures, `DisconnectStalled` for the local deadline,
    /// `DisconnectUnacknowledged` for protocol timeout, or
    /// `DisconnectedBeforeUnlink` for another terminal cause.
    pub fn disconnect(&mut self) -> Result<(), ShellError> {
        if let Some(reason) = self.disconnect_reason {
            return disconnect_result(reason);
        }
        if self.session.state_kind() != ClientStateKind::Disconnecting {
            self.session.disconnect_in_place(Instant::now())?;
        }
        let cutoff = Instant::now()
            .checked_add(DISCONNECT_WAIT)
            .ok_or(ShellError::DisconnectStalled)?;
        loop {
            if Instant::now() >= cutoff {
                return Err(ShellError::DisconnectStalled);
            }
            let _event = self.run_until_event()?;
            if let Some(reason) = self.disconnect_reason {
                return disconnect_result(reason);
            }
        }
    }

    fn flush_transmit(&mut self) -> Result<(), ShellError> {
        while let Some(transmit) = self.session.poll_transmit(Instant::now()) {
            let _sent = self
                .socket
                .send_to(transmit.payload, transmit.dst)
                .map_err(send_error)?;
        }
        Ok(())
    }
}

fn receive_error(source: std::io::Error) -> ShellError {
    CoreError::Io {
        source,
        operation: IoOperation::UdpRecv,
    }
    .into()
}

fn send_error(source: std::io::Error) -> ShellError {
    CoreError::Io {
        source,
        operation: IoOperation::UdpSend,
    }
    .into()
}

const fn disconnect_result(reason: DisconnectReason) -> Result<(), ShellError> {
    match reason {
        DisconnectReason::UnlinkAcked => Ok(()),
        DisconnectReason::DisconnectTimeout => Err(ShellError::DisconnectUnacknowledged),
        reason => Err(ShellError::DisconnectedBeforeUnlink { reason }),
    }
}
