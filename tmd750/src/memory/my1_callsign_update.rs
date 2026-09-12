//! Bounded PM-off MY1 storage updates with explicit, fail-closed evidence.
//!
//! This module performs no I/O. Events attest observations and durable storage;
//! they cannot independently prove connection freshness or physical continuity.

use std::num::NonZeroU64;

use super::{
    FieldCodec, FieldDescriptor, FieldValue, MenuField, SLOT_TERM, StringEncoding, menu_field,
};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page, RadioModel, SlotIndex};

const FIELD_LENGTH: usize = 8;
const FIELD_OFFSET: usize = 8;
const TARGET_ADDRESS: u32 = 331_776;
const CONTROL_ADDRESS: u32 = 323_584;

/// Exact MY1 storage text: one to eight uppercase ASCII letters, digits, or spaces.
///
/// At least one letter or digit is required. Leading, trailing, and interior
/// spaces are preserved. This validates storage syntax only, not callsign
/// ownership, licensing, reflector authentication, or Terminal acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct My1Callsign(String);

impl My1Callsign {
    /// Validate MY1 text without normalization, truncation, or replacement.
    ///
    /// # Errors
    ///
    /// Returns [`My1CallsignUpdateError::InvalidCallsign`] for an empty or
    /// oversized value, missing alphanumeric content, or any unsupported byte.
    /// Lowercase, slashes, Unicode, controls, and embedded NUL are rejected.
    pub fn new(value: &str) -> Result<Self, My1CallsignUpdateError> {
        if value.is_empty()
            || value.len() > FIELD_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ')
            || !value.bytes().any(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(My1CallsignUpdateError::InvalidCallsign);
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact validated bytes as text, including every space.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Conservative result of accepted evidence, not a current-state observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum My1CallsignUpdateStatus {
    /// No intent was accepted; this model has not permitted a write.
    NotWritten,
    /// The sole intent was accepted, but complete independent verification is owed.
    PossiblyChanged,
    /// The desired page matched in both sessions and both lifecycles finalized.
    /// This is not an independently observed power cycle or radio display.
    VerifiedAcrossSessions,
}

/// The two fixed sessions of a normal MY1 storage update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum My1CallsignUpdateSession {
    /// Compare both original pages, journal the sole write, then read it back.
    Apply,
    /// Independently compare both pages without another write.
    Verify,
}

/// Caller-attested evidence for one exact MY1 update, with no unchecked fresh path.
///
/// IDs prevent accidental reuse inside this instance; they do not prove new
/// connections or persistence. Every event is checked against its exact phase.
#[derive(Debug)]
pub enum My1CallsignUpdateEvent<'a> {
    /// Attest a freshly identified MCP session and both complete acknowledged pages.
    FreshSession {
        /// Unique connection identifier, different from every prior session.
        id: NonZeroU64,
        /// Exact identity freshly obtained on this connection before entry.
        identity: &'a Identity,
        /// Fresh memory-format byte at absolute address 10; must be zero.
        memory_format: u8,
        /// Actual fresh pre-entry Gateway observation; must be Off.
        gateway_mode: DvGatewayMode,
        /// Complete unchanged control page, including the PM-Off selector.
        control_page: &'a [u8],
        /// Complete target page expected at this phase, including all other fields.
        whole_page: &'a [u8],
    },
    /// Attest separately obtained authorization and a private, synchronized
    /// journal containing exact identity, original/desired/control pages, and
    /// the sole intent before dispatch. This event does not authorize hardware.
    /// Acceptance marks possible change even if dispatch never occurs.
    DurableWriteIntent {
        /// Identifier of the single synchronized intent, not the connection ID.
        id: NonZeroU64,
    },
    /// Attest a complete acknowledged target-page read immediately after the write.
    ImmediateReadback {
        /// Actual readback, including every byte outside the MY1 text field.
        whole_page: &'a [u8],
    },
    /// Attest E/ACK, original close/drop, a new CAT connection with the supplied
    /// exact identity and Gateway Off, fresh close, complete captures, and
    /// synchronized session evidence. Establish every fact before recording.
    /// Neither a matching tuple nor USB re-enumeration proves a power cycle.
    SessionFinalized {
        /// Identifier accepted for the current fresh MCP session.
        id: NonZeroU64,
        /// Actual identity observed on the separate post-exit CAT connection.
        identity: &'a Identity,
        /// Actual Gateway state on that fresh connection; must be Off.
        gateway_mode: DvGatewayMode,
    },
}

use My1CallsignUpdateSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent,
    Readback,
    Finalize(Session),
    Complete,
    Halted,
}

