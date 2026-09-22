//! Fresh CAT observation and bounded identity readiness on a caller-selected
//! control endpoint, through a [`ControlHost`] that enumerates, opens and
//! closes serial connections.
//!
//! Nothing here enters programming mode or writes a setting. A matching
//! `ID`/`FV`/`TY` tuple identifies the model and firmware, not the physical
//! unit.
//!
//! On firmware 1.02 the complete identity tuple becomes available roughly 10
//! to 13 seconds after an MCP exit acknowledgment. The main-unit USB endpoint
//! re-enumerates at about that time; the control-panel USB endpoint can
//! reappear within a few seconds and then leave `ID` unanswered until the
//! tuple is ready. [`verify_readiness`] therefore waits [`SETTLE`], polls the
//! enumeration for the pinned endpoint, and retries only an entirely silent
//! `ID` timeout, within [`READINESS_BUDGET`] and [`MAXIMUM_OPEN_ATTEMPTS`].
//! These bounds are host policy sized from observation, not firmware timing
//! guarantees.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kenwood_transport::{Transport, TransportError};

use super::{Identity, Radio};
use crate::error::Error;
use crate::transport::reenumeration::{
    Reenumeration, ReenumerationRejection, are_aliases, select_pinned,
};
use crate::transport::{SerialCandidate, SerialTransport, discover_serial, open_serial};
use crate::types::DvGatewayMode;

/// Silent wait after an MCP exit acknowledgment, before the first enumeration.
pub const SETTLE: Duration = Duration::from_secs(2);
/// Interval between enumerations while the pinned endpoint is absent.
pub const ENUMERATION_INTERVAL: Duration = Duration::from_millis(250);
/// Window for enumeration, opens, identity queries and closes, measured from
/// the end of the settle wait.
pub const READINESS_BUDGET: Duration = Duration::from_secs(60);
/// Wait between one attempt's close and the next open.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// Opens permitted within one [`verify_readiness`] call.
pub const MAXIMUM_OPEN_ATTEMPTS: usize = 4;
/// Deadline for each write and each reply of the `ID`, `FV`, `TY` and `GW`
/// queries.
pub const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Budget for the single close of a connection.
pub const CLOSE_BUDGET: Duration = Duration::from_secs(2);
/// Time one attempt may still need: a write and a reply deadline for each of
/// `ID`, `FV` and `TY`, followed by the bounded close.
pub const ATTEMPT_ALLOWANCE: Duration = EXCHANGE_TIMEOUT
    .saturating_mul(6)
    .saturating_add(CLOSE_BUDGET);

/// The step of the Terminal lifecycle that the next control-endpoint open
/// serves, announced through [`ControlHost::stage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ControlStage {
    /// Identity and Gateway observation before any change.
    Preflight,
    /// Terminal programming over the control endpoint while Gateway is active.
    ActiveEntry,
    /// Identity readiness before restoration.
    ReadinessBeforeRestore,
    /// Restoration programming.
    Restore,
    /// Identity readiness after restoration.
    ReadinessAfterRestore,
    /// Identity and Gateway verification after restoration.
    Verification,
}

