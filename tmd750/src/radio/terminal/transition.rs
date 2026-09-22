//! Bounded MMDVM acquisition on the modem link after the Terminal pages are
//! written.
//!
//! Nothing here enters programming mode, sends CAT, changes modem
//! configuration or selects an endpoint: the [`ModemHost`] opens and reopens
//! the exact link its owner chose. A complete `GET_VERSION` reply proves the
//! current connection's wire protocol, not the radio identity or the stored
//! Gateway setting.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_transport::Transport;
use mmdvm::core::VersionResponse;
use mmdvm::probe::ProbeError;
use tokio::time::Instant;

use crate::radio::readiness::{CLOSE_BUDGET, CloseFailure, close_within};

/// Total window for acquiring MMDVM framing after the Terminal update, from
/// the first silent wait to the last probe. Host policy sized from
/// observation, not a firmware bound.
pub const WINDOW: Duration = Duration::from_secs(90);
/// Silent wait before each probe inside [`WINDOW`].
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Budget for one `GET_VERSION` request and its complete reply.
pub const VERSION_BUDGET: Duration = Duration::from_secs(2);

/// Host operations on the modem link: open, reopen, wait, retire.
///
/// The host resolves the link once in [`Self::open`] and reopens exactly that
/// link in [`Self::reopen`]; the library never substitutes another address,
/// channel or path. Every connection the host returns is handed back through
/// [`Self::retire`], whose result must cover the connection and any
/// host-owned release work.
pub trait ModemHost {
    /// Connection the host returns; the library sends only MMDVM frames on it
    /// after Terminal entry, or CAT and MCP while Gateway is still Off.
    type Connection: Transport;

    /// Open the modem link, resolving its service if needed. Sends nothing.
    ///
    /// # Errors
    ///
    /// Returns a [`ModemOpenFailure`] whose `released` flag says whether the
    /// host still holds a connection from the attempt.
    fn open(
        &mut self,
        cancelled: &AtomicBool,
    ) -> impl Future<Output = Result<Self::Connection, ModemOpenFailure>> + Send;

    /// Reopen the link [`Self::open`] established, completing before
    /// `deadline`.
    ///
    /// In-flight work is joined before returning, including on cancellation
    /// or expiry. A connection completed after `deadline` is closed by the
    /// host and reported as a failure.
    ///
    /// # Errors
    ///
    /// Returns a [`ModemOpenFailure`] whose `retry_allowed` flag says whether
    /// another attempt may run inside the window.
    fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> impl Future<Output = Result<Self::Connection, ModemOpenFailure>> + Send;

    /// Wait `duration` without sending anything on the link.
    fn wait(&mut self, duration: Duration) -> impl Future<Output = ()> + Send {
        tokio::time::sleep(duration)
    }

    /// Close and drop `connection`, then finish any host-owned release work.
    ///
    /// The default closes within [`CLOSE_BUDGET`] and drops the connection.
    /// The call is never cancelled, and a failure prevents a further reopen.
    ///
    /// # Errors
    ///
    /// Returns the close timeout, the transport's close failure, or the host's
    /// release failure; the connection is dropped in every case.
    fn retire(
        &mut self,
        connection: Self::Connection,
    ) -> impl Future<Output = Result<(), CloseFailure>> + Send {
        close_within(connection, CLOSE_BUDGET)
    }
}

/// A failed modem open or reopen.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct ModemOpenFailure {
    /// Why the open failed.
    #[source]
    pub source: Box<dyn std::error::Error + Send + Sync>,
    /// Whether another attempt may run inside the transition window. False
    /// after an interruption, a release failure, or a stage that does not
    /// recover by retrying.
    pub retry_allowed: bool,
    /// Whether the host holds no connection from this attempt; a connection
    /// that arrived after the failure was decided must have been closed.
    pub released: bool,
}

/// A connection on which a complete MMDVM `GET_VERSION` exchange succeeded.
///
/// Only [`Self::probe`] constructs this type, so the wrapped transport is the
/// connection that answered.
#[derive(Debug)]
pub struct ProvenModem<T> {
    transport: T,
    version: VersionResponse,
}

impl<T: Transport> ProvenModem<T> {
    /// Send one `GET_VERSION` on `transport` and require a complete reply
    /// within `budget`.
    ///
    /// No CAT is sent. On failure the same transport is returned with the
    /// error, for the caller to close.
    ///
    /// # Errors
    ///
    /// Returns the transport and the [`ProbeError`]: an expired budget, a
    /// transport failure, or a reply that is not a complete version frame.
    pub async fn probe(mut transport: T, budget: Duration) -> Result<Self, (T, ProbeError)> {
        match mmdvm::probe::probe_version(&mut transport, budget).await {
            Ok(version) => Ok(Self { transport, version }),
            Err(error) => Err((transport, error)),
        }
    }