/// An immutable, compare-before-write MY1 update with two-session verification.
///
/// Only TM-D750 / firmware 1.02 / type `K,2,1`, PM Off, stored and freshly
/// observed Gateway Off, and MY1 selected are admitted. The target and control
/// pages remain immutable except for MY1's eight bytes in the desired image.
/// This API neither changes the generic schema gate nor configures Terminal.
///
/// A normal update leaves the requested text installed after verification.
/// It does not automatically restore, retry, merge, rebase, or repair a page.
/// Failure permanently halts further events while preserving possible change.
#[derive(Debug)]
pub struct My1CallsignUpdate {
    identity: Identity,
    page: Page,
    control_spec: Page,
    original: [u8; PAGE_SIZE],
    desired: [u8; PAGE_SIZE],
    control: [u8; PAGE_SIZE],
    current: Option<My1Callsign>,
    requested: My1Callsign,
    status: My1CallsignUpdateStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intent: Option<NonZeroU64>,
}

impl My1CallsignUpdate {
    /// Resolve the sole target page after validating all target/control descriptors.
    ///
    /// # Errors
    ///
    /// Returns [`My1CallsignUpdateError::UnsupportedDescriptor`] on layout drift.
    pub fn required_page() -> Result<Page, My1CallsignUpdateError> {
        Descriptors::load()?.pages().map(|(target, _)| target)
    }

    /// Resolve the complete immutable control page after all descriptor checks.
    ///
    /// # Errors
    ///
    /// Returns [`My1CallsignUpdateError::UnsupportedDescriptor`] on layout drift.
    pub fn required_control_page() -> Result<Page, My1CallsignUpdateError> {
        Descriptors::load()?.pages().map(|(_, control)| control)
    }

    /// Prepare one persistent text update from complete observed capture pages.
    ///
    /// `expected_current: None` requires exactly eight NUL bytes, not spaces or
    /// erased bytes. A supplied current value requires its exact NUL-padded
    /// encoding. Desired text is always nonempty; a no-op is rejected. The
    /// caller separately establishes capture provenance and write permission.
    ///
    /// # Errors
    ///
    /// Rejects identity or descriptor drift, incomplete pages, active PM or
    /// Gateway, another MY selection, a current-value mismatch, or no change.
    pub fn prepare(
        identity: &Identity,
        target_page: &[u8],
        control_page: &[u8],
        expected_current: Option<&My1Callsign>,
        desired: &My1Callsign,
    ) -> Result<Self, My1CallsignUpdateError> {
        Self::prepare_with_fields(
            identity,
            target_page,
            control_page,
            expected_current,
            desired,
            &Descriptors::load()?,
        )
    }

