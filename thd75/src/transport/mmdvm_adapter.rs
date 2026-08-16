//! Adapter bridging [`crate::Transport`] to tokio's
//! [`AsyncRead`] + [`AsyncWrite`] contracts.
//!
//! The [`mmdvm`] crate's tokio shell requires transports that implement
//! [`tokio::io::AsyncRead`] and [`tokio::io::AsyncWrite`]. The
//! [`crate::Transport`] trait exposes ergonomic `async fn read` /
//! `async fn write` methods, which are incompatible at the trait-object
//! level. This adapter converts the `async fn` interface into the
//! poll-based interface tokio uses internally.
//!
//! # Implementation strategy
//!
//! A **pump task** owns the inner transport and serializes reads and
//! writes via [`tokio::select!`]. The adapter communicates with the
//! pump via two mpsc channels:
//!
//! - **Write channel** (`adapter → pump`): bounded write requests carrying a
//!   completion acknowledgement.
//! - **Read channel** (`pump → adapter`): byte buffers read from the
//!   transport, one `Vec<u8>` per [`crate::Transport::read`] call.
//!
//! The pump task's `select!` interleaves read and write operations on
//! the same `T`, so a pending read never blocks an outgoing write (and
//! vice versa). This mirrors the serialization that
//! [`tokio::io::split`] provides for types that support an explicit
//! half-split, without requiring `T` to support one.
//!
//! [`AsyncWrite::poll_write`] reserves bounded channel capacity before it
//! accepts bytes. [`AsyncWrite::poll_flush`] waits for every accepted
//! request's acknowledgement, so `Ok(())` means the underlying
//! [`crate::Transport::write`] calls have completed. Shutdown flushes first,
//! closes the request channel, and waits for the pump to return. The same pump
//! result lets [`MmdvmTransportAdapter::shutdown_and_recover`] recover `T`
//! after a clean exit without dummy replacement channels; terminal transport
//! errors remain errors rather than being discarded during recovery.
//!
//! # Local task ownership
//!
//! The pump uses [`tokio::task::spawn_local`], so callers must construct this
//! adapter inside a [`tokio::task::LocalSet`]. This is an adapter ownership
//! choice, not an `IOBluetooth` thread-affinity requirement: native macOS
//! Bluetooth owns its framework objects and `CFRunLoop` in a private helper
//! process.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::TransportError;

use super::Transport;

/// Channel capacity for outbound write buffers.
///
/// MMDVM frames are small (≤ 255 bytes) and send rates are modest; a
/// small buffer here is plenty while still providing modest
/// backpressure if the pump task ever falls behind.
const WRITE_CHANNEL_CAPACITY: usize = 64;

/// Channel capacity for inbound read buffers.
///
/// Read chunks are up to [`READ_CHUNK_SIZE`] bytes each; 64 slots is
/// over 30 KiB of burst capacity, far beyond anything the MMDVM
/// protocol produces.
const READ_CHANNEL_CAPACITY: usize = 64;

/// Size of each scratch buffer the pump task uses for one
/// [`Transport::read`] call.
const READ_CHUNK_SIZE: usize = 512;

/// Cloneable description of a terminal I/O failure.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IoFailure {
    kind: io::ErrorKind,
    message: Arc<str>,
}

impl IoFailure {
    fn from_error(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: Arc::from(error.to_string()),
        }
    }

    fn to_error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.to_string())
    }
}

type TerminalFailure = Arc<Mutex<Option<IoFailure>>>;
type WriteCompletion = oneshot::Receiver<Result<(), IoFailure>>;
type ReserveWriteSlot = Pin<
    Box<
        dyn Future<Output = Result<mpsc::OwnedPermit<WriteRequest>, mpsc::error::SendError<()>>>
            + Send,
    >,
>;

#[derive(Debug)]
struct WriteRequest {
    bytes: Vec<u8>,
    completion: oneshot::Sender<Result<(), IoFailure>>,
}

/// Poll-aware owner of the bounded write sender and an optional capacity
/// reservation.
struct WriteChannel {
    sender: Option<mpsc::Sender<WriteRequest>>,
    reservation: Option<ReserveWriteSlot>,
    permit: Option<mpsc::OwnedPermit<WriteRequest>>,
}

