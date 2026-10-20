//! Bounded CAT reacquisition after an acknowledged MCP exit.
//!
//! [`readiness::verify_readiness`] re-enumerates the selected endpoint, opens
//! it and reads the ID/FV/TY tuple, optionally followed by one `GW` query once
//! the tuple matches, through a [`RecordingHost`] that writes every host call
//! to the verification transcript. This module serializes the library report.
//! Nothing here re-enters MCP, re-reads memory, or writes a setting.

use super::host::RecordingHost;
use super::{
    AtomicBool, Backend, ConnectionAttempt, DvGatewayMode, Enumeration, Failure, File,
    GatewayEvidence, Identity, ObservedIdentity, OperationOutcome, Ordering, Recorder, SETTLE,
    SerialCandidate, Serialize, SkipReason, TranscriptSummary, VerificationOutcome,
    VerificationStage, milliseconds,
};
use kenwood_tmd750::radio::readiness::{
    self, ATTEMPT_ALLOWANCE, CloseFailure, EXCHANGE_TIMEOUT, GATEWAY_ATTEMPT_ALLOWANCE,
    MAXIMUM_OPEN_ATTEMPTS, READINESS_BUDGET, RETRY_INTERVAL, ReadinessEnding, ReadinessError,
    ReadinessOutcome, ReadinessReport,
};

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

impl ReadinessAttempt {
    /// Serialize one library attempt; `capture_refused` names the `capture`
    /// stage for a failure that an incomplete transcript caused.
    fn from_library(attempt: readiness::ReadinessAttempt, capture_refused: bool) -> Self {
        let identity_read = attempt
            .connection
            .as_ref()
            .is_some_and(|connection| connection.identity.is_some());
        // The host's close fails with `CloseFailure::Host` only when the
        // transcript did not synchronize.
        let unsynchronized = attempt
            .connection
            .as_ref()
            .is_some_and(|connection| matches!(connection.close, Some(Err(CloseFailure::Host(_)))));
        let capture_refused = capture_refused
            || (unsynchronized
                && matches!(
                    attempt.outcome,
                    ReadinessOutcome::Failed(ReadinessError::Close)
                ));
        let open_error = match &attempt.outcome {
            ReadinessOutcome::Failed(ReadinessError::Open(error)) => {
                Some(Failure::from_error(error))
            }
            _ => None,
        };
        Self {
            enumerations: attempt
                .enumerations
                .into_iter()
                .map(|enumeration| Enumeration {
                    elapsed_milliseconds: milliseconds(enumeration.elapsed),
                    candidates: enumeration.candidates.iter().map(Into::into).collect(),
                })
                .collect(),
            connection: attempt
                .connection
                .map(|connection| connection_evidence(connection, open_error)),
            outcome: verification_outcome(&attempt.outcome, identity_read, capture_refused),
            retry_admission: match attempt.retry {
                readiness::RetryAdmission::SilentIdentityTimeout => {
                    RetryAdmission::SilentIdentityTimeout
                }
                readiness::RetryAdmission::Terminal => RetryAdmission::Terminal,
            },
        }
    }
}

/// Serialize one library connection; `open_error` is the failure of an
/// unopened connection.
fn connection_evidence(
    connection: readiness::ConnectionAttempt,
    open_error: Option<Failure>,
) -> ConnectionAttempt {
    ConnectionAttempt {
        endpoint: (&connection.endpoint).into(),
        open: match (connection.opened, open_error) {
            (true, _) => OperationOutcome::Succeeded,
            (false, Some(error)) => OperationOutcome::Failed { error },
            (false, None) => OperationOutcome::Failed {
                error: Failure::from_error(&std::io::Error::other("the open failed")),
            },
        },
        identity: connection.identity.map(ObservedIdentity),
        gateway_mode: connection.gateway.map(GatewayEvidence),
        close: connection.close.map(|close| match close {
            Ok(()) => OperationOutcome::Succeeded,
            Err(error) => OperationOutcome::Failed {
                error: Failure::from_error(&error),
            },
        }),
    }
}

/// Serialize one attempt outcome.
fn verification_outcome(
    outcome: &ReadinessOutcome,
    identity_read: bool,
    capture_refused: bool,
) -> VerificationOutcome {
    match outcome {
        ReadinessOutcome::Matched => VerificationOutcome::Matched,
        ReadinessOutcome::Cancelled => VerificationOutcome::Cancelled,
        ReadinessOutcome::Failed(error) => VerificationOutcome::Failed {
            stage: if capture_refused {
                VerificationStage::Capture
            } else {
                failure_stage(error, identity_read)
            },
            error: Failure::from_error(error),
        },
    }
}

