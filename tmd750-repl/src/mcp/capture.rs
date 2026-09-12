//! Exclusive capture files with observational or required transport recording.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use kenwood_tmd750::transport::{Transport, TransportError};
use serde::Serialize;
use time::OffsetDateTime;

use super::Failure;

/// A newly reserved directory and its two exclusive output files.
#[derive(Debug)]
pub(super) struct Artifacts {
    /// Directory printed to the operator.
    pub(super) directory: PathBuf,
    /// Reserved final report; empty until the connection has been closed.
    pub(super) report: File,
    /// Per-operation transcript, flushed before returning to the protocol.
    pub(super) transcript: Recorder<File>,
}

impl Artifacts {
    /// Reserve every output before the radio is opened.
    pub(super) fn create(requested: Option<&Path>, cancelled: Arc<AtomicBool>) -> io::Result<Self> {
        let directory = if let Some(path) = requested {
            create_private_directory(path)?;
            path.to_owned()
        } else {
            fs::create_dir_all("captures")?;
            let name = format!(
                "tmd750-mcp-{}-{}",
                OffsetDateTime::now_utc().unix_timestamp_nanos(),
                std::process::id()
            );
            reserve_default_directory(&name)?
        };
        let report = create_private_file(&directory.join("report.json"))?;
        let transcript = Recorder::new(
            create_private_file(&directory.join("transcript.jsonl"))?,
            cancelled,
        );
        Ok(Self {
            directory,
            report,
            transcript,
        })
    }

    /// Reserve the fresh-verification transcript before opening any connection.
    pub(super) fn reserve_post_exit(
        &self,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Recorder<File>> {
        let filename = "post-exit-transcript.jsonl";
        Ok(Recorder::named(
            create_private_file(&self.directory.join(filename))?,
            cancelled,
            filename,
        ))
    }
}

fn reserve_default_directory(name: &str) -> io::Result<PathBuf> {
    for suffix in 0..1024 {
        let directory = Path::new("captures").join(format!("{name}-{suffix}"));
        match create_private_directory(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique MCP capture directory",
    ))
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        let _builder = builder.mode(0o700);
        builder
    };
    builder.create(path)
}

pub(super) fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    let _options = options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _options = options.mode(0o600);
    }
    options.open(path)
}

