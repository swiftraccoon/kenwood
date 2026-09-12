//! Closed adapters for the two typed, independently guarded text updates.

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{
    My1CallsignUpdate, My1CallsignUpdateEvent, My1CallsignUpdateStatus, Pm1NameUpdate,
    Pm1NameUpdateEvent,
};
use kenwood_tmd750::transport::Transport;
use kenwood_tmd750::types::PAGE_SIZE;
use kenwood_tmd750::{
    Identity, McpProbeExit, My1CallsignUpdateSessionReport, Page, Pm1NameUpdateSessionReport, Radio,
};
use serde::Serialize;

use super::super::reconnect::PostExitVerification;
use super::UpdateStatus;
use super::journal::UpdateJournal;
use super::workflow::CaptureSynchronization;
use crate::AppResult;

/// Preparation retains the concrete engine; neither variant can widen its scope.
#[derive(Debug)]
pub(super) enum PreparedUpdate {
    Pm1(Box<Pm1NameUpdate>),
    My1(Box<My1CallsignUpdate>),
}

/// A closed policy choice, never an arbitrary field or address supplied by a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UpdateKind {
    Pm1Name,
    PmOffMy1,
}

impl UpdateKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Pm1Name => "PM1",
            Self::PmOffMy1 => "PM Off MY1",
        }
    }

    pub(super) const fn format_version(self) -> u8 {
        match self {
            Self::Pm1Name => 5,
            Self::PmOffMy1 => 6,
        }
    }

    pub(super) const fn operation(self) -> &'static str {
        match self {
            Self::Pm1Name => "pm1_name_update",
            Self::PmOffMy1 => "my1_callsign_update",
        }
    }

    pub(super) const fn scope(self) -> &'static str {
        match self {
            Self::Pm1Name => {
                "global PM1 only; verification targets MCP exit/re-entry, not a power cycle; endpoint and CAT tuple do not prove physical continuity"
            }
            Self::PmOffMy1 => {
                "MY1 in PM Off with Gateway Off only; configurable leave-in-place workflow is not hardware-qualified; verification targets MCP exit/re-entry, not a power cycle or Terminal acceptance"
            }
        }
    }

    pub(super) const fn qualification(self) -> &'static str {
        match self {
            Self::Pm1Name => "PM1 name only; TM-D750 firmware 1.02 and type K,2,1",
            Self::PmOffMy1 => {
                "MY1 in PM Off with Gateway Off; TM-D750 firmware 1.02 and type K,2,1; configurable update is mock-tested, not hardware-qualified"
            }
        }
    }

    pub(super) fn verification_succeeded(self, evidence: &PostExitVerification) -> bool {
        match self {
            Self::Pm1Name => evidence.succeeded(),
            Self::PmOffMy1 => evidence.gateway_off_evidence().is_some(),
        }
    }
}

/// Keep the original library reports and typed identity until capture conversion.
#[derive(Debug)]
pub(super) enum SessionReport {
    Pm1(Pm1NameUpdateSessionReport),
    My1(My1CallsignUpdateSessionReport),
}

impl SessionReport {
    pub(super) const fn identity(&self) -> Option<&Identity> {
        match self {
            Self::Pm1(report) => report.identity.as_ref(),
            Self::My1(report) => report.identity.as_ref(),
        }
    }

    pub(super) const fn exit(&self) -> McpProbeExit {
        match self {
            Self::Pm1(report) => report.exit,
            Self::My1(report) => report.exit,
        }
    }
}

/// Only the two typed library engines implement this private orchestration seam.
/// No implementation constructs a write frame or admits an arbitrary field.
pub(super) trait Update: Send + Sync {
    fn kind(&self) -> UpdateKind;
    fn field(&self) -> &'static str;
    fn identity(&self) -> &Identity;
    fn page(&self) -> Page;
    fn original_page(&self) -> &[u8; PAGE_SIZE];
    fn desired_page(&self) -> &[u8; PAGE_SIZE];
    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])>;
    fn current_text(&self) -> &str;
    fn desired_text(&self) -> &str;
    fn status(&self) -> UpdateStatus;
    fn halt(&mut self);
    fn finalize_session(
        &mut self,
        id: NonZeroU64,
        evidence: &PostExitVerification,
    ) -> AppResult<()>;
    fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut UpdateJournal,
        capture: &mut CaptureSynchronization,
    ) -> impl Future<Output = SessionReport> + Send;
}

