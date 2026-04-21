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
//! - **Write channel** (`adapter → pump`): byte buffers to write.
//! - **Read channel** (`pump → adapter`): byte buffers read from the
//!   transport, one `Vec<u8>` per [`crate::Transport::read`] call.
//!
//! The pump task's `select!` interleaves read and write operations on
//! the same `T`, so a pending read never blocks an outgoing write (and
//! vice versa). This mirrors the serialization that
//! [`tokio::io::split`] provides for types that support an explicit
//! half-split, without requiring `T` to support one.
//!
//! [`MmdvmTransportAdapter::into_inner`] closes the write channel,
//! awaits the pump task's [`JoinHandle`], and recovers the inner `T`
//! that the pump returned on clean exit.
//!
//! # Thread-affinity (macOS Bluetooth)
//!
//! The pump task is spawned with [`tokio::task::spawn_local`] so it
//! runs on the same OS thread as the calling [`tokio::task::LocalSet`].
//! This is **required** for [`crate::transport::BluetoothTransport`] on
//! macOS: `IOBluetooth`'s RFCOMM channel callbacks are dispatched to the
//! `CFRunLoop` of the thread that opened the channel (typically the
//! main thread, before the tokio runtime starts). Pumping that runloop
//! from a worker thread is a no-op — the callbacks never deliver data
//! into the pipe that `BluetoothTransport::read` waits on. By keeping
//! the pump on the same thread, every `bt_pump_runloop()` call drains
//! pending callbacks where they actually live.
//!
//! Callers must therefore construct this adapter from inside a
//! [`tokio::task::LocalSet`]. For the REPL/TUI, the top-level
//! `run_repl` future is launched via `LocalSet::block_on`, satisfying
//! this requirement transparently.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
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
    read_rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    /// Outbound byte buffers to the pump task.
    write_tx: mpsc::Sender<Vec<u8>>,
    /// Join handle for the pump task. Dropping the adapter without
    /// [`Self::into_inner`] still cleanly terminates the pump via the
    /// channel close; the join handle is retained only so
    /// [`Self::into_inner`] can await the pump and recover `T`.
    pump: Option<JoinHandle<T>>,
}

impl<T: Transport + 'static> std::fmt::Debug for MmdvmTransportAdapter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmdvmTransportAdapter")
            .field("leftover_len", &self.leftover.len())
            .finish_non_exhaustive()
    }
}

