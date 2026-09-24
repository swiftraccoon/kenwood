//! Typed, single-channel name updates with fail-closed event sequencing.
//!
//! This module performs no I/O. Each event carries facts the caller reports;
//! this module checks their order and compares page bytes, but cannot confirm
//! that a connection was fresh or that a journal record reached disk.

use std::num::NonZeroU64;

use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{
    Address, CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET, PAGE_SIZE, Page, PhysicalChannel, RadioModel,
};

/// A nonempty channel name containing at most sixteen printable ASCII bytes.
///
/// Spaces are preserved exactly, including leading and trailing spaces. The
/// accepted bytes are ASCII graphic characters and the space. The stored
/// field is the text followed by NUL padding to sixteen bytes; an unnamed
/// channel stores sixteen NUL bytes and is represented by `None` wherever an
/// `Option<&ChannelNameText>` is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNameText(String);

impl ChannelNameText {
    /// Validate a name without truncation, trimming, or replacement encoding.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelNameUpdateError::InvalidName`] for empty, oversized,
    /// control, non-ASCII, or embedded-NUL input.
    pub fn new(value: &str) -> Result<Self, ChannelNameUpdateError> {
        if value.is_empty()
            || value.len() > CHANNEL_NAME_SIZE
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(ChannelNameUpdateError::InvalidName);
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact validated text, with spaces preserved.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The sixteen stored bytes: the text followed by NUL padding.
    #[must_use]
    pub fn to_field(&self) -> [u8; CHANNEL_NAME_SIZE] {
        let mut field = [0; CHANNEL_NAME_SIZE];
        for (slot, byte) in field.iter_mut().zip(self.0.bytes()) {
            *slot = byte;
        }
        field
    }
}

/// The stored field of an optional name; `None` is the all-NUL field of an
/// unnamed channel.
fn encode_field(name: Option<&ChannelNameText>) -> [u8; CHANNEL_NAME_SIZE] {
    name.map_or([0; CHANNEL_NAME_SIZE], ChannelNameText::to_field)
}

/// Update status derived from the events recorded so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelNameUpdateStatus {
    /// No write intent has been recorded.
    NotWritten,
    /// A write intent was recorded, and independent-session verification and
    /// finalization have not both completed, so the page may have changed.
    PossiblyChanged,
    /// The desired full page matched immediately and in a distinct MCP session,
    /// and both session lifecycles were finalized.
    VerifiedAcrossSessions,
}

/// The two fixed sessions of a channel name update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelNameUpdateSession {
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
pub enum ChannelNameUpdateEvent<'a> {
    /// Report a newly opened MCP connection and its completed reads of the
    /// memory-format byte and the entire target page.
    FreshSession {
        /// Connection identifier, distinct from every previous session ID.
        id: NonZeroU64,
        /// Complete identity freshly obtained from this connection.
        identity: &'a Identity,
        /// Fresh byte at absolute address 10; only zero is supported.
        memory_format: u8,
        /// Complete acknowledged page at [`ChannelNameUpdate::page`].
        whole_page: &'a [u8],
    },
    /// Report that a private journal record holding the identity, the exact
    /// original and desired pages, and this intent has been written and
    /// synchronized to durable storage before the `W` frame is sent.
    ///
    /// Accepting this event sets [`ChannelNameUpdateStatus::PossiblyChanged`],
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

use ChannelNameUpdateSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent,
    Readback,
    Finalize(Session),
    Complete,
    Halted,
}

/// Immutable page preparation and fail-closed sequencing for one channel's
/// name update.
///
/// Scope is fixed: one physical channel's sixteen name bytes on
/// TM-D750 / firmware 1.02 / type `K,2,1`, inside the complete 256-byte name
/// page that holds it. The other fifteen names on that page, including the
/// weather-channel names that share the last page, are carried unchanged and
/// compared in full before and after the write.
///
/// The sequence is: fresh original-page comparison, journal write-intent
/// record, immediate desired-page readback, finalized session; then a
/// distinct fresh session with a desired-page comparison and finalized
/// cleanup. An error permanently halts the transaction and keeps its current
/// status. A changed page is never rebased and a stale original page is never
/// written back automatically.
#[derive(Debug)]
pub struct ChannelNameUpdate {
    identity: Identity,
    channel: PhysicalChannel,
    page: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    current_name: Option<ChannelNameText>,
    desired_name: Option<ChannelNameText>,
    status: ChannelNameUpdateStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intent: Option<NonZeroU64>,
}

