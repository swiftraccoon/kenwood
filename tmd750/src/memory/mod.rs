//! Offline storage, captured-configuration comparison, and immutable update plans.
//!
//! This module performs no radio I/O. A decoded value describes the supplied
//! bytes, not the radio's current state.
//!
//! # Choose a task
//!
//! - Keep a capture with [`Configuration`]: its identity, pages and bytes,
//!   coverage-checked access, the standard and menu views, comparison, and a
//!   persisted form.
//! - Use [`MemoryImage`] for a full-sized buffer and [`FieldAccess`] for
//!   global or PM-relative decoding. Establish field coverage separately when
//!   the bytes came from a sparse radio read.
//! - Prefer [`crate::radio::menu::MenuFieldSnapshot`] when retaining complete
//!   captured pages: it refuses decoding a field whose coverage is missing.
//!   [`StandardConfiguration`] validates the full standard transfer schedule;
//!   [`StandardConfigurationDiff`] compares two such captures byte-for-byte.
//! - Discover storage metadata with [`menu_field`] and [`MenuField`].
//!   [`schema`] explains addressing, scalar interpretation, and masked planning.
//! - Use [`TextImage`] for typed offline text previews and
//!   [`ReflectorTerminalPreflight`] for captured Terminal-setting inspection.
//! - Read the D-STAR repeater and hotspot lists with
//!   [`repeater_list::RepeaterList`], the destination callsign list with
//!   [`callsign_list::CallsignList`] and the six-entry MY callsign list with
//!   [`MyCallsignList`]; the CALL and weather channels with
//!   [`ChannelAccess`]; the user Mic-E type codes with
//!   [`mic_e::MicETypeCodeTables`].
//! - Prepare ordinary registered changes with
//!   [`crate::radio::menu::MenuUpdatePlan`], or persistent Gateway changes with
//!   [`crate::radio::terminal::TerminalPlan`]. Their session drivers separately
//!   require identity, fresh complete-page comparisons, and caller-owned cleanup.
//!
//! [`TextFieldUpdate`], prepared as [`Pm1NameUpdate`], [`My1CallsignUpdate`]
//! or [`ChannelNameUpdate`], sequences a two-session compare-write-verify
//! update for one text field under its sealed guard policy. It checks event
//! order and page equality; the facts a caller reports (fresh connection,
//! synchronized journal record, completed capture) are taken on trust. File
//! containers live in [`crate::file`].

pub mod callsign_list;
mod captured_configuration;
mod chain;
mod channel_names;
pub mod channels;
mod configuration;
pub mod menu_fields;
mod menu_patch;
mod menu_policy;
pub mod mic_e;
mod my_callsigns;
pub mod repeater_list;
pub mod schema;
mod stored_text;
mod terminal;
mod text;
mod text_field_update;
pub mod vfo;

use crate::error::{SchemaError, ValidationError};
use crate::types::{IMAGE_LENGTH, SlotIndex};

pub use captured_configuration::Configuration;
pub use channel_names::ChannelNamePlan;
pub use channels::{
    CallBand, CallChannel, CallChannelEntry, CallChannelPlan, CallMode, ChannelAccess,
    WeatherChannel, WeatherChannelEntry,
};
pub use configuration::{
    ChangedByte, ChangedPage, ConfigurationError, StandardConfiguration, StandardConfigurationDiff,
};
pub use menu_fields::{
    MCP_D750_IMAGE_LENGTH, MCP_D750_MENU_FIELDS, MCP_D750_SCHEMA_VERSION, MCP_D750_SLOT_COUNT,
    MCP_D750_SLOT_STRIDE, MCP_D750_SOURCE_SHA256, MenuField, MenuFieldId, MenuOption,
    StorageTransform, menu_field,
};
pub use menu_patch::MenuValueError;
pub use menu_policy::{MenuWritePolicy, MenuWritePolicyError};
pub use my_callsigns::{
    MY_CALLSIGN_ENTRY_SIZE, MY_CALLSIGN_LIST_ADDRESS, MY_CALLSIGN_SELECTION_ADDRESS, MyCallsignList,
};
pub use schema::{
    DecodedFieldValue, Endian, FieldCodec, FieldDescriptor, FieldValue, PatchPlanner, PatchSet,
    SLOT_TERM, StringEncoding, Term,
};
pub use stored_text::{StoredText, StoredTextError};
pub use terminal::{
    ReflectorTerminalPreflight, TerminalFinding, TerminalGatewayMode, TerminalGatewayRoute,
    TerminalMode, TerminalMyCallsignIndex, TerminalPreflightError, TerminalUsbFunction,
    TerminalUsbRoute,
};
pub use text::{
    TextError, TextImage, TextMetadata, TextPreview, TextScope, TextScopeKind, TextSetting,
};
pub use text_field_update::{
    ChannelNameText, ChannelNameUpdate, ControlPage, GuardKind, GuardPolicy, My1Callsign,
    My1CallsignUpdate, NoGuards, Pm1Name, Pm1NameUpdate, PmOffGatewayFinal, PmOffGatewayFresh,
    PmOffGatewayGuards, TextField, TextFieldUpdate, TextFieldUpdateError, TextFieldUpdateEvent,
    TextFieldUpdateSession, TextFieldUpdateStatus, TextValue,
};

