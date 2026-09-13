//! Diagnostic exchanges must remain bounded and carry no configuration traffic.

use std::cell::Cell;
use std::collections::VecDeque;
use std::io;

use kenwood_transport::MockTransport;
use mmdvm_core::ModemMode;

use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const VERSION_REQUEST: &[u8] = b"\xE0\x03\x00";
const STATUS_REQUEST: &[u8] = b"\xE0\x03\x01";
const VERSION: &[u8] = b"\xE0\x08\x00\x01TEST";
const STATUS: &[u8] = b"\xE0\x0A\x01\x01\x01\x40\x07\x03\x04\x05";
const BUDGET: Duration = Duration::from_secs(2);

/// Status write outcomes independent of the successful version exchange.
#[derive(Debug, Clone, Copy)]
enum StatusWrite {
    Complete,
    Fail,
    Pending,
}

/// Record every control operation while modelling bounded write latency.
#[derive(Debug)]
struct TimedTransport {
    inner: MockTransport,
    write_delay: Duration,
    status_write: StatusWrite,
    writes: Vec<Vec<u8>>,
    controls: Vec<&'static str>,
}

impl TimedTransport {
    fn new(write_delay: Duration, status_write: StatusWrite) -> Self {
        let mut inner = MockTransport::new();
        inner.expect(VERSION_REQUEST, VERSION);
        Self {
            inner,
            write_delay,
            status_write,
            writes: Vec::new(),
            controls: Vec::new(),
        }
    }
}

impl Transport for TimedTransport {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.writes.push(data.to_vec());
        if data == STATUS_REQUEST {
            match self.status_write {
                StatusWrite::Complete => {}
                StatusWrite::Fail => {
                    return Err(TransportError::Write(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "scripted status write failure",
                    )));
                }
                StatusWrite::Pending => return std::future::pending().await,
            }
        }
        tokio::time::sleep(self.write_delay).await;
        self.inner.write(data).await
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        self.inner.read(buffer).await
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
async fn version_then_status_sends_only_two_queries() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect(STATUS_REQUEST, STATUS);
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    let status = report.status?;
    assert_eq!(status.mode, ModemMode::Dstar);
    assert_eq!(status.dstar_space, 7);
    assert!(status.cd());
    assert!(!status.tx());
    assert_eq!(transport.writes(), [VERSION_REQUEST, STATUS_REQUEST]);
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn protocol_two_status_uses_the_identified_layout() -> TestResult {
    let mut version = vec![0xE0, 27, 0, 2, 0x41, 0x01, 0];
    version.extend_from_slice(&[0; 16]);
    version.extend_from_slice(b"TEST");
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, &version);
    transport.expect(
        STATUS_REQUEST,
        b"\xE0\x0F\x01\x01\x40\x00\x07\x03\x04\x05\x06\x08\x00\x09\x0A",
    );
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.protocol, 2);
    let status = report.status?;
    assert_eq!(status.mode, ModemMode::Dstar);
    assert_eq!(status.dstar_space, 7);
    assert_eq!(status.fm_space, 9);
    assert_eq!(status.pocsag_space, 10);
    assert!(status.cd());
    transport.assert_complete();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn failed_version_never_sends_status() {
    let mut transport = MockTransport::new();
    transport.expect_partial_then_hang(VERSION_REQUEST, b"\xE0\x04\x00\x01");
    let result = probe_diagnostics(&mut transport, BUDGET).await;
    assert!(matches!(result, Err(ProbeError::Timeout { .. })));
    assert_eq!(transport.writes(), [VERSION_REQUEST]);
    transport.assert_complete();
}

