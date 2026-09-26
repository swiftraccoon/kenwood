//! Narrowly scoped text field updates over one complete page.
//!
//! An update writes the new text in one session and re-reads the page in a
//! separate read-only session, so it verifies persistence across MCP exit and
//! re-entry, not across a power cycle. The field, its value type and its
//! guard policy come from the [`TextFieldUpdate`] the caller prepared.

use std::num::NonZeroU64;

use super::pm1_page::FixedTextTarget;
use super::{Identity, Radio};
use crate::error::Error;
use crate::memory::{
    GuardKind, GuardPolicy, NoGuards, PmOffGatewayFresh, PmOffGatewayGuards, TextField,
    TextFieldUpdate, TextFieldUpdateError, TextFieldUpdateEvent, TextFieldUpdateSession,
    TextFieldUpdateStatus, TextValue,
};
use crate::radio::qualification::{McpProbeExit, McpProbeSegment};
use crate::types::{Address, DvGatewayMode, PAGE_SIZE, Page, RadioModel};
use kenwood_transport::Transport;

/// The step at which a bounded text field update session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFieldUpdateSessionStage {
    /// Obtain the update-selected session before any I/O.
    Preparation,
    /// Obtain and compare a complete, new CAT identity before MCP entry.
    Identity,
    /// Require a new typed Gateway-Off observation before MCP entry
    /// ([`GuardKind::PmOffGateway`] only).
    GatewayGuard,
    /// Enter MCP and require the exact reply.
    Entry,
    /// Obtain one complete, acknowledged format fragment, control page or
    /// target page.
    Read {
        /// Exact fragment or page requested.
        page: Page,
    },
    /// Compare the format byte, the guards and the complete page with the
    /// required before-image.
    FreshComparison,
    /// Durably record the caller's intent before the only W command.
    DurableIntent,
    /// Dispatch the single complete page and require its acknowledgment.
    Write,
    /// Obtain and compare every byte of the immediate post-write page.
    ImmediateReadback,
    /// Acknowledge MCP exit without subsequently reusing the original handle.
    Exit,
}

