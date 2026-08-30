//! Domain validation and reads for generated menu fields.

use super::FieldAccess;
use super::menu_fields::MenuField;
use super::schema::{DecodedFieldValue, FieldValue, PatchPlanner};
use crate::error::SchemaError;
use crate::types::SlotIndex;

impl MenuField {
    /// Reject values outside the field's enum members or allowed choices.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::BlobNotPatchable`] for a blob,
    /// [`SchemaError::TypeMismatch`] for a non-unsigned value on a choice
    /// field, and [`SchemaError::DisallowedValue`] for a value outside the
    /// domain.
    pub fn validate_patch_value(&self, value: FieldValue<'_>) -> Result<(), SchemaError> {
        if self.is_blob {
            return Err(SchemaError::BlobNotPatchable {
                field: self.descriptor.name,
            });
        }
        if self.options.is_empty() && self.allowed_values.is_empty() {
            return Ok(());
        }
        let FieldValue::Unsigned(raw) = value else {
            return Err(SchemaError::TypeMismatch {
                field: self.descriptor.name,
                expected: "unsigned",
                actual: value.kind_name(),
            });
        };
        let missing_option = !self.options.is_empty() && self.option(raw).is_none();
        let missing_choice = !self.allowed_values.is_empty() && !self.allowed_values.contains(&raw);
        if missing_option || missing_choice {
            return Err(SchemaError::DisallowedValue {
                field: self.descriptor.name,
                value: raw,
            });
        }
        Ok(())
    }

    /// Decode this field through `access`.
    ///
    /// # Errors
    ///
    /// Address and decode errors.
    pub fn read(&self, access: &FieldAccess<'_>) -> Result<DecodedFieldValue, SchemaError> {
        access.read(&self.descriptor)
    }
}

impl PatchPlanner {
    /// Plan a registry field after domain validation.
    ///
    /// # Errors
    ///
    /// Validation errors from [`MenuField::validate_patch_value`], then the
    /// errors of [`PatchPlanner::set`].
    pub fn set_menu(
        &mut self,
        field: &MenuField,
        slot: Option<SlotIndex>,
        value: FieldValue<'_>,
    ) -> Result<&mut Self, SchemaError> {
        field.validate_patch_value(value)?;
        self.set(&field.descriptor, slot, value)
    }
}
