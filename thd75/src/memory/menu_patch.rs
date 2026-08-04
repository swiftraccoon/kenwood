//! Safe value planning for generated MCP-D75 menu metadata.

use super::{DecodedFieldValue, FieldValue, MenuField, PatchPlanner, SchemaError};

impl MenuField {
    pub(super) fn validate_patch_value(&self, value: FieldValue<'_>) -> Result<(), SchemaError> {
        if !self.options.is_empty() || !self.allowed_values.is_empty() {
            let FieldValue::Unsigned(raw) = value else {
                return Err(SchemaError::TypeMismatch {
                    field: self.descriptor.name,
                    expected: "unsigned",
                    actual: value.kind_name(),
                });
            };
            let missing_enum = !self.options.is_empty() && self.option(raw).is_none();
            let missing_choice =
                !self.allowed_values.is_empty() && !self.allowed_values.contains(&raw);
            if missing_enum || missing_choice {
                return Err(SchemaError::DisallowedValue {
                    field: self.descriptor.name,
                    value: raw,
                });
            }
        }
        Ok(())
    }

    /// Decode and validate this field from a complete MCP memory image.
    ///
    /// This enforces both the storage codec and any finite enum or UI-choice
    /// domain declared by the generated MCP-D75 catalog. A corrupt raw value
    /// is therefore reported instead of being presented as a valid setting.
    ///
    /// # Errors
    ///
    /// Returns an error when the field is outside the image, its descriptor is
    /// malformed, or its stored value is outside the catalog's accepted
    /// domain.
    pub fn read(&self, image: &[u8]) -> Result<DecodedFieldValue, SchemaError> {
        self.descriptor.read(image)
    }

    /// Validate and add a value to a masked patch plan.
    ///
    /// In addition to storage-codec validation, this rejects gap values for
    /// enum domains. Some official MCP-D75 enums are non-contiguous, so their
    /// numeric minimum and maximum alone are not sufficient validation. A
    /// field with a finite enum or UI-choice domain accepts only
    /// [`FieldValue::Unsigned`]; every other value kind is rejected before
    /// codec validation, so the domain gate cannot be bypassed by a
    /// mismatched value variant.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::DisallowedValue`] for a raw value absent from an
    /// official enum or finite UI-choice domain, and
    /// [`SchemaError::TypeMismatch`] for a non-unsigned value supplied to a
    /// finite-domain field. Other failures report codec type, range, length,
    /// encoding, or overlapping-patch errors.
    pub fn plan_value(
        &self,
        planner: &mut PatchPlanner,
        value: FieldValue<'_>,
    ) -> Result<(), SchemaError> {
        self.validate_patch_value(value)?;
        let _planner = planner.set(&self.descriptor, value)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::menu_field;

    fn gapped_field() -> Result<&'static MenuField, &'static str> {
        menu_field("radio.AutoWeatherScan").ok_or("generated test field missing")
    }

    #[test]
    fn gapped_enum_rejects_value_between_min_and_max() -> Result<(), &'static str> {
        let field = gapped_field()?;
        let mut planner = PatchPlanner::new();
        assert_eq!(
            field.plan_value(&mut planner, FieldValue::Unsigned(3)),
            Err(SchemaError::DisallowedValue {
                field: "radio.AutoWeatherScan",
                value: 3,
            })
        );
        Ok(())
    }

    #[test]
    fn gapped_enum_rejects_invalid_stored_value() -> Result<(), &'static str> {
        let field = gapped_field()?;
        let mut image = vec![0; field.descriptor.offset + 1];
        let stored = image
            .get_mut(field.descriptor.offset)
            .ok_or("generated field offset missing")?;
        *stored = 3;

        assert_eq!(
            field.read(&image),
            Err(SchemaError::DisallowedValue {
                field: "radio.AutoWeatherScan",
                value: 3,
            })
        );
        assert_eq!(
            field.descriptor.read(&image),
            Err(SchemaError::DisallowedValue {
                field: "radio.AutoWeatherScan",
                value: 3,
            })
        );
        Ok(())
    }

    #[test]
    fn low_level_planner_cannot_bypass_gapped_enum_domain() -> Result<(), &'static str> {
        let field = gapped_field()?;
        let mut planner = PatchPlanner::new();
        assert!(
            matches!(
                planner.set(&field.descriptor, FieldValue::Unsigned(3)),
                Err(SchemaError::DisallowedValue {
                    field: "radio.AutoWeatherScan",
                    value: 3,
                })
            ),
            "the descriptor-level API must enforce generated enum membership"
        );
        assert!(
            planner
                .finish()
                .map_err(|_| "empty plan did not finish")?
                .is_empty(),
            "a rejected value must not leave a partial patch"
        );
        Ok(())
    }

    #[test]
    fn declared_enum_member_is_planned() -> Result<(), Box<dyn std::error::Error>> {
        let field = gapped_field()?;
        let mut planner = PatchPlanner::new();
        field.plan_value(&mut planner, FieldValue::Unsigned(2))?;
        let patches = planner.finish()?;
        assert_eq!(patches.pages().count(), 1);
        Ok(())
    }

    #[test]
    fn finite_domain_fields_reject_non_unsigned_values() -> Result<(), &'static str> {
        let field = gapped_field()?;
        let mut planner = PatchPlanner::new();
        let result = field.plan_value(&mut planner, FieldValue::Bool(true));
        assert!(
            matches!(
                result,
                Err(SchemaError::TypeMismatch {
                    expected: "unsigned",
                    ..
                })
            ),
            "finite-domain fields must only accept unsigned raw values: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn non_contiguous_ui_choices_reject_gaps() -> Result<(), Box<dyn std::error::Error>> {
        let field = menu_field("radio.GroupLink0").ok_or("generated choice field missing")?;
        let mut invalid = PatchPlanner::new();
        assert_eq!(
            field.plan_value(&mut invalid, FieldValue::Unsigned(30)),
            Err(SchemaError::DisallowedValue {
                field: "radio.GroupLink0",
                value: 30,
            })
        );

        let mut valid = PatchPlanner::new();
        field.plan_value(&mut valid, FieldValue::Unsigned(255))?;
        assert_eq!(valid.finish()?.pages().count(), 1);

        let mut low_level = PatchPlanner::new();
        assert!(
            matches!(
                low_level.set(&field.descriptor, FieldValue::Unsigned(30)),
                Err(SchemaError::DisallowedValue {
                    field: "radio.GroupLink0",
                    value: 30,
                })
            ),
            "the descriptor-level API must enforce generated finite choices"
        );
        Ok(())
    }

    #[test]
    fn catalog_name_with_mismatched_descriptor_is_rejected() -> Result<(), &'static str> {
        let field = gapped_field()?;
        let stale = super::super::FieldDescriptor::new(
            field.descriptor.name,
            field.descriptor.offset + 1,
            field.descriptor.codec,
        );
        let mut planner = PatchPlanner::new();
        assert!(
            matches!(
                planner.set(&stale, FieldValue::Unsigned(2)),
                Err(SchemaError::CatalogDescriptorMismatch {
                    field: "radio.AutoWeatherScan",
                    offset,
                    expected_offset,
                }) if offset == field.descriptor.offset + 1
                    && expected_offset == field.descriptor.offset
            ),
            "a catalog name with stale descriptor metadata must fail closed"
        );
        Ok(())
    }
}
