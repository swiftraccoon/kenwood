//! Adapter from the library's text field update to the shared workflow.
//!
//! The trait and its implementation are private, so no caller can point the
//! shared workflow at another field, address or value.

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{
    ChannelNameUpdate, My1CallsignUpdate, NoGuards, Pm1NameUpdate, PmOffGatewayFinal,
    PmOffGatewayGuards, TextField, TextFieldUpdate, TextFieldUpdateEvent, TextValue,
};
use kenwood_tmd750::types::{PAGE_SIZE, PhysicalChannel};
use kenwood_tmd750::{Identity, Page, Radio, SessionGuards, TextFieldUpdateSessionReport};
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

impl From<TextField> for UpdateKind {
    fn from(field: TextField) -> Self {
        match field {
            TextField::Pm1Name => Self::Pm1Name,
            TextField::PmOffMy1Callsign => Self::PmOffMy1,
            TextField::ChannelName(_) => Self::ChannelName,
        }
    }
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
                "MY1 in PM Off with Gateway Off only; verification targets MCP exit/re-entry, not a power cycle or Terminal acceptance; the endpoint and CAT tuple name the model and firmware, not the physical unit"
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
                "MY1 in PM Off with Gateway Off; TM-D750 firmware 1.02 and type K,2,1"
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

/// The post-exit facts a guard policy requires before a session is finalized.
///
/// [`NoGuards`] requires the fresh identity read to have completed;
/// [`PmOffGatewayGuards`] requires the fresh identity and Gateway Off reads.
pub(super) trait FinalGuards: SessionGuards {
    fn final_facts(kind: UpdateKind, evidence: &PostExit) -> AppResult<Self::Final<'_>>;
}

impl FinalGuards for NoGuards {
    fn final_facts(kind: UpdateKind, evidence: &PostExit) -> AppResult<Self::Final<'_>> {
        if evidence.succeeded() {
            Ok(())
        } else {
            Err(format!(
                "{} finalization requires a complete fresh identity read",
                kind.label()
            )
            .into())
        }
    }
}

impl FinalGuards for PmOffGatewayGuards {
    fn final_facts(kind: UpdateKind, evidence: &PostExit) -> AppResult<Self::Final<'_>> {
        let (identity, gateway_mode) = evidence.gateway_off_evidence().ok_or_else(|| {
            format!(
                "{} finalization requires a complete fresh Gateway Off read",
                kind.label()
            )
        })?;
        Ok(PmOffGatewayFinal {
            identity,
            gateway_mode,
        })
    }
}

/// The write engine the shared workflow drives.
///
/// Exposes the target page, its original and desired bytes, an optional control
/// page, the current and desired text, the status and the halt switch.
/// Implemented only by the library's text field update; no implementation
/// builds a write frame or takes an address from a caller.
pub(super) trait Update: Send + Sync {
    fn kind(&self) -> UpdateKind;
    fn field(&self) -> &'static str;
    fn identity(&self) -> &Identity;
    fn page(&self) -> Page;
    /// The physical channel whose name changes; `None` for the other fields.
    fn channel(&self) -> Option<PhysicalChannel>;
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
    ) -> impl Future<Output = TextFieldUpdateSessionReport> + Send;
}

impl<V: TextValue, G: FinalGuards> Update for TextFieldUpdate<V, G> {
    fn kind(&self) -> UpdateKind {
        Self::field(self).into()
    }

    fn field(&self) -> &'static str {
        Self::field(self).name()
    }

    fn identity(&self) -> &Identity {
        self.identity()
    }

    fn page(&self) -> Page {
        self.page()
    }

    fn channel(&self) -> Option<PhysicalChannel> {
        match Self::field(self) {
            TextField::ChannelName(channel) => Some(channel),
            TextField::Pm1Name | TextField::PmOffMy1Callsign => None,
        }
    }

    fn original_page(&self) -> &[u8; PAGE_SIZE] {
        self.original_page()
    }

    fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        self.desired_page()
    }

    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])> {
        G::control_page(self)
    }

    fn current_text(&self) -> &str {
        self.current().map_or("", V::as_str)
    }

    fn desired_text(&self) -> &str {
        self.requested().map_or("", V::as_str)
    }

    fn status(&self) -> UpdateStatus {
        self.status().into()
    }

    fn halt(&mut self) {
        self.halt();
    }

    fn finalize_session(&mut self, id: NonZeroU64, evidence: &PostExit) -> AppResult<()> {
        let guards = G::final_facts(self.kind(), evidence)?;
        Ok(self.record(TextFieldUpdateEvent::SessionFinalized { id, guards })?)
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut UpdateJournal,
        capture: &mut CaptureSynchronization,
    ) -> TextFieldUpdateSessionReport {
        radio
            .set_text_field_session_until_exit(
                self,
                || cancelled.load(Ordering::Relaxed),
                |update| {
                    capture.synchronize()?;
                    journal.intent(update)
                },
            )
            .await
    }
}
