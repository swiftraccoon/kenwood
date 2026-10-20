//! Read-only CAT verification on a fresh connection after an MCP exit.
//!
//! The library readiness engine waits out the settle delay, polls enumeration
//! for the selected endpoint, opens it, reads the ID/FV/TY tuple and
//! optionally one `GW` query, then closes; [`host`] records each of its host
//! calls in the verification transcript and [`readiness`] serializes its
//! report. Nothing here re-enters MCP, writes a setting, or reopens a
//! transport.

pub(crate) use readiness::{ReadinessVerification, verify_readiness, verify_readiness_gateway_off};

use std::fs::File;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kenwood_tmd750::radio::readiness::{
    ReadinessError, SETTLE, SETTLE_BUDGET, SETTLE_QUIET, SettleOutcome,
    await_continuous_enumeration,
};
use kenwood_tmd750::transport::reenumeration::{ReenumerationRejection, macos_service_name};
use kenwood_tmd750::transport::{SerialCandidate, SerialTransport, discover_serial, open_serial};
use kenwood_tmd750::{DvGatewayMode, Identity};
use kenwood_transport::{Transport, TransportError};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use self::host::SettleHost;
use super::IdentityEvidence;
use super::link::NativeVerification;
use crate::capture::{Failure, Recorder, TranscriptSummary};

#[cfg(test)]
#[path = "reconnect/gateway_tests.rs"]
mod gateway_tests;
mod host;
mod readiness;
#[cfg(test)]
#[path = "reconnect/readiness_tests.rs"]
mod readiness_tests;

/// Why an endpoint did not settle within the budget.
#[derive(Debug)]
pub(super) enum SettleFailure {
    /// Cancellation was requested before the endpoint settled.
    Cancelled,
    /// The endpoint was not continuously present for `SETTLE_QUIET` within
    /// `SETTLE_BUDGET`.
    BudgetExhausted,
    /// Fresh metadata no longer uniquely selects the endpoint.
    EndpointRejected(ReenumerationRejection),
    /// Port enumeration failed.
    Enumeration(TransportError),
    /// The transcript could not record the wait.
    Capture(std::io::Error),
    /// Another failure of the library settle wait.
    Readiness(ReadinessError),
}

impl std::fmt::Display for SettleFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("cancelled before the endpoint settled"),
            Self::BudgetExhausted => write!(
                formatter,
                "endpoint was not continuously present for {} s within the {} s settle budget",
                SETTLE_QUIET.as_secs(),
                SETTLE_BUDGET.as_secs()
            ),
            Self::EndpointRejected(error) => {
                write!(formatter, "endpoint selection failed: {error}")
            }
            Self::Enumeration(error) => write!(formatter, "port enumeration failed: {error}"),
            Self::Capture(error) => write!(formatter, "transcript capture failed: {error}"),
            Self::Readiness(error) => write!(formatter, "settle wait failed: {error}"),
        }
    }
}

impl std::error::Error for SettleFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EndpointRejected(error) => Some(error),
            Self::Enumeration(error) => Some(error),
            Self::Capture(error) => Some(error),
            Self::Readiness(error) => Some(error),
            Self::Cancelled | Self::BudgetExhausted => None,
        }
    }
}

