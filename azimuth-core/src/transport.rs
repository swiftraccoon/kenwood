//! Adapter from Swift's USB byte stream to the TH-D75 transport trait.

use std::num::NonZeroU32;
use std::sync::Arc;

use async_trait::async_trait;
use kenwood_transport::{Transport, TransportError};

/// Error returned by the Swift USB implementation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum ByteTransportError {
    /// A platform USB operation failed.
    #[error("{message}")]
    Platform {
        /// Human-readable platform error with enough detail for diagnostics.
        message: String,
    },
}

/// Async byte transport implemented by the iPadOS or macOS app.
///
/// `read` must wait until bytes, EOF, or an error is available. Returning an
/// empty vector means the device disconnected; it must not mean "no bytes
/// yet." The returned vector must contain at most `max_length` bytes.
///
/// Writes complete only after accepting every byte, preserving order. Failure
/// or cancellation may follow a written prefix; the protocol caller must
/// retire that uncertain connection before another operation. Do not retry a
/// prefix implicitly. Reads must retain buffered bytes between calls and must
/// not poll an empty live stream by returning an empty vector.
///
/// The protocol layer supplies operation deadlines and may abandon a callback
/// future. Implementations must keep native resources alive until an operation
/// stops, and make `close` release them and wake parked reads. `reopen` closes
/// the preceding connection, opens the same selected physical endpoint and
/// propagates failure; it must never select another device. Empty writes and
/// zero-length reads are no-ops. Implementations must serialize their native
/// I/O safely and make `close` idempotent.
#[uniffi::export(with_foreign)]
#[async_trait]
pub trait ByteTransport: Send + Sync + std::fmt::Debug {
    /// Write every byte to the radio's USB CDC stream.
    async fn write(&self, bytes: Vec<u8>) -> Result<(), ByteTransportError>;

    /// Read no more than `max_length` bytes from the USB CDC stream.
    async fn read(&self, max_length: u32) -> Result<Vec<u8>, ByteTransportError>;

    /// Close the current USB connection.
    async fn close(&self) -> Result<(), ByteTransportError>;

    /// Reopen the same USB device after the radio re-enumerates.
    async fn reopen(&self) -> Result<(), ByteTransportError>;

    /// Set the CDC baud rate used by the current radio mode.
    ///
    /// # Errors
    ///
    /// Returns a platform error if the active USB connection cannot apply the
    /// requested line coding.
    fn set_baud_rate(&self, baud: u32) -> Result<(), ByteTransportError>;
}

/// TH-D75 transport backed by a foreign Swift callback object.
pub(crate) struct SwiftByteTransport {
    inner: Arc<dyn ByteTransport>,
}

impl SwiftByteTransport {
    pub(crate) const fn new(inner: Arc<dyn ByteTransport>) -> Self {
        Self { inner }
    }

    fn read_error(error: ByteTransportError) -> TransportError {
        TransportError::Read(std::io::Error::other(error))
    }

    fn write_error(error: ByteTransportError) -> TransportError {
        TransportError::Write(std::io::Error::other(error))
    }

    fn connection_error(error: ByteTransportError) -> TransportError {
        TransportError::Disconnected(std::io::Error::other(error))
    }
}

impl std::fmt::Debug for SwiftByteTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SwiftByteTransport")
            .finish_non_exhaustive()
    }
}

