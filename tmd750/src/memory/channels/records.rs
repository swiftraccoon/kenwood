//! Memory channel stores and clears planned as page writes.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, ValidationError};
use crate::memory::MemoryImage;
use crate::protocol::mcp::regions;
use crate::radio::ChannelWrite;
use crate::radio::programming::PageReplacement;
use crate::types::{
    Address, BandCode, CHANNEL_NAME_SIZE, CHANNEL_RECORD_SIZE, EMPTY_CHANNEL_RECORD,
    MemoryChannelAddress, Page, PhysicalChannel, StoredChannel,
};

/// Stores and clears of regular, program scan and priority channels, planned
/// as whole-page writes.
///
/// A store writes what a CAT `ME` store of the same record writes into the
/// band code, scan lockout and record: [`BandCode::for_frequency`], the
/// record's lockout and [`StoredChannel::from_cat_record`]. A clear writes
/// what a CAT clear leaves: the empty band code, no lockout,
/// [`EMPTY_CHANNEL_RECORD`] and a name of sixteen NUL bytes. Both keep the
/// flag's group byte and byte 3, and a store keeps the name. Unlike CAT,
/// which answers `N` for it, the plan stores the priority channel, which
/// priority scan (`radio.PriorityScan`) then selects on band B. Apply the
/// replacements with [`crate::radio::McpSession::compare_exchange_pages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryChannelPlan {
    entries: BTreeMap<PhysicalChannel, Entry>,
}

/// One planned channel: the request and the bytes it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    address: MemoryChannelAddress,
    write: ChannelWrite,
    /// Flag bytes 0 and 1: the band code and the scan lockout.
    flag: [u8; 2],
    record: [u8; CHANNEL_RECORD_SIZE],
}

impl MemoryChannelPlan {
    /// Collect one write per channel.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DuplicateMemoryChannel`] when a channel
    /// appears twice, and the [`StoredChannel::from_cat_record`] errors.
    pub fn new(
        writes: impl IntoIterator<Item = (MemoryChannelAddress, ChannelWrite)>,
    ) -> Result<Self, ValidationError> {
        let mut entries = BTreeMap::new();
        for (address, write) in writes {
            let (flag, record) = match &write {
                ChannelWrite::Store(record) => (
                    [
                        BandCode::for_frequency(record.channel.receive_frequency).as_raw(),
                        u8::from(record.scan_lockout),
                    ],
                    StoredChannel::from_cat_record(record)?.to_bytes(),
                ),
                ChannelWrite::Clear => ([BandCode::EMPTY, 0], EMPTY_CHANNEL_RECORD),
            };
            let entry = Entry {
                address,
                write,
                flag,
                record,
            };
            if entries
                .insert(PhysicalChannel::from_address(address), entry)
                .is_some()
            {
                return Err(ValidationError::DuplicateMemoryChannel { address });
            }
        }
        Ok(Self { entries })
    }

    /// The planned writes, in table order.
    pub fn writes(&self) -> impl Iterator<Item = (MemoryChannelAddress, &ChannelWrite)> {
        self.entries
            .values()
            .map(|entry| (entry.address, &entry.write))
    }

    /// The flag, record and name pages the plan touches, each once, in
    /// address order; a store touches no name page.
    #[must_use]
    pub fn pages(&self) -> Vec<Page> {
        let addresses: BTreeSet<u32> = self
            .fields()
            .iter()
            .filter_map(|(address, _)| writable_page(*address))
            .map(|page| page.address().as_u32())
            .collect();
        addresses
            .into_iter()
            .filter_map(|address| writable_page(address as usize))
            .collect()
    }

    /// The writes of the touched pages whose bytes change, each keeping every
    /// other byte as `image` holds it.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(&self, image: &MemoryImage) -> Result<Vec<PageReplacement>, Error> {
        let fields = self.fields();
        let mut replacements = Vec::new();
        for page in self.pages() {
            let start = page.address().as_usize();
            let expected = image
                .as_bytes()
                .get(start..start + page.len())
                .unwrap_or_default();
            let mut replacement = expected.to_vec();
            for (address, bytes) in &fields {
                if let Some(offset) = address.checked_sub(start)
                    && let Some(target) = replacement.get_mut(offset..offset + bytes.len())
                {
                    target.copy_from_slice(bytes);
                }
            }
            if replacement != expected {
                replacements.push(PageReplacement::new(page, expected, &replacement)?);
            }
        }
        Ok(replacements)
    }

    /// Every byte run the plan writes, by image address.
    fn fields(&self) -> Vec<(usize, Vec<u8>)> {
        let mut fields = Vec::new();
        for (channel, entry) in &self.entries {
            fields.push((channel.flag_address(), entry.flag.to_vec()));
            fields.push((channel.data_address(), entry.record.to_vec()));
            if entry.write == ChannelWrite::Clear {
                fields.push((channel.name_address(), vec![0; CHANNEL_NAME_SIZE]));
            }
        }
        fields
    }
}

/// The writable page holding image address `address`.
fn writable_page(address: usize) -> Option<Page> {
    let address = Address::new(u32::try_from(address).ok()?).ok()?;
    regions::writable_page_for(address)
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