/// Why a connection was not confirmed closed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CloseFailure {
    /// The close did not complete within its budget; the connection was dropped.
    #[error("connection close exceeded its {budget:?} budget")]
    Timeout {
        /// Budget that elapsed.
        budget: Duration,
    },
    /// The transport reported a close failure.
    #[error(transparent)]
    Transport(#[from] TransportError),
    /// The host's own release work failed after the connection closed, for
    /// example writing its transcript.
    #[error("host release failed: {0}")]
    Host(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Host operations on the control endpoint: enumerate, open, clock, wait,
/// close.
///
/// Sending bytes is the library's job; the host owns the connection's lifetime
/// and anything it records about it. Every connection the library opens
/// through [`Self::open`] is handed back through [`Self::close`], whose result
/// must cover the connection and any host-owned release work. Tests supply a
/// host that touches no serial port.
pub trait ControlHost {
    /// Connection returned by [`Self::open`]; it is never reopened.
    type Connection: Transport;

    /// Observe serial port metadata without opening any port.
    ///
    /// # Errors
    ///
    /// Returns the platform enumeration failure.
    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError>;

    /// Open exactly `endpoint` at `baud`, sending nothing.
    ///
    /// # Errors
    ///
    /// Returns the open failure; the library records it and makes no retry of
    /// its own.
    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError>;

    /// Monotonic elapsed time from this host's origin.
    fn now(&self) -> Duration;

    /// Wait `duration` without radio I/O.
    fn wait(&mut self, duration: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }

    /// Close and drop `connection`, then finish any host-owned release work.
    ///
    /// The default closes within [`CLOSE_BUDGET`] and drops the connection.
    ///
    /// # Errors
    ///
    /// Returns the close timeout, the transport's close failure, or the host's
    /// release failure; the connection is dropped in every case.
    fn close(
        &mut self,
        connection: Self::Connection,
    ) -> impl Future<Output = Result<(), CloseFailure>> + Send {
        close_within(connection, CLOSE_BUDGET)
    }

    /// Announce the lifecycle stage the following opens serve.
    ///
    /// Called before the first host call of each stage. The default does
    /// nothing; a host that records per-stage transcripts switches here.
    fn stage(&mut self, _stage: ControlStage) {}
}

/// Close `connection` within `budget`, then drop it.
///
/// # Errors
///
/// Returns [`CloseFailure::Timeout`] when the budget elapses first and
/// [`CloseFailure::Transport`] when the transport reports a failure.
pub async fn close_within<T: Transport>(
    mut connection: T,
    budget: Duration,
) -> Result<(), CloseFailure> {
    let result = tokio::time::timeout(budget, connection.close())
        .await
        .map_err(|_elapsed| CloseFailure::Timeout { budget })?
        .map_err(CloseFailure::Transport);
    drop(connection);
    result
}

/// [`ControlHost`] over this crate's serial transport and the Tokio clock.
#[derive(Debug)]
pub struct SystemControlHost {
    started: Instant,
}

impl SystemControlHost {
    /// Start the clock that [`ControlHost::now`] measures elapsed time from.
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl Default for SystemControlHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlHost for SystemControlHost {
    type Connection = SerialTransport;

    fn enumerate(&mut self) -> Result<Vec<SerialCandidate>, TransportError> {
        discover_serial()
    }

    fn open(
        &mut self,
        endpoint: &SerialCandidate,
        baud: u32,
    ) -> Result<Self::Connection, TransportError> {
        open_serial(&endpoint.path, baud)
    }

    fn now(&self) -> Duration {
        self.started.elapsed()
    }
}

/// What a fresh observation must read to count as matched.
#[derive(Debug, Clone, Copy, Default)]
pub struct Expectation<'a> {
    /// Required identity tuple; `None` records whatever identity is read.
    pub identity: Option<&'a Identity>,
    /// Required Gateway state; `None` records whatever state is read.
    pub gateway: Option<DvGatewayMode>,
}

/// One enumeration inside a readiness attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enumeration {
    /// Time since the readiness window opened, per the host clock.
    pub elapsed: Duration,
    /// The TM-D750 endpoints plus every alias of the pinned endpoint or of a
    /// TM-D750 endpoint, in enumeration order.
    pub candidates: Vec<SerialCandidate>,
}

/// One open of the pinned endpoint and what was read on it.
#[derive(Debug)]
pub struct ConnectionAttempt {
    /// Endpoint the open targeted.
    pub endpoint: SerialCandidate,
    /// Whether the open returned a connection; a failed open leaves its error
    /// in the attempt outcome and needs no close.
    pub opened: bool,
    /// Complete identity tuple read on the connection, matching or not.
    pub identity: Option<Identity>,
    /// Gateway state read on the connection, matching or not.
    pub gateway: Option<DvGatewayMode>,
    /// Result of the host close, present whenever the open returned.
    pub close: Option<Result<(), CloseFailure>>,
}

impl ConnectionAttempt {
    fn unopened(endpoint: &SerialCandidate) -> Self {
        Self {
            endpoint: endpoint.clone(),
            opened: false,
            identity: None,
            gateway: None,
            close: None,
        }
    }

    /// Whether this process holds no connection from the attempt: the open
    /// failed, or the host close succeeded.
    #[must_use]
    pub const fn released(&self) -> bool {
        matches!(
            (self.opened, &self.close),
            (false, None) | (true, Some(Ok(())))
        )
    }
}

/// First failure of an observation or a readiness attempt.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadinessError {
    /// The host could not enumerate serial ports.
    #[error("serial enumeration failed: {0}")]
    Enumeration(#[source] TransportError),
    /// The pinned endpoint stayed absent for the whole readiness budget.
    #[error("the pinned endpoint did not return within {budget:?}")]
    EndpointAbsent {
        /// Budget that elapsed.
        budget: Duration,
    },
    /// The fresh enumeration cannot select the pinned endpoint.
    #[error(transparent)]
    Reenumeration(#[from] ReenumerationRejection),
    /// The host could not open the pinned endpoint.
    #[error("open failed: {0}")]
    Open(#[source] TransportError),
    /// An identity or Gateway query failed, timed out, or parsed unexpectedly.
    #[error(transparent)]
    Cat(#[from] Error),
    /// The complete fresh identity differs from the required one.
    #[error("fresh identity {actual} differs from the required {expected}")]
    IdentityMismatch {
        /// Required identity.
        expected: Identity,
        /// Identity the connection reported.
        actual: Identity,
    },
    /// The fresh Gateway state differs from the required one.
    #[error("fresh Gateway state is {actual}; required {required}")]
    GatewayMismatch {
        /// Required Gateway state.
        required: DvGatewayMode,
        /// State the connection reported.
        actual: DvGatewayMode,
    },
    /// The host did not confirm the close; the connection record holds the
    /// close failure.
    #[error("the connection was not confirmed closed")]
    Close,
    /// Too little of the readiness budget remained for one more identity
    /// attempt and its close.
    #[error("the readiness budget cannot cover another identity attempt and close")]
    BudgetExhausted,
}

/// Result of one observation or one readiness attempt.
#[derive(Debug)]
pub enum ReadinessOutcome {
    /// Every required value matched and the connection closed.
    Matched,
    /// Cancellation was seen at an exchange boundary; an open connection was
    /// still closed.
    Cancelled,
    /// The first failure; a later close failure stays in the connection record.
    Failed(ReadinessError),
}

impl ReadinessOutcome {
    /// Whether every required value matched.
    #[must_use]
    pub const fn is_matched(&self) -> bool {
        matches!(self, Self::Matched)
    }
}

/// Whether a completed attempt permits another open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryAdmission {
    /// The `ID` query timed out with no byte received, and the close succeeded.
    SilentIdentityTimeout,
    /// No further connection is permitted by this attempt's observations.
    Terminal,
}

/// Record of one open attempt inside [`verify_readiness`].
///
/// Failed attempts stay in the report when a later attempt succeeds.
#[derive(Debug)]
pub struct ReadinessAttempt {
    /// Enumerations taken while waiting for the pinned endpoint.
    pub enumerations: Vec<Enumeration>,
    /// The connection, absent when no open was made.
    pub connection: Option<ConnectionAttempt>,
    /// This attempt's result.
    pub outcome: ReadinessOutcome,
    /// Whether this attempt permitted another open.
    pub retry: RetryAdmission,
}

impl ReadinessAttempt {
    const fn pending() -> Self {
        Self {
            enumerations: Vec::new(),
            connection: None,
            outcome: ReadinessOutcome::Cancelled,
            retry: RetryAdmission::Terminal,
        }
    }

    /// Whether this attempt left no connection open.
    #[must_use]
    pub fn released(&self) -> bool {
        self.connection
            .as_ref()
            .is_none_or(ConnectionAttempt::released)
    }
}

/// How a readiness verification ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessEnding {
    /// The last attempt read the required identity and closed.
    Matched,
    /// Cancellation was requested before, between or during attempts.
    Cancelled,
    /// The last attempt failed without permitting a retry; its outcome names
    /// the cause.
    Failed,
    /// [`MAXIMUM_OPEN_ATTEMPTS`] attempts ran without a complete identity.
    AttemptCapExhausted,
    /// Too little of [`READINESS_BUDGET`] remained for the retry wait plus
    /// one attempt.
    BudgetExhausted,
}

/// Report of one [`verify_readiness`] call.
#[derive(Debug)]
pub struct ReadinessReport {
    /// Every attempt, in order.
    pub attempts: Vec<ReadinessAttempt>,
    /// How the verification ended.
    pub ending: ReadinessEnding,
    /// Time from the end of the settle wait to the return, per the host clock.
    pub elapsed: Duration,
}

impl ReadinessReport {
    /// Whether the required identity was read on a connection that closed.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.ending == ReadinessEnding::Matched
    }

    /// Whether every connection that opened was closed successfully.
    #[must_use]
    pub fn released(&self) -> bool {
        self.attempts.iter().all(ReadinessAttempt::released)
    }
}

/// Identity and Gateway read once on a freshly opened control connection,
/// which is then closed.
#[derive(Debug)]
pub struct ObservationReport {
    /// Endpoint the observation targeted.
    pub endpoint: SerialCandidate,
    /// The connection, absent when cancellation stopped the open.
    pub connection: Option<ConnectionAttempt>,
    /// The result.
    pub outcome: ReadinessOutcome,
}

impl ObservationReport {
    /// Whether identity and Gateway were read, matched every expectation, and
    /// the connection closed.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.outcome.is_matched()
            && self.connection.as_ref().is_some_and(|connection| {
                connection.released()
                    && connection.identity.is_some()
                    && connection.gateway.is_some()
            })
    }

    /// Whether this process holds no connection from the observation.
    #[must_use]
    pub fn released(&self) -> bool {
        self.connection
            .as_ref()
            .is_none_or(ConnectionAttempt::released)
    }

    /// Identity and Gateway state when [`Self::succeeded`].
    #[must_use]
    pub fn observed(&self) -> Option<(&Identity, DvGatewayMode)> {
        if !self.succeeded() {
            return None;
        }
        let connection = self.connection.as_ref()?;
        Some((connection.identity.as_ref()?, connection.gateway?))
    }
}

