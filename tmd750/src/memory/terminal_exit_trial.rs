//! Pure preparation and fail-closed evidence sequencing for one Terminal exit.
//!
//! Caller events are attestations, not independent proof of hardware state,
//! connection freshness, operator approval, or durable evidence.

use std::num::NonZeroU64;

use super::{FieldCodec, FieldDescriptor, MenuField, SLOT_TERM, menu_field};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page, RadioModel, SlotIndex};

const TARGET_PAGE_ADDRESS: u32 = 331_776;
const CONTROL_PAGE_ADDRESS: u32 = 323_584;
const ROUTING_PAGE_ADDRESS: u32 = 328_960;

#[derive(Clone, Copy)]
struct FieldPin {
    descriptor: FieldDescriptor,
    menu: &'static str,
    options: usize,
    page_address: u32,
}

const GATEWAY: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.DvGatewayModeDvGateway",
        331_776,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 2 },
    ),
    menu: "dv",
    options: 3,
    page_address: TARGET_PAGE_ADDRESS,
};

const SUBTYPE: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.SelectTerminalMode",
        331_778,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 1 },
    ),
    menu: "dv",
    options: 2,
    page_address: TARGET_PAGE_ADDRESS,
};

const PM_SELECTION: FieldPin = FieldPin {
    descriptor: FieldDescriptor::new("pm.PmSelect", 323_593, FieldCodec::Byte { min: 0, max: 6 }),
    menu: "pm",
    options: 7,
    page_address: CONTROL_PAGE_ADDRESS,
};

const USB_FUNCTION: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "radio.UsbFunction",
        329_031,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 1 },
    ),
    menu: "radio",
    options: 2,
    page_address: ROUTING_PAGE_ADDRESS,
};

const GATEWAY_ROUTE: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "radio.DvGatewayInterface",
        329_037,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 2 },
    ),
    menu: "radio",
    options: 3,
    page_address: ROUTING_PAGE_ADDRESS,
};

struct Descriptors<'a> {
    gateway: &'a MenuField,
    subtype: &'a MenuField,
    pm_selection: &'a MenuField,
    usb_function: &'a MenuField,
    gateway_route: &'a MenuField,
}

impl Descriptors<'static> {
    fn load() -> Result<Self, TerminalExitTrialError> {
        let resolve = |pin: FieldPin| {
            menu_field(pin.descriptor.name).ok_or(TerminalExitTrialError::UnsupportedDescriptor)
        };
        Ok(Self {
            gateway: resolve(GATEWAY)?,
            subtype: resolve(SUBTYPE)?,
            pm_selection: resolve(PM_SELECTION)?,
            usb_function: resolve(USB_FUNCTION)?,
            gateway_route: resolve(GATEWAY_ROUTE)?,
        })
    }
}

impl Descriptors<'_> {
    fn pages(&self) -> Result<(Page, Page, Page), TerminalExitTrialError> {
        let target = supported_page(self.gateway, GATEWAY)?;
        let control = supported_page(self.pm_selection, PM_SELECTION)?;
        let routing = supported_page(self.usb_function, USB_FUNCTION)?;
        if supported_page(self.subtype, SUBTYPE)? != target
            || supported_page(self.gateway_route, GATEWAY_ROUTE)? != routing
        {
            return Err(TerminalExitTrialError::UnsupportedDescriptor);
        }
        Ok((target, control, routing))
    }
}

/// The two separately identified sessions required for one fixed exit attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExitTrialSession {
    /// Match the entire expected active page, write Off once, and read it back.
    Apply,
    /// Independently read the complete Off page without another write.
    Verify,
}

/// Conservative outcome modeled from accepted caller evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExitTrialStatus {
    /// No durable write intent has been accepted by this instance.
    NotWritten,
    /// The sole intent was accepted; the write may have reached the radio.
    PossiblyChanged,
    /// Complete Off pages matched immediately and in a distinct session, and
    /// both finalized lifecycles included fresh matching CAT identity and Off.
    /// This does not establish a power cycle or hardware qualification.
    OffVerifiedAcrossSessions,
}

