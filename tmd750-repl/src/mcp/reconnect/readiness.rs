//! Bounded CAT reacquisition after an acknowledged MCP exit.
//!
//! Attempts re-enumerate the selected endpoint, open it and read the ID/FV/TY
//! tuple, optionally followed by one `GW` query once the tuple matches.
//! Nothing here re-enters MCP, re-reads memory, or writes a setting.

use super::{
    AtomicBool, Backend, CLOSE_TIMEOUT, ConnectionAttempt, Duration, DvGatewayMode, Enumeration,
    EnumerationWindow, Failure, File, GatewayEvidence, Identity, Ordering, PostExitVerification,
    Recorder, SETTLE, SerialCandidate, Serialize, SkipReason, TranscriptSummary,
    VerificationContext, VerificationGoal, VerificationOutcome, VerificationStage,
    attempt_identity, await_endpoint, finalize_capture, milliseconds, wait_recorded,
};

/// Deadline for each write and each reply of the three identity queries (1.5 s).
pub(super) const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Window covering enumeration polling, opens, queries and closes (60 s),
/// measured from the end of the settle wait.
const READINESS_BUDGET: Duration = Duration::from_secs(60);
/// Wait between one attempt's close and the next open (2 s).
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
/// Opens permitted within one reacquisition.
///
/// Attempts run about 3.5 s apart, so six cover roughly the first 25 s of
/// the budget. On firmware 1.02 the operation-panel endpoint has answered
/// `ID` on the third or fourth open after an MCP exit; the main-unit endpoint
/// answers on the first open once it re-enumerates.
pub(crate) const MAXIMUM_OPEN_ATTEMPTS: usize = 6;
/// Budget one identity-only attempt may still need: a write and a reply
/// deadline for each of ID, FV and TY, followed by the bounded close.
const ATTEMPT_ALLOWANCE: Duration = EXCHANGE_TIMEOUT
    .saturating_mul(6)
    .saturating_add(CLOSE_TIMEOUT);
/// `ATTEMPT_ALLOWANCE` plus the write and reply deadlines of the one `GW` query.
const GATEWAY_ATTEMPT_ALLOWANCE: Duration = EXCHANGE_TIMEOUT
    .saturating_mul(8)
    .saturating_add(CLOSE_TIMEOUT);

/// Budget one attempt of `goal` may still need before its close completes.
pub(super) const fn attempt_allowance(goal: VerificationGoal) -> Duration {
    match goal {
        VerificationGoal::IdentityOnly => ATTEMPT_ALLOWANCE,
        VerificationGoal::GatewayOff => GATEWAY_ATTEMPT_ALLOWANCE,
    }
}

/// Whether this completed check permits one more open attempt.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    /// The first query timed out without any input; capture and close succeeded.
    SilentIdentityTimeout,
    /// No additional connection is permitted by this check's observations.
    Terminal,
}

/// Record of one open attempt.
///
/// Holds its enumeration snapshots, the connection attempt, the outcome and
/// the retry decision. Failed attempts stay in the report when a later attempt
/// succeeds.
#[derive(Debug, Serialize)]
struct ReadinessAttempt {
    enumerations: Vec<Enumeration>,
    connection: Option<ConnectionAttempt>,
    outcome: VerificationOutcome,
    retry_admission: RetryAdmission,
}

/// Serialized report of one bounded CAT reacquisition.
///
/// Holds the settle delay, budgets, retry interval and per-attempt allowance in
/// milliseconds, the open-attempt cap, the Gateway state one `GW` query had to
/// report when the caller required it, one record per attempt, the shared
/// transcript summary and the final outcome.
#[derive(Debug, Serialize)]
pub(crate) struct ReadinessVerification {
    identity_assurance: &'static str,
    settle_milliseconds: u64,
    readiness_budget_milliseconds: u64,
    retry_interval_milliseconds: u64,
    exchange_timeout_milliseconds: u64,
    attempt_allowance_milliseconds: u64,
    maximum_open_attempts: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_gateway_mode: Option<GatewayEvidence>,
    elapsed_milliseconds: u64,
    attempts: Vec<ReadinessAttempt>,
    /// Summary of the one transcript spanning all readiness attempts.
    pub(in crate::mcp) transcript: TranscriptSummary,
    /// Final outcome; failed attempts remain in `attempts` on success.
    pub(in crate::mcp) outcome: VerificationOutcome,
}