/// The report stage of a library failure; a CAT failure after a complete
/// identity belongs to the `GW` query.
const fn failure_stage(error: &ReadinessError, identity_read: bool) -> VerificationStage {
    match error {
        ReadinessError::Enumeration(_)
        | ReadinessError::EndpointAbsent { .. }
        | ReadinessError::Unsettled { .. } => VerificationStage::Enumeration,
        ReadinessError::Reenumeration(_) => VerificationStage::EndpointSelection,
        ReadinessError::Open(_) => VerificationStage::Open,
        ReadinessError::Cat(_) if identity_read => VerificationStage::Gateway,
        ReadinessError::Cat(_) => VerificationStage::Identity,
        ReadinessError::IdentityMismatch { .. } => VerificationStage::IdentityMismatch,
        ReadinessError::GatewayMismatch { .. } => VerificationStage::GatewayMismatch,
        ReadinessError::Close => VerificationStage::Close,
        _ => VerificationStage::Readiness,
    }
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

    /// Take the attempts, elapsed time and final outcome from `report`.
    ///
    /// `capture_refused` marks the last attempt's failure as the `capture`
    /// stage.
    fn absorb(&mut self, report: ReadinessReport, capture_refused: bool) {
        let last = report.attempts.len().saturating_sub(1);
        self.elapsed_milliseconds = milliseconds(report.elapsed);
        self.attempts = report
            .attempts
            .into_iter()
            .enumerate()
            .map(|(index, attempt)| {
                ReadinessAttempt::from_library(attempt, capture_refused && index == last)
            })
            .collect();
        match report.ending {
            ReadinessEnding::Matched => self.outcome = VerificationOutcome::Matched,
            ReadinessEnding::Cancelled => self.outcome = VerificationOutcome::Cancelled,
            ReadinessEnding::Failed => {
                self.outcome = self
                    .attempts
                    .last()
                    .map_or(VerificationOutcome::Cancelled, |attempt| {
                        attempt.outcome.clone()
                    });
            }
            ReadinessEnding::AttemptCapExhausted => self.fail(
                VerificationStage::Readiness,
                "CAT readiness open-attempt cap exhausted",
            ),
            ReadinessEnding::BudgetExhausted => self.fail(
                VerificationStage::Readiness,
                "CAT readiness budget exhausted before retry",
            ),
        }
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

/// Reacquire the CAT identity tuple on fresh connections within 60 s.
///
/// Call this only after an acknowledged MCP exit with the original connection
/// closed and its transcript complete. Runs [`readiness::verify_readiness`]:
/// each attempt re-enumerates the exact endpoint, opens it once and reads
/// ID/FV/TY with `EXCHANGE_TIMEOUT` per write and per reply. Another open
/// follows only when the identity query timed out after the ID write with no
/// bytes received, the close succeeded and the transcript synchronized; at
/// most `MAXIMUM_OPEN_ATTEMPTS` opens run, `RETRY_INTERVAL` apart. No MCP
/// entry, Gateway query, setter, baud change, packet exit, transport reopen or
/// reset is sent. Cancellation finishes the current identity attempt and its
/// close before stopping.
pub(crate) async fn verify_readiness(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
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
        None,
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
    baud: std::num::NonZeroU32,
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
        Some(DvGatewayMode::Off),
    )
    .await
}

async fn verify_readiness_with(
    backend: &mut impl Backend,
    endpoint: &SerialCandidate,
    baud: std::num::NonZeroU32,
    original_identity: &Identity,
    mut recorder: Recorder<File>,
    cancelled: &AtomicBool,
    gateway: Option<DvGatewayMode>,
) -> ReadinessVerification {
    let mut result = ReadinessVerification::skipped(SkipReason::Cancelled, recorder.summary());
    if gateway.is_some() {
        result.attempt_allowance_milliseconds = milliseconds(GATEWAY_ATTEMPT_ALLOWANCE);
    }
    result.required_gateway_mode = gateway.map(GatewayEvidence);
    if cancelled.load(Ordering::Relaxed) || recorder.ensure_complete().is_err() {
        result.finalize(&mut recorder);
        return result;
    }
    let mut host = RecordingHost::new(backend, endpoint, recorder);
    let report = Box::pin(readiness::verify_readiness(
        &mut host,
        endpoint,
        baud,
        original_identity,
        gateway,
        cancelled,
    ))
    .await;
    let capture_refused = host.capture_refused();
    result.absorb(report, capture_refused);
    if let Some(mut recorder) = host.into_recorder() {
        result.finalize(&mut recorder);
    } else {
        result.fail(
            VerificationStage::Capture,
            "the readiness check did not return the transcript",
        );
        result.transcript.complete = false;
    }
    result
}
