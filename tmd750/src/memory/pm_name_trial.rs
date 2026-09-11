//! Pure evidence sequencing for an explicitly unqualified, fixed PM1 trial.
//!
//! Nothing here opens a radio, persists a journal, authorizes a live write, or
//! changes the independent schema-target gate. Events are caller attestations,
//! not independently verified hardware or filesystem facts.

use std::num::NonZeroU64;

use super::{FieldCodec, FieldValue, MenuField, StringEncoding, menu_field};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{PAGE_SIZE, Page, RadioModel};

const FIELD_NAME: &str = "pm.PmName1";
const FIELD_ADDRESS: u32 = 323_594;
const FIELD_LENGTH: usize = 16;
const PAGE_ADDRESS: u32 = 323_584;

/// Conservative restoration obligation represented by the recorded evidence.
///
/// This is modeled state, not an independent observation of the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialStatus {
    /// No write intent has been recorded; this engine has not permitted a
    /// transition representing a possibly dispatched write.
    NotWritten,
    /// A write intent was recorded. Original-page persistence and final
    /// session cleanup have not both been attested; restoration remains owed.
    PossiblyChanged,
    /// Three distinct sessions supplied every required comparison and cleanup
    /// attestation, including original-page persistence in the third session.
    RestorationVerified,
}

/// The only two write intents in the fixed trial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialWrite {
    /// Replace PM1's name with [`PmNameTrial::TEMPORARY_NAME`].
    Rename,
    /// Restore the exact original page, not a newly synthesized name.
    Restore,
}

/// Caller-supplied evidence, accepted only in the fixed three-session order.
///
/// IDs catch accidental reuse within this instance; they cannot establish that
/// connections are fresh, storage is durable, or the physical unit is unchanged.
#[derive(Debug)]
pub enum PmNameTrialEvent<'a> {
    /// Attest a newly opened, independently identified MCP session and its
    /// completed reads of memory-format byte 10 and the entire target page.
    ///
    /// For sessions two and three, the caller must have established the prior
    /// exit/re-entry boundary and obtained new reads, not replayed a capture.
    FreshSession {
        /// Caller-assigned connection identity, never reused within the trial.
        id: NonZeroU64,
        /// Newly obtained complete CAT identity from this connection.
        identity: &'a Identity,
        /// Freshly read memory-format byte at absolute address 10; must be zero.
        memory_format: u8,
        /// Complete, acknowledged bytes at [`PmNameTrial::page`].
        whole_page: &'a [u8],
    },
    /// Attest that separate operator approval applies and a private journal
    /// containing identity, original and expected bytes, and this exact intent
    /// has been successfully synchronized to durable storage before any W.
    ///
    /// Acceptance immediately records a possible change, even if dispatch
    /// subsequently fails or never occurs. This is not write authorization.
    DurableWriteIntent {
        /// Distinct durable journal record identity for this write intent.
        id: NonZeroU64,
        /// The exact next intent: rename in session one, restore in session two.
        write: PmNameTrialWrite,
    },
    /// Attest a complete, acknowledged immediate readback after the intended
    /// write. The full page must match, including every unrelated byte.
    ImmediateReadback {
        /// Newly read bytes at [`PmNameTrial::page`].
        whole_page: &'a [u8],
    },
    /// Attest successful E/ACK, original close/drop, a bounded fresh CAT proof
    /// matching the entire identity, fresh close, complete captures, and durable
    /// recording of this session's evidence and current restoration obligation.
    ///
    /// The caller establishes each fact. A matching tuple or session ID alone
    /// proves none of them and does not establish physical-unit continuity.
    SessionFinalized {
        /// The session ID supplied by the corresponding `FreshSession` event.
        id: NonZeroU64,
    },
}

/// The next fixed session of the three-session persistence experiment.
///
/// A session value describes sequence only, never permission to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmNameTrialSession {
    /// Compare the original page, then perform the fixed temporary rename.
    Rename,
    /// Prove the changed page persisted before restoring the original page.
    Restore,
    /// Read only and prove the original page persisted after restoration.
    VerifyRestoration,
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

/// An offline, unqualified PM1 rename/restore evidence state machine.
///
/// The fixed scope is global PM1 on TM-D750 / firmware 1.02 / type `K,2,1`.
/// Successful construction or event recording never qualifies that firmware or
/// supplies a live writer. The generic exact schema-target gate is independent
/// and unchanged. The caller must separately establish operator approval,
/// physical continuity, fresh connections, protocol framing, capture completion,
/// durable storage, and MCP exit/re-entry boundaries before reporting hardware
/// success. The model does not establish an independently observed full-radio
/// reboot or persistence across a power cycle.
///
/// Sequence: fresh original-page comparison, durable rename intent, immediate
/// changed-page readback, finalized session; a distinct re-entered session with
/// a changed-page comparison, durable restore intent, immediate original-page
/// readback, finalized session; a third distinct re-entered read-only session
/// with an original-page comparison and finalized cleanup.
#[derive(Debug)]
pub struct PmNameTrial {
    identity: Identity,
    page: Page,
    original: [u8; PAGE_SIZE],
    expected: [u8; PAGE_SIZE],
    status: PmNameTrialStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intents: Vec<NonZeroU64>,
}

