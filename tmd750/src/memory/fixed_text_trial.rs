//! Private sequencing shared by the two independently bounded text trials.

use std::num::NonZeroU64;

use super::{
    PmNameTrialError, PmNameTrialEvent, PmNameTrialSession, PmNameTrialStatus, PmNameTrialWrite,
};
use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page};

mod sealed {
    pub(crate) trait Sealed {}
}

impl sealed::Sealed for super::PmNameTrial {}
impl sealed::Sealed for super::MyCallsignTrial {}

/// Closed selection of the independently prepared fixed field and page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixedTextTrialScope {
    /// Global PM1 label, preserving its existing wire sequence.
    Pm1,
    /// PM-off MY1 callsign, with additional immutable control-page guards.
    My1,
}

/// Fresh-session facts the shared sequence requires before it advances.
pub(crate) struct FixedTextTrialObservation<'a> {
    /// Never-reused connection identity within this trial.
    pub(crate) id: NonZeroU64,
    /// Fresh complete CAT identity.
    pub(crate) identity: &'a Identity,
    /// Fresh memory-format byte at address 10.
    pub(crate) memory_format: u8,
    /// Complete fresh target-page bytes.
    pub(crate) whole_page: &'a [u8],
    /// Complete fresh immutable control page, required only for MY1.
    pub(crate) control_page: Option<&'a [u8]>,
    /// Fresh pre-entry read-only gateway observation, required only for MY1.
    pub(crate) gateway_mode: Option<DvGatewayMode>,
}

/// Sealed bridge from fixed preparation policies to the private session runner.
pub(crate) trait FixedTextTrial: sealed::Sealed + Send + Sync {
    /// Fixed scope; this cannot be supplied by a public caller.
    fn scope(&self) -> FixedTextTrialScope;
    /// Exact baseline CAT identity.
    fn identity(&self) -> &Identity;
    /// Sole validated target page.
    fn page(&self) -> Page;
    /// Complete immutable before-image.
    fn original_page(&self) -> &[u8; PAGE_SIZE];
    /// Complete immutable temporary image.
    fn expected_page(&self) -> &[u8; PAGE_SIZE];
    /// Current status; `PossiblyChanged` once a write intent is recorded.
    fn status(&self) -> PmNameTrialStatus;
    /// The session expected next, or an error mid-session or when terminal.
    fn next_session(&self) -> Result<PmNameTrialSession, PmNameTrialError>;
    /// Permanently stop accepting events, keeping the current status.
    fn halt(&mut self);
    /// Validate a non-fresh event; MY1 rejects unguarded fresh events.
    fn record(&mut self, event: PmNameTrialEvent<'_>) -> Result<(), PmNameTrialError>;
    /// Immutable guard-page specification, absent for the PM1 trial.
    fn guard_page(&self) -> Option<Page>;
    /// Validate every scope-specific guard before recording a fresh session.
    fn fresh_session(
        &mut self,
        observation: FixedTextTrialObservation<'_>,
    ) -> Result<(), PmNameTrialError>;
}

use PmNameTrialSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent(PmNameTrialWrite),
    Readback(PmNameTrialWrite),
    Finalize(Session),
    Complete,
    Halted,
}

/// Event sequencing only; the fixed wrappers own every field-specific check.
#[derive(Debug)]
pub(super) struct TrialSequence {
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
    status: PmNameTrialStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intents: Vec<NonZeroU64>,
}

impl TrialSequence {
    pub(super) fn new(
        identity: &Identity,
        page: Page,
        original: [u8; PAGE_SIZE],
        expected: [u8; PAGE_SIZE],
    ) -> Self {
        Self {
            identity: identity.clone(),
            page,
            original,
            expected,
            status: PmNameTrialStatus::NotWritten,
            phase: Phase::Fresh(Session::Rename),
            sessions: Vec::with_capacity(3),
            intents: Vec::with_capacity(2),
        }
    }