impl WriteChannel {
    const fn new(sender: mpsc::Sender<WriteRequest>) -> Self {
        Self {
            sender: Some(sender),
            reservation: None,
            permit: None,
        }
    }

    fn poll_reserve(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.permit.is_some() {
            return Poll::Ready(Ok(()));
        }

        if self.reservation.is_none() {
            let Some(sender) = self.sender.as_ref() else {
                return Poll::Ready(Err(write_channel_closed()));
            };
            self.reservation = Some(Box::pin(sender.clone().reserve_owned()));
        }

        let Some(reservation) = self.reservation.as_mut() else {
            return Poll::Ready(Err(io::Error::other(
                "MmdvmTransportAdapter: write reservation state was lost",
            )));
        };
        match reservation.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(permit)) => {
                self.reservation = None;
                self.permit = Some(permit);
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(_closed)) => {
                self.close();
                Poll::Ready(Err(write_channel_closed()))
            }
        }
    }

    fn send(&mut self, request: WriteRequest) -> io::Result<()> {
        let Some(permit) = self.permit.take() else {
            return Err(io::Error::other(
                "MmdvmTransportAdapter: write attempted without reserved channel capacity",
            ));
        };
        drop(permit.send(request));
        Ok(())
    }

    fn abort_reservation(&mut self) {
        drop(self.reservation.take());
        drop(self.permit.take());
    }

    fn close(&mut self) {
        self.abort_reservation();
        drop(self.sender.take());
    }
}

struct PumpExit<T> {
    transport: T,
    failure: Option<IoFailure>,
}

/// Failure returned while recovering a transport from an MMDVM adapter.
///
/// A transport-level pump failure still leaves ownership of the transport in
/// [`Self::transport`], allowing a caller to reopen the physical link while
/// retaining the original failure. A pump-task panic cannot preserve `T`
/// because unwinding has already dropped it, so `transport` is `None` on that
/// path.
pub struct MmdvmTransportRecoveryError<T> {
    transport: Option<T>,
    source: io::Error,
}

impl<T> MmdvmTransportRecoveryError<T> {
    /// Borrow the recovered transport, if the pump returned it.
    #[must_use]
    pub const fn transport(&self) -> Option<&T> {
        self.transport.as_ref()
    }

    /// Separate the recoverable transport from the original I/O failure.
    #[must_use]
    pub fn into_parts(self) -> (Option<T>, io::Error) {
        (self.transport, self.source)
    }
}

impl<T> std::fmt::Debug for MmdvmTransportRecoveryError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmTransportRecoveryError")
            .field("transport_recovered", &self.transport.is_some())
            .field("source", &self.source)
            .finish()
    }
}

impl<T> std::fmt::Display for MmdvmTransportRecoveryError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MMDVM transport recovery failed: {}", self.source)
    }
}

impl<T> std::error::Error for MmdvmTransportRecoveryError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn write_channel_closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::BrokenPipe,
        "MmdvmTransportAdapter: pump task exited",
    )
}

fn store_terminal_failure(slot: &TerminalFailure, failure: &IoFailure) {
    let mut current = match slot.lock() {
        Ok(current) => current,
        Err(poisoned) => poisoned.into_inner(),
    };
    if current.is_none() {
        *current = Some(failure.clone());
    }
}

fn load_terminal_failure(slot: &TerminalFailure) -> Option<IoFailure> {
    let current = match slot.lock() {
        Ok(current) => current,
        Err(poisoned) => poisoned.into_inner(),
    };
    current.clone()
}