/// Host operations a verification needs: open, enumerate, clock and wait.
///
/// `Send` because the library readiness engine's host futures are `Send`.
/// Tests supply a backend that touches no serial port.
pub(crate) trait Backend: Send {
    /// Connection returned by `open`; it is never reopened.
    type Connection: Transport;
    /// Open exactly the supplied endpoint.
    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: std::num::NonZeroU32,
    ) -> Result<Self::Connection, TransportError>;
    /// Observe port metadata without opening any port.
    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError>;
    /// Monotonic elapsed time from this backend's origin.
    fn now(&self) -> Duration;
    /// Wait without radio I/O.
    fn wait(&mut self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// [`Backend`] over the host serial layer and the Tokio clock.
#[derive(Debug)]
pub(crate) struct SystemBackend {
    started: Instant,
}

impl SystemBackend {
    /// Start the monotonic clock that `now` measures elapsed time from.
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Backend for SystemBackend {
    type Connection = SerialTransport;

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: std::num::NonZeroU32,
    ) -> Result<Self::Connection, TransportError> {
        open_serial(&endpoint.path, baud)
    }

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        discover_serial()
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }

    async fn wait(&mut self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

/// Why a requested verification was never started.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SkipReason {
    /// Original connection could not be established.
    OriginalOpenFailed,
    /// Original probe did not produce both complete fragments and exit ACK.
    OriginalProbeIncomplete,
    /// Standard configuration pages or the acknowledged MCP exit are incomplete.
    OriginalBackupIncomplete,
    /// A bounded settings update did not acknowledge its MCP exit.
    OriginalUpdateIncomplete,
    /// The update's recovery journal failed an append or a synchronization.
    OriginalUpdateJournalIncomplete,
    /// Original connection release did not succeed.
    OriginalCloseFailed,
    /// Original capture was incomplete.
    OriginalCaptureIncomplete,
    /// The link's check before programming entry failed on the original
    /// connection, so programming was never entered.
    OriginalAdmissionFailed,
    /// The hold of the original connection after the exit failed to record.
    PostExitHoldFailed,
    /// Cancellation was requested before verification began.
    Cancelled,
}

/// A failed step of the fresh, read-only connection attempt.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum VerificationStage {
    /// Passive enumeration failed or expired.
    Enumeration,
    /// Fresh metadata did not uniquely retain the selected endpoint.
    EndpointSelection,
    /// The fresh open failed.
    Open,
    /// CAT identity could not be read completely.
    Identity,
    /// Complete CAT identity differed from the original tuple.
    IdentityMismatch,
    /// The requested fresh Gateway state could not be read completely.
    Gateway,
    /// The fresh Gateway state was not the required Off state.
    GatewayMismatch,
    /// The fresh connection could not be closed successfully.
    Close,
    /// Writing the transcript, or synchronizing it to storage, failed.
    Capture,
    /// The bounded identity-readiness dispatch window or attempt cap expired.
    Readiness,
}

/// Final result of one post-exit verification.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum VerificationOutcome {
    /// Requested fresh CAT observations matched, and the connection closed.
    Matched,
    /// No fresh connection was eligible to be opened.
    Skipped { reason: SkipReason },
    /// Current work finished and no subsequent attempt was started.
    Cancelled,
    /// First failed stage; later close errors stay in the attempt record.
    Failed {
        stage: VerificationStage,
        error: Failure,
    },
}