impl<T: Transport + 'static> MmdvmTransportAdapter<T> {
    /// Wrap an existing transport.
    ///
    /// Spawns the pump task on the current [`tokio::task::LocalSet`]
    /// via [`tokio::task::spawn_local`]. **Panics** if no `LocalSet`
    /// is active — see the [module-level docs](self) for why this is
    /// required (macOS Bluetooth thread-affinity).
    #[must_use]
    pub fn new(inner: T) -> Self {
        let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(WRITE_CHANNEL_CAPACITY);
        let (read_tx, read_rx) = mpsc::channel::<io::Result<Vec<u8>>>(READ_CHANNEL_CAPACITY);
        let pump = tokio::task::spawn_local(pump_task(inner, write_rx, read_tx));
        Self {
            leftover: Vec::new(),
            read_rx,
            write_tx,
            pump: Some(pump),
        }
    }

    /// Recover the inner transport after the adapter's consumer has
    /// finished with it.
    ///
    /// Closes the write channel, which signals the pump task to drop
    /// the transport cleanly. Then awaits the pump's [`JoinHandle`]
    /// to recover the inner `T`. Call this after
    /// [`mmdvm::AsyncModem::shutdown`] has returned — by then the
    /// modem loop has released the adapter and the pump's write
    /// channel will close as soon as the adapter is dropped... but
    /// we own the adapter here, so closing happens via explicit drop
    /// of `write_tx` below.
    ///
    /// # Errors
    ///
    /// Returns [`io::Error`] if:
    /// - The pump task panicked (join fails).
    /// - The pump exited because of a transport error before we asked
    ///   it to shut down.
    pub async fn into_inner(mut self) -> io::Result<T> {
        // Drop the write sender; this closes the write channel,
        // causing the pump task to break out of its loop and return
        // the transport.
        drop(std::mem::replace(
            &mut self.write_tx,
            mpsc::channel(1).0, // placeholder, never used
        ));
        // Also drop the read receiver so the pump doesn't block
        // trying to send a final read result on shutdown.
        let _ = std::mem::replace(&mut self.read_rx, mpsc::channel(1).1);

        let pump = self
            .pump
            .take()
            .ok_or_else(|| io::Error::other("MmdvmTransportAdapter: pump already joined"))?;
        match pump.await {
            Ok(transport) => Ok(transport),
            Err(join_err) => Err(io::Error::other(format!(
                "MmdvmTransportAdapter: pump task panicked: {join_err}"
            ))),
        }
    }
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
/// On any exit path the transport is returned so
/// [`MmdvmTransportAdapter::into_inner`] can recover it for the next
/// session (TX re-entry into CAT mode, etc.).
async fn pump_task<T: Transport>(
    mut transport: T,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    read_tx: mpsc::Sender<io::Result<Vec<u8>>>,
) -> T {
    let mut scratch = [0u8; READ_CHUNK_SIZE];
    loop {
        tokio::select! {
            biased;

            maybe_write = write_rx.recv() => {
                let Some(data) = maybe_write else {
                    tracing::debug!(
                        target: "kenwood_thd75::transport::mmdvm_adapter",
                        "write channel closed; pump task exiting"
                    );
                    return transport;
                };
                tracing::trace!(
                    target: "mmdvm::hang_hunt",
                    len = data.len(),
                    "pump: write branch — calling transport.write"
                );
                if let Err(e) = transport.write(&data).await {
                    tracing::warn!(
                        target: "kenwood_thd75::transport::mmdvm_adapter",
                        error = %e,
                        "transport write failed; pump task exiting"
                    );
                    let _ = read_tx.send(Err(transport_err_to_io(e))).await;
                    return transport;
                }
                tracing::trace!(target: "mmdvm::hang_hunt", "pump: transport.write returned");
            }

            read_result = transport.read(&mut scratch) => {
                match read_result {
                    Ok(0) => {
                        tracing::debug!(
                            target: "kenwood_thd75::transport::mmdvm_adapter",
                            "transport read returned EOF; pump exiting"
                        );
                        let _ = read_tx
                            .send(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "transport EOF",
                            )))
                            .await;
                        return transport;
                    }
                    Ok(n) => {
                        let Some(slice) = scratch.get(..n) else {
                            tracing::warn!(
                                target: "kenwood_thd75::transport::mmdvm_adapter",
                                got = n,
                                cap = READ_CHUNK_SIZE,
                                "transport read reported impossible length; dropping"
                            );
                            continue;
                        };
                        let bytes = slice.to_vec();
                        tracing::trace!(
                            target: "mmdvm::hang_hunt",
                            len = bytes.len(),
                            cap_remaining = read_tx.capacity(),
                            "pump: read branch — awaiting read_tx.send"
                        );
                        if read_tx.send(Ok(bytes)).await.is_err() {
                            tracing::debug!(
                                target: "kenwood_thd75::transport::mmdvm_adapter",
                                "read consumer closed; pump exiting"
                            );
                            return transport;
                        }
                        tracing::trace!(target: "mmdvm::hang_hunt", "pump: read_tx.send done");
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "kenwood_thd75::transport::mmdvm_adapter",
                            error = %e,
                            "transport read failed; pump exiting"
                        );
                        let _ = read_tx.send(Err(transport_err_to_io(e))).await;
                        return transport;
                    }
                }
            }
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

        // First drain anything left over from a previous oversize read.
        if !this.leftover.is_empty() {
            let take = this.leftover.len().min(buf.remaining());
            let drained: Vec<u8> = this.leftover.drain(..take).collect();
            buf.put_slice(&drained);
            return Poll::Ready(Ok(()));
        }

        match this.read_rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                // Pump exited. Treat as EOF so tokio's higher-level
                // readers stop cleanly.
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(Some(Ok(bytes))) => {
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
        let data = buf.to_vec();
        match this.write_tx.try_send(data) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // Pump task is briefly busy. Wake ourselves and
                // retry shortly; the pump drains write_rx eagerly.
                // A sustained hang-hunt trace here (many "Full" in
                // a row with no matching pump write progress) means
                // the pump task is wedged in FFI. The log is
                // rate-limited by the spin itself — every retry
                // emits one line, so "hundreds of Full in a millisecond"
                // is the signal, not the volume.
                tracing::trace!(
                    target: "mmdvm::hang_hunt",
                    cap_remaining = this.write_tx.capacity(),
                    "adapter.poll_write: write_tx FULL (will wake-retry)"
                );
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MmdvmTransportAdapter: pump task exited",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // [`crate::Transport::write`] flushes synchronously and the
        // pump task writes eagerly — once `poll_write` accepts a
        // buffer, the pump will drain it on its next loop turn and
        // call [`Transport::write`] which itself flushes. No explicit
        // flush is needed here.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Shutdown is driven by dropping the adapter (which closes
        // `write_tx` and terminates the pump). Returning `Ok` lets
        // tokio's shutdown-on-drop path complete cleanly.
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::task::LocalSet;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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
    async fn into_inner_recovers_transport() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mut mock = MockTransport::new();
                mock.expect(b"X", b"Y");
                let mut adapter = MmdvmTransportAdapter::new(mock);
                adapter.write_all(b"X").await?;
                let mut buf = [0u8; 1];
                let n = adapter.read(&mut buf).await?;
                assert_eq!(n, 1);

                let recovered = adapter.into_inner().await?;
                drop(recovered);
                Ok(())
            })
            .await
    }

    #[tokio::test]
    async fn into_inner_without_io_succeeds() -> TestResult {
        LocalSet::new()
            .run_until(async {
                let mock = MockTransport::new();
                let adapter = MmdvmTransportAdapter::new(mock);
                let _mock = adapter.into_inner().await?;
                Ok(())
            })
            .await
    }
}