/// Open `endpoint` once, read `ID`/`FV`/`TY` and `GW`, compare them with
/// `expected`, and close.
///
/// No settle wait and no enumeration precede the open: this is the check for
/// an endpoint that is already answering CAT. Each query uses
/// [`EXCHANGE_TIMEOUT`]. Cancellation is checked before each query, never by
/// dropping an in-flight exchange, and an opened connection is always handed
/// to the host's close.
pub async fn observe_control<C: ControlHost>(
    host: &mut C,
    endpoint: &SerialCandidate,
    baud: u32,
    expected: Expectation<'_>,
    cancelled: &AtomicBool,
) -> ObservationReport {
    if cancelled.load(Ordering::Relaxed) {
        return ObservationReport {
            endpoint: endpoint.clone(),
            connection: None,
            outcome: ReadinessOutcome::Cancelled,
        };
    }
    let observed = run_connection(
        host,
        endpoint,
        baud,
        Query {
            expected,
            gateway: true,
            dispatch_deadline: None,
        },
        cancelled,
    )
    .await;
    ObservationReport {
        endpoint: endpoint.clone(),
        connection: Some(observed.connection),
        outcome: observed.outcome,
    }
}

/// Read the identity tuple on fresh connections to `endpoint` after an MCP
/// exit, within [`READINESS_BUDGET`] after [`SETTLE`].
///
/// Call this only after the exit was acknowledged and the original connection
/// was closed. Each attempt re-enumerates until [`select_pinned`] returns the
/// exact endpoint, opens it once, and reads `ID`, `FV` and `TY` with
/// [`EXCHANGE_TIMEOUT`] per write and per reply. Another open follows only
/// when the `ID` query timed out with no byte received and the close
/// succeeded; at most [`MAXIMUM_OPEN_ATTEMPTS`] opens run, [`RETRY_INTERVAL`]
/// apart, and an attempt starts only while [`ATTEMPT_ALLOWANCE`] still fits in
/// the budget. No `GW` query, setter, programming entry, baud change or reopen
/// is sent. Cancellation finishes the current queries and close first.
pub async fn verify_readiness<C: ControlHost>(
    host: &mut C,
    endpoint: &SerialCandidate,
    baud: u32,
    expected: &Identity,
    cancelled: &AtomicBool,
) -> ReadinessReport {
    let mut report = ReadinessReport {
        attempts: Vec::new(),
        ending: ReadinessEnding::Cancelled,
        elapsed: Duration::ZERO,
    };
    if cancelled.load(Ordering::Relaxed) {
        return report;
    }
    host.wait(SETTLE).await;
    let started = host.now();
    let deadline = started.saturating_add(READINESS_BUDGET);
    loop {
        if cancelled.load(Ordering::Relaxed) {
            report.ending = ReadinessEnding::Cancelled;
            break;
        }
        let mut attempt = ReadinessAttempt::pending();
        if let Some(selected) =
            await_endpoint(host, endpoint, &mut attempt, cancelled, started).await
        {
            if can_dispatch(host.now(), deadline) {
                let query = Query {
                    expected: Expectation {
                        identity: Some(expected),
                        gateway: None,
                    },
                    gateway: false,
                    dispatch_deadline: Some(deadline),
                };
                let observed = run_connection(host, &selected, baud, query, cancelled).await;
                if observed.silent_identity_timeout && observed.connection.released() {
                    attempt.retry = RetryAdmission::SilentIdentityTimeout;
                }
                attempt.connection = Some(observed.connection);
                attempt.outcome = observed.outcome;
            } else {
                attempt.outcome = ReadinessOutcome::Failed(ReadinessError::BudgetExhausted);
            }
        }
        let retry = attempt.retry == RetryAdmission::SilentIdentityTimeout;
        let ending = match attempt.outcome {
            ReadinessOutcome::Matched => ReadinessEnding::Matched,
            ReadinessOutcome::Cancelled => ReadinessEnding::Cancelled,
            ReadinessOutcome::Failed(_) => ReadinessEnding::Failed,
        };
        report.attempts.push(attempt);
        if !retry {
            report.ending = ending;
            break;
        }
        if cancelled.load(Ordering::Relaxed) {
            report.ending = ReadinessEnding::Cancelled;
            break;
        }
        if report.attempts.len() >= MAXIMUM_OPEN_ATTEMPTS {
            report.ending = ReadinessEnding::AttemptCapExhausted;
            break;
        }
        if deadline.saturating_sub(host.now()) < RETRY_INTERVAL.saturating_add(ATTEMPT_ALLOWANCE) {
            report.ending = ReadinessEnding::BudgetExhausted;
            break;
        }
        host.wait(RETRY_INTERVAL).await;
    }
    report.elapsed = host.now().saturating_sub(started);
    report
}

