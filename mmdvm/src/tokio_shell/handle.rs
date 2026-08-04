//! User-facing handle for an async MMDVM modem running in a spawned
//! task.

use mmdvm_core::ModemMode;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::ShellError;
use crate::transport::Transport;

use super::{Command, Event, ModemLoop};

/// Capacity of the command channel between handle and modem task.
///
/// Commands are small (frame enqueues, mode changes). 32 provides
/// headroom for bursts without unbounded memory use. If the loop is
/// running behind, `send_*` awaits backpressure rather than blocking
/// unboundedly.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the event ring from modem task to handle.
///
/// Events cover both periodic status pushes and inbound radio frames.
/// 256 covers a full D-STAR transmission (~100 voice frames) plus
/// status polls at 4 Hz with generous headroom.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// How long [`AsyncModem::set_mode`] waits for the modem's ACK/NAK
/// before failing with [`ShellError::ResponseTimeout`].
///
/// The firmware acknowledges `SetMode` immediately on receipt; 2 s is
/// generous even over Bluetooth SPP.
const SET_MODE_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Async handle to an MMDVM modem running in a spawned tokio task.
///
/// The handle is generic over the transport type `T` so that
/// [`AsyncModem::shutdown`] can recover the original transport for
/// reuse (e.g. to send post-MMDVM CAT commands on the same serial
/// port).
///
/// Dropping the handle closes the command channel, which causes the
/// spawned loop to exit on its next iteration. For a graceful
/// shutdown that also flushes the pending TX queue AND recovers the
/// inner transport, call [`AsyncModem::shutdown`].
#[derive(Debug)]
pub struct AsyncModem<T: Transport + 'static> {
    command_tx: mpsc::Sender<Command>,
    event_rx: broadcast::Receiver<Event>,
    join_handle: Option<JoinHandle<Result<T, ShellError>>>,
}

