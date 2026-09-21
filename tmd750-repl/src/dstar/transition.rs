//! Bounded MMDVM acquisition after the Terminal update has been written.
//!
//! This module never enters MCP, issues CAT, changes modem configuration, or
//! selects an endpoint; the backend reopens the exact endpoint its caller
//! chose. Complete version framing proves the current connection's wire
//! protocol, not the radio identity or the persistent Gateway setting.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kenwood_transport::Transport;
use serde::Serialize;
use tokio::time::Instant;

use crate::capture::Failure;

use super::modem::ProvenModem;

#[cfg(test)]
mod tests;

/// Total window for acquiring MMDVM framing after the Terminal update. Host
/// policy, not a measured firmware bound.
const WINDOW: Duration = Duration::from_secs(90);
/// Silent wait between successive reopen and probe attempts inside `WINDOW`.
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Budget for one `GET_VERSION` request and its complete reply.
const VERSION_BUDGET: Duration = Duration::from_secs(2);
/// Budget for the single close performed when retiring a connection.
const CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// An opening implementation pinned to one previously selected endpoint.
pub(crate) trait Backend {
    type Connection: Transport;

    /// Reopen the caller's exact endpoint before `deadline`.
    ///
    /// An in-flight native open is joined, with its cleanup, before returning,
    /// including on cancellation or expiry. A connection returned after
    /// `deadline` is rejected and closed by the transition.
    async fn reopen(
        &mut self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self::Connection, ReopenFailure>;

    /// Wait `duration` without sending anything on the endpoint.
    async fn wait(&mut self, duration: Duration, cancelled: &AtomicBool) -> Result<(), Failure>;

    /// Close and drop `owner`, returning any close or capture failure.
    ///
    /// Capture backends synchronize the final transcript here and return close
    /// and capture failures separately. This call is never cancelled, and any
    /// failure prevents a further reopen. The default closes within
    /// `CLOSE_BUDGET`, then drops the connection.
    async fn retire(&mut self, owner: Self::Connection) -> Result<(), Failure> {
        close(owner).await
    }
}

/// A failed reopen attempt.
#[derive(Debug)]
pub(crate) struct ReopenFailure {
    /// Why the reopen failed; per-attempt detail stays in the backend's report.
    pub(crate) error: Failure,
    /// Whether another attempt may run inside the window. False after a capture
    /// or cleanup failure, an interruption, or a non-retryable opening stage.
    pub(crate) retry_allowed: bool,
}

/// Which operation began a transition step.
#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Step {
    #[default]
    Probe,
    Reopen,
}

/// What one probe or reopen step observed, recorded whether or not it failed.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Attempt {
    pub(crate) number: usize,
    pub(crate) step: Step,
    pub(crate) probe_error: Option<Failure>,
    pub(crate) retirement_error: Option<Failure>,
    pub(crate) reopen_error: Option<Failure>,
    pub(crate) reopen_retry_allowed: bool,
    pub(crate) version_proved: bool,
}

impl Attempt {
    fn refusal(&self) -> Option<Failure> {
        if self.reopen_retry_allowed {
            None
        } else {
            self.reopen_error.clone()
        }
    }
}

/// The proved connection, if one was obtained, plus every attempt made.
pub(crate) struct Outcome<T> {
    pub(super) proof: Option<ProvenModem<T>>,
    pub(crate) attempts: Vec<Attempt>,
    pub(crate) error: Option<Failure>,
    pub(crate) cleanup_error: Option<Failure>,
}

impl<T> Default for Outcome<T> {
    fn default() -> Self {
        Self {
            proof: None,
            attempts: Vec::new(),
            error: None,
            cleanup_error: None,
        }
    }
}

/// Probe for complete MMDVM framing within `WINDOW`, reopening as needed.
///
/// `initial` is the connection retained after the acknowledged MCP exit and its
/// two-second silent settle, or `None` when it was already closed. A failed
/// probe closes its connection before the pinned endpoint is reopened, and any
/// connection still held at cancellation or expiry is closed too. `WINDOW`
/// bounds new acquisition work only: joined opening cleanup and the two-second
/// close budget, plus a capture backend's final synchronization, can extend the
/// total time before this returns.
pub(crate) async fn run<B: Backend>(
    backend: &mut B,
    initial: Option<B::Connection>,
    cancelled: &AtomicBool,
) -> Outcome<B::Connection> {
    run_with_window(backend, initial, cancelled, WINDOW).await
}

