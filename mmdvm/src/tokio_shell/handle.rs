//! User-facing handle for an async MMDVM modem running in a spawned
//! task.

use mmdvm_core::ModemMode;
use tokio::sync::{mpsc, oneshot};
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

/// Capacity of the event channel from modem task to handle.
///
/// Events cover both periodic status pushes and inbound radio frames.
/// 256 covers a full D-STAR transmission (~100 voice frames) plus
/// status polls at 4 Hz with generous headroom.
const EVENT_CHANNEL_CAPACITY: usize = 256;

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
    event_rx: mpsc::Receiver<Event>,
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
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);

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
    /// # Cancellation safety
    ///
    /// Cancel-safe — backed by `tokio::sync::mpsc::Receiver::recv`.
    pub async fn next_event(&mut self) -> Option<Event> {
        self.event_rx.recv().await
    }

    /// Enqueue a D-STAR header for transmission.
    ///
    /// The frame is placed in the loop's TX queue and drained only
    /// when the modem reports enough D-STAR FIFO space.
    ///
    /// # Errors
    ///
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
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    pub async fn send_dstar_data(&mut self, bytes: [u8; 12]) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SendDStarData { bytes, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
    }

    /// Enqueue a D-STAR end-of-transmission marker.
    ///
    /// # Errors
    ///
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
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop has exited.
    /// - [`ShellError::Io`] if writing to the transport fails.
    /// - [`ShellError::Core`] if the codec rejects the frame.
    pub async fn set_mode(&mut self, mode: ModemMode) -> Result<(), ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::SetMode { mode, reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?
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

    /// Send a raw frame — escape hatch for protocols we don't model
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

    /// Graceful shutdown — flushes the TX queue, exits the loop, and
    /// returns the recovered transport.
    ///
    /// Consumes the handle. After `shutdown` returns, the task has
    /// fully wound down and ownership of the transport is handed back
    /// to the caller so it can be reused (e.g. to switch back to CAT
    /// mode on a serial port).
    ///
    /// # Errors
    ///
    /// - [`ShellError::SessionClosed`] if the loop had already exited
    ///   before the shutdown command could be delivered, or the task
    ///   panicked / was aborted before it could hand the transport back.
    pub async fn shutdown(mut self) -> Result<T, ShellError> {
        let (tx, rx) = oneshot::channel();
        self.command_tx
            .send(Command::Shutdown { reply: tx })
            .await
            .map_err(|_| ShellError::SessionClosed)?;
        rx.await.map_err(|_| ShellError::SessionClosed)?;

        // Drain any remaining events so the loop can finish its
        // flush. Once the send half drops (when the loop exits), this
        // loop terminates.
        while self.event_rx.recv().await.is_some() {}

        // Reclaim the transport from the task.
        let handle = self.join_handle.take().ok_or(ShellError::SessionClosed)?;
        match handle.await {
            Ok(transport_result) => transport_result,
            Err(_join_err) => Err(ShellError::SessionClosed),
        }
    }
}

impl<T: Transport + 'static> Drop for AsyncModem<T> {
    fn drop(&mut self) {
        // Dropping command_tx closes the channel, which signals the
        // modem task to exit on its next loop iteration. The spawned
        // task's JoinHandle is detached — if the caller never invoked
        // `shutdown`, we do not await the task (awaiting in Drop would
        // require blocking). The tokio runtime detaches the task and
        // its transport will be dropped when the task finishes.
    }
}