/// Evidence accepted only in the fixed apply-then-verify sequence.
///
/// Callers must independently establish every attested fact. This pure API
/// cannot authenticate a capture, synchronize a journal, or authorize hardware.
#[derive(Debug)]
pub enum TerminalExitTrialEvent<'a> {
    /// Attest one fresh identified connection, a pre-entry Gateway query, and
    /// complete acknowledged reads of format, control, routing, and target.
    FreshSession {
        /// Connection identifier, distinct from the other session's identifier.
        id: NonZeroU64,
        /// Complete CAT identity freshly read before this session's MCP entry.
        identity: &'a Identity,
        /// Fresh byte at address 10; only zero is admitted.
        memory_format: u8,
        /// Fresh pre-entry query: Terminal for apply, Off for verification.
        gateway_mode: DvGatewayMode,
        /// Complete fresh target page; never synthesized from the Off baseline.
        whole_page: &'a [u8],
        /// Complete fresh immutable PM control page.
        control_page: &'a [u8],
        /// Complete fresh immutable interface-routing page.
        routing_page: &'a [u8],
    },
    /// Attest separately obtained exact-scope approval and a private journal
    /// containing identity, all immutable pages, the complete freshly observed
    /// active page, and the sole intent, synchronized before any write dispatch.
    /// Acceptance conservatively records possible change even if dispatch fails.
    DurableWriteIntent {
        /// Nonzero identifier of the sole durable intent, not a connection ID.
        id: NonZeroU64,
    },
    /// Attest a complete acknowledged target-page read immediately after writing.
    ImmediateReadback {
        /// Fresh full page, required to match the immutable Off page exactly.
        whole_page: &'a [u8],
    },
    /// Attest E/ACK, original close/drop, a new matching CAT identification and
    /// read-only Gateway Off query, fresh close, complete captures, and durable
    /// session evidence. No command may follow E/ACK on the original handle.
    ///
    /// Neither this event nor a matching identity proves physical continuity.
    SessionFinalized {
        /// Identifier from the current session's accepted fresh observation.
        id: NonZeroU64,
        /// Complete identity newly read after retiring the original handle.
        identity: &'a Identity,
        /// Newly read post-exit Gateway mode; only Off is accepted.
        gateway_mode: DvGatewayMode,
    },
}

use TerminalExitTrialSession as Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh(Session),
    Intent,
    Readback,
    Finalize(Session),
    Complete,
    Halted,
}

/// One explicitly unqualified Terminal-to-Off experiment with immutable scope.
///
/// Preparation admits only TM-D750 / firmware 1.02 / type `K,2,1`, PM Off,
/// Reflector subtype, COM+AF USB, and panel USB Gateway routing. The complete
/// captured Off page is immutable. The active expectation changes only that
/// page's Gateway byte from zero to two; it is synthetic until a complete fresh
/// active-state read matches. Callsigns, routing, PM selection, and every other
/// byte are preserved; no text value, slot, page, or replacement is selectable.
///
/// The sole allowed modeled write installs the exact Off page. Immediate
/// readback and a separate read-only session must each match every byte. Both
/// finalized lifecycles require new matching CAT identity and Gateway Off.
/// Errors permanently halt progress without erasing possible change. There is
/// no automatic rollback, retry, rebase, RF operation, or generic schema-gate
/// override. The caller must separately establish approval and hardware safety.
#[derive(Debug)]
pub struct TerminalExitTrial {
    identity: Identity,
    page: Page,
    control_spec: Page,
    routing_spec: Page,
    off: [u8; PAGE_SIZE],
    expected_active: [u8; PAGE_SIZE],
    control: [u8; PAGE_SIZE],
    routing: [u8; PAGE_SIZE],
    status: TerminalExitTrialStatus,
    phase: Phase,
    sessions: Vec<NonZeroU64>,
    intent: Option<NonZeroU64>,
}

impl TerminalExitTrial {
    /// Resolve the sole complete target page after checking every descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalExitTrialError::UnsupportedDescriptor`] on shape drift.
    pub fn required_page() -> Result<Page, TerminalExitTrialError> {
        Descriptors::load()?.pages().map(|(target, _, _)| target)
    }

    /// Resolve the complete immutable control page after checking all fields.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalExitTrialError::UnsupportedDescriptor`] on shape drift.
    pub fn required_control_page() -> Result<Page, TerminalExitTrialError> {
        Descriptors::load()?.pages().map(|(_, control, _)| control)
    }

