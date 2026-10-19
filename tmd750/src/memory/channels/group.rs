//! The names of the thirty memory groups that organize the regular channels,
//! which the radio shows when a group is selected.
//!
//! The names follow the CALL channel names in the name table: groups 0 to 29
//! at name-table indexes 1152 to 1181, then the name of the weather
//! channels' group at 1182, sixteen bytes each and NUL padded. On firmware
//! 1.02, type `K,2,1`, a radio whose group names were never edited held
//! `GRP-0` to `GRP-29` and `Weather`; two names written over the
//! operation-panel USB read back in place in a fresh session, changing only
//! their own bytes, and writing the defaults back restored every byte of the
//! standard backup. Whether the radio displays a written name is not
//! established.

use std::collections::BTreeMap;
use std::fmt;

use crate::error::{Error, ValidationError};
use crate::memory::{ChannelNameText, MemoryImage};
use crate::protocol::mcp::regions;
use crate::radio::programming::PageReplacement;
use crate::types::stored::name_address_at;
use crate::types::{Address, CHANNEL_NAME_SIZE, Page};

/// Memory groups.
pub const MEMORY_GROUPS: u8 = 30;

/// Name-table index of group 0's name.
const FIRST_NAME_INDEX: u16 = 1152;
/// Name-table index of the weather channels' group name.
pub(super) const WEATHER_GROUP_NAME_INDEX: u16 = 1182;

/// A memory group, 0 to 29.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryGroup(u8);

impl MemoryGroup {
    /// Group `index`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] at or above
    /// [`MEMORY_GROUPS`].
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index >= MEMORY_GROUPS {
            Err(ValidationError::SettingOutOfRange {
                setting: "memory group",
                value: index as u64,
                max: (MEMORY_GROUPS - 1) as u64,
            })
        } else {
            Ok(Self(index))
        }
    }

    /// The group number.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Image address of the group's name.
    #[must_use]
    pub const fn name_address(self) -> usize {
        name_address_at(FIRST_NAME_INDEX + self.0 as u16)
    }

    /// Every group in order.
    pub fn all() -> impl Iterator<Item = Self> {
        (0..MEMORY_GROUPS).map(Self)
    }
}

impl fmt::Display for MemoryGroup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "group {}", self.0)
    }
}

/// New names for a set of memory groups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNamePlan {
    names: BTreeMap<MemoryGroup, ChannelNameText>,
}

impl GroupNamePlan {
    /// Collect one name per group.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DuplicateGroupName`] when a group appears
    /// twice.
    pub fn new(
        names: impl IntoIterator<Item = (MemoryGroup, ChannelNameText)>,
    ) -> Result<Self, ValidationError> {
        let mut plan = BTreeMap::new();
        for (group, name) in names {
            if plan.insert(group, name).is_some() {
                return Err(ValidationError::DuplicateGroupName { group });
            }
        }
        Ok(Self { names: plan })
    }

    /// The name-table pages the plan touches, each once, in address order:
    /// the page from `0x14800` for groups 0 to 15 and the page from
    /// `0x14900` for groups 16 to 29.
    #[must_use]
    pub fn pages(&self) -> Vec<Page> {
        let mut pages: Vec<Page> = Vec::new();
        for group in self.names.keys() {
            if let Some(page) = name_page(*group)
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
    /// which compares each page with a fresh read before writing.
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
                .get(start..start + page.len())
                .unwrap_or_default();
            let mut replacement = expected.to_vec();
            for (group, name) in &self.names {
                if let Some(offset) = group.name_address().checked_sub(start)
                    && let Some(field) = replacement.get_mut(offset..offset + CHANNEL_NAME_SIZE)
                {
                    field.copy_from_slice(&name.to_field());
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, expected, &replacement)?);
            }
        }
        Ok(replacements)
    }
}

/// The writable page holding `group`'s name.
fn name_page(group: MemoryGroup) -> Option<Page> {
    let address = Address::new(u32::try_from(group.name_address()).ok()?).ok()?;
    regions::writable_page_for(address)
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
