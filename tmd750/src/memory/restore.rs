//! Restoring a standard configuration capture onto the radio it came from.
//!
//! [`RestorePlan::new`] compares two complete standard configurations with
//! one radio identity, the radio's current one and the one to restore, and
//! writes every canonical page whose bytes differ with the restored bytes.
//! Every field the menu registry classifies
//! [`MenuWritePolicy::LifecycleRequired`] keeps its current value in each
//! slot, so the active PM, the PC and data interfaces, Bluetooth, the DV
//! Gateway and Terminal selection and the automatic transmissions change
//! only through their own workflows. Every other byte of the standard
//! schedule takes the restored value, the memory channels, lists and VFO
//! memory included. [`RestorePlan::from_d750`] restores a `.d750` file the
//! same way from its bytes at the standard schedule's addresses.
//!
//! On firmware 1.02, type `K,2,1`, restoring over the operation-panel USB a
//! backup that held a stored, named channel the radio had since cleared
//! wrote its flag, record and name pages, and a CAT memory read then
//! reported the channel. A fresh standard backup equaled the restored one
//! except byte `0x5016B` of band B's slot state, which the radio sets itself;
//! restoring the earlier backup made the radio equal it again byte for byte.
//! A `.d750` file holding another backup's bytes, restored the same way,
//! made a fresh backup equal that backup byte for byte; files the official
//! program wrote were not tested.

use crate::error::{McpError, SchemaError};
use crate::file::RadioConfig;
use crate::memory::{Configuration, ConfigurationError, MenuFieldId, MenuWritePolicy};
use crate::protocol::mcp::regions::menu_regions;
use crate::radio::programming::PageReplacement;
use crate::types::{Page, Region, SlotIndex};
use kenwood_schema::{CodecSpec, FieldCodec};

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;

/// A lifecycle field whose restored value differed from the current one and
/// was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptField {
    field: MenuFieldId,
    slot: Option<SlotIndex>,
}

impl KeptField {
    /// The field.
    #[must_use]
    pub const fn field(self) -> MenuFieldId {
        self.field
    }

    /// The slot of a per-PM field, `None` for a global one.
    #[must_use]
    pub const fn slot(self) -> Option<SlotIndex> {
        self.slot
    }
}

/// The page writes that restore a configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    replacements: Vec<PageReplacement>,
    kept: Vec<KeptField>,
}

impl RestorePlan {
    /// Plan restoring `target` onto a radio holding `current`.
    ///
    /// Each replacement expects `current`'s page and writes `target`'s,
    /// except the bits each lifecycle field owns, which keep `current`'s.
    /// Apply the replacements with
    /// [`crate::radio::McpSession::compare_exchange_pages`], which compares
    /// each page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns [`RestoreError::Configuration`] unless both are complete
    /// standard configurations with equal identities, and the
    /// [`PageReplacement::new`] errors as [`RestoreError::Mcp`].
    pub fn new(current: &Configuration, target: &Configuration) -> Result<Self, RestoreError> {
        let diff = current.diff(target)?;
        let mut pages = Vec::with_capacity(diff.pages().len());
        for changed in diff.pages() {
            let page = changed.page();
            pages.push((
                page,
                current.bytes(page.region())?.to_vec(),
                target.bytes(page.region())?.to_vec(),
            ));
        }
        let mut kept = Vec::new();
        for field in MenuFieldId::ALL {
            if field.field().write_policy() != MenuWritePolicy::LifecycleRequired {
                continue;
            }
            let descriptor = &field.field().descriptor;
            let slots: Vec<Option<SlotIndex>> = if descriptor.terms.is_empty() {
                vec![None]
            } else {
                SlotIndex::all().map(Some).to_vec()
            };
            for slot in slots {
                let start = descriptor.address(slot)?.as_usize();
                let owned = owned_bits(descriptor.codec);
                if keep(
                    &mut pages,
                    start..start + descriptor.codec.encoded_len(),
                    owned,
                ) {
                    kept.push(KeptField { field, slot });
                }
            }
        }
        let replacements = pages
            .into_iter()
            .filter(|(_, expected, replacement)| expected != replacement)
            .map(|(page, expected, replacement)| {
                PageReplacement::new(page, &expected, &replacement)
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { replacements, kept })
    }

    /// Plan restoring the standard schedule's bytes of a `.d750` file onto a
    /// radio holding `current`.
    ///
    /// The file carries no firmware, so its bytes take `current`'s identity
    /// once the header's radio-type bytes equal `current`'s radio type
    /// without its separators; every other rule is [`Self::new`]'s. Bytes
    /// outside the standard schedule are not read.
    ///
    /// # Errors
    ///
    /// Returns [`RestoreError::FileRadioType`] when the radio types differ,
    /// and the [`Self::new`] errors.
    pub fn from_d750(current: &Configuration, file: &RadioConfig) -> Result<Self, RestoreError> {
        let identity = current.identity();
        let found = file.header().radio_type_bytes();
        let expected = identity
            .radio_type
            .as_str()
            .bytes()
            .filter(|&byte| byte != b',');
        if !expected.eq(found) {
            return Err(RestoreError::FileRadioType {
                found,
                radio_type: identity.radio_type.as_str().to_owned(),
            });
        }
        let image = file.image_bytes();
        let pages = menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| {
                let start = page.address().as_usize();
                let bytes = image.get(start..start + page.len()).unwrap_or_default();
                (page, bytes.to_vec())
            });
        Self::new(
            current,
            &Configuration::from_pages(identity.clone(), pages)?,
        )
    }

