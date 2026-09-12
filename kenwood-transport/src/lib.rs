#![doc = include_str!("../README.md")]

pub mod error;
pub mod mock;
#[cfg(feature = "serial")]
pub mod serial;
pub mod stream;

use std::future::Future;

pub use error::TransportError;
pub use mock::MockTransport;
#[cfg(feature = "serial")]
pub use serial::SerialTransport;
pub use stream::{StreamAdapter, StreamRecoveryError};

/// An asynchronous, exclusively owned byte connection.
///
/// Implementations provide I/O and connection ownership, not protocol framing,
/// device identification, operating-mode transitions, or retry admission.
pub trait Transport: Send + Sync {
    /// Send all supplied bytes to the connected endpoint.
    ///
    /// # Cancellation safety
    ///
    /// A dropped future may leave a transmitted prefix. Callers must not
    /// infer that cancellation prevented a write or that retrying is safe.
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Read available bytes into `buf`, returning the initialized byte count.
    ///
    /// # Cancellation safety
    ///
    /// This future must be cancellation-safe: if it is dropped while pending,
    /// the next call must still be able to deliver every byte that was not
    /// returned to the caller. [`StreamAdapter`] deliberately races a pending
    /// read against outbound work so one blocked read cannot prevent a write;
    /// that race cancels and recreates the losing read future.
    ///
    /// Returning a count larger than `buf.len()` violates this trait's
    /// contract. Consumers treat that as terminal transport corruption rather
    /// than indexing beyond the initialized region.
    fn read(
        &mut self,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<usize, TransportError>> + Send;

    /// Close the connection and release its active resources.
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Change the transport baud rate.
    ///
    /// The caller supplies the protocol's required rate. Transports without a
    /// configurable baud rate may use this default no-op implementation.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] if the baud rate cannot be applied.
    fn set_baud_rate(&mut self, _baud: u32) -> Result<(), TransportError> {
        Ok(())
    }

    /// Re-establish a dropped connection using its original identity.
    ///
    /// Implementations own their platform's full recovery sequence,
    /// including any mandatory release/settle delays. The default declines:
    /// transports that cannot recover their own connection report
    /// [`TransportError::ReopenUnsupported`], and the caller must build a fresh
    /// transport instead. This operation does not establish protocol readiness
    /// or authorize device-specific mode changes.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::ReopenUnsupported`] if this transport cannot
    /// reopen; implementation-specific errors otherwise.
    fn reopen(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send {
        async { Err(TransportError::ReopenUnsupported) }
    }
}
