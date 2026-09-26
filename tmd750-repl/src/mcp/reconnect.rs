//! Read-only CAT verification on a fresh connection after an MCP exit.
//!
//! A verification waits out the settle delay, polls enumeration for the
//! selected endpoint, opens it, reads the ID/FV/TY tuple and optionally one
//! `GW` query, then closes. Nothing here re-enters MCP, writes a setting, or
//! reopens a transport.

mod readiness;

pub(crate) use readiness::{
    MAXIMUM_OPEN_ATTEMPTS, ReadinessVerification, verify_readiness, verify_readiness_gateway_off,
};

use std::fs::File;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kenwood_tmd750::transport::{SerialCandidate, SerialTransport, discover_serial, open_serial};
use kenwood_tmd750::{DvGatewayMode, Identity, Radio};
use kenwood_transport::{Transport, TransportError};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::reconnect_policy::{ReconnectDecision, ReconnectRejection, classify};
use super::{CLOSE_TIMEOUT, Failure, IdentityEvidence, close_transport};
use crate::capture::{CaptureTransport, Recorder, TranscriptSummary};

/// Silent wait after the MCP exit ACK, before polling for the endpoint.
const SETTLE: Duration = Duration::from_secs(2);
/// Passive budget for the selected USB endpoint to re-enumerate (60 s).
///
/// Host policy, not a firmware timing guarantee: the endpoint has been observed
/// returning later than 10 s after the MCP exit ACK.
const ENUMERATION_BUDGET: Duration = Duration::from_secs(60);
/// Interval between enumeration snapshots inside `ENUMERATION_BUDGET`.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Continuous presence an endpoint must show before a further programming
/// session opens on it (10 s).
///
/// Host policy sized from observation: on firmware 1.02 the operation-panel
/// endpoint has re-enumerated once more within a few seconds of first
/// answering `ID` after an MCP exit, and a handle opened before that drop
/// failed with `ENXIO`.
pub(super) const SETTLE_QUIET: Duration = Duration::from_secs(10);
/// Interval between the enumeration snapshots of a settle wait (1 s).
const SETTLE_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Passive budget for a settle wait, measured from its start (60 s).
const SETTLE_BUDGET: Duration = Duration::from_secs(60);

/// Why an endpoint did not settle within the budget.
#[derive(Debug)]
pub(super) enum SettleFailure {
    /// Cancellation was requested before the endpoint settled.
    Cancelled,
    /// The endpoint was not continuously present for `SETTLE_QUIET` within
    /// `SETTLE_BUDGET`.
    BudgetExhausted,
    /// Fresh metadata no longer uniquely selects the endpoint.
    EndpointRejected(ReconnectRejection),
    /// Port enumeration failed.
    Enumeration(TransportError),
    /// The transcript could not record the wait.
    Capture(std::io::Error),
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
        }
    }
}

impl std::error::Error for SettleFailure {}

/// True when `candidates` selects exactly `endpoint` without ambiguity.
///
/// A macOS dial-in/callout alias pair counts as one service.
pub(crate) fn endpoint_is_unambiguous(
    endpoint: &SerialCandidate,
    candidates: &[SerialCandidate],
) -> bool {
    matches!(classify(endpoint, candidates), ReconnectDecision::Ready(selected) if selected == *endpoint)
}

