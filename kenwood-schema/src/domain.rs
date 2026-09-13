//! Finite writable domains, independent of generated catalog representation.

use crate::FieldValue;

/// A value does not belong to a finite writable domain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChoiceError {
    /// Finite numeric choices require an unsigned value.
    #[error("finite choices require unsigned input, received {actual}")]
    TypeMismatch {
        /// Kind of the rejected value.
        actual: &'static str,
    },
    /// At least one nonempty domain excludes the value.
    #[error("raw value {value} is not an allowed choice")]
    DisallowedValue {
        /// Rejected unsigned value.
        value: u64,
    },
}

/// Validate the intersection of optional enum and explicit-choice domains.
///
/// An empty domain imposes no restriction. If both are empty, any value kind
/// passes this check; its storage codec must still validate it. Otherwise an
/// unsigned value must belong to every nonempty domain. Input order and
/// duplicate entries do not change membership. No allocation is required.
///
/// # Errors
///
/// Returns [`ChoiceError::TypeMismatch`] for a non-unsigned constrained value,
/// or [`ChoiceError::DisallowedValue`] when an applicable domain excludes it.
pub fn validate_choices(
    value: FieldValue<'_>,
    enum_values: impl IntoIterator<Item = u64>,
    allowed_values: &[u64],
) -> Result<(), ChoiceError> {
    let mut options = enum_values.into_iter().peekable();
    let has_options = options.peek().is_some();
    if !has_options && allowed_values.is_empty() {
        return Ok(());
    }
    let FieldValue::Unsigned(raw) = value else {
        return Err(ChoiceError::TypeMismatch {
            actual: value.kind_name(),
        });
    };
    if (has_options && !options.any(|candidate| candidate == raw))
        || (!allowed_values.is_empty() && !allowed_values.contains(&raw))
    {
        return Err(ChoiceError::DisallowedValue { value: raw });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_nonempty_domains_must_admit_the_value() -> Result<(), ChoiceError> {
        let options = [1, 3, 3, 9];
        let choices = [3, 7];
        validate_choices(FieldValue::Unsigned(3), options, &choices)?;
        for rejected in [1, 2, 7, 9] {
            assert_eq!(
                validate_choices(FieldValue::Unsigned(rejected), options, &choices),
                Err(ChoiceError::DisallowedValue { value: rejected })
            );
        }
        Ok(())
    }

    #[test]
    fn empty_domains_are_unconstrained() -> Result<(), ChoiceError> {
        validate_choices(FieldValue::Text("model text"), [], &[])?;
        validate_choices(FieldValue::Unsigned(3), [1, 3], &[])?;
        validate_choices(FieldValue::Unsigned(3), [], &[1, 3])?;
        assert!(validate_choices(FieldValue::Unsigned(2), [], &[1, 3]).is_err());
        assert!(validate_choices(FieldValue::Unsigned(2), [1, 3], &[]).is_err());
        Ok(())
    }

    #[test]
    fn constrained_domains_never_accept_another_value_kind() {
        for (options, choices) in [(&[1][..], &[][..]), (&[][..], &[1][..])] {
            assert_eq!(
                validate_choices(FieldValue::Bool(true), options.iter().copied(), choices),
                Err(ChoiceError::TypeMismatch { actual: "boolean" })
            );
        }
    }
}