impl ReadinessVerification {
    /// Report that reacquisition never started, naming `reason`.
    ///
    /// No connection is opened and no query is sent.
    pub(in crate::mcp) fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self {
            identity_assurance: "endpoint_and_cat_tuple_only",
            settle_milliseconds: milliseconds(SETTLE),
            readiness_budget_milliseconds: milliseconds(READINESS_BUDGET),
            retry_interval_milliseconds: milliseconds(RETRY_INTERVAL),
            exchange_timeout_milliseconds: milliseconds(EXCHANGE_TIMEOUT),
            attempt_allowance_milliseconds: milliseconds(ATTEMPT_ALLOWANCE),
            maximum_open_attempts: MAXIMUM_OPEN_ATTEMPTS,
            required_gateway_mode: None,
            elapsed_milliseconds: 0,
            attempts: Vec::new(),
            transcript,
            outcome: VerificationOutcome::Skipped { reason },
        }
    }

    /// True when the outcome is `Matched` and the transcript is complete.
    pub(crate) const fn succeeded(&self) -> bool {
        self.transcript.complete && matches!(self.outcome, VerificationOutcome::Matched)
    }

    /// Identity and Gateway mode read by a reacquisition that required Gateway Off.
    ///
    /// Returns `None` when the reacquisition read the identity only, did not
    /// succeed, or its final attempt did not observe Gateway Off.
    pub(in crate::mcp) fn gateway_off_evidence(&self) -> Option<(&Identity, DvGatewayMode)> {
        if !self.succeeded()
            || self.required_gateway_mode != Some(GatewayEvidence(DvGatewayMode::Off))
        {
            return None;
        }
        let connection = self.attempts.last()?.connection.as_ref()?;
        let mode = connection.gateway_mode?.0;
        if mode != DvGatewayMode::Off {
            return None;
        }
        Some((&connection.identity.as_ref()?.0, mode))
    }

    fn fail(&mut self, stage: VerificationStage, message: &str) {
        self.outcome = VerificationOutcome::Failed {
            stage,
            error: Failure::from_error(&std::io::Error::other(message)),
        };
    }

    fn finalize(&mut self, recorder: &mut Recorder<File>) {
        if let Err(error) = recorder.synchronize()
            && !matches!(self.outcome, VerificationOutcome::Failed { .. })
        {
            self.outcome = VerificationOutcome::Failed {
                stage: VerificationStage::Capture,
                error: Failure::from_error(&error),
            };
        }
        self.transcript = recorder.summary();
    }
}

/// True when `deadline - now` still covers one attempt of `goal`.
///
/// Both durations are elapsed times from the backend's clock origin.
pub(super) fn can_dispatch(now: Duration, deadline: Duration, goal: VerificationGoal) -> bool {
    deadline.saturating_sub(now) >= attempt_allowance(goal)
}

/// Record a `Readiness` failure: too little budget remains for one attempt.
pub(super) fn budget_exhausted(report: &mut PostExitVerification) {
    report.fail(
        VerificationStage::Readiness,
        &std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "CAT readiness budget has insufficient time for identity and close",
        ),
    );
}

