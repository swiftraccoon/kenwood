//! Typed, single-field PM1 updates with fail-closed event sequencing.
//!
//! This module performs no I/O. Each event carries facts the caller reports;
//! this module checks their order and compares page bytes, but cannot confirm
//! that a connection was fresh or that a journal record reached disk.

use std::num::NonZeroU64;

use super::{FieldCodec, FieldValue, MenuField, StringEncoding, menu_field};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{PAGE_SIZE, Page, RadioModel};

const FIELD_NAME: &str = "pm.PmName1";
const FIELD_ADDRESS: u32 = 323_594;
const FIELD_LENGTH: usize = 16;
const PAGE_ADDRESS: u32 = 323_584;

/// A nonempty PM1 name containing at most sixteen printable ASCII bytes.
///
/// Spaces are preserved exactly, including leading and trailing spaces. The
/// accepted bytes are ASCII graphic characters and the space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pm1Name(String);

impl Pm1Name {
    /// Validate a name without truncation, trimming, or replacement encoding.
    ///
    /// # Errors
    ///
    /// Returns [`Pm1NameUpdateError::InvalidName`] for empty, oversized, control,
    /// non-ASCII, or embedded-NUL input.
    pub fn new(value: &str) -> Result<Self, Pm1NameUpdateError> {
        if value.is_empty()
            || value.len() > FIELD_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(Pm1NameUpdateError::InvalidName);
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact validated text, with spaces preserved.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Update status derived from the events recorded so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm1NameUpdateStatus {
    /// No write intent has been recorded.
    NotWritten,
    /// A write intent was recorded, and independent-session verification and
    /// finalization have not both completed, so the page may have changed.
    PossiblyChanged,
    /// The desired full page matched immediately and in a distinct MCP session,
    /// and both session lifecycles were finalized.
    VerifiedAcrossSessions,
}

/// The two fixed sessions of a PM1 name update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pm1NameUpdateSession {
    /// Compare the original page, journal intent, write once, and read back.
    Apply,
    /// Independently read and compare the desired page without another write.
    Verify,
}

/// Events for the two-session update, accepted only in order.
///
/// Each `id` is checked for reuse within this instance and carries no other
/// meaning.
#[derive(Debug)]
pub enum Pm1NameUpdateEvent<'a> {
    /// Report a newly opened MCP connection and its completed reads of the
    /// memory-format byte and the entire target page.
    FreshSession {
        /// Connection identifier, distinct from every previous session ID.
        id: NonZeroU64,
        /// Complete identity freshly obtained from this connection.
        identity: &'a Identity,
        /// Fresh byte at absolute address 10; only zero is supported.
        memory_format: u8,
        /// Complete acknowledged page at [`Pm1NameUpdate::page`].
        whole_page: &'a [u8],
    },
    /// Report that a private journal record holding the identity, the exact
    /// original and desired pages, and this intent has been written and
    /// synchronized to durable storage before the `W` frame is sent.
    ///
    /// Accepting this event sets [`Pm1NameUpdateStatus::PossiblyChanged`],
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
    },
}

use Pm1NameUpdateSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent,
    Readback,
    Finalize(Session),
    Complete,
    Halted,
}

/// Immutable page preparation and fail-closed sequencing for a PM1 name update.
///
/// Scope is fixed: global PM1 on TM-D750 / firmware 1.02 / type `K,2,1`, and
/// the generated descriptor must retain its pinned shape. No other address or
/// field can be selected.
///
/// The sequence is: fresh original-page comparison, journal write-intent record, immediate
/// desired-page readback, finalized session; then a distinct fresh session with
/// a desired-page comparison and finalized cleanup. An error permanently halts
/// the transaction and keeps its current status. A changed page is never
/// rebased and a stale original page is never written back automatically.
#[derive(Debug)]
pub struct Pm1NameUpdate {
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    current_name: Pm1Name,
    desired_name: Pm1Name,
    status: Pm1NameUpdateStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intent: Option<NonZeroU64>,
}