#[tokio::test(start_paused = true)]
async fn failed_status_preserves_complete_version_evidence() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect_hang(STATUS_REQUEST);
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::Timeout { .. })));
    assert_eq!(transport.writes(), [VERSION_REQUEST, STATUS_REQUEST]);
    transport.assert_complete();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn version_and_status_share_the_same_absolute_deadline() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, b"");
    transport.expect(STATUS_REQUEST, b"");
    transport.queue_read_delayed(VERSION, 1_100);
    transport.queue_read_delayed(STATUS, 1_100);
    let started = tokio::time::Instant::now();
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::Timeout { .. })));
    assert_eq!(started.elapsed(), BUDGET);
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn short_status_is_a_codec_failure_not_readiness() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect(STATUS_REQUEST, b"\xE0\x06\x01\x01\x01\x00");
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(
        report.status,
        Err(ProbeError::Frame(MmdvmError::InvalidStatusLength {
            len: 3,
            min: 7,
        }))
    ));
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn unknown_status_mode_must_not_be_reported_as_idle() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect(STATUS_REQUEST, b"\xE0\x0A\x01\x01\xFF\x00\x07\x03\x04\x05");
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert!(matches!(
        report.status,
        Err(ProbeError::UnknownStatusMode { mode: 0xFF })
    ));
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn cancelled_before_version_sends_nothing() {
    let mut transport = MockTransport::new();
    let result = probe_diagnostics_until(&mut transport, BUDGET, || true).await;
    assert!(matches!(result, Err(ProbeError::Cancelled)));
    assert!(transport.writes().is_empty());
}

#[tokio::test]
async fn cancelled_after_version_retains_evidence_without_status_write() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    let calls = Cell::new(0);
    let report = probe_diagnostics_until(&mut transport, BUDGET, || {
        let previous = calls.replace(calls.get() + 1);
        previous != 0
    })
    .await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::Cancelled)));
    assert_eq!(transport.writes(), [VERSION_REQUEST]);
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn version_does_not_retain_earlier_unsolicited_status_for_later_query() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect_reads(VERSION_REQUEST, &[STATUS, VERSION]);
    transport.expect_eof(STATUS_REQUEST);
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::UnexpectedEof)));
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn fragmented_status_leaves_following_frame_unread() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect_reads(
        STATUS_REQUEST,
        &[
            b"noise\xE0\x04\x70\x01\xE0",
            b"\x0A\x01\x01\x01\x40",
            b"\x07\x03\x04\x05next",
        ],
    );
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.status?.dstar_space, 7);
    let mut following = [0; 4];
    assert_eq!(transport.read(&mut following).await?, following.len());
    assert_eq!(following, *b"next");
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn invalid_timeout_does_not_send_either_query() {
    for timeout in [Duration::ZERO, Duration::MAX] {
        let mut transport = MockTransport::new();
        let result = probe_diagnostics(&mut transport, timeout).await;
        assert!(matches!(result, Err(ProbeError::InvalidTimeout { .. })));
        assert!(transport.writes().is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn both_writes_consume_the_same_budget_without_control_operations() -> TestResult {
    let mut transport = TimedTransport::new(Duration::from_millis(1_100), StatusWrite::Complete);
    let started = tokio::time::Instant::now();
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::Timeout { .. })));
    assert_eq!(started.elapsed(), BUDGET);
    assert_eq!(transport.writes, [VERSION_REQUEST, STATUS_REQUEST]);
    assert!(transport.controls.is_empty());
    transport.inner.assert_complete();
    Ok(())
}

#[tokio::test]
async fn failed_status_write_preserves_error_and_version_without_retries() -> TestResult {
    let mut transport = TimedTransport::new(Duration::ZERO, StatusWrite::Fail);
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(
        report.status,
        Err(ProbeError::Transport(TransportError::Write(error)))
        if error.kind() == io::ErrorKind::BrokenPipe
            && error.to_string() == "scripted status write failure"
    ));
    assert_eq!(transport.writes, [VERSION_REQUEST, STATUS_REQUEST]);
    assert!(transport.controls.is_empty());
    transport.inner.assert_complete();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn pending_status_write_expires_at_original_deadline() -> TestResult {
    let mut transport = TimedTransport::new(Duration::from_millis(500), StatusWrite::Pending);
    let started = tokio::time::Instant::now();
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.version.description, "TEST");
    assert!(matches!(report.status, Err(ProbeError::Timeout { .. })));
    assert_eq!(started.elapsed(), BUDGET);
    assert_eq!(transport.writes, [VERSION_REQUEST, STATUS_REQUEST]);
    assert!(transport.controls.is_empty());
    transport.inner.assert_complete();
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn dropping_during_status_write_does_not_emit_recovery_traffic() {
    let mut transport = TimedTransport::new(Duration::ZERO, StatusWrite::Pending);
    let cancel_after = Duration::from_millis(25);
    let result =
        tokio::time::timeout(cancel_after, probe_diagnostics(&mut transport, BUDGET)).await;
    assert!(result.is_err(), "the pending probe must be cancelled");
    assert_eq!(transport.writes, [VERSION_REQUEST, STATUS_REQUEST]);
    assert!(transport.controls.is_empty());
    transport.inner.assert_complete();
}