/// The original typed cause of a text field update session failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TextFieldUpdateSessionError {
    /// CAT, MCP, transport, or individual exchange timeout failure.
    #[error(transparent)]
    Io(#[from] Error),
    /// The update's narrow scope or session sequence rejected the operation.
    #[error(transparent)]
    Evidence(#[from] TextFieldUpdateError),
    /// The `before_write` callback failed to record the intent before dispatch.
    #[error("text field update journal record failed: {0}")]
    DurableIntent(#[source] std::io::Error),
}

/// How far the sole W frame reached on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFieldUpdateWriteDisposition {
    /// No W dispatch was attempted during this session.
    NotAttempted,
    /// Dispatch began; an error or timeout leaves it unknown whether the radio
    /// received the frame.
    PossiblyDispatched,
    /// The radio acknowledged W; whole-page readback and external finalization
    /// remain separate requirements.
    Acknowledged,
}

/// Completion, safe pre-write cancellation, or failure of one update session.
#[derive(Debug)]
pub enum TextFieldUpdateSessionOutcome {
    /// Required reads, any write/readback, and E/ACK completed.
    ///
    /// The caller closes and drops the transport, verifies a fresh CAT
    /// identity (and Gateway Off for [`GuardKind::PmOffGateway`]), closes that
    /// connection, and durably records the report before `SessionFinalized`.
    AwaitingCatVerification,
    /// Cancellation was honored before accepting the only write intent.
    ///
    /// If MCP was entered, that session exited successfully. Cancellation after
    /// a possible write is never reported here.
    Cancelled,
    /// The first failed requirement, retaining every earlier completed exchange.
    Failed {
        /// Exact failed step.
        stage: TextFieldUpdateSessionStage,
        /// Original typed cause.
        error: TextFieldUpdateSessionError,
    },
}

/// The wire exchanges of one bounded text field update session.
///
/// Preserve the raw transcript alongside these complete read segments: failed
/// exchanges and any secondary exit error appear only there.
#[derive(Debug)]
pub struct TextFieldUpdateSessionReport {
    /// The field the update changes.
    pub field: TextField,
    /// Session selected by the update, absent when preparation failed.
    pub session: Option<TextFieldUpdateSession>,
    /// New full CAT identity, including a mismatching tuple if obtained.
    pub identity: Option<Identity>,
    /// Fresh Gateway observation ([`GuardKind::PmOffGateway`] only), including
    /// a refused active or unknown mode.
    pub gateway_mode: Option<DvGatewayMode>,
    /// Accepted programming-entry reply, excluding its carriage return.
    pub entry_reply: Option<Vec<u8>>,
    /// Complete acknowledged reads in order: format, any control page, target,
    /// then any immediate readback.
    pub segments: Vec<McpProbeSegment>,
    /// Exit disposition, independent of the original session failure.
    pub exit: McpProbeExit,
    /// Whether this session attempted or acknowledged the sole W command.
    pub write: TextFieldUpdateWriteDisposition,
    /// Update status on return; the caller performs finalization.
    pub status: TextFieldUpdateStatus,
    /// First failure, safe cancellation, or outstanding fresh CAT verification.
    pub outcome: TextFieldUpdateSessionOutcome,
    /// Additional exit error when another failure was already recorded.
    pub cleanup_error: Option<Error>,
}

impl TextFieldUpdateSessionReport {
    const fn pending(field: TextField, status: TextFieldUpdateStatus) -> Self {
        Self {
            field,
            session: None,
            identity: None,
            gateway_mode: None,
            entry_reply: None,
            segments: Vec::new(),
            exit: McpProbeExit::NotEntered,
            write: TextFieldUpdateWriteDisposition::NotAttempted,
            status,
            outcome: TextFieldUpdateSessionOutcome::AwaitingCatVerification,
            cleanup_error: None,
        }
    }

    /// Fixed session number: apply 1, separate-session verification 2.
    #[must_use]
    pub const fn session_id(&self) -> Option<NonZeroU64> {
        match self.session {
            Some(session) => Some(session_id(session)),
            None => None,
        }
    }

    fn fail(&mut self, failure: Failure) {
        self.outcome = TextFieldUpdateSessionOutcome::Failed {
            stage: failure.stage,
            error: failure.error,
        };
    }
}

use TextFieldUpdateSessionOutcome as Outcome;
use TextFieldUpdateSessionStage as Stage;

struct Failure {
    stage: Stage,
    error: TextFieldUpdateSessionError,
}

fn failure(stage: Stage, error: impl Into<TextFieldUpdateSessionError>) -> Failure {
    Failure {
        stage,
        error: error.into(),
    }
}

const fn session_id(session: TextFieldUpdateSession) -> NonZeroU64 {
    match session {
        TextFieldUpdateSession::Apply => NonZeroU64::MIN,
        TextFieldUpdateSession::Verify => NonZeroU64::MIN.saturating_add(1),
    }
}

/// Honor cancellation only before any accepted intent can imply a change.
fn cancelled<V: TextValue, G: GuardPolicy>(
    update: &TextFieldUpdate<V, G>,
    should_cancel: &mut impl FnMut() -> bool,
) -> bool {
    update.status() == TextFieldUpdateStatus::NotWritten && should_cancel()
}

/// The private frame writer's target for a prepared update.
fn frame_target(field: TextField, page: Page) -> Result<FixedTextTarget, TextFieldUpdateError> {
    match field {
        TextField::Pm1Name => Ok(FixedTextTarget::Pm1),
        TextField::PmOffMy1Callsign => Ok(FixedTextTarget::My1),
        TextField::ChannelName(_) => {
            FixedTextTarget::channel_names(page).ok_or(TextFieldUpdateError::UnsupportedPage)
        }
    }
}

/// The complete page a prepared update must target, resolved again from the
/// field so a hand-built update cannot redirect the frame.
fn required_page(field: TextField) -> Result<Page, TextFieldUpdateError> {
    match field {
        TextField::Pm1Name => TextFieldUpdate::<crate::memory::Pm1Name, NoGuards>::required_page(),
        TextField::PmOffMy1Callsign => {
            TextFieldUpdate::<crate::memory::My1Callsign, PmOffGatewayGuards>::required_page()
        }
        TextField::ChannelName(channel) => {
            TextFieldUpdate::<crate::memory::ChannelNameText, NoGuards>::required_page(channel)
        }
    }
}

/// The extra exchanges the driver performs for a guard policy.
///
/// Implemented for [`NoGuards`] and [`PmOffGatewayGuards`]; the sealed
/// [`GuardPolicy`] supertrait admits no other implementation.
pub trait SessionGuards: GuardPolicy + Sized {
    /// The control page to read inside programming mode, with the stored
    /// bytes it must equal; `None` when the policy reads none.
    fn control_page<V: TextValue>(
        update: &TextFieldUpdate<V, Self>,
    ) -> Option<(Page, &[u8; PAGE_SIZE])>;

    /// The policy's fresh facts, built from the session's completed reads.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::UnsupportedDescriptor`] when a required
    /// read is missing from the report.
    fn fresh<'a>(
        report: &TextFieldUpdateSessionReport,
        control: Option<&'a [u8]>,
    ) -> Result<Self::Fresh<'a>, TextFieldUpdateError>;
}

