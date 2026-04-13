//! Transport abstraction for the MMDVM async shell.
//!
//! Consumers provide any type that implements both [`AsyncRead`] and
//! [`AsyncWrite`] — the shell takes ownership and drives it from the
//! internal modem loop. Typical concrete types:
//!
//! - `tokio::io::DuplexStream` for unit tests
//! - `tokio_serial::SerialStream` for USB-CDC (Kenwood TH-D75)
//! - `thd75::transport::EitherTransport` for the USB/Bluetooth SPP
//!   auto-selection used by the TH-D75 crates

use tokio::io::{AsyncRead, AsyncWrite};

/// Async bidirectional byte stream for talking to an MMDVM modem.
///
/// Blanket-implemented for any type that is both [`AsyncRead`] +
/// [`AsyncWrite`] + [`Send`] + [`Unpin`]. See [`mmdvm_core`] for the
/// wire-format specifics the bytes on this stream must follow.
///
/// [`mmdvm_core`]: crate::core
pub trait Transport: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> Transport for T {}