/// Adapter that presents a [`crate::Transport`] as a tokio
/// [`AsyncRead`] + [`AsyncWrite`] + [`Send`] + [`Unpin`] duplex stream.
///
/// See the [module-level docs](self) for the pump-task architecture
/// and rationale.
pub struct MmdvmTransportAdapter<T: Transport + 'static> {
    /// Buffered bytes from the latest read that didn't fit in the
    /// caller's [`ReadBuf`]. Drained first by [`Self::poll_read`]
    /// before pulling more from [`Self::read_rx`].
    leftover: Vec<u8>,
    /// Inbound byte buffers from the pump task.
    read_rx: Option<mpsc::Receiver<Vec<u8>>>,
    /// Poll-aware bounded channel for outbound write requests.
    write_channel: WriteChannel,
    /// Completion acknowledgements for accepted writes, in acceptance order.
    write_completions: VecDeque<WriteCompletion>,
    /// First terminal pump failure, shared with both I/O halves.
    terminal_failure: TerminalFailure,
    /// Whether shutdown has closed the outbound channel.
    shutdown_started: bool,
    /// Join handle for the pump task. Dropping the adapter without
    /// [`Self::shutdown_and_recover`] still cleanly terminates the pump via the
    /// channel close; the join handle is retained only so
    /// [`Self::shutdown_and_recover`] can await the pump and recover `T`.
    pump: Option<JoinHandle<PumpExit<T>>>,
    /// Pump result retained when `poll_shutdown` joins it before
    /// [`Self::shutdown_and_recover`] consumes the adapter.
    pump_result: Option<Result<PumpExit<T>, IoFailure>>,
}

impl<T: Transport + 'static> std::fmt::Debug for MmdvmTransportAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmTransportAdapter")
            .field("leftover_len", &self.leftover.len())
            .field("pending_writes", &self.write_completions.len())
            .field("shutdown_started", &self.shutdown_started)
            .finish_non_exhaustive()
    }
}

impl<T: Transport + 'static> MmdvmTransportAdapter<T> {
    /// Wrap an existing transport.
    ///
    /// Spawns the pump task on the current [`tokio::task::LocalSet`]
    /// via [`tokio::task::spawn_local`]. **Panics** if no `LocalSet`
    /// is active; see the [module-level docs](self).
    #[must_use]
    pub fn new(inner: T) -> Self {
        let (write_tx, write_rx) = mpsc::channel::<WriteRequest>(WRITE_CHANNEL_CAPACITY);
        let (read_tx, read_rx) = mpsc::channel::<Vec<u8>>(READ_CHANNEL_CAPACITY);
        let terminal_failure = Arc::new(Mutex::new(None));
        let pump = tokio::task::spawn_local(pump_task(
            inner,
            write_rx,
            read_tx,
            Arc::clone(&terminal_failure),
        ));
        Self {
            leftover: Vec::new(),
            read_rx: Some(read_rx),
            write_channel: WriteChannel::new(write_tx),
            write_completions: VecDeque::new(),
            terminal_failure,
            shutdown_started: false,
            pump: Some(pump),
            pump_result: None,
        }
    }

    /// Recover the inner transport after the adapter's consumer has
    /// finished with it.
    ///
    /// Closes the write channel, which signals the pump task to drop
    /// the transport cleanly. Then awaits the pump's [`JoinHandle`]
    /// to recover the inner `T`. Call this after
    /// [`mmdvm::AsyncModem::shutdown`] has returned: by then the
    /// modem loop has released the adapter. Any writes already accepted by
    /// [`AsyncWrite::poll_write`] are completed before the pump returns.
    ///
    /// # Errors
    ///
    /// Returns [`MmdvmTransportRecoveryError`] if the pump task panicked or
    /// exited because of a transport error. A transport-level failure retains
    /// `T` for recovery; a task panic cannot.
    pub async fn shutdown_and_recover(mut self) -> Result<T, MmdvmTransportRecoveryError<T>> {
        self.begin_shutdown();
        drop(self.read_rx.take());

        let result = if let Some(result) = self.pump_result.take() {
            result
        } else {
            let Some(pump) = self.pump.take() else {
                return Err(MmdvmTransportRecoveryError {
                    transport: None,
                    source: io::Error::other("MmdvmTransportAdapter: pump already joined"),
                });
            };
            match pump.await {
                Ok(exit) => Ok(exit),
                Err(join_error) => Err(pump_join_failure(&join_error)),
            }
        };

        match result {
            Ok(PumpExit {
                transport,
                failure: None,
            }) => Ok(transport),
            Ok(PumpExit {
                failure: Some(failure),
                transport,
            }) => Err(MmdvmTransportRecoveryError {
                transport: Some(transport),
                source: failure.to_error(),
            }),
            Err(failure) => Err(MmdvmTransportRecoveryError {
                transport: None,
                source: failure.to_error(),
            }),
        }
    }

    fn begin_shutdown(&mut self) {
        if !self.shutdown_started {
            self.shutdown_started = true;
            self.write_channel.close();
        }
    }

    fn terminal_error(&self) -> Option<io::Error> {
        load_terminal_failure(&self.terminal_failure).map(|failure| failure.to_error())
    }

    fn poll_write_completions(
        &mut self,
        cx: &mut Context<'_>,
        wait_for_all: bool,
    ) -> Poll<io::Result<()>> {
        loop {
            if let Some(error) = self.terminal_error() {
                return Poll::Ready(Err(error));
            }

            let Some(completion) = self.write_completions.front_mut() else {
                return Poll::Ready(Ok(()));
            };
            match Pin::new(completion).poll(cx) {
                Poll::Pending if wait_for_all => return Poll::Pending,
                Poll::Pending => return Poll::Ready(Ok(())),
                Poll::Ready(Ok(Ok(()))) => {
                    drop(self.write_completions.pop_front());
                }
                Poll::Ready(Ok(Err(failure))) => {
                    drop(self.write_completions.pop_front());
                    return Poll::Ready(Err(failure.to_error()));
                }
                Poll::Ready(Err(_sender_dropped)) => {
                    return Poll::Ready(Err(self
                        .terminal_error()
                        .unwrap_or_else(write_channel_closed)));
                }
            }
        }
    }

    fn poll_pump_exit(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.pump_result.is_none() {
            let Some(pump) = self.pump.as_mut() else {
                return Poll::Ready(Err(io::Error::other(
                    "MmdvmTransportAdapter: pump result was lost",
                )));
            };
            let result = match Pin::new(pump).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Ok(exit)) => Ok(exit),
                Poll::Ready(Err(join_error)) => Err(pump_join_failure(&join_error)),
            };
            drop(self.pump.take());
            self.pump_result = Some(result);
        }

        match self.pump_result.as_ref() {
            Some(Ok(PumpExit { failure: None, .. })) => Poll::Ready(Ok(())),
            Some(
                Ok(PumpExit {
                    failure: Some(failure),
                    ..
                })
                | Err(failure),
            ) => Poll::Ready(Err(failure.to_error())),
            None => Poll::Ready(Err(io::Error::other(
                "MmdvmTransportAdapter: pump result was lost",
            ))),
        }
    }
}

