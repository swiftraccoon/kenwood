//! Mock transport for testing without real hardware.

use std::collections::VecDeque;
use std::path::Path;

use crate::error::TransportError;

use super::Transport;

/// Mock transport for testing. Programs expected command/response exchanges.
#[derive(Debug)]
pub struct MockTransport {
    exchanges: VecDeque<(Vec<u8>, Vec<u8>)>,
    pending_response: Option<Vec<u8>>,
    accept_any_write: bool,
}

impl MockTransport {
    /// Create a new empty mock transport with no expected exchanges.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            exchanges: VecDeque::new(),
            pending_response: None,
            accept_any_write: false,
        }
    }

    /// Queue an expected command/response exchange.
    ///
    /// When `write()` is called with `command`, the corresponding `response`
    /// will be returned by the next `read()`.
    pub fn expect(&mut self, command: &[u8], response: &[u8]) {
        self.exchanges
            .push_back((command.to_vec(), response.to_vec()));
    }

    /// Load expected exchanges from a fixture file.
    ///
    /// The file format uses `> ` prefixed lines for commands and `< ` prefixed
    /// lines for responses. Literal `\r` sequences are converted to `0x0D`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read.
    pub fn from_fixture(path: &Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let mut mock = Self::new();
        let mut current_command: Option<Vec<u8>> = None;

        for line in content.lines() {
            if let Some(cmd) = line.strip_prefix("> ") {
                let bytes = cmd.replace("\\r", "\r").into_bytes();
                current_command = Some(bytes);
            } else if let Some(resp) = line.strip_prefix("< ") {
                let bytes = resp.replace("\\r", "\r").into_bytes();
                if let Some(cmd) = current_command.take() {
                    mock.exchanges.push_back((cmd, bytes));
                }
            }
        }

        Ok(mock)
    }

    /// Queue data to be returned by the next `read()` without requiring
    /// a preceding `write()`.
    ///
    /// This is useful for testing unsolicited incoming data (e.g., MMDVM
    /// frames received from the radio without a prior command).
    pub fn queue_read(&mut self, data: &[u8]) {
        self.pending_response = Some(data.to_vec());
    }

    /// Accept any subsequent `write()` calls without validation.
    ///
    /// When enabled, writes succeed without checking against expected
    /// exchanges and no response is queued.
    pub const fn expect_any_write(&mut self) {
        self.accept_any_write = true;
    }

    /// Panic if any expected exchanges remain unconsumed.
    ///
    /// # Panics
    ///
    /// Panics if there are remaining exchanges that were not exercised.
    pub fn assert_complete(&self) {
        assert!(
            self.exchanges.is_empty(),
            "MockTransport has {} unconsumed exchange(s)",
            self.exchanges.len()
        );
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for MockTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        tracing::debug!(bytes = data.len(), "mock: write");

        if self.accept_any_write && self.exchanges.is_empty() {
            tracing::debug!("mock: accepting any write (no response queued)");
            return Ok(());
        }

        let (expected_cmd, response) = self.exchanges.pop_front().ok_or_else(|| {
            TransportError::Write(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "no more expected exchanges, but got write: {:?}",
                    String::from_utf8_lossy(data)
                ),
            ))
        })?;

        if data != expected_cmd {
            return Err(TransportError::Write(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "expected command {:?}, got {:?}",
                    String::from_utf8_lossy(&expected_cmd),
                    String::from_utf8_lossy(data)
                ),
            )));
        }

        tracing::debug!(bytes = response.len(), "mock: read response queued");
        self.pending_response = Some(response);
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let response = self.pending_response.take().ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no pending response — call write() first",
            ))
        })?;

        let len = response.len().min(buf.len());
        buf[..len].copy_from_slice(&response[..len]);
        tracing::debug!(bytes = len, "mock: read");
        Ok(len)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::debug!("mock: closing transport");
        self.exchanges.clear();
        self.pending_response = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_exchange() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.write(b"ID\r").await.unwrap();
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ID TH-D75\r");
        mock.assert_complete();
    }

    #[tokio::test]
    async fn unexpected_command() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let result = mock.write(b"FV\r").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn multiple_exchanges() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03.000\r");

        mock.write(b"ID\r").await.unwrap();
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ID TH-D75\r");

        mock.write(b"FV\r").await.unwrap();
        let n = mock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"FV 1.03.000\r");

        mock.assert_complete();
    }

    #[tokio::test]
    async fn from_fixture_file() {
        let mut mock =
            MockTransport::from_fixture(Path::new("tests/fixtures/identify.txt")).unwrap();

        mock.write(b"ID\r").await.unwrap();
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"ID TH-D75\r");

        mock.write(b"FV\r").await.unwrap();
        let n = mock.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"FV 1.03.000\r");

        mock.assert_complete();
    }

    #[tokio::test]
    async fn read_without_write_errors() {
        let mut mock = MockTransport::new();
        let mut buf = [0u8; 64];
        let result = mock.read(&mut buf).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_with_no_exchanges_errors() {
        let mut mock = MockTransport::new();
        let result = mock.write(b"ID\r").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn default_creates_empty() {
        let mock = MockTransport::default();
        mock.assert_complete();
    }

    #[tokio::test]
    async fn close_clears_state() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.close().await.unwrap();
        mock.assert_complete();
    }
}