    pub(super) const fn page(&self) -> Page {
        self.page
    }

    pub(super) const fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(super) const fn original_page(&self) -> &[u8; PAGE_SIZE] {
        &self.original
    }

    pub(super) const fn expected_page(&self) -> &[u8; PAGE_SIZE] {
        &self.expected
    }

    pub(super) const fn status(&self) -> PmNameTrialStatus {
        self.status
    }

    pub(super) const fn next_session(&self) -> Result<Session, PmNameTrialError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(PmNameTrialError::TerminalState),
            _ => Err(PmNameTrialError::UnexpectedEvent),
        }
    }

    pub(super) fn record(&mut self, event: PmNameTrialEvent<'_>) -> Result<(), PmNameTrialError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    pub(super) const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    fn record_inner(&mut self, event: PmNameTrialEvent<'_>) -> Result<(), PmNameTrialError> {
        match (self.phase, event) {
            (Phase::Complete | Phase::Halted, _) => Err(PmNameTrialError::TerminalState),
            (
                Phase::Fresh(session),
                PmNameTrialEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    whole_page,
                },
            ) => self.fresh_session(session, id, identity, memory_format, whole_page),
            (Phase::Intent(expected), PmNameTrialEvent::DurableWriteIntent { id, write }) => {
                if write != expected {
                    return Err(PmNameTrialError::UnexpectedEvent);
                }
                if self.intents.contains(&id) {
                    return Err(PmNameTrialError::ReusedWriteIntent);
                }
                self.intents.push(id);
                self.status = PmNameTrialStatus::PossiblyChanged;
                self.phase = Phase::Readback(write);
                Ok(())
            }
            (Phase::Readback(write), PmNameTrialEvent::ImmediateReadback { whole_page }) => {
                let session = match write {
                    PmNameTrialWrite::Rename => Session::Rename,
                    PmNameTrialWrite::Restore => Session::Restore,
                };
                self.compare_page(whole_page, session == Session::Rename)?;
                self.phase = Phase::Finalize(session);
                Ok(())
            }
            (Phase::Finalize(session), PmNameTrialEvent::SessionFinalized { id }) => {
                if self.sessions.last() != Some(&id) {
                    return Err(PmNameTrialError::SessionMismatch);
                }
                self.phase = match session {
                    Session::Rename => Phase::Fresh(Session::Restore),
                    Session::Restore => Phase::Fresh(Session::VerifyRestoration),
                    Session::VerifyRestoration => {
                        self.status = PmNameTrialStatus::RestorationVerified;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(PmNameTrialError::UnexpectedEvent),
        }
    }

    fn fresh_session(
        &mut self,
        session: Session,
        id: NonZeroU64,
        identity: &Identity,
        memory_format: u8,
        whole_page: &[u8],
    ) -> Result<(), PmNameTrialError> {
        if self.sessions.contains(&id) {
            return Err(PmNameTrialError::ReusedSession);
        }
        if identity != &self.identity {
            return Err(PmNameTrialError::IdentityMismatch);
        }
        if memory_format != 0 {
            return Err(PmNameTrialError::MemoryFormat {
                actual: memory_format,
            });
        }
        self.compare_page(whole_page, session == Session::Restore)?;
        self.sessions.push(id);
        self.phase = match session {
            Session::Rename => Phase::Intent(PmNameTrialWrite::Rename),
            Session::Restore => Phase::Intent(PmNameTrialWrite::Restore),
            Session::VerifyRestoration => Phase::Finalize(session),
        };
        Ok(())
    }

    fn compare_page(&self, bytes: &[u8], changed: bool) -> Result<(), PmNameTrialError> {
        if bytes.len() != PAGE_SIZE {
            return Err(PmNameTrialError::PageLength {
                actual: bytes.len(),
            });
        }
        let expected = if changed {
            &self.expected
        } else {
            &self.original
        };
        if bytes != expected {
            return Err(PmNameTrialError::PageMismatch);
        }
        Ok(())
    }
}
