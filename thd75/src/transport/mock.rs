//! Mock transport for testing without real hardware.

use std::collections::VecDeque;
use std::path::Path;

use crate::error::TransportError;

use super::Transport;

/// A scripted outcome for a single [`MockTransport::read`] call.
#[derive(Debug, Clone)]
enum MockRead {
    /// Deliver these bytes.
    Data(Vec<u8>),
    /// Sleep for the given milliseconds, then deliver these bytes.
    /// Models a response with real wire latency (needed when the
    /// consumer correlates a response to a write it must make first).
    Delayed(Vec<u8>, u64),
    /// Return `Ok(0)`: transport EOF (device unplugged / port closed).
    Eof,
    /// Never resolve: a wedged link. Pair with a timeout in the test.
    Hang,
}

/// Mock transport for testing. Programs expected command/response exchanges.
#[derive(Debug)]
pub struct MockTransport {
    exchanges: VecDeque<(Vec<u8>, Vec<MockRead>)>,
    pending: VecDeque<MockRead>,
    accept_any_write: bool,
    pend_when_empty: bool,
    /// When the front `Delayed` entry started waiting. Persists
    /// across cancelled read futures so the delay makes progress even
    /// if the consumer keeps cancelling reads (e.g. a biased select
    /// with a busy write branch).
    delay_started: Option<tokio::time::Instant>,
    /// Scripted outcomes for [`Transport::reopen`] calls, consumed
    /// FIFO. An empty queue means reopen succeeds.
    reopen_script: VecDeque<Result<(), TransportError>>,
}