/// One exact transport observation; requested writes are not proof of delivery.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum Event<'a> {
    /// Opening the selected endpoint was requested; this is not protocol proof.
    OpenRequested { path: &'a str, baud: u32 },
    /// A new host handle was obtained; firmware readiness is not established.
    OpenCompleted,
    /// No handle was obtained from the requested opening operation.
    OpenFailed { error: Failure },
    /// Bytes supplied to the transport, recorded before dispatch.
    WriteRequested { bytes: &'a [u8] },
    /// The transport accepted and flushed the entire requested write.
    WriteCompleted,
    /// The transport failed; it cannot report a partial write count.
    WriteFailed { error: Failure },
    /// Bytes actually returned from a read.
    ReadCompleted { bytes: &'a [u8] },
    /// The read failed.
    ReadFailed { error: Failure },
    /// The transport violated the read-count contract.
    InvalidReadCount { count: usize, capacity: usize },
    /// Requested serial baud change.
    BaudRequested { baud: u32 },
    /// The serial baud change succeeded.
    BaudCompleted,
    /// The serial baud change failed.
    BaudFailed { error: Failure },
    /// Connection release started; this sends no radio protocol request.
    CloseRequested,
    /// Connection release finished.
    CloseCompleted,
    /// Connection release failed.
    CloseFailed { error: Failure },
}

#[derive(Debug, Serialize)]
struct Record<E> {
    sequence: u64,
    utc_unix_nanoseconds: String,
    elapsed_microseconds: u128,
    event: E,
}

/// Recorder errors request cooperative cancellation, never interrupt cleanup.
#[derive(Debug)]
pub(super) struct Recorder<W> {
    writer: W,
    started: Instant,
    events: u64,
    error: Option<Failure>,
    cancelled: Arc<AtomicBool>,
    filename: &'static str,
}

impl<W: Write> Recorder<W> {
    fn new(writer: W, cancelled: Arc<AtomicBool>) -> Self {
        Self::named(writer, cancelled, "transcript.jsonl")
    }

    /// Create a recorder for one predefined, separately reserved transcript.
    pub(super) fn named(writer: W, cancelled: Arc<AtomicBool>, filename: &'static str) -> Self {
        Self {
            writer,
            started: Instant::now(),
            events: 0,
            error: None,
            cancelled,
            filename,
        }
    }

    /// Flush one typed transport or connection-lifecycle observation.
    pub(super) fn record(&mut self, event: impl Serialize) {
        if self.error.is_some() {
            return;
        }
        let record = Record {
            sequence: self.events,
            utc_unix_nanoseconds: OffsetDateTime::now_utc().unix_timestamp_nanos().to_string(),
            elapsed_microseconds: self.started.elapsed().as_micros(),
            event,
        };
        match self.write_record(&record) {
            Ok(()) => self.events += 1,
            Err(error) => self.fail(&error),
        }
    }

    fn fail(&mut self, error: &io::Error) {
        if self.error.is_none() {
            self.error = Some(Failure::from_error(error));
        }
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn write_record(&mut self, record: &impl Serialize) -> io::Result<()> {
        serde_json::to_writer(&mut self.writer, record)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }

    /// Describe whether the entire attempted transport transcript was captured.
    pub(super) fn summary(&self) -> TranscriptSummary {
        TranscriptSummary {
            file: self.filename,
            complete: self.error.is_none(),
            events: self.events,
            error: self.error.clone(),
        }
    }

    /// Refuse a subsequent operation when any preceding capture failed.
    pub(super) fn ensure_complete(&self) -> io::Result<()> {
        self.error.as_ref().map_or(Ok(()), |error| {
            Err(io::Error::other(format!(
                "cannot accept an incomplete transcript: {error}"
            )))
        })
    }
}

impl Recorder<File> {
    /// Clone this file descriptor solely for synchronizing already flushed events.
    ///
    /// The handle must never write or seek. Cloning does not synchronize content;
    /// the caller must explicitly call `sync_all` at the intended boundary and
    /// retain any failure before permitting further protocol work.
    pub(super) fn synchronization_handle(&self) -> io::Result<File> {
        self.ensure_complete()?;
        self.writer.try_clone()
    }

    /// Flush and synchronize transcript evidence before accepting a session.
    ///
    /// A prior recording failure still makes this fail even if synchronization
    /// succeeds. A synchronization failure marks the transcript incomplete and
    /// requests cancellation without replacing the first recorded error.
    pub(super) fn synchronize(&mut self) -> io::Result<()> {
        self.synchronize_with(File::sync_all)
    }

    fn synchronize_with(
        &mut self,
        synchronize: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Err(error) = self.writer.flush().and_then(|()| synchronize(&self.writer)) {
            self.fail(&error);
            return Err(error);
        }
        self.ensure_complete()
    }
}

/// Completeness of the transcript, independent of the radio protocol outcome.
#[derive(Debug, Serialize)]
pub(super) struct TranscriptSummary {
    file: &'static str,
    /// Whether all completed transport observations were written and flushed.
    pub(super) complete: bool,
    events: u64,
    error: Option<Failure>,
}

/// Whether capture failure may interrupt subsequent protocol operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapturePolicy {
    /// Preserve transport results and allow cleanup after capture failure.
    Observational,
    /// Require capture for protocol traffic; always attempt connection release.
    Required,
}

/// Records the original transport without adding radio commands or retries.
#[derive(Debug)]
pub(super) struct CaptureTransport<T, W> {
    inner: T,
    recorder: Recorder<W>,
    policy: CapturePolicy,
}

impl<T, W> CaptureTransport<T, W> {
    /// Begin observational recording without changing transport results.
    pub(super) const fn new(inner: T, recorder: Recorder<W>) -> Self {
        Self {
            inner,
            recorder,
            policy: CapturePolicy::Observational,
        }
    }

    /// Require complete recording before further protocol traffic is allowed.
    ///
    /// Failure to record a request prevents its dispatch. Failure to record an
    /// operation's result returns an error after dispatch, so delivery or read
    /// state must be treated as uncertain. Connection release is always tried,
    /// even after capture failure; no later protocol read, write, or baud change
    /// is dispatched. This policy does not add retries or protocol cleanup.
    pub(super) const fn required(inner: T, recorder: Recorder<W>) -> Self {
        Self {
            inner,
            recorder,
            policy: CapturePolicy::Required,
        }
    }

    fn require_capture(&self, context: &str) -> io::Result<()> {
        if self.policy == CapturePolicy::Required
            && let Some(error) = &self.recorder.error
        {
            return Err(io::Error::other(format!(
                "required capture failed {context}: {error}"
            )));
        }
        Ok(())
    }

    /// Recover the recorder after closing and dropping the underlying transport.
    pub(super) fn into_recorder(self) -> Recorder<W> {
        self.recorder
    }
}

impl<T: Transport, W: Write + Send + Sync> Transport for CaptureTransport<T, W> {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.recorder.record(Event::WriteRequested { bytes: data });
        self.require_capture("before write dispatch; write was not attempted")
            .map_err(TransportError::Write)?;
        let result = self.inner.write(data).await;
        self.recorder.record(match &result {
            Ok(()) => Event::WriteCompleted,
            Err(error) => Event::WriteFailed {
                error: Failure::from_error(error),
            },
        });
        result?;
        self.require_capture("after write dispatch; delivery cannot be qualified")
            .map_err(TransportError::Write)
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        self.require_capture("before read dispatch; read was not attempted")
            .map_err(TransportError::Read)?;
        let result = self.inner.read(buf).await;
        match &result {
            Ok(count) => {
                if let Some(bytes) = buf.get(..*count) {
                    self.recorder.record(Event::ReadCompleted { bytes });
                } else {
                    self.recorder.record(Event::InvalidReadCount {
                        count: *count,
                        capacity: buf.len(),
                    });
                    if self.policy == CapturePolicy::Required {
                        return Err(TransportError::Read(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "transport returned a read count beyond the buffer capacity",
                        )));
                    }
                }
            }
            Err(error) => self.recorder.record(Event::ReadFailed {
                error: Failure::from_error(error),
            }),
        }
        let count = result?;
        self.require_capture("after read dispatch; received bytes cannot be qualified")
            .map_err(TransportError::Read)?;
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.recorder.record(Event::CloseRequested);
        let result = self.inner.close().await;
        self.recorder.record(match &result {
            Ok(()) => Event::CloseCompleted,
            Err(error) => Event::CloseFailed {
                error: Failure::from_error(error),
            },
        });
        result?;
        self.require_capture("after close dispatch; connection release was attempted")
            .map_err(TransportError::Disconnected)
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.recorder.record(Event::BaudRequested { baud });
        self.require_capture("before baud dispatch; baud change was not attempted")
            .map_err(capture_baud_error)?;
        let result = self.inner.set_baud_rate(baud);
        self.recorder.record(match &result {
            Ok(()) => Event::BaudCompleted,
            Err(error) => Event::BaudFailed {
                error: Failure::from_error(error),
            },
        });
        result?;
        self.require_capture("after baud dispatch; baud change cannot be qualified")
            .map_err(capture_baud_error)
    }
}

