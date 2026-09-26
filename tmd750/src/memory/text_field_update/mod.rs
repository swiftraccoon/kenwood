//! Typed, single-field text updates with fail-closed event sequencing.
//!
//! One update changes one NUL-padded text field inside one complete 256-byte
//! page and verifies it across two MCP sessions. The field is one of
//! [`TextField`]; the value type and the guard policy are type parameters, so
//! the PM Off MY1 update cannot be prepared or driven without its control-page
//! and Gateway facts, and the PM1 name and channel name updates carry none.
//!
//! This module performs no I/O. Each event carries facts the caller reports;
//! this module checks their order and compares page bytes, but cannot confirm
//! that a connection was fresh or that a journal record reached disk.

mod channel;
mod my1;
mod pm1;

use std::fmt;
use std::num::NonZeroU64;
use std::ops::Range;

pub use channel::ChannelNameText;
pub use my1::{ControlPage, My1Callsign, PmOffGatewayFinal, PmOffGatewayFresh, PmOffGatewayGuards};
pub use pm1::Pm1Name;

use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page, PhysicalChannel, RadioModel};

/// The storage fields a text update can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextField {
    /// The global PM1 name: sixteen NUL-padded bytes in the PM control page.
    Pm1Name,
    /// The MY1 callsign of PM Off: eight NUL-padded bytes in the DV Gateway
    /// page, written only with PM Off selected, Gateway Off stored and
    /// observed, and MY1 selected.
    PmOffMy1Callsign,
    /// One physical channel's sixteen NUL-padded name bytes in the name table.
    ChannelName(PhysicalChannel),
}

impl TextField {
    /// The registry field name of the PM1 and MY1 fields; the name table has
    /// no registry entry and reports `memory.ChannelName`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pm1Name => pm1::FIELD_NAME,
            Self::PmOffMy1Callsign => my1::FIELD_NAME,
            Self::ChannelName(_) => channel::FIELD_NAME,
        }
    }
}

mod sealed {
    pub trait Sealed {}
}

/// The validated text of one [`TextField`].
///
/// Implemented by [`Pm1Name`], [`My1Callsign`] and [`ChannelNameText`]; each
/// constructor validates its own field's syntax and the stored encoding is
/// the text followed by NUL padding.
pub trait TextValue: sealed::Sealed + Clone + PartialEq + Eq + fmt::Debug + Send + Sync {
    /// The exact validated text, with spaces preserved.
    fn as_str(&self) -> &str;
}

/// The extra exchanges a driver performs for a guard policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    /// Identity, memory format and the target page only.
    None,
    /// Additionally a fresh Gateway Off observation before entry and the
    /// complete control page in every session, and Gateway Off on the fresh
    /// post-exit connection.
    PmOffGateway,
}

/// The facts a target requires in both sessions beyond the identity, the
/// memory-format byte and the whole target page.
///
/// [`NoGuards`] requires nothing more; [`PmOffGatewayGuards`] requires the
/// control page and Gateway Off. The trait is sealed.
pub trait GuardPolicy: sealed::Sealed + fmt::Debug + Send + Sync {
    /// Facts reported with [`TextFieldUpdateEvent::FreshSession`].
    type Fresh<'a>: fmt::Debug + Send + Sync;
    /// Facts reported with [`TextFieldUpdateEvent::SessionFinalized`].
    type Final<'a>: fmt::Debug + Send + Sync;
    /// Immutable guard state captured at preparation.
    type Stored: fmt::Debug + Send + Sync;
    /// The exchanges a driver performs for this policy.
    const KIND: GuardKind;