    /// Resolve the complete immutable routing page after checking all fields.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalExitTrialError::UnsupportedDescriptor`] on shape drift.
    pub fn required_routing_page() -> Result<Page, TerminalExitTrialError> {
        Descriptors::load()?.pages().map(|(_, _, routing)| routing)
    }

    /// Prepare from three complete captured canonical pages with Gateway Off.
    ///
    /// Pages must come from validated capture coverage, never filled gaps. This
    /// method does not qualify an active-state page or establish write approval.
    ///
    /// # Errors
    ///
    /// Rejects unsupported identity, descriptor drift, incomplete pages, active
    /// PM or Gateway, non-Reflector subtype, or different USB function or route.
    pub fn prepare_unqualified_offline(
        identity: &Identity,
        off_page: &[u8],
        control_page: &[u8],
        routing_page: &[u8],
    ) -> Result<Self, TerminalExitTrialError> {
        Self::prepare_with_fields(
            identity,
            off_page,
            control_page,
            routing_page,
            &Descriptors::load()?,
        )
    }

    fn prepare_with_fields(
        identity: &Identity,
        off_page: &[u8],
        control_page: &[u8],
        routing_page: &[u8],
        fields: &Descriptors<'_>,
    ) -> Result<Self, TerminalExitTrialError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(TerminalExitTrialError::IdentityMismatch);
        }
        let (page, control_spec, routing_spec) = fields.pages()?;
        let off: [u8; PAGE_SIZE] =
            off_page
                .try_into()
                .map_err(|_| TerminalExitTrialError::PageLength {
                    actual: off_page.len(),
                })?;
        let control: [u8; PAGE_SIZE] =
            control_page
                .try_into()
                .map_err(|_| TerminalExitTrialError::ControlPageLength {
                    actual: control_page.len(),
                })?;
        let routing: [u8; PAGE_SIZE] =
            routing_page
                .try_into()
                .map_err(|_| TerminalExitTrialError::RoutingPageLength {
                    actual: routing_page.len(),
                })?;
        validate_stored_guards(&off, &control, &routing)?;
        let mut expected_active = off;
        *expected_active
            .first_mut()
            .ok_or(TerminalExitTrialError::UnsupportedDescriptor)? = 2;
        Ok(Self {
            identity: identity.clone(),
            page,
            control_spec,
            routing_spec,
            off,
            expected_active,
            control,
            routing,
            status: TerminalExitTrialStatus::NotWritten,
            phase: Phase::Fresh(Session::Apply),
            sessions: Vec::with_capacity(2),
            intent: None,
        })
    }

    /// Exact immutable complete identity retained with all captured pages.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Sole fixed target-page specification.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Immutable captured Off page, also the sole desired write payload.
    #[must_use]
    pub const fn off_page(&self) -> &[u8; PAGE_SIZE] {
        &self.off
    }

    /// Synthetic active expectation, not an observed active-state before-image.
    #[must_use]
    pub const fn expected_active_page(&self) -> &[u8; PAGE_SIZE] {
        &self.expected_active
    }

    /// Complete captured immutable control page; never a write target.
    #[must_use]
    pub const fn control_page(&self) -> &[u8; PAGE_SIZE] {
        &self.control
    }

    /// Complete captured immutable routing page; never a write target.
    #[must_use]
    pub const fn routing_page(&self) -> &[u8; PAGE_SIZE] {
        &self.routing
    }

    /// Exact control-page address and length.
    #[must_use]
    pub const fn control_page_spec(&self) -> Page {
        self.control_spec
    }

    /// Exact routing-page address and length.
    #[must_use]
    pub const fn routing_page_spec(&self) -> Page {
        self.routing_spec
    }

    /// Conservative modeled outcome, not independent hardware-state evidence.
    #[must_use]
    pub const fn status(&self) -> TerminalExitTrialStatus {
        self.status
    }

    /// Whether an error or explicit halt permanently stopped this instance.
    #[must_use]
    pub const fn is_halted(&self) -> bool {
        matches!(self.phase, Phase::Halted)
    }

    /// Compare a newly obtained complete identity with the immutable baseline.
    /// This read-only check does not advance or halt the evidence sequence.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalExitTrialError::IdentityMismatch`] on any difference.
    pub fn validate_identity(&self, identity: &Identity) -> Result<(), TerminalExitTrialError> {
        if identity == &self.identity {
            Ok(())
        } else {
            Err(TerminalExitTrialError::IdentityMismatch)
        }
    }

    /// Next fixed session only while waiting for fresh connection evidence.
    ///
    /// # Errors
    ///
    /// Returns [`TerminalExitTrialError::TerminalState`] after halt/completion,
    /// or [`TerminalExitTrialError::UnexpectedEvent`] within a session.
    pub const fn next_session(&self) -> Result<Session, TerminalExitTrialError> {
        match self.phase {
            Phase::Fresh(session) => Ok(session),
            Phase::Halted | Phase::Complete => Err(TerminalExitTrialError::TerminalState),
            _ => Err(TerminalExitTrialError::UnexpectedEvent),
        }
    }

    /// Permanently stop without clearing possible change or doing any cleanup.
    /// Completed evidence remains complete; no new event is accepted afterward.
    pub const fn halt(&mut self) {
        if !matches!(self.phase, Phase::Complete) {
            self.phase = Phase::Halted;
        }
    }

    /// Validate one caller attestation in the fixed sequence.
    ///
    /// # Errors
    ///
    /// Rejects any guard mismatch, repeated identifier, wrong event order, or
    /// terminal instance. Every failure halts further progress without clearing
    /// conservative state. Events after completion cannot revoke prior evidence.
    pub fn record(
        &mut self,
        event: TerminalExitTrialEvent<'_>,
    ) -> Result<(), TerminalExitTrialError> {
        let result = self.record_inner(event);
        if result.is_err() {
            self.halt();
        }
        result
    }

    fn record_inner(
        &mut self,
        event: TerminalExitTrialEvent<'_>,
    ) -> Result<(), TerminalExitTrialError> {
        if matches!(self.phase, Phase::Halted | Phase::Complete) {
            return Err(TerminalExitTrialError::TerminalState);
        }
        if matches!(&event, TerminalExitTrialEvent::DurableWriteIntent { id } if self.intent == Some(*id))
        {
            return Err(TerminalExitTrialError::ReusedWriteIntent);
        }
        match (self.phase, event) {
            (
                Phase::Fresh(session),
                TerminalExitTrialEvent::FreshSession {
                    id,
                    identity,
                    memory_format,
                    gateway_mode,
                    whole_page,
                    control_page,
                    routing_page,
                },
            ) => {
                self.validate_fresh(session, identity, memory_format, gateway_mode)?;
                if self.sessions.contains(&id) {
                    return Err(TerminalExitTrialError::ReusedSession);
                }
                compare_page(control_page, &self.control, PageRole::Control)?;
                compare_page(routing_page, &self.routing, PageRole::Routing)?;
                let target = match session {
                    Session::Apply => &self.expected_active,
                    Session::Verify => &self.off,
                };
                compare_page(whole_page, target, PageRole::Target)?;
                self.sessions.push(id);
                self.phase = match session {
                    Session::Apply => Phase::Intent,
                    Session::Verify => Phase::Finalize(session),
                };
                Ok(())
            }
            (Phase::Intent, TerminalExitTrialEvent::DurableWriteIntent { id }) => {
                self.intent = Some(id);
                self.status = TerminalExitTrialStatus::PossiblyChanged;
                self.phase = Phase::Readback;
                Ok(())
            }
            (Phase::Readback, TerminalExitTrialEvent::ImmediateReadback { whole_page }) => {
                compare_page(whole_page, &self.off, PageRole::Target)?;
                self.phase = Phase::Finalize(Session::Apply);
                Ok(())
            }
            (
                Phase::Finalize(session),
                TerminalExitTrialEvent::SessionFinalized {
                    id,
                    identity,
                    gateway_mode,
                },
            ) => {
                if self.sessions.last() != Some(&id) {
                    return Err(TerminalExitTrialError::SessionMismatch);
                }
                self.validate_identity(identity)?;
                validate_gateway(gateway_mode, DvGatewayMode::Off)?;
                self.phase = match session {
                    Session::Apply => Phase::Fresh(Session::Verify),
                    Session::Verify => {
                        self.status = TerminalExitTrialStatus::OffVerifiedAcrossSessions;
                        Phase::Complete
                    }
                };
                Ok(())
            }
            _ => Err(TerminalExitTrialError::UnexpectedEvent),
        }
    }

    fn validate_fresh(
        &self,
        session: Session,
        identity: &Identity,
        memory_format: u8,
        gateway_mode: DvGatewayMode,
    ) -> Result<(), TerminalExitTrialError> {
        self.validate_identity(identity)?;
        if memory_format != 0 {
            return Err(TerminalExitTrialError::MemoryFormat {
                actual: memory_format,
            });
        }
        validate_gateway(
            gateway_mode,
            match session {
                Session::Apply => DvGatewayMode::Terminal,
                Session::Verify => DvGatewayMode::Off,
            },
        )
    }
}

