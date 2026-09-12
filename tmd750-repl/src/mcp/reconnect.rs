//! Read-only CAT qualification with explicitly scoped fresh-handle policies.

mod readiness;

pub(super) use readiness::{ReadinessVerification, verify_readiness};

use std::fs::File;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kenwood_tmd750::transport::{SerialCandidate, SerialTransport, discover_serial, open_serial};
use kenwood_tmd750::{DvGatewayMode, Identity, Radio};
use kenwood_transport::{Transport, TransportError};
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};

use super::capture::{CaptureTransport, Recorder, TranscriptSummary};
use super::reconnect_policy::{ReconnectDecision, classify};
use super::{CLOSE_TIMEOUT, Failure, IdentityEvidence, close_transport};

const SETTLE: Duration = Duration::from_secs(2);
// The earlier ten-second observation never saw the endpoint return.
// This longer passive budget is a host policy, not a firmware timing guarantee.
const ENUMERATION_BUDGET: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Fixed workflow dependencies, replaceable by an entirely local test backend.
pub(super) trait Backend {
    /// One owned connection; reopening this value is never requested.
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

/// Host implementation; never scans ports with CAT or invokes transport reopen.
#[derive(Debug)]
pub(super) struct SystemBackend {
    started: Instant,
}

impl SystemBackend {
    /// Begin the monotonic host-policy clock.
    pub(super) fn new() -> Self {
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
    /// The bounded qualification session did not complete its required exchange.
    OriginalTrialIncomplete,
    /// A bounded settings update did not acknowledge its MCP exit.
    OriginalUpdateIncomplete,
    /// The update's durable recovery journal failed before fresh verification.
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
    /// Required transcript recording or durable synchronization failed.
    Capture,
    /// The bounded identity-readiness dispatch window or attempt cap expired.
    Readiness,
}

/// Outcome independent of the original MCP report.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum VerificationOutcome {
    /// Requested fresh CAT observations matched, and the connection closed.
    Matched,
    /// No fresh connection was eligible to be opened.
    Skipped { reason: SkipReason },
    /// Current work finished and no subsequent attempt was started.
    Cancelled,
    /// First failed stage; later close errors remain in the attempt evidence.
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

/// Retain the actual typed identity while preserving the existing JSON shape.
#[derive(Debug)]
struct ObservedIdentity(Identity);

impl Serialize for ObservedIdentity {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        IdentityEvidence::from(&self.0).serialize(serializer)
    }
}

/// Lossless typed Gateway evidence, including unnamed wire values.
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

/// Separate evidence; matching public identity is not physical-unit continuity.
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
    /// Completeness of the independently reserved verification transcript.
    pub(super) transcript: TranscriptSummary,
    /// Result of the explicitly requested additional workflow.
    pub(super) outcome: VerificationOutcome,
}

impl PostExitVerification {
    /// Record ineligibility without opening another connection.
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

    /// A successful observation also requires its complete capture.
    pub(super) const fn succeeded(&self) -> bool {
        self.transcript.complete && matches!(self.outcome, VerificationOutcome::Matched)
    }

    /// Return the actual fresh observations only after required Off verification.
    ///
    /// Identity-only verification, incomplete capture, failed close or durable
    /// synchronization, cancellation, and mismatching Gateway states return
    /// `None`. Matching identity does not establish physical-unit continuity.
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

/// Verify one fresh CAT connection with fail-closed, synchronized evidence.
///
/// The capture recorder's failure flag need not be the caller's cancellation
/// flag. Completeness is checked independently before opening and before every
/// protocol operation. The connection is always released after an open, and
/// capture synchronization cannot replace an earlier verification failure.
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

/// Require fresh matching identity followed by Gateway Off on the same handle.
///
/// This makes exactly one eligible open and at most one `GW` query, after the
/// complete identity matches. It adds no setter, MCP entry, retry, or recovery
/// traffic. The required transcript, fresh close, and durable synchronization
/// must all succeed before [`PostExitVerification::gateway_off_evidence`] can
/// return the actual observations. Cancellation is checked before the Gateway
/// query, never by dropping an in-flight CAT exchange.
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

/// Fixed read scope; every query requires complete recording.
#[derive(Clone, Copy)]
enum VerificationGoal {
    /// Preserve the original three-query reconnect workflow.
    IdentityOnly,
    /// Additionally require a fresh read-only Gateway Off observation.
    GatewayOff,
}

/// Stop new operations on capture failure without replacing an earlier failure.
fn capture_ready(recorder: &Recorder<File>, report: &mut PostExitVerification) -> bool {
    if let Err(error) = recorder.ensure_complete() {
        if !matches!(report.outcome, VerificationOutcome::Failed { .. }) {
            report.fail(VerificationStage::Capture, &error);
        }
        return false;
    }
    true
}

/// Synchronize final evidence while preserving the first verification failure.
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
    /// Only bounded read-only reacquisition has a shared dispatch deadline.
    dispatch_deadline: Option<Duration>,
}

/// One passive polling clock, shared across attempts when reacquiring CAT.
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

/// Retry admission is decided from typed failure and actual handle activity.
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
    // identify() begins with ID. No later command can qualify: its dispatch
    // raises the write count, even if that later write times out.
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

/// Typed query result; timeout alone does not establish retry eligibility.
enum IdentityObservation {
    /// The complete tuple, whether or not it matches the original identity.
    Complete(Identity),
    /// A typed query timeout, still requiring dispatch and input evidence.
    TimedOut,
    /// Cancellation, transport, parsing, or another terminal query failure.
    Failed,
}

/// Finish one complete identity tuple, retaining mismatches as actual evidence.
async fn observe_identity(
    radio: &mut Radio<impl Transport>,
    context: VerificationContext<'_>,
    report: &mut PostExitVerification,
    now: Duration,
) -> IdentityObservation {
    if let Some(deadline) = context.dispatch_deadline {
        if !readiness::can_dispatch(now, deadline) {
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

/// Run the sole additional query only after a matching identity and safe boundary.
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
            super::super::capture::create_private_file(&path)?,
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