    fn prepare_with_fields(
        identity: &Identity,
        target_page: &[u8],
        control_page: &[u8],
        expected_current: Option<&My1Callsign>,
        requested: &My1Callsign,
        fields: &Descriptors<'_>,
    ) -> Result<Self, My1CallsignUpdateError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(My1CallsignUpdateError::IdentityMismatch);
        }
        let (page, control_spec) = fields.pages()?;
        let original: [u8; PAGE_SIZE] =
            target_page
                .try_into()
                .map_err(|_| My1CallsignUpdateError::PageLength {
                    actual: target_page.len(),
                })?;
        let control: [u8; PAGE_SIZE] =
            control_page
                .try_into()
                .map_err(|_| My1CallsignUpdateError::ControlPageLength {
                    actual: control_page.len(),
                })?;
        validate_stored_guards(&original, &control)?;
        let expected = expected_current.map_or(Ok([0; FIELD_LENGTH]), |value| {
            encode_callsign(fields.callsign, value)
        })?;
        let range = FIELD_OFFSET..FIELD_OFFSET + FIELD_LENGTH;
        if original.get(range.clone()) != Some(expected.as_slice()) {
            return Err(My1CallsignUpdateError::CurrentCallsignMismatch);
        }
        if expected_current == Some(requested) {
            return Err(My1CallsignUpdateError::NoChange);
        }
        let mut desired = original;
        desired
            .get_mut(range)
            .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?
            .copy_from_slice(&encode_callsign(fields.callsign, requested)?);
        Ok(Self {
            identity: identity.clone(),
            page,
            control_spec,
            original,
            desired,
            control,
            current: expected_current.cloned(),
            requested: requested.clone(),
            status: My1CallsignUpdateStatus::NotWritten,
            phase: Phase::Fresh(Session::Apply),
            sessions: Vec::with_capacity(2),
            intent: None,
        })
    }

    /// Exact captured identity required on every subsequent connection.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// The sole complete MY1 target page.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// The complete immutable control-page address and length.
    #[must_use]
    pub const fn control_page_spec(&self) -> Page {
        self.control_spec
    }

    /// Exact captured target page, retained as evidence, not an automatic rollback.
    #[must_use]
    pub const fn original_page(&self) -> &[u8; PAGE_SIZE] {
        &self.original
    }

    /// Desired target page; only MY1's eight-byte field may differ.
    #[must_use]
    pub const fn desired_page(&self) -> &[u8; PAGE_SIZE] {
        &self.desired
    }

    /// Complete unchanged control-page bytes required in both sessions.
    #[must_use]
    pub const fn control_page(&self) -> &[u8; PAGE_SIZE] {
        &self.control
    }

    /// Matched current text, or `None` for the exactly eight-NUL field.
    #[must_use]
    pub const fn current_callsign(&self) -> Option<&My1Callsign> {
        self.current.as_ref()
    }

    /// The exact requested nonempty MY1 text.
    #[must_use]
    pub const fn desired_callsign(&self) -> &My1Callsign {
        &self.requested
    }

    /// Conservative accepted-evidence status, not an independent radio measurement.
    #[must_use]
    pub const fn status(&self) -> My1CallsignUpdateStatus {
        self.status
    }

    /// Return the next session only while awaiting fresh-session evidence.
    ///
    /// # Errors
    ///
    /// Returns [`My1CallsignUpdateError::TerminalState`] after halt/completion,
    /// or [`My1CallsignUpdateError::UnexpectedEvent`] inside an active session.
    pub const fn next_session(&self) -> Result<Session, My1CallsignUpdateError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Complete | Phase::Halted => Err(My1CallsignUpdateError::TerminalState),
            _ => Err(My1CallsignUpdateError::UnexpectedEvent),
        }
    }

    /// Accept only the exact next evidence and preserve risk on every failure.
    ///
    /// # Errors
    ///
    /// Rejects wrong event order, identity/Gateway/format/page drift, reused
    /// connection IDs, mismatched finalization, or a terminal instance.
    pub fn record(
        &mut self,
        event: My1CallsignUpdateEvent<'_>,
    ) -> Result<(), My1CallsignUpdateError> {
        let result = self.record_inner(event);
        if result.is_err() && self.phase != Phase::Complete {
            self.phase = Phase::Halted;
        }
        result
    }

    /// Permanently halt without cleanup, automatic rollback, or status erasure.
    /// A completed verification remains complete after subsequent misuse.
    pub const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    fn record_inner(
        &mut self,
        event: My1CallsignUpdateEvent<'_>,
    ) -> Result<(), My1CallsignUpdateError> {
        match (self.phase, event) {
            (Phase::Complete | Phase::Halted, _) => Err(My1CallsignUpdateError::TerminalState),
            (
                Phase::Fresh(session),
                My1CallsignUpdateEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    gateway_mode,
                    control_page,
                    whole_page,
                },
            ) => {
                if self.sessions.contains(&id) {
                    return Err(My1CallsignUpdateError::ReusedSession);
                }
                self.compare_identity_gateway(identity, gateway_mode)?;
                if memory_format != 0 {
                    return Err(My1CallsignUpdateError::MemoryFormat {
                        actual: memory_format,
                    });
                }
                self.compare_control(control_page)?;
                self.compare_page(whole_page, session == Session::Verify)?;
                self.sessions.push(id);
                self.phase = match session {
                    Session::Apply => Phase::Intent,
                    Session::Verify => Phase::Finalize(session),
                };
                Ok(())
            }
            (Phase::Intent, My1CallsignUpdateEvent::DurableWriteIntent { id }) => {
                if self.intent.is_some() {
                    return Err(My1CallsignUpdateError::UnexpectedEvent);
                }
                self.intent = Some(id);
                self.status = My1CallsignUpdateStatus::PossiblyChanged;
                self.phase = Phase::Readback;
                Ok(())
            }
            (Phase::Readback, My1CallsignUpdateEvent::ImmediateReadback { whole_page }) => {
                self.compare_page(whole_page, true)?;
                self.phase = Phase::Finalize(Session::Apply);
                Ok(())
            }
            (
                Phase::Finalize(session),
                My1CallsignUpdateEvent::SessionFinalized {
                    id,
                    identity,
                    gateway_mode,
                },
            ) => {
                if self.sessions.last() != Some(&id) {
                    return Err(My1CallsignUpdateError::SessionMismatch);
                }
                self.compare_identity_gateway(identity, gateway_mode)?;
                self.phase = match session {
                    Session::Apply => Phase::Fresh(Session::Verify),
                    Session::Verify => {
                        self.status = My1CallsignUpdateStatus::VerifiedAcrossSessions;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(My1CallsignUpdateError::UnexpectedEvent),
        }
    }

    fn compare_identity_gateway(
        &self,
        identity: &Identity,
        gateway: DvGatewayMode,
    ) -> Result<(), My1CallsignUpdateError> {
        if identity != &self.identity {
            return Err(My1CallsignUpdateError::IdentityMismatch);
        }
        if gateway != DvGatewayMode::Off {
            return Err(My1CallsignUpdateError::GatewayMode { actual: gateway });
        }
        Ok(())
    }

    fn compare_page(&self, bytes: &[u8], desired: bool) -> Result<(), My1CallsignUpdateError> {
        if bytes.len() != PAGE_SIZE {
            return Err(My1CallsignUpdateError::PageLength {
                actual: bytes.len(),
            });
        }
        let expected = if desired {
            &self.desired
        } else {
            &self.original
        };
        if bytes != expected {
            return Err(My1CallsignUpdateError::PageMismatch);
        }
        Ok(())
    }

    fn compare_control(&self, bytes: &[u8]) -> Result<(), My1CallsignUpdateError> {
        if bytes.len() != PAGE_SIZE {
            return Err(My1CallsignUpdateError::ControlPageLength {
                actual: bytes.len(),
            });
        }
        if bytes != self.control {
            return Err(My1CallsignUpdateError::ControlPageMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FieldPin {
    descriptor: FieldDescriptor,
    menu: &'static str,
    options: Option<usize>,
    page_address: u32,
}

const CALLSIGN: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway",
        331_784,
        &[SLOT_TERM],
        FieldCodec::FixedString {
            len: FIELD_LENGTH,
            encoding: StringEncoding::Utf8,
            padding: 0,
        },
    ),
    menu: "dv",
    options: None,
    page_address: TARGET_ADDRESS,
};
const GATEWAY: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.DvGatewayModeDvGateway",
        TARGET_ADDRESS,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 2 },
    ),
    menu: "dv",
    options: Some(3),
    page_address: TARGET_ADDRESS,
};
const MY_SELECTION: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.MyCallsignSelectDvGateway",
        331_777,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 255 },
    ),
    menu: "dv",
    options: None,
    page_address: TARGET_ADDRESS,
};
const PM_SELECTION: FieldPin = FieldPin {
    descriptor: FieldDescriptor::new("pm.PmSelect", 323_593, FieldCodec::Byte { min: 0, max: 6 }),
    menu: "pm",
    options: Some(7),
    page_address: CONTROL_ADDRESS,
};