#[tokio::test(start_paused = true)]
async fn noise_during_status_does_not_renew_the_deadline() -> TestResult {
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, b"");
    transport.expect(STATUS_REQUEST, b"");
    transport.queue_read(VERSION);
    transport.queue_read_delayed(b"\xE0\x04\x70\x01", 700);
    transport.queue_read_delayed(b"unrelated", 700);
    transport.queue_read_delayed(b"\xE0\x04\x70\x01", 700);
    let started = tokio::time::Instant::now();
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert!(matches!(report.status, Err(ProbeError::Timeout { .. })));
    assert_eq!(started.elapsed(), BUDGET);
    transport.assert_complete();
    Ok(())
}

#[tokio::test]
async fn extended_status_is_decoded_without_consuming_next_frame() -> TestResult {
    let mut status = vec![0xE0, 0, 0, 1, 1, 1, 0x40, 7, 3, 4, 5];
    status.resize(255, 0);
    status.extend_from_slice(b"next");
    let mut transport = MockTransport::new();
    transport.expect(VERSION_REQUEST, VERSION);
    transport.expect(STATUS_REQUEST, &status);
    let report = probe_diagnostics(&mut transport, BUDGET).await?;
    assert_eq!(report.status?.dstar_space, 7);
    let mut following = [0; 4];
    assert_eq!(transport.read(&mut following).await?, following.len());
    assert_eq!(following, *b"next");
    transport.assert_complete();
    Ok(())
}

/// Build either supported description layout without transforming its bytes.
fn version_wire(protocol: u8, description: &[u8]) -> Result<Vec<u8>, MmdvmError> {
    let mut payload = vec![protocol];
    if protocol == 2 {
        payload.extend_from_slice(&[0x41, 0x01, 0]);
        payload.extend_from_slice(&[0; 16]);
    }
    payload.extend_from_slice(description);
    mmdvm_core::encode_frame(&MmdvmFrame::with_payload(MMDVM_GET_VERSION, payload))
}

#[tokio::test]
async fn invalid_utf8_is_rejected_in_both_version_description_layouts() -> TestResult {
    for protocol in [1, 2] {
        for description in [b"TEST\xFF".as_slice(), b"TEST\xC3", b"TEST\xC0\xAF"] {
            let mut transport = MockTransport::new();
            transport.expect(VERSION_REQUEST, &version_wire(protocol, description)?);
            let result = probe_version(&mut transport, BUDGET).await;
            assert!(
                matches!(result, Err(ProbeError::InvalidDescriptionEncoding(_))),
                "invalid UTF-8 must not become repaired evidence: {result:?}"
            );
            assert_eq!(transport.writes(), [VERSION_REQUEST]);
            transport.assert_complete();
        }
    }
    Ok(())
}

#[tokio::test]
async fn interior_controls_are_rejected_in_both_version_description_layouts() -> TestResult {
    for protocol in [1, 2] {
        for character in ['\0', '\n', '\r', '\t', '\u{1b}', '\u{7f}', '\u{85}'] {
            let description = format!("TEST{character}suffix");
            let mut transport = MockTransport::new();
            transport.expect(
                VERSION_REQUEST,
                &version_wire(protocol, description.as_bytes())?,
            );
            let result = probe_version(&mut transport, BUDGET).await;
            assert!(
                matches!(
                    result,
                    Err(ProbeError::DescriptionControlCharacter {
                        character: actual,
                        byte_offset: 4,
                    }) if actual == character
                ),
                "interior controls must not become description evidence: {result:?}"
            );
            transport.assert_complete();
        }
    }
    Ok(())
}

