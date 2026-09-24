//! Adapters for the write engines: PM1 name, PM-Off MY1 and channel name.
//!
//! The trait and its implementations are private, so no caller can point the
//! shared workflow at another field, address or value.

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{
    ChannelNameText, ChannelNameUpdate, ChannelNameUpdateEvent, ChannelNameUpdateStatus,
    My1CallsignUpdate, My1CallsignUpdateEvent, My1CallsignUpdateStatus, Pm1NameUpdate,
    Pm1NameUpdateEvent,
};
use kenwood_tmd750::types::{PAGE_SIZE, PhysicalChannel};
use kenwood_tmd750::{
    ChannelNameUpdateSessionReport, Identity, McpProbeExit, My1CallsignUpdateSessionReport, Page,
    Pm1NameUpdateSessionReport, Radio,
};
use kenwood_transport::Transport;
use serde::Serialize;

use super::UpdateStatus;
use super::journal::UpdateJournal;
use super::workflow::{CaptureSynchronization, PostExit};
use crate::AppResult;

/// A validated update, holding the concrete engine for its field.
#[derive(Debug)]
pub(super) enum PreparedUpdate {
    Pm1(Box<Pm1NameUpdate>),
    My1(Box<My1CallsignUpdate>),
    ChannelName(Box<ChannelNameUpdate>),
}

/// Which of the writable text fields an update targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UpdateKind {
    /// PM1's display name.
    Pm1Name,
    /// The MY1 callsign in PM Off.
    PmOffMy1,
    /// One memory channel's sixteen-byte name.
    ChannelName,
}

impl UpdateKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Pm1Name => "PM1",
            Self::PmOffMy1 => "PM Off MY1",
            Self::ChannelName => "Channel name",
        }
    }

    pub(super) const fn format_version(self) -> u8 {
        match self {
            Self::Pm1Name => 5,
            Self::PmOffMy1 => 6,
            Self::ChannelName => 7,
        }
    }

    pub(super) const fn operation(self) -> &'static str {
        match self {
            Self::Pm1Name => "pm1_name_update",
            Self::PmOffMy1 => "my1_callsign_update",
            Self::ChannelName => "channel_name_update",
        }
    }

    pub(super) const fn scope(self) -> &'static str {
        match self {
            Self::Pm1Name => {
                "global PM1 only; verification targets MCP exit/re-entry, not a power cycle; the endpoint and CAT tuple name the model and firmware, not the physical unit"
            }
            Self::PmOffMy1 => {
                "MY1 in PM Off with Gateway Off only; configurable leave-in-place workflow not run on hardware; verification targets MCP exit/re-entry, not a power cycle or Terminal acceptance"
            }
            Self::ChannelName => {
                "one channel's sixteen name bytes on its complete name-table page; verification targets MCP exit/re-entry, not a power cycle or display rendering; the endpoint and CAT tuple name the model and firmware, not the physical unit"
            }
        }
    }

    pub(super) const fn qualification(self) -> &'static str {
        match self {
            Self::Pm1Name => "PM1 name only; TM-D750 firmware 1.02 and type K,2,1",
            Self::PmOffMy1 => {
                "MY1 in PM Off with Gateway Off; TM-D750 firmware 1.02 and type K,2,1; configurable update is mock-tested, not run on hardware"
            }
            Self::ChannelName => {
                "one channel name on its name-table page; TM-D750 firmware 1.02 and type K,2,1"
            }
        }
    }

    pub(super) fn verification_succeeded(self, evidence: &PostExit) -> bool {
        match self {
            Self::Pm1Name | Self::ChannelName => evidence.succeeded(),
            Self::PmOffMy1 => evidence.gateway_off_evidence().is_some(),
        }
    }
}

/// One session's library report, kept in its typed form until serialization.
#[derive(Debug)]
pub(super) enum SessionReport {
    Pm1(Pm1NameUpdateSessionReport),
    My1(My1CallsignUpdateSessionReport),
    ChannelName(ChannelNameUpdateSessionReport),
}