impl PmNameTrial {
    /// The sole temporary label used by this trial; never caller-selectable.
    pub const TEMPORARY_NAME: &str = "PC TEXT TEST";

    /// Resolve the sole canonical page before extracting it from a sparse
    /// capture. This performs the same generated-descriptor shape checks as
    /// preparation and does not qualify that layout for live writes.
    ///
    /// # Errors
    ///
    /// Returns [`PmNameTrialError::UnsupportedDescriptor`] when the generated
    /// PM1 field or canonical page no longer has the expected bounded shape.
    pub fn required_page() -> Result<Page, PmNameTrialError> {
        let field = menu_field(FIELD_NAME).ok_or(PmNameTrialError::UnsupportedDescriptor)?;
        supported_page(field)
    }

    /// Prepare immutable pages for an explicitly unqualified offline trial.
    ///
    /// `baseline` must be the exact complete canonical target page from a
    /// validated capture, never an erased-gap reconstruction. The independent
    /// `operator_confirmed_pm1_name` must come from the display, not be copied
    /// from that capture. This method cannot verify those sources or obtain
    /// approval. It accepts nonempty printable ASCII up to sixteen bytes and
    /// requires an exact NUL-padded match to the captured name.
    ///
    /// # Errors
    ///
    /// Rejects any target, descriptor, page length, name, or display mismatch;
    /// also rejects a name already equal to the fixed temporary label.
    pub fn prepare_unqualified_offline(
        identity: &Identity,
        baseline: &[u8],
        operator_confirmed_pm1_name: &str,
    ) -> Result<Self, PmNameTrialError> {
        let field = menu_field(FIELD_NAME).ok_or(PmNameTrialError::UnsupportedDescriptor)?;
        Self::prepare_with_field(identity, baseline, operator_confirmed_pm1_name, field)
    }