impl<T: Transport + 'static> AsyncModem<T> {
    /// Spawn the modem loop on the current tokio runtime and return
    /// a handle for controlling it.
    ///
    /// The `transport` must be an already-connected duplex byte
    /// stream (serial port, Bluetooth SPP, test duplex). The shell
    /// takes ownership; it is automatically dropped when the loop
    /// exits.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use mmdvm::AsyncModem;
    /// use tokio::io::duplex;
    ///
    /// # async fn demo() {
    /// let (client, _modem_side) = duplex(4096);
    /// let mut modem = AsyncModem::spawn(client);
    /// while let Some(event) = modem.next_event().await {
    ///     println!("{event:?}");
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn spawn(transport: T) -> Self {
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let (event_tx, event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        let loop_state = ModemLoop::new(transport, command_rx, event_tx);
        let join_handle = tokio::spawn(async move { loop_state.run().await });

        Self {
            command_tx,
            event_rx,
            join_handle: Some(join_handle),
        }
    }

    /// Pull the next event from the modem loop.
    ///
    /// Returns `None` once the task has exited and the event channel
    /// has been fully drained.
    ///
    /// Consume events promptly: the modem loop never blocks on a slow
    /// consumer. If the bounded event ring wraps, the next call returns
    /// [`Event::EventsDropped`] with the exact number of overwritten
    /// events before delivery resumes at the oldest retained event.
    ///
    /// # Cancellation safety
    ///
    /// Cancel-safe: backed by `tokio::sync::broadcast::Receiver::recv`.
    pub async fn next_event(&mut self) -> Option<Event> {
        match self.event_rx.recv().await {
            Ok(event) => Some(event),
            Err(broadcast::error::RecvError::Lagged(count)) => Some(Event::EventsDropped { count }),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Enqueue a D-STAR header for transmission.
    ///
    /// The frame is placed in the loop's TX queue and drained only
    /// when the modem reports enough D-STAR FIFO space.
    ///
    /// # Cancellation
    ///
    /// Not cancellation-atomic: if this future is dropped (e.g. by a
    /// `timeout`) after the command was queued but before the reply
    /// arrived, the frame may still be transmitted. This applies to
    /// all `send_*` and `set_mode` calls on this handle.
    ///
    /// # Errors
    ///
    /// - [`ShellError::BufferFull`] if the TX queue is at capacity.
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    pub async fn send_dstar_header(&mut self, bytes: [u8; 41]) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SendDStarHeader { bytes, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Enqueue a D-STAR voice data frame for transmission.
    ///
    /// See [`AsyncModem::send_dstar_header`] for cancellation
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`ShellError::BufferFull`] if the TX queue is at capacity.
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    pub async fn send_dstar_data(&mut self, bytes: [u8; 12]) -> Result<(), ShellError> {
        // Hang-hunt: two awaits here. Trace both sides so a repro
        // log narrows the freeze to "command_tx full" (ModemLoop
        // not draining command_rx) vs. "reply never came"
        // (ModemLoop received but stuck before replying).
        let (tx, rx) = oneshot::channel();
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            cmd_cap = self.command_tx.capacity(),
            "send_dstar_data: awaiting command_tx.send"
        );
        self.command_tx
            .send(Command::SendDStarData { bytes, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            "send_dstar_data: command queued, awaiting reply"
        );
        let r = rx.await.map_err(|_| ShellError::SessionClosed)?;
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            "send_dstar_data: reply received"
        );
        r
    }

    /// Enqueue a D-STAR end-of-transmission marker.
    ///
    /// See [`AsyncModem::send_dstar_header`] for cancellation
    /// semantics.
    ///
    /// # Errors
    ///
    /// - [`ShellError::BufferFull`] if the TX queue is at capacity.
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    pub async fn send_dstar_eot(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SendDStarEot { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Set the modem's operating mode.
    ///
    /// Resolves only after the modem acknowledges the mode change:
    /// an `Ok(())` means the modem actually switched, not merely
    /// that the request was written. (The corresponding
    /// [`Event::Ack`]/[`Event::Nak`] is still emitted on the event
    /// stream as well.)
    ///
    /// # Errors
    ///
    /// - [`ShellError::Nak`] if the modem rejected the mode change.
    /// - [`ShellError::ResponseTimeout`] if the modem did not answer
    ///   within 2 s.
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    /// - [`ShellError::Core`] if the codec rejects the frame.
    pub async fn set_mode(&mut self, mode: ModemMode) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetMode { mode, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        match tokio::time::timeout(SET_MODE_RESPONSE_TIMEOUT, rx).await {
            Ok(reply) => reply.map_err(|_| ShellError::SessionClosed)?,
            Err(_elapsed) => Err(ShellError::ResponseTimeout),
        }
    }

    /// Trigger a `GetVersion` request. The response arrives as
    /// [`Event::Version`].
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    pub async fn request_version(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetVersion { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Trigger an immediate `GetStatus` request. The response
    /// arrives as [`Event::Status`]. The loop also polls status
    /// every 250 ms on its own, so this is only needed for explicit
    /// "check now" flows.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    pub async fn request_status(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::GetStatus { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Send a raw frame: an escape hatch for protocols we don't model
    /// yet.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    /// - [`ShellError::Core`] if the codec rejects the frame (e.g.
    ///   oversized payload).
    pub async fn send_raw(&mut self, command: u8, payload: Vec<u8>) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SendRaw {
                command,
                payload,
                reply: tx,
            })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Graceful shutdown: flushes the TX queue (bounded by an
    /// internal ~2 s deadline), exits the loop, and returns the
    /// recovered transport.
    ///
    /// Consumes the handle. After `shutdown` returns, the task has
    /// fully wound down and ownership of the transport is handed back
    /// to the caller so it can be reused (e.g. to switch back to CAT
    /// mode on a serial port). If the modem never grants FIFO space
    /// for queued frames, the flush deadline expires, the remaining
    /// frames are dropped (logged and reported as
    /// [`Event::TxDropped`]), and shutdown still completes; it never
    /// hangs on a wedged modem.
    ///
    /// Works even if the loop already exited on its own (EOF or
    /// transport error): the transport recovered by the task is
    /// handed back whenever the loop terminated cleanly.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] / [`ShellError::Core`] if the loop exited
    ///   with that error (the transport is not recoverable).
    /// - [`ShellError::SessionClosed`] if the task panicked or was
    ///   aborted before it could hand the transport back.
    pub async fn shutdown(mut self) -> Result<T, ShellError> {
        let (tx, rx) = oneshot::channel();
        if self
            .command_tx
            .send(Command::Shutdown { reply: tx })
            .await
            .is_ok()
        {
            // Ignore a dropped reply: the loop may already be on
            // its way out, which is fine; we only need it to
            // terminate.
            if rx.await.is_err() {
                tracing::debug!(
                    target: "mmdvm::tokio_shell",
                    "loop exited before acknowledging shutdown"
                );
            }
        }

        // Drain any remaining events so the loop can finish its
        // flush. Once the send half drops (when the loop exits), this
        // terminates, also immediately if the loop was already gone.
        while self.next_event().await.is_some() {}

        // Reclaim the transport from the task.
        let handle = self.join_handle.take().ok_or(ShellError::SessionClosed)?;
        match handle.await {
            Ok(transport_result) => transport_result,
            Err(join_err) => {
                // A panic in the loop would otherwise vanish into a
                // generic "session closed".
                tracing::warn!(
                    target: "mmdvm::tokio_shell",
                    error = %join_err,
                    "modem task did not complete cleanly"
                );
                Err(ShellError::SessionClosed)
            }
        }
    }
}

impl<T: Transport + 'static> Drop for AsyncModem<T> {
    fn drop(&mut self) {
        // Dropping command_tx closes the channel, which signals the
        // modem task to exit on its next loop iteration. The spawned
        // task's JoinHandle is detached: if the caller never invoked
        // `shutdown`, we do not await the task (awaiting in Drop would
        // require blocking). The tokio runtime detaches the task and
        // its transport will be dropped when the task finishes.
    }
}
