//! Bounded identification and diagnostics on a caller-selected byte transport.
//!
//! This probe reads MMDVM framing on a connection it borrows; it identifies no
//! radio and selects no operating mode. It never opens, closes, reopens or
//! retries: the caller owns the connection's lifetime, its cleanup after a
//! failure or cancellation, and any retry policy.

use std::time::Duration;

use kenwood_transport::{Transport, TransportError};
use mmdvm_core::{
    MIN_FRAME_LEN, MMDVM_FRAME_START, MMDVM_GET_STATUS, MMDVM_GET_VERSION, MmdvmError, MmdvmFrame,
    ModemStatus, VersionResponse, decode_frame,
};

/// Failures from bounded MMDVM identification and status exchanges.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProbeError {
    /// The caller requested cancellation at a complete request boundary.
    #[error("MMDVM diagnostic probe cancelled before its next request")]
    Cancelled,
    /// A status mode byte has no lossless typed representation.
    #[error("unknown MMDVM status mode: 0x{mode:02X}")]
    UnknownStatusMode {
        /// Unrecognized raw mode byte.
        mode: u8,
    },
    /// The original version description is not valid UTF-8.
    #[error("invalid UTF-8 in MMDVM version description: {0}")]
    InvalidDescriptionEncoding(#[source] std::str::Utf8Error),
    /// A control character remains after trimming documented trailing padding.
    #[error(
        "MMDVM version description contains control character {character:?} at byte {byte_offset}"
    )]
    DescriptionControlCharacter {
        /// Rejected control character, never silently removed or repaired.
        character: char,
        /// Byte offset within the description, excluding the protocol prefix.
        byte_offset: usize,
    },
    /// The requested timeout is zero or cannot be represented as a deadline.
    #[error("invalid MMDVM probe timeout: {timeout:?}")]
    InvalidTimeout {
        /// Rejected timeout value.
        timeout: Duration,
    },
    /// The shared write/read deadline expired.
    #[error("MMDVM probe timed out after {timeout:?}")]
    Timeout {
        /// Total budget shared by the probe's write and read operations.
        timeout: Duration,
    },
    /// The underlying byte connection failed.
    #[error("MMDVM probe transport failed: {0}")]
    Transport(#[from] TransportError),
    /// The endpoint closed before a complete response arrived.
    #[error("MMDVM endpoint closed before a complete response")]
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

/// Validated version reply and the outcome of its following status query.
///
/// A failed status exchange leaves the version already read on this connection
/// intact.
#[derive(Debug)]
pub struct DiagnosticResponse {
    /// Complete protocol-1 or protocol-2 version response.
    pub version: VersionResponse,
    /// Status parsed using the version established on this same connection.
    pub status: Result<ModemStatus, ProbeError>,
}

/// Read version and status without configuring the modem.
///
/// This sends one `GET_VERSION`, then only after a complete accepted reply,
/// one `GET_STATUS` on the same borrowed connection. One absolute deadline
/// covers both writes and all reads. It never creates a modem runtime, starts
/// periodic polling, configures a mode, or sends voice or network traffic.
///
/// The version determines which status layout to parse. Unknown mode bytes
/// are rejected instead of using the core codec's lossy `Idle` fallback.
/// Descriptions use the original valid UTF-8, never replacement decoding;
/// embedded control characters are rejected as described in [`probe_version`].
/// Complete unrelated frames and noise may be consumed, but are never reused
/// as replies to subsequent requests. Each read stops at a frame boundary.
/// The protocol has no request correlation identifier, so a received status
/// cannot prove that the device generated it after the status request.
///
/// # Cancellation safety
///
/// Dropping the future can leave a partial exchange; retire or explicitly
/// recover the connection before reuse. For cancellation that stops only at
/// complete request boundaries, use [`probe_diagnostics_until`].
///
/// # Errors
///
/// Returns an error if version identification fails. Status failures remain
/// inside [`DiagnosticResponse`] alongside the accepted version reply.
pub async fn probe_diagnostics<T: Transport>(
    transport: &mut T,
    timeout: Duration,
) -> Result<DiagnosticResponse, ProbeError> {
    probe_diagnostics_until(transport, timeout, || false).await
}

/// Read version and status, honoring cancellation at request boundaries.
///
/// The exchange and deadline semantics are those of [`probe_diagnostics`].
/// `should_cancel` is called immediately before the version request and again
/// after complete version identification, before any status request. It is
/// not polled while either exchange is in progress: the active exchange
/// reaches its response boundary or the original deadline first.
/// Cancellation before status preserves the version and records
/// [`ProbeError::Cancelled`] in [`DiagnosticResponse::status`].
///
/// # Cancellation safety
///
/// Callback cancellation never interrupts an in-progress exchange. Dropping
/// this future directly can still leave a partial request or response; the
/// caller must retire or explicitly recover that connection before reuse.
///
/// # Errors
///
/// Returns an error if version identification fails. Status failures remain
/// inside [`DiagnosticResponse`] alongside the accepted version reply.
pub async fn probe_diagnostics_until<T: Transport>(
    transport: &mut T,
    timeout: Duration,
    should_cancel: impl Fn() -> bool,
) -> Result<DiagnosticResponse, ProbeError> {
    let deadline = Deadline::new(timeout)?;
    if should_cancel() {
        return Err(ProbeError::Cancelled);
    }
    let version = read_version(transport, deadline).await?;
    let status = if should_cancel() {
        Err(ProbeError::Cancelled)
    } else {
        read_status(transport, deadline, &version).await
    };
    Ok(DiagnosticResponse { version, status })
}

/// Send one `GET_VERSION` and accept a complete protocol-1 or protocol-2 reply.
///
/// One absolute deadline covers the write and every subsequent read. Other
/// complete frames and non-frame bytes are skipped within that same budget.
/// The response must include a nonempty version description, so a bare echo of
/// the request is rejected. Original description bytes must be valid UTF-8.
/// Trailing NUL bytes are removed first, then trailing Unicode whitespace,
/// matching the core codec's padding semantics. Any remaining control
/// character is rejected rather than repaired or silently discarded.
/// Reads stop at each advertised frame boundary, leaving
/// any following bytes for the consumer that takes over this same connection.
/// No configuration or mode-switch command is sent, and a version reply
/// describes the MMDVM firmware without distinguishing a radio's Terminal and
/// Access Point modes.
///
/// # Cancellation safety
///
/// Cancellation can leave a transmitted request or a partially consumed
/// reply, so the caller must retire or explicitly recover the connection
/// before another workflow.
///
/// # Errors
///
/// Returns [`ProbeError`] for an invalid budget, expiration, transport/codec
/// failure, or incomplete input. Only a complete accepted version returns
/// success.
pub async fn probe_version<T: Transport>(
    transport: &mut T,
    timeout: Duration,
) -> Result<VersionResponse, ProbeError> {
    read_version(transport, Deadline::new(timeout)?).await
}

/// Establish the protocol layout before any optional diagnostic status query.
async fn read_version<T: Transport>(
    transport: &mut T,
    deadline: Deadline,
) -> Result<VersionResponse, ProbeError> {
    deadline.write_request(transport, MMDVM_GET_VERSION).await?;
    loop {
        let frame = deadline.read_frame(transport).await?;
        if frame.command == MMDVM_GET_VERSION
            && let Ok(version) = VersionResponse::parse(&frame.payload)
            && matches!(version.protocol, 1 | 2)
            && !version.description.is_empty()
        {
            validate_description(&frame.payload, version.protocol)?;
            tracing::info!(
                protocol = version.protocol,
                description = %version.description,
                "validated MMDVM GET_VERSION response"
            );
            return Ok(version);
        }
    }
}

/// Validate the original description before accepting the codec's owned text.
fn validate_description(payload: &[u8], protocol: u8) -> Result<(), ProbeError> {
    // Protocol 1 places the description after its protocol byte. Protocol 2
    // adds capability bytes, CPU type, and the sixteen-byte device identifier.
    let offset = match protocol {
        1 => 1,
        2 => 20,
        _ => return Err(MmdvmError::InvalidVersionResponse.into()),
    };
    let raw = payload
        .get(offset..)
        .ok_or(MmdvmError::InvalidVersionResponse)?;
    let description = std::str::from_utf8(raw)
        .map_err(ProbeError::InvalidDescriptionEncoding)?
        .trim_end_matches('\0')
        .trim_end();
    if let Some((byte_offset, character)) = description
        .char_indices()
        .find(|(_, character)| character.is_control())
    {
        return Err(ProbeError::DescriptionControlCharacter {
            character,
            byte_offset,
        });
    }
    Ok(())
}

/// A version borrowed from this call selects the status decoder.
async fn read_status<T: Transport>(
    transport: &mut T,
    deadline: Deadline,
    version: &VersionResponse,
) -> Result<ModemStatus, ProbeError> {
    deadline.write_request(transport, MMDVM_GET_STATUS).await?;
    loop {
        let frame = deadline.read_frame(transport).await?;
        if frame.command != MMDVM_GET_STATUS {
            continue;
        }
        let (status, mode_offset) = match version.protocol {
            1 => (ModemStatus::parse_v1(&frame.payload)?, 1),
            2 => (ModemStatus::parse_v2(&frame.payload)?, 0),
            _ => return Err(MmdvmError::InvalidVersionResponse.into()),
        };
        let mode = frame
            .payload
            .get(mode_offset)
            .copied()
            .ok_or(ProbeError::IncompleteFrame)?;
        if status.mode.as_byte() != mode {
            return Err(ProbeError::UnknownStatusMode { mode });
        }
        tracing::info!(?status, "validated MMDVM GET_STATUS response");
        return Ok(status);
    }
}

#[cfg(test)]
#[path = "probe/diagnostic_tests.rs"]
mod diagnostic_tests;

/// One budget shared by all fragments, never renewed by received noise.
#[derive(Debug, Clone, Copy)]
struct Deadline {
    at: tokio::time::Instant,
    timeout: Duration,
}

impl Deadline {
    fn new(timeout: Duration) -> Result<Self, ProbeError> {
        let at = tokio::time::Instant::now()
            .checked_add(timeout)
            .filter(|_| !timeout.is_zero())
            .ok_or(ProbeError::InvalidTimeout { timeout })?;
        Ok(Self { at, timeout })
    }

    const fn expired(self) -> ProbeError {
        ProbeError::Timeout {
            timeout: self.timeout,
        }
    }

    fn ensure_remaining(self) -> Result<(), ProbeError> {
        if tokio::time::Instant::now() >= self.at {
            return Err(self.expired());
        }
        Ok(())
    }

    async fn write_request<T: Transport>(
        self,
        transport: &mut T,
        command: u8,
    ) -> Result<(), ProbeError> {
        self.ensure_remaining()?;
        let request = [MMDVM_FRAME_START, MIN_FRAME_LEN, command];
        tokio::time::timeout_at(self.at, transport.write(&request))
            .await
            .map_err(|_| self.expired())??;
        self.ensure_remaining()
    }

    async fn read_frame<T: Transport>(self, transport: &mut T) -> Result<MmdvmFrame, ProbeError> {
        loop {
            let mut start = [0_u8; 1];
            self.read_exact(transport, &mut start).await?;
            if start[0] != MMDVM_FRAME_START {
                continue;
            }
            let mut length = [0_u8; 1];
            self.read_exact(transport, &mut length).await?;
            let mut wire = vec![MMDVM_FRAME_START, length[0]];
            let frame_len = if length[0] == 0 {
                let mut extended = [0_u8; 1];
                self.read_exact(transport, &mut extended).await?;
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
            self.read_exact(transport, remaining).await?;
            let (frame, consumed) = decode_frame(&wire)?.ok_or(ProbeError::IncompleteFrame)?;
            if consumed != wire.len() {
                return Err(ProbeError::IncompleteFrame);
            }
            tracing::debug!(?wire, "complete MMDVM probe response frame");
            return Ok(frame);
        }
    }

    async fn read_exact<T: Transport>(
        self,
        transport: &mut T,
        target: &mut [u8],
    ) -> Result<(), ProbeError> {
        let mut filled = 0;
        while filled < target.len() {
            self.ensure_remaining()?;
            let remaining = target
                .get_mut(filled..)
                .ok_or(ProbeError::IncompleteFrame)?;
            let capacity = remaining.len();
            let count = tokio::time::timeout_at(self.at, transport.read(remaining))
                .await
                .map_err(|_| self.expired())??;
            self.ensure_remaining()?;
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