impl SessionGuards for NoGuards {
    fn control_page<V: TextValue>(
        _update: &TextFieldUpdate<V, Self>,
    ) -> Option<(Page, &[u8; PAGE_SIZE])> {
        None
    }

    fn fresh<'a>(
        _report: &TextFieldUpdateSessionReport,
        _control: Option<&'a [u8]>,
    ) -> Result<Self::Fresh<'a>, TextFieldUpdateError> {
        Ok(())
    }
}

impl SessionGuards for PmOffGatewayGuards {
    fn control_page<V: TextValue>(
        update: &TextFieldUpdate<V, Self>,
    ) -> Option<(Page, &[u8; PAGE_SIZE])> {
        Some((update.guards().spec(), update.guards().bytes()))
    }

    fn fresh<'a>(
        report: &TextFieldUpdateSessionReport,
        control: Option<&'a [u8]>,
    ) -> Result<Self::Fresh<'a>, TextFieldUpdateError> {
        Ok(PmOffGatewayFresh {
            gateway_mode: report
                .gateway_mode
                .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?,
            control_page: control.ok_or(TextFieldUpdateError::UnsupportedDescriptor)?,
        })
    }
}

impl<T: Transport> Radio<T> {
    /// Run one session of a typed, narrowly scoped text field update.
    ///
    /// The immutable update chooses apply or read-only verification. It exposes
    /// no caller-selected address, alternate field, force-write option, or
    /// automatic rollback, and leaves the generic schema-write gate unchanged.
    /// Caller preconditions: a fully captured baseline for the target page (and
    /// the control page for [`GuardKind::PmOffGateway`]) and a transport
    /// wrapped in fail-closed raw capture. This generic transport API cannot
    /// check the USB connector identity, so the caller selects the endpoint.
    ///
    /// Each invocation obtains a fresh complete identity before MCP entry; a
    /// [`GuardKind::PmOffGateway`] update additionally requires a fresh Gateway
    /// Off observation before entry and reads the complete control page after
    /// the format byte. Every session reads format byte 10 and the complete
    /// target page and requires exact equality with the appropriate immutable
    /// page. No merge or rebase occurs. In the apply session, `before_write`
    /// must durably record the identity, both pages, and exact intent. Its
    /// successful return permits recording the possible-write status and
    /// dispatching one W frame. The immediate readback compares all 256 bytes.
    /// Verification is a separate read-only session; no RF command is sent in
    /// either session.
    ///
    /// A comparison or journal error at a known exchange boundary still exits;
    /// an uncertain exchange sends no E. After E/ACK, this method performs no
    /// CAT, baud, or close operation on the retired handle. The caller closes
    /// and drops it, verifies a matching identity on a fresh connection (and
    /// Gateway Off for [`GuardKind::PmOffGateway`]), closes that connection,
    /// and durably records the report before recording
    /// [`TextFieldUpdateEvent::SessionFinalized`]. Only then may the caller
    /// open the next session.
    ///
    /// # Cancellation
    ///
    /// Await this future to completion; never drop it to cancel live I/O.
    /// Cooperative cancellation is honored only at complete exchange boundaries
    /// before the write intent. After intent, finish the safe current phase and
    /// subsequent verification. On failure the update halts, keeps the
    /// possibly-changed status, and sends no rollback or retry.
    pub async fn set_text_field_session_until_exit<V: TextValue, G: SessionGuards>(
        &mut self,
        update: &mut TextFieldUpdate<V, G>,
        mut should_cancel: impl FnMut() -> bool,
        mut before_write: impl FnMut(&TextFieldUpdate<V, G>) -> Result<(), std::io::Error>,
    ) -> TextFieldUpdateSessionReport {
        let mut report = TextFieldUpdateSessionReport::pending(update.field(), update.status());
        if let Err(error) = self
            .run_text_field_session(update, &mut should_cancel, &mut before_write, &mut report)
            .await
        {
            update.halt();
            report.fail(error);
        }
        if matches!(report.outcome, Outcome::Cancelled) {
            update.halt();
        }
        if report.entry_reply.is_some() && self.mcp_ready() {
            self.finish_text_field_session(&mut report).await;
        }
        if matches!(report.outcome, Outcome::Failed { .. }) {
            update.halt();
        }
        report.status = update.status();
        report
    }

