//! The MY1 callsign of PM Off: eight NUL-padded bytes in the DV Gateway page,
//! guarded by the PM control page, stored and observed Gateway Off, and MY1
//! selected.

use super::{
    GuardKind, GuardPolicy, PmOffGatewayGuards as Guards, TextField, TextFieldUpdate,
    TextFieldUpdateError, TextValue, check_identity, complete_page, replace_field, sealed,
};
use crate::memory::{
    FieldCodec, FieldDescriptor, FieldValue, MenuField, SLOT_TERM, StringEncoding, menu_field,
};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{DvGatewayMode, PAGE_SIZE, Page, SlotIndex};

pub(super) const FIELD_NAME: &str = "dv.MyCallsignDvGatewayList[0].MyCallsignDvGateway";
const FIELD_LENGTH: usize = 8;
const FIELD_OFFSET: usize = 8;
const TARGET_ADDRESS: u32 = 331_776;
const CONTROL_ADDRESS: u32 = 323_584;
const RULE: &str = "1 to 8 uppercase ASCII letters, digits, or spaces, including a letter or digit";

/// Exact MY1 storage text: one to eight uppercase ASCII letters, digits, or spaces.
///
/// At least one letter or digit is required. Leading, trailing, and interior
/// spaces are preserved. This validates storage syntax only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct My1Callsign(String);