/// True when `deadline - now` still covers [`ATTEMPT_ALLOWANCE`].
///
/// Both arguments are elapsed times from the host's clock origin.
const fn can_dispatch(now: Duration, deadline: Duration) -> bool {
    deadline.saturating_sub(now).as_nanos() >= ATTEMPT_ALLOWANCE.as_nanos()
}

/// Poll the enumeration until the pinned endpoint is selectable.
///
/// Returns `None` after recording the attempt outcome: cancellation, an
/// enumeration failure, a rejection, or an absence lasting the whole budget.
async fn await_endpoint<C: ControlHost>(
    host: &mut C,
    original: &SerialCandidate,
    attempt: &mut ReadinessAttempt,
    cancelled: &AtomicBool,
    started: Duration,
) -> Option<SerialCandidate> {
    loop {
        if cancelled.load(Ordering::Relaxed) {
            attempt.outcome = ReadinessOutcome::Cancelled;
            return None;
        }
        let elapsed = host.now().saturating_sub(started);
        if elapsed >= READINESS_BUDGET {
            attempt.outcome = ReadinessOutcome::Failed(ReadinessError::EndpointAbsent {
                budget: READINESS_BUDGET,
            });
            return None;
        }
        let candidates = match host.enumerate() {
            Ok(candidates) => candidates,
            Err(error) => {
                attempt.outcome = ReadinessOutcome::Failed(ReadinessError::Enumeration(error));
                return None;
            }
        };
        attempt.enumerations.push(Enumeration {
            elapsed,
            candidates: relevant_candidates(original, &candidates),
        });
        match select_pinned(original, &candidates) {
            Reenumeration::Ready(selected) => return Some(selected),
            Reenumeration::Absent => {
                let remaining = READINESS_BUDGET.saturating_sub(host.now().saturating_sub(started));
                host.wait(ENUMERATION_INTERVAL.min(remaining)).await;
            }
            Reenumeration::Rejected(rejection) => {
                attempt.outcome = ReadinessOutcome::Failed(rejection.into());
                return None;
            }
        }
    }
}

