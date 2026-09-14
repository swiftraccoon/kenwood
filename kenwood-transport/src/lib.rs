#![doc = include_str!("../README.md")]

#[cfg(feature = "native-bluetooth")]
pub mod bluetooth;
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
/// A successful operation proves only its backend's documented completion
/// boundary. It is not peer receipt, command acceptance, or protocol readiness.
///
/// The trait imposes no common I/O deadline. Callers own protocol deadlines,
/// preserve operation and cleanup errors separately, and consult the concrete
/// backend before canceling an operation or treating a closed owner as retired.
pub trait Transport: Send + Sync {
    /// Submit all supplied bytes according to the backend's completion contract.
    ///
    /// Serial completes its write and stream flush, without independent
    /// hardware-drain evidence. Native Bluetooth completes a write to the
    /// helper's stdin pipe, not an RFCOMM write-completion callback. The mock
    /// matches one scripted write and queues its response. [`StreamAdapter`]
    /// preserves these boundaries; its flush does not strengthen them.
    ///
    /// Empty writes are backend-specific: they can flush a serial stream,
    /// consume a mock expectation, or be a native no-op. They are not portable
    /// connection-health checks.
    ///
    /// # Cancellation safety
    ///
    /// A dropped future may leave a transmitted prefix. Callers must not
    /// infer that cancellation prevented a write or that retrying is safe.
    /// Canceling a pending native Bluetooth write also invalidates its helper;
    /// that connection must not be reused for another exchange.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Write`] for a backend write failure, or an
    /// implementation-specific connection error. An error does not identify
    /// how many bytes, if any, reached the peer.
    fn write(&mut self, data: &[u8]) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Read available bytes into `buf`, returning the initialized byte count.
    ///
    /// A short read is normal and does not delimit a protocol message. For a
    /// nonempty buffer, `Ok(0)` represents EOF; a backend may instead report
    /// EOF as an error. Serial forwards stream EOF, native Bluetooth reports
    /// [`TransportError::Read`] with [`std::io::ErrorKind::UnexpectedEof`],
    /// and the mock returns its explicitly scripted EOF. [`StreamAdapter`]
    /// treats either form as terminal.
    ///
    /// An empty buffer cannot establish EOF or connection health. Native
    /// Bluetooth returns zero immediately; serial can still reject a closed
    /// descriptor, and a mock still follows its pending read script. Use a
    /// nonempty buffer for actual I/O. Serial and native reads have no intrinsic
    /// deadline. An empty mock script returns `WouldBlock` unless configured
    /// to remain pending; scripted hangs require a caller-owned timeout.
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
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Read`] for a backend read failure, or an
    /// implementation-specific connection error. Previously returned bytes
    /// remain the caller's responsibility after an error.
    fn read(
        &mut self,
        buf: &mut [u8],
    ) -> impl Future<Output = Result<usize, TransportError>> + Send;

    /// Attempt to close the connection and report the backend's cleanup result.
    ///
    /// Closing does not flush a protocol exchange, restore a device mode, or
    /// prove that the operating system canceled an in-flight native operation.
    /// Preserve this result independently of any preceding I/O error.
    ///
    /// # Cancellation and execution
    ///
    /// There is no trait-wide deadline or cancellation guarantee. Serial takes
    /// its descriptor before awaiting shutdown, so cancellation still leaves
    /// it closed. Native Bluetooth performs bounded synchronous helper cleanup
    /// in one poll; an async timeout cannot preempt that poll. Its Drop path
    /// can block similarly. Mock close only clears pending responses and
    /// retains future scripted exchanges; it models no physical resource.
    ///
    /// Repeated serial/mock closes succeed. Native close retains its first
    /// cleanup outcome, including failure; a second call cannot upgrade that
    /// evidence to clean release.
    ///
    /// # Errors
    ///
    /// Serial shutdown reports [`TransportError::Disconnected`]; native cleanup
    /// reports [`TransportError::BluetoothClose`]. Other implementations may
    /// provide their own connection errors. A failed close is not proof that
    /// every resource remains live or that every resource was cleanly released.
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// Change the transport baud rate.
    ///
    /// The caller supplies the protocol's required rate. Transports without a
    /// configurable baud rate may use this default no-op implementation.
    ///
    /// # Errors
    ///
    /// The serial backend returns [`TransportError::Open`] if the baud rate
    /// is zero or cannot be applied, and [`TransportError::Disconnected`] if
    /// its descriptor is closed. The default implementation always succeeds;
    /// other implementations define their own errors.
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
