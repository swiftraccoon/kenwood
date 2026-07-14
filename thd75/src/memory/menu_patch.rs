//! Safe value planning for generated MCP-D75 menu metadata.

use super::{FieldValue, MenuField, PatchPlanner, SchemaError};

impl MenuField {
    /// Validate and add a value to a masked patch plan.
    ///
    /// In addition to storage-codec validation, this rejects gap values for
    /// enum domains. Some official MCP-D75 enums are non-contiguous, so their
    /// numeric minimum and maximum alone are not sufficient validation.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::DisallowedValue`] for a raw value absent from an
    /// official enum or finite UI-choice domain. Other failures report codec
    /// type, range, length, encoding, or overlapping-patch errors.
    pub fn plan_value(
        &self,
        planner: &mut PatchPlanner,
        value: FieldValue<'_>,
    ) -> Result<(), SchemaError> {
        if let FieldValue::Unsigned(raw) = value {
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
    fn declared_enum_member_is_planned() -> Result<(), Box<dyn std::error::Error>> {
        let field = gapped_field()?;
        let mut planner = PatchPlanner::new();
        field.plan_value(&mut planner, FieldValue::Unsigned(2))?;
        let patches = planner.finish()?;
        assert_eq!(patches.pages().count(), 1);
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
        Ok(())
    }
}