fn pump_join_failure(error: &tokio::task::JoinError) -> IoFailure {
    IoFailure::from_error(&io::Error::other(format!(
        "MmdvmTransportAdapter: pump task failed: {error}"
    )))
}

/// Convert a [`TransportError`] to [`io::Error`] for the tokio traits.
fn transport_err_to_io(err: TransportError) -> io::Error {
    match err {
        TransportError::Disconnected(e) => io::Error::new(io::ErrorKind::BrokenPipe, e),
        TransportError::Read(e) | TransportError::Write(e) => e,
        TransportError::NotFound => io::Error::new(io::ErrorKind::NotFound, "device not found"),
        TransportError::Open { path, source } => {
            io::Error::new(source.kind(), format!("failed to open {path}: {source}"))
        }
        TransportError::BluetoothHelper { context, source } => io::Error::new(
            source.kind(),
            format!("Bluetooth helper failed during {context}: {source}"),
        ),
        error @ TransportError::BluetoothDeviceNameAmbiguous => {
            io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
        }
        TransportError::BrokerUnavailable => {
            io::Error::new(io::ErrorKind::BrokenPipe, "transport broker unavailable")
        }
        e @ (TransportError::ReopenUnsupported | TransportError::WrongThread) => {
            io::Error::new(io::ErrorKind::Unsupported, e.to_string())
        }
    }
}

/// Background task that owns the transport and serializes reads and
/// writes via [`tokio::select!`].
///
/// Exits and returns the transport when:
/// - The write channel is closed by the adapter (normal shutdown).
/// - A read or write fails (transport error).
/// - The read receiver closes (consumer lost interest).
///
/// On a clean exit the transport is returned for recovery. A terminal failure
/// travels with it so [`MmdvmTransportAdapter::shutdown_and_recover`] cannot
/// silently reinterpret a failed pump as a clean recovery.
enum PumpAction {
    Continue(Option<Vec<u8>>),
    Exit(Option<IoFailure>),
}

