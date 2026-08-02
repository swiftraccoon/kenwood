//! Adapter from Swift's USB byte stream to the TH-D75 transport trait.

use std::sync::Arc;

use async_trait::async_trait;
use kenwood_thd75::error::TransportError;
use kenwood_thd75::transport::Transport;

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

    fn read_error(error: &ByteTransportError) -> TransportError {
        TransportError::Read(std::io::Error::other(error.to_string()))
    }

    fn write_error(error: &ByteTransportError) -> TransportError {
        TransportError::Write(std::io::Error::other(error.to_string()))
    }

    fn connection_error(error: &ByteTransportError) -> TransportError {
        TransportError::Disconnected(std::io::Error::other(error.to_string()))
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
        self.inner
            .write(data.to_vec())
            .await
            .map_err(|error| Self::write_error(&error))
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let maximum = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
        let bytes = self
            .inner
            .read(maximum)
            .await
            .map_err(|error| Self::read_error(&error))?;
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
        self.inner
            .close()
            .await
            .map_err(|error| Self::connection_error(&error))
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.inner
            .set_baud_rate(baud)
            .map_err(|error| TransportError::Open {
                path: "Swift USB transport".to_owned(),
                source: std::io::Error::other(error.to_string()),
            })
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        self.inner
            .reopen()
            .await
            .map_err(|error| Self::connection_error(&error))
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
}