impl std::fmt::Display for VerificationOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Matched => formatter.write_str("endpoint and CAT identity tuple matched"),
            Self::Cancelled => formatter.write_str("cancelled after completing current work"),
            Self::Skipped { reason } => formatter.write_str(match reason {
                SkipReason::OriginalOpenFailed => "original connection did not open",
                SkipReason::OriginalProbeIncomplete => "original MCP probe was incomplete",
                SkipReason::OriginalBackupIncomplete => "original MCP backup was incomplete",
                SkipReason::OriginalUpdateIncomplete => "original text update was incomplete",
                SkipReason::OriginalUpdateJournalIncomplete => "text update journal was incomplete",
                SkipReason::OriginalCloseFailed => "original connection close failed",
                SkipReason::OriginalCaptureIncomplete => "original capture was incomplete",
                SkipReason::OriginalAdmissionFailed => {
                    "original connection failed its check before programming entry"
                }
                SkipReason::PostExitHoldFailed => "post-exit hold failed to record",
                SkipReason::Cancelled => "cancelled before verification",
            }),
            Self::Failed { stage, error } => {
                let stage = match stage {
                    VerificationStage::Enumeration => "passive enumeration",
                    VerificationStage::EndpointSelection => "endpoint selection",
                    VerificationStage::Open => "fresh connection open",
                    VerificationStage::Identity => "fresh CAT identity read",
                    VerificationStage::IdentityMismatch => "identity comparison",
                    VerificationStage::Gateway => "fresh Gateway read",
                    VerificationStage::GatewayMismatch => "Gateway Off comparison",
                    VerificationStage::Close => "fresh connection close",
                    VerificationStage::Capture => "required transcript capture",
                    VerificationStage::Readiness => "bounded CAT readiness",
                };
                write!(formatter, "{stage} failed: {error}")
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct ObservedEndpoint {
    path: String,
    usb_vendor_id: Option<u16>,
    usb_product_id: Option<u16>,
}

impl From<&SerialCandidate> for ObservedEndpoint {
    fn from(endpoint: &SerialCandidate) -> Self {
        Self {
            path: endpoint.path.clone(),
            usb_vendor_id: endpoint.vid,
            usb_product_id: endpoint.pid,
        }
    }
}

#[derive(Debug, Serialize)]
struct Enumeration {
    elapsed_milliseconds: u64,
    candidates: Vec<ObservedEndpoint>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum OperationOutcome {
    Succeeded,
    Failed { error: Failure },
}

/// Typed identity serialized in the report's model/firmware/type shape.
#[derive(Debug)]
struct ObservedIdentity(Identity);

impl Serialize for ObservedIdentity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        IdentityEvidence::from(&self.0).serialize(serializer)
    }
}

/// Observed DV Gateway mode, serialized as a name plus the raw wire byte.
///
/// A value with no name serializes as `unqualified` with its byte preserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GatewayEvidence(DvGatewayMode);

impl Serialize for GatewayEvidence {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let name = match self.0 {
            DvGatewayMode::Off => "off",
            DvGatewayMode::Terminal => "terminal",
            DvGatewayMode::Unnamed(_) => "unqualified",
        };
        let mut state = serializer.serialize_struct("GatewayEvidence", 2)?;
        state.serialize_field("state", name)?;
        state.serialize_field("raw", &u8::from(self.0))?;
        state.end()
    }
}

#[derive(Debug, Serialize)]
struct ConnectionAttempt {
    endpoint: ObservedEndpoint,
    open: OperationOutcome,
    identity: Option<ObservedIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gateway_mode: Option<GatewayEvidence>,
    close: Option<OperationOutcome>,
}

/// The post-exit CAT check of one session: the bounded silent-`ID` retry on
/// either USB role, or a fresh native Bluetooth connection on the pinned
/// channel.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum PostExit {
    /// Up to six opens within the readiness budget.
    Readiness(Box<ReadinessVerification>),
    /// One native reopen with the identity tuple and Gateway Off.
    Native(Box<NativeVerification>),
}

