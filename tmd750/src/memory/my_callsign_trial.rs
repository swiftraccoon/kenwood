//! Offline preparation for one fixed PM-off MY1 callsign trial and restoration.

use std::num::NonZeroU64;

use super::fixed_text_trial::{
    FixedTextTrial, FixedTextTrialObservation, FixedTextTrialScope, TrialSequence,
};
use super::{
    FieldCodec, FieldDescriptor, FieldValue, MenuField, PmNameTrialError, PmNameTrialEvent,
    PmNameTrialSession, PmNameTrialStatus, SLOT_TERM, StringEncoding, menu_field,
};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page, RadioModel, SlotIndex};

const FIELD_LENGTH: usize = 8;
const TARGET_PAGE_ADDRESS: u32 = 331_776;
const CONTROL_PAGE_ADDRESS: u32 = 323_584;

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
    page_address: TARGET_PAGE_ADDRESS,
};

const GATEWAY: FieldPin = FieldPin {
    descriptor: FieldDescriptor::with_terms(
        "dv.DvGatewayModeDvGateway",
        331_776,
        &[SLOT_TERM],
        FieldCodec::Byte { min: 0, max: 2 },
    ),
    menu: "dv",
    options: Some(3),
    page_address: TARGET_PAGE_ADDRESS,
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
    page_address: TARGET_PAGE_ADDRESS,
};

const PM_SELECTION: FieldPin = FieldPin {
    descriptor: FieldDescriptor::new("pm.PmSelect", 323_593, FieldCodec::Byte { min: 0, max: 6 }),
    menu: "pm",
    options: Some(7),
    page_address: CONTROL_PAGE_ADDRESS,
};

struct Descriptors<'a> {
    callsign: &'a MenuField,
    gateway: &'a MenuField,
    my_selection: &'a MenuField,
    pm_selection: &'a MenuField,
}

