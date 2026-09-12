//! Bounded version identification on a caller-selected byte transport.
//!
//! This probe establishes MMDVM framing on the connection it borrows, not a
//! radio identity or operating-mode selection. The caller owns admission,
//! connection continuity, and cleanup after failure or cancellation.

use std::time::Duration;

use kenwood_transport::{Transport, TransportError};
use mmdvm_core::{
    MIN_FRAME_LEN, MMDVM_FRAME_START, MMDVM_GET_VERSION, MmdvmError, VersionResponse, decode_frame,
};

/// Failures from one bounded MMDVM version exchange.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// The requested timeout is zero or cannot be represented as a deadline.
    #[error("invalid MMDVM probe timeout: {timeout:?}")]
    InvalidTimeout {
        /// Rejected timeout value.
        timeout: Duration,
    },
    /// The shared write/read deadline expired.
    #[error("MMDVM version probe timed out after {timeout:?}")]
    Timeout {
        /// Total budget for the single exchange.
        timeout: Duration,
    },
    /// The underlying byte connection failed.
    #[error("MMDVM version probe transport failed: {0}")]
    Transport(#[from] TransportError),
    /// The endpoint closed before a complete version response arrived.
    #[error("MMDVM endpoint closed before a complete version response")]
    UnexpectedEof,
    /// The transport violated its initialized-byte-count contract.
    #[error("transport reported {reported} bytes for a {capacity}-byte probe buffer")]
    InvalidReadCount {
        /// Count returned by the transport.
        reported: usize,
        /// Actual capacity supplied to the transport.
        capacity: usize,
    },
    /// A complete frame did not satisfy the wire codec.
    #[error("invalid MMDVM response frame: {0}")]
    Frame(#[from] MmdvmError),
    /// The advertised complete frame could not be decoded in full.
    #[error("MMDVM response did not decode as one complete frame")]
    IncompleteFrame,
}

/// Send one `GET_VERSION` and accept a complete protocol-1 or protocol-2 reply.
///
/// One absolute deadline covers the write and every subsequent read. Other
/// complete frames and non-frame bytes are skipped within that same budget.
/// The response must include a nonempty version description; an echoed
/// request is not proof. Reads stop at each advertised frame boundary, leaving
/// any following bytes for the consumer that takes over this same connection.
/// No configuration, mode-switch, retry, close, or reopen operation is sent.
///
/// A success does not distinguish a radio's Terminal and Access Point modes
/// or authorize transmission. Those decisions remain with the caller.
///
/// # Cancellation safety
///
/// Cancellation can leave a transmitted request or a partially consumed
/// reply. It never establishes a usable protocol boundary. The caller must
/// retire or explicitly recover the connection before another workflow.
///
/// # Errors
///
/// Returns [`ProbeError`] for an invalid budget, expiration, transport/codec
/// failure, or incomplete input. Only a complete accepted version returns
/// success; a timeout alone conveys no protocol identity.
pub async fn probe_version<T: Transport>(
    transport: &mut T,
    timeout: Duration,
) -> Result<VersionResponse, ProbeError> {
    let at = tokio::time::Instant::now()
        .checked_add(timeout)
        .filter(|_| !timeout.is_zero())
        .ok_or(ProbeError::InvalidTimeout { timeout })?;
    let deadline = Deadline { at, timeout };
    let request = [MMDVM_FRAME_START, MIN_FRAME_LEN, MMDVM_GET_VERSION];
    tokio::time::timeout_at(at, transport.write(&request))
        .await
        .map_err(|_| deadline.expired())??;

    loop {
        let mut start = [0_u8; 1];
        deadline.read_exact(transport, &mut start).await?;
        if start[0] != MMDVM_FRAME_START {
            continue;
        }
        let mut length = [0_u8; 1];
        deadline.read_exact(transport, &mut length).await?;
        let mut wire = vec![MMDVM_FRAME_START, length[0]];
        let frame_len = if length[0] == 0 {
            let mut extended = [0_u8; 1];
            deadline.read_exact(transport, &mut extended).await?;
            wire.push(extended[0]);
            usize::from(extended[0]) + 255
        } else {
            let frame_len = usize::from(length[0]);
            if frame_len < usize::from(MIN_FRAME_LEN) {
                continue;
            }
            frame_len
        };
        let consumed = wire.len();
        wire.resize(frame_len, 0);
        let remaining = wire
            .get_mut(consumed..)
            .ok_or(ProbeError::IncompleteFrame)?;
        deadline.read_exact(transport, remaining).await?;
        let (frame, consumed) = decode_frame(&wire)?.ok_or(ProbeError::IncompleteFrame)?;
        if consumed != wire.len() {
            return Err(ProbeError::IncompleteFrame);
        }
        if frame.command == MMDVM_GET_VERSION
            && let Ok(version) = VersionResponse::parse(&frame.payload)
            && matches!(version.protocol, 1 | 2)
            && !version.description.is_empty()
        {
            tracing::info!(
                protocol = version.protocol,
                description = %version.description,
                wire = ?wire,
                "validated MMDVM GET_VERSION response"
            );
            return Ok(version);
        }
    }
}

/// One budget shared by all fragments, never renewed by received noise.
#[derive(Debug, Clone, Copy)]
struct Deadline {
    at: tokio::time::Instant,
    timeout: Duration,
}

impl Deadline {
    const fn expired(self) -> ProbeError {
        ProbeError::Timeout {
            timeout: self.timeout,
        }
    }

    async fn read_exact<T: Transport>(
        self,
        transport: &mut T,
        target: &mut [u8],
    ) -> Result<(), ProbeError> {
        let mut filled = 0;
        while filled < target.len() {
            if tokio::time::Instant::now() >= self.at {
                return Err(self.expired());
            }
            let remaining = target
                .get_mut(filled..)
                .ok_or(ProbeError::IncompleteFrame)?;
            let capacity = remaining.len();
            let count = tokio::time::timeout_at(self.at, transport.read(remaining))
                .await
                .map_err(|_| self.expired())??;
            if count == 0 {
                return Err(ProbeError::UnexpectedEof);
            }
            if count > capacity {
                return Err(ProbeError::InvalidReadCount {
                    reported: count,
                    capacity,
                });
            }
            filled += count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use kenwood_transport::MockTransport;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    const REQUEST: &[u8] = b"\xE0\x03\x00";
    const VERSION: &[u8] = b"\xE0\x08\x00\x01TEST";
    const BUDGET: Duration = Duration::from_secs(2);

    /// Deterministic write outcomes for boundary-failure tests.
    #[derive(Debug, Clone, Copy)]
    enum WriteBehavior {
        Complete,
        Pending,
        Fail,
    }

    /// Deterministic read failures, including a broken transport contract.
    #[derive(Debug, Clone, Copy)]
    enum ReadBehavior {
        InvalidCount,
        Fail,
    }

    /// A transport that records every operation without opening an endpoint.
    #[derive(Debug)]
    struct FaultTransport {
        write_behavior: WriteBehavior,
        read_behavior: ReadBehavior,
        writes: Vec<Vec<u8>>,
        reads: usize,
        controls: Vec<&'static str>,
    }

    impl FaultTransport {
        const fn new(write_behavior: WriteBehavior, read_behavior: ReadBehavior) -> Self {
            Self {
                write_behavior,
                read_behavior,
                writes: Vec::new(),
                reads: 0,
                controls: Vec::new(),
            }
        }
    }

    impl Transport for FaultTransport {
        async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
            self.writes.push(data.to_vec());
            match self.write_behavior {
                WriteBehavior::Complete => Ok(()),
                WriteBehavior::Pending => std::future::pending().await,
                WriteBehavior::Fail => Err(TransportError::Write(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "scripted probe write failure",
                ))),
            }
        }

        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
            self.reads += 1;
            match self.read_behavior {
                ReadBehavior::InvalidCount => Ok(buffer.len() + 1),
                ReadBehavior::Fail => Err(TransportError::Read(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "scripted probe read failure",
                ))),
            }
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            self.controls.push("close");
            Ok(())
        }

        fn set_baud_rate(&mut self, _baud: u32) -> Result<(), TransportError> {
            self.controls.push("baud");
            Ok(())
        }

        async fn reopen(&mut self) -> Result<(), TransportError> {
            self.controls.push("reopen");
            Ok(())
        }
    }

    #[tokio::test]
    async fn fragmented_version_preserves_following_frame() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_reads(
            REQUEST,
            &[b"noise\xE0", b"\x08\x00", b"\x01TEST\xE0\x04\x70\x02"],
        );
        let version = probe_version(&mut transport, BUDGET).await?;
        assert_eq!(version.protocol, 1);
        assert_eq!(version.description, "TEST");
        let mut following = [0; 4];
        let count = transport.read(&mut following).await?;
        assert_eq!(count, following.len());
        assert_eq!(following, *b"\xE0\x04\x70\x02");
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn non_version_frames_do_not_prove_identity() -> TestResult {
        let mut transport = MockTransport::new();
        transport.expect_reads(REQUEST, &[b"\xE0\x03\x00", b"\xE0\x04\x70\x02", VERSION]);
        let version = probe_version(&mut transport, BUDGET).await?;
        assert_eq!(version.description, "TEST");
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn invalid_versions_and_truncation_never_prove_identity() {
        for response in [
            REQUEST,
            b"\xE0\x04\x00\x01",
            b"\xE0\x08\x00\x03TEST",
            b"\xE0\x08\x00\x01TE",
        ] {
            let mut transport = MockTransport::new();
            transport.expect(REQUEST, response);
            let result = probe_version(&mut transport, BUDGET).await;
            assert!(result.is_err(), "invalid version proved MMDVM: {result:?}");
            transport.assert_complete();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fragmented_reads_share_one_deadline() {
        let mut transport = MockTransport::new();
        transport.expect(REQUEST, b"");
        transport.queue_read_delayed(b"\xE0", 1_100);
        transport.queue_read_delayed(b"\x08", 1_100);
        let started = tokio::time::Instant::now();
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(matches!(result, Err(ProbeError::Timeout { timeout }) if timeout == BUDGET));
        assert_eq!(started.elapsed(), BUDGET);
        transport.assert_complete();
    }

    #[tokio::test]
    async fn invalid_timeout_sends_nothing() {
        for timeout in [Duration::ZERO, Duration::MAX] {
            let mut transport = MockTransport::new();
            let result = probe_version(&mut transport, timeout).await;
            assert!(matches!(result, Err(ProbeError::InvalidTimeout { .. })));
            assert!(transport.writes().is_empty());
            transport.assert_complete();
        }
    }

    #[tokio::test]
    async fn protocol_two_requires_its_complete_description_and_preserves_trailing_data()
    -> TestResult {
        let mut response = vec![0xE0, 27, 0, 2, 0x41, 0x01, 0];
        response.extend_from_slice(&[0; 16]);
        response.extend_from_slice(b"TESTnext");
        let mut transport = MockTransport::new();
        transport.expect(REQUEST, &response);
        let version = probe_version(&mut transport, BUDGET).await?;
        assert_eq!(version.protocol, 2);
        assert_eq!(version.description, "TEST");
        let capabilities = version
            .capabilities
            .ok_or("protocol two requires capabilities")?;
        assert!(capabilities.has_dstar());
        assert!(capabilities.has_fm());
        assert!(capabilities.has_pocsag());
        let mut following = [0; 4];
        assert_eq!(transport.read(&mut following).await?, following.len());
        assert_eq!(following, *b"next");
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn invalid_read_count_is_terminal_without_extra_operations() {
        let mut transport =
            FaultTransport::new(WriteBehavior::Complete, ReadBehavior::InvalidCount);
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(matches!(
            result,
            Err(ProbeError::InvalidReadCount {
                reported: 2,
                capacity: 1
            })
        ));
        assert_eq!(transport.writes, [REQUEST]);
        assert_eq!(transport.reads, 1);
        assert!(transport.controls.is_empty());
    }

    #[tokio::test]
    async fn write_failure_preserves_io_error_and_never_reads() {
        let mut transport = FaultTransport::new(WriteBehavior::Fail, ReadBehavior::InvalidCount);
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(
            matches!(result, Err(ProbeError::Transport(TransportError::Write(error)))
            if error.kind() == io::ErrorKind::BrokenPipe && error.to_string() == "scripted probe write failure")
        );
        assert_eq!(transport.writes, [REQUEST]);
        assert_eq!(transport.reads, 0);
        assert!(transport.controls.is_empty());
    }

    #[tokio::test]
    async fn read_failure_preserves_io_error_without_retry() {
        let mut transport = FaultTransport::new(WriteBehavior::Complete, ReadBehavior::Fail);
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(
            matches!(result, Err(ProbeError::Transport(TransportError::Read(error)))
            if error.kind() == io::ErrorKind::ConnectionReset && error.to_string() == "scripted probe read failure")
        );
        assert_eq!(transport.writes, [REQUEST]);
        assert_eq!(transport.reads, 1);
        assert!(transport.controls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn pending_write_consumes_the_total_budget_without_retry() {
        let mut transport = FaultTransport::new(WriteBehavior::Pending, ReadBehavior::InvalidCount);
        let started = tokio::time::Instant::now();
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(matches!(result, Err(ProbeError::Timeout { timeout }) if timeout == BUDGET));
        assert_eq!(started.elapsed(), BUDGET);
        assert_eq!(transport.writes, [REQUEST]);
        assert_eq!(transport.reads, 0);
        assert!(transport.controls.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn caller_cancellation_during_write_does_not_issue_cleanup_or_retry() {
        let mut transport = FaultTransport::new(WriteBehavior::Pending, ReadBehavior::InvalidCount);
        let cancellation = Duration::from_millis(25);
        let started = tokio::time::Instant::now();
        let result =
            tokio::time::timeout(cancellation, probe_version(&mut transport, BUDGET)).await;
        assert!(
            result.is_err(),
            "the caller must cancel the still-pending probe"
        );
        assert_eq!(started.elapsed(), cancellation);
        assert_eq!(transport.writes, [REQUEST]);
        assert_eq!(transport.reads, 0);
        assert!(transport.controls.is_empty());
    }

    #[tokio::test]
    async fn extended_version_preserves_following_bytes() -> TestResult {
        let mut response = vec![0xE0, 0, 3, 0, 1];
        response.extend(std::iter::repeat_n(b'A', 253));
        response.extend_from_slice(b"next");
        let mut transport = MockTransport::new();
        transport.expect(REQUEST, &response);
        let version = probe_version(&mut transport, BUDGET).await?;
        assert_eq!(version.protocol, 1);
        assert_eq!(version.description, "A".repeat(253));
        let mut following = [0; 4];
        assert_eq!(transport.read(&mut following).await?, following.len());
        assert_eq!(following, *b"next");
        transport.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn eof_is_not_a_silent_success() {
        let mut transport = MockTransport::new();
        transport.expect_eof(REQUEST);
        let result = probe_version(&mut transport, BUDGET).await;
        assert!(matches!(result, Err(ProbeError::UnexpectedEof)));
        transport.assert_complete();
    }
}