impl My1Callsign {
    /// Validate MY1 text without normalization, truncation, or replacement.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::InvalidText`] for an empty or oversized
    /// value, missing alphanumeric content, or any unsupported byte.
    /// Lowercase, slashes, Unicode, controls, and embedded NUL are rejected.
    pub fn new(value: &str) -> Result<Self, TextFieldUpdateError> {
        if value.is_empty()
            || value.len() > FIELD_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ')
            || !value.bytes().any(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(TextFieldUpdateError::InvalidText {
                field: "MY1 callsign",
                rule: RULE,
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact validated bytes as text, including every space.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl sealed::Sealed for My1Callsign {}

impl TextValue for My1Callsign {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The PM control page captured with the target page and required unchanged
/// in both sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlPage {
    spec: Page,
    bytes: [u8; PAGE_SIZE],
}

impl ControlPage {
    /// The complete control-page address and length.
    #[must_use]
    pub const fn spec(&self) -> Page {
        self.spec
    }

    /// The complete unchanged control-page bytes.
    #[must_use]
    pub const fn bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.bytes
    }
}

/// The guard policy of the PM Off MY1 field: the control page unchanged and
/// Gateway Off in every session, observed again on each fresh post-exit
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmOffGatewayGuards;

/// Fresh facts of a [`PmOffGatewayGuards`] session.
#[derive(Debug, Clone, Copy)]
pub struct PmOffGatewayFresh<'a> {
    /// Actual fresh pre-entry Gateway observation; must be Off.
    pub gateway_mode: DvGatewayMode,
    /// Complete unchanged control page, including the PM-Off selector.
    pub control_page: &'a [u8],
}

/// Post-exit facts of a [`PmOffGatewayGuards`] session.
#[derive(Debug, Clone, Copy)]
pub struct PmOffGatewayFinal<'a> {
    /// Actual identity observed on the separate post-exit CAT connection.
    pub identity: &'a Identity,
    /// Actual Gateway state on that fresh connection; must be Off.
    pub gateway_mode: DvGatewayMode,
}

impl sealed::Sealed for PmOffGatewayGuards {}

impl GuardPolicy for PmOffGatewayGuards {
    type Fresh<'a> = PmOffGatewayFresh<'a>;
    type Final<'a> = PmOffGatewayFinal<'a>;
    type Stored = ControlPage;
    const KIND: GuardKind = GuardKind::PmOffGateway;

    fn check_fresh_state(fresh: &Self::Fresh<'_>) -> Result<(), TextFieldUpdateError> {
        gateway_off(fresh.gateway_mode)
    }

    fn check_fresh_pages(
        stored: &Self::Stored,
        fresh: &Self::Fresh<'_>,
    ) -> Result<(), TextFieldUpdateError> {
        if fresh.control_page.len() != PAGE_SIZE {
            return Err(TextFieldUpdateError::ControlPageLength {
                actual: fresh.control_page.len(),
            });
        }
        if fresh.control_page != stored.bytes {
            return Err(TextFieldUpdateError::ControlPageMismatch);
        }
        Ok(())
    }

    fn check_final(
        identity: &Identity,
        facts: &Self::Final<'_>,
    ) -> Result<(), TextFieldUpdateError> {
        if facts.identity != identity {
            return Err(TextFieldUpdateError::IdentityMismatch);
        }
        gateway_off(facts.gateway_mode)
    }
}

fn gateway_off(gateway: DvGatewayMode) -> Result<(), TextFieldUpdateError> {
    if gateway == DvGatewayMode::Off {
        Ok(())
    } else {
        Err(TextFieldUpdateError::GatewayMode { actual: gateway })
    }
}

impl TextFieldUpdate<My1Callsign, Guards> {
    /// Resolve the sole target page after validating all target/control descriptors.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::UnsupportedDescriptor`] on layout drift.
    pub fn required_page() -> Result<Page, TextFieldUpdateError> {
        Descriptors::load()?.pages().map(|(target, _)| target)
    }

    /// Resolve the complete immutable control page after all descriptor checks.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::UnsupportedDescriptor`] on layout drift.
    pub fn required_control_page() -> Result<Page, TextFieldUpdateError> {
        Descriptors::load()?.pages().map(|(_, control)| control)
    }

    /// Prepare one persistent text update from complete observed capture pages.
    ///
    /// `expected_current: None` requires exactly eight NUL bytes, not spaces or
    /// erased bytes. A supplied current value requires its exact NUL-padded
    /// encoding. `desired: None` clears the field to eight NUL bytes, the
    /// stored form of an empty MY1; a no-op, both values `None` or equal, is
    /// rejected.
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
        desired: Option<&My1Callsign>,
    ) -> Result<Self, TextFieldUpdateError> {
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
        requested: Option<&My1Callsign>,
        fields: &Descriptors<'_>,
    ) -> Result<Self, TextFieldUpdateError> {
        check_identity(identity)?;
        let (page, control_spec) = fields.pages()?;
        let original = complete_page(target_page)?;
        let control: [u8; PAGE_SIZE] =
            control_page
                .try_into()
                .map_err(|_| TextFieldUpdateError::ControlPageLength {
                    actual: control_page.len(),
                })?;
        validate_stored_guards(&original, &control)?;
        let desired = replace_field(
            &original,
            FIELD_OFFSET..FIELD_OFFSET + FIELD_LENGTH,
            &encode_field(fields.callsign, expected_current)?,
            &encode_field(fields.callsign, requested)?,
            expected_current == requested,
        )?;
        Ok(Self::new(
            TextField::PmOffMy1Callsign,
            identity.clone(),
            page,
            original,
            desired,
            expected_current.cloned(),
            requested.cloned(),
            ControlPage {
                spec: control_spec,
                bytes: control,
            },
        ))
    }

    /// The complete immutable control-page address and length.
    #[must_use]
    pub const fn control_page_spec(&self) -> Page {
        self.guards.spec
    }

    /// Complete unchanged control-page bytes required in both sessions.
    #[must_use]
    pub const fn control_page(&self) -> &[u8; PAGE_SIZE] {
        &self.guards.bytes
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
        FIELD_NAME,
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

/// The four registry descriptors the MY1 update pins.
struct Descriptors<'a> {
    callsign: &'a MenuField,
    gateway: &'a MenuField,
    my_selection: &'a MenuField,
    pm_selection: &'a MenuField,
}

impl Descriptors<'static> {
    fn load() -> Result<Self, TextFieldUpdateError> {
        let resolve = |pin: FieldPin| {
            menu_field(pin.descriptor.name).ok_or(TextFieldUpdateError::UnsupportedDescriptor)
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
    fn pages(&self) -> Result<(Page, Page), TextFieldUpdateError> {
        let target = supported_page(self.callsign, CALLSIGN)?;
        if supported_page(self.gateway, GATEWAY)? != target
            || supported_page(self.my_selection, MY_SELECTION)? != target
        {
            return Err(TextFieldUpdateError::UnsupportedDescriptor);
        }
        Ok((target, supported_page(self.pm_selection, PM_SELECTION)?))
    }
}

fn supported_page(field: &MenuField, pin: FieldPin) -> Result<Page, TextFieldUpdateError> {
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
        return Err(TextFieldUpdateError::UnsupportedDescriptor);
    }
    let slot = if pin.descriptor.is_per_slot() {
        Some(SlotIndex::new(0).map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?)
    } else {
        None
    };
    let address = field
        .descriptor
        .address(slot)
        .map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?;
    let page = writable_page_for(address)
        .filter(|page| page.address().as_u32() == pin.page_address && page.len() == PAGE_SIZE)
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?;
    let length = u32::try_from(field.descriptor.codec.encoded_len())
        .map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?;
    if address
        .as_u32()
        .checked_add(length)
        .is_none_or(|end| end > page.end())
    {
        return Err(TextFieldUpdateError::UnsupportedDescriptor);
    }
    Ok(page)
}

fn validate_stored_guards(
    target: &[u8; PAGE_SIZE],
    control: &[u8; PAGE_SIZE],
) -> Result<(), TextFieldUpdateError> {
    let pm = *control
        .get(9)
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?;
    if pm != 0 {
        return Err(TextFieldUpdateError::PmSelection { actual: pm });
    }
    let gateway = DvGatewayMode::from(
        *target
            .first()
            .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?,
    );
    gateway_off(gateway)?;
    let selection = *target
        .get(1)
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?;
    if selection != 0 {
        return Err(TextFieldUpdateError::MySelection { actual: selection });
    }
    Ok(())
}

/// The stored field of an optional callsign; `None` is the all-NUL field of an
/// empty MY1.
fn encode_field(
    field: &MenuField,
    callsign: Option<&My1Callsign>,
) -> Result<[u8; FIELD_LENGTH], TextFieldUpdateError> {
    callsign.map_or(Ok([0; FIELD_LENGTH]), |value| encode_callsign(field, value))
}

fn encode_callsign(
    field: &MenuField,
    callsign: &My1Callsign,
) -> Result<[u8; FIELD_LENGTH], TextFieldUpdateError> {
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(callsign.as_str()))
        .map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(TextFieldUpdateError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    bytes
        .get_mut(..callsign.as_str().len())
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)?
        .copy_from_slice(callsign.as_str().as_bytes());
    for (index, patch) in encoded.into_iter().enumerate() {
        if index != patch.offset()
            || patch.mask() != u8::MAX
            || bytes.get(patch.offset()) != Some(&patch.value())
        {
            return Err(TextFieldUpdateError::UnsupportedDescriptor);
        }
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "my1_tests.rs"]
mod tests;