    fn prepare_with_field(
        identity: &Identity,
        baseline: &[u8],
        confirmed_name: &str,
        field: &MenuField,
    ) -> Result<Self, PmNameTrialError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(PmNameTrialError::IdentityMismatch);
        }
        let page = supported_page(field)?;
        let original: [u8; PAGE_SIZE] =
            baseline
                .try_into()
                .map_err(|_| PmNameTrialError::PageLength {
                    actual: baseline.len(),
                })?;
        let confirmed = encode_name(field, confirmed_name)?;
        let offset = field
            .descriptor
            .address(None)
            .map_err(|_| PmNameTrialError::UnsupportedDescriptor)?
            .as_usize()
            - page.address().as_usize();
        let range = offset..offset + FIELD_LENGTH;
        if original.get(range.clone()) != Some(confirmed.as_slice()) {
            return Err(PmNameTrialError::DisplayNameMismatch);
        }
        if confirmed_name == Self::TEMPORARY_NAME {
            return Err(PmNameTrialError::AlreadyTemporaryName);
        }
        let mut expected = original;
        expected
            .get_mut(range.clone())
            .ok_or(PmNameTrialError::UnsupportedDescriptor)?
            .copy_from_slice(&encode_name(field, Self::TEMPORARY_NAME)?);
        if original
            .iter()
            .zip(&expected)
            .enumerate()
            .any(|(index, (before, after))| before != after && !range.contains(&index))
        {
            return Err(PmNameTrialError::UnsupportedDescriptor);
        }
        Ok(Self {
            identity: identity.clone(),
            page,
            original,
            expected,
            status: PmNameTrialStatus::NotWritten,
            phase: Phase::Fresh(Session::Rename),
            sessions: Vec::with_capacity(3),
            intents: Vec::with_capacity(2),
        })
    }

    /// The single canonical target page, resolved from the generated descriptor.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Exact baseline identity, to retain with both immutable page images.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Describe the next session only while awaiting a fresh-session event.
    ///
    /// This is sequencing information, not a live-write authorization or proof
    /// that a connection is available, fresh, or safe to use.
    ///
    /// # Errors
    ///
    /// Returns [`PmNameTrialError::TerminalState`] after completion or halt,
    /// otherwise [`PmNameTrialError::UnexpectedEvent`] during a session.
    pub const fn next_session(&self) -> Result<PmNameTrialSession, PmNameTrialError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(PmNameTrialError::TerminalState),
            _ => Err(PmNameTrialError::UnexpectedEvent),
        }
    }

    /// Exact immutable captured page to retain in the durable recovery record.
    #[must_use]
    pub const fn original_page(&self) -> &[u8; PAGE_SIZE] {
        &self.original
    }

    /// Exact immutable temporary page, differing only within the PM1 name.
    #[must_use]
    pub const fn expected_page(&self) -> &[u8; PAGE_SIZE] {
        &self.expected
    }

    /// Conservative modeled restoration obligation, not a write-ready predicate.
    #[must_use]
    pub const fn status(&self) -> PmNameTrialStatus {
        self.status
    }

    /// Validate and record the exact next caller-attested evidence.
    ///
    /// An accepted durable intent changes status to `PossiblyChanged` before
    /// dispatch could occur. No error can erase that obligation. Any error
    /// permanently halts further event acceptance; mismatches must never be
    /// rebased into this transaction or used to justify a stale restore.
    ///
    /// # Errors
    ///
    /// Rejects wrong order, repeated identities of sessions or journal records,
    /// target/version/page mismatches, and every event after halt or completion.
    pub fn record(&mut self, event: PmNameTrialEvent<'_>) -> Result<(), PmNameTrialError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    /// Permanently stop evidence acceptance after uncertain framing, failed
    /// capture/storage, lost continuity, or another unrecoverable evidence gap.
    ///
    /// This does not send cleanup, cancel I/O, or restore anything. A pending
    /// cancellation alone is not a reason to abandon safe cleanup when framing
    /// remains known. Once an intent was recorded, `PossiblyChanged` survives
    /// halt. A previously completed restoration proof remains completed.
    pub const fn halt(&mut self) {
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

fn supported_page(field: &MenuField) -> Result<Page, PmNameTrialError> {
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
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    field
        .descriptor
        .address(None)
        .ok()
        .and_then(writable_page_for)
        .filter(|page| page.address().as_u32() == PAGE_ADDRESS && page.len() == PAGE_SIZE)
        .ok_or(PmNameTrialError::UnsupportedDescriptor)
}

fn encode_name(field: &MenuField, name: &str) -> Result<[u8; FIELD_LENGTH], PmNameTrialError> {
    if name.is_empty()
        || name.len() > FIELD_LENGTH
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return Err(PmNameTrialError::InvalidName);
    }
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(name))
        .map_err(|_| PmNameTrialError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    for (index, (offset, mask, value)) in encoded.into_iter().enumerate() {
        if index != offset || mask != u8::MAX {
            return Err(PmNameTrialError::UnsupportedDescriptor);
        }
        *bytes
            .get_mut(offset)
            .ok_or(PmNameTrialError::UnsupportedDescriptor)? = value;
    }
    Ok(bytes)
}

/// A failed offline precondition or evidence transition; never a rollback proof.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PmNameTrialError {
    /// The generated field or canonical page no longer has the pinned shape.
    #[error("generated PM1 descriptor is outside this fixed offline trial")]
    UnsupportedDescriptor,
    /// The supplied full identity does not match the fixed target or baseline.
    #[error("PM1 trial identity does not match the fixed target and baseline")]
    IdentityMismatch,
    /// The entire canonical page is required at every comparison.
    #[error("PM1 trial requires a complete 256-byte page, got {actual} bytes")]
    PageLength {
        /// Supplied byte count.
        actual: usize,
    },
    /// The independent name was empty, oversized, or not printable ASCII.
    #[error("PM1 trial names must contain 1 to 16 printable ASCII bytes")]
    InvalidName,
    /// Display text, encoded exactly, does not match the captured field.
    #[error("independently confirmed PM1 name does not match the captured page")]
    DisplayNameMismatch,
    /// A no-op rename cannot establish the proposed change's persistence.
    #[error("PM1 already has the fixed trial name")]
    AlreadyTemporaryName,
    /// A fresh session supplied an unsupported memory-format byte.
    #[error("PM1 trial requires memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Observed memory-format byte.
        actual: u8,
    },
    /// Any byte differs from the exact page required at this stage.
    #[error("PM1 trial whole-page comparison failed; stale restoration is forbidden")]
    PageMismatch,
    /// The evidence is not the exact next event in the three-session sequence.
    #[error("PM1 trial evidence is out of order")]
    UnexpectedEvent,
    /// A supposed fresh session reused a previously supplied connection ID.
    #[error("PM1 trial session ID was reused")]
    ReusedSession,
    /// The second intent reused the first intent's durable journal record ID.
    #[error("PM1 trial durable intent ID was reused")]
    ReusedWriteIntent,
    /// Finalization does not identify the current session.
    #[error("PM1 trial finalization does not match the current session")]
    SessionMismatch,
    /// The instance is completed or permanently halted; no more events apply.
    #[error("PM1 trial is terminal and accepts no further evidence")]
    TerminalState,
}

#[cfg(test)]
#[path = "pm_name_trial_tests.rs"]
mod tests;