/// TM-D750 endpoints plus aliases of the pinned endpoint or of a TM-D750
/// endpoint, keeping enumeration order.
fn relevant_candidates(
    original: &SerialCandidate,
    candidates: &[SerialCandidate],
) -> Vec<SerialCandidate> {
    candidates
        .iter()
        .filter(|candidate| {
            candidate.is_tmd750()
                || are_aliases(candidate, original)
                || candidates
                    .iter()
                    .any(|known| known.is_tmd750() && are_aliases(candidate, known))
        })
        .cloned()
        .collect()
}

/// What one connection reads and under which deadline.
#[derive(Clone, Copy)]
struct Query<'a> {
    expected: Expectation<'a>,
    /// Whether `GW` is read after a matching identity.
    gateway: bool,
    /// Readiness deadline; the identity query is skipped when
    /// [`ATTEMPT_ALLOWANCE`] no longer fits before it.
    dispatch_deadline: Option<Duration>,
}

/// One opened connection after its queries and close.
struct Observed {
    connection: ConnectionAttempt,
    outcome: ReadinessOutcome,
    /// The `ID` query timed out after exactly one completed write with no byte
    /// received.
    silent_identity_timeout: bool,
}

/// Open `endpoint`, run the queries, and hand the connection to the host close.
async fn run_connection<C: ControlHost>(
    host: &mut C,
    endpoint: &SerialCandidate,
    baud: u32,
    query: Query<'_>,
    cancelled: &AtomicBool,
) -> Observed {
    let mut connection = ConnectionAttempt::unopened(endpoint);
    let transport = match host.open(endpoint, baud) {
        Ok(transport) => transport,
        Err(error) => {
            return Observed {
                connection,
                outcome: ReadinessOutcome::Failed(ReadinessError::Open(error)),
                silent_identity_timeout: false,
            };
        }
    };
    connection.opened = true;
    let mut radio = Radio::new(Counted::new(transport));
    radio.set_timeout(EXCHANGE_TIMEOUT);
    let queried = if query
        .dispatch_deadline
        .is_some_and(|deadline| !can_dispatch(host.now(), deadline))
    {
        Queried {
            outcome: ReadinessOutcome::Failed(ReadinessError::BudgetExhausted),
            identity_timed_out: false,
        }
    } else {
        run_queries(&mut radio, query, cancelled, &mut connection).await
    };
    let counted = radio.into_transport();
    let silent = queried.identity_timed_out && counted.silent_first_exchange();
    let close = host.close(counted.into_inner()).await;
    let closed = close.is_ok();
    connection.close = Some(close);
    let outcome = match queried.outcome {
        ReadinessOutcome::Matched | ReadinessOutcome::Cancelled if !closed => {
            ReadinessOutcome::Failed(ReadinessError::Close)
        }
        ReadinessOutcome::Matched if cancelled.load(Ordering::Relaxed) => {
            ReadinessOutcome::Cancelled
        }
        outcome => outcome,
    };
    Observed {
        connection,
        outcome,
        silent_identity_timeout: silent && closed,
    }
}

