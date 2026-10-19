//! The slot tables the repeater, hotspot and D-STAR callsign lists share:
//! each populated slot stores the slot numbers of the previous and next
//! entries in display order, and two stored slot numbers, little-endian,
//! name the chain's first and last entries; `FFFF` names no slot.

use crate::memory::MemoryImage;
use crate::radio::programming::PageReplacement;
use crate::types::{Address, PAGE_SIZE, Page};

/// The stored link or end that names no slot.
pub(crate) const NO_LINK: u16 = 0xFFFF;

/// A populated slot's links.
pub(crate) trait Linked {
    /// The previous and the next entry's slot numbers.
    fn links(&self) -> [Option<u16>; 2];
}

/// How a stored chain fails to cover its table's populated slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChainFault {
    /// The chain reaches an empty slot, a slot outside the table, a slot it
    /// already passed, or an entry whose previous link names another slot.
    Broken {
        /// The slot number the chain reached.
        index: u16,
    },
    /// The chain ends other than at the stored last slot.
    EndsMismatch {
        /// The slot number of the chain's last entry.
        chain: Option<u16>,
        /// The stored last slot number.
        stored: Option<u16>,
    },
    /// A populated slot the chain never reaches.
    Unchained {
        /// The slot number.
        index: u16,
    },
}

/// Follow the chain from the stored first slot through `slots`, taking each
/// entry it reaches, and require it to end at the stored last slot and to
/// reach every populated slot.
pub(crate) fn follow<R: Linked>(
    mut slots: Vec<Option<R>>,
    [first, last]: [Option<u16>; 2],
) -> Result<Vec<(u16, R)>, ChainFault> {
    let mut entries = Vec::new();
    let mut previous = None;
    let mut cursor = first;
    while let Some(index) = cursor {
        let entry = slots
            .get_mut(usize::from(index))
            .and_then(Option::take)
            .ok_or(ChainFault::Broken { index })?;
        let [back, next] = entry.links();
        if back != previous {
            return Err(ChainFault::Broken { index });
        }
        previous = Some(index);
        cursor = next;
        entries.push((index, entry));
    }
    if previous != last {
        return Err(ChainFault::EndsMismatch {
            chain: previous,
            stored: last,
        });
    }
    if let Some(index) = slots.iter().position(Option::is_some) {
        return Err(ChainFault::Unchained {
            index: u16::try_from(index).unwrap_or(u16::MAX),
        });
    }
    Ok(entries)
}

/// The stored first and last slot numbers at `address`.
pub(crate) fn stored_ends(image: &MemoryImage, address: u32) -> [Option<u16>; 2] {
    let mut raw = [0xFF; 4];
    put_from(&mut raw, image, address);
    let [first_low, first_high, last_low, last_high] = raw;
    [[first_low, first_high], [last_low, last_high]]
        .map(|end| Some(u16::from_le_bytes(end)).filter(|&value| value != NO_LINK))
}

/// The stored form of the first and last slot numbers.
pub(crate) fn ends_bytes(first: Option<u16>, last: Option<u16>) -> [u8; 4] {
    let [first_low, first_high] = first.unwrap_or(NO_LINK).to_le_bytes();
    let [last_low, last_high] = last.unwrap_or(NO_LINK).to_le_bytes();
    [first_low, first_high, last_low, last_high]
}

/// The `N` bytes of `image` from `address`; bytes past the image read `FF`.
pub(crate) fn stored_field<const N: usize>(image: &MemoryImage, address: u32) -> [u8; N] {
    let mut field = [0xFF; N];
    put_from(&mut field, image, address);
    field
}

/// `length` bytes of `image` from `start`.
pub(crate) fn stored_bytes(image: &MemoryImage, start: u32, length: u32) -> Vec<u8> {
    let begin = start as usize;
    image
        .as_bytes()
        .get(begin..begin + length as usize)
        .unwrap_or_default()
        .to_vec()
}

/// Copy `value` into `buffer` at `offset`.
pub(crate) fn put(buffer: &mut [u8], offset: u32, value: &[u8]) {
    for (destination, source) in buffer.iter_mut().skip(offset as usize).zip(value) {
        *destination = *source;
    }
}

/// The writes of the 256-byte pages from `start` whose bytes in `updated`
/// differ from `image`'s.
pub(crate) fn changed_pages(
    image: &MemoryImage,
    start: u32,
    updated: &[u8],
) -> Result<Vec<PageReplacement>, crate::Error> {
    let mut replacements = Vec::new();
    for (page_address, replacement) in (start..).step_by(PAGE_SIZE).zip(updated.chunks(PAGE_SIZE)) {
        let begin = page_address as usize;
        let expected = image
            .as_bytes()
            .get(begin..begin + replacement.len())
            .unwrap_or_default();
        if expected != replacement {
            let page = Page::new(Address::new(page_address)?, replacement.len())?;
            replacements.push(PageReplacement::new(page, expected, replacement)?);
        }
    }
    Ok(replacements)
}

/// Fill `buffer` from `image` at `address`, leaving bytes past the image.
fn put_from(buffer: &mut [u8], image: &MemoryImage, address: u32) {
    for (destination, source) in buffer
        .iter_mut()
        .zip(image.as_bytes().iter().skip(address as usize))
    {
        *destination = *source;
    }
}