async fn dispatch_pending_read<T: Transport>(
    transport: &mut T,
    write_rx: &mut mpsc::Receiver<WriteRequest>,
    read_tx: &mpsc::Sender<Vec<u8>>,
    terminal_failure: &TerminalFailure,
    bytes: Vec<u8>,
) -> PumpAction {
    let mut unsent = Some(bytes);
    tokio::select! {
        biased;

        maybe_write = write_rx.recv() => {
            let pending = unsent.take();
            let Some(request) = maybe_write else {
                tracing::debug!(
                    target: "kenwood_thd75::transport::mmdvm_adapter",
                    "write channel closed; pump task exiting"
                );
                return PumpAction::Exit(None);
            };
            match perform_write(transport, request, terminal_failure).await {
                Ok(()) => PumpAction::Continue(pending),
                Err(failure) => PumpAction::Exit(Some(failure)),
            }
        }

        permit = read_tx.reserve() => {
            let Ok(permit) = permit else {
                tracing::debug!(
                    target: "kenwood_thd75::transport::mmdvm_adapter",
                    "read consumer closed; pump exiting"
                );
                return PumpAction::Exit(None);
            };
            if let Some(bytes) = unsent.take() {
                permit.send(bytes);
            }
            PumpAction::Continue(None)
        }
    }
}

fn handle_read_result(
    read_result: Result<usize, TransportError>,
    scratch: &[u8; READ_CHUNK_SIZE],
    read_tx: &mpsc::Sender<Vec<u8>>,
    terminal_failure: &TerminalFailure,
) -> PumpAction {
    match read_result {
        Ok(0) => {
            tracing::debug!(
                target: "kenwood_thd75::transport::mmdvm_adapter",
                "transport read returned EOF; pump exiting"
            );
            let error = io::Error::new(io::ErrorKind::UnexpectedEof, "transport EOF");
            terminal_read_failure(terminal_failure, &error)
        }
        Ok(n) => {
            let Some(slice) = scratch.get(..n) else {
                let error = io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "transport reported {n} bytes read into a {}-byte buffer",
                        scratch.len()
                    ),
                );
                tracing::warn!(
                    target: "kenwood_thd75::transport::mmdvm_adapter",
                    got = n,
                    cap = READ_CHUNK_SIZE,
                    "transport read reported impossible length; pump exiting"
                );
                return terminal_read_failure(terminal_failure, &error);
            };
            match read_tx.try_send(slice.to_vec()) {
                Ok(()) => PumpAction::Continue(None),
                Err(mpsc::error::TrySendError::Full(bytes)) => PumpAction::Continue(Some(bytes)),
                Err(mpsc::error::TrySendError::Closed(_bytes)) => {
                    tracing::debug!(
                        target: "kenwood_thd75::transport::mmdvm_adapter",
                        "read consumer closed; pump exiting"
                    );
                    PumpAction::Exit(None)
                }
            }
        }
        Err(error) => {
            tracing::warn!(
                target: "kenwood_thd75::transport::mmdvm_adapter",
                error = %error,
                "transport read failed; pump exiting"
            );
            let error = transport_err_to_io(error);
            terminal_read_failure(terminal_failure, &error)
        }
    }
}

fn terminal_read_failure(terminal_failure: &TerminalFailure, error: &io::Error) -> PumpAction {
    let failure = IoFailure::from_error(error);
    store_terminal_failure(terminal_failure, &failure);
    PumpAction::Exit(Some(failure))
}

