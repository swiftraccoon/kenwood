//! Exact USB control ownership with shared CAT observation and bounded retirement.

use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::transport::{DEFAULT_BAUD, SerialCandidate};
use kenwood_tmd750::{DvGatewayMode, Identity};
use kenwood_transport::Transport;
use serde::Serialize;

use crate::capture::{CaptureTransport, Event, Failure, Recorder, TranscriptSummary};
use crate::mcp::reconnect::{Backend, endpoint_is_unambiguous};
use crate::native::cat;
use crate::{AppResult, CommandError};

/// Cleanup and required capture are independent of protocol success.
#[derive(Clone, Debug, Serialize)]
pub(super) struct Retirement {
    pub(super) close_error: Option<Failure>,
    pub(super) capture_error: Option<Failure>,
    pub(super) transcript: TranscriptSummary,
}

impl Retirement {
    /// A dropped owner alone cannot satisfy either cleanup or capture.
    pub(super) const fn succeeded(&self) -> bool {
        self.close_error.is_none() && self.capture_error.is_none() && self.transcript.complete
    }
}

/// Failed opening admission retains its primary cause and independent cleanup.
#[derive(Debug, Serialize)]
pub(super) struct OpenFailure {
    pub(super) primary: Failure,
    pub(super) retirement: Retirement,
}

impl std::fmt::Display for OpenFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "USB control admission failed: {}", self.primary)?;
        for (stage, error) in [
            ("close", &self.retirement.close_error),
            ("capture", &self.retirement.capture_error),
        ] {
            if let Some(error) = error {
                write!(formatter, "; {stage}: {error}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for OpenFailure {}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AdmissionEvent<'a> {
    ControlEndpointVerified {
        path: &'a str,
        usb_vendor_id: Option<u16>,
        usb_product_id: Option<u16>,
    },
    ControlAdmissionFailed {
        error: &'a Failure,
    },
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "USB control admission cancelled",
        ))
    } else {
        Ok(())
    }
}