#[tokio::test]
async fn valid_unicode_and_trailing_padding_remain_lossless() -> TestResult {
    for protocol in [1, 2] {
        for description in ["Récepteur 日本語", "Literal replacement character �"] {
            let padded = format!("{description} \t\r\n\0\0");
            let mut transport = MockTransport::new();
            transport.expect(VERSION_REQUEST, &version_wire(protocol, padded.as_bytes())?);
            let version = probe_version(&mut transport, BUDGET).await?;
            assert_eq!(version.protocol, protocol);
            assert_eq!(version.description, description);
            transport.assert_complete();
        }
    }
    Ok(())
}

#[tokio::test]
async fn rejected_description_never_admits_the_status_query() -> TestResult {
    for protocol in [1, 2] {
        let mut transport = MockTransport::new();
        transport.expect(VERSION_REQUEST, &version_wire(protocol, b"TEST\xFF")?);
        let result = probe_diagnostics(&mut transport, BUDGET).await;
        assert!(matches!(
            result,
            Err(ProbeError::InvalidDescriptionEncoding(_))
        ));
        assert_eq!(transport.writes(), [VERSION_REQUEST]);
        transport.assert_complete();
    }
    Ok(())
}

/// Finite input whose reads never yield `Pending`, even as real time advances.
///
/// The finite stop makes a missing clock check fail with EOF, not hang forever.
#[derive(Debug)]
struct ReadyNoiseTransport {
    version: VecDeque<u8>,
    remaining_noise_reads: u8,
    noise_reads: u8,
    writes: Vec<Vec<u8>>,
}

impl ReadyNoiseTransport {
    fn new(version: &[u8]) -> Self {
        Self {
            version: version.iter().copied().collect(),
            remaining_noise_reads: 10,
            noise_reads: 0,
            writes: Vec::new(),
        }
    }
}

impl Transport for ReadyNoiseTransport {
    async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.writes.push(bytes.to_vec());
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let Some(target) = buffer.first_mut() else {
            return Ok(0);
        };
        if let Some(byte) = self.version.pop_front() {
            *target = byte;
            return Ok(1);
        }
        if self.remaining_noise_reads == 0 {
            return Ok(0);
        }
        // Deliberately synchronous: timeout futures must not rely on a Pending
        // return to notice that immediately available input exhausted the budget.
        std::thread::sleep(Duration::from_millis(10));
        self.remaining_noise_reads -= 1;
        self.noise_reads += 1;
        *target = b'x';
        Ok(1)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[tokio::test]
async fn always_ready_version_noise_stops_before_its_finite_input_is_exhausted() {
    let mut transport = ReadyNoiseTransport::new(&[]);
    let budget = Duration::from_millis(50);
    let result = probe_version(&mut transport, budget).await;
    assert!(matches!(result, Err(ProbeError::Timeout { timeout }) if timeout == budget));
    assert!(transport.noise_reads > 0);
    assert!(transport.remaining_noise_reads > 0);
    assert_eq!(transport.writes, [VERSION_REQUEST]);
}

#[tokio::test]
async fn always_ready_status_noise_stops_before_its_finite_input_is_exhausted() -> TestResult {
    let mut transport = ReadyNoiseTransport::new(VERSION);
    let budget = Duration::from_millis(50);
    let result = probe_diagnostics(&mut transport, budget).await?;
    assert_eq!(result.version.description, "TEST");
    assert!(matches!(result.status, Err(ProbeError::Timeout { timeout }) if timeout == budget));
    assert!(transport.noise_reads > 0);
    assert!(transport.remaining_noise_reads > 0);
    assert_eq!(transport.writes, [VERSION_REQUEST, STATUS_REQUEST]);
    Ok(())
}
