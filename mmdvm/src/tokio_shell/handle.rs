//! User-facing handle for an async MMDVM modem running in a spawned
//! task.

use std::fmt;

use mmdvm_core::{Command, MmdvmFrame, ModemMode};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::ShellError;
use crate::transport::Transport;

use super::{
    DstarConfiguration, Event, ModemLoop, ModemShutdown, Request, ShutdownFailure, ShutdownSummary,
    TaskCompletion,
};

/// Capacity of the command channel between handle and modem task.
///
/// At most 32 requests wait for the modem task; further submissions await
/// channel capacity. Each raw request already carries a bounded core frame.
const COMMAND_CHANNEL_CAPACITY: usize = 32;

/// Capacity of the event ring from modem task to handle.
///
/// At most 256 events remain available to a receiver; overwritten events are
/// reported by [`Event::EventsDropped`].
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// How long [`AsyncModem::set_mode`] waits for the modem's ACK/NAK
/// before failing with [`ShellError::ResponseTimeout`].
///
/// This host policy covers channel submission and waiting for the reply.
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
/// shutdown that attempts to drain the pending TX queue and recovers the
/// transport after a clean exit, call [`AsyncModem::shutdown`].
/// See the [runtime example](crate#drive-and-reclaim-a-modem).
pub struct AsyncModem<T> {
    command_tx: mpsc::Sender<Request>,
    event_rx: broadcast::Receiver<Event>,
    join_handle: JoinHandle<TaskCompletion<T>>,
}
impl<T> fmt::Debug for AsyncModem<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AsyncModem")
            .field("command_capacity", &self.command_tx.capacity())
            .field("task_finished", &self.join_handle.is_finished())
            .finish_non_exhaustive()
    }
}