async fn pump_task<T: Transport>(
    mut transport: T,
    mut write_rx: mpsc::Receiver<WriteRequest>,
    read_tx: mpsc::Sender<Vec<u8>>,
    terminal_failure: TerminalFailure,
) -> PumpExit<T> {
    let mut scratch = [0u8; READ_CHUNK_SIZE];
    let mut pending_read = None;
    loop {
        let action = if let Some(bytes) = pending_read.take() {
            dispatch_pending_read(
                &mut transport,
                &mut write_rx,
                &read_tx,
                &terminal_failure,
                bytes,
            )
            .await
        } else {
            tokio::select! {
                biased;

                maybe_write = write_rx.recv() => {
                    let Some(request) = maybe_write else {
                        tracing::debug!(
                            target: "kenwood_thd75::transport::mmdvm_adapter",
                            "write channel closed; pump task exiting"
                        );
                        return PumpExit { transport, failure: None };
                    };
                    match perform_write(&mut transport, request, &terminal_failure).await {
                        Ok(()) => PumpAction::Continue(None),
                        Err(failure) => PumpAction::Exit(Some(failure)),
                    }
                }

                read_result = transport.read(&mut scratch) => {
                    handle_read_result(read_result, &scratch, &read_tx, &terminal_failure)
                }
            }
        };

        match action {
            PumpAction::Continue(bytes) => pending_read = bytes,
            PumpAction::Exit(failure) => return PumpExit { transport, failure },
        }
    }
}

async fn perform_write<T: Transport>(
    transport: &mut T,
    request: WriteRequest,
    terminal_failure: &TerminalFailure,
) -> Result<(), IoFailure> {
    let WriteRequest { bytes, completion } = request;
    tracing::trace!(
        target: "mmdvm::hang_hunt",
        len = bytes.len(),
        "pump: write branch, calling transport.write"
    );
    match transport.write(&bytes).await {
        Ok(()) => {
            drop(completion.send(Ok(())));
            tracing::trace!(target: "mmdvm::hang_hunt", "pump: transport.write returned");
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                target: "kenwood_thd75::transport::mmdvm_adapter",
                error = %error,
                "transport write failed; pump task exiting"
            );
            let error = transport_err_to_io(error);
            let failure = IoFailure::from_error(&error);
            store_terminal_failure(terminal_failure, &failure);
            drop(completion.send(Err(failure.clone())));
            Err(failure)
        }
    }
}

impl<T: Transport + Unpin + 'static> AsyncRead for MmdvmTransportAdapter<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();

        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }

        // First drain anything left over from a previous oversize read.
        if !this.leftover.is_empty() {
            let take = this.leftover.len().min(buf.remaining());
            let drained: Vec<u8> = this.leftover.drain(..take).collect();
            buf.put_slice(&drained);
            return Poll::Ready(Ok(()));
        }

        let Some(read_rx) = this.read_rx.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        match read_rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(this.terminal_error().map_or(Ok(()), Err)),
            Poll::Ready(Some(bytes)) => {
                let take = bytes.len().min(buf.remaining());
                let (to_put, to_save) = bytes.split_at(take);
                buf.put_slice(to_put);
                if !to_save.is_empty() {
                    this.leftover.extend_from_slice(to_save);
                }
                Poll::Ready(Ok(()))
            }
        }
    }
}