#[derive(Clone, Copy)]
enum PageRole {
    Target,
    Control,
    Routing,
}

fn compare_page(
    actual: &[u8],
    expected: &[u8; PAGE_SIZE],
    role: PageRole,
) -> Result<(), TerminalExitTrialError> {
    if actual.len() != PAGE_SIZE {
        let actual = actual.len();
        return Err(match role {
            PageRole::Target => TerminalExitTrialError::PageLength { actual },
            PageRole::Control => TerminalExitTrialError::ControlPageLength { actual },
            PageRole::Routing => TerminalExitTrialError::RoutingPageLength { actual },
        });
    }
    if actual != expected {
        return Err(match role {
            PageRole::Target => TerminalExitTrialError::PageMismatch,
            PageRole::Control => TerminalExitTrialError::ControlPageMismatch,
            PageRole::Routing => TerminalExitTrialError::RoutingPageMismatch,
        });
    }
    Ok(())
}

fn validate_gateway(
    actual: DvGatewayMode,
    expected: DvGatewayMode,
) -> Result<(), TerminalExitTrialError> {
    if actual == expected {
        Ok(())
    } else {
        Err(TerminalExitTrialError::GatewayModeMismatch { expected, actual })
    }
}

fn validate_stored_guards(
    off: &[u8; PAGE_SIZE],
    control: &[u8; PAGE_SIZE],
    routing: &[u8; PAGE_SIZE],
) -> Result<(), TerminalExitTrialError> {
    let pm = *control
        .get(9)
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    if pm != 0 {
        return Err(TerminalExitTrialError::PmSelection { actual: pm });
    }
    let gateway = *off
        .first()
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    if gateway != 0 {
        return Err(TerminalExitTrialError::StoredGatewayMode { actual: gateway });
    }
    let subtype = *off
        .get(2)
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    if subtype != 0 {
        return Err(TerminalExitTrialError::TerminalSubtype { actual: subtype });
    }
    let usb = *routing
        .get(71)
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    if usb != 0 {
        return Err(TerminalExitTrialError::UsbFunction { actual: usb });
    }
    let route = *routing
        .get(77)
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    if route != 1 {
        return Err(TerminalExitTrialError::GatewayRoute { actual: route });
    }
    Ok(())
}