    /// Check the fresh facts that precede programming entry.
    ///
    /// # Errors
    ///
    /// Returns the guard's error variant for a state the policy refuses.
    fn check_fresh_state(fresh: &Self::Fresh<'_>) -> Result<(), TextFieldUpdateError>;

    /// Check the fresh facts read inside programming mode against the stored
    /// guard state.
    ///
    /// # Errors
    ///
    /// Returns the guard's error variant for a page that differs.
    fn check_fresh_pages(
        stored: &Self::Stored,
        fresh: &Self::Fresh<'_>,
    ) -> Result<(), TextFieldUpdateError>;

    /// Check the facts observed on the fresh post-exit connection.
    ///
    /// # Errors
    ///
    /// Returns the guard's error variant for a mismatching identity or state.
    fn check_final(
        identity: &Identity,
        facts: &Self::Final<'_>,
    ) -> Result<(), TextFieldUpdateError>;
}

/// The guard policy of the PM1 name and channel name fields: no facts beyond
/// the identity, the memory-format byte and the whole target page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoGuards;

impl sealed::Sealed for NoGuards {}

impl GuardPolicy for NoGuards {
    type Fresh<'a> = ();
    type Final<'a> = ();
    type Stored = ();
    const KIND: GuardKind = GuardKind::None;

    fn check_fresh_state((): &Self::Fresh<'_>) -> Result<(), TextFieldUpdateError> {
        Ok(())
    }

    fn check_fresh_pages(
        (): &Self::Stored,
        (): &Self::Fresh<'_>,
    ) -> Result<(), TextFieldUpdateError> {
        Ok(())
    }

    fn check_final(_identity: &Identity, (): &Self::Final<'_>) -> Result<(), TextFieldUpdateError> {
        Ok(())
    }
}

/// Update status derived from the events recorded so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFieldUpdateStatus {
    /// No write intent has been recorded.
    NotWritten,
    /// A write intent was recorded, and independent-session verification and
    /// finalization have not both completed, so the page may have changed.
    PossiblyChanged,
    /// The desired full page matched immediately and in a distinct MCP session,
    /// and both session lifecycles were finalized.
    VerifiedAcrossSessions,
}

/// The two fixed sessions of a text field update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFieldUpdateSession {
    /// Compare the original page, journal intent, write once, and read back.
    Apply,
    /// Independently read and compare the desired page without another write.
    Verify,
}

/// Events for the two-session update, accepted only in order.
///
/// Each `id` is checked for reuse within this instance and carries no other
/// meaning. `guards` carries the policy's extra facts: `()` for
/// [`NoGuards`], the control page and Gateway observations for
/// [`PmOffGatewayGuards`].
#[derive(Debug)]
pub enum TextFieldUpdateEvent<'a, G: GuardPolicy> {
    /// Report a newly opened MCP connection and its completed reads of the
    /// memory-format byte and the entire target page.
    FreshSession {
        /// Connection identifier, distinct from every previous session ID.
        id: NonZeroU64,
        /// Complete identity freshly obtained from this connection.
        identity: &'a Identity,
        /// Fresh byte at absolute address 10; only zero is supported.
        memory_format: u8,
        /// Complete acknowledged page at [`TextFieldUpdate::page`].
        whole_page: &'a [u8],
        /// The policy's extra fresh facts.
        guards: G::Fresh<'a>,
    },
    /// Report that a private journal record holding the identity, the exact
    /// original and desired pages, and this intent has been written and
    /// synchronized to durable storage before the `W` frame is sent.
    ///
    /// Accepting this event sets [`TextFieldUpdateStatus::PossiblyChanged`],
    /// which no later error clears, even if the write is never dispatched.
    DurableWriteIntent {
        /// Identifier of the sole journal write-intent record, not a session ID.
        id: NonZeroU64,
    },
    /// Report a complete, acknowledged full-page read taken immediately after
    /// the write.
    ImmediateReadback {
        /// Newly read page, including every unrelated byte.
        whole_page: &'a [u8],
    },
    /// Report E/ACK, original close and drop, a bounded fresh CAT exchange
    /// matching the entire identity, fresh close, complete captures, and a
    /// synchronized record of this session.
    SessionFinalized {
        /// Identifier supplied in the current session's `FreshSession` event.
        id: NonZeroU64,
        /// The policy's extra post-exit facts.
        guards: G::Final<'a>,
    },
}

use TextFieldUpdateSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent,
    Readback,
    Finalize(Session),
    Complete,
    Halted,
}