fn prepare<B: Backend>(
    backend: &mut B,
    endpoint: &SerialCandidate,
    recorder: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> AppResult<B::Connection> {
    recorder.record(Event::OpenRequested {
        path: &endpoint.path,
        baud: DEFAULT_BAUD,
    });
    recorder.synchronize()?;
    check_cancelled(cancelled)?;
    let candidates = backend.enumerate()?;
    let selected = crate::dstar::probe::select_endpoint(&endpoint.path, candidates.clone())?;
    if selected != *endpoint || !endpoint_is_unambiguous(&selected, &candidates) {
        return Err(CommandError(
            "USB control endpoint metadata changed or became ambiguous; no substitute was selected"
                .to_owned(),
        )
        .into());
    }
    recorder.record(AdmissionEvent::ControlEndpointVerified {
        path: &selected.path,
        usb_vendor_id: selected.vid,
        usb_product_id: selected.pid,
    });
    recorder.synchronize()?;
    check_cancelled(cancelled)?;
    match backend.open(&selected, DEFAULT_BAUD) {
        Ok(connection) => {
            recorder.record(Event::OpenCompleted);
            Ok(connection)
        }
        Err(error) => {
            recorder.record(Event::OpenFailed {
                error: Failure::from_error(&error),
            });
            Err(error.into())
        }
    }
}

fn finish_capture(mut recorder: Recorder<File>, close_error: Option<Failure>) -> Retirement {
    let capture_error = recorder
        .synchronize()
        .err()
        .as_ref()
        .map(|error| Failure::from_error(error));
    Retirement {
        close_error,
        capture_error,
        transcript: recorder.summary(),
    }
}

/// Always close once within the shared two-second bound, then drop and sync.
pub(super) async fn retire<T: Transport>(mut transport: CaptureTransport<T, File>) -> Retirement {
    let close_error = crate::native::close(&mut transport).await;
    finish_capture(transport.into_recorder(), close_error)
}

async fn admit<T: Transport>(
    mut transport: CaptureTransport<T, File>,
    cancelled: &AtomicBool,
) -> AppResult<CaptureTransport<T, File>> {
    if let Err(error) = transport
        .synchronize()
        .and_then(|()| check_cancelled(cancelled))
    {
        return Err(OpenFailure {
            primary: Failure::from_error(&error),
            retirement: retire(transport).await,
        }
        .into());
    }
    Ok(transport)
}

/// Open only fresh, unambiguous metadata for the exact selected USB endpoint.
///
/// Acquired owners that fail cancellation or capture admission are closed and
/// dropped before the error returns. This function sends no radio commands.
pub(super) async fn open<B: Backend>(
    backend: &mut B,
    endpoint: &SerialCandidate,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> AppResult<CaptureTransport<B::Connection, File>> {
    match prepare(backend, endpoint, &mut recorder, cancelled) {
        Ok(connection) => admit(CaptureTransport::required(connection, recorder), cancelled).await,
        Err(error) => {
            let primary = Failure::from_error(error.as_ref());
            recorder.record(AdmissionEvent::ControlAdmissionFailed { error: &primary });
            Err(OpenFailure {
                primary,
                retirement: finish_capture(recorder, None),
            }
            .into())
        }
    }
}

/// Observe Gateway CAT through the shared query and independent cleanup logic.
pub(super) async fn observe<B: Backend>(
    backend: &mut B,
    endpoint: &SerialCandidate,
    expected_identity: Option<&Identity>,
    expected_gateway: Option<DvGatewayMode>,
    recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> AppResult<cat::Observation> {
    let transport = open(backend, endpoint, recorder, cancelled).await?;
    Ok(cat::observe(
        transport,
        cat::Request {
            scope: cat::Scope::Gateway,
            expected_identity,
            expected_gateway,
        },
        cancelled,
    )
    .await)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID, TMD750_PANEL_PID};
    use kenwood_tmd750::{FirmwareIdentity, RadioModel, RadioType};
    use kenwood_transport::{MockTransport, TransportError};

    use super::*;

    type TestResult = AppResult<()>;

    #[derive(Debug, PartialEq, Eq)]
    enum Seen {
        Enumerate,
        Open(String, u32),
        Write(Vec<u8>),
        Close,
        Drop,
    }

    type Log = Arc<Mutex<Vec<Seen>>>;

    fn record(log: &Log, event: Seen) -> Result<(), TransportError> {
        log.lock()
            .map_err(|_| TransportError::Read(io::Error::other("test log poisoned")))?
            .push(event);
        Ok(())
    }

    enum Release {
        Clean,
        Failed,
        Pending,
    }

    struct Connection {
        script: MockTransport,
        log: Log,
        release: Release,
    }

    impl Transport for Connection {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
            record(&self.log, Seen::Write(bytes.to_vec()))?;
            self.script.write(bytes).await
        }

        async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
            self.script.read(bytes).await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            record(&self.log, Seen::Close)?;
            self.script.assert_complete();
            match self.release {
                Release::Clean => Ok(()),
                Release::Failed => Err(TransportError::Disconnected(io::Error::other(
                    "scripted independent close failure",
                ))),
                Release::Pending => std::future::pending().await,
            }
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            if let Ok(mut events) = self.log.lock() {
                events.push(Seen::Drop);
            }
        }
    }

    struct FakeBackend {
        candidates: Vec<SerialCandidate>,
        connection: Option<Connection>,
        log: Log,
        cancelled: Arc<AtomicBool>,
        cancel_on_open: bool,
    }

    impl Backend for FakeBackend {
        type Connection = Connection;

        fn open(
            &mut self,
            endpoint: &SerialCandidate,
            baud: u32,
        ) -> Result<Connection, TransportError> {
            record(&self.log, Seen::Open(endpoint.path.clone(), baud))?;
            if self.cancel_on_open {
                self.cancelled.store(true, Ordering::Relaxed);
            }
            self.connection.take().ok_or_else(|| TransportError::Open {
                path: endpoint.path.clone(),
                source: io::Error::other("scripted opening failure"),
            })
        }

        fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
            record(&self.log, Seen::Enumerate)?;
            Ok(self.candidates.clone())
        }

        fn now(&self) -> Duration {
            Duration::ZERO
        }

        async fn wait(&mut self, _duration: Duration) {}
    }

    struct Harness {
        directory: tempfile::TempDir,
        recorder: Option<Recorder<File>>,
        backend: FakeBackend,
    }

    fn endpoint(path: &str, pid: u16) -> SerialCandidate {
        SerialCandidate {
            path: path.to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(pid),
        }
    }

    impl Harness {
        fn new(script: MockTransport, release: Release) -> AppResult<Self> {
            let directory = tempfile::tempdir()?;
            let log = Arc::new(Mutex::new(Vec::new()));
            let cancelled = Arc::new(AtomicBool::new(false));
            let recorder = Recorder::named(
                crate::capture::create_private_file(&directory.path().join("control.jsonl"))?,
                Arc::clone(&cancelled),
                "control.jsonl",
            );
            Ok(Self {
                directory,
                recorder: Some(recorder),
                backend: FakeBackend {
                    candidates: vec![endpoint("/dev/cu.usbmodem1", TMD750_MAIN_PID)],
                    connection: Some(Connection {
                        script,
                        log: Arc::clone(&log),
                        release,
                    }),
                    log,
                    cancelled,
                    cancel_on_open: false,
                },
            })
        }

        fn recorder(&mut self) -> AppResult<Recorder<File>> {
            self.recorder
                .take()
                .ok_or_else(|| "capture already consumed".into())
        }

        fn readonly(&self) -> AppResult<Recorder<File>> {
            Ok(Recorder::named(
                File::open(self.directory.path().join("control.jsonl"))?,
                Arc::clone(&self.backend.cancelled),
                "control.jsonl",
            ))
        }

        async fn open(&mut self) -> AppResult<CaptureTransport<Connection, File>> {
            let recorder = self.recorder()?;
            let cancelled = Arc::clone(&self.backend.cancelled);
            open(
                &mut self.backend,
                &endpoint("/dev/cu.usbmodem1", TMD750_MAIN_PID),
                recorder,
                &cancelled,
            )
            .await
        }
    }

    fn identity(firmware: &str) -> AppResult<Identity> {
        Ok(Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new(firmware)?,
            radio_type: RadioType::new("K,2,1")?,
        })
    }

    fn cat_script(gateway: Option<&[u8]>) -> MockTransport {
        let mut script = MockTransport::new();
        script.expect(b"ID\r", b"ID TM-D750\r");
        script.expect(b"FV\r", b"FV 1.02\r");
        script.expect(b"TY\r", b"TY K,2,1\r");
        if let Some(reply) = gateway {
            script.expect(b"GW\r", reply);
        }
        script
    }

    #[tokio::test]
    async fn observation_reuses_exact_gateway_schedule_and_retires_usb_owner() -> TestResult {
        let mut harness = Harness::new(cat_script(Some(b"GW 0\r")), Release::Clean)?;
        let recorder = harness.recorder()?;
        let cancelled = Arc::clone(&harness.backend.cancelled);
        let observed = observe(
            &mut harness.backend,
            &endpoint("/dev/cu.usbmodem1", TMD750_MAIN_PID),
            Some(&identity("1.02")?),
            Some(DvGatewayMode::Off),
            recorder,
            &cancelled,
        )
        .await?;
        assert!(observed.succeeded(), "{observed:?}");
        let events = harness
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?;
        assert_eq!(
            *events,
            [
                Seen::Enumerate,
                Seen::Open("/dev/cu.usbmodem1".to_owned(), DEFAULT_BAUD),
                Seen::Write(b"ID\r".to_vec()),
                Seen::Write(b"FV\r".to_vec()),
                Seen::Write(b"TY\r".to_vec()),
                Seen::Write(b"GW\r".to_vec()),
                Seen::Close,
                Seen::Drop,
            ]
        );
        drop(events);
        let transcript = std::fs::read_to_string(harness.directory.path().join("control.jsonl"))?;
        let kinds: Vec<_> = transcript
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line)?;
                Ok::<_, serde_json::Error>(row.pointer("/event/kind").cloned())
            })
            .collect::<Result<_, _>>()?;
        assert_eq!(
            kinds.get(..3),
            Some(
                [
                    Some(serde_json::json!("open_requested")),
                    Some(serde_json::json!("control_endpoint_verified")),
                    Some(serde_json::json!("open_completed")),
                ]
                .as_slice()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn changed_or_ambiguous_metadata_never_opens_control() -> TestResult {
        let selected = endpoint("/dev/cu.usbmodem1", TMD750_MAIN_PID);
        for candidates in [
            Vec::new(),
            vec![endpoint("/dev/tty.usbmodem1", TMD750_MAIN_PID)],
            vec![endpoint(&selected.path, TMD750_PANEL_PID)],
            vec![selected.clone(), selected.clone()],
            vec![
                selected.clone(),
                endpoint("/dev/cu.usbmodem2", TMD750_MAIN_PID),
            ],
            vec![
                selected.clone(),
                endpoint("/dev/tty.usbmodem1", TMD750_PANEL_PID),
            ],
        ] {
            let mut harness = Harness::new(MockTransport::new(), Release::Clean)?;
            harness.backend.candidates = candidates;
            let result = harness.open().await;
            assert!(result.is_err());
            assert_eq!(
                *harness
                    .backend
                    .log
                    .lock()
                    .map_err(|_| "test log poisoned")?,
                [Seen::Enumerate]
            );
            assert!(harness.backend.connection.is_some());
        }
        Ok(())
    }

    #[tokio::test]
    async fn matching_callout_and_dialin_pair_preserves_exact_selected_path() -> TestResult {
        let mut harness = Harness::new(MockTransport::new(), Release::Clean)?;
        harness
            .backend
            .candidates
            .push(endpoint("/dev/tty.usbmodem1", TMD750_MAIN_PID));
        let transport = harness.open().await?;
        assert!(retire(transport).await.succeeded());
        assert_eq!(
            *harness
                .backend
                .log
                .lock()
                .map_err(|_| "test log poisoned")?,
            [
                Seen::Enumerate,
                Seen::Open("/dev/cu.usbmodem1".to_owned(), DEFAULT_BAUD),
                Seen::Close,
                Seen::Drop,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_and_uncaptured_intent_prevent_backend_access() -> TestResult {
        for readonly in [false, true] {
            let mut harness = Harness::new(MockTransport::new(), Release::Clean)?;
            if readonly {
                harness.recorder = Some(harness.readonly()?);
            } else {
                harness.backend.cancelled.store(true, Ordering::Relaxed);
            }
            assert!(harness.open().await.is_err());
            assert!(
                harness
                    .backend
                    .log
                    .lock()
                    .map_err(|_| "test log poisoned")?
                    .is_empty()
            );
            assert!(harness.backend.connection.is_some());
        }
        Ok(())
    }

    #[tokio::test]
    async fn cancellation_after_open_closes_and_drops_without_cat() -> TestResult {
        let mut harness = Harness::new(MockTransport::new(), Release::Failed)?;
        harness.backend.cancel_on_open = true;
        let result = harness.open().await;
        let error = result.err().ok_or("cancelled owner admitted")?;
        let failure = error
            .downcast_ref::<OpenFailure>()
            .ok_or("cleanup evidence absent")?;
        assert!(failure.primary.message.contains("cancelled"));
        assert!(failure.retirement.close_error.is_some());
        assert!(!failure.retirement.succeeded());
        assert_eq!(
            *harness
                .backend
                .log
                .lock()
                .map_err(|_| "test log poisoned")?,
            [
                Seen::Enumerate,
                Seen::Open("/dev/cu.usbmodem1".to_owned(), DEFAULT_BAUD),
                Seen::Close,
                Seen::Drop,
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn uncaptured_acquired_owner_still_preserves_independent_close_failure() -> TestResult {
        let mut harness = Harness::new(MockTransport::new(), Release::Failed)?;
        let mut recorder = harness.readonly()?;
        recorder.record(Event::OpenCompleted);
        let connection = harness
            .backend
            .connection
            .take()
            .ok_or("connection absent")?;
        let result = admit(
            CaptureTransport::required(connection, recorder),
            &harness.backend.cancelled,
        )
        .await;
        let error = result.err().ok_or("uncaptured owner admitted")?;
        let failure = error
            .downcast_ref::<OpenFailure>()
            .ok_or("cleanup evidence absent")?;
        assert!(failure.retirement.close_error.is_some());
        assert!(failure.retirement.capture_error.is_some());
        assert!(!failure.retirement.transcript.complete);
        assert_eq!(
            *harness
                .backend
                .log
                .lock()
                .map_err(|_| "test log poisoned")?,
            [Seen::Close, Seen::Drop]
        );
        Ok(())
    }

    #[tokio::test]
    async fn close_timeout_still_drops_owner_and_synchronizes_capture() -> TestResult {
        let mut harness = Harness::new(MockTransport::new(), Release::Pending)?;
        let transport = harness.open().await?;
        let retired = retire(transport).await;
        assert_eq!(crate::native::CLOSE_BUDGET, Duration::from_secs(2));
        assert!(!retired.succeeded());
        assert!(retired.close_error.is_some());
        assert!(retired.capture_error.is_none());
        assert!(retired.transcript.complete);
        let events = harness
            .backend
            .log
            .lock()
            .map_err(|_| "test log poisoned")?;
        assert_eq!(events.get(2..), Some([Seen::Close, Seen::Drop].as_slice()));
        drop(events);
        assert!(
            serde_json::to_value(&retired)?
                .get("close_error")
                .is_some_and(serde_json::Value::is_object)
        );
        Ok(())
    }

    #[tokio::test]
    async fn identity_or_gateway_mismatch_returns_shared_observation_evidence() -> TestResult {
        for (firmware, gateway) in [("1.03", None), ("1.02", Some(b"GW 2\r".as_slice()))] {
            let mut harness = Harness::new(cat_script(gateway), Release::Clean)?;
            let recorder = harness.recorder()?;
            let cancelled = Arc::clone(&harness.backend.cancelled);
            let observed = observe(
                &mut harness.backend,
                &endpoint("/dev/cu.usbmodem1", TMD750_MAIN_PID),
                Some(&identity(firmware)?),
                Some(DvGatewayMode::Off),
                recorder,
                &cancelled,
            )
            .await?;
            assert!(!observed.succeeded());
            assert!(observed.operation_error.is_some());
            assert!(observed.close_error.is_none());
            assert!(observed.capture_error.is_none());
            let events = harness
                .backend
                .log
                .lock()
                .map_err(|_| "test log poisoned")?;
            assert!(matches!(events.last(), Some(Seen::Drop)));
            drop(events);
        }
        Ok(())
    }
}