impl PostExit {
    /// A check that never started, naming `reason`.
    pub(super) fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self::Readiness(Box::new(ReadinessVerification::skipped(reason, transcript)))
    }

    /// Replace the outcome with `Skipped { reason }`, keeping the variant.
    pub(super) fn skip(&mut self, reason: SkipReason) {
        let outcome = match self {
            Self::Readiness(verification) => &mut verification.outcome,
            Self::Native(verification) => &mut verification.outcome,
        };
        *outcome = VerificationOutcome::Skipped { reason };
    }

    /// True when the outcome is `Matched` and the transcript is complete.
    pub(super) const fn succeeded(&self) -> bool {
        match self {
            Self::Readiness(verification) => verification.succeeded(),
            Self::Native(verification) => verification.succeeded(),
        }
    }

    /// Identity and Gateway mode read by a check that required Gateway Off.
    pub(super) fn gateway_off_evidence(&self) -> Option<(&Identity, DvGatewayMode)> {
        match self {
            Self::Readiness(verification) => verification.gateway_off_evidence(),
            Self::Native(verification) => verification.gateway_off_evidence(),
        }
    }

    /// Final outcome of the check.
    pub(super) const fn outcome(&self) -> &VerificationOutcome {
        match self {
            Self::Readiness(verification) => &verification.outcome,
            Self::Native(verification) => &verification.outcome,
        }
    }

    /// Summary of the transcript reserved for the check.
    #[cfg(all(test, unix))]
    pub(super) const fn transcript(&self) -> &TranscriptSummary {
        match self {
            Self::Readiness(verification) => &verification.transcript,
            Self::Native(verification) => &verification.transcript,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LifecycleEvent<'a> {
    WaitRequested {
        milliseconds: u64,
    },
    WaitCompleted,
    EnumerationRequested,
    EnumerationCompleted {
        candidates: &'a [ObservedEndpoint],
    },
    EnumerationFailed {
        error: Failure,
    },
    EndpointRejected {
        error: Failure,
    },
    OpenRequested {
        path: &'a str,
        baud: std::num::NonZeroU32,
    },
    OpenCompleted,
    OpenFailed {
        error: Failure,
    },
    EndpointSettled {
        quiet_milliseconds: u64,
    },
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn same_service(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    first.path == second.path
        || macos_service_name(&first.path)
            .zip(macos_service_name(&second.path))
            .is_some_and(|(first, second)| first == second)
}

fn observed_candidates(
    original: &SerialCandidate,
    candidates: &[SerialCandidate],
) -> Vec<ObservedEndpoint> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.is_tmd750()
                || same_service(candidate, original)
                || candidates
                    .iter()
                    .any(|known| known.is_tmd750() && same_service(candidate, known))
        })
        .map(ObservedEndpoint::from)
        .collect()
}