    /// Take back the transport, for example to start the modem runtime on it
    /// or to close it.
    #[must_use]
    pub fn into_transport(self) -> T {
        self.transport
    }

    /// Borrow the proved transport.
    #[must_use]
    pub const fn transport(&self) -> &T {
        &self.transport
    }

    /// The version reply that proved the framing.
    #[must_use]
    pub const fn version(&self) -> &VersionResponse {
        &self.version
    }
}

/// Which operation began a transition step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransitionStep {
    /// A `GET_VERSION` probe on a held connection.
    #[default]
    Probe,
    /// A reopen with no connection held.
    Reopen,
}

/// What one probe or reopen step observed, recorded whether or not it failed.
#[derive(Debug, Default)]
pub struct TransitionAttempt {
    /// Attempt number, starting at 1.
    pub number: usize,
    /// Operation that began the step.
    pub step: TransitionStep,
    /// Why the probe failed, when a probe ran.
    pub probe_error: Option<ProbeError>,
    /// Why the failed connection was not confirmed closed.
    pub retirement_error: Option<CloseFailure>,
    /// Why the reopen after a failed probe, or the reopen step, failed.
    pub reopen_error: Option<ModemOpenFailure>,
    /// Whether the probe read a complete version frame.
    pub version_proved: bool,
}

impl TransitionAttempt {
    const fn numbered(number: usize, step: TransitionStep) -> Self {
        Self {
            number,
            step,
            probe_error: None,
            retirement_error: None,
            reopen_error: None,
            version_proved: false,
        }
    }

    /// Whether the reopen failed in a way that permits no further attempt.
    #[must_use]
    pub fn reopen_refused(&self) -> bool {
        self.reopen_error
            .as_ref()
            .is_some_and(|error| !error.retry_allowed)
    }
}

/// Why the transition stopped without a proved connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TransitionError {
    /// Cancellation was requested at a step boundary.
    #[error("MMDVM transition cancelled")]
    Cancelled,
    /// [`WINDOW`] elapsed before a complete version frame.
    #[error("MMDVM transition window expired")]
    WindowExpired,
    /// A failed connection was not confirmed closed, so no reopen followed.
    #[error("MMDVM transition stopped because the previous connection did not close cleanly")]
    RetirementFailed,
    /// The host refused another reopen; the last attempt holds its failure.
    #[error("the modem host refused another reopen")]
    ReopenRefused,
}

/// The proved connection, if one was obtained, plus every attempt made.
#[derive(Debug)]
pub struct TransitionReport<T> {
    /// The connection that answered `GET_VERSION`, when one did.
    pub proof: Option<ProvenModem<T>>,
    /// Every probe and reopen step, in order.
    pub attempts: Vec<TransitionAttempt>,
    /// Why no proved connection is returned.
    pub error: Option<TransitionError>,
    /// Why a connection still held at the end was not confirmed closed.
    pub cleanup_error: Option<CloseFailure>,
}

impl<T> TransitionReport<T> {
    const fn pending() -> Self {
        Self {
            proof: None,
            attempts: Vec::new(),
            error: None,
            cleanup_error: None,
        }
    }

    /// Whether every connection this transition held was confirmed closed or
    /// is the returned proof.
    #[must_use]
    pub fn released(&self) -> bool {
        self.cleanup_error.is_none()
            && self
                .attempts
                .iter()
                .all(|attempt| attempt.retirement_error.is_none())
    }
}

/// Probe for complete MMDVM framing within [`WINDOW`], reopening as needed.
///
/// `initial` is the connection retained after the acknowledged MCP exit, or
/// `None` when it was already closed. Every step begins with a silent
/// [`POLL_INTERVAL`] wait. A failed probe hands its connection to
/// [`ModemHost::retire`] before the link is reopened, and any connection still
/// held at cancellation or expiry is retired too. [`WINDOW`] bounds new
/// acquisition work only: joined reopen work and the close budget can extend
/// the total time before this returns.
pub async fn acquire_modem<M: ModemHost>(
    host: &mut M,
    initial: Option<M::Connection>,
    cancelled: &AtomicBool,
) -> TransitionReport<M::Connection> {
    acquire_modem_within(host, initial, cancelled, WINDOW).await
}