/// Result of the queries on one connection, before its close.
struct Queried {
    outcome: ReadinessOutcome,
    identity_timed_out: bool,
}

/// Read the identity tuple and, when requested, the Gateway state.
async fn run_queries<T: Transport>(
    radio: &mut Radio<T>,
    query: Query<'_>,
    cancelled: &AtomicBool,
    connection: &mut ConnectionAttempt,
) -> Queried {
    if cancelled.load(Ordering::Relaxed) {
        return Queried {
            outcome: ReadinessOutcome::Cancelled,
            identity_timed_out: false,
        };
    }
    let identity = match radio.identify().await {
        Ok(identity) => identity,
        Err(error) => {
            let identity_timed_out = matches!(error, Error::Timeout { .. });
            return Queried {
                outcome: ReadinessOutcome::Failed(ReadinessError::Cat(error)),
                identity_timed_out,
            };
        }
    };
    connection.identity = Some(identity.clone());
    let mut outcome = ReadinessOutcome::Matched;
    if let Some(expected) = query.expected.identity
        && *expected != identity
    {
        outcome = ReadinessOutcome::Failed(ReadinessError::IdentityMismatch {
            expected: expected.clone(),
            actual: identity,
        });
    } else if query.gateway {
        outcome = read_gateway(radio, query.expected.gateway, cancelled, connection).await;
    }
    Queried {
        outcome,
        identity_timed_out: false,
    }
}

