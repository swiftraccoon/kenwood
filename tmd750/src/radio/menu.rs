//! Schema-driven sparse menu access within an explicitly owned MCP session.
//!
//! Field codecs and PM-slot addressing are shared across the whole registry.
//! Snapshot coverage is explicit; unread gaps never become observed zero bytes.
//! Callers own entry, capture, detached exit, transport release, and fresh
//! verification. This module neither opens connections nor enables Terminal Mode.

mod update;

pub use update::{MenuAssignment, MenuUpdateError, MenuUpdatePlan};

use std::collections::BTreeMap;
use std::io;

use crate::error::{Error, McpError, SchemaError};
use crate::memory::{DecodedFieldValue, MenuField, PatchSet};
use crate::protocol::mcp::regions;
use crate::types::{Address, IMAGE_LENGTH, PAGE_SIZE_U32, Page, SlotIndex};
use kenwood_transport::Transport;

use super::Progress;
use super::programming::{McpCompareExchangeReport, McpSession, PageReplacement};

/// One menu field with an explicit, validated global or PM-slot scope.
///
/// A per-slot field requires slot 0 through 5; slot 0 is PM Off. A global
/// field rejects any slot rather than silently ignoring it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopedMenuField<'a> {
    field: &'a MenuField,
    slot: Option<SlotIndex>,
}

impl<'a> ScopedMenuField<'a> {
    /// Resolve the field's scope without reading or writing a radio.
    ///
    /// # Errors
    ///
    /// Rejects an unnecessary or missing slot, unknown address dimensions,
    /// and storage outside the image.
    pub fn new(field: &'a MenuField, slot: Option<SlotIndex>) -> Result<Self, SchemaError> {
        if !field.descriptor.is_per_slot() && slot.is_some() {
            return Err(SchemaError::UnexpectedSlot {
                field: field.descriptor.name,
            });
        }
        let _address = field.descriptor.address(slot)?;
        Ok(Self { field, slot })
    }

    /// The registry metadata, including codec, choices, and display labels.
    #[must_use]
    pub const fn field(self) -> &'a MenuField {
        self.field
    }

    /// Explicit PM slot, or `None` for a global setting.
    #[must_use]
    pub const fn slot(self) -> Option<SlotIndex> {
        self.slot
    }

    /// Canonical transfer pages containing every byte of this field.
    ///
    /// Short and non-aligned region fragments are retained. An explicitly
    /// selected startup bitmap can be read, but remains outside scalar patching.
    ///
    /// # Errors
    ///
    /// Returns address errors or [`SchemaError::SnapshotPageNotCanonical`] if
    /// any field byte falls outside the known configuration transfer regions.
    pub fn pages(self) -> Result<Vec<Page>, SchemaError> {
        let start = self.field.descriptor.address(self.slot)?.as_u32();
        let len = self.field.descriptor.codec.encoded_len();
        let end = u64::from(start) + u64::try_from(len).unwrap_or(u64::MAX);
        let mut cursor = start;
        let mut pages = Vec::new();
        while u64::from(cursor) < end {
            let page = Address::new(cursor)
                .ok()
                .and_then(canonical_read_page)
                .ok_or(SchemaError::SnapshotPageNotCanonical {
                    address: cursor,
                    len,
                })?;
            cursor = page.region().end();
            pages.push(page);
        }
        Ok(pages)
    }
}

/// Sparse, complete canonical pages used for typed decoding and stale checks.
///
/// Stored numeric values are preserved even when absent from the writable menu
/// domain; planning new values remains domain-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuFieldSnapshot {
    image: Vec<u8>,
    pages: Vec<(Page, Vec<u8>)>,
}

impl MenuFieldSnapshot {
    /// Construct a snapshot from already-captured complete pages.
    ///
    /// Pages are sorted by address. Duplicate, partial, noncanonical, and
    /// unknown-region pages are refused, including identical duplicate entries.
    ///
    /// # Errors
    ///
    /// Returns a typed snapshot error identifying invalid coverage or lengths.
    pub fn from_pages(mut pages: Vec<(Page, Vec<u8>)>) -> Result<Self, SchemaError> {
        pages.sort_by_key(|(page, _bytes)| page.address().as_u32());
        let mut image = vec![0; IMAGE_LENGTH];
        let mut previous = None;
        for (page, bytes) in &pages {
            let address = page.address().as_u32();
            if canonical_read_page(page.address()) != Some(*page) {
                return Err(SchemaError::SnapshotPageNotCanonical {
                    address,
                    len: page.len(),
                });
            }
            if bytes.len() != page.len() {
                return Err(SchemaError::SnapshotPageLength {
                    address,
                    expected: page.len(),
                    actual: bytes.len(),
                });
            }
            if previous == Some(address) {
                return Err(SchemaError::DuplicateSnapshotPage { address });
            }
            previous = Some(address);
            let start = page.address().as_usize();
            image
                .get_mut(start..start + page.len())
                .ok_or(SchemaError::SnapshotPageNotCanonical {
                    address,
                    len: page.len(),
                })?
                .copy_from_slice(bytes);
        }
        Ok(Self { image, pages })
    }

    /// Complete captured pages, sorted by address, without synthesized gaps.
    #[must_use]
    pub fn pages(&self) -> &[(Page, Vec<u8>)] {
        &self.pages
    }

    /// The complete bytes of `page`, only when that exact page was captured.
    #[must_use]
    pub fn page(&self, page: Page) -> Option<&[u8]> {
        self.pages
            .iter()
            .find(|(captured, _bytes)| *captured == page)
            .map(|(_page, bytes)| bytes.as_slice())
    }