    async fn run_text_field_session<V: TextValue, G: SessionGuards>(
        &mut self,
        update: &mut TextFieldUpdate<V, G>,
        should_cancel: &mut impl FnMut() -> bool,
        before_write: &mut impl FnMut(&TextFieldUpdate<V, G>) -> Result<(), std::io::Error>,
        report: &mut TextFieldUpdateSessionReport,
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
                TextFieldUpdateError::IdentityMismatch,
            ));
        }
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        if G::KIND == GuardKind::PmOffGateway {
            let gateway_mode = self
                .get_dv_gateway_mode()
                .await
                .map_err(|error| failure(Stage::GatewayGuard, error))?;
            report.gateway_mode = Some(gateway_mode);
            if gateway_mode != DvGatewayMode::Off {
                return Err(failure(
                    Stage::GatewayGuard,
                    TextFieldUpdateError::GatewayMode {
                        actual: gateway_mode,
                    },
                ));
            }
            if cancelled(update, should_cancel) {
                report.outcome = Outcome::Cancelled;
                return Ok(());
            }
        }
        report.exit = McpProbeExit::RecoveryRequired;
        let session = self
            .enter_mcp()
            .await
            .map_err(|error| failure(Stage::Entry, error))?;
        report.entry_reply = Some(session.entry_reply().to_vec());
        self.compare_text_field_session(update, phase, &identity, should_cancel, report)
            .await?;
        if matches!(report.outcome, Outcome::Cancelled) {
            return Ok(());
        }
        if phase == TextFieldUpdateSession::Apply {
            before_write(update).map_err(|error| {
                failure(
                    Stage::DurableIntent,
                    TextFieldUpdateSessionError::DurableIntent(error),
                )
            })?;
            update
                .record(TextFieldUpdateEvent::DurableWriteIntent {
                    id: NonZeroU64::MIN,
                })
                .map_err(|error| failure(Stage::DurableIntent, error))?;
            self.write_text_field(update, report)
                .await
                .map_err(|error| failure(Stage::Write, error))?;
            let data = self
                .read_text_field_segment(update.page(), Stage::ImmediateReadback, report)
                .await?;
            update
                .record(TextFieldUpdateEvent::ImmediateReadback { whole_page: &data })
                .map_err(|error| failure(Stage::ImmediateReadback, error))?;
        }
        Ok(())
    }

    async fn compare_text_field_session<V: TextValue, G: SessionGuards>(
        &mut self,
        update: &mut TextFieldUpdate<V, G>,
        phase: TextFieldUpdateSession,
        identity: &Identity,
        should_cancel: &mut impl FnMut() -> bool,
        report: &mut TextFieldUpdateSessionReport,
    ) -> Result<(), Failure> {
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let format_page = Address::new(8)
            .and_then(|address| Page::new(address, 40))
            .map_err(|error| failure(Stage::Preparation, Error::from(error)))?;
        let format = self
            .read_text_field_segment(format_page, Stage::Read { page: format_page }, report)
            .await?;
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
            return Ok(());
        }
        let control = match G::control_page(update) {
            Some((control_spec, _)) => {
                let control = self
                    .read_text_field_segment(
                        control_spec,
                        Stage::Read { page: control_spec },
                        report,
                    )
                    .await?;
                if cancelled(update, should_cancel) {
                    report.outcome = Outcome::Cancelled;
                    return Ok(());
                }
                Some(control)
            }
            None => None,
        };
        let page = update.page();
        let data = self
            .read_text_field_segment(page, Stage::Read { page }, report)
            .await?;
        let invalid = || {
            failure(
                Stage::FreshComparison,
                TextFieldUpdateError::UnsupportedDescriptor,
            )
        };
        let guards = G::fresh(report, control.as_deref())
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        update
            .record(TextFieldUpdateEvent::FreshSession {
                id: session_id(phase),
                identity,
                memory_format: format.get(2).copied().ok_or_else(invalid)?,
                whole_page: &data,
                guards,
            })
            .map_err(|error| failure(Stage::FreshComparison, error))?;
        if cancelled(update, should_cancel) {
            report.outcome = Outcome::Cancelled;
        }
        Ok(())
    }

    async fn read_text_field_segment(
        &mut self,
        page: Page,
        stage: Stage,
        report: &mut TextFieldUpdateSessionReport,
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

    async fn write_text_field<V: TextValue, G: SessionGuards>(
        &mut self,
        update: &TextFieldUpdate<V, G>,
        report: &mut TextFieldUpdateSessionReport,
    ) -> Result<(), TextFieldUpdateSessionError> {
        self.require_mcp_ready()?;
        if self.identity() != Some(update.identity())
            || update.identity().model != RadioModel::TmD750
            || update.identity().firmware.as_str() != "1.02"
            || update.identity().radio_type.as_str() != "K,2,1"
        {
            return Err(TextFieldUpdateError::IdentityMismatch.into());
        }
        if (G::KIND == GuardKind::PmOffGateway && report.gateway_mode != Some(DvGatewayMode::Off))
            || update.page() != required_page(update.field())?
            || update.status() != TextFieldUpdateStatus::PossiblyChanged
            || report.session != Some(TextFieldUpdateSession::Apply)
        {
            return Err(TextFieldUpdateError::UnsupportedDescriptor.into());
        }
        if !report.segments.last().is_some_and(|segment| {
            segment.page == update.page() && segment.data == update.original_page().as_slice()
        }) {
            return Err(TextFieldUpdateError::PageMismatch.into());
        }
        if let Some((control_spec, control_bytes)) = G::control_page(update) {
            let control_matches = report.segments.iter().any(|segment| {
                segment.page == control_spec && segment.data == control_bytes.as_slice()
            });
            if !control_matches {
                return Err(TextFieldUpdateError::ControlPageMismatch.into());
            }
        }
        let target = frame_target(update.field(), update.page())?;
        report.write = TextFieldUpdateWriteDisposition::PossiblyDispatched;
        self.write_fixed_text_frame(target, update.desired_page(), "text field page write")
            .await?;
        report.write = TextFieldUpdateWriteDisposition::Acknowledged;
        Ok(())
    }

    async fn finish_text_field_session(&mut self, report: &mut TextFieldUpdateSessionReport) {
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