impl Descriptors<'static> {
    fn load() -> Result<Self, PmNameTrialError> {
        let resolve = |pin: FieldPin| {
            menu_field(pin.descriptor.name).ok_or(PmNameTrialError::UnsupportedDescriptor)
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
    fn pages(&self) -> Result<(Page, Page), PmNameTrialError> {
        let target = supported_page(self.callsign, CALLSIGN)?;
        if supported_page(self.gateway, GATEWAY)? != target
            || supported_page(self.my_selection, MY_SELECTION)? != target
        {
            return Err(PmNameTrialError::UnsupportedDescriptor);
        }
        Ok((target, supported_page(self.pm_selection, PM_SELECTION)?))
    }
}

/// Offline state machine for a fixed MY1 change followed by restoration.
///
/// Scope is fixed: TM-D750 / firmware 1.02 / type `K,2,1`, PM Off, Gateway
/// Off, MY selector zero, and an exactly eight-NUL MY1 baseline. The temporary
/// value is [`Self::TEMPORARY_CALLSIGN`], encoded as `KQ4NIT\0\0`. No caller
/// can choose a callsign, PM slot, page address, or other setting. Every other
/// target-page byte and the complete control page remain immutable.
///
/// This type performs no I/O. Its three-session sequence applies the temporary
/// MY1 value, writes the exact original page back, then rereads it in a
/// read-only third session. Each session must supply a fresh Gateway Off
/// observation and the complete control page, both taken on trust.
#[derive(Debug)]
pub struct MyCallsignTrial {
    sequence: TrialSequence,
    control_spec: Page,
    control: [u8; PAGE_SIZE],
}

impl MyCallsignTrial {
    /// Sole temporary MY1 value; never a caller-selectable setting.
    pub const TEMPORARY_CALLSIGN: &str = "KQ4NIT";

    /// Resolve the sole complete target page after validating every fixed field
    /// and guard descriptor against the generated registry.
    ///
    /// # Errors
    ///
    /// Returns [`PmNameTrialError::UnsupportedDescriptor`] on any shape drift.
    pub fn required_page() -> Result<Page, PmNameTrialError> {
        Descriptors::load()?.pages().map(|(target, _)| target)
    }

    /// Resolve the sole complete control page, including the PM Off selector.
    ///
    /// This validates every target and control descriptor's shape.
    ///
    /// # Errors
    ///
    /// Returns [`PmNameTrialError::UnsupportedDescriptor`] on any shape drift.
    pub fn required_control_page() -> Result<Page, PmNameTrialError> {
        Descriptors::load()?.pages().map(|(_, control)| control)
    }

    /// Prepare both immutable pages from complete, validated sparse captures.
    ///
    /// `target_page` and `control_page` must be actual captured pages, never
    /// synthesized unread gaps. The MY1 field in `target_page` must be exactly
    /// eight NUL bytes; that is a check on the captured bytes, not on the
    /// radio's display.
    ///
    /// # Errors
    ///
    /// Rejects any identity or descriptor mismatch, incomplete page, active PM
    /// or Gateway, nonzero MY selector, or MY1 byte other than NUL.
    pub fn prepare_unqualified_offline(
        identity: &Identity,
        target_page: &[u8],
        control_page: &[u8],
    ) -> Result<Self, PmNameTrialError> {
        Self::prepare_with_fields(identity, target_page, control_page, &Descriptors::load()?)
    }

    fn prepare_with_fields(
        identity: &Identity,
        target_page: &[u8],
        control_page: &[u8],
        fields: &Descriptors<'_>,
    ) -> Result<Self, PmNameTrialError> {
        if identity.model != RadioModel::TmD750
            || identity.firmware.as_str() != "1.02"
            || identity.radio_type.as_str() != "K,2,1"
        {
            return Err(PmNameTrialError::IdentityMismatch);
        }
        let (page, control_spec) = fields.pages()?;
        let original: [u8; PAGE_SIZE] =
            target_page
                .try_into()
                .map_err(|_| PmNameTrialError::PageLength {
                    actual: target_page.len(),
                })?;
        let control: [u8; PAGE_SIZE] =
            control_page
                .try_into()
                .map_err(|_| PmNameTrialError::ControlPageLength {
                    actual: control_page.len(),
                })?;
        validate_stored_guards(&original, &control)?;
        if original.get(8..16) != Some([0; FIELD_LENGTH].as_slice()) {
            return Err(PmNameTrialError::MyCallsignNotEmpty);
        }
        let mut expected = original;
        expected
            .get_mut(8..16)
            .ok_or(PmNameTrialError::UnsupportedDescriptor)?
            .copy_from_slice(&encode_callsign(fields.callsign)?);
        Ok(Self {
            sequence: TrialSequence::new(identity, page, original, expected),
            control_spec,
            control,
        })
    }

    /// Exact immutable complete CAT identity retained with the captured pages.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        self.sequence.identity()
    }

    /// Sole validated target page, never caller-selectable.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.sequence.page()
    }

    /// Exact original target page retained for restoration, including all
    /// unrelated bytes and the original empty MY1 field.
    #[must_use]
    pub const fn original_page(&self) -> &[u8; PAGE_SIZE] {
        self.sequence.original_page()
    }

    /// Exact temporary target page, differing only in MY1's eight-byte field.
    #[must_use]
    pub const fn expected_page(&self) -> &[u8; PAGE_SIZE] {
        self.sequence.expected_page()
    }

    /// Complete immutable control-page bytes that each fresh session must match.
    /// This page is never a write target of the trial.
    #[must_use]
    pub const fn control_page(&self) -> &[u8; PAGE_SIZE] {
        &self.control
    }

    /// Exact validated address and length of the immutable control page.
    #[must_use]
    pub const fn control_page_spec(&self) -> Page {
        self.control_spec
    }

    /// Status derived from the events recorded so far.
    #[must_use]
    pub const fn status(&self) -> PmNameTrialStatus {
        self.sequence.status()
    }

    /// The session expected next, available only while awaiting a fresh session.
    ///
    /// # Errors
    ///
    /// Returns [`PmNameTrialError::TerminalState`] after completion or halt;
    /// otherwise returns [`PmNameTrialError::UnexpectedEvent`] during a session.
    pub const fn next_session(&self) -> Result<PmNameTrialSession, PmNameTrialError> {
        self.sequence.next_session()
    }

    /// Permanently stop accepting events, keeping the current status.
    ///
    /// Performs no cleanup, cancellation, or restoration.
    pub const fn halt(&mut self) {
        self.sequence.halt();
    }

    /// Report completed E/ACK, original close and drop, matching fresh CAT
    /// identity, fresh close, complete captures, and a synchronized record of
    /// this session.
    ///
    /// Status becomes [`PmNameTrialStatus::RestorationVerified`] only after all
    /// three sessions and their whole-page comparisons have been accepted.
    ///
    /// # Errors
    ///
    /// Rejects wrong order, a mismatching session ID, or a terminal instance.
    /// Any failure permanently halts further acceptance and keeps the status.
    pub fn finalize_session(&mut self, id: NonZeroU64) -> Result<(), PmNameTrialError> {
        self.sequence
            .record(PmNameTrialEvent::SessionFinalized { id })
    }

    fn compare_guards(
        &self,
        observation: &FixedTextTrialObservation<'_>,
    ) -> Result<(), PmNameTrialError> {
        let _session = self.next_session()?;
        let gateway = observation
            .gateway_mode
            .ok_or(PmNameTrialError::FreshGuardsMissing)?;
        if gateway != DvGatewayMode::Off {
            return Err(PmNameTrialError::GatewayMode { actual: gateway });
        }
        let control = observation
            .control_page
            .ok_or(PmNameTrialError::FreshGuardsMissing)?;
        if control.len() != PAGE_SIZE {
            return Err(PmNameTrialError::ControlPageLength {
                actual: control.len(),
            });
        }
        if control != self.control {
            return Err(PmNameTrialError::ControlPageMismatch);
        }
        Ok(())
    }
}

