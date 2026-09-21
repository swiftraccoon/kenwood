//! Offline storage, captured-configuration comparison, and immutable update plans.
//!
//! This module performs no radio I/O. A decoded value describes the supplied
//! bytes, not the radio's current state.
//!
//! # Choose a task
//!
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
//! - Prepare ordinary registered changes with
//!   [`crate::radio::menu::MenuUpdatePlan`], or persistent Gateway changes with
//!   [`crate::radio::terminal::TerminalPlan`]. Their session drivers separately
//!   require identity, fresh complete-page comparisons, and caller-owned cleanup.
//!
//! [`Pm1NameUpdate`] and [`My1CallsignUpdate`] sequence a two-session
//! compare-write-verify update for one field. [`PmNameTrial`],
//! [`MyCallsignTrial`], and [`TerminalExitTrial`] are fixed-scope variants with
//! no caller-selectable value. All of them check event order and page equality;
//! the facts a caller reports (fresh connection, synchronized journal record,
//! completed capture) are taken on trust. File containers live in
//! [`crate::file`].

mod configuration;
pub(crate) mod fixed_text_trial;
pub mod menu_fields;
mod menu_patch;
mod menu_policy;
mod my1_callsign_update;
mod my_callsign_trial;
mod pm1_name_update;
mod pm_name_trial;
pub mod schema;
mod terminal;
mod terminal_exit_trial;
mod text;

use crate::error::{SchemaError, ValidationError};
use crate::types::{FirmwareIdentity, IMAGE_LENGTH, RadioModel, SlotIndex};

pub use configuration::{
    ChangedByte, ChangedPage, ConfigurationError, StandardConfiguration, StandardConfigurationDiff,
};
pub use menu_fields::{
    MCP_D750_IMAGE_LENGTH, MCP_D750_MENU_FIELDS, MCP_D750_SCHEMA_VERSION, MCP_D750_SLOT_COUNT,
    MCP_D750_SLOT_STRIDE, MCP_D750_SOURCE_SHA256, MenuField, MenuOption, StorageTransform,
    menu_field,
};
pub use menu_patch::MenuValueError;
pub use menu_policy::{MenuWritePolicy, MenuWritePolicyError};
pub use my_callsign_trial::MyCallsignTrial;
pub use my1_callsign_update::{
    My1Callsign, My1CallsignUpdate, My1CallsignUpdateError, My1CallsignUpdateEvent,
    My1CallsignUpdateSession, My1CallsignUpdateStatus,
};
pub use pm_name_trial::{
    PmNameTrial, PmNameTrialError, PmNameTrialEvent, PmNameTrialSession, PmNameTrialStatus,
    PmNameTrialWrite,
};
pub use pm1_name_update::{
    Pm1Name, Pm1NameUpdate, Pm1NameUpdateError, Pm1NameUpdateEvent, Pm1NameUpdateSession,
    Pm1NameUpdateStatus,
};
pub use schema::{
    DecodedFieldValue, Endian, FieldCodec, FieldDescriptor, FieldValue, PatchPlanner, PatchSet,
    SLOT_TERM, StringEncoding, Term,
};
pub use terminal::{
    ReflectorTerminalPreflight, TerminalFinding, TerminalGatewayMode, TerminalGatewayRoute,
    TerminalMode, TerminalMyCallsignIndex, TerminalPreflightError, TerminalUsbFunction,
    TerminalUsbRoute,
};
pub use terminal_exit_trial::{
    TerminalExitTrial, TerminalExitTrialError, TerminalExitTrialEvent, TerminalExitTrialSession,
    TerminalExitTrialStatus,
};
pub use text::{
    TextError, TextImage, TextLayoutQualification, TextMetadata, TextPreview, TextScope,
    TextScopeKind, TextSetting,
};

/// Model whose layout the generated registry describes.
pub const MCP_D750_SCHEMA_MODEL: &str = "TM-D750";
/// Firmware label declared to the extractor that generated the registry.
///
/// It records that declared provenance, not a vendor version range.
pub const MCP_D750_SCHEMA_FIRMWARE: &str = "1.00";
/// Firmware labels accepted by [`is_supported_schema_target`].
///
/// Each is compared against [`FirmwareIdentity::as_str`] by exact string match.
pub const MCP_D750_SCHEMA_FIRMWARE_IDENTITIES: &[&str] = &["1.00"];

/// Whether `model` and `firmware` match the registry's extraction target.
#[must_use]
pub fn is_supported_schema_target(model: RadioModel, firmware: &FirmwareIdentity) -> bool {
    model == RadioModel::TmD750 && MCP_D750_SCHEMA_FIRMWARE_IDENTITIES.contains(&firmware.as_str())
}

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
        assert!(is_supported_schema_target(
            RadioModel::TmD750,
            &FirmwareIdentity::new("1.00")?
        ));
        assert!(!is_supported_schema_target(
            RadioModel::TmD750,
            &FirmwareIdentity::new("1.01")?
        ));
        Ok(())
    }
}
