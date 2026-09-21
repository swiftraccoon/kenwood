//! Serialized results of a managed run: per-connection outcomes, the pages
//! compared, possibly written and verified, and the restoration state.

use kenwood_tmd750::{Page, PageReplacement};
use serde::Serialize;

use crate::capture::{Failure, TranscriptSummary};
use crate::mcp::reconnect::ReadinessVerification;

use super::super::report::{IdentityEvidence, WorkflowResult as ProbeResult};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FailureStage {
    Admission,
    Operation,
    Exit,
    Close,
    Capture,
    Journal,
}

#[derive(Debug, Serialize)]
pub(super) struct Problem {
    stage: FailureStage,
    error: Failure,
}

impl Problem {
    pub(super) fn new(stage: FailureStage, error: &(dyn std::error::Error + 'static)) -> Self {
        Self {
            stage,
            error: Failure::from_error(error),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Exit {
    NotEntered,
    Uncertain,
    Acknowledged,
}

#[derive(Debug, Serialize)]
pub(super) struct PageEvidence {
    address: u32,
    length: usize,
}

impl From<Page> for PageEvidence {
    fn from(page: Page) -> Self {
        Self {
            address: page.address().as_u32(),
            length: page.len(),
        }
    }
}

#[derive(Serialize)]
pub(super) struct IntendedPage<'a> {
    address: u32,
    length: usize,
    expected: &'a [u8],
    replacement: &'a [u8],
}

impl<'a> From<&'a PageReplacement> for IntendedPage<'a> {
    fn from(page: &'a PageReplacement) -> Self {
        Self {
            address: page.page().address().as_u32(),
            length: page.page().len(),
            expected: page.expected(),
            replacement: page.replacement(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct ConnectionResult {
    pub(super) identity: Option<IdentityEvidence>,
    pub(super) gateway: Option<u8>,
    pub(super) exit: Exit,
    pub(super) intent_recorded: bool,
    pub(super) compared: Vec<PageEvidence>,
    pub(super) possible: Vec<PageEvidence>,
    pub(super) verified: Vec<PageEvidence>,
    pub(super) problems: Vec<Problem>,
    pub(super) transcript: TranscriptSummary,
}

impl ConnectionResult {
    pub(super) const fn new(transcript: TranscriptSummary) -> Self {
        Self {
            identity: None,
            gateway: None,
            exit: Exit::NotEntered,
            intent_recorded: false,
            compared: Vec::new(),
            possible: Vec::new(),
            verified: Vec::new(),
            problems: Vec::new(),
            transcript,
        }
    }

    pub(super) fn clean_release(&self) -> bool {
        self.transcript.complete
            && self
                .problems
                .iter()
                .all(|problem| matches!(problem.stage, FailureStage::Operation))
    }

    pub(super) const fn succeeded(&self) -> bool {
        self.problems.is_empty()
            && self.transcript.complete
            && self.identity.is_some()
            && self.gateway.is_some()
    }
}

#[derive(Debug, Serialize)]
pub(super) struct PhaseResult {
    pub(super) exchange: ConnectionResult,
    pub(super) readiness: Option<ReadinessVerification>,
    pub(super) verification: Option<ConnectionResult>,
}

impl PhaseResult {
    pub(super) fn control_ready(&self) -> bool {
        self.exchange.clean_release()
            && self.exchange.exit == Exit::Acknowledged
            && self
                .readiness
                .as_ref()
                .is_some_and(ReadinessVerification::succeeded)
            && self
                .verification
                .as_ref()
                .is_some_and(ConnectionResult::succeeded)
    }

    pub(super) fn succeeded(&self) -> bool {
        self.exchange.succeeded() && self.control_ready()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Restoration {
    #[default]
    NotOwed,
    Owed,
    Verified,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct WorkflowResult {
    pub(super) entry: Option<PhaseResult>,
    pub(super) probe: Option<ProbeResult>,
    pub(super) restore: Option<PhaseResult>,
    pub(super) restoration: Restoration,
    pub(super) problems: Vec<Problem>,
}

impl WorkflowResult {
    pub(super) fn succeeded(&self) -> bool {
        self.problems.is_empty()
            && self.entry.as_ref().is_some_and(PhaseResult::succeeded)
            && self.probe.as_ref().is_some_and(|probe| {
                probe.succeeded()
                    && matches!(probe.outcome, super::super::Outcome::MmdvmObserved { .. })
            })
            && self.restoration != Restoration::Owed
            && self.restore.as_ref().is_none_or(PhaseResult::succeeded)
    }
}