fn capture_baud_error(source: io::Error) -> TransportError {
    TransportError::Open {
        path: "<required capture>".to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kenwood_tmd750::transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    #[test]
    fn exclusive_directory_preserves_existing_capture() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("Probe Case");
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut original = Artifacts::create(Some(&path), Arc::clone(&cancelled))?;
        original.report.write_all(b"original report")?;
        assert!(
            Artifacts::create(Some(&path), cancelled).is_err(),
            "an existing capture must never be reused"
        );
        assert_eq!(fs::read(path.join("report.json"))?, b"original report");
        Ok(())
    }

    #[test]
    fn an_existing_empty_directory_is_also_rejected() -> TestResult {
        let root = tempfile::tempdir()?;
        assert!(
            Artifacts::create(Some(root.path()), Arc::new(AtomicBool::new(false))).is_err(),
            "even an empty output directory belongs to its existing owner"
        );
        assert_eq!(fs::read_dir(root.path())?.count(), 0);
        Ok(())
    }

    #[test]
    fn capture_file_creation_is_exclusive() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("report.json");
        create_private_file(&path)?.write_all(b"existing evidence")?;
        assert!(
            create_private_file(&path).is_err(),
            "an existing file must never be reused"
        );
        assert_eq!(fs::read(path)?, b"existing evidence");
        Ok(())
    }

    #[test]
    fn post_exit_capture_is_separate_private_and_exclusive() -> TestResult {
        let root = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let artifacts =
            Artifacts::create(Some(&root.path().join("capture")), Arc::clone(&cancelled))?;
        let mut post_exit = artifacts.reserve_post_exit(Arc::clone(&cancelled))?;
        post_exit.record(Event::WriteRequested { bytes: b"ID\r" });
        assert_eq!(artifacts.transcript.summary().file, "transcript.jsonl");
        assert_eq!(artifacts.transcript.summary().events, 0);
        assert_eq!(post_exit.summary().file, "post-exit-transcript.jsonl");
        assert_eq!(post_exit.summary().events, 1);
        let path = artifacts.directory.join("post-exit-transcript.jsonl");
        let evidence = fs::read(&path)?;
        assert!(
            artifacts.reserve_post_exit(cancelled).is_err(),
            "verification evidence must never be overwritten"
        );
        assert_eq!(fs::read(&path)?, evidence);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn captures_are_private_to_the_operator() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let capture = Artifacts::create(
            Some(&root.path().join("private")),
            Arc::new(AtomicBool::new(false)),
        )?;
        assert_eq!(
            fs::metadata(&capture.directory)?.permissions().mode() & 0o777,
            0o700
        );
        for name in ["report.json", "transcript.jsonl"] {
            assert_eq!(
                fs::metadata(capture.directory.join(name))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn captures_exact_bytes_and_operation_order() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"ID\r", b"ID TM-D750\r");
        let recorder = Recorder::new(Vec::new(), Arc::new(AtomicBool::new(false)));
        let mut transport = CaptureTransport::new(mock, recorder);
        transport.write(b"ID\r").await?;
        let mut bytes = [0; 32];
        let count = transport.read(&mut bytes).await?;
        assert_eq!(bytes.get(..count), Some(b"ID TM-D750\r".as_slice()));
        transport.set_baud_rate(9600)?;
        transport.close().await?;
        transport.inner.assert_complete();
        let records = String::from_utf8(transport.into_recorder().writer)?;
        let parsed: Vec<serde_json::Value> = records
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let events: Vec<_> = parsed
            .iter()
            .filter_map(|record| record.get("event"))
            .collect();
        let kinds: Vec<_> = events
            .iter()
            .filter_map(|event| event.get("kind"))
            .collect();
        assert_eq!(
            kinds,
            [
                "write_requested",
                "write_completed",
                "read_completed",
                "baud_requested",
                "baud_completed",
                "close_requested",
                "close_completed"
            ]
        );
        assert_eq!(
            events.first().and_then(|event| event.get("bytes")),
            Some(&serde_json::json!([73, 68, 13]))
        );
        for (index, record) in parsed.iter().enumerate() {
            assert_eq!(record.get("sequence"), Some(&serde_json::json!(index)));
            assert!(
                record
                    .get("utc_unix_nanoseconds")
                    .and_then(serde_json::Value::as_str)
                    .is_some(),
                "each event requires an exact UTC timestamp"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn zero_length_reads_are_recorded_as_observations() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect_eof(b"ID\r");
        let recorder = Recorder::new(Vec::new(), Arc::new(AtomicBool::new(false)));
        let mut transport = CaptureTransport::new(mock, recorder);
        transport.write(b"ID\r").await?;
        assert_eq!(transport.read(&mut [0; 16]).await?, 0);
        transport.inner.assert_complete();
        let records = String::from_utf8(transport.into_recorder().writer)?;
        let final_record = records.lines().last().ok_or("EOF observation missing")?;
        let parsed: serde_json::Value = serde_json::from_str(final_record)?;
        assert_eq!(
            parsed.get("event"),
            Some(&serde_json::json!({"kind": "read_completed", "bytes": []}))
        );
        Ok(())
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("capture disk failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn capture_failure_requests_cancellation_without_interrupting_io() -> TestResult {
        let cancelled = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::new(FailingWriter, Arc::clone(&cancelled));
        let mut mock = MockTransport::new();
        mock.expect(b"E", &[6]);
        let mut transport = CaptureTransport::new(mock, recorder);
        transport.write(b"E").await?;
        assert!(
            cancelled.load(Ordering::Relaxed),
            "capture failure must stop the next exchange"
        );
        let mut bytes = [0; 1];
        assert_eq!(transport.read(&mut bytes).await?, 1);
        assert_eq!(bytes, [6]);
        transport.close().await?;
        transport.inner.assert_complete();
        let summary = transport.into_recorder().summary();
        assert!(
            !summary.complete,
            "failed capture must never be reported complete"
        );
        assert_eq!(summary.events, 0);
        assert!(
            summary.error.is_some(),
            "the original disk failure must be retained"
        );
        Ok(())
    }

    #[derive(Debug)]
    struct FlushWitness {
        bytes: Vec<u8>,
        flushed: Arc<AtomicBool>,
    }

    impl Write for FlushWitness {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushed.store(true, Ordering::Relaxed);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FailedUsb {
        request_flushed: Arc<AtomicBool>,
    }

    impl Transport for FailedUsb {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            let message = if self.request_flushed.load(Ordering::Relaxed) {
                "original USB write error"
            } else {
                "request evidence was not flushed before dispatch"
            };
            Err(TransportError::Write(io::Error::other(message)))
        }

        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            Err(TransportError::Read(io::Error::other(
                "original USB read error",
            )))
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn failed_transport_preserves_pre_dispatch_evidence_and_original_errors() -> TestResult {
        let flushed = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::new(
            FlushWitness {
                bytes: Vec::new(),
                flushed: Arc::clone(&flushed),
            },
            Arc::new(AtomicBool::new(false)),
        );
        let mut transport = CaptureTransport::new(
            FailedUsb {
                request_flushed: flushed,
            },
            recorder,
        );
        let write = transport.write(b"ID\r").await;
        assert!(
            matches!(write, Err(TransportError::Write(ref source)) if source.to_string() == "original USB write error"),
            "request evidence must be flushed before dispatch, and the transport error preserved: {write:?}"
        );
        let read = transport.read(&mut [0; 8]).await;
        assert!(
            matches!(read, Err(TransportError::Read(ref source)) if source.to_string() == "original USB read error"),
            "the wrapper must preserve the original read error: {read:?}"
        );
        let recorder = transport.into_recorder();
        assert!(
            recorder.summary().complete,
            "serial failure is not capture failure"
        );
        let text = String::from_utf8(recorder.writer.bytes)?;
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        let kinds: Vec<_> = records
            .iter()
            .filter_map(|record| record.get("event")?.get("kind"))
            .collect();
        assert_eq!(kinds, ["write_requested", "write_failed", "read_failed"]);
        assert!(
            text.contains("original USB write error"),
            "write error cause must be captured"
        );
        assert!(
            text.contains("original USB read error"),
            "read error cause must be captured"
        );
        Ok(())
    }

    #[derive(Debug)]
    struct FlushLimitWriter {
        remaining: usize,
    }

    impl Write for FlushLimitWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let Some(remaining) = self.remaining.checked_sub(1) else {
                return Err(io::Error::other("capture flush failure"));
            };
            self.remaining = remaining;
            Ok(())
        }
    }

    #[tokio::test]
    async fn required_capture_failure_prevents_the_first_memory_write() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::new(FailingWriter, Arc::clone(&cancelled));
        let mut transport = CaptureTransport::required(MockTransport::new(), recorder);
        let result = transport.write(b"W\x04\xf0\x00\x01X").await;
        assert!(
            matches!(result, Err(TransportError::Write(ref source)) if source.to_string().contains("before write dispatch")),
            "a failed request capture must prevent dispatch: {result:?}"
        );
        assert!(transport.inner.writes().is_empty());
        transport.inner.assert_complete();
        assert!(cancelled.load(Ordering::Relaxed));
        assert!(!transport.recorder.summary().complete);
    }

    #[tokio::test]
    async fn required_read_capture_failure_prevents_the_next_ack() -> TestResult {
        let mut mock = MockTransport::new();
        mock.expect(b"R\x04\xf0\x00\x01", b"W\x04\xf0\x00\x01X");
        let recorder = Recorder::new(
            FlushLimitWriter { remaining: 2 },
            Arc::new(AtomicBool::new(false)),
        );
        let mut transport = CaptureTransport::required(mock, recorder);
        transport.write(b"R\x04\xf0\x00\x01").await?;
        let read = transport.read(&mut [0; 16]).await;
        assert!(
            matches!(read, Err(TransportError::Read(ref source)) if source.to_string().contains("after read dispatch")),
            "unrecorded received bytes must not look successful: {read:?}"
        );
        let ack = transport.write(&[6]).await;
        assert!(matches!(ack, Err(TransportError::Write(_))));
        assert_eq!(transport.inner.writes(), [b"R\x04\xf0\x00\x01".to_vec()]);
        transport.inner.assert_complete();
        Ok(())
    }

    #[tokio::test]
    async fn required_pre_write_capture_failure_stops_traffic_after_a_successful_read() -> TestResult
    {
        let mut mock = MockTransport::new();
        mock.queue_read(&[6]);
        let recorder = Recorder::new(
            FlushLimitWriter { remaining: 1 },
            Arc::new(AtomicBool::new(false)),
        );
        let mut transport = CaptureTransport::required(mock, recorder);
        assert_eq!(transport.read(&mut [0; 1]).await?, 1);
        let write = transport.write(b"W\x04\xf0\x00\x01X").await;
        assert!(
            matches!(write, Err(TransportError::Write(ref source)) if source.to_string().contains("before write dispatch")),
            "capture can fail at a new request after prior evidence succeeded: {write:?}"
        );
        assert!(transport.inner.writes().is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn required_write_completion_capture_failure_is_uncertain_and_stops_retries() {
        let request = b"W\x04\xf0\x00\x01X";
        let mut mock = MockTransport::new();
        mock.expect(request, b"");
        let recorder = Recorder::new(
            FlushLimitWriter { remaining: 1 },
            Arc::new(AtomicBool::new(false)),
        );
        let mut transport = CaptureTransport::required(mock, recorder);
        let write = transport.write(request).await;
        assert!(
            matches!(write, Err(TransportError::Write(ref source)) if source.to_string().contains("after write dispatch; delivery cannot be qualified")),
            "post-dispatch evidence failure must not imply that no write occurred: {write:?}"
        );
        assert!(transport.write(request).await.is_err());
        assert_eq!(transport.inner.writes(), [request.to_vec()]);
        transport.inner.assert_complete();
    }

    #[derive(Debug, Default)]
    struct OperationWitness {
        operations: Vec<&'static str>,
    }

    impl Transport for OperationWitness {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            self.operations.push("write");
            Ok(())
        }

        async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, TransportError> {
            self.operations.push("read");
            Ok(0)
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            self.operations.push("close");
            Ok(())
        }

        fn set_baud_rate(&mut self, _baud: u32) -> Result<(), TransportError> {
            self.operations.push("baud");
            Ok(())
        }
    }

    #[tokio::test]
    async fn required_capture_failure_blocks_every_protocol_operation_but_still_closes() {
        let mut recorder = Recorder::new(FailingWriter, Arc::new(AtomicBool::new(false)));
        recorder.record(Event::WriteRequested { bytes: b"ID\r" });
        let mut transport = CaptureTransport::required(OperationWitness::default(), recorder);
        assert!(matches!(
            transport.read(&mut [0; 1]).await,
            Err(TransportError::Read(_))
        ));
        assert!(matches!(
            transport.write(b"E").await,
            Err(TransportError::Write(_))
        ));
        assert!(matches!(
            transport.set_baud_rate(9600),
            Err(TransportError::Open { .. })
        ));
        assert!(transport.inner.operations.is_empty());
        let close = transport.close().await;
        assert!(matches!(close, Err(TransportError::Disconnected(_))));
        assert_eq!(transport.inner.operations, ["close"]);
    }

    struct InvalidReadCountTransport;

    impl Transport for InvalidReadCountTransport {
        async fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            Ok(())
        }

        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
            Ok(buffer.len() + 1)
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn required_read_rejects_impossible_counts_without_changing_observational_policy()
    -> TestResult {
        for policy in [CapturePolicy::Observational, CapturePolicy::Required] {
            let mut transport = CaptureTransport {
                inner: InvalidReadCountTransport,
                recorder: Recorder::new(Vec::new(), Arc::new(AtomicBool::new(false))),
                policy,
            };
            let result = transport.read(&mut [0; 1]).await;
            match policy {
                CapturePolicy::Observational => assert_eq!(result?, 2),
                CapturePolicy::Required => assert!(matches!(
                    result,
                    Err(TransportError::Read(ref error)) if error.kind() == io::ErrorKind::InvalidData
                )),
            }
            assert!(transport.recorder.summary().complete);
            let records = String::from_utf8(transport.into_recorder().writer)?;
            assert!(records.contains("invalid_read_count"));
            assert!(!records.contains("read_completed"));
        }
        Ok(())
    }

    #[test]
    fn required_baud_capture_is_checked_before_and_after_dispatch() {
        for remaining in [0, 1] {
            let recorder = Recorder::new(
                FlushLimitWriter { remaining },
                Arc::new(AtomicBool::new(false)),
            );
            let mut transport = CaptureTransport::required(OperationWitness::default(), recorder);
            let baud = transport.set_baud_rate(9600);
            assert!(matches!(baud, Err(TransportError::Open { .. })));
            assert_eq!(transport.inner.operations.len(), remaining);
            assert!(transport.set_baud_rate(9600).is_err());
            assert_eq!(transport.inner.operations.len(), remaining);
        }
    }

    #[tokio::test]
    async fn required_capture_preserves_original_write_error_when_completion_capture_fails() {
        let recorder = Recorder::new(
            FlushLimitWriter { remaining: 1 },
            Arc::new(AtomicBool::new(false)),
        );
        let mut transport = CaptureTransport::required(
            FailedUsb {
                request_flushed: Arc::new(AtomicBool::new(true)),
            },
            recorder,
        );
        let write = transport.write(b"ID\r").await;
        assert!(
            matches!(write, Err(TransportError::Write(ref source)) if source.to_string() == "original USB write error"),
            "capture failure must not replace the original transport error: {write:?}"
        );
        assert!(!transport.recorder.summary().complete);
    }

    #[tokio::test]
    async fn required_capture_preserves_original_read_error_when_capture_also_fails() {
        let recorder = Recorder::new(FailingWriter, Arc::new(AtomicBool::new(false)));
        let mut transport = CaptureTransport::required(
            FailedUsb {
                request_flushed: Arc::new(AtomicBool::new(true)),
            },
            recorder,
        );
        let read = transport.read(&mut [0; 1]).await;
        assert!(
            matches!(read, Err(TransportError::Read(ref source)) if source.to_string() == "original USB read error"),
            "capture failure must not replace the original read error: {read:?}"
        );
        assert!(!transport.recorder.summary().complete);
    }

    #[test]
    fn transcript_synchronization_keeps_successful_named_evidence_complete() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("trial-session.jsonl");
        let mut recorder = Recorder::named(
            create_private_file(&path)?,
            Arc::new(AtomicBool::new(false)),
            "trial-session.jsonl",
        );
        recorder.record(Event::CloseCompleted);
        recorder.synchronize()?;
        assert_eq!(recorder.summary().file, "trial-session.jsonl");
        assert_eq!(recorder.summary().events, 1);
        assert!(recorder.summary().complete);
        assert!(!fs::read(path)?.is_empty());
        Ok(())
    }

    #[test]
    fn transcript_synchronization_cannot_clear_a_prior_failure() -> TestResult {
        let root = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut recorder = Recorder::named(
            create_private_file(&root.path().join("trial-session.jsonl"))?,
            Arc::clone(&cancelled),
            "trial-session.jsonl",
        );
        recorder.fail(&io::Error::other("original capture failure"));
        assert!(recorder.synchronize().is_err());
        assert!(cancelled.load(Ordering::Relaxed));
        let summary = recorder.summary();
        assert!(!summary.complete);
        assert_eq!(
            summary.error.ok_or("original failure was lost")?.message,
            "original capture failure"
        );
        Ok(())
    }

    #[test]
    fn transcript_synchronization_failure_marks_evidence_incomplete_and_requests_cancellation()
    -> TestResult {
        let root = tempfile::tempdir()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut recorder = Recorder::named(
            create_private_file(&root.path().join("trial-session.jsonl"))?,
            Arc::clone(&cancelled),
            "trial-session.jsonl",
        );
        recorder.record(Event::CloseCompleted);
        let result =
            recorder.synchronize_with(|_file| Err(io::Error::other("synchronization failure")));
        assert!(result.is_err());
        assert!(cancelled.load(Ordering::Relaxed));
        let summary = recorder.summary();
        assert!(!summary.complete);
        assert_eq!(summary.events, 1);
        assert_eq!(
            summary
                .error
                .ok_or("synchronization failure was lost")?
                .message,
            "synchronization failure"
        );
        assert!(recorder.synchronize().is_err());
        Ok(())
    }
}