impl Pm1NameUpdate {
    /// Resolve the complete canonical page required from a validated capture.
    ///
    /// # Errors
    ///
    /// Returns [`Pm1NameUpdateError::UnsupportedDescriptor`] if the generated
    /// PM1 descriptor or canonical page no longer has the pinned shape.
    pub fn required_page() -> Result<Page, Pm1NameUpdateError> {
        let field = menu_field(FIELD_NAME).ok_or(Pm1NameUpdateError::UnsupportedDescriptor)?;
        supported_page(field)
    }

    /// Prepare a compare-before-write update without opening a radio.
    ///
    /// `baseline` must contain the complete observed canonical page from a
    /// validated capture, not synthesized gap bytes. `expected_current` must
    /// match its exact NUL-padded PM1 field.
    ///
    /// # Errors
    ///
    /// Rejects unsupported identity or descriptor, incomplete page, mismatched
    /// current name, or identical current and desired names. No-op requests
    /// return [`Pm1NameUpdateError::NoChange`] before any I/O is possible.
    pub fn prepare(
        identity: &Identity,
        baseline: &[u8],
        expected_current: &Pm1Name,
        desired: &Pm1Name,
    ) -> Result<Self, Pm1NameUpdateError> {
        let field = menu_field(FIELD_NAME).ok_or(Pm1NameUpdateError::UnsupportedDescriptor)?;
        Self::prepare_with_field(identity, baseline, expected_current, desired, field)
    }

