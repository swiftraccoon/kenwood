//! One physical channel's sixteen name bytes inside its complete name-table
//! page.

use super::{
    NoGuards, TextField, TextFieldUpdate, TextFieldUpdateError, TextValue, check_identity,
    complete_page, replace_field, sealed,
};
use crate::protocol::mcp::regions::writable_page_for;
use crate::radio::Identity;
use crate::types::{
    Address, CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET, PAGE_SIZE, Page, PhysicalChannel,
};

pub(super) const FIELD_NAME: &str = "memory.ChannelName";
const RULE: &str = "1 to 16 printable ASCII bytes";

/// A nonempty channel name containing at most sixteen printable ASCII bytes.
///
/// Spaces are preserved exactly, including leading and trailing spaces. The
/// accepted bytes are ASCII graphic characters and the space. The stored
/// field is the text followed by NUL padding to sixteen bytes; an unnamed
/// channel stores sixteen NUL bytes and is represented by `None` wherever an
/// `Option<&ChannelNameText>` is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNameText(String);

impl ChannelNameText {
    /// Validate a name without truncation, trimming, or replacement encoding.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::InvalidText`] for empty, oversized,
    /// control, non-ASCII, or embedded-NUL input.
    pub fn new(value: &str) -> Result<Self, TextFieldUpdateError> {
        if value.is_empty()
            || value.len() > CHANNEL_NAME_SIZE
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_graphic() || byte == b' ')
        {
            return Err(TextFieldUpdateError::InvalidText {
                field: "channel name",
                rule: RULE,
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// The exact validated text, with spaces preserved.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The sixteen stored bytes: the text followed by NUL padding.
    #[must_use]
    pub fn to_field(&self) -> [u8; CHANNEL_NAME_SIZE] {
        let mut field = [0; CHANNEL_NAME_SIZE];
        for (slot, byte) in field.iter_mut().zip(self.0.bytes()) {
            *slot = byte;
        }
        field
    }
}

impl sealed::Sealed for ChannelNameText {}

impl TextValue for ChannelNameText {
    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The stored field of an optional name; `None` is the all-NUL field of an
/// unnamed channel.
fn encode_field(name: Option<&ChannelNameText>) -> [u8; CHANNEL_NAME_SIZE] {
    name.map_or([0; CHANNEL_NAME_SIZE], ChannelNameText::to_field)
}

impl TextFieldUpdate<ChannelNameText, NoGuards> {
    /// Resolve the complete name-table page that holds `channel`'s name.
    ///
    /// Sixteen names share each 256-byte page of the table at
    /// [`CHANNEL_NAMES_OFFSET`]; the returned page starts on that grid and
    /// lies inside the writable global settings region.
    ///
    /// # Errors
    ///
    /// Returns [`TextFieldUpdateError::UnsupportedPage`] if the name address
    /// does not resolve to a complete page on the name-table grid.
    pub fn required_page(channel: PhysicalChannel) -> Result<Page, TextFieldUpdateError> {
        let name_address = channel.name_address();
        let address = u32::try_from(name_address)
            .ok()
            .and_then(|address| Address::new(address).ok())
            .ok_or(TextFieldUpdateError::UnsupportedPage)?;
        let page = writable_page_for(address).ok_or(TextFieldUpdateError::UnsupportedPage)?;
        let grid_start =
            CHANNEL_NAMES_OFFSET + (name_address - CHANNEL_NAMES_OFFSET) / PAGE_SIZE * PAGE_SIZE;
        if page.len() != PAGE_SIZE || page.address().as_usize() != grid_start {
            return Err(TextFieldUpdateError::UnsupportedPage);
        }
        Ok(page)
    }

    /// Prepare a compare-before-write update without opening a radio.
    ///
    /// `baseline` must contain the complete observed page returned by
    /// [`Self::required_page`] for `channel`, taken from a validated capture,
    /// not synthesized gap bytes. `expected_current` must match its exact
    /// NUL-padded name field; `None` is the all-NUL field of an unnamed
    /// channel. `desired` replaces that field; `None` clears the name.
    ///
    /// # Errors
    ///
    /// Rejects an unsupported identity or page, an incomplete baseline, a
    /// mismatched current name, or identical current and desired names. No-op
    /// requests return [`TextFieldUpdateError::NoChange`] before any I/O is
    /// possible.
    pub fn prepare(
        identity: &Identity,
        baseline: &[u8],
        channel: PhysicalChannel,
        expected_current: Option<&ChannelNameText>,
        desired: Option<&ChannelNameText>,
    ) -> Result<Self, TextFieldUpdateError> {
        check_identity(identity)?;
        let page = Self::required_page(channel)?;
        let original = complete_page(baseline)?;
        let offset = channel.name_address() - page.address().as_usize();
        let desired_page = replace_field(
            &original,
            offset..offset + CHANNEL_NAME_SIZE,
            &encode_field(expected_current),
            &encode_field(desired),
            expected_current == desired,
        )?;
        Ok(Self::new(
            TextField::ChannelName(channel),
            identity.clone(),
            page,
            original,
            desired_page,
            expected_current.cloned(),
            desired.cloned(),
            (),
        ))
    }

    /// The physical channel whose name changes.
    #[must_use]
    pub const fn channel(&self) -> PhysicalChannel {
        match self.field {
            TextField::ChannelName(channel) => channel,
            // `prepare` is the only constructor of this alias and always
            // stores a channel field.
            TextField::Pm1Name | TextField::PmOffMy1Callsign => unreachable!(),
        }
    }
}

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
