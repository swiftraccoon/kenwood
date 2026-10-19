//! Names for many memory channels, planned as whole name-table page writes.
//!
//! A channel's name is sixteen bytes at [`PhysicalChannel::name_address`],
//! sixteen names to a 256-byte page of the name table; only an MCP write sets
//! it. Store the records themselves over CAT with
//! [`crate::Radio::program_memory_channels`], which lets the radio derive each
//! band code.

use std::collections::BTreeMap;

use crate::error::{Error, ValidationError};
use crate::memory::{ChannelNameText, MemoryImage};
use crate::radio::programming::PageReplacement;
use crate::types::{Address, CHANNEL_NAME_SIZE, PAGE_SIZE, Page, PhysicalChannel};

/// The names of a set of channels; `None` leaves a channel unnamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelNamePlan {
    names: BTreeMap<PhysicalChannel, Option<ChannelNameText>>,
}

impl ChannelNamePlan {
    /// Collect one name per channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DuplicateChannelName`] when a channel
    /// appears twice.
    pub fn new(
        names: impl IntoIterator<Item = (PhysicalChannel, Option<ChannelNameText>)>,
    ) -> Result<Self, ValidationError> {
        let mut plan = BTreeMap::new();
        for (channel, name) in names {
            if plan.insert(channel, name).is_some() {
                return Err(ValidationError::DuplicateChannelName {
                    index: channel.index(),
                });
            }
        }
        Ok(Self { names: plan })
    }

    /// The name-table pages the plan touches, each once, in address order.
    #[must_use]
    pub fn pages(&self) -> Vec<Page> {
        let mut pages: Vec<Page> = Vec::new();
        for channel in self.names.keys() {
            if let Some(page) = name_page(*channel)
                && pages.last() != Some(&page)
            {
                pages.push(page);
            }
        }
        pages
    }

    /// The writes of the touched pages whose bytes change, each keeping the
    /// other names on its page as `image` holds them.
    ///
    /// Apply them with [`crate::radio::McpSession::compare_exchange_pages`],
    /// or let [`crate::radio::McpSession::write_channel_names`] read the
    /// pages fresh first.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(&self, image: &MemoryImage) -> Result<Vec<PageReplacement>, Error> {
        let mut replacements = Vec::new();
        for page in self.pages() {
            let start = page.address().as_usize();
            let expected = image
                .as_bytes()
                .get(start..start + PAGE_SIZE)
                .unwrap_or_default();
            let mut replacement = expected.to_vec();
            for (channel, name) in &self.names {
                if name_page(*channel) != Some(page) {
                    continue;
                }
                let field = name
                    .as_ref()
                    .map_or([0; CHANNEL_NAME_SIZE], ChannelNameText::to_field);
                let offset = channel.name_address() - start;
                for (destination, source) in replacement.iter_mut().skip(offset).zip(field) {
                    *destination = source;
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, expected, &replacement)?);
            }
        }
        Ok(replacements)
    }
}

/// The 256-byte page holding `channel`'s name.
fn name_page(channel: PhysicalChannel) -> Option<Page> {
    let start = channel.name_address() / PAGE_SIZE * PAGE_SIZE;
    let address = Address::new(u32::try_from(start).ok()?).ok()?;
    Page::new(address, PAGE_SIZE).ok()
}

#[cfg(test)]
#[path = "channel_names_tests.rs"]
mod tests;
