//! Closed-scope MY1 updates with fresh Gateway and whole-page guards.
//!
//! An update writes the callsign in one session and re-reads the page in a
//! separate verification session, so it verifies persistence across MCP exit
//! and re-entry, not across a power cycle.

use std::num::NonZeroU64;

use super::pm1_page::FixedTextTarget;
use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::{
    My1CallsignUpdate, My1CallsignUpdateError, My1CallsignUpdateEvent, My1CallsignUpdateSession,
    My1CallsignUpdateStatus,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, DvGatewayMode, Page, RadioModel};
use kenwood_transport::Transport;

/// The first failed requirement in one bounded MY1 update session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum My1CallsignUpdateSessionStage {
    /// Select the next permitted session before sending any command.
    Preparation,
    /// Obtain and compare fresh, complete CAT identity.
    Identity,
    /// Require a new typed Gateway-Off observation before MCP entry.
    GatewayGuard,
    /// Enter MCP and require its exact response.
    Entry,
    /// Obtain one complete acknowledged format fragment or page.
    Read {
        /// Exact requested fragment or page.
        page: Page,
    },
    /// Compare memory format, Gateway, control page, and target page.
    FreshComparison,
    /// Durably record the immutable scope and sole write intent before dispatch.
    DurableIntent,
    /// Send the complete desired page in one frame and require its ACK.
    Write,
    /// Read and compare the entire desired page immediately after the write.
    ImmediateReadback,
    /// Require E/ACK without any later command on this handle.
    Exit,
}

/// Typed cause retained alongside the session's exact failed requirement.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum My1CallsignUpdateSessionError {
    /// CAT, MCP, transport, or exchange-timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// Immutable scope or session order did not match.
    #[error(transparent)]
    Evidence(#[from] My1CallsignUpdateError),
    /// The `before_write` callback failed to record recovery bytes and intent.
    #[error("MY1 update journal record failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// How far the sole W frame reached on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum My1CallsignUpdateWriteDisposition {
    /// No W dispatch was attempted in this session.
    NotAttempted,
    /// Dispatch started; an error leaves it unknown whether the radio received
    /// the frame.
    PossiblyDispatched,
    /// W was acknowledged; immediate and independent readback remain distinct.
    Acknowledged,
}

/// Outcome of one session, not of the whole two-session update.
#[derive(Debug)]
pub enum My1CallsignUpdateSessionOutcome {
    /// Required exchanges and E/ACK completed. The caller closes and drops the
    /// original transport, verifies a matching CAT identity and Gateway Off on a
    /// fresh connection, closes it, and durably records the report before
    /// finalizing this session.
    AwaitingCatVerification,
    /// Cancellation was honored before the sole intent. Any MCP session that was
    /// entered exited successfully; no write was permitted.
    Cancelled,
    /// The first failure, retaining every earlier completed exchange.
    Failed {
        /// First rejected requirement.
        stage: My1CallsignUpdateSessionStage,
        /// Original typed failure.
        error: My1CallsignUpdateSessionError,
    },
}

/// The wire exchanges of one narrowly scoped MY1 apply or verification session.
///
/// Keep the raw transport capture alongside it: this report records only the
/// exchanges that completed in this session.
#[derive(Debug)]
pub struct My1CallsignUpdateSessionReport {
    /// Phase selected by the update; absent when preparation failed.
    pub session: Option<My1CallsignUpdateSession>,
    /// Complete fresh identity, including a mismatching tuple when obtained.
    pub identity: Option<Identity>,
    /// Fresh Gateway observation, including a refused active or unknown mode.
    pub gateway_mode: Option<DvGatewayMode>,
    /// Accepted entry response without the carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Acknowledged reads in order: format, control, target, any readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of an earlier operation failure.
    pub exit: McpProbeExit,
    /// Whether dispatch of the sole W command began or was acknowledged.
    pub write: My1CallsignUpdateWriteDisposition,
    /// Update status on return; the caller performs finalization.
    pub status: My1CallsignUpdateStatus,
    /// First failure, pre-intent cancellation, or pending caller finalization.
    pub outcome: My1CallsignUpdateSessionOutcome,
    /// Secondary exit failure when the operation already had a primary failure.
    pub cleanup_error: Option<Error>,
}