/// Host operations a verification needs: open, enumerate, clock and wait.
///
/// Tests supply a backend that touches no serial port.
pub(crate) trait Backend {
    /// Connection returned by `open`; it is never reopened.
    type Connection: Transport;
    /// Open exactly the supplied endpoint.
    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError>;
    /// Observe port metadata without opening any port.
    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError>;
    /// Monotonic elapsed time from this backend's origin.
    fn now(&self) -> Duration;
    /// Wait without radio I/O.
    fn wait(&mut self, duration: Duration) -> impl Future<Output = ()>;
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
        baud: u32,
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
    /// A trial session did not finish its exchange and acknowledged MCP exit.
    OriginalTrialIncomplete,
    /// A bounded settings update did not acknowledge its MCP exit.
    OriginalUpdateIncomplete,
    /// The update's recovery journal failed an append or a synchronization.
    OriginalUpdateJournalIncomplete,
    /// Original connection release did not succeed.
    OriginalCloseFailed,
    /// Original capture was incomplete.
    OriginalCaptureIncomplete,
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
    /// The single fresh open failed.
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
                SkipReason::OriginalTrialIncomplete => "original MCP trial session was incomplete",
                SkipReason::OriginalUpdateIncomplete => "original text update was incomplete",
                SkipReason::OriginalUpdateJournalIncomplete => "text update journal was incomplete",
                SkipReason::OriginalCloseFailed => "original connection close failed",
                SkipReason::OriginalCaptureIncomplete => "original capture was incomplete",
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
            DvGatewayMode::Unqualified(_) => "unqualified",
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

/// Serialized record of the post-exit CAT check.
///
/// Holds the settle delay and enumeration budget in milliseconds, the open
/// attempt cap, every enumeration snapshot, the single open attempt, the
/// transcript summary and the final outcome. A matching ID/FV/TY tuple
/// identifies the model and firmware, not the physical unit.
#[derive(Debug, Serialize)]
pub(super) struct PostExitVerification {
    identity_assurance: &'static str,
    settle_milliseconds: u64,
    enumeration_budget_milliseconds: u64,
    maximum_open_attempts: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_gateway_mode: Option<GatewayEvidence>,
    enumerations: Vec<Enumeration>,
    attempt: Option<ConnectionAttempt>,
    /// Summary of the transcript reserved for this verification.
    pub(super) transcript: TranscriptSummary,
    /// Final outcome of this verification.
    pub(super) outcome: VerificationOutcome,
}

impl PostExitVerification {
    /// Report that verification never started, naming `reason`.
    ///
    /// No connection is opened and no enumeration snapshot is taken.
    pub(super) fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self {
            identity_assurance: "endpoint_and_cat_tuple_only",
            settle_milliseconds: milliseconds(SETTLE),
            enumeration_budget_milliseconds: milliseconds(ENUMERATION_BUDGET),
            maximum_open_attempts: 1,
            required_gateway_mode: None,
            enumerations: Vec::new(),
            attempt: None,
            transcript,
            outcome: VerificationOutcome::Skipped { reason },
        }
    }

    /// True when the outcome is `Matched` and the transcript is complete.
    pub(super) const fn succeeded(&self) -> bool {
        self.transcript.complete && matches!(self.outcome, VerificationOutcome::Matched)
    }

    /// Identity and Gateway mode read by a check that required Gateway Off.
    ///
    /// Returns `None` when the check read the identity only, the transcript is
    /// incomplete, the close or synchronization failed, cancellation
    /// intervened, or the observed Gateway mode is not Off.
    pub(super) fn gateway_off_evidence(&self) -> Option<(&Identity, DvGatewayMode)> {
        if !self.succeeded()
            || self.required_gateway_mode != Some(GatewayEvidence(DvGatewayMode::Off))
        {
            return None;
        }
        let attempt = self.attempt.as_ref()?;
        let mode = attempt.gateway_mode?.0;
        if mode != DvGatewayMode::Off {
            return None;
        }
        Some((&attempt.identity.as_ref()?.0, mode))
    }

    fn fail(&mut self, stage: VerificationStage, error: &(dyn std::error::Error + 'static)) {
        self.outcome = VerificationOutcome::Failed {
            stage,
            error: Failure::from_error(error),
        };
    }
}

/// The post-exit CAT check of one session: a single open within the
/// enumeration budget, or the bounded silent-`ID` retry that the
/// operation-panel endpoint needs because it answers `ID` only once its
/// tuple is ready.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum PostExit {
    /// One open and one identity read, optionally followed by `GW`.
    Single(PostExitVerification),
    /// Up to six opens within the readiness budget.
    Readiness(ReadinessVerification),
}

