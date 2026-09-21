//! Private trait with exactly two implementations: the PM1 name engine and the
//! PM-Off MY1 engine, so no caller can point the write engine at another field,
//! address or value.

use std::future::Future;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};

use kenwood_tmd750::memory::{MyCallsignTrial, PmNameTrial, PmNameTrialEvent, PmNameTrialStatus};
use kenwood_tmd750::types::PAGE_SIZE;
use kenwood_tmd750::{Identity, Page, PmNameTrialSessionReport, Radio};
use kenwood_transport::Transport;
use serde::Serialize;

use super::journal::Journal;
use crate::AppResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum TrialKind {
    Pm1Name,
    PmOffMy1,
}

/// The per-field write engine the shared workflow drives.
///
/// Exposes the target page, its original and expected bytes, an optional
/// control page, the restoration status and the session driver. No method
/// builds a write frame or takes an address from a caller.
pub(super) trait Trial: Send + Sync {
    fn kind(&self) -> TrialKind;
    fn label(&self) -> &'static str;
    fn field(&self) -> &'static str;
    fn temporary_text(&self) -> &'static str;
    fn identity(&self) -> &Identity;
    fn page(&self) -> Page;
    fn original_page(&self) -> &[u8; PAGE_SIZE];
    fn expected_page(&self) -> &[u8; PAGE_SIZE];
    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])>;
    fn validate_confirmation(&self, text: &str) -> AppResult<()>;
    fn status(&self) -> PmNameTrialStatus;
    fn finalize_session(&mut self, id: NonZeroU64) -> AppResult<()>;
    fn halt(&mut self);
    fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut Journal,
    ) -> impl Future<Output = PmNameTrialSessionReport> + Send;
}

impl Trial for PmNameTrial {
    fn kind(&self) -> TrialKind {
        TrialKind::Pm1Name
    }

    fn label(&self) -> &'static str {
        "PM1 name"
    }

    fn field(&self) -> &'static str {
        "pm.PmName1"
    }

    fn temporary_text(&self) -> &'static str {
        Self::TEMPORARY_NAME
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

    fn expected_page(&self) -> &[u8; PAGE_SIZE] {
        self.expected_page()
    }

    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])> {
        None
    }

    fn validate_confirmation(&self, text: &str) -> AppResult<()> {
        let _confirmed =
            Self::prepare_unqualified_offline(self.identity(), self.original_page(), text)?;
        Ok(())
    }

    fn status(&self) -> PmNameTrialStatus {
        self.status()
    }

    fn finalize_session(&mut self, id: NonZeroU64) -> AppResult<()> {
        Ok(self.record(PmNameTrialEvent::SessionFinalized { id })?)
    }

    fn halt(&mut self) {
        self.halt();
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut Journal,
    ) -> PmNameTrialSessionReport {
        radio
            .run_approved_pm1_trial_session_until_exit(
                self,
                || cancelled.load(Ordering::Relaxed),
                |trial, write| journal.intent(trial, write),
            )
            .await
    }
}

impl Trial for MyCallsignTrial {
    fn kind(&self) -> TrialKind {
        TrialKind::PmOffMy1
    }

    fn label(&self) -> &'static str {
        "PM Off MY1"
    }

    fn field(&self) -> &'static str {
        "dstar-my-callsign-1"
    }

    fn temporary_text(&self) -> &'static str {
        Self::TEMPORARY_CALLSIGN
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

    fn expected_page(&self) -> &[u8; PAGE_SIZE] {
        self.expected_page()
    }

    fn control_page(&self) -> Option<(Page, &[u8; PAGE_SIZE])> {
        Some((self.control_page_spec(), self.control_page()))
    }

    fn validate_confirmation(&self, text: &str) -> AppResult<()> {
        if !text.is_empty() {
            return Err(
                "MY1 trial requires an empty captured baseline, not display-name confirmation"
                    .into(),
            );
        }
        let _confirmed = Self::prepare_unqualified_offline(
            self.identity(),
            self.original_page(),
            self.control_page(),
        )?;
        Ok(())
    }

    fn status(&self) -> PmNameTrialStatus {
        self.status()
    }

    fn finalize_session(&mut self, id: NonZeroU64) -> AppResult<()> {
        Ok(self.finalize_session(id)?)
    }

    fn halt(&mut self) {
        self.halt();
    }

    async fn run_session<T: Transport>(
        &mut self,
        radio: &mut Radio<T>,
        cancelled: &AtomicBool,
        journal: &mut Journal,
    ) -> PmNameTrialSessionReport {
        radio
            .run_approved_my1_trial_session_until_exit(
                self,
                || cancelled.load(Ordering::Relaxed),
                |trial, write| journal.intent(trial, write),
            )
            .await
    }
}