impl MockTransport {
    /// Create a new empty mock transport with no expected exchanges.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            exchanges: VecDeque::new(),
            pending: VecDeque::new(),
            accept_any_write: false,
            pend_when_empty: false,
            delay_started: None,
            reopen_script: VecDeque::new(),
        }
    }

    /// Queue the outcome of the next [`Transport::reopen`] call.
    ///
    /// Outcomes are consumed FIFO; when the queue is empty, `reopen`
    /// succeeds. Lets tests model reopen-fails-then-succeeds flows.
    pub fn expect_reopen(&mut self, outcome: Result<(), TransportError>) {
        self.reopen_script.push_back(outcome);
    }

    /// Queue an expected command/response exchange.
    ///
    /// When `write()` is called with `command`, the corresponding `response`
    /// will be returned by the next `read()`. An empty `response` means
    /// "expect the write, queue nothing" (use [`Self::expect_eof`] to
    /// model a disconnect instead).
    pub fn expect(&mut self, command: &[u8], response: &[u8]) {
        let reads = if response.is_empty() {
            Vec::new()
        } else {
            vec![MockRead::Data(response.to_vec())]
        };
        self.exchanges.push_back((command.to_vec(), reads));
    }

    /// Queue an expected command followed by several read chunks,
    /// delivered one per `read()` call.
    ///
    /// Models interleaved traffic: unsolicited frames (AI pushes, NMEA
    /// sentences) arriving on the stream before the real response.
    pub fn expect_reads(&mut self, command: &[u8], responses: &[&[u8]]) {
        let reads = responses
            .iter()
            .map(|r| MockRead::Data(r.to_vec()))
            .collect();
        self.exchanges.push_back((command.to_vec(), reads));
    }

    /// Queue an expected command whose next `read()` reports EOF
    /// (`Ok(0)`), i.e. the device disappeared mid-command.
    pub fn expect_eof(&mut self, command: &[u8]) {
        self.exchanges
            .push_back((command.to_vec(), vec![MockRead::Eof]));
    }

    /// Queue an expected command whose `read()` never resolves, i.e. a
    /// wedged link. The caller's timeout machinery must fire.
    pub fn expect_hang(&mut self, command: &[u8]) {
        self.exchanges
            .push_back((command.to_vec(), vec![MockRead::Hang]));
    }

    /// Queue an expected command whose response begins with `partial` and
    /// then never completes.
    ///
    /// Models a truncated frame on a live but wedged transport.
    pub fn expect_partial_then_hang(&mut self, command: &[u8], partial: &[u8]) {
        self.exchanges.push_back((
            command.to_vec(),
            vec![MockRead::Data(partial.to_vec()), MockRead::Hang],
        ));
    }

    /// Queue an expected command whose response begins, wedges, and then
    /// leaves `late` bytes queued after the blocked read is cancelled.
    ///
    /// Models a truncated exchange whose delayed tail can be mistaken for a
    /// later command's response.
    pub fn expect_partial_then_hang_with_late(
        &mut self,
        command: &[u8],
        partial: &[u8],
        late: &[u8],
    ) {
        self.exchanges.push_back((
            command.to_vec(),
            vec![
                MockRead::Data(partial.to_vec()),
                MockRead::Hang,
                MockRead::Data(late.to_vec()),
            ],
        ));
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
                    mock.exchanges.push_back((cmd, vec![MockRead::Data(bytes)]));
                }
            }
        }

        Ok(mock)
    }

    /// Queue data to be returned by a subsequent `read()` without
    /// requiring a preceding `write()`.
    ///
    /// Useful for unsolicited incoming data (AI pushes, MMDVM frames,
    /// stale late responses). Multiple calls queue multiple chunks,
    /// delivered one per `read()` in call order.
    pub fn queue_read(&mut self, data: &[u8]) {
        self.pending.push_back(MockRead::Data(data.to_vec()));
    }

    /// Queue data delivered by a subsequent `read()` only after the
    /// given delay. Models wire latency so the consumer can perform
    /// the write this data responds to before it arrives.
    pub fn queue_read_delayed(&mut self, data: &[u8], delay_ms: u64) {
        self.pending
            .push_back(MockRead::Delayed(data.to_vec(), delay_ms));
    }

    /// Accept otherwise-unscripted subsequent `write()` calls without
    /// validation.
    ///
    /// A write that exactly matches the next scripted exchange still consumes
    /// that exchange and queues its response. Any other write succeeds without
    /// consuming the script or queuing data. This permits tests to ignore a
    /// variable number of write-only frames while retaining a later exact
    /// request/response boundary.
    pub const fn expect_any_write(&mut self) {
        self.accept_any_write = true;
    }

    /// Make `read()` pend forever (like an idle serial port) instead
    /// of failing with `WouldBlock` when no scripted read is queued.
    ///
    /// Required for consumers with an always-reading pump task (the
    /// MMDVM adapter) that treats a read error as a dead transport.
    /// The whole read script must be queued up front, so use
    /// [`Self::queue_read_delayed`] to sequence responses after the
    /// writes they answer.
    pub const fn pend_when_empty(&mut self) {
        self.pend_when_empty = true;
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

    /// Panic if any scripted reopen outcomes remain unconsumed.
    ///
    /// # Panics
    ///
    /// Panics when the code under test did not perform every expected reopen.
    pub fn assert_reopen_script_complete(&self) {
        assert!(
            self.reopen_script.is_empty(),
            "MockTransport has {} unconsumed reopen outcome(s)",
            self.reopen_script.len()
        );
    }

    /// Copy one transport-sized prefix and preserve the unread suffix for the
    /// next read, matching stream-oriented serial and RFCOMM transports.
    fn deliver_data(&mut self, data: &[u8], buf: &mut [u8]) -> Result<usize, TransportError> {
        let len = data.len().min(buf.len());
        let source = data.get(..len).ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mock response prefix exceeded its source buffer",
            ))
        })?;
        let target = buf.get_mut(..len).ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "mock response prefix exceeded the caller's read buffer",
            ))
        })?;
        target.copy_from_slice(source);

        if let Some(remainder) = data.get(len..).filter(|remainder| !remainder.is_empty()) {
            self.pending.push_front(MockRead::Data(remainder.to_vec()));
        }
        Ok(len)
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

        if self.accept_any_write {
            let matches_next_exchange = self
                .exchanges
                .front()
                .is_some_and(|(expected, _)| expected == data);
            if !matches_next_exchange {
                tracing::debug!("mock: accepting unscripted write (no response queued)");
                return Ok(());
            }
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

        tracing::debug!(reads = response.len(), "mock: read outcomes queued");
        self.pending.extend(response);
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.pending.is_empty() && self.pend_when_empty {
            // Nothing can enqueue more data once the consumer owns
            // this transport, so pend like an idle line until the test
            // tears the task down.
            tracing::debug!("mock: read pending forever (script exhausted)");
            std::future::pending::<()>().await;
        }
        // Cancellation safety: consumers may drop this read future
        // mid-await (e.g. a select! with a busy write branch). Sleep
        // for a Delayed entry BEFORE popping it, and measure the
        // delay from its first attempt so cancelled reads still make
        // progress toward delivery.
        if let Some(MockRead::Delayed(_, delay_ms)) = self.pending.front() {
            let delay = std::time::Duration::from_millis(*delay_ms);
            let started = *self
                .delay_started
                .get_or_insert_with(tokio::time::Instant::now);
            let deadline = started + delay;
            tokio::time::sleep_until(deadline).await;
            self.delay_started = None;
            if let Some(MockRead::Delayed(data, _)) = self.pending.pop_front() {
                let len = self.deliver_data(&data, buf)?;
                tracing::debug!(bytes = len, "mock: delayed read");
                return Ok(len);
            }
            // Unreachable: front was Delayed and nothing else pops.
            return Ok(0);
        }

        let outcome = self.pending.pop_front().ok_or_else(|| {
            TransportError::Read(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no pending response; call write() first",
            ))
        })?;

        match outcome {
            MockRead::Data(response) => {
                let len = self.deliver_data(&response, buf)?;
                tracing::debug!(bytes = len, "mock: read");
                Ok(len)
            }
            MockRead::Delayed(..) => {
                // Handled above via the peek path; unreachable here.
                Ok(0)
            }
            MockRead::Eof => {
                tracing::debug!("mock: read EOF");
                Ok(0)
            }
            MockRead::Hang => {
                tracing::debug!("mock: read hanging forever");
                std::future::pending().await
            }
        }
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        tracing::debug!("mock: closing transport");
        // In-flight response bytes die with the connection, but
        // scripted future exchanges survive: they model traffic the
        // test expects after a reopen (reconnect flows close first).
        self.pending.clear();
        Ok(())
    }

    async fn reopen(&mut self) -> Result<(), TransportError> {
        tracing::debug!("mock: reopen");
        self.reopen_script.pop_front().unwrap_or(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    /// Return `&buf[..n]` or an error if `n` exceeds buffer length.
    fn read_prefix(buf: &[u8], n: usize) -> Result<&[u8], BoxErr> {
        buf.get(..n)
            .ok_or_else(|| format!("read_prefix: len {n} exceeds buffer len {}", buf.len()).into())
    }

    #[tokio::test]
    async fn basic_exchange() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.write(b"ID\r").await?;
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"ID TH-D75\r");
        mock.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn unexpected_command() {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        let result = mock.write(b"FV\r").await;
        assert!(
            result.is_err(),
            "expected write to unexpected cmd to fail: {result:?}"
        );
    }

    #[tokio::test]
    async fn wildcard_writes_preserve_a_later_exact_exchange() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_any_write();
        mock.expect(b"ID\r", b"ID TH-D75\r");

        mock.write(b"write-only binary frame").await?;
        mock.write(b"another binary frame").await?;
        mock.write(b"ID\r").await?;

        let mut buffer = [0_u8; 64];
        let count = mock.read(&mut buffer).await?;
        assert_eq!(read_prefix(&buffer, count)?, b"ID TH-D75\r");
        mock.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn multiple_exchanges() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03.000\r");

        mock.write(b"ID\r").await?;
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"ID TH-D75\r");

        mock.write(b"FV\r").await?;
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"FV 1.03.000\r");

        mock.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn from_fixture_file() -> TestResult {
        let mut mock = MockTransport::from_fixture(Path::new("tests/fixtures/identify.txt"))?;

        mock.write(b"ID\r").await?;
        let mut buf = [0u8; 64];
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"ID TH-D75\r");

        mock.write(b"FV\r").await?;
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"FV 1.03.000\r");

        mock.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn read_without_write_errors() {
        let mut mock = MockTransport::new();
        let mut buf = [0u8; 64];
        let result = mock.read(&mut buf).await;
        assert!(
            result.is_err(),
            "expected read-before-write to fail: {result:?}"
        );
    }

    #[tokio::test]
    async fn reads_preserve_data_that_does_not_fit_the_caller_buffer() -> TestResult {
        let mut mock = MockTransport::new();
        mock.queue_read(b"abcdef");
        let mut buf = [0u8; 2];

        for expected in [b"ab".as_slice(), b"cd".as_slice(), b"ef".as_slice()] {
            let n = mock.read(&mut buf).await?;
            assert_eq!(read_prefix(&buf, n)?, expected);
        }
        Ok(())
    }

    #[tokio::test]
    async fn delayed_reads_preserve_their_unread_suffix() -> TestResult {
        let mut mock = MockTransport::new();
        mock.queue_read_delayed(b"abcd", 0);
        let mut buf = [0u8; 3];

        let first = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, first)?, b"abc");
        let second = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, second)?, b"d");
        Ok(())
    }

    #[tokio::test]
    async fn write_with_no_exchanges_errors() {
        let mut mock = MockTransport::new();
        let result = mock.write(b"ID\r").await;
        assert!(
            result.is_err(),
            "expected write with no expectations to fail: {result:?}"
        );
    }

    #[tokio::test]
    async fn default_creates_empty() {
        let mock = MockTransport::default();
        mock.assert_complete();
    }

    #[tokio::test]
    async fn close_drops_inflight_reads_but_keeps_script() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TH-D75\r");
        mock.expect(b"FV\r", b"FV 1.03\r");
        // Consume the first write; its response is now in flight.
        mock.write(b"ID\r").await?;
        mock.close().await?;
        // The in-flight response died with the connection...
        let mut buf = [0u8; 32];
        let r = mock.read(&mut buf).await;
        assert!(
            matches!(r, Err(TransportError::Read(_))),
            "in-flight read should be gone, got {r:?}"
        );
        // ...but the next scripted exchange survives for post-reopen use.
        mock.write(b"FV\r").await?;
        let n = mock.read(&mut buf).await?;
        assert_eq!(read_prefix(&buf, n)?, b"FV 1.03\r");
        Ok(())
    }

    #[tokio::test]
    async fn default_reopen_is_unsupported() {
        struct Bare;
        impl Transport for Bare {
            async fn write(&mut self, _d: &[u8]) -> Result<(), TransportError> {
                Ok(())
            }
            async fn read(&mut self, _b: &mut [u8]) -> Result<usize, TransportError> {
                Ok(0)
            }
            async fn close(&mut self) -> Result<(), TransportError> {
                Ok(())
            }
        }
        let mut t = Bare;
        let r = t.reopen().await;
        assert!(
            matches!(r, Err(TransportError::ReopenUnsupported)),
            "expected ReopenUnsupported, got {r:?}"
        );
    }

    #[tokio::test]
    async fn mock_reopen_is_scriptable_fifo() {
        let mut mock = MockTransport::new();
        mock.expect_reopen(Err(TransportError::NotFound));
        mock.expect_reopen(Ok(()));
        let first = mock.reopen().await;
        assert!(
            matches!(first, Err(TransportError::NotFound)),
            "got {first:?}"
        );
        let second = mock.reopen().await;
        assert!(second.is_ok(), "got {second:?}");
        // A drained script defaults to success.
        let third = mock.reopen().await;
        assert!(third.is_ok(), "got {third:?}");
    }
}
