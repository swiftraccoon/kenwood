//! Transport abstraction for the MMDVM async shell.
//!
//! Consumers provide any type that implements both [`AsyncRead`] and
//! [`AsyncWrite`]. The shell takes ownership and drives it from the
//! internal modem loop. A `tokio::io::DuplexStream` supplies an in-memory
//! stream; `kenwood_transport::StreamAdapter` adapts an already-open transport.

use tokio::io::{AsyncRead, AsyncWrite};

/// Async bidirectional byte stream for talking to an MMDVM modem.
///
/// Blanket-implemented for any type that is both [`AsyncRead`] +
/// [`AsyncWrite`] + [`Send`] + [`Unpin`]. See [`mmdvm_core`] for the
/// wire-format specifics the bytes on this stream must follow.
/// Implementers provide the inherited Tokio read, write and flush contracts.
/// This marker is object-safe and remains open to downstream stream types;
/// shared access through `Sync` is not required.
/// See the [runtime example](crate#drive-and-reclaim-a-modem).
///
/// [`mmdvm_core`]: crate::core
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Transport for T {}
