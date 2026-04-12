//! User-facing handle for an async session running in a spawned task.

use std::marker::PhantomData;
use std::sync::Arc;

use dstar_gateway_core::header::DStarHeader;
use dstar_gateway_core::session::client::{Connected, Event, Protocol, Session};
use dstar_gateway_core::types::StreamId;
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use super::{Command, SessionLoop, ShellError};

/// Capacity of the command channel between handle and session task.
///
/// Voice commands are small (header/voice/eot) and arrive at a
/// modest rate (≈50 frames/s max). 32 provides headroom for bursts
/// without unbounded memory use. If the consumer is running
/// behind, `send_voice` awaits backpressure rather than blocking
/// unboundedly.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the event channel from session task to handle.
///
/// Events are produced by the loop and consumed by the user via
/// `next_event`. A deeper buffer here lets the loop keep running
/// while the consumer is processing the previous batch. 256 frames
/// is enough to cover a full 5-second stream of voice data (rough
/// upper bound of ≈100 frames) plus some headroom.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Async handle to a session running in a spawned tokio task.
///
/// Methods translate to commands sent over an internal channel and
/// reply over a oneshot. Dropping the handle severs the connection
/// from the consumer side; the spawned task exits on its next loop.
///
/// **Drop is not graceful** — for graceful shutdown call
/// [`AsyncSession::disconnect`]. Drop just severs the connection from
/// the consumer's side; the reflector eventually times the link out
/// via inactivity.
#[derive(Debug)]
pub struct AsyncSession<P: Protocol> {
    pub(crate) command_tx: mpsc::Sender<Command>,
    pub(crate) event_rx: mpsc::Receiver<Event<P>>,
    pub(crate) _protocol: PhantomData<P>,
}

impl<P: Protocol> AsyncSession<P> {
    /// Spawn the session loop on the current tokio runtime and
    /// return a handle for controlling it.
    ///
    /// The `session` must already be in the [`Connected`] state
    /// (typically via `Session::<P, Connecting>::promote` after
    /// observing [`Event::Connected`] from the handshake). The
    /// `socket` must be bound (typically via `UdpSocket::bind`).
    ///
    /// The loop runs until the handle is dropped, the consumer's
    /// command channel closes, or a fatal I/O error occurs.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use dstar_gateway::tokio_shell::AsyncSession;
    /// use dstar_gateway_core::session::client::{Connected, DExtra, Session};
    /// use tokio::net::UdpSocket;
    ///
    /// # async fn demo(connected: Session<DExtra, Connected>) -> Result<(), Box<dyn std::error::Error>> {
    /// let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    /// let mut shell = AsyncSession::spawn(connected, sock);
    /// while let Some(event) = shell.next_event().await {
    ///     println!("{event:?}");
    /// }
    /// # Ok(()) }
    /// ```
    #[must_use]
    pub fn spawn(session: Session<P, Connected>, socket: Arc<UdpSocket>) -> Self
    where
        P: Send + 'static,
    {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

        let inner_loop = SessionLoop {
            session,
            socket,
            event_tx,
            command_rx,
        };

        drop(tokio::spawn(async move {
            // Loop errors bubble up as `Err`; the consumer sees
            // `SessionClosed` via the event channel closing when the
            // task exits.
            drop(inner_loop.run().await);
        }));

        Self {
            command_tx,
            event_rx,
            _protocol: PhantomData,
        }
    }

    /// Pull the next event from the inbound stream.
    ///
    /// Returns `None` once the session task has exited and the event
    /// channel has been fully drained.
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. It only awaits a `tokio::sync::mpsc`
    /// receiver, which is documented as cancel-safe: dropping the future
    /// leaves the channel in a clean state and any undelivered events
    /// remain queued for the next call.
    pub async fn next_event(&mut self) -> Option<Event<P>> {
        self.event_rx.recv().await
    }

    /// Send a voice header and start a new outbound voice stream.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the encoder rejects the header
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe. The method enqueues a [`Command`]
    /// on the command channel and awaits a oneshot reply. If the future
    /// is dropped before the enqueue completes no command is sent; if
    /// it is dropped after the enqueue the session task still executes
    /// the command and the (now-orphaned) oneshot reply is simply
    /// discarded. Either way the session state remains consistent.
    pub async fn send_header(
        &mut self,
        header: DStarHeader,
        stream_id: StreamId,
    ) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendHeader {
                header: Box::new(header),
                stream_id,
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Send a voice data frame.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the encoder rejects the frame
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe under the same rules as
    /// [`Self::send_header`]. Dropping the future either before the
    /// command is enqueued or after it has been dispatched leaves the
    /// session in a coherent state; orphaning the oneshot reply is
    /// harmless.
    pub async fn send_voice(
        &mut self,
        stream_id: StreamId,
        seq: u8,
        frame: VoiceFrame,
    ) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendVoice {
                stream_id,
                seq,
                frame: Box::new(frame),
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Send a voice EOT and close the outbound stream.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    /// - [`ShellError::Core`] if the encoder rejects the EOT
    ///
    /// # Cancellation safety
    ///
    /// This method is cancel-safe under the same rules as
    /// [`Self::send_header`].
    pub async fn send_eot(&mut self, stream_id: StreamId, seq: u8) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::SendEot {
                stream_id,
                seq,
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Request a graceful disconnect.
    ///
    /// Sends an UNLINK to the reflector and returns when the loop
    /// has enqueued it. The caller should continue polling
    /// [`Self::next_event`] until [`Event::Disconnected`] arrives,
    /// then drop the session.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the session task has exited
    ///
    /// # Cancellation safety
    ///
    /// This method is **not** cancel-safe. Cancelling the future drives
    /// a state-machine transition (`Connected` → `Disconnecting`) that
    /// may be partially complete: the UNLINK may already be in the
    /// outbox even though the reply oneshot has been dropped. Callers
    /// that cancel `disconnect()` should treat the session as
    /// indeterminate and drop the handle rather than attempting further
    /// sends. For graceful shutdown, always `await` this method to
    /// completion before dropping the session.
    pub async fn disconnect(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_tx
            .send(Command::Disconnect { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?;
        Ok(())
    }
}

impl<P: Protocol> Drop for AsyncSession<P> {
    fn drop(&mut self) {
        // Dropping command_tx closes the channel, which signals
        // the session task to exit on its next loop iteration.
        // No explicit shutdown needed.
    }
}