impl<T: Transport + 'static> AsyncModem<T> {
    /// Configure a public MMDVM D-STAR layout and await its matching ACK.
    ///
    /// One two-second deadline covers submission, the startup version response,
    /// and the configuration reply. The selected version must match that
    /// response before any configuration bytes are written. An unwritten
    /// request is discarded after cancellation or timeout. Once written, its
    /// reservation survives cancellation and blocks another configuration until
    /// the matching ACK/NAK arrives or the stream is retired.
    ///
    /// # Errors
    ///
    /// Returns [`ShellError::ConfigurationProtocolMismatch`] before writing for
    /// a different confirmed version, [`ShellError::ConfigurationPending`] for
    /// an unresolved configuration, [`ShellError::ResponseTimeout`] at the total
    /// deadline, or the typed NAK, I/O, or closed-session error.
    pub async fn configure_dstar(
        &mut self,
        configuration: DstarConfiguration,
    ) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + SET_MODE_RESPONSE_TIMEOUT;
        tokio::time::timeout_at(
            deadline,
            self.command_tx.send(Request::ConfigureDstar {
                configuration,
                deadline,
                reply: tx,
            }),
        )
        .await
        .map_err(|_| ShellError::ResponseTimeout)?
        .map_err(|_| ShellError::SessionClosed)?;
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(reply) => reply.map_err(|_| ShellError::SessionClosed)?,
            Err(_elapsed) => Err(ShellError::ResponseTimeout),
        }
    }

    /// Spawn the modem loop on the current Tokio runtime and return
    /// a handle for controlling it.
    ///
    /// The `transport` must be an already-connected duplex byte
    /// stream (serial port, Bluetooth SPP, test duplex). The shell
    /// takes ownership. A clean shutdown returns the stream; a failed task
    /// drops it. Initial version and status requests are sent asynchronously.
    /// The runtime must have its time driver enabled for polling and deadlines.
    ///
    /// # Panics
    ///
    /// Panics outside an active Tokio runtime.
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
            join_handle,
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
            .send(Request::SendDStarHeader { bytes, reply: tx })
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
        // Trace channel backpressure separately from the queue result.
        let (tx, rx) = oneshot::channel();
        tracing::trace!(
            target: "mmdvm::hang_hunt",
            cmd_cap = self.command_tx.capacity(),
            "send_dstar_data: awaiting command_tx.send"
        );
        self.command_tx
            .send(Request::SendDStarData { bytes, reply: tx })
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
            .send(Request::SendDStarEot { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Set the modem's operating mode.
    ///
    /// Resolves only after the modem acknowledges the requested command.
    /// An `Ok(())` means its ACK was received. The corresponding
    /// [`Event::Ack`]/[`Event::Nak`] is still emitted on the event
    /// stream as well. One two-second deadline covers channel submission and
    /// response waiting. Cancellation or timeout can leave an already-written
    /// change in effect. Until its reply arrives, another mode change is
    /// rejected; a caller can also shut down and retire the stream.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Nak`] if the modem rejected the mode change.
    /// - [`ShellError::ResponseTimeout`] if the modem did not answer
    ///   within the total two-second deadline.
    /// - [`ShellError::ModeChangePending`] if an earlier change is unresolved.
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    pub async fn set_mode(&mut self, mode: ModemMode) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + SET_MODE_RESPONSE_TIMEOUT;
        tokio::time::timeout_at(
            deadline,
            self.command_tx.send(Request::SetMode {
                mode,
                deadline,
                reply: tx,
            }),
        )
        .await
        .map_err(|_| ShellError::ResponseTimeout)?
        .map_err(|_| ShellError::SessionClosed)?;
        match tokio::time::timeout_at(deadline, rx).await {
            Ok(reply) => reply.map_err(|_| ShellError::SessionClosed)?,
            Err(_elapsed) => Err(ShellError::ResponseTimeout),
        }
    }

    /// Trigger a `GetVersion` request. The response arrives as
    /// [`Event::Version`].
    /// Request completion means written and flushed, not that a response was
    /// received. Cancellation after submission can leave the request in flight.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    pub async fn request_version(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Request::GetVersion { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Trigger an immediate `GetStatus` request. The response
    /// arrives as [`Event::Status`]. The loop also polls status
    /// every 250 ms on its own, so this is only needed for explicit
    /// "check now" flows.
    /// Request completion means written and flushed. Cancellation after
    /// submission can leave the request in flight.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    pub async fn request_status(&mut self) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Request::GetStatus { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Write and flush a bounded payload with a typed command identifier.
    ///
    /// This bypasses D-STAR FIFO gating. Use the typed D-STAR submission
    /// methods for voice traffic. `SET_MODE` and `SET_CONFIG` are reserved for
    /// [`Self::set_mode`] and [`Self::configure_dstar`] so their ACKs cannot
    /// resolve another operation. Payload validation precedes
    /// channel submission. See [`Self::send_dstar_header`] for cancellation.
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    /// - [`ShellError::Core`] if the codec rejects the frame (e.g.
    ///   oversized payload).
    /// - [`ShellError::ReservedCommand`] for `SET_MODE` or `SET_CONFIG`.
    pub async fn send_raw(&mut self, command: Command, payload: Vec<u8>) -> Result<(), ShellError> {
        if matches!(command, Command::SET_MODE | Command::SET_CONFIG) {
            return Err(ShellError::ReservedCommand { command });
        }
        let frame = MmdvmFrame::with_payload(command, payload)?;
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Request::SendRaw { frame, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Attempt to drain the TX queue, stop the task and reclaim its stream.
    ///
    /// Consumes the handle. After `shutdown` returns, the task has
    /// fully wound down and ownership of the transport is handed back
    /// to the caller so it can be reused (e.g. to switch back to CAT
    /// mode on a serial port). If the modem never grants FIFO space
    /// for queued frames, the flush deadline expires, the remaining
    /// frames are counted in [`ShutdownSummary::dropped_frames`] and also
    /// reported as [`Event::TxDropped`]. Unread events are retained in the
    /// bounded completion summary. The
    /// two-second drain deadline starts when the loop accepts shutdown; it is
    /// checked between operations. A write and flush can take up to five
    /// seconds, including one already in progress before shutdown is accepted.
    ///
    /// # Cancellation
    ///
    /// Dropping this future detaches the task and loses recovery of its stream.
    /// Already submitted commands and queued frames may still take effect.
    ///
    /// Works even if the loop already exited on its own (EOF or
    /// transport error): the transport recovered by the task is
    /// handed back whenever the loop terminated cleanly.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] / [`ShellError::Core`] if the loop exited
    ///   with that error (the transport is not recoverable).
    /// - [`ShellError::Task`] if the task panicked or was aborted before it
    ///   could hand the transport back. Its dropped-frame count is unknown.
    ///
    /// Every failure retains its observations in [`ShutdownFailure::summary`].
    pub async fn shutdown(mut self) -> Result<ModemShutdown<T>, ShutdownFailure> {
        let (tx, rx) = oneshot::channel();
        if self
            .command_tx
            .send(Request::Shutdown { reply: tx })
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

        // Broadcast delivery never waits for a receiver. Joining first bounds
        // the following drain to the final retained ring, even on a busy link.
        let (transport, dropped_frames) = match self.join_handle.await {
            Ok(completion) => (completion.transport, Some(completion.dropped_frames)),
            Err(error) => (Err(ShellError::Task(error)), None),
        };
        let mut summary = ShutdownSummary {
            dropped_frames,
            events: Vec::new(),
            skipped_events: 0,
        };
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => summary.events.push(event),
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    // The sender has stopped, so at most one lag notification
                    // precedes the static retained suffix.
                    summary.skipped_events = count;
                }
                Err(
                    broadcast::error::TryRecvError::Closed | broadcast::error::TryRecvError::Empty,
                ) => break,
            }
        }
        match transport {
            Ok(transport) => Ok(ModemShutdown { transport, summary }),
            Err(error) => Err(ShutdownFailure { error, summary }),
        }
    }
}