    /// The writes of the pages that change, in transfer order.
    #[must_use]
    pub fn replacements(&self) -> &[PageReplacement] {
        &self.replacements
    }

    /// The lifecycle fields that keep their current values although the
    /// restored configuration holds others.
    #[must_use]
    pub fn kept(&self) -> &[KeptField] {
        &self.kept
    }
}

/// The bits of each of a field's bytes the field owns.
const fn owned_bits(codec: FieldCodec) -> u8 {
    match codec.spec() {
        CodecSpec::BitBool { mask } | CodecSpec::BitField { mask, .. } => mask,
        CodecSpec::Byte { .. }
        | CodecSpec::Bool
        | CodecSpec::FixedString { .. }
        | CodecSpec::Unsigned { .. }
        | CodecSpec::Signed { .. }
        | CodecSpec::Bytes { .. } => 0xFF,
    }
}

/// Copy the `owned` bits of the bytes at `addresses` from each changed
/// page's current bytes into its replacement, and report whether a
/// replacement byte changed.
fn keep(
    pages: &mut [(Page, Vec<u8>, Vec<u8>)],
    addresses: std::ops::Range<usize>,
    owned: u8,
) -> bool {
    let mut changed = false;
    for address in addresses {
        let index =
            pages.partition_point(|(page, _, _)| page.address().as_usize() + page.len() <= address);
        if let Some((page, current, restored)) = pages.get_mut(index)
            && let Some(offset) = address.checked_sub(page.address().as_usize())
            && let (Some(current), Some(restored)) = (current.get(offset), restored.get_mut(offset))
        {
            let kept = (*restored & !owned) | (current & owned);
            changed |= kept != *restored;
            *restored = kept;
        }
    }
    changed
}

/// A configuration pair the library cannot plan a restore for.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreError {
    /// The configurations are not complete standard captures of one identity.
    #[error(transparent)]
    Configuration(#[from] ConfigurationError),
    /// A registry field's address did not resolve.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// A changed page is not a canonical writable page.
    #[error(transparent)]
    Mcp(#[from] McpError),
    /// A `.d750` file's radio-type bytes are not the radio's type.
    #[error("the file's radio-type bytes {found:02X?} are not the radio's type {radio_type}")]
    FileRadioType {
        /// The header's three radio-type bytes.
        found: [u8; 3],
        /// The radio's type.
        radio_type: String,
    },
}