impl Transport for SwiftByteTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        if data.is_empty() {
            return Ok(());
        }
        self.inner
            .write(data.to_vec())
            .await
            .map_err(Self::write_error)
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let maximum = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let bytes = self.inner.read(maximum).await.map_err(Self::read_error)?;
        if bytes.len() > buffer.len() {
            return Err(TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Swift USB read returned {} bytes for a maximum of {}",
                    bytes.len(),
                    buffer.len()
                ),
            )));
        }
        let destination = buffer.get_mut(..bytes.len()).ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Swift USB read length did not fit the destination buffer",
            ))
        })?;
        destination.copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await.map_err(Self::connection_error)
    }

    fn set_baud_rate(&mut self, baud: NonZeroU32) -> Result<(), TransportError> {
        self.inner
            .set_baud_rate(baud.get())
            .map_err(|error| TransportError::Open {
                path: "Swift USB transport".to_owned(),
                source: std::io::Error::other(error),
            })
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        self.inner.reopen().await.map_err(Self::connection_error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[derive(Debug)]
    struct TestTransport {
        reads: Mutex<VecDeque<Vec<u8>>>,
        writes: Mutex<Vec<Vec<u8>>>,
    }

    impl TestTransport {
        fn new(reads: Vec<Vec<u8>>) -> Self {
            Self {
                reads: Mutex::new(reads.into()),
                writes: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ByteTransport for TestTransport {
        async fn write(&self, bytes: Vec<u8>) -> Result<(), ByteTransportError> {
            self.writes
                .lock()
                .map_err(|error| ByteTransportError::Platform {
                    message: error.to_string(),
                })?
                .push(bytes);
            Ok(())
        }

        async fn read(&self, _max_length: u32) -> Result<Vec<u8>, ByteTransportError> {
            Ok(self
                .reads
                .lock()
                .map_err(|error| ByteTransportError::Platform {
                    message: error.to_string(),
                })?
                .pop_front()
                .unwrap_or_default())
        }

        async fn close(&self) -> Result<(), ByteTransportError> {
            Ok(())
        }

        async fn reopen(&self) -> Result<(), ByteTransportError> {
            Ok(())
        }

        fn set_baud_rate(&self, _baud: u32) -> Result<(), ByteTransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn adapter_copies_bounded_reads_and_writes() -> TestResult {
        let foreign = Arc::new(TestTransport::new(vec![b"ID TH-D75\r".to_vec()]));
        let mut adapter = SwiftByteTransport::new(foreign.clone());
        adapter.write(b"ID\r").await?;
        let mut buffer = [0_u8; 32];
        let count = adapter.read(&mut buffer).await?;

        assert_eq!(buffer.get(..count), Some(b"ID TH-D75\r".as_slice()));
        let writes = foreign
            .writes
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        assert_eq!(writes.as_slice(), &[b"ID\r".to_vec()]);
        Ok(())
    }

    #[tokio::test]
    async fn adapter_rejects_foreign_overflow() {
        let foreign = Arc::new(TestTransport::new(vec![vec![0_u8; 5]]));
        let mut adapter = SwiftByteTransport::new(foreign);
        let mut buffer = [0_u8; 4];
        let result = adapter.read(&mut buffer).await;

        assert!(
            matches!(result, Err(TransportError::Read(_))),
            "oversized foreign reads must fail: {result:?}"
        );
    }

    #[tokio::test]
    async fn empty_operations_do_not_invoke_the_foreign_connection() -> TestResult {
        let foreign = Arc::new(TestTransport::new(vec![b"ID TH-D75\r".to_vec()]));
        let mut adapter = SwiftByteTransport::new(foreign.clone());
        adapter.write(&[]).await?;
        assert_eq!(adapter.read(&mut []).await?, 0);
        assert!(
            foreign
                .writes
                .lock()
                .map_err(|error| error.to_string())?
                .is_empty(),
            "empty write invoked the foreign callback"
        );
        let mut buffer = [0; 32];
        let count = adapter.read(&mut buffer).await?;
        assert_eq!(
            buffer.get(..count),
            Some(b"ID TH-D75\r".as_slice()),
            "empty read consumed pending bytes"
        );
        Ok(())
    }

    #[test]
    fn platform_failure_retains_the_typed_foreign_cause() -> TestResult {
        let original = ByteTransportError::Platform {
            message: "injected USB failure".to_owned(),
        };
        let failure = SwiftByteTransport::write_error(original.clone());
        let TransportError::Write(error) = failure else {
            return Err("expected write failure".into());
        };
        assert_eq!(
            error
                .get_ref()
                .and_then(|cause| cause.downcast_ref::<ByteTransportError>()),
            Some(&original)
        );
        Ok(())
    }
}