impl SessionReport {
    pub(super) const fn identity(&self) -> Option<&Identity> {
        match self {
            Self::Pm1(report) => report.identity.as_ref(),
            Self::My1(report) => report.identity.as_ref(),
            Self::ChannelName(report) => report.identity.as_ref(),
        }
    }

    pub(super) const fn exit(&self) -> McpProbeExit {
        match self {
            Self::Pm1(report) => report.exit,
            Self::My1(report) => report.exit,
            Self::ChannelName(report) => report.exit,
        }
    }
}

/// The per-field write engine the shared workflow drives.
///
/// Exposes the target page, its original and desired bytes, an optional control
/// page, the current and desired text, the status and the halt switch.
/// Implemented only by the PM1-name, MY1 and channel-name engines; no
/// implementation builds a write frame or takes an address from a caller.
pub(super) trait Update: Send + Sync {
    fn kind(&self) -> UpdateKind;
    fn field(&self) -> &'static str;
    fn identity(&self) -> &Identity;
    fn page(&self) -> Page;
    /// The physical channel whose name changes; `None` for the other fields.
    fn channel(&self) -> Option<PhysicalChannel> {
        None
    }
    fn original_page(&self) -> &[u8; PAGE_SIZE];
    fn desired_page(&self) -> &[u8; PAGE_SIZE];
    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])>;
    fn current_text(&self) -> &str;
    fn desired_text(&self) -> &str;
    fn status(&self) -> UpdateStatus;
    fn halt(&mut self);
    fn finalize_session(&mut self, id: NonZeroU64, evidence: &PostExit) -> AppResult<()>;
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

    fn finalize_session(&mut self, id: NonZeroU64, evidence: &PostExit) -> AppResult<()> {
        if !evidence.succeeded() {
            return Err("PM1 finalization requires a complete fresh identity read".into());
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

    fn finalize_session(&mut self, id: NonZeroU64, evidence: &PostExit) -> AppResult<()> {
        let (identity, gateway_mode) = evidence
            .gateway_off_evidence()
            .ok_or("MY1 finalization requires a complete fresh Gateway Off read")?;
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

impl Update for ChannelNameUpdate {
    fn kind(&self) -> UpdateKind {
        UpdateKind::ChannelName
    }

    fn field(&self) -> &'static str {
        "memory.ChannelName"
    }

    fn identity(&self) -> &Identity {
        self.identity()
    }

    fn page(&self) -> Page {
        self.page()
    }

    fn channel(&self) -> Option<PhysicalChannel> {
        Some(self.channel())
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
        self.current_name().map_or("", ChannelNameText::as_str)
    }

    fn desired_text(&self) -> &str {
        self.desired_name().map_or("", ChannelNameText::as_str)
    }

    fn status(&self) -> UpdateStatus {
        self.status().into()
    }

    fn halt(&mut self) {
        self.halt();
    }

    fn finalize_session(&mut self, id: NonZeroU64, evidence: &PostExit) -> AppResult<()> {
        if !evidence.succeeded() {
            return Err("channel name finalization requires a complete fresh identity read".into());
        }
        Ok(self.record(ChannelNameUpdateEvent::SessionFinalized { id })?)
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut UpdateJournal,
        capture: &mut CaptureSynchronization,
    ) -> SessionReport {
        SessionReport::ChannelName(
            radio
                .set_channel_name_session_until_exit(
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

impl From<ChannelNameUpdateStatus> for UpdateStatus {
    fn from(status: ChannelNameUpdateStatus) -> Self {
        match status {
            ChannelNameUpdateStatus::NotWritten => Self::NotWritten,
            ChannelNameUpdateStatus::PossiblyChanged => Self::PossiblyChanged,
            ChannelNameUpdateStatus::VerifiedAcrossSessions => Self::VerifiedAcrossSessions,
        }
    }
}