impl<T: Transport + Unpin + 'static> AsyncWrite for MmdvmTransportAdapter<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Poll::Ready(Err(error)) = this.poll_write_completions(cx, false) {
            return Poll::Ready(Err(error));
        }
        if this.shutdown_started {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MmdvmTransportAdapter: write side is shut down",
            )));
        }
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        match this.write_channel.poll_reserve(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_channel_closed)) => Poll::Ready(Err(this
                .terminal_error()
                .unwrap_or_else(write_channel_closed))),
            Poll::Ready(Ok(())) => {
                let (completion, completed) = oneshot::channel();
                let request = WriteRequest {
                    bytes: buf.to_vec(),
                    completion,
                };
                if let Err(error) = this.write_channel.send(request) {
                    return Poll::Ready(Err(error));
                }
                this.write_completions.push_back(completed);
                Poll::Ready(Ok(buf.len()))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // A cancelled `write` future may leave only a capacity reservation;
        // it accepted no bytes and must not hold a queue slot through flush.
        this.write_channel.abort_reservation();
        this.poll_write_completions(cx, true)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if !this.shutdown_started {
            this.write_channel.abort_reservation();
            match this.poll_write_completions(cx, true) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    this.begin_shutdown();
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(())) => this.begin_shutdown(),
            }
        }
        this.poll_pump_exit(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Wake, Waker};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Notify;
    use tokio::task::LocalSet;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[derive(Debug, Default)]
    struct WriteGate {
        started: Notify,
        release: Notify,
    }

    #[derive(Debug)]
    struct DelayedWriteTransport {
        gate: Arc<WriteGate>,
        block_first_write: bool,
        writes: Vec<Vec<u8>>,
    }

    impl DelayedWriteTransport {
        fn new() -> (Self, Arc<WriteGate>) {
            let gate = Arc::new(WriteGate::default());
            (
                Self {
                    gate: Arc::clone(&gate),
                    block_first_write: true,
                    writes: Vec::new(),
                },
                gate,
            )
        }
    }

    impl Transport for DelayedWriteTransport {
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            if self.block_first_write {
                self.block_first_write = false;
                self.gate.started.notify_one();
                self.gate.release.notified().await;
            }
            self.writes.push(data.to_vec());
            Ok(())
        }

        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            std::future::pending().await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FailingWriteTransport;

    impl Transport for FailingWriteTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            Err(TransportError::Write(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "deliberate transport write failure",
            )))
        }

        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            std::future::pending().await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct OversizedReadTransport;

    impl Transport for OversizedReadTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }

        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
            Ok(buf.len() + 1)
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct WakeCounter(AtomicUsize);

    impl Wake for WakeCounter {
        fn wake(self: Arc<Self>) {
            let _previous = self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            let _previous = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn assert_write_error(error: &io::Error) {
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(
            error
                .to_string()
                .contains("deliberate transport write failure"),
            "transport error text was not preserved: {error}",
        );
    }

    #[test]
    fn bluetooth_helper_error_keeps_io_kind_and_context() {
        let error = transport_err_to_io(TransportError::BluetoothHelper {
            context: "launching /Applications/AzimuthBluetoothHelper".to_owned(),
            source: io::Error::new(io::ErrorKind::PermissionDenied, "sandbox denied exec"),
        });

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("AzimuthBluetoothHelper"));
        assert!(error.to_string().contains("sandbox denied exec"));
    }

    #[test]
    fn ambiguous_bluetooth_name_maps_to_actionable_invalid_input() {
        let error = transport_err_to_io(TransportError::BluetoothDeviceNameAmbiguous);

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("exact Bluetooth address"));
    }

    #[tokio::test]
    async fn roundtrip_write_then_read() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mut mock = MockTransport::new();
                mock.expect(b"PING\r", b"PONG\r");
                let mut adapter = MmdvmTransportAdapter::new(mock);

                adapter.write_all(b"PING\r").await?;
                let mut buf = [0u8; 16];
                let n = adapter.read(&mut buf).await?;
                assert_eq!(buf.get(..n).ok_or("slice")?, b"PONG\r");
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn shutdown_and_recover_returns_transport() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mut mock = MockTransport::new();
                mock.expect(b"X", b"Y");
                mock.pend_when_empty();
                let mut adapter = MmdvmTransportAdapter::new(mock);
                adapter.write_all(b"X").await?;
                let mut buf = [0u8; 1];
                let n = adapter.read(&mut buf).await?;
                assert_eq!(n, 1);

                let recovered = adapter.shutdown_and_recover().await?;
                drop(recovered);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn shutdown_and_recover_without_io_succeeds() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mock = MockTransport::new();
                let adapter = MmdvmTransportAdapter::new(mock);
                let _mock = adapter.shutdown_and_recover().await?;
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn flush_waits_for_every_accepted_transport_write() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let (transport, gate) = DelayedWriteTransport::new();
                let mut adapter = MmdvmTransportAdapter::new(transport);

                adapter.write_all(b"first").await?;
                adapter.write_all(b"second").await?;
                gate.started.notified().await;

                let early_flush =
                    tokio::time::timeout(Duration::from_millis(20), adapter.flush()).await;
                assert!(
                    early_flush.is_err(),
                    "flush completed before Transport::write"
                );

                gate.release.notify_one();
                adapter.flush().await?;
                let recovered = adapter.shutdown_and_recover().await?;
                assert_eq!(recovered.writes, [b"first".to_vec(), b"second".to_vec()]);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn shutdown_flushes_before_closing_and_preserves_recovery() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let (transport, gate) = DelayedWriteTransport::new();
                let mut adapter = MmdvmTransportAdapter::new(transport);

                adapter.write_all(b"before shutdown").await?;
                gate.started.notified().await;
                let early_shutdown =
                    tokio::time::timeout(Duration::from_millis(20), adapter.shutdown()).await;
                assert!(
                    early_shutdown.is_err(),
                    "shutdown completed before its accepted write"
                );

                gate.release.notify_one();
                adapter.shutdown().await?;
                let Err(late_error) = adapter.write_all(b"after shutdown").await else {
                    return Err("write unexpectedly succeeded after shutdown".into());
                };
                assert_eq!(late_error.kind(), io::ErrorKind::BrokenPipe);

                let recovered = adapter.shutdown_and_recover().await?;
                assert_eq!(recovered.writes, [b"before shutdown".to_vec()]);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn write_failure_reaches_flush_shutdown_and_typed_recovery() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mut adapter = MmdvmTransportAdapter::new(FailingWriteTransport);
                adapter.write_all(b"fails in pump").await?;

                let Err(flush_error) = adapter.flush().await else {
                    return Err("flush hid write failure".into());
                };
                assert_write_error(&flush_error);

                let Err(shutdown_error) = adapter.shutdown().await else {
                    return Err("shutdown hid write failure".into());
                };
                assert_write_error(&shutdown_error);

                let Err(recovery_error) = adapter.shutdown_and_recover().await else {
                    return Err("recovery hid write failure".into());
                };
                assert!(
                    recovery_error.transport().is_some(),
                    "transport was discarded on a recoverable pump failure"
                );
                let (transport, source) = recovery_error.into_parts();
                assert!(transport.is_some(), "typed recovery lost its transport");
                assert_write_error(&source);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn full_write_channel_registers_a_waker_without_self_waking() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let (transport, gate) = DelayedWriteTransport::new();
                let mut adapter = MmdvmTransportAdapter::new(transport);
                let wake_counter = Arc::new(WakeCounter::default());
                let waker = Waker::from(Arc::clone(&wake_counter));
                let mut cx = Context::from_waker(&waker);

                for _ in 0..WRITE_CHANNEL_CAPACITY {
                    assert!(
                        matches!(
                            Pin::new(&mut adapter).poll_write(&mut cx, b"x"),
                            Poll::Ready(Ok(1))
                        ),
                        "bounded write slot was unexpectedly unavailable"
                    );
                }
                assert!(
                    matches!(
                        Pin::new(&mut adapter).poll_write(&mut cx, b"y"),
                        Poll::Pending
                    ),
                    "write beyond channel capacity did not apply backpressure"
                );
                assert_eq!(
                    wake_counter.0.load(Ordering::SeqCst),
                    0,
                    "full channel busy-woke its own task"
                );

                tokio::task::yield_now().await;
                gate.started.notified().await;
                assert!(
                    wake_counter.0.load(Ordering::SeqCst) > 0,
                    "receiver capacity did not wake the pending writer"
                );
                assert!(
                    matches!(
                        Pin::new(&mut adapter).poll_write(&mut cx, b"y"),
                        Poll::Ready(Ok(1))
                    ),
                    "woken writer could not consume reserved capacity"
                );

                gate.release.notify_one();
                adapter.flush().await?;
                adapter.shutdown().await?;
                let recovered = adapter.shutdown_and_recover().await?;
                assert_eq!(recovered.writes.len(), WRITE_CHANNEL_CAPACITY + 1);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn impossible_read_count_is_terminal_and_preserves_transport() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mut adapter = MmdvmTransportAdapter::new(OversizedReadTransport);
                let mut byte = [0_u8; 1];
                let Err(read_error) = adapter.read(&mut byte).await else {
                    return Err("oversized read count was accepted".into());
                };
                assert_eq!(read_error.kind(), io::ErrorKind::InvalidData);
                assert!(
                    read_error
                        .to_string()
                        .contains("bytes read into a 512-byte buffer"),
                    "invalid read count lost its details: {read_error}"
                );

                let Err(recovery_error) = adapter.shutdown_and_recover().await else {
                    return Err("terminal read failure was discarded".into());
                };
                assert!(
                    recovery_error.transport().is_some(),
                    "terminal read failure discarded the transport"
                );
                let (_transport, source) = recovery_error.into_parts();
                assert_eq!(source.kind(), io::ErrorKind::InvalidData);
                Ok(())
            })
            .await
    }
}