    fn prepare_with_field(
        identity: &Identity,
        baseline: &[u8],
        expected_current: &Pm1Name,
        desired_name: &Pm1Name,
        field: &MenuField,
    ) -> Result<Self, Pm1NameUpdateError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(Pm1NameUpdateError::IdentityMismatch);
        }
        let page = supported_page(field)?;
        let original: [u8; PAGE_SIZE] =
            baseline
                .try_into()
                .map_err(|_| Pm1NameUpdateError::PageLength {
                    actual: baseline.len(),
                })?;
        let offset = field
            .descriptor
            .address(None)
            .map_err(|_| Pm1NameUpdateError::UnsupportedDescriptor)?
            .as_usize()
            - page.address().as_usize();
        let range = offset..offset + FIELD_LENGTH;
        if original.get(range.clone()) != Some(encode_name(field, expected_current)?.as_slice()) {
            return Err(Pm1NameUpdateError::CurrentNameMismatch);
        }
        if expected_current == desired_name {
            return Err(Pm1NameUpdateError::NoChange);
        }
        let mut desired = original;
        desired
            .get_mut(range)
            .ok_or(Pm1NameUpdateError::UnsupportedDescriptor)?
            .copy_from_slice(&encode_name(field, desired_name)?);
        Ok(Self {
            identity: identity.clone(),
            page,
            original,
            desired,
            current_name: expected_current.clone(),
            desired_name: desired_name.clone(),
            status: Pm1NameUpdateStatus::NotWritten,
            phase: Phase::Fresh(Session::Apply),
            sessions: Vec::with_capacity(2),
            intent: None,
        })
    }

    /// The single canonical page containing global PM1's name.
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

    /// Exact desired page, differing from the original only within PM1's name.
    #[must_use]
    pub const fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        &self.desired
    }

    /// Expected original name, matched to the complete captured field.
    #[must_use]
    pub const fn current_name(&self) -> &Pm1Name {
        &self.current_name
    }

    /// Desired name, encoded without truncation or trimming.
    #[must_use]
    pub const fn desired_name(&self) -> &Pm1Name {
        &self.desired_name
    }

    /// Update status derived from the events recorded so far.
    #[must_use]
    pub const fn status(&self) -> Pm1NameUpdateStatus {
        self.status
    }

    /// The session expected next, available only while awaiting a
    /// [`Pm1NameUpdateEvent::FreshSession`].
    ///
    /// # Errors
    ///
    /// Returns [`Pm1NameUpdateError::TerminalState`] after completion or halt,
    /// otherwise [`Pm1NameUpdateError::UnexpectedEvent`] during a session.
    pub const fn next_session(&self) -> Result<Pm1NameUpdateSession, Pm1NameUpdateError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(Pm1NameUpdateError::TerminalState),
            _ => Err(Pm1NameUpdateError::UnexpectedEvent),
        }
    }

    /// Validate the exact next event and advance the sequence.
    ///
    /// An accepted [`Pm1NameUpdateEvent::DurableWriteIntent`] sets status to
    /// [`Pm1NameUpdateStatus::PossiblyChanged`] before dispatch could occur.
    /// Any error permanently halts further acceptance without clearing it.
    ///
    /// # Errors
    ///
    /// Rejects incorrect order, reused sessions, identity or format mismatches,
    /// partial or differing pages, and mismatched finalization IDs.
    pub fn record(&mut self, event: Pm1NameUpdateEvent<'_>) -> Result<(), Pm1NameUpdateError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    /// Permanently halt the transaction.
    ///
    /// Performs no cleanup, rollback, or I/O cancellation. A previously
    /// accepted intent keeps [`Pm1NameUpdateStatus::PossiblyChanged`]; a
    /// completed update stays [`Pm1NameUpdateStatus::VerifiedAcrossSessions`].
    pub const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    fn record_inner(&mut self, event: Pm1NameUpdateEvent<'_>) -> Result<(), Pm1NameUpdateError> {
        match (self.phase, event) {
            (Phase::Complete | Phase::Halted, _) => Err(Pm1NameUpdateError::TerminalState),
            (
                Phase::Fresh(session),
                Pm1NameUpdateEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    whole_page,
                },
            ) => self.fresh_session(session, id, identity, memory_format, whole_page),
            (Phase::Intent, Pm1NameUpdateEvent::DurableWriteIntent { id }) => {
                if self.intent.is_some() {
                    return Err(Pm1NameUpdateError::UnexpectedEvent);
                }
                self.intent = Some(id);
                self.status = Pm1NameUpdateStatus::PossiblyChanged;
                self.phase = Phase::Readback;
                Ok(())
            }
            (Phase::Readback, Pm1NameUpdateEvent::ImmediateReadback { whole_page }) => {
                self.compare_page(whole_page, true)?;
                self.phase = Phase::Finalize(Session::Apply);
                Ok(())
            }
            (Phase::Finalize(session), Pm1NameUpdateEvent::SessionFinalized { id }) => {
                if self.sessions.last() != Some(&id) {
                    return Err(Pm1NameUpdateError::SessionMismatch);
                }
                self.phase = match session {
                    Session::Apply => Phase::Fresh(Session::Verify),
                    Session::Verify => {
                        self.status = Pm1NameUpdateStatus::VerifiedAcrossSessions;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(Pm1NameUpdateError::UnexpectedEvent),
        }
    }

    fn fresh_session(
        &mut self,
        session: Session,
        id: NonZeroU64,
        identity: &Identity,
        memory_format: u8,
        whole_page: &[u8],
    ) -> Result<(), Pm1NameUpdateError> {
        if self.sessions.contains(&id) {
            return Err(Pm1NameUpdateError::ReusedSession);
        }
        if identity != &self.identity {
            return Err(Pm1NameUpdateError::IdentityMismatch);
        }
        if memory_format != 0 {
            return Err(Pm1NameUpdateError::MemoryFormat {
                actual: memory_format,
            });
        }
        self.compare_page(whole_page, session == Session::Verify)?;
        self.sessions.push(id);
        self.phase = match session {
            Session::Apply => Phase::Intent,
            Session::Verify => Phase::Finalize(session),
        };
        Ok(())
    }

    fn compare_page(&self, bytes: &[u8], desired: bool) -> Result<(), Pm1NameUpdateError> {
        if bytes.len() != PAGE_SIZE {
            return Err(Pm1NameUpdateError::PageLength {
                actual: bytes.len(),
            });
        }
        let expected = if desired {
            &self.desired
        } else {
            &self.original
        };
        if bytes != expected {
            return Err(Pm1NameUpdateError::PageMismatch);
        }
        Ok(())
    }
}

fn supported_page(field: &MenuField) -> Result<Page, Pm1NameUpdateError> {
    if field.descriptor.name != FIELD_NAME
        || field.descriptor.base != FIELD_ADDRESS
        || !field.descriptor.terms.is_empty()
        || field.descriptor.codec
            != (FieldCodec::FixedString {
                len: FIELD_LENGTH,
                encoding: StringEncoding::Utf8,
                padding: 0,
            })
        || field.menu != "pm"
        || field.enum_type.is_some()
        || !field.options.is_empty()
        || !field.allowed_values.is_empty()
        || field.storage_transform.is_some()
        || field.is_blob
    {
        return Err(Pm1NameUpdateError::UnsupportedDescriptor);
    }
    field
        .descriptor
        .address(None)
        .ok()
        .and_then(writable_page_for)
        .filter(|page| page.address().as_u32() == PAGE_ADDRESS && page.len() == PAGE_SIZE)
        .ok_or(Pm1NameUpdateError::UnsupportedDescriptor)
}

fn encode_name(
    field: &MenuField,
    name: &Pm1Name,
) -> Result<[u8; FIELD_LENGTH], Pm1NameUpdateError> {
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(name.as_str()))
        .map_err(|_| Pm1NameUpdateError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(Pm1NameUpdateError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    for (index, patch) in encoded.into_iter().enumerate() {
        if index != patch.offset() || patch.mask() != u8::MAX {
            return Err(Pm1NameUpdateError::UnsupportedDescriptor);
        }
        *bytes
            .get_mut(patch.offset())
            .ok_or(Pm1NameUpdateError::UnsupportedDescriptor)? = patch.value();
    }
    Ok(bytes)
}

/// A rejected name, preparation precondition, or event transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Pm1NameUpdateError {
    /// The name is empty, oversized, or contains bytes outside printable ASCII.
    #[error("PM1 names must contain 1 to 16 printable ASCII bytes")]
    InvalidName,
    /// The generated PM1 field or canonical page has a different shape.
    #[error("generated PM1 descriptor is outside the bounded name-update scope")]
    UnsupportedDescriptor,
    /// The full identity differs from the pinned target or captured baseline.
    #[error("PM1 name update requires the exact target and baseline identity")]
    IdentityMismatch,
    /// A complete canonical page is required at preparation and every comparison.
    #[error("PM1 name update requires a complete 256-byte page, got {actual} bytes")]
    PageLength {
        /// Supplied byte count.
        actual: usize,
    },
    /// The supplied expected name does not exactly match the captured field.
    #[error("expected PM1 name does not match the captured page")]
    CurrentNameMismatch,
    /// Current and desired names are identical, so no write is needed.
    #[error("PM1 already has the requested name; no change is needed")]
    NoChange,
    /// A fresh session supplied an unsupported memory-format byte.
    #[error("PM1 name update requires memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Observed memory-format byte.
        actual: u8,
    },
    /// At least one byte differs from the exact page required at this stage.
    #[error("PM1 whole-page comparison failed")]
    PageMismatch,
    /// The event is not the exact next step of the two-session sequence.
    #[error("PM1 name-update events are out of order")]
    UnexpectedEvent,
    /// A supposed fresh session reused an earlier connection identifier.
    #[error("PM1 name-update session ID was reused")]
    ReusedSession,
    /// Finalization does not identify the current session.
    #[error("PM1 name-update finalization does not match the current session")]
    SessionMismatch,
    /// The completed or permanently halted instance accepts no more events.
    #[error("PM1 name update is terminal and accepts no further events")]
    TerminalState,
}

#[cfg(test)]
#[path = "pm1_name_update_tests.rs"]
mod tests;