/// Wait for the operation-panel endpoint to settle before an MCP entry.
///
/// About ten seconds after a programming exit, once it first answers `ID`,
/// the operation-panel endpoint re-enumerates once more, and an entry command
/// sent at that moment fails with `ENXIO`. This waits through
/// [`await_settled_endpoint`] on that endpoint and returns at once on the
/// main-unit endpoint, which re-enumerates before it answers.
///
/// # Errors
///
/// The failures of [`await_settled_endpoint`].
pub(super) async fn settle_before_entry(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    recorder: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> Result<(), SettleFailure> {
    if endpoint.pid == Some(kenwood_tmd750::transport::TMD750_PANEL_PID) {
        await_settled_endpoint(backend, endpoint, recorder, cancelled).await
    } else {
        Ok(())
    }
}

/// Wait until `endpoint` has been enumerated continuously for `SETTLE_QUIET`.
///
/// Runs the library's [`await_continuous_enumeration`], which polls
/// enumeration every `SETTLE_POLL_INTERVAL`, and records every snapshot and
/// wait in `recorder`. An absent endpoint restarts the quiet period; a
/// snapshot that no longer uniquely selects the endpoint, an enumeration
/// failure, cancellation, an incomplete transcript or the `SETTLE_BUDGET`
/// expiring ends the wait with the corresponding [`SettleFailure`]. Nothing
/// is opened.
pub(super) async fn await_settled_endpoint(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    recorder: &mut Recorder<File>,
    cancelled: &AtomicBool,
) -> Result<(), SettleFailure> {
    let (outcome, capture_refused) = {
        let mut host = SettleHost::new(backend, endpoint, recorder);
        let report = await_continuous_enumeration(&mut host, endpoint, cancelled).await;
        (report.outcome, host.capture_refused())
    };
    match outcome {
        SettleOutcome::Settled | SettleOutcome::NotRequired => {
            recorder.record(LifecycleEvent::EndpointSettled {
                quiet_milliseconds: milliseconds(SETTLE_QUIET),
            });
            recorder.ensure_complete().map_err(SettleFailure::Capture)
        }
        SettleOutcome::Cancelled => Err(SettleFailure::Cancelled),
        SettleOutcome::Failed(_) if capture_refused => Err(SettleFailure::Capture(
            recorder
                .ensure_complete()
                .err()
                .unwrap_or_else(|| std::io::Error::other("the transcript refused an enumeration")),
        )),
        SettleOutcome::Failed(ReadinessError::Unsettled { .. }) => {
            Err(SettleFailure::BudgetExhausted)
        }
        SettleOutcome::Failed(ReadinessError::Enumeration(error)) => {
            Err(SettleFailure::Enumeration(error))
        }
        SettleOutcome::Failed(ReadinessError::Reenumeration(rejection)) => {
            recorder.record(LifecycleEvent::EndpointRejected {
                error: Failure::from_error(&rejection),
            });
            Err(SettleFailure::EndpointRejected(rejection))
        }
        SettleOutcome::Failed(error) => Err(SettleFailure::Readiness(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use kenwood_tmd750::transport::{KENWOOD_VID, TMD750_MAIN_PID};
    use kenwood_tmd750::{FirmwareIdentity, RadioModel, RadioType};
    use kenwood_transport::MockTransport;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    struct TestBackend<F> {
        on_open: F,
        opens: usize,
        enumerations: usize,
        elapsed: Duration,
        writes: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
    }

    impl<F> TestBackend<F> {
        fn new(on_open: F) -> Self {
            Self {
                on_open,
                opens: 0,
                enumerations: 0,
                elapsed: Duration::ZERO,
                writes: Arc::new(AtomicUsize::new(0)),
                closes: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    struct CountedConnection {
        mock: MockTransport,
        writes: Arc<AtomicUsize>,
        closes: Arc<AtomicUsize>,
    }

    impl Transport for CountedConnection {
        async fn write(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
            let _previous = self.writes.fetch_add(1, Ordering::Relaxed);
            self.mock.write(bytes).await
        }

        async fn read(&mut self, bytes: &mut [u8]) -> Result<usize, TransportError> {
            self.mock.read(bytes).await
        }

        async fn close(&mut self) -> Result<(), TransportError> {
            let _previous = self.closes.fetch_add(1, Ordering::Relaxed);
            self.mock.close().await
        }
    }

    impl<F: FnMut() + Send> Backend for TestBackend<F> {
        type Connection = CountedConnection;

        fn open(
            &mut self,
            _endpoint: &SerialCandidate,
            _baud: std::num::NonZeroU32,
        ) -> Result<Self::Connection, TransportError> {
            self.opens += 1;
            (self.on_open)();
            let mut mock = MockTransport::new();
            mock.expect(b"ID\r", b"ID TM-D750\r");
            mock.expect(b"FV\r", b"FV 1.02\r");
            mock.expect(b"TY\r", b"TY K,2,1\r");
            Ok(CountedConnection {
                mock,
                writes: Arc::clone(&self.writes),
                closes: Arc::clone(&self.closes),
            })
        }

        fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
            self.enumerations += 1;
            Ok(vec![endpoint()])
        }

        fn now(&self) -> Duration {
            self.elapsed
        }

        async fn wait(&mut self, duration: Duration) {
            self.elapsed += duration;
        }
    }

    fn endpoint() -> SerialCandidate {
        SerialCandidate {
            path: "/dev/cu.fresh-radio".to_owned(),
            vid: Some(KENWOOD_VID),
            pid: Some(TMD750_MAIN_PID),
        }
    }

    fn identity() -> Result<Identity, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Identity {
            model: RadioModel::TmD750,
            firmware: FirmwareIdentity::new("1.02")?,
            radio_type: RadioType::new("K,2,1")?,
        })
    }

    #[tokio::test]
    async fn the_main_unit_check_completes_one_captured_identity_and_close() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("fresh-cat.jsonl");
        let recorder = Recorder::named(
            crate::capture::create_private_file(&path)?,
            Arc::new(AtomicBool::new(false)),
            "fresh-cat.jsonl",
        );
        let mut backend = TestBackend::new(|| {});
        let result = verify_readiness(
            &mut backend,
            &endpoint(),
            kenwood_tmd750::transport::DEFAULT_BAUD,
            &identity()?,
            recorder,
            &AtomicBool::new(false),
        )
        .await;
        assert!(result.succeeded(), "{result:?}");
        assert_eq!(backend.opens, 1);
        assert_eq!(backend.writes.load(Ordering::Relaxed), 3);
        assert_eq!(backend.closes.load(Ordering::Relaxed), 1);
        let text = std::fs::read_to_string(path)?;
        assert!(text.contains("close_completed"));
        Ok(())
    }

    #[tokio::test]
    async fn a_transcript_that_cannot_be_written_prevents_every_enumeration_and_open() -> TestResult
    {
        let file = tempfile::NamedTempFile::new()?;
        let capture_failed = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::named(
            File::open(file.path())?,
            Arc::clone(&capture_failed),
            "fresh-cat.jsonl",
        );
        let user_cancelled = AtomicBool::new(false);
        let mut backend = TestBackend::new(|| {});
        let result = verify_readiness(
            &mut backend,
            &endpoint(),
            kenwood_tmd750::transport::DEFAULT_BAUD,
            &identity()?,
            recorder,
            &user_cancelled,
        )
        .await;
        assert!(
            matches!(
                result.outcome,
                VerificationOutcome::Failed {
                    stage: VerificationStage::Capture,
                    ..
                }
            ),
            "{result:?}"
        );
        assert!(!result.transcript.complete);
        assert!(capture_failed.load(Ordering::Relaxed));
        assert!(!user_cancelled.load(Ordering::Relaxed));
        assert_eq!(backend.opens, 0);
        assert_eq!(backend.enumerations, 0);
        assert_eq!(backend.writes.load(Ordering::Relaxed), 0);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_capture_failure_after_open_prevents_cat_but_still_closes() -> TestResult {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        // A disconnected local socket supplies a deterministic recording failure
        // after open without filling a disk or changing another process's files.
        let (writer, reader) = UnixStream::pair()?;
        let mut sink = Some(reader);
        let recorder = Recorder::named(
            File::from(OwnedFd::from(writer)),
            Arc::new(AtomicBool::new(false)),
            "fresh-cat.jsonl",
        );
        let mut backend = TestBackend::new(|| drop(sink.take()));
        let result = verify_readiness(
            &mut backend,
            &endpoint(),
            kenwood_tmd750::transport::DEFAULT_BAUD,
            &identity()?,
            recorder,
            &AtomicBool::new(false),
        )
        .await;
        assert!(!result.succeeded());
        assert!(!result.transcript.complete);
        assert_eq!(backend.opens, 1);
        assert_eq!(backend.writes.load(Ordering::Relaxed), 0);
        assert_eq!(backend.closes.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_sync_failure_after_complete_cat_and_close_fails_the_check() -> TestResult {
        // The null stream accepts writes but cannot durably synchronize them.
        let capture_failed = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::named(
            std::fs::OpenOptions::new().write(true).open("/dev/null")?,
            Arc::clone(&capture_failed),
            "fresh-cat.jsonl",
        );
        let mut backend = TestBackend::new(|| {});
        let result = verify_readiness(
            &mut backend,
            &endpoint(),
            kenwood_tmd750::transport::DEFAULT_BAUD,
            &identity()?,
            recorder,
            &AtomicBool::new(false),
        )
        .await;
        assert!(!result.succeeded(), "{result:?}");
        assert!(!result.transcript.complete);
        assert!(capture_failed.load(Ordering::Relaxed));
        assert_eq!(backend.writes.load(Ordering::Relaxed), 3);
        assert_eq!(backend.closes.load(Ordering::Relaxed), 1);
        Ok(())
    }
}
