//! Read-only MCP workflows' CAT reacquisition; never retries programming.

use super::{
    AtomicBool, Backend, CLOSE_TIMEOUT, ConnectionAttempt, Duration, Enumeration,
    EnumerationWindow, Failure, File, Identity, Ordering, PostExitVerification, Recorder, SETTLE,
    SerialCandidate, Serialize, SkipReason, TranscriptSummary, VerificationContext,
    VerificationGoal, VerificationOutcome, VerificationStage, attempt_identity, await_endpoint,
    finalize_capture, milliseconds, wait_recorded,
};

/// Explicit per-write and per-reply deadline for the three identity queries.
pub(super) const EXCHANGE_TIMEOUT: Duration = Duration::from_millis(1_500);
const READINESS_BUDGET: Duration = Duration::from_secs(60);
const RETRY_INTERVAL: Duration = Duration::from_secs(2);
const MAXIMUM_OPEN_ATTEMPTS: usize = 4;
// ID/FV/TY each have a write and reply deadline, followed by bounded close.
const ATTEMPT_ALLOWANCE: Duration = EXCHANGE_TIMEOUT
    .saturating_mul(6)
    .saturating_add(CLOSE_TIMEOUT);

/// Classification of one completed fresh-handle check, not a recovery command.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RetryAdmission {
    /// The first query timed out without any input; capture and close succeeded.
    SilentIdentityTimeout,
    /// No additional connection is permitted by this check's observations.
    Terminal,
}

/// Preserve every failed observation even if a later check succeeds.
#[derive(Debug, Serialize)]
struct ReadinessAttempt {
    enumerations: Vec<Enumeration>,
    connection: Option<ConnectionAttempt>,
    outcome: VerificationOutcome,
    retry_admission: RetryAdmission,
}

/// Evidence for the probe and backup's bounded, identity-only readiness policy.
///
/// Guarded writes and qualification experiments retain single-attempt
/// verification. This policy changes only fresh CAT handling after a completed
/// read-only MCP session; it cannot repeat entry, reads, exit, or settings writes.
#[derive(Debug, Serialize)]
pub(in crate::mcp) struct ReadinessVerification {
    identity_assurance: &'static str,
    settle_milliseconds: u64,
    readiness_budget_milliseconds: u64,
    retry_interval_milliseconds: u64,
    exchange_timeout_milliseconds: u64,
    attempt_allowance_milliseconds: u64,
    maximum_open_attempts: usize,
    elapsed_milliseconds: u64,
    attempts: Vec<ReadinessAttempt>,
    /// Completeness of the one transcript spanning all readiness attempts.
    pub(in crate::mcp) transcript: TranscriptSummary,
    /// Final policy outcome; failed checks remain in `attempts` on success.
    pub(in crate::mcp) outcome: VerificationOutcome,
}

impl ReadinessVerification {
    /// Record ineligibility without opening or querying a fresh connection.
    pub(in crate::mcp) fn skipped(reason: SkipReason, transcript: TranscriptSummary) -> Self {
        Self {
            identity_assurance: "endpoint_and_cat_tuple_only",
            settle_milliseconds: milliseconds(SETTLE),
            readiness_budget_milliseconds: milliseconds(READINESS_BUDGET),
            retry_interval_milliseconds: milliseconds(RETRY_INTERVAL),
            exchange_timeout_milliseconds: milliseconds(EXCHANGE_TIMEOUT),
            attempt_allowance_milliseconds: milliseconds(ATTEMPT_ALLOWANCE),
            maximum_open_attempts: MAXIMUM_OPEN_ATTEMPTS,
            elapsed_milliseconds: 0,
            attempts: Vec::new(),
            transcript,
            outcome: VerificationOutcome::Skipped { reason },
        }
    }

    /// A match counts only with complete, durably synchronized evidence.
    pub(in crate::mcp) const fn succeeded(&self) -> bool {
        self.transcript.complete && matches!(self.outcome, VerificationOutcome::Matched)
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

/// Reserve complete identity exchanges and cleanup without cancelling either.
pub(super) fn can_dispatch(now: Duration, deadline: Duration) -> bool {
    deadline.saturating_sub(now) >= ATTEMPT_ALLOWANCE
}

pub(super) fn budget_exhausted(report: &mut PostExitVerification) {
    report.fail(
        VerificationStage::Readiness,
        &std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "CAT readiness budget has insufficient time for identity and close",
        ),
    );
}

/// Reacquire only after the caller has proved MCP exit, capture and handle release.
///
/// Retry admission requires a typed timeout, one completed initial ID write,
/// no input bytes, successful close/drop, and complete synchronized capture.
/// Every new open requires fresh exact-endpoint enumeration. No MCP, Gateway,
/// setter, baud change, packet exit, transport reopen or reset is dispatched.
/// Cancellation finishes the bounded identity attempt and close before stopping.
pub(in crate::mcp) async fn verify_readiness(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: u32,
    original_identity: &Identity,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
) -> ReadinessVerification {
    let mut result = ReadinessVerification::skipped(SkipReason::Cancelled, recorder.summary());
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
        goal: VerificationGoal::IdentityOnly,
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
            if can_dispatch(backend.now(), deadline) {
                let attempt =
                    attempt_identity(backend, &selected, context, recorder, &mut check).await;
                recorder = attempt.recorder;
                retry = attempt.silent_identity_timeout;
            } else {
                budget_exhausted(&mut check);
            }
        }
        // A retry is new protocol traffic, so its predecessor's evidence must
        // already be durable. Synchronization failure never opens another port.
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
        if deadline.saturating_sub(backend.now()) < RETRY_INTERVAL + ATTEMPT_ALLOWANCE {
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