struct Descriptors<'a> {
    callsign: &'a MenuField,
    gateway: &'a MenuField,
    my_selection: &'a MenuField,
    pm_selection: &'a MenuField,
}

impl Descriptors<'static> {
    fn load() -> Result<Self, My1CallsignUpdateError> {
        let resolve = |pin: FieldPin| {
            menu_field(pin.descriptor.name).ok_or(My1CallsignUpdateError::UnsupportedDescriptor)
        };
        Ok(Self {
            callsign: resolve(CALLSIGN)?,
            gateway: resolve(GATEWAY)?,
            my_selection: resolve(MY_SELECTION)?,
            pm_selection: resolve(PM_SELECTION)?,
        })
    }
}

impl Descriptors<'_> {
    fn pages(&self) -> Result<(Page, Page), My1CallsignUpdateError> {
        let target = supported_page(self.callsign, CALLSIGN)?;
        if supported_page(self.gateway, GATEWAY)? != target
            || supported_page(self.my_selection, MY_SELECTION)? != target
        {
            return Err(My1CallsignUpdateError::UnsupportedDescriptor);
        }
        Ok((target, supported_page(self.pm_selection, PM_SELECTION)?))
    }
}

fn supported_page(field: &MenuField, pin: FieldPin) -> Result<Page, My1CallsignUpdateError> {
    let options_match = pin.options.map_or_else(
        || field.enum_type.is_none() && field.options.is_empty(),
        |count| {
            field.enum_type.is_some()
                && field.options.len() == count
                && field
                    .options
                    .iter()
                    .enumerate()
                    .all(|(index, option)| usize::try_from(option.raw).ok() == Some(index))
        },
    );
    if field.descriptor != pin.descriptor
        || field.menu != pin.menu
        || !options_match
        || !field.allowed_values.is_empty()
        || field.storage_transform.is_some()
        || field.is_blob
    {
        return Err(My1CallsignUpdateError::UnsupportedDescriptor);
    }
    let slot = if pin.descriptor.is_per_slot() {
        Some(SlotIndex::new(0).map_err(|_| My1CallsignUpdateError::UnsupportedDescriptor)?)
    } else {
        None
    };
    let address = field
        .descriptor
        .address(slot)
        .map_err(|_| My1CallsignUpdateError::UnsupportedDescriptor)?;
    let page = writable_page_for(address)
        .filter(|page| page.address().as_u32() == pin.page_address && page.len() == PAGE_SIZE)
        .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?;
    let length = u32::try_from(field.descriptor.codec.encoded_len())
        .map_err(|_| My1CallsignUpdateError::UnsupportedDescriptor)?;
    if address
        .as_u32()
        .checked_add(length)
        .is_none_or(|end| end > page.end())
    {
        return Err(My1CallsignUpdateError::UnsupportedDescriptor);
    }
    Ok(page)
}