/// Reacquire the CAT identity tuple on fresh connections within 60 s.
///
/// Call this only after an acknowledged MCP exit with the original connection
/// closed and its transcript complete. Each attempt re-enumerates the exact
/// endpoint, opens it once and reads ID/FV/TY with `EXCHANGE_TIMEOUT` per write
/// and per reply. Another open follows only when the identity query timed out
/// after the ID write with no bytes received, the close succeeded and the
/// transcript synchronized; at most `MAXIMUM_OPEN_ATTEMPTS` opens run,
/// `RETRY_INTERVAL` apart. No MCP entry, Gateway query, setter, baud change,
/// packet exit, transport reopen or reset is sent. Cancellation finishes the
/// current identity attempt and its close before stopping.
pub(crate) async fn verify_readiness(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> ReadinessVerification {
    verify_readiness_with(
        backend,
        endpoint,
        baud,
        original_identity,
        recorder,
        cancelled,
        VerificationGoal::IdentityOnly,
    )
    .await
}

/// Reacquire the CAT identity tuple, then require one `GW` query to report Off.
///
/// The open, retry and budget rules are those of [`verify_readiness`], with
/// each attempt's allowance extended by the `GW` exchange. The query is sent
/// once per attempt, only after that attempt's complete tuple matched; a
/// Gateway state other than Off or a failed `GW` read ends the reacquisition
/// on that attempt, since only a silent identity timeout admits another open.
/// [`ReadinessVerification::gateway_off_evidence`] returns the observations
/// after success.
pub(crate) async fn verify_readiness_gateway_off(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> ReadinessVerification {
    verify_readiness_with(
        backend,
        endpoint,
        baud,
        original_identity,
        recorder,
        cancelled,
        VerificationGoal::GatewayOff,
    )
    .await
}

async fn verify_readiness_with(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
    goal: VerificationGoal,
) -> ReadinessVerification {
    let allowance = attempt_allowance(goal);
    let mut result = ReadinessVerification::skipped(SkipReason::Cancelled, recorder.summary());
    result.attempt_allowance_milliseconds = milliseconds(allowance);
    if matches!(goal, VerificationGoal::GatewayOff) {
        result.required_gateway_mode = Some(GatewayEvidence(DvGatewayMode::Off));
    }
    if cancelled.load(Ordering::Relaxed) || recorder.ensure_complete().is_err() {
        result.finalize(&mut recorder);
        return result;
    }
    wait_recorded(backend, &mut recorder, SETTLE).await;
    let window = EnumerationWindow {
        started: backend.now(),
        budget: READINESS_BUDGET,
    };
    let deadline = window.started.saturating_add(window.budget);
    let context = VerificationContext {
        baud,
        original_identity,
        cancelled,
        goal,
        dispatch_deadline: Some(deadline),
    };
    loop {
        if cancelled.load(Ordering::Relaxed) {
            result.outcome = VerificationOutcome::Cancelled;
            break;
        }
        let mut check = PostExitVerification::skipped(SkipReason::Cancelled, recorder.summary());
        let selected = await_endpoint(
            backend,
            endpoint,
            &mut recorder,
            &mut check,
            cancelled,
            window,
        )
        .await;
        let mut retry = false;
        if let Some(selected) = selected {
            if can_dispatch(backend.now(), deadline, goal) {
                let attempt =
                    attempt_identity(backend, &selected, context, recorder, &mut check).await;
                recorder = attempt.recorder;
                retry = attempt.silent_identity_timeout;
            } else {
                budget_exhausted(&mut check);
            }
        }
        // Another open requires the previous attempt's transcript to be
        // synchronized; a synchronization failure ends the loop.
        finalize_capture(&mut recorder, &mut check);
        retry &= check.transcript.complete;
        result.outcome = check.outcome.clone();
        result.attempts.push(ReadinessAttempt {
            enumerations: check.enumerations,
            connection: check.attempt,
            outcome: check.outcome,
            retry_admission: if retry {
                RetryAdmission::SilentIdentityTimeout
            } else {
                RetryAdmission::Terminal
            },
        });
        if !retry {
            break;
        }
        if cancelled.load(Ordering::Relaxed) {
            result.outcome = VerificationOutcome::Cancelled;
            break;
        }
        if result.attempts.len() >= MAXIMUM_OPEN_ATTEMPTS {
            result.fail(
                VerificationStage::Readiness,
                "CAT readiness open-attempt cap exhausted",
            );
            break;
        }
        if deadline.saturating_sub(backend.now()) < RETRY_INTERVAL + allowance {
            result.fail(
                VerificationStage::Readiness,
                "CAT readiness budget exhausted before retry",
            );
            break;
        }
        wait_recorded(backend, &mut recorder, RETRY_INTERVAL).await;
    }
    result.elapsed_milliseconds = milliseconds(backend.now().saturating_sub(window.started));
    result.finalize(&mut recorder);
    result
}
