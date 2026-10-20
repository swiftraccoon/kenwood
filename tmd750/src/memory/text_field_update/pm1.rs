//! The global PM1 name: sixteen NUL-padded bytes pinned to the generated
//! `pm.PmName1` descriptor and its canonical page.

use super::{
    NoGuards, TextField, TextFieldUpdate, TextFieldUpdateError, TextValue, check_identity,
    complete_page, replace_field, sealed,
};
use crate::memory::{MenuField, menu_field};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{PAGE_SIZE, Page};
use kenwood_schema::{CodecSpec, FieldValue, StringEncoding};

#[cfg(test)]
#[path = "pm1_tests.rs"]
mod tests;

pub(super) const FIELD_NAME: &str = "pm.PmName1";
const FIELD_ADDRESS: u32 = 323_594;
const FIELD_LENGTH: usize = 16;
const PAGE_ADDRESS: u32 = 323_584;
const RULE: &str = "1 to 16 printable ASCII bytes";

/// A nonempty PM1 name containing at most sixteen printable ASCII bytes.
///
/// Spaces are preserved exactly, including leading and trailing spaces. The
/// accepted bytes are ASCII graphic characters and the space.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pm1Name(String);

impl Pm1Name {
    /// Validate a name without truncation, trimming, or replacement encoding.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::InvalidText`] for empty, oversized,
    /// control, non-ASCII, or embedded-NUL input.
    pub fn new(value: &str) -> Result<Self, TextFieldUpdateError> {
        Self::validate(value)?;
        Ok(Self(value.to_owned()))
    }

    fn validate(text: &str) -> Result<(), TextFieldUpdateError> {
        if text.is_empty()
            || text.len() > FIELD_LENGTH
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(TextFieldUpdateError::InvalidText {
                field: "PM1 name",
                rule: RULE,
            });
        }
        Ok(())
    }

    /// The exact validated text, with spaces preserved.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Pm1Name {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::str::FromStr for Pm1Name {
    type Err = TextFieldUpdateError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Pm1Name {
    type Error = TextFieldUpdateError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<String> for Pm1Name {
    type Error = TextFieldUpdateError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<Pm1Name> for String {
    fn from(value: Pm1Name) -> Self {
        value.0
    }
}

impl std::fmt::Display for Pm1Name {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl sealed::Sealed for Pm1Name {}

impl TextValue for Pm1Name {
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl TextFieldUpdate<Pm1Name, NoGuards> {
    /// Resolve the complete canonical page required from a validated capture.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::UnsupportedDescriptor`] if the generated
    /// PM1 descriptor or canonical page no longer has the pinned shape.
    pub fn required_page() -> Result<Page, TextFieldUpdateError> {
        supported_page(menu_field(FIELD_NAME).ok_or(TextFieldUpdateError::UnsupportedDescriptor)?)
    }

    /// Prepare a compare-before-write update without opening a radio.
    ///
    /// `baseline` must contain the complete observed canonical page from a
    /// validated capture, not synthesized gap bytes. `expected_current` must
    /// match its exact NUL-padded PM1 field.
    ///
    /// # Errors
    ///
    /// Rejects an unsupported identity or descriptor, an incomplete page, a
    /// mismatched current name, or identical current and desired names. No-op
    /// requests return [`TextFieldUpdateError::NoChange`] before any I/O is
    /// possible.
    pub fn prepare(
        identity: &Identity,
        baseline: &[u8],
        expected_current: &Pm1Name,
        desired: &Pm1Name,
    ) -> Result<Self, TextFieldUpdateError> {
        let field = menu_field(FIELD_NAME).ok_or(TextFieldUpdateError::UnsupportedDescriptor)?;
        Self::prepare_with_field(identity, baseline, expected_current, desired, field)
    }

    fn prepare_with_field(
        identity: &Identity,
        baseline: &[u8],
        expected_current: &Pm1Name,
        desired: &Pm1Name,
        field: &MenuField,
    ) -> Result<Self, TextFieldUpdateError> {
        check_identity(identity)?;
        let page = supported_page(field)?;
        let original = complete_page(baseline)?;
        let offset = field
            .descriptor
            .address(None)
            .map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?
            .as_usize()
            - page.address().as_usize();
        let desired_page = replace_field(
            &original,
            offset..offset + FIELD_LENGTH,
            &encode_name(field, expected_current)?,
            &encode_name(field, desired)?,
            expected_current == desired,
        )?;
        Ok(Self::new(
            TextField::Pm1Name,
            identity.clone(),
            page,
            original,
            desired_page,
            Some(expected_current.clone()),
            Some(desired.clone()),
            (),
        ))
    }
}

fn supported_page(field: &MenuField) -> Result<Page, TextFieldUpdateError> {
    if field.descriptor.name != FIELD_NAME
        || field.descriptor.base != FIELD_ADDRESS
        || !field.descriptor.terms.is_empty()
        || field.descriptor.codec.spec()
            != (CodecSpec::FixedString {
                len: FIELD_LENGTH,
                encoding: StringEncoding::Utf8,
                padding: 0,
            })
        || field.menu != "pm"
        || field.enum_type.is_some()
        || !field.options.is_empty()
        || !field.allowed_values.is_empty()
        || field.storage_transform.is_some()
        || field.is_blob
    {
        return Err(TextFieldUpdateError::UnsupportedDescriptor);
    }
    field
        .descriptor
        .address(None)
        .ok()
        .and_then(writable_page_for)
        .filter(|page| page.address().as_u32() == PAGE_ADDRESS && page.len() == PAGE_SIZE)
        .ok_or(TextFieldUpdateError::UnsupportedDescriptor)
}

fn encode_name(
    field: &MenuField,
    name: &Pm1Name,
) -> Result<[u8; FIELD_LENGTH], TextFieldUpdateError> {
    let encoded = field
        .descriptor
        .encode(FieldValue::Text(name.as_str()))
        .map_err(|_| TextFieldUpdateError::UnsupportedDescriptor)?;
    if encoded.len() != FIELD_LENGTH {
        return Err(TextFieldUpdateError::UnsupportedDescriptor);
    }
    let mut bytes = [0; FIELD_LENGTH];
    for (index, patch) in encoded.into_iter().enumerate() {
        if index != patch.offset() || patch.mask() != u8::MAX {
            return Err(TextFieldUpdateError::UnsupportedDescriptor);
        }
        *bytes
            .get_mut(patch.offset())
            .ok_or(TextFieldUpdateError::UnsupportedDescriptor)? = patch.value();
    }
    Ok(bytes)
}