    /// Decode a selected field only when all of its pages are present.
    ///
    /// Unknown stored numeric values remain numeric; no default is substituted.
    ///
    /// # Errors
    ///
    /// Returns missing-coverage, scope, or storage-codec errors.
    pub fn value(&self, selection: ScopedMenuField<'_>) -> Result<DecodedFieldValue, SchemaError> {
        for page in selection.pages()? {
            if self.page(page).is_none() {
                return Err(missing_page(selection.field.descriptor.name, page));
            }
        }
        selection.field.descriptor.read(&self.image, selection.slot)
    }

    /// Prepare complete expected/replacement pages for a schema-generated patch.
    ///
    /// Every touched page must have been captured. All unrelated bits and bytes
    /// retain their exact before-image; the source snapshot is never changed.
    /// The result is derived from the snapshot, so it can be stale: every page
    /// is compared against a fresh read before it is written.
    ///
    /// # Errors
    ///
    /// Returns missing snapshot coverage or an invalid writable page error.
    pub fn plan_exchanges(&self, patches: &PatchSet) -> Result<Vec<PageReplacement>, Error> {
        patches
            .pages()
            .iter()
            .map(|patch| {
                let expected = self
                    .page(patch.page())
                    .ok_or_else(|| missing_page("menu patch", patch.page()))?;
                let mut replacement = expected.to_vec();
                patch.apply(&mut replacement)?;
                Ok(PageReplacement::new(patch.page(), expected, &replacement)?)
            })
            .collect()
    }

    /// Produce a local preview while preserving the original snapshot.
    ///
    /// The preview is in memory only: no radio is read and no file is written.
    ///
    /// # Errors
    ///
    /// Returns the same coverage and writable-page errors as [`Self::plan_exchanges`].
    pub fn patched(&self, patches: &PatchSet) -> Result<Self, Error> {
        let replacements = self.plan_exchanges(patches)?;
        let mut pages = self.pages.clone();
        for (page, bytes) in &mut pages {
            if let Some(replacement) = replacements.iter().find(|change| change.page() == *page) {
                bytes.copy_from_slice(replacement.replacement());
            }
        }
        Ok(Self::from_pages(pages)?)
    }
}

impl<T: Transport> McpSession<'_, T> {
    /// Read the minimum canonical page set covering the selected menu fields.
    ///
    /// Repeated fields and shared pages are read once, in address order. All
    /// field spans are validated before the first read. Empty input sends no
    /// traffic. No memory write, automatic exit, close, or reopen is performed.
    ///
    /// # Errors
    ///
    /// Returns scope/coverage errors before reading, or the underlying MCP error.
    /// Incomplete protocol exchanges retain the session's recovery-required state.
    pub async fn read_menu_snapshot(
        &mut self,
        fields: &[ScopedMenuField<'_>],
        mut progress: impl FnMut(Progress),
    ) -> Result<MenuFieldSnapshot, Error> {
        if !self.is_ready() {
            return Err(McpError::RecoveryRequired.into());
        }
        let mut pages = BTreeMap::new();
        for selection in fields {
            for page in selection.pages()? {
                let _previous = pages.insert(page.address().as_u32(), page);
            }
        }
        let total = pages.len();
        let mut captured = Vec::with_capacity(total);
        for (index, page) in pages.into_values().enumerate() {
            captured.push((page, self.read_page(page).await?));
            progress(Progress {
                done: index + 1,
                total,
            });
        }
        Ok(MenuFieldSnapshot::from_pages(captured)?)
    }

    /// Apply a batch of menu changes only while all source pages still match.
    ///
    /// Composes snapshot planning with [`Self::compare_exchange_pages`]. Every
    /// expected page is freshly compared before the first write. The fallible
    /// callback must durably retain each complete before/after image before W.
    /// No-op pages are compared but never written. This is not a firmware-atomic
    /// transaction; partial write risk remains in the session journal.
    ///
    /// # Errors
    ///
    /// Returns incomplete snapshot coverage, schema-target, stale-page,
    /// durable-intent, or MCP exchange/readback errors. No automatic rollback,
    /// retry, exit, or reconnection occurs.
    pub async fn compare_exchange_menu_patches(
        &mut self,
        patches: &PatchSet,
        snapshot: &MenuFieldSnapshot,
        before_write: impl FnMut(&PageReplacement) -> io::Result<()>,
        progress: impl FnMut(Progress),
    ) -> Result<McpCompareExchangeReport, Error> {
        let replacements = snapshot.plan_exchanges(patches)?;
        self.compare_exchange_pages(&replacements, before_write, progress)
            .await
    }
}

const fn missing_page(field: &'static str, page: Page) -> SchemaError {
    SchemaError::SnapshotPageMissing {
        field,
        address: page.address().as_u32(),
        len: page.len(),
    }
}

fn canonical_read_page(address: Address) -> Option<Page> {
    if let Some(page) = regions::writable_page_for(address) {
        return Some(page);
    }
    let region = regions::STARTUP_BITMAP;
    if !region.contains(address) {
        return None;
    }
    let offset = (address.as_u32() - region.start()) / PAGE_SIZE_U32 * PAGE_SIZE_U32;
    let start = Address::new(region.start() + offset).ok()?;
    let len = usize::try_from((region.end() - start.as_u32()).min(PAGE_SIZE_U32)).ok()?;
    Page::new(start, len).ok()
}

#[cfg(test)]
mod tests;