impl Update for Pm1NameUpdate {
    fn kind(&self) -> UpdateKind {
        UpdateKind::Pm1Name
    }

    fn field(&self) -> &'static str {
        "pm.PmName1"
    }

    fn identity(&self) -> &Identity {
        self.identity()
    }

    fn page(&self) -> Page {
        self.page()
    }

    fn original_page(&self) -> &[u8; PAGE_SIZE] {
        self.original_page()
    }

    fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        self.desired_page()
    }

    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])> {
        None
    }

    fn current_text(&self) -> &str {
        self.current_name().as_str()
    }

    fn desired_text(&self) -> &str {
        self.desired_name().as_str()
    }

    fn status(&self) -> UpdateStatus {
        self.status().into()
    }

    fn halt(&mut self) {
        self.halt();
    }

    fn finalize_session(
        &mut self,
        id: NonZeroU64,
        evidence: &PostExitVerification,
    ) -> AppResult<()> {
        if !evidence.succeeded() {
            return Err("PM1 finalization requires complete fresh identity evidence".into());
        }
        Ok(self.record(Pm1NameUpdateEvent::SessionFinalized { id })?)
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut UpdateJournal,
        capture: &mut CaptureSynchronization,
    ) -> SessionReport {
        SessionReport::Pm1(
            radio
                .set_pm1_name_session_until_exit(
                    self,
                    || cancelled.load(Ordering::Relaxed),
                    |update| {
                        capture.synchronize()?;
                        journal.intent(update)
                    },
                )
                .await,
        )
    }
}

impl Update for My1CallsignUpdate {
    fn kind(&self) -> UpdateKind {
        UpdateKind::PmOffMy1
    }

    fn field(&self) -> &'static str {
        "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway"
    }

    fn identity(&self) -> &Identity {
        self.identity()
    }

    fn page(&self) -> Page {
        self.page()
    }

    fn original_page(&self) -> &[u8; PAGE_SIZE] {
        self.original_page()
    }

    fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        self.desired_page()
    }

    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])> {
        Some((self.control_page_spec(), self.control_page()))
    }

    fn current_text(&self) -> &str {
        self.current_callsign().map_or("", |value| value.as_str())
    }

    fn desired_text(&self) -> &str {
        self.desired_callsign().as_str()
    }

    fn status(&self) -> UpdateStatus {
        self.status().into()
    }

    fn halt(&mut self) {
        self.halt();
    }

    fn finalize_session(
        &mut self,
        id: NonZeroU64,
        evidence: &PostExitVerification,
    ) -> AppResult<()> {
        let (identity, gateway_mode) = evidence
            .gateway_off_evidence()
            .ok_or("MY1 finalization requires complete fresh Gateway-Off evidence")?;
        Ok(self.record(My1CallsignUpdateEvent::SessionFinalized {
            id,
            identity,
            gateway_mode,
        })?)
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut UpdateJournal,
        capture: &mut CaptureSynchronization,
    ) -> SessionReport {
        SessionReport::My1(
            radio
                .set_my1_callsign_session_until_exit(
                    self,
                    || cancelled.load(Ordering::Relaxed),
                    |update| {
                        capture.synchronize()?;
                        journal.intent(update)
                    },
                )
                .await,
        )
    }
}

impl From<My1CallsignUpdateStatus> for UpdateStatus {
    fn from(status: My1CallsignUpdateStatus) -> Self {
        match status {
            My1CallsignUpdateStatus::NotWritten => Self::NotWritten,
            My1CallsignUpdateStatus::PossiblyChanged => Self::PossiblyChanged,
            My1CallsignUpdateStatus::VerifiedAcrossSessions => Self::VerifiedAcrossSessions,
        }
    }
}