fn supported_page(field: &MenuField, pin: FieldPin) -> Result<Page, TerminalExitTrialError> {
    let options_match = field.enum_type.is_some()
        && field.options.len() == pin.options
        && field
            .options
            .iter()
            .enumerate()
            .all(|(index, option)| usize::try_from(option.raw).ok() == Some(index));
    if field.descriptor != pin.descriptor
        || field.menu != pin.menu
        || !options_match
        || !field.allowed_values.is_empty()
        || field.storage_transform.is_some()
        || field.is_blob
    {
        return Err(TerminalExitTrialError::UnsupportedDescriptor);
    }
    let slot = if pin.descriptor.is_per_slot() {
        Some(SlotIndex::new(0).map_err(|_| TerminalExitTrialError::UnsupportedDescriptor)?)
    } else {
        None
    };
    let address = field
        .descriptor
        .address(slot)
        .map_err(|_| TerminalExitTrialError::UnsupportedDescriptor)?;
    let page = writable_page_for(address)
        .filter(|page| page.address().as_u32() == pin.page_address && page.len() == PAGE_SIZE)
        .ok_or(TerminalExitTrialError::UnsupportedDescriptor)?;
    let length = u32::try_from(field.descriptor.codec.encoded_len())
        .map_err(|_| TerminalExitTrialError::UnsupportedDescriptor)?;
    if address
        .as_u32()
        .checked_add(length)
        .is_none_or(|end| end > page.end())
    {
        return Err(TerminalExitTrialError::UnsupportedDescriptor);
    }
    Ok(page)
}