impl FixedTextTrial for MyCallsignTrial {
    fn scope(&self) -> FixedTextTrialScope {
        FixedTextTrialScope::My1
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
    fn status(&self) -> PmNameTrialStatus {
        self.status()
    }
    fn next_session(&self) -> Result<PmNameTrialSession, PmNameTrialError> {
        self.next_session()
    }
    fn halt(&mut self) {
        self.halt();
    }
    fn guard_page(&self) -> Option<Page> {
        Some(self.control_spec)
    }

    fn record(&mut self, event: PmNameTrialEvent<'_>) -> Result<(), PmNameTrialError> {
        if matches!(event, PmNameTrialEvent::FreshSession { .. }) {
            self.halt();
            return Err(PmNameTrialError::FreshGuardsMissing);
        }
        self.sequence.record(event)
    }

    fn fresh_session(
        &mut self,
        observation: FixedTextTrialObservation<'_>,
    ) -> Result<(), PmNameTrialError> {
        if let Err(error) = self.compare_guards(&observation) {
            self.halt();
            return Err(error);
        }
        self.sequence.record(PmNameTrialEvent::FreshSession {
            id: observation.id,
            identity: observation.identity,
            memory_format: observation.memory_format,
            whole_page: observation.whole_page,
        })
    }
}

fn supported_page(field: &MenuField, pin: FieldPin) -> Result<Page, PmNameTrialError> {
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
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    let slot = if pin.descriptor.is_per_slot() {
        Some(SlotIndex::new(0).map_err(|_| PmNameTrialError::UnsupportedDescriptor)?)
    } else {
        None
    };
    let address = field
        .descriptor
        .address(slot)
        .map_err(|_| PmNameTrialError::UnsupportedDescriptor)?;
    let page = writable_page_for(address)
        .filter(|page| page.address().as_u32() == pin.page_address && page.len() == PAGE_SIZE)
        .ok_or(PmNameTrialError::UnsupportedDescriptor)?;
    let length = u32::try_from(field.descriptor.codec.encoded_len())
        .map_err(|_| PmNameTrialError::UnsupportedDescriptor)?;
    if address
        .as_u32()
        .checked_add(length)
        .is_none_or(|end| end > page.end())
    {
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    Ok(page)
}

fn validate_stored_guards(
    target: &[u8; PAGE_SIZE],
    control: &[u8; PAGE_SIZE],
) -> Result<(), PmNameTrialError> {
    let pm = *control
        .get(9)
        .ok_or(PmNameTrialError::UnsupportedDescriptor)?;
    if pm != 0 {
        return Err(PmNameTrialError::PmSelection { actual: pm });
    }
    let gateway = DvGatewayMode::from(
        *target
            .first()
            .ok_or(PmNameTrialError::UnsupportedDescriptor)?,
    );
    if gateway != DvGatewayMode::Off {
        return Err(PmNameTrialError::GatewayMode { actual: gateway });
    }
    let selection = *target
        .get(1)
        .ok_or(PmNameTrialError::UnsupportedDescriptor)?;
    if selection != 0 {
        return Err(PmNameTrialError::MySelection { actual: selection });
    }
    Ok(())
}

fn encode_callsign(field: &MenuField) -> Result<[u8; FIELD_LENGTH], PmNameTrialError> {
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(MyCallsignTrial::TEMPORARY_CALLSIGN))
        .map_err(|_| PmNameTrialError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    for (index, patch) in encoded.into_iter().enumerate() {
        if index != patch.offset() || patch.mask() != u8::MAX {
            return Err(PmNameTrialError::UnsupportedDescriptor);
        }
        *bytes
            .get_mut(patch.offset())
            .ok_or(PmNameTrialError::UnsupportedDescriptor)? = patch.value();
    }
    if bytes != *b"KQ4NIT\0\0" {
        return Err(PmNameTrialError::UnsupportedDescriptor);
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "my_callsign_trial_tests.rs"]
mod tests;