impl PostExit {
    /// A single-open check that never started, naming `reason`.
    pub(super) fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self::Single(PostExitVerification::skipped(reason, transcript))
    }

    /// Replace the outcome with `Skipped { reason }`, keeping the variant.
    pub(super) fn skip(&mut self, reason: SkipReason) {
        match self {
            Self::Single(verification) => {
                verification.outcome = VerificationOutcome::Skipped { reason };
            }
            Self::Readiness(verification) => {
                verification.outcome = VerificationOutcome::Skipped { reason };
            }
        }
    }

    /// True when the outcome is `Matched` and the transcript is complete.
    pub(super) const fn succeeded(&self) -> bool {
        match self {
            Self::Single(verification) => verification.succeeded(),
            Self::Readiness(verification) => verification.succeeded(),
        }
    }

    /// Identity and Gateway mode read by a check that required Gateway Off.
    pub(super) fn gateway_off_evidence(&self) -> Option<(&Identity, DvGatewayMode)> {
        match self {
            Self::Single(verification) => verification.gateway_off_evidence(),
            Self::Readiness(verification) => verification.gateway_off_evidence(),
        }
    }

    /// Final outcome of the check.
    pub(super) const fn outcome(&self) -> &VerificationOutcome {
        match self {
            Self::Single(verification) => &verification.outcome,
            Self::Readiness(verification) => &verification.outcome,
        }
    }

    /// Summary of the transcript reserved for the check.
    #[cfg(all(test, unix))]
    pub(super) const fn transcript(&self) -> &TranscriptSummary {
        match self {
            Self::Single(verification) => &verification.transcript,
            Self::Readiness(verification) => &verification.transcript,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LifecycleEvent<'a> {
    WaitRequested { milliseconds: u64 },
    WaitCompleted,
    EnumerationRequested,
    EnumerationCompleted { candidates: &'a [ObservedEndpoint] },
    EnumerationFailed { error: Failure },
    EndpointRejected { error: Failure },
    OpenRequested { path: &'a str, baud: u32 },
    OpenCompleted,
    OpenFailed { error: Failure },
    EndpointSettled { quiet_milliseconds: u64 },
}

fn milliseconds(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn same_service(first: &SerialCandidate, second: &SerialCandidate) -> bool {
    first.path == second.path
        || crate::macos_serial_service(&first.path)
            .zip(crate::macos_serial_service(&second.path))
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

async fn wait_recorded(
    backend: &mut impl Backend,
    recorder: &mut Recorder<File>,
    duration: Duration,
) {
    recorder.record(LifecycleEvent::WaitRequested {
        milliseconds: milliseconds(duration),
    });
    backend.wait(duration).await;
    recorder.record(LifecycleEvent::WaitCompleted);
}

/// Verify that one fresh connection reads back the original identity tuple.
///
/// Waits the settle delay, polls for the endpoint within the enumeration
/// budget, opens it once and reads ID/FV/TY, then closes. The transcript must
/// be complete before the open and before every protocol operation; a
/// successful open is always closed, and a later synchronization failure never
/// replaces an earlier failure in the report.
pub(super) async fn verify_required(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> PostExitVerification {
    verify_with(
        backend,
        endpoint,
        recorder,
        VerificationContext {
            baud,
            original_identity,
            cancelled,
            goal: VerificationGoal::IdentityOnly,
            dispatch_deadline: None,
        },
    )
    .await
}

/// Verify a matching identity followed by Gateway Off on the same connection.
///
/// Makes exactly one open and at most one `GW` query, issued only after the
/// complete identity matches. The transcript, the close and the final
/// synchronization must all succeed before
/// [`PostExitVerification::gateway_off_evidence`] returns the observations.
/// Cancellation is checked before the Gateway query, never by dropping an
/// in-flight CAT exchange.
pub(super) async fn verify_required_gateway_off(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> PostExitVerification {
    verify_with(
        backend,
        endpoint,
        recorder,
        VerificationContext {
            baud,
            original_identity,
            cancelled,
            goal: VerificationGoal::GatewayOff,
            dispatch_deadline: None,
        },
    )
    .await
}

/// How much the post-exit check reads on the fresh connection.
#[derive(Clone, Copy)]
enum VerificationGoal {
    /// Read the ID/FV/TY tuple only.
    IdentityOnly,
    /// Follow the tuple with one read-only `GW` query and require Off.
    GatewayOff,
}

/// True when the transcript is still complete, so another operation may run.
///
/// A failed check records a `Capture` failure unless the report already holds
/// one.
fn capture_ready(recorder: &Recorder<File>, report: &mut PostExitVerification) -> bool {
    if let Err(error) = recorder.ensure_complete() {
        if !matches!(report.outcome, VerificationOutcome::Failed { .. }) {
            report.fail(VerificationStage::Capture, &error);
        }
        return false;
    }
    true
}

/// Synchronize the recorder and store its summary, keeping the first failure.
fn finalize_capture(recorder: &mut Recorder<File>, report: &mut PostExitVerification) {
    if let Err(error) = recorder.synchronize()
        && !matches!(report.outcome, VerificationOutcome::Failed { .. })
    {
        report.fail(VerificationStage::Capture, &error);
    }
    report.transcript = recorder.summary();
}

#[derive(Clone, Copy)]
struct VerificationContext<'a> {
    baud: u32,
    original_identity: &'a Identity,
    cancelled: &'a AtomicBool,
    goal: VerificationGoal,
    /// Shared dispatch deadline, set only by the bounded readiness policy.
    dispatch_deadline: Option<Duration>,
}

/// Start time and budget for enumeration polling, shared across attempts.
#[derive(Clone, Copy)]
struct EnumerationWindow {
    started: Duration,
    budget: Duration,
}

async fn verify_with(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    mut recorder: Recorder<File>,
    context: VerificationContext<'_>,
) -> PostExitVerification {
    let mut report = PostExitVerification::skipped(SkipReason::Cancelled, recorder.summary());
    if matches!(context.goal, VerificationGoal::GatewayOff) {
        report.required_gateway_mode = Some(GatewayEvidence(DvGatewayMode::Off));
    }
    if capture_ready(&recorder, &mut report) && !context.cancelled.load(Ordering::Relaxed) {
        wait_recorded(backend, &mut recorder, SETTLE).await;
        let window = EnumerationWindow {
            started: backend.now(),
            budget: ENUMERATION_BUDGET,
        };
        if let Some(selected) = await_endpoint(
            backend,
            endpoint,
            &mut recorder,
            &mut report,
            context.cancelled,
            window,
        )
        .await
        {
            recorder = attempt_identity(backend, &selected, context, recorder, &mut report)
                .await
                .recorder;
        }
    }
    finalize_capture(&mut recorder, &mut report);
    report
}

async fn await_endpoint(
    backend: &mut impl Backend,
    original: &SerialCandidate,
    recorder: &mut Recorder<File>,
    report: &mut PostExitVerification,
    cancelled: &AtomicBool,
    window: EnumerationWindow,
) -> Option<SerialCandidate> {
    let EnumerationWindow { started, budget } = window;
    loop {
        if !capture_ready(recorder, report) {
            return None;
        }
        if cancelled.load(Ordering::Relaxed) {
            report.outcome = VerificationOutcome::Cancelled;
            return None;
        }
        if backend.now().saturating_sub(started) >= budget {
            report.fail(
                VerificationStage::Enumeration,
                &std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "selected USB endpoint did not return within the passive enumeration budget",
                ),
            );
            return None;
        }
        recorder.record(LifecycleEvent::EnumerationRequested);
        if !capture_ready(recorder, report) {
            return None;
        }
        if cancelled.load(Ordering::Relaxed) {
            continue;
        }
        let candidates = match backend.enumerate() {
            Ok(candidates) => candidates,
            Err(error) => {
                recorder.record(LifecycleEvent::EnumerationFailed {
                    error: Failure::from_error(&error),
                });
                report.fail(VerificationStage::Enumeration, &error);
                return None;
            }
        };
        let observed = observed_candidates(original, &candidates);
        recorder.record(LifecycleEvent::EnumerationCompleted {
            candidates: &observed,
        });
        report.enumerations.push(Enumeration {
            elapsed_milliseconds: milliseconds(backend.now().saturating_sub(started)),
            candidates: observed,
        });
        if !capture_ready(recorder, report) {
            return None;
        }
        if cancelled.load(Ordering::Relaxed) || backend.now().saturating_sub(started) >= budget {
            continue;
        }
        match classify(original, &candidates) {
            ReconnectDecision::Ready(selected) => return Some(selected),
            ReconnectDecision::AwaitingEndpoint => {
                let remaining = budget.saturating_sub(backend.now().saturating_sub(started));
                wait_recorded(backend, recorder, POLL_INTERVAL.min(remaining)).await;
            }
            ReconnectDecision::Rejected(error) => {
                recorder.record(LifecycleEvent::EndpointRejected {
                    error: Failure::from_error(&error),
                });
                report.fail(VerificationStage::EndpointSelection, &error);
                return None;
            }
        }
    }
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
/// Polls enumeration every `SETTLE_POLL_INTERVAL` and records every snapshot
/// and wait in `recorder`. An absent endpoint restarts the quiet period; a
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
    let started = backend.now();
    let mut present_since = None;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(SettleFailure::Cancelled);
        }
        let now = backend.now();
        if now.saturating_sub(started) >= SETTLE_BUDGET {
            return Err(SettleFailure::BudgetExhausted);
        }
        recorder.record(LifecycleEvent::EnumerationRequested);
        recorder.ensure_complete().map_err(SettleFailure::Capture)?;
        let candidates = match backend.enumerate() {
            Ok(candidates) => candidates,
            Err(error) => {
                recorder.record(LifecycleEvent::EnumerationFailed {
                    error: Failure::from_error(&error),
                });
                return Err(SettleFailure::Enumeration(error));
            }
        };
        let observed = observed_candidates(endpoint, &candidates);
        recorder.record(LifecycleEvent::EnumerationCompleted {
            candidates: &observed,
        });
        recorder.ensure_complete().map_err(SettleFailure::Capture)?;
        match classify(endpoint, &candidates) {
            ReconnectDecision::Ready(_) => {
                let since = *present_since.get_or_insert(now);
                if now.saturating_sub(since) >= SETTLE_QUIET {
                    recorder.record(LifecycleEvent::EndpointSettled {
                        quiet_milliseconds: milliseconds(SETTLE_QUIET),
                    });
                    recorder.ensure_complete().map_err(SettleFailure::Capture)?;
                    return Ok(());
                }
            }
            ReconnectDecision::AwaitingEndpoint => present_since = None,
            ReconnectDecision::Rejected(error) => {
                recorder.record(LifecycleEvent::EndpointRejected {
                    error: Failure::from_error(&error),
                });
                return Err(SettleFailure::EndpointRejected(error));
            }
        }
        wait_recorded(backend, recorder, SETTLE_POLL_INTERVAL).await;
    }
}

/// One completed open attempt.
///
/// Carries its recorder and whether the identity query timed out without the
/// connection receiving a byte.
struct IdentityAttempt {
    recorder: Recorder<File>,
    silent_identity_timeout: bool,
}

impl IdentityAttempt {
    const fn terminal(recorder: Recorder<File>) -> Self {
        Self {
            recorder,
            silent_identity_timeout: false,
        }
    }
}

async fn attempt_identity(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    context: VerificationContext<'_>,
    mut recorder: Recorder<File>,
    report: &mut PostExitVerification,
) -> IdentityAttempt {
    let VerificationContext {
        baud, cancelled, ..
    } = context;
    if !capture_ready(&recorder, report) {
        return IdentityAttempt::terminal(recorder);
    }
    if cancelled.load(Ordering::Relaxed) {
        report.outcome = VerificationOutcome::Cancelled;
        return IdentityAttempt::terminal(recorder);
    }
    recorder.record(LifecycleEvent::OpenRequested {
        path: &endpoint.path,
        baud,
    });
    if !capture_ready(&recorder, report) {
        return IdentityAttempt::terminal(recorder);
    }
    if cancelled.load(Ordering::Relaxed) {
        report.outcome = VerificationOutcome::Cancelled;
        return IdentityAttempt::terminal(recorder);
    }
    let connection = match backend.open(endpoint, baud) {
        Ok(connection) => connection,
        Err(error) => {
            let failure = Failure::from_error(&error);
            recorder.record(LifecycleEvent::OpenFailed {
                error: failure.clone(),
            });
            report.attempt = Some(ConnectionAttempt {
                endpoint: endpoint.into(),
                open: OperationOutcome::Failed { error: failure },
                identity: None,
                gateway_mode: None,
                close: None,
            });
            report.fail(VerificationStage::Open, &error);
            return IdentityAttempt::terminal(recorder);
        }
    };
    recorder.record(LifecycleEvent::OpenCompleted);
    let mut radio = Radio::new(CaptureTransport::required(connection, recorder));
    let observation = observe_identity(&mut radio, context, report, backend.now()).await;
    let gateway_mode = observe_gateway_off(&mut radio, context, report).await;
    let mut transport = radio.into_transport();
    // identify() sends ID first, so a silent first exchange can only be that ID:
    // any later command raises the write count, even when it times out.
    let silent_identity_timeout = matches!(observation, IdentityObservation::TimedOut)
        && transport.activity().silent_first_exchange();
    let close_error = close_transport(&mut transport).await;
    let close = close_error
        .as_ref()
        .map_or(OperationOutcome::Succeeded, |error| {
            OperationOutcome::Failed {
                error: error.clone(),
            }
        });
    let closed = close_error.is_none();
    if let Some(error) = close_error {
        if matches!(
            report.outcome,
            VerificationOutcome::Matched | VerificationOutcome::Cancelled
        ) {
            report.outcome = VerificationOutcome::Failed {
                stage: VerificationStage::Close,
                error,
            };
        }
    } else if cancelled.load(Ordering::Relaxed)
        && matches!(report.outcome, VerificationOutcome::Matched)
    {
        report.outcome = VerificationOutcome::Cancelled;
    }
    report.attempt = Some(ConnectionAttempt {
        endpoint: endpoint.into(),
        open: OperationOutcome::Succeeded,
        identity: match observation {
            IdentityObservation::Complete(identity) => Some(ObservedIdentity(identity)),
            IdentityObservation::TimedOut | IdentityObservation::Failed => None,
        },
        gateway_mode,
        close: Some(close),
    });
    let recorder = transport.into_recorder();
    IdentityAttempt {
        silent_identity_timeout: silent_identity_timeout && closed && recorder.summary().complete,
        recorder,
    }
}

/// Result of the fresh identity query.
enum IdentityObservation {
    /// The complete tuple, whether or not it matches the original identity.
    Complete(Identity),
    /// The query timed out; the caller still checks write and input counts.
    TimedOut,
    /// Cancellation, transport, parsing, or another terminal query failure.
    Failed,
}

/// Read the ID/FV/TY tuple once, recording a match or a mismatch.
async fn observe_identity(
    radio: &mut Radio<impl Transport>,
    context: VerificationContext<'_>,
    report: &mut PostExitVerification,
    now: Duration,
) -> IdentityObservation {
    if let Some(deadline) = context.dispatch_deadline {
        if !readiness::can_dispatch(now, deadline, context.goal) {
            readiness::budget_exhausted(report);
            return IdentityObservation::Failed;
        }
        radio.set_timeout(readiness::EXCHANGE_TIMEOUT);
    }
    if context.cancelled.load(Ordering::Relaxed) {
        report.outcome = VerificationOutcome::Cancelled;
        return IdentityObservation::Failed;
    }
    match radio.identify().await {
        Ok(identity) => {
            if identity == *context.original_identity {
                report.outcome = VerificationOutcome::Matched;
            } else {
                report.fail(
                    VerificationStage::IdentityMismatch,
                    &std::io::Error::other("fresh CAT identity tuple differs from the original"),
                );
            }
            IdentityObservation::Complete(identity)
        }
        Err(error) => {
            report.fail(VerificationStage::Identity, &error);
            if matches!(error, kenwood_tmd750::Error::Timeout { .. }) {
                IdentityObservation::TimedOut
            } else {
                IdentityObservation::Failed
            }
        }
    }
}

/// Issue the single `GW` query when the goal requires Off and identity matched.
async fn observe_gateway_off(
    radio: &mut Radio<impl Transport>,
    context: VerificationContext<'_>,
    report: &mut PostExitVerification,
) -> Option<GatewayEvidence> {
    if !matches!(context.goal, VerificationGoal::GatewayOff)
        || !matches!(report.outcome, VerificationOutcome::Matched)
    {
        return None;
    }
    if context.cancelled.load(Ordering::Relaxed) {
        report.outcome = VerificationOutcome::Cancelled;
        return None;
    }
    match radio.get_dv_gateway_mode().await {
        Ok(mode) => {
            if mode != DvGatewayMode::Off {
                report.fail(
                    VerificationStage::GatewayMismatch,
                    &std::io::Error::other(format!("fresh Gateway state is {mode}; required Off")),
                );
            }
            Some(GatewayEvidence(mode))
        }
        Err(error) => {
            report.fail(VerificationStage::Gateway, &error);
            None
        }
    }
}

#[cfg(test)]
#[path = "reconnect/gateway_tests.rs"]
mod gateway_tests;

#[cfg(test)]
#[path = "reconnect/readiness_tests.rs"]
mod readiness_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
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

    impl<F: FnMut()> Backend for TestBackend<F> {
        type Connection = CountedConnection;

        fn open(
            &mut self,
            _endpoint: &SerialCandidate,
            _baud: u32,
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
            path: "/dev/cu.trial-radio".to_owned(),
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
    async fn required_reconnect_completes_exactly_one_captured_identity_and_close() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join("fresh-cat.jsonl");
        let recorder = Recorder::named(
            crate::capture::create_private_file(&path)?,
            Arc::new(AtomicBool::new(false)),
            "fresh-cat.jsonl",
        );
        let mut backend = TestBackend::new(|| {});
        let result = verify_required(
            &mut backend,
            &endpoint(),
            9600,
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
    async fn required_reconnect_capture_failure_prevents_open_with_separate_cancellation_flag()
    -> TestResult {
        let file = tempfile::NamedTempFile::new()?;
        let capture_failed = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::named(
            File::open(file.path())?,
            Arc::clone(&capture_failed),
            "fresh-cat.jsonl",
        );
        let user_cancelled = AtomicBool::new(false);
        let mut backend = TestBackend::new(|| {});
        let result = verify_required(
            &mut backend,
            &endpoint(),
            9600,
            &identity()?,
            recorder,
            &user_cancelled,
        )
        .await;
        assert!(matches!(
            result.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::Capture,
                ..
            }
        ));
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
    async fn required_reconnect_capture_failure_after_open_prevents_cat_but_still_closes()
    -> TestResult {
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
        let result = verify_required(
            &mut backend,
            &endpoint(),
            9600,
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
    async fn required_reconnect_rejects_sync_failure_after_complete_cat_and_close() -> TestResult {
        // The null stream accepts writes but cannot durably synchronize them.
        let capture_failed = Arc::new(AtomicBool::new(false));
        let recorder = Recorder::named(
            std::fs::OpenOptions::new().write(true).open("/dev/null")?,
            Arc::clone(&capture_failed),
            "fresh-cat.jsonl",
        );
        let mut backend = TestBackend::new(|| {});
        let result = verify_required(
            &mut backend,
            &endpoint(),
            9600,
            &identity()?,
            recorder,
            &AtomicBool::new(false),
        )
        .await;
        assert!(matches!(
            result.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::Capture,
                ..
            }
        ));
        assert!(!result.transcript.complete);
        assert!(capture_failed.load(Ordering::Relaxed));
        assert_eq!(backend.writes.load(Ordering::Relaxed), 3);
        assert_eq!(backend.closes.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn required_finalization_preserves_an_earlier_failure() -> TestResult {
        let file = tempfile::NamedTempFile::new()?;
        let mut recorder = Recorder::named(
            File::open(file.path())?,
            Arc::new(AtomicBool::new(false)),
            "fresh-cat.jsonl",
        );
        recorder.record(LifecycleEvent::WaitCompleted);
        let mut report = PostExitVerification::skipped(SkipReason::Cancelled, recorder.summary());
        report.fail(
            VerificationStage::IdentityMismatch,
            &io::Error::other("original identity mismatch"),
        );
        finalize_capture(&mut recorder, &mut report);
        assert!(matches!(
            report.outcome,
            VerificationOutcome::Failed {
                stage: VerificationStage::IdentityMismatch,
                ref error,
            } if error.message == "original identity mismatch"
        ));
        assert!(!report.transcript.complete);
        Ok(())
    }
}