/// Immutable page preparation and fail-closed sequencing for one text field.
///
/// Scope is fixed by the field: exactly one field on TM-D750 / firmware 1.02
/// / type `K,2,1`, inside the complete 256-byte page that holds it. Every
/// other byte of that page is carried unchanged and compared in full before
/// and after the write. The three fields are prepared through the
/// constructors of [`Pm1NameUpdate`], [`My1CallsignUpdate`] and
/// [`ChannelNameUpdate`].
///
/// The sequence is: fresh original-page comparison, journal write-intent
/// record, immediate desired-page readback, finalized session; then a
/// distinct fresh session with a desired-page comparison and finalized
/// cleanup. An error permanently halts the transaction and keeps its current
/// status. A changed page is never rebased and a stale original page is never
/// written back automatically.
#[derive(Debug)]
pub struct TextFieldUpdate<V: TextValue, G: GuardPolicy> {
    field: TextField,
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    current: Option<V>,
    requested: Option<V>,
    guards: G::Stored,
    status: TextFieldUpdateStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intent: Option<NonZeroU64>,
}

/// The global PM1 name update.
pub type Pm1NameUpdate = TextFieldUpdate<Pm1Name, NoGuards>;
/// The PM Off MY1 callsign update.
pub type My1CallsignUpdate = TextFieldUpdate<My1Callsign, PmOffGatewayGuards>;
/// One channel's name update.
pub type ChannelNameUpdate = TextFieldUpdate<ChannelNameText, NoGuards>;

/// Require the exact software layout every text update is pinned to.
fn check_identity(identity: &Identity) -> Result<(), TextFieldUpdateError> {
    if identity.model != RadioModel::TmD750
        || identity.firmware.as_str() != "1.02"
        || identity.radio_type.as_str() != "K,2,1"
    {
        return Err(TextFieldUpdateError::IdentityMismatch);
    }
    Ok(())
}

/// Require a complete page.
fn complete_page(bytes: &[u8]) -> Result<[u8; PAGE_SIZE], TextFieldUpdateError> {
    bytes
        .try_into()
        .map_err(|_| TextFieldUpdateError::PageLength {
            actual: bytes.len(),
        })
}

/// The desired page: `original` with `range` replaced by `desired`, after the
/// captured field matched `expected` exactly and the change is not a no-op.
fn replace_field(
    original: &[u8; PAGE_SIZE],
    range: Range<usize>,
    expected: &[u8],
    desired: &[u8],
    unchanged: bool,
) -> Result<[u8; PAGE_SIZE], TextFieldUpdateError> {
    if original.get(range.clone()) != Some(expected) {
        return Err(TextFieldUpdateError::CurrentValueMismatch);
    }
    if unchanged {
        return Err(TextFieldUpdateError::NoChange);
    }
    let mut page = *original;
    page.get_mut(range)
        .filter(|slice| slice.len() == desired.len())
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?
        .copy_from_slice(desired);
    Ok(page)
}

