//! Byte-level comparison of complete, explicitly covered standard configurations.
//!
//! These types borrow captured page payloads. They perform no I/O, allocate no
//! dense image, and interpret no setting; capture provenance is the caller's.

use crate::error::ValidationError;
use crate::protocol::mcp::regions::menu_regions;
use crate::radio::Identity;
use crate::types::{Address, Page, Region};

/// A malformed standard configuration or an incompatible comparison target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigurationError {
    /// Input ended before the next canonical page was supplied.
    #[error("standard configuration is missing page {index}: expected {expected:?}")]
    MissingPage {
        /// Zero-based position in the canonical transfer schedule.
        index: usize,
        /// Page that should occupy this position.
        expected: Page,
    },
    /// A page's address or declared length disagrees with its canonical position.
    #[error("standard configuration page {index} is {actual:?}; expected {expected:?}")]
    UnexpectedPage {
        /// Zero-based position of the rejected page.
        index: usize,
        /// Canonical page at this position.
        expected: Page,
        /// Supplied page, including its declared length.
        actual: Page,
    },
    /// Input continued beyond the complete standard schedule.
    #[error("standard configuration has an extra page at position {index}: {actual:?}")]
    ExtraPage {
        /// Zero-based position of the first extra page.
        index: usize,
        /// First unexpected trailing page.
        actual: Page,
    },
    /// Supplied bytes do not exactly fill their declared canonical page.
    #[error("standard configuration page {index} ({page:?}) contains {actual} bytes")]
    PageLength {
        /// Zero-based position of the malformed payload.
        index: usize,
        /// Canonical page whose complete payload is required.
        page: Page,
        /// Actual supplied byte count.
        actual: usize,
    },
    /// Model, exact firmware string, or complete radio-type payload differs.
    #[error("standard configuration identities differ: before {before:?}; after {after:?}")]
    IdentityMismatch {
        /// Complete identity attached to the earlier bytes.
        before: Identity,
        /// Complete identity attached to the later bytes.
        after: Identity,
    },
    /// A computed changed-byte address failed the image's address validation.
    #[error("invalid changed-byte address: {0}")]
    InvalidAddress(#[from] ValidationError),
}

/// Borrowed bytes covering exactly the standard global and per-slot schedule.
///
/// Construction validates every page's order, address, declared length, and
/// complete payload. The startup-screen region is outside that schedule, so its
/// pages are rejected.
#[derive(Debug)]
pub struct StandardConfiguration<'a> {
    identity: &'a Identity,
    pages: Vec<(Page, &'a [u8])>,
}

