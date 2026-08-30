//! Typed access to the TM-D750 memory image.

pub mod menu_fields;
mod menu_patch;
pub mod schema;

use crate::error::{SchemaError, ValidationError};
use crate::types::{FirmwareIdentity, IMAGE_LENGTH, RadioModel, SlotIndex};

pub use menu_fields::{
    MCP_D750_IMAGE_LENGTH, MCP_D750_MENU_FIELDS, MCP_D750_SCHEMA_VERSION, MCP_D750_SLOT_COUNT,
    MCP_D750_SLOT_STRIDE, MCP_D750_SOURCE_SHA256, MenuField, MenuOption, StorageTransform,
    menu_field,
};
pub use schema::{
    DecodedFieldValue, Endian, FieldCodec, FieldDescriptor, FieldValue, PatchPlanner, PatchSet,
    SLOT_TERM, StringEncoding, Term,
};

/// Model whose layout the generated registry describes.
pub const MCP_D750_SCHEMA_MODEL: &str = "TM-D750";
/// Firmware release whose layout the generated registry describes.
pub const MCP_D750_SCHEMA_FIRMWARE: &str = "1.00";
/// Exact `FV` identities whose layout matches the registry. The first
/// hardware `FV` reply fixes the format; a new entry and a new manifest
/// release land together.
pub const MCP_D750_SCHEMA_FIRMWARE_IDENTITIES: &[&str] = &["1.00"];

/// Whether a proven identity matches the registry's target.
#[must_use]
pub fn is_supported_schema_target(model: RadioModel, firmware: &FirmwareIdentity) -> bool {
    model == RadioModel::TmD750 && MCP_D750_SCHEMA_FIRMWARE_IDENTITIES.contains(&firmware.as_str())
}

/// The full 1,929,472-byte memory image.
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

    /// An image of `0xFF` bytes (erased flash).
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
        for (offset, mask, bits) in field.encode(value)? {
            let byte =
                self.bytes
                    .get_mut(start + offset)
                    .ok_or_else(|| SchemaError::OutOfBounds {
                        field: field.name,
                        address: u64::try_from(start + offset).unwrap_or(u64::MAX),
                        len: 1,
                        image_length: IMAGE_LENGTH,
                    })?;
            *byte = (*byte & !mask) | (bits & mask);
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