/// [`acquire_modem`] with an explicit window instead of [`WINDOW`].
pub async fn acquire_modem_within<M: ModemHost>(
    host: &mut M,
    initial: Option<M::Connection>,
    cancelled: &AtomicBool,
    window: Duration,
) -> TransitionReport<M::Connection> {
    let deadline = Instant::now() + window;
    let mut report = TransitionReport::pending();
    let mut owner = initial;
    loop {
        if let Some(error) = stop_reason(deadline, cancelled) {
            report.error = Some(error);
            break;
        }
        if let Err(error) = wait(host, deadline, cancelled).await {
            report.error = Some(error);
            break;
        }
        if owner.is_none() {
            let mut attempt =
                TransitionAttempt::numbered(report.attempts.len() + 1, TransitionStep::Reopen);
            owner = reopen(host, deadline, cancelled, &mut attempt).await;
            let refused = attempt.reopen_refused();
            report.attempts.push(attempt);
            if refused {
                report.error = Some(TransitionError::ReopenRefused);
                break;
            }
            if owner.is_none() {
                continue;
            }
        }
        if let Some(error) = stop_reason(deadline, cancelled) {
            report.error = Some(error);
            break;
        }
        let Some(transport) = owner.take() else {
            unreachable!("the reopen path either supplied a connection or continued");
        };
        match probe_step(host, transport, deadline, cancelled, &mut report).await {
            ProbeStep::Proved(proof) => {
                if let Some(error) = stop_reason(deadline, cancelled) {
                    report.error = Some(error);
                    owner = Some(proof.into_transport());
                } else {
                    report.proof = Some(proof);
                }
                break;
            }
            ProbeStep::Continue(next) => owner = next,
            ProbeStep::Stopped(error) => {
                report.error = Some(error);
                break;
            }
        }
    }
    if let Some(transport) = owner {
        report.cleanup_error = host.retire(transport).await.err();
    }
    report
}

/// Result of one probe step.
enum ProbeStep<T> {
    /// The connection answered `GET_VERSION`.
    Proved(ProvenModem<T>),
    /// The probe failed; the connection was retired and possibly reopened.
    Continue(Option<T>),
    /// The probe failed and no further step may run.
    Stopped(TransitionError),
}

/// Probe `transport`; on failure retire it and reopen the link at once.
async fn probe_step<M: ModemHost>(
    host: &mut M,
    transport: M::Connection,
    deadline: Instant,
    cancelled: &AtomicBool,
    report: &mut TransitionReport<M::Connection>,
) -> ProbeStep<M::Connection> {
    let mut attempt = TransitionAttempt::numbered(report.attempts.len() + 1, TransitionStep::Probe);
    let budget = VERSION_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
    match ProvenModem::probe(transport, budget).await {
        Ok(proof) => {
            attempt.version_proved = true;
            report.attempts.push(attempt);
            ProbeStep::Proved(proof)
        }
        Err((transport, error)) => {
            attempt.probe_error = Some(error);
            attempt.retirement_error = host.retire(transport).await.err();
            let mut next = None;
            if attempt.retirement_error.is_none() && stop_reason(deadline, cancelled).is_none() {
                next = reopen(host, deadline, cancelled, &mut attempt).await;
            }
            let retirement_failed = attempt.retirement_error.is_some();
            let refused = attempt.reopen_refused();
            report.attempts.push(attempt);
            if retirement_failed {
                ProbeStep::Stopped(TransitionError::RetirementFailed)
            } else if refused {
                ProbeStep::Stopped(TransitionError::ReopenRefused)
            } else {
                ProbeStep::Continue(next)
            }
        }
    }
}

async fn reopen<M: ModemHost>(
    host: &mut M,
    deadline: Instant,
    cancelled: &AtomicBool,
    attempt: &mut TransitionAttempt,
) -> Option<M::Connection> {
    match host.reopen(deadline, cancelled).await {
        Ok(connection) => Some(connection),
        Err(error) => {
            attempt.reopen_error = Some(error);
            None
        }
    }
}

/// Wait one poll interval, bounded by the deadline and by cancellation.
async fn wait<M: ModemHost>(
    host: &mut M,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<(), TransitionError> {
    let duration = POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()));
    tokio::select! {
        biased;
        () = cancellation(cancelled) => return Err(TransitionError::Cancelled),
        result = tokio::time::timeout_at(deadline, host.wait(duration)) => {
            result.map_err(|_elapsed| TransitionError::WindowExpired)?;
        }
    }
    stop_reason(deadline, cancelled).map_or(Ok(()), Err)
}

/// Resolve once `cancelled` is set, polling every 10 ms.
async fn cancellation(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn stop_reason(deadline: Instant, cancelled: &AtomicBool) -> Option<TransitionError> {
    if cancelled.load(Ordering::Acquire) {
        Some(TransitionError::Cancelled)
    } else if Instant::now() >= deadline {
        Some(TransitionError::WindowExpired)
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