impl<'a> StandardConfiguration<'a> {
    /// Validate borrowed page payloads against the exact standard transfer order.
    ///
    /// The accepted order is [`menu_regions`] followed by each [`Region::pages`]
    /// sequence. No sorting, deduplication, partial coverage, or gap filling is
    /// performed. At most the complete schedule and one extra item are consumed,
    /// so an excessive iterator cannot make this method retain unbounded input.
    /// Identity is retained exactly and is not checked against
    /// [`super::MCP_D750_SCHEMA_FIRMWARE_IDENTITIES`].
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::MissingPage`],
    /// [`ConfigurationError::UnexpectedPage`], [`ConfigurationError::ExtraPage`],
    /// or [`ConfigurationError::PageLength`] at the first invalid input item.
    pub fn new(
        identity: &'a Identity,
        pages: impl IntoIterator<Item = (Page, &'a [u8])>,
    ) -> Result<Self, ConfigurationError> {
        let mut input = pages.into_iter();
        let mut captured = Vec::new();
        for (index, expected) in menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .enumerate()
        {
            let (actual, data) = input
                .next()
                .ok_or(ConfigurationError::MissingPage { index, expected })?;
            if actual != expected {
                return Err(ConfigurationError::UnexpectedPage {
                    index,
                    expected,
                    actual,
                });
            }
            if data.len() != expected.len() {
                return Err(ConfigurationError::PageLength {
                    index,
                    page: expected,
                    actual: data.len(),
                });
            }
            captured.push((actual, data));
        }
        if let Some((actual, _)) = input.next() {
            return Err(ConfigurationError::ExtraPage {
                index: captured.len(),
                actual,
            });
        }
        Ok(Self {
            identity,
            pages: captured,
        })
    }

    /// Complete caller-supplied identity associated with these borrowed bytes.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        self.identity
    }
}

/// One byte address whose value differs between the two configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangedByte {
    address: Address,
    before: u8,
    after: u8,
}

impl ChangedByte {
    /// Absolute address in the explicitly covered standard configuration.
    #[must_use]
    pub const fn address(self) -> Address {
        self.address
    }

    /// Byte in the earlier configuration.
    #[must_use]
    pub const fn before(self) -> u8 {
        self.before
    }

    /// Byte in the later configuration.
    #[must_use]
    pub const fn after(self) -> u8 {
        self.after
    }
}

/// A canonical page containing at least one changed byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedPage {
    page: Page,
    changes: Vec<ChangedByte>,
}

impl ChangedPage {
    /// Complete canonical transfer unit containing these changes.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Only changed bytes, in ascending absolute-address order within the page.
    #[must_use]
    pub fn changes(&self) -> &[ChangedByte] {
        &self.changes
    }

    fn between(page: Page, before: &[u8], after: &[u8]) -> Result<Self, ConfigurationError> {
        let mut changes = Vec::new();
        for (address, (&before, &after)) in
            (page.address().as_u32()..page.end()).zip(before.iter().zip(after))
        {
            if before != after {
                changes.push(ChangedByte {
                    address: Address::new(address)?,
                    before,
                    after,
                });
            }
        }
        Ok(Self { page, changes })
    }
}

/// Deterministic byte differences between two complete standard configurations.
///
/// Only changed pages and bytes are retained; unchanged bytes and gaps are not
/// copied into the result. Comparison covers the standard transfer schedule
/// only, not the entire memory image, and reports byte addresses without naming
/// the settings they belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandardConfigurationDiff {
    compared_pages: usize,
    compared_bytes: usize,
    changed_bytes: usize,
    pages: Vec<ChangedPage>,
}

impl StandardConfigurationDiff {
    /// Compare every supplied standard byte after requiring full identity equality.
    ///
    /// Pages follow the canonical transfer order; changes within each page are
    /// ordered by absolute address. The result owns its small change records,
    /// not the borrowed source payloads.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::IdentityMismatch`] before comparison when
    /// any identity component differs. Address validation failures are retained
    /// as [`ConfigurationError::InvalidAddress`]; valid canonical pages already
    /// ensure that every compared byte lies inside the image.
    pub fn between(
        before: &StandardConfiguration<'_>,
        after: &StandardConfiguration<'_>,
    ) -> Result<Self, ConfigurationError> {
        if before.identity != after.identity {
            return Err(ConfigurationError::IdentityMismatch {
                before: before.identity.clone(),
                after: after.identity.clone(),
            });
        }
        let mut result = Self {
            compared_pages: 0,
            compared_bytes: 0,
            changed_bytes: 0,
            pages: Vec::new(),
        };
        for ((page, before), (_, after)) in before.pages.iter().zip(&after.pages) {
            result.compared_pages += 1;
            result.compared_bytes += page.len();
            let changed = ChangedPage::between(*page, before, after)?;
            result.changed_bytes += changed.changes.len();
            if !changed.changes.is_empty() {
                result.pages.push(changed);
            }
        }
        Ok(result)
    }

    /// Number of complete canonical pages compared, including unchanged pages.
    #[must_use]
    pub const fn compared_pages(&self) -> usize {
        self.compared_pages
    }

    /// Number of explicitly covered bytes compared, excluding every unread gap.
    #[must_use]
    pub const fn compared_bytes(&self) -> usize {
        self.compared_bytes
    }

    /// Total differing bytes across the retained changed pages.
    #[must_use]
    pub const fn changed_bytes(&self) -> usize {
        self.changed_bytes
    }

    /// Only pages with changes, in canonical standard transfer order.
    #[must_use]
    pub fn pages(&self) -> &[ChangedPage] {
        &self.pages
    }
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