impl ChannelNameUpdate {
    /// Resolve the complete name-table page that holds `channel`'s name.
    ///
    /// Sixteen names share each 256-byte page of the table at
    /// [`CHANNEL_NAMES_OFFSET`]; the returned page starts on that grid and
    /// lies inside the writable global settings region.
    ///
    /// # Errors
    ///
    /// Returns [`ChannelNameUpdateError::UnsupportedPage`] if the name address
    /// does not resolve to a complete page on the name-table grid.
    pub fn required_page(channel: PhysicalChannel) -> Result<Page, ChannelNameUpdateError> {
        let name_address = channel.name_address();
        let address = u32::try_from(name_address)
            .ok()
            .and_then(|address| Address::new(address).ok())
            .ok_or(ChannelNameUpdateError::UnsupportedPage)?;
        let page = writable_page_for(address).ok_or(ChannelNameUpdateError::UnsupportedPage)?;
        let grid_start =
            CHANNEL_NAMES_OFFSET + (name_address - CHANNEL_NAMES_OFFSET) / PAGE_SIZE * PAGE_SIZE;
        if page.len() != PAGE_SIZE || page.address().as_usize() != grid_start {
            return Err(ChannelNameUpdateError::UnsupportedPage);
        }
        Ok(page)
    }

    /// Prepare a compare-before-write update without opening a radio.
    ///
    /// `baseline` must contain the complete observed page returned by
    /// [`Self::required_page`] for `channel`, taken from a validated capture,
    /// not synthesized gap bytes. `expected_current` must match its exact
    /// NUL-padded name field; `None` is the all-NUL field of an unnamed
    /// channel. `desired` replaces that field; `None` clears the name.
    ///
    /// # Errors
    ///
    /// Rejects an unsupported identity or page, an incomplete baseline, a
    /// mismatched current name, or identical current and desired names. No-op
    /// requests return [`ChannelNameUpdateError::NoChange`] before any I/O is
    /// possible.
    pub fn prepare(
        identity: &Identity,
        baseline: &[u8],
        channel: PhysicalChannel,
        expected_current: Option<&ChannelNameText>,
        desired: Option<&ChannelNameText>,
    ) -> Result<Self, ChannelNameUpdateError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(ChannelNameUpdateError::IdentityMismatch);
        }
        let page = Self::required_page(channel)?;
        let original: [u8; PAGE_SIZE] =
            baseline
                .try_into()
                .map_err(|_| ChannelNameUpdateError::PageLength {
                    actual: baseline.len(),
                })?;
        let offset = channel.name_address() - page.address().as_usize();
        let range = offset..offset + CHANNEL_NAME_SIZE;
        if original.get(range.clone()) != Some(encode_field(expected_current).as_slice()) {
            return Err(ChannelNameUpdateError::CurrentNameMismatch);
        }
        if expected_current == desired {
            return Err(ChannelNameUpdateError::NoChange);
        }
        let mut desired_page = original;
        desired_page
            .get_mut(range)
            .ok_or(ChannelNameUpdateError::UnsupportedPage)?
            .copy_from_slice(&encode_field(desired));
        Ok(Self {
            identity: identity.clone(),
            channel,
            page,
            original,
            desired: desired_page,
            current_name: expected_current.cloned(),
            desired_name: desired.cloned(),
            status: ChannelNameUpdateStatus::NotWritten,
            phase: Phase::Fresh(Session::Apply),
            sessions: Vec::with_capacity(2),
            intent: None,
        })
    }

    /// The complete name-table page containing the channel's name.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// The physical channel whose name changes.
    #[must_use]
    pub const fn channel(&self) -> PhysicalChannel {
        self.channel
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

    /// Exact desired page, differing from the original only within the
    /// channel's sixteen name bytes.
    #[must_use]
    pub const fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        &self.desired
    }

    /// Expected original name, matched to the complete captured field;
    /// `None` for an unnamed channel.
    #[must_use]
    pub const fn current_name(&self) -> Option<&ChannelNameText> {
        self.current_name.as_ref()
    }

    /// Desired name, encoded without truncation or trimming; `None` clears
    /// the name.
    #[must_use]
    pub const fn desired_name(&self) -> Option<&ChannelNameText> {
        self.desired_name.as_ref()
    }

    /// Update status derived from the events recorded so far.
    #[must_use]
    pub const fn status(&self) -> ChannelNameUpdateStatus {
        self.status
    }

    /// The session expected next, available only while awaiting a
    /// [`ChannelNameUpdateEvent::FreshSession`].
    ///
    /// # Errors
    ///
    /// Returns [`ChannelNameUpdateError::TerminalState`] after completion or
    /// halt, otherwise [`ChannelNameUpdateError::UnexpectedEvent`] during a
    /// session.
    pub const fn next_session(&self) -> Result<ChannelNameUpdateSession, ChannelNameUpdateError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(ChannelNameUpdateError::TerminalState),
            _ => Err(ChannelNameUpdateError::UnexpectedEvent),
        }
    }

    /// Validate the exact next event and advance the sequence.
    ///
    /// An accepted [`ChannelNameUpdateEvent::DurableWriteIntent`] sets status
    /// to [`ChannelNameUpdateStatus::PossiblyChanged`] before dispatch could
    /// occur. Any error permanently halts further acceptance without clearing
    /// it.
    ///
    /// # Errors
    ///
    /// Rejects incorrect order, reused sessions, identity or format mismatches,
    /// partial or differing pages, and mismatched finalization IDs.
    pub fn record(
        &mut self,
        event: ChannelNameUpdateEvent<'_>,
    ) -> Result<(), ChannelNameUpdateError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    /// Permanently halt the transaction.
    ///
    /// Performs no cleanup, rollback, or I/O cancellation. A previously
    /// accepted intent keeps [`ChannelNameUpdateStatus::PossiblyChanged`]; a
    /// completed update stays [`ChannelNameUpdateStatus::VerifiedAcrossSessions`].
    pub const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    fn record_inner(
        &mut self,
        event: ChannelNameUpdateEvent<'_>,
    ) -> Result<(), ChannelNameUpdateError> {
        match (self.phase, event) {
            (Phase::Complete | Phase::Halted, _) => Err(ChannelNameUpdateError::TerminalState),
            (
                Phase::Fresh(session),
                ChannelNameUpdateEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    whole_page,
                },
            ) => self.fresh_session(session, id, identity, memory_format, whole_page),
            (Phase::Intent, ChannelNameUpdateEvent::DurableWriteIntent { id }) => {
                if self.intent.is_some() {
                    return Err(ChannelNameUpdateError::UnexpectedEvent);
                }
                self.intent = Some(id);
                self.status = ChannelNameUpdateStatus::PossiblyChanged;
                self.phase = Phase::Readback;
                Ok(())
            }
            (Phase::Readback, ChannelNameUpdateEvent::ImmediateReadback { whole_page }) => {
                self.compare_page(whole_page, true)?;
                self.phase = Phase::Finalize(Session::Apply);
                Ok(())
            }
            (Phase::Finalize(session), ChannelNameUpdateEvent::SessionFinalized { id }) => {
                if self.sessions.last() != Some(&id) {
                    return Err(ChannelNameUpdateError::SessionMismatch);
                }
                self.phase = match session {
                    Session::Apply => Phase::Fresh(Session::Verify),
                    Session::Verify => {
                        self.status = ChannelNameUpdateStatus::VerifiedAcrossSessions;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(ChannelNameUpdateError::UnexpectedEvent),
        }
    }

    fn fresh_session(
        &mut self,
        session: Session,
        id: NonZeroU64,
        identity: &Identity,
        memory_format: u8,
        whole_page: &[u8],
    ) -> Result<(), ChannelNameUpdateError> {
        if self.sessions.contains(&id) {
            return Err(ChannelNameUpdateError::ReusedSession);
        }
        if identity != &self.identity {
            return Err(ChannelNameUpdateError::IdentityMismatch);
        }
        if memory_format != 0 {
            return Err(ChannelNameUpdateError::MemoryFormat {
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

    fn compare_page(&self, bytes: &[u8], desired: bool) -> Result<(), ChannelNameUpdateError> {
        if bytes.len() != PAGE_SIZE {
            return Err(ChannelNameUpdateError::PageLength {
                actual: bytes.len(),
            });
        }
        let expected = if desired {
            &self.desired
        } else {
            &self.original
        };
        if bytes != expected {
            return Err(ChannelNameUpdateError::PageMismatch);
        }
        Ok(())
    }
}

/// A rejected name, preparation precondition, or event transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ChannelNameUpdateError {
    /// The name is empty, oversized, or contains bytes outside printable ASCII.
    #[error("channel names must contain 1 to 16 printable ASCII bytes")]
    InvalidName,
    /// The channel's name address does not resolve to a complete page on the
    /// name-table grid inside the writable global region.
    #[error("channel name address is outside the bounded name-table page scope")]
    UnsupportedPage,
    /// The full identity differs from the pinned target or captured baseline.
    #[error("channel name update requires the exact target and baseline identity")]
    IdentityMismatch,
    /// A complete page is required at preparation and every comparison.
    #[error("channel name update requires a complete 256-byte page, got {actual} bytes")]
    PageLength {
        /// Supplied byte count.
        actual: usize,
    },
    /// The supplied expected name does not exactly match the captured field.
    #[error("expected channel name does not match the captured page")]
    CurrentNameMismatch,
    /// Current and desired names are identical, so no write is needed.
    #[error("the channel already has the requested name; no change is needed")]
    NoChange,
    /// A fresh session supplied an unsupported memory-format byte.
    #[error("channel name update requires memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Observed memory-format byte.
        actual: u8,
    },
    /// At least one byte differs from the exact page required at this stage.
    #[error("channel name whole-page comparison failed")]
    PageMismatch,
    /// The event is not the exact next step of the two-session sequence.
    #[error("channel name-update events are out of order")]
    UnexpectedEvent,
    /// A supposed fresh session reused an earlier connection identifier.
    #[error("channel name-update session ID was reused")]
    ReusedSession,
    /// Finalization does not identify the current session.
    #[error("channel name-update finalization does not match the current session")]
    SessionMismatch,
    /// The completed or permanently halted instance accepts no more events.
    #[error("channel name update is terminal and accepts no further events")]
    TerminalState,
}

#[cfg(test)]
#[path = "channel_name_update_tests.rs"]
mod tests;