/// Version of the official MCP-D750 program whose serializers the generated
/// registry was extracted from.
///
/// It names the registry's source, not a radio firmware; writes are gated by
/// [`crate::radio::WRITE_TARGETS`].
pub const MCP_D750_PROGRAM_VERSION: &str = "1.00";

/// Full-sized 1,929,472-byte storage without coverage or firmware provenance.
///
/// [`Self::from_bytes`] checks length only. [`Self::blank`] fills the buffer
/// with `0xFF`. Converting from a sparse [`crate::radio::RegionImage`] keeps
/// its synthetic gap bytes and drops its coverage map.
///
/// Callers must establish coverage before decoding each field. Prefer
/// [`crate::radio::menu::MenuFieldSnapshot`] for coverage-checked sparse access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryImage {
    bytes: Vec<u8>,
}

impl MemoryImage {
    /// Wrap exactly [`IMAGE_LENGTH`] bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ImageLength`] for any other length.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ValidationError> {
        if bytes.len() != IMAGE_LENGTH {
            return Err(ValidationError::ImageLength {
                actual: bytes.len(),
                expected: IMAGE_LENGTH,
            });
        }
        Ok(Self { bytes })
    }

    /// Synthetic storage filled with `0xFF`, the erased-byte representation.
    #[must_use]
    pub fn blank() -> Self {
        Self {
            bytes: vec![0xFF; IMAGE_LENGTH],
        }
    }

    /// The bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Take the bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Access to global fields.
    #[must_use]
    pub fn global(&self) -> FieldAccess<'_> {
        FieldAccess {
            image: &self.bytes,
            slot: None,
        }
    }

    /// Access to one slot's fields (global fields resolve identically).
    #[must_use]
    pub fn slot(&self, slot: SlotIndex) -> FieldAccess<'_> {
        FieldAccess {
            image: &self.bytes,
            slot: Some(slot),
        }
    }

    /// Write `value` into the image (no region check; the planner does that for the radio).
    ///
    /// # Errors
    ///
    /// Returns address and encode errors.
    pub fn set(
        &mut self,
        field: &FieldDescriptor,
        slot: Option<SlotIndex>,
        value: FieldValue<'_>,
    ) -> Result<(), SchemaError> {
        let start = field.address(slot)?.as_usize();
        for patch in field.encode(value)? {
            let byte = self.bytes.get_mut(start + patch.offset()).ok_or_else(|| {
                SchemaError::OutOfBounds {
                    field: field.name,
                    address: u64::try_from(start + patch.offset()).unwrap_or(u64::MAX),
                    len: 1,
                    image_length: IMAGE_LENGTH,
                }
            })?;
            *byte = patch.apply(*byte);
        }
        Ok(())
    }
}

/// Read access bound to a slot (or none).
#[derive(Debug, Clone, Copy)]
pub struct FieldAccess<'a> {
    image: &'a [u8],
    slot: Option<SlotIndex>,
}

impl FieldAccess<'_> {
    /// Decode `field`.
    ///
    /// # Errors
    ///
    /// Address and decode errors.
    pub fn read(&self, field: &FieldDescriptor) -> Result<DecodedFieldValue, SchemaError> {
        field.read(self.image, self.slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn images_are_exactly_sized_and_slot_access_resolves() -> TestResult {
        let short = MemoryImage::from_bytes(vec![0; 10]);
        assert!(
            matches!(short, Err(ValidationError::ImageLength { actual: 10, .. })),
            "{short:?}"
        );
        let mut image = MemoryImage::blank();
        let meter = FieldDescriptor::with_terms(
            "radio.MeterType",
            328_995,
            &[SLOT_TERM],
            FieldCodec::Byte { min: 0, max: 2 },
        );
        let slot = SlotIndex::new(4)?;
        image.set(&meter, Some(slot), FieldValue::Unsigned(2))?;
        assert_eq!(
            image.slot(slot).read(&meter)?,
            DecodedFieldValue::Unsigned(2)
        );
        assert_eq!(
            image.slot(SlotIndex::new(0)?).read(&meter)?,
            DecodedFieldValue::Unsigned(0xFF)
        );
        Ok(())
    }
}