/// Read `GW` once after a matching identity and compare it with `required`.
async fn read_gateway<T: Transport>(
    radio: &mut Radio<T>,
    required: Option<DvGatewayMode>,
    cancelled: &AtomicBool,
    connection: &mut ConnectionAttempt,
) -> ReadinessOutcome {
    if cancelled.load(Ordering::Relaxed) {
        return ReadinessOutcome::Cancelled;
    }
    match radio.get_dv_gateway_mode().await {
        Ok(actual) => {
            connection.gateway = Some(actual);
            match required {
                Some(required) if required != actual => {
                    ReadinessOutcome::Failed(ReadinessError::GatewayMismatch { required, actual })
                }
                _ => ReadinessOutcome::Matched,
            }
        }
        Err(error) => ReadinessOutcome::Failed(ReadinessError::Cat(error)),
    }
}

/// Counts writes and received bytes on a connection without changing them.
#[derive(Debug)]
struct Counted<T> {
    inner: T,
    writes_started: u64,
    writes_completed: u64,
    received_bytes: u64,
}

impl<T> Counted<T> {
    const fn new(inner: T) -> Self {
        Self {
            inner,
            writes_started: 0,
            writes_completed: 0,
            received_bytes: 0,
        }
    }

    /// Whether exactly one write completed and no byte at all was received.
    const fn silent_first_exchange(&self) -> bool {
        self.writes_started == 1 && self.writes_completed == 1 && self.received_bytes == 0
    }

    fn into_inner(self) -> T {
        self.inner
    }
}

impl<T: Transport> Transport for Counted<T> {
    async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.writes_started = self.writes_started.saturating_add(1);
        self.inner.write(data).await?;
        self.writes_completed = self.writes_completed.saturating_add(1);
        Ok(())
    }

    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let count = self.inner.read(buffer).await?;
        self.received_bytes = self
            .received_bytes
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        Ok(count)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        self.inner.close().await
    }

    fn set_baud_rate(&mut self, baud: u32) -> Result<(), TransportError> {
        self.inner.set_baud_rate(baud)
    }
}

#[cfg(test)]
mod tests;