fn validate_stored_guards(
    target: &[u8; PAGE_SIZE],
    control: &[u8; PAGE_SIZE],
) -> Result<(), My1CallsignUpdateError> {
    let pm = *control
        .get(9)
        .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?;
    if pm != 0 {
        return Err(My1CallsignUpdateError::PmSelection { actual: pm });
    }
    let gateway = DvGatewayMode::from(
        *target
            .first()
            .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?,
    );
    if gateway != DvGatewayMode::Off {
        return Err(My1CallsignUpdateError::GatewayMode { actual: gateway });
    }
    let selection = *target
        .get(1)
        .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?;
    if selection != 0 {
        return Err(My1CallsignUpdateError::MySelection { actual: selection });
    }
    Ok(())
}

fn encode_callsign(
    field: &MenuField,
    callsign: &My1Callsign,
) -> Result<[u8; FIELD_LENGTH], My1CallsignUpdateError> {
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(callsign.as_str()))
        .map_err(|_| My1CallsignUpdateError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(My1CallsignUpdateError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    bytes
        .get_mut(..callsign.as_str().len())
        .ok_or(My1CallsignUpdateError::UnsupportedDescriptor)?
        .copy_from_slice(callsign.as_str().as_bytes());
    for (index, (offset, mask, value)) in encoded.into_iter().enumerate() {
        if index != offset || mask != u8::MAX || bytes.get(offset) != Some(&value) {
            return Err(My1CallsignUpdateError::UnsupportedDescriptor);
        }
    }
    Ok(bytes)
}

/// Rejected storage syntax, immutable preparation, or evidence sequence.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum My1CallsignUpdateError {
    /// Text is outside the bounded uppercase-alphanumeric-and-space syntax.
    #[error(
        "MY1 requires 1 to 8 uppercase ASCII letters, digits, or spaces, including a letter or digit"
    )]
    InvalidCallsign,
    /// A generated field or canonical page no longer matches the fixed scope.
    #[error("generated MY1 or guard descriptor is outside the bounded update scope")]
    UnsupportedDescriptor,
    /// Complete identity differs from the qualified target or captured baseline.
    #[error("MY1 update requires the exact target and captured baseline identity")]
    IdentityMismatch,
    /// The target page is incomplete or oversized.
    #[error("MY1 update requires a complete 256-byte target page, got {actual} bytes")]
    PageLength {
        /// Supplied target-page byte count.
        actual: usize,
    },
    /// The immutable control page is incomplete or oversized.
    #[error("MY1 update requires a complete 256-byte control page, got {actual} bytes")]
    ControlPageLength {
        /// Supplied control-page byte count.
        actual: usize,
    },
    /// Captured current text does not exactly match the expected NUL-padded field.
    #[error("expected MY1 callsign does not exactly match the captured page")]
    CurrentCallsignMismatch,
    /// Current and desired values are identical; no write is permitted.
    #[error("MY1 already contains the requested callsign; no change is needed")]
    NoChange,
    /// Only PM Off is admitted; other slots are never selected by this update.
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
    /// The memory-format byte is unsupported.
    #[error("MY1 update requires memory-format byte zero, got {actual}")]
    MemoryFormat {
        /// Actual memory-format byte.
        actual: u8,
    },
    /// The complete target page differs from the exact expected image.
    #[error("MY1 whole-page comparison failed; rebase and stale rollback are forbidden")]
    PageMismatch,
    /// Any byte of the immutable control page differs.
    #[error("MY1 immutable control-page comparison failed")]
    ControlPageMismatch,
    /// An event arrived outside its permitted evidence phase.
    #[error("MY1 update evidence is out of order")]
    UnexpectedEvent,
    /// A claimed fresh session reused a prior connection identifier.
    #[error("MY1 update session ID was reused")]
    ReusedSession,
    /// Finalization does not name the current session.
    #[error("MY1 update finalization does not match the current session")]
    SessionMismatch,
    /// A halted or completed transaction accepts no further evidence.
    #[error("MY1 update is terminal and accepts no further evidence")]
    TerminalState,
}

#[cfg(test)]
#[path = "my1_callsign_update_tests.rs"]
mod tests;