/// A rejected fixed guard or caller-evidence transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TerminalExitTrialError {
    /// A generated field or canonical page differs from its pinned shape.
    #[error("Terminal exit descriptor or canonical page is unsupported")]
    UnsupportedDescriptor,
    /// Complete identity does not exactly match the admitted baseline.
    #[error("Terminal exit requires the exact TM-D750 / 1.02 / K,2,1 identity")]
    IdentityMismatch,
    /// Target page is incomplete or oversized.
    #[error("Terminal exit target page has {actual} bytes; expected 256")]
    PageLength {
        /// Observed length.
        actual: usize,
    },
    /// Control page is incomplete or oversized.
    #[error("Terminal exit control page has {actual} bytes; expected 256")]
    ControlPageLength {
        /// Observed length.
        actual: usize,
    },
    /// Routing page is incomplete or oversized.
    #[error("Terminal exit routing page has {actual} bytes; expected 256")]
    RoutingPageLength {
        /// Observed length.
        actual: usize,
    },
    /// Captured PM selector is not Off.
    #[error("Terminal exit requires captured PM Off, not selector {actual}")]
    PmSelection {
        /// Observed stored selector.
        actual: u8,
    },
    /// Captured baseline Gateway byte is not Off.
    #[error("Terminal exit requires an Off baseline, not stored Gateway {actual}")]
    StoredGatewayMode {
        /// Observed stored mode.
        actual: u8,
    },
    /// Captured Terminal subtype is not Reflector.
    #[error("Terminal exit requires Reflector subtype, not {actual}")]
    TerminalSubtype {
        /// Observed stored subtype.
        actual: u8,
    },
    /// Captured USB function is not COM+AF.
    #[error("Terminal exit requires COM+AF USB, not function {actual}")]
    UsbFunction {
        /// Observed stored function.
        actual: u8,
    },
    /// Captured Gateway route is not panel USB.
    #[error("Terminal exit requires panel USB Gateway route, not {actual}")]
    GatewayRoute {
        /// Observed stored route.
        actual: u8,
    },
    /// Fresh CAT Gateway evidence differs from the required state.
    #[error("Terminal exit requires Gateway {expected}; observed {actual}")]
    GatewayModeMismatch {
        /// Required Gateway state.
        expected: DvGatewayMode,
        /// Observed Gateway state.
        actual: DvGatewayMode,
    },
    /// Fresh memory-format byte is not zero.
    #[error("Terminal exit requires memory format zero, not {actual}")]
    MemoryFormat {
        /// Observed format byte.
        actual: u8,
    },
    /// Any complete target-page byte differs from the immutable expectation.
    #[error("Terminal exit complete target page differs from its immutable expectation")]
    PageMismatch,
    /// Any complete control-page byte differs from its captured baseline.
    #[error("Terminal exit complete control page differs from its immutable baseline")]
    ControlPageMismatch,
    /// Any complete routing-page byte differs from its captured baseline.
    #[error("Terminal exit complete routing page differs from its immutable baseline")]
    RoutingPageMismatch,
    /// A fresh-session identifier was already accepted.
    #[error("Terminal exit session identifier was reused")]
    ReusedSession,
    /// The sole write-intent identifier was already accepted.
    #[error("Terminal exit write-intent identifier was reused")]
    ReusedWriteIntent,
    /// Finalization names a different session from the accepted observation.
    #[error("Terminal exit finalization session identifier does not match")]
    SessionMismatch,
    /// Evidence arrived before or after its only permitted phase.
    #[error("Terminal exit evidence is out of order")]
    UnexpectedEvent,
    /// No further evidence is accepted after completion or halt.
    #[error("Terminal exit trial has completed or halted")]
    TerminalState,
}

#[cfg(test)]
#[path = "terminal_exit_trial_tests.rs"]
mod tests;