impl My1CallsignUpdateSessionReport {
    const fn pending(status: My1CallsignUpdateStatus) -> Self {
        Self {
            session: None,
            identity: None,
            gateway_mode: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: My1CallsignUpdateWriteDisposition::NotAttempted,
            status,
            outcome: My1CallsignUpdateSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed session number: apply 1, independent verification 2.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = My1CallsignUpdateSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

use My1CallsignUpdateSessionOutcome as Outcome;
use My1CallsignUpdateSessionStage as Stage;

struct Failure {
    stage: Stage,
    error: My1CallsignUpdateSessionError,
}

fn failure(stage: Stage, error: impl Into<My1CallsignUpdateSessionError>) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: My1CallsignUpdateSession) -> NonZeroU64 {
    match session {
        My1CallsignUpdateSession::Apply => NonZeroU64::MIN,
        My1CallsignUpdateSession::Verify => NonZeroU64::MIN.saturating_add(1),
    }
}

/// Honor cancellation only before any accepted intent can imply a change.
fn cancelled(update: &My1CallsignUpdate, should_cancel: &mut impl FnMut() -> bool) -> bool {
    update.status() == My1CallsignUpdateStatus::NotWritten && should_cancel()
}

impl<T: Transport> Radio<T> {
    /// Apply or independently verify one requested MY1 callsign in PM Off.
    ///
    /// The immutable update admits only the complete pinned target and control
    /// pages on TM-D750 / firmware 1.02 / type `K,2,1`. Each invocation reads
    /// fresh ID/FV/TY and requires Gateway Off before entry, then obtains format,
    /// control, and target bytes. Apply requires exact original-page agreement;
    /// Verify requires exact desired-page agreement. No rebase occurs.
    ///
    /// Before the sole W, `before_write` must durably record the intent, the
    /// identity, both complete immutable pages, and the recovery bytes. Its
    /// successful return marks the update possibly changed, then one full-page
    /// frame is dispatched and the immediate readback compares every byte. This
    /// generic transport method cannot check connector identity: the caller
    /// selects the main-unit USB endpoint at 9600 baud with RTS/CTS and asserted
    /// DTR/RTS, supplies the captured baseline, and wraps the transport in
    /// complete raw capture.
    ///
    /// A comparison or journal failure at a known exchange boundary still exits;
    /// an uncertain exchange permits only close and drop, never a speculative E.
    /// After E/ACK, no CAT, baud, or close command is issued here. The caller
    /// closes and drops the transport, verifies the exact fresh CAT identity and
    /// Gateway Off, closes the fresh handle, and durably records the report
    /// before recording [`My1CallsignUpdateEvent::SessionFinalized`] or opening
    /// the next session. No arbitrary address, Gateway change, RF command,
    /// rollback, or retry is exposed, and the generic schema gate is unchanged.
    ///
    /// # Cancellation
    ///
    /// Await the future to completion; never drop it to cancel live I/O.
    /// Cancellation is checked at complete exchange boundaries before intent.
    /// After intent, finish known-safe verification despite cancellation, but
    /// halt on a failed scope, framing, or comparison check. A failure keeps the
    /// possibly-changed status and never restores a stale page automatically.
    pub async fn set_my1_callsign_session_until_exit(
        &mut self,
        update: &mut My1CallsignUpdate,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&My1CallsignUpdate) -> Result<(), std::io::Error>,
    ) -> My1CallsignUpdateSessionReport {
        let mut report = My1CallsignUpdateSessionReport::pending(update.status());
        if let Err(error) = self
            .run_my1_update(update, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            update.halt();
            report.fail(error);
        }
        if matches!(report.outcome, Outcome::Cancelled) {
            update.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_my1_update(&mut report).await;
        }
        if matches!(report.outcome, Outcome::Failed { .. }) {
            update.halt();
        }
        report.status = update.status();
        report
    }

    async fn run_my1_update(
        &mut self,
        update: &mut My1CallsignUpdate,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&My1CallsignUpdate) -> Result<(), std::io::Error>,
        report: &mut My1CallsignUpdateSessionReport,
    ) -> Result<(), Failure> {
        let phase = update
            .next_session()
            .map_err(|error| failure(Stage::Preparation, error))?;
        report.session = Some(phase);
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let identity = self
            .identify()
            .await
            .map_err(|error| failure(Stage::Identity, error))?;
        report.identity = Some(identity.clone());
        if &identity != update.identity() {
            return Err(failure(
                Stage::Identity,
                My1CallsignUpdateError::IdentityMismatch,
            ));
        }
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let gateway_mode = self
            .get_dv_gateway_mode()
            .await
            .map_err(|error| failure(Stage::GatewayGuard, error))?;
        report.gateway_mode = Some(gateway_mode);
        if gateway_mode != DvGatewayMode::Off {
            return Err(failure(
                Stage::GatewayGuard,
                My1CallsignUpdateError::GatewayMode {
                    actual: gateway_mode,
                },
            ));
        }
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_my1_update(update, should_cancel, report)
            .await?;
        if matches!(report.outcome, Outcome::Cancelled) {
            return Ok(());
        }
        if phase == My1CallsignUpdateSession::Apply {
            before_write(update).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    My1CallsignUpdateSessionError::DurableIntent(error),
                )
            })?;
            update
                .record(My1CallsignUpdateEvent::DurableWriteIntent {
                    id: NonZeroU64::MIN,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_my1_update(update, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_my1_update_segment(update.page(), Stage::ImmediateReadback, report)
                .await?;
            update
                .record(My1CallsignUpdateEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_my1_update(
        &mut self,
        update: &mut My1CallsignUpdate,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut My1CallsignUpdateSessionReport,
    ) -> Result<(), Failure> {
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        let format = self
            .read_my1_update_segment(format_page, Stage::Read { page: format_page }, report)
            .await?;
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let control_spec = update.control_page_spec();
        let control = self
            .read_my1_update_segment(control_spec, Stage::Read { page: control_spec }, report)
            .await?;
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let page = update.page();
        let data = self
            .read_my1_update_segment(page, Stage::Read { page }, report)
            .await?;
        let invalid = || {
            failure(
                Stage::FreshComparison,
                My1CallsignUpdateError::UnsupportedDescriptor,
            )
        };
        update
            .record(My1CallsignUpdateEvent::FreshSession {
                id: report.session_id().ok_or_else(invalid)?,
                identity: report.identity.as_ref().ok_or_else(invalid)?,
                memory_format: format.get(2).copied().ok_or_else(invalid)?,
                gateway_mode: report.gateway_mode.ok_or_else(invalid)?,
                control_page: &control,
                whole_page: &data,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
        }
        Ok(())
    }

    async fn read_my1_update_segment(
        &mut self,
        page: Page,
        stage: Stage,
        report: &mut My1CallsignUpdateSessionReport,
    ) -> Result<Vec<u8>, Failure> {
        let mut session = self.mcp_session().map_err(|error| failure(stage, error))?;
        let data = session
            .read_page(page)
            .await
            .map_err(|error| failure(stage, error))?;
        report.segments.push(McpProbeSegment {
            page,
            data: data.clone(),
        });
        Ok(data)
    }

    async fn write_my1_update(
        &mut self,
        update: &My1CallsignUpdate,
        report: &mut My1CallsignUpdateSessionReport,
    ) -> Result<(), My1CallsignUpdateSessionError> {
        self.require_mcp_ready()?;
        if self.identity() != Some(update.identity())
            || update.identity().model != RadioModel::TmD750
            || update.identity().firmware.as_str() != "1.02"
            || update.identity().radio_type.as_str() != "K,2,1"
        {
            return Err(My1CallsignUpdateError::IdentityMismatch.into());
        }
        if report.gateway_mode != Some(DvGatewayMode::Off)
            || update.page() != My1CallsignUpdate::required_page()?
            || update.control_page_spec() != My1CallsignUpdate::required_control_page()?
            || update.status() != My1CallsignUpdateStatus::PossiblyChanged
            || report.session != Some(My1CallsignUpdateSession::Apply)
        {
            return Err(My1CallsignUpdateError::UnsupportedDescriptor.into());
        }
        if !report.segments.last().is_some_and(|segment| {
            segment.page == update.page() && segment.data == update.original_page().as_slice()
        }) {
            return Err(My1CallsignUpdateError::PageMismatch.into());
        }
        if !report.segments.iter().any(|segment| {
            segment.page == update.control_page_spec()
                && segment.data == update.control_page().as_slice()
        }) {
            return Err(My1CallsignUpdateError::ControlPageMismatch.into());
        }
        report.write = My1CallsignUpdateWriteDisposition::PossiblyDispatched;
        self.write_fixed_text_frame(
            FixedTextTarget::My1,
            update.desired_page(),
            "MY1 callsign page write",
        )
        .await?;
        report.write = My1CallsignUpdateWriteDisposition::Acknowledged;
        Ok(())
    }

    async fn finish_my1_update(&mut self, report: &mut My1CallsignUpdateSessionReport) {
        let result = match self.mcp_session() {
            Ok(session) => session.exit().await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => report.exit = McpProbeExit::Acknowledged,
            Err(error) => {
                report.exit = McpProbeExit::NotAcknowledged;
                if matches!(report.outcome, Outcome::Failed { .. }) {
                    report.cleanup_error = Some(error);
                } else {
                    report.fail(failure(Stage::Exit, error));
                }
            }
        }
    }
}
