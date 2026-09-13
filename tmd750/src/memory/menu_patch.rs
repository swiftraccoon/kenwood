//! Domain validation and reads for generated menu fields.

use super::FieldAccess;
use super::menu_fields::MenuField;
use super::schema::{DecodedFieldValue, FieldCodec, FieldValue, PatchPlanner};
use crate::error::SchemaError;
use crate::types::SlotIndex;
use kenwood_schema::{ChoiceError, validate_choices};

/// Failure to parse or validate one scalar menu value.
///
/// Input text is never included in parser diagnostics because a menu field may
/// hold private credentials. Numeric domain and storage errors retain the
/// existing typed schema diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MenuValueError {
    /// Blobs and byte-array codecs have no scalar keyboard representation.
    #[error("{field} is a binary field; scalar menu input is not supported")]
    UnsupportedBinaryField {
        /// Registry field whose storage is not scalar.
        field: &'static str,
    },
    /// Boolean input did not match any documented alias.
    #[error("{field} requires true/false, on/off, yes/no, or 1/0")]
    InvalidBoolean {
        /// Registry field requiring a boolean value.
        field: &'static str,
    },
    /// Unsigned input was not a decimal or hexadecimal integer in range.
    #[error("{field} requires an unsigned decimal or 0x-prefixed integer")]
    InvalidUnsigned {
        /// Registry field requiring an unsigned value.
        field: &'static str,
    },
    /// Signed input was not a decimal integer in range.
    #[error("{field} requires a signed decimal integer")]
    InvalidSigned {
        /// Registry field requiring a signed value.
        field: &'static str,
    },
    /// Enum text matched neither an unsigned integer nor a public label.
    #[error("{field} requires a raw enum value or an exact public option label")]
    UnknownOption {
        /// Registry field whose public options did not match.
        field: &'static str,
    },
    /// More than one public option label matched case-insensitively.
    #[error("{field} has an ambiguous public option label; use its raw enum value")]
    AmbiguousOption {
        /// Registry field with non-unique public labels.
        field: &'static str,
    },
    /// Text contained NUL or the field's configured string-padding byte.
    #[error("{field} text contains a storage terminator and cannot round-trip exactly")]
    EmbeddedTerminator {
        /// Registry field whose text would terminate before its end.
        field: &'static str,
    },
    /// The parsed scalar violated the field's enum, choice, or storage domain.
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

impl MenuField {
    /// Parse keyboard input and validate it against this scalar menu field.
    ///
    /// Unsigned integers accept decimal or a `0x`/`0X` hexadecimal prefix;
    /// signed integers accept decimal only. Enum fields also accept one unique
    /// public display label, matched case-insensitively without trimming. Private
    /// enum member names and language-resource keys are never input aliases.
    /// Boolean aliases are `true`/`false`, `on`/`off`, `yes`/`no`, and `1`/`0`,
    /// with ASCII case ignored. Strings preserve their exact text and spacing.
    ///
    /// Numeric inputs are raw stored integers, not display units. This method
    /// does not apply scaling metadata, guess units, truncate text, establish
    /// firmware compatibility, or authorize a radio write. Blobs and byte arrays
    /// are not scalar input. A string containing NUL or its configured padding
    /// byte is rejected because decoding would stop at that byte.
    ///
    /// # Errors
    ///
    /// Returns [`MenuValueError`] for invalid syntax, ambiguous or missing public
    /// labels, binary fields, embedded text terminators, and any writable-domain
    /// or codec error from the parsed value. No input text is included in parser
    /// diagnostics.
    pub fn parse_value(&self, text: &str) -> Result<DecodedFieldValue, MenuValueError> {
        let field = self.descriptor.name;
        if self.is_blob || matches!(self.descriptor.codec, FieldCodec::Bytes { .. }) {
            return Err(MenuValueError::UnsupportedBinaryField { field });
        }
        let value = match self.descriptor.codec {
            FieldCodec::Bool | FieldCodec::BitBool { .. } => {
                let flag = if ["true", "on", "yes", "1"]
                    .iter()
                    .any(|alias| text.eq_ignore_ascii_case(alias))
                {
                    true
                } else if ["false", "off", "no", "0"]
                    .iter()
                    .any(|alias| text.eq_ignore_ascii_case(alias))
                {
                    false
                } else {
                    return Err(MenuValueError::InvalidBoolean { field });
                };
                DecodedFieldValue::Bool(flag)
            }
            FieldCodec::Byte { .. } | FieldCodec::BitField { .. } | FieldCodec::Unsigned { .. } => {
                DecodedFieldValue::Unsigned(self.parse_unsigned(text)?)
            }
            FieldCodec::Signed { .. } => DecodedFieldValue::Signed(
                text.parse()
                    .map_err(|_| MenuValueError::InvalidSigned { field })?,
            ),
            FieldCodec::FixedString { padding, .. } => {
                if text
                    .as_bytes()
                    .iter()
                    .any(|byte| *byte == 0 || *byte == padding)
                {
                    return Err(MenuValueError::EmbeddedTerminator { field });
                }
                DecodedFieldValue::Text(text.to_owned())
            }
            FieldCodec::Bytes { .. } => {
                return Err(MenuValueError::UnsupportedBinaryField { field });
            }
        };
        self.validate_patch_value(value.as_field_value())?;
        let _encoded = self.descriptor.encode(value.as_field_value())?;
        Ok(value)
    }

    fn parse_unsigned(&self, text: &str) -> Result<u64, MenuValueError> {
        let parsed = text
            .strip_prefix("0x")
            .or_else(|| text.strip_prefix("0X"))
            .map_or_else(|| text.parse(), |digits| u64::from_str_radix(digits, 16));
        if let Ok(value) = parsed {
            return Ok(value);
        }
        let field = self.descriptor.name;
        if self.options.is_empty() {
            return Err(MenuValueError::InvalidUnsigned { field });
        }
        let mut matching = self.options.iter().filter(|option| {
            option
                .label
                .is_some_and(|label| text.eq_ignore_ascii_case(label))
        });
        let first = matching
            .next()
            .ok_or(MenuValueError::UnknownOption { field })?;
        if matching.next().is_some() {
            return Err(MenuValueError::AmbiguousOption { field });
        }
        Ok(first.raw)
    }

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
        self.validate_value_domain(value)
    }

    /// Validate finite enum and choice domains without deciding radio-write policy.
    ///
    /// Pure image encoding also uses this check. A registered binary field may
    /// be encoded offline even when scalar page planning does not admit it.
    pub(crate) fn validate_value_domain(&self, value: FieldValue<'_>) -> Result<(), SchemaError> {
        validate_choices(
            value,
            self.options.iter().map(|option| option.raw),
            self.allowed_values,
        )
        .map_err(|error| match error {
            ChoiceError::TypeMismatch { actual } => SchemaError::TypeMismatch {
                field: self.descriptor.name,
                expected: "unsigned",
                actual,
            },
            ChoiceError::DisallowedValue { value } => SchemaError::DisallowedValue {
                field: self.descriptor.name,
                value,
            },
        })
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

#[cfg(test)]
#[path = "menu_patch_tests.rs"]
mod tests;