impl<V: TextValue, G: GuardPolicy> TextFieldUpdate<V, G> {
    #[expect(
        clippy::too_many_arguments,
        reason = "the field, identity, page, both images, both values and the guards are the whole immutable state"
    )]
    const fn new(
        field: TextField,
        identity: Identity,
        page: Page,
        original: [u8; PAGE_SIZE],
        desired: [u8; PAGE_SIZE],
        current: Option<V>,
        requested: Option<V>,
        guards: G::Stored,
    ) -> Self {
        Self {
            field,
            identity,
            page,
            original,
            desired,
            current,
            requested,
            guards,
            status: TextFieldUpdateStatus::NotWritten,
            phase: Phase::Fresh(Session::Apply),
            sessions: Vec::new(),
            intent: None,
        }
    }

    /// The field this update changes.
    #[must_use]
    pub const fn field(&self) -> TextField {
        self.field
    }

    /// The complete page containing the field.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Complete baseline identity, retained with both exact page images.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Exact captured page, for compare-before-write and the recovery record.
    #[must_use]
    pub const fn original_page(&self) -> &[u8; PAGE_SIZE] {
        &self.original
    }

    /// Exact desired page, differing from the original only within the field.
    #[must_use]
    pub const fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        &self.desired
    }

    /// The value matched against the captured field; `None` is the all-NUL
    /// field.
    #[must_use]
    pub const fn current(&self) -> Option<&V> {
        self.current.as_ref()
    }

    /// The requested value; `None` clears the field to NUL bytes.
    #[must_use]
    pub const fn requested(&self) -> Option<&V> {
        self.requested.as_ref()
    }

    /// The policy's immutable guard state captured at preparation.
    #[must_use]
    pub const fn guards(&self) -> &G::Stored {
        &self.guards
    }

    /// Update status derived from the events recorded so far.
    #[must_use]
    pub const fn status(&self) -> TextFieldUpdateStatus {
        self.status
    }

    /// The session expected next, available only while awaiting a
    /// [`TextFieldUpdateEvent::FreshSession`].
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::TerminalState`] after completion or
    /// halt, otherwise [`TextFieldUpdateError::UnexpectedEvent`] during a
    /// session.
    pub const fn next_session(&self) -> Result<TextFieldUpdateSession, TextFieldUpdateError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(TextFieldUpdateError::TerminalState),
            _ => Err(TextFieldUpdateError::UnexpectedEvent),
        }
    }

    /// Validate the exact next event and advance the sequence.
    ///
    /// An accepted [`TextFieldUpdateEvent::DurableWriteIntent`] sets status to
    /// [`TextFieldUpdateStatus::PossiblyChanged`] before dispatch could occur.
    /// Any error permanently halts further acceptance without clearing it.
    ///
    /// # Errors
    ///
    /// Rejects incorrect order, reused sessions, identity, guard or format
    /// mismatches, partial or differing pages, and mismatched finalization IDs.
    pub fn record(
        &mut self,
        event: TextFieldUpdateEvent<'_, G>,
    ) -> Result<(), TextFieldUpdateError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    /// Permanently halt the transaction.
    ///
    /// Performs no cleanup, rollback, or I/O cancellation. A previously
    /// accepted intent keeps [`TextFieldUpdateStatus::PossiblyChanged`]; a
    /// completed update stays [`TextFieldUpdateStatus::VerifiedAcrossSessions`].
    pub const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    fn record_inner(
        &mut self,
        event: TextFieldUpdateEvent<'_, G>,
    ) -> Result<(), TextFieldUpdateError> {
        match (self.phase, event) {
            (Phase::Complete | Phase::Halted, _) => Err(TextFieldUpdateError::TerminalState),
            (
                Phase::Fresh(session),
                TextFieldUpdateEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    whole_page,
                    guards,
                },
            ) => self.fresh_session(session, id, identity, memory_format, whole_page, &guards),
            (Phase::Intent, TextFieldUpdateEvent::DurableWriteIntent { id }) => {
                if self.intent.is_some() {
                    return Err(TextFieldUpdateError::UnexpectedEvent);
                }
                self.intent = Some(id);
                self.status = TextFieldUpdateStatus::PossiblyChanged;
                self.phase = Phase::Readback;
                Ok(())
            }
            (Phase::Readback, TextFieldUpdateEvent::ImmediateReadback { whole_page }) => {
                self.compare_page(whole_page, true)?;
                self.phase = Phase::Finalize(Session::Apply);
                Ok(())
            }
            (Phase::Finalize(session), TextFieldUpdateEvent::SessionFinalized { id, guards }) => {
                if self.sessions.last() != Some(&id) {
                    return Err(TextFieldUpdateError::SessionMismatch);
                }
                G::check_final(&self.identity, &guards)?;
                self.phase = match session {
                    Session::Apply => Phase::Fresh(Session::Verify),
                    Session::Verify => {
                        self.status = TextFieldUpdateStatus::VerifiedAcrossSessions;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(TextFieldUpdateError::UnexpectedEvent),
        }
    }

    fn fresh_session(
        &mut self,
        session: Session,
        id: NonZeroU64,
        identity: &Identity,
        memory_format: u8,
        whole_page: &[u8],
        guards: &G::Fresh<'_>,
    ) -> Result<(), TextFieldUpdateError> {
        if self.sessions.contains(&id) {
            return Err(TextFieldUpdateError::ReusedSession);
        }
        if identity != &self.identity {
            return Err(TextFieldUpdateError::IdentityMismatch);
        }
        G::check_fresh_state(guards)?;
        if memory_format != 0 {
            return Err(TextFieldUpdateError::MemoryFormat {
                actual: memory_format,
            });
        }
        G::check_fresh_pages(&self.guards, guards)?;
        self.compare_page(whole_page, session == Session::Verify)?;
        self.sessions.push(id);
        self.phase = match session {
            Session::Apply => Phase::Intent,
            Session::Verify => Phase::Finalize(session),
        };
        Ok(())
    }

    fn compare_page(&self, bytes: &[u8], desired: bool) -> Result<(), TextFieldUpdateError> {
        if bytes.len() != PAGE_SIZE {
            return Err(TextFieldUpdateError::PageLength {
                actual: bytes.len(),
            });
        }
        let expected = if desired {
            &self.desired
        } else {
            &self.original
        };
        if bytes != expected {
            return Err(TextFieldUpdateError::PageMismatch);
        }
        Ok(())
    }
}

/// A rejected value, preparation precondition, or event transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TextFieldUpdateError {
    /// The text is outside the field's accepted syntax.
    #[error("{field} text must be {rule}")]
    InvalidText {
        /// The field whose syntax rejected the text.
        field: &'static str,
        /// The accepted syntax.
        rule: &'static str,
    },
    /// A generated registry field or canonical page no longer has the pinned
    /// shape.
    #[error("generated descriptor is outside the bounded text-update scope")]
    UnsupportedDescriptor,
    /// The channel's name address does not resolve to a complete page on the
    /// name-table grid inside the writable global region.
    #[error("channel name address is outside the bounded name-table page scope")]
    UnsupportedPage,
    /// The full identity differs from the pinned target or captured baseline.
    #[error("text field update requires the exact target and baseline identity")]
    IdentityMismatch,
    /// A complete page is required at preparation and every comparison.
    #[error("text field update requires a complete 256-byte page, got {actual} bytes")]
    PageLength {
        /// Supplied byte count.
        actual: usize,
    },
    /// The immutable control page is incomplete or oversized.
    #[error("text field update requires a complete 256-byte control page, got {actual} bytes")]
    ControlPageLength {
        /// Supplied control-page byte count.
        actual: usize,
    },
    /// The supplied expected value does not exactly match the captured field.
    #[error("expected text does not match the captured field")]
    CurrentValueMismatch,
    /// Current and desired values are identical, so no write is needed.
    #[error("the field already holds the requested text; no change is needed")]
    NoChange,
    /// Only PM Off is accepted; this update never selects another slot.
    #[error("MY1 update requires PM Off, got stored selector {actual}")]
    PmSelection {
        /// Observed PM selector.
        actual: u8,
    },
    /// Stored or fresh Gateway state must remain Off throughout both sessions.
    #[error("MY1 update requires Gateway Off, got {actual}")]
    GatewayMode {
        /// Actual observed Gateway state.
        actual: DvGatewayMode,
    },
    /// The captured MY selection must point to MY1.
    #[error("MY1 update requires stored MY selector zero, got {actual}")]
    MySelection {
        /// Observed MY selector.
        actual: u8,
    },
    /// A fresh session supplied an unsupported memory-format byte.
    #[error("text field update requires memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Observed memory-format byte.
        actual: u8,
    },
    /// At least one byte differs from the exact page required at this stage.
    #[error("whole-page comparison failed")]
    PageMismatch,
    /// Any byte of the immutable control page differs.
    #[error("immutable control-page comparison failed")]
    ControlPageMismatch,
    /// The event is not the exact next step of the two-session sequence.
    #[error("text field update events are out of order")]
    UnexpectedEvent,
    /// A supposed fresh session reused an earlier connection identifier.
    #[error("text field update session ID was reused")]
    ReusedSession,
    /// Finalization does not identify the current session.
    #[error("text field update finalization does not match the current session")]
    SessionMismatch,
    /// The completed or permanently halted instance accepts no more events.
    #[error("text field update is terminal and accepts no further events")]
    TerminalState,
}