async fn run_with_window<B: Backend>(
    backend: &mut B,
    mut owner: Option<B::Connection>,
    cancelled: &AtomicBool,
    window: Duration,
) -> Outcome<B::Connection> {
    let deadline = Instant::now() + window;
    let mut outcome = Outcome::default();
    loop {
        if let Some(error) = stop_reason(deadline, cancelled) {
            outcome.error = Some(error);
            break;
        }
        if let Err(error) = wait(backend, deadline, cancelled).await {
            outcome.error = Some(error);
            break;
        }
        if owner.is_none() {
            let mut attempt = Attempt {
                number: outcome.attempts.len() + 1,
                step: Step::Reopen,
                ..Attempt::default()
            };
            owner = reopen(backend, deadline, cancelled, &mut attempt).await;
            let refusal = attempt.refusal();
            outcome.attempts.push(attempt);
            if let Some(error) = refusal {
                outcome.error = Some(error);
                break;
            }
            if owner.is_none() {
                continue;
            }
        }
        if let Some(error) = stop_reason(deadline, cancelled) {
            outcome.error = Some(error);
            break;
        }
        let Some(transport) = owner.take() else {
            unreachable!("the opening path either supplied an owner or continued");
        };
        let mut attempt = Attempt {
            number: outcome.attempts.len() + 1,
            ..Attempt::default()
        };
        let budget = VERSION_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
        match ProvenModem::probe(transport, budget).await {
            Ok(proof) => {
                attempt.version_proved = true;
                outcome.attempts.push(attempt);
                if let Some(error) = stop_reason(deadline, cancelled) {
                    outcome.error = Some(error);
                    owner = Some(proof.into_transport());
                } else {
                    outcome.proof = Some(proof);
                }
                break;
            }
            Err((transport, error)) => {
                attempt.probe_error = Some(Failure::from_error(&error));
                attempt.retirement_error = backend.retire(transport).await.err();
                if attempt.retirement_error.is_none() && stop_reason(deadline, cancelled).is_none()
                {
                    owner = reopen(backend, deadline, cancelled, &mut attempt).await;
                }
                let retirement_failed = attempt.retirement_error.is_some();
                let refusal = attempt.refusal();
                outcome.attempts.push(attempt);
                if retirement_failed {
                    outcome.error = Some(failure(
                        "MMDVM transition stopped because the previous connection did not close cleanly",
                    ));
                    break;
                }
                if let Some(error) = refusal {
                    outcome.error = Some(error);
                    break;
                }
            }
        }
    }
    if let Some(transport) = owner {
        outcome.cleanup_error = backend.retire(transport).await.err();
    }
    outcome
}

async fn reopen<B: Backend>(
    backend: &mut B,
    deadline: Instant,
    cancelled: &AtomicBool,
    attempt: &mut Attempt,
) -> Option<B::Connection> {
    match backend.reopen(deadline, cancelled).await {
        Ok(owner) => Some(owner),
        Err(error) => {
            attempt.reopen_retry_allowed = error.retry_allowed;
            attempt.reopen_error = Some(error.error);
            None
        }
    }
}

async fn wait<B: Backend>(
    backend: &mut B,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<(), Failure> {
    let duration = POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now()));
    tokio::select! {
        biased;
        () = cancellation(cancelled) => return Err(failure("MMDVM transition cancelled")),
        result = tokio::time::timeout_at(deadline, backend.wait(duration, cancelled)) => {
            result.map_err(|_| failure("MMDVM transition window expired during the silent wait"))??;
        }
    }
    stop_reason(deadline, cancelled).map_or(Ok(()), Err)
}

async fn cancellation(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn stop_reason(deadline: Instant, cancelled: &AtomicBool) -> Option<Failure> {
    if cancelled.load(Ordering::Acquire) {
        Some(failure("MMDVM transition cancelled"))
    } else if Instant::now() >= deadline {
        Some(failure("MMDVM transition window expired"))
    } else {
        None
    }
}

async fn close<T: Transport>(mut owner: T) -> Result<(), Failure> {
    tokio::time::timeout(CLOSE_BUDGET, owner.close())
        .await
        .map_err(|_| failure("MMDVM transition connection close exceeded its budget"))?
        .map_err(|error| Failure::from_error(&error))
}

fn failure(message: &str) -> Failure {
    Failure {
        message: message.to_owned(),
        causes: Vec::new(),
    }
}
