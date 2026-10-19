//! The D-STAR repeater and hotspot lists held in a captured image.
//!
//! Both lists are tables of 80-byte [`RepeaterRecord`]s, three to each
//! 256-byte page: [`RepeaterListKind::Repeater`] has 1,500 slots and
//! [`RepeaterListKind::Hotspot`] 30. A list's populated records form a chain
//! linked by slot number, and the slot numbers of its first and last records
//! are stored little-endian in the four bytes after the three records of the
//! list's final page. A record names its world region, country and group by
//! index into three tables of 16-byte names, read with [`RepeaterNames`];
//! every name is a [`StoredText`]. The lists and the tables lie inside the
//! standard backup schedule. Edit one
//! record in place with [`RepeaterRecord::page_replacement`]; store a whole
//! list, reordered, shortened or extended with records derived from stored
//! ones, with [`RepeaterList::from_records`] and
//! [`RepeaterList::page_replacements`], and renamed tables with
//! [`RepeaterNames::page_replacements`].
//!
//! The layout was established on firmware 1.02, type `K,2,1`, by decoding a
//! backup that held 1,455 repeaters against Kenwood's published repeater
//! catalog; re-encoding that backup's lists and tables reproduces every byte
//! of their pages. An in-place frequency edit of the repeater the DR tuning
//! mode had selected was written over the operation-panel USB, reported by
//! `FO` in DR mode, and restored. Selecting DR after that edit also rewrote
//! band A's copy of the selected record in the PM Off settings at `0x50430`
//! (frequency, offset, URCALL and both repeater callsigns, with the frequency
//! again at `0x50460`), so a backup taken then differs there as well. The
//! hotspot list was empty in every
//! capture, so the decoding of a populated hotspot record is unverified.
//! Byte 74, bits 0 to 3 of byte 75 and the bits of byte 64 other than the
//! hemisphere bits have no established meaning; records carry them unchanged.

#[cfg(test)]
mod fixtures;
mod record;

use std::fmt;

pub use record::{
    Coordinate, CountryIndex, GroupIndex, Latitude, LatitudeHemisphere, Longitude,
    LongitudeHemisphere, PositionAccuracy, RegionIndex, RepeaterFrequency, RepeaterPosition,
    RepeaterRecord, RepeaterShift, UtcOffset,
};

use crate::memory::chain::{self, ChainFault, Linked};
use crate::memory::{MemoryImage, StoredText, StoredTextError};
use crate::radio::programming::PageReplacement;
use crate::types::{Address, PAGE_SIZE, PAGE_SIZE_U32, Page};

/// Bytes per record.
pub const REPEATER_RECORD_SIZE: usize = 80;
/// Bytes per stored name: a record's name and sub-name, and each entry of the
/// region, country and group tables.
pub const REPEATER_TEXT_SIZE: usize = 16;
/// Address of the world-region name table, [`RegionIndex::COUNT`] names.
pub const REPEATER_REGION_NAMES_ADDRESS: u32 = 0x4_A000;
/// Address of the country name table, [`CountryIndex::COUNT`] names.
pub const REPEATER_COUNTRY_NAMES_ADDRESS: u32 = 0x4_A100;
/// Address of the group name table, [`GroupIndex::COUNT`] names.
pub const REPEATER_GROUP_NAMES_ADDRESS: u32 = 0x4_AB00;

const RECORDS_PER_PAGE: u16 = 3;
const RECORD_SIZE_U32: u32 = 80;
const TEXT_SIZE_U32: u32 = 16;
const ENDS_OFFSET: u32 = 240;

/// An empty slot as stored: NUL text and callsigns, an all-ones frequency
/// and offset, a zero position, then all-ones fields with the empty-slot flag
/// of byte 75 set.
const EMPTY_SLOT: [u8; REPEATER_RECORD_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0, //
    0xFB, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0xF0, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// One of the two lists of [`RepeaterRecord`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterListKind {
    /// The repeater list: 1,500 slots from `0x2A000`.
    Repeater,
    /// The hotspot list: 30 slots from `0x49400`.
    Hotspot,
}

impl RepeaterListKind {
    /// Slots in the list.
    #[must_use]
    pub const fn capacity(self) -> u16 {
        match self {
            Self::Repeater => 1500,
            Self::Hotspot => 30,
        }
    }

    /// Address of slot 0.
    #[must_use]
    pub const fn records_address(self) -> u32 {
        match self {
            Self::Repeater => 0x2_A000,
            Self::Hotspot => 0x4_9400,
        }
    }

    /// Address of the stored slot numbers of the chain's first and last
    /// records, two little-endian `u16`s; `FFFF` stores none.
    #[must_use]
    pub fn ends_address(self) -> u32 {
        let pages = u32::from(self.capacity() / RECORDS_PER_PAGE);
        self.records_address() + (pages - 1) * PAGE_SIZE_U32 + ENDS_OFFSET
    }
}

impl fmt::Display for RepeaterListKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repeater => "repeater",
            Self::Hotspot => "hotspot",
        })
    }
}

/// A slot of one list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterSlot {
    kind: RepeaterListKind,
    index: u16,
}

impl RepeaterSlot {
    /// Validate a slot number.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::SlotOutOfRange`] at or past the list's
    /// capacity.
    pub const fn new(kind: RepeaterListKind, index: u16) -> Result<Self, RepeaterListError> {
        if index < kind.capacity() {
            Ok(Self { kind, index })
        } else {
            Err(RepeaterListError::SlotOutOfRange { kind, index })
        }
    }

    /// The list.
    #[must_use]
    pub const fn kind(self) -> RepeaterListKind {
        self.kind
    }

    /// The slot number.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.index
    }

    /// The image address of the slot's record.
    #[must_use]
    pub fn address(self) -> u32 {
        self.page_address() + u32::from(self.index % RECORDS_PER_PAGE) * RECORD_SIZE_U32
    }

    /// The image address of the page holding the slot's record.
    fn page_address(self) -> u32 {
        self.kind.records_address() + u32::from(self.index / RECORDS_PER_PAGE) * PAGE_SIZE_U32
    }
}

impl fmt::Display for RepeaterSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} slot {}", self.kind, self.index)
    }
}

/// The populated records of one list in chain order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeaterList {
    kind: RepeaterListKind,
    entries: Vec<(RepeaterSlot, RepeaterRecord)>,
}

impl RepeaterList {
    /// Decode every slot of `kind` and follow the chain from the stored first
    /// slot.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::Record`] for a populated record whose
    /// fields fail their domains, [`RepeaterListError::ChainBroken`] when the
    /// chain reaches an empty slot, a slot outside the list or a record whose
    /// back link names another slot, [`RepeaterListError::EndsMismatch`] when
    /// it ends other than at the stored last slot, and
    /// [`RepeaterListError::UnchainedRecord`] for a populated record the chain
    /// never reaches.
    pub fn read(image: &MemoryImage, kind: RepeaterListKind) -> Result<Self, RepeaterListError> {
        let mut slots = Vec::with_capacity(usize::from(kind.capacity()));
        for index in 0..kind.capacity() {
            let slot = RepeaterSlot { kind, index };
            let record =
                RepeaterRecord::from_bytes(stored_record(image, slot)).map_err(|source| {
                    RepeaterListError::Record {
                        slot,
                        source: Box::new(source),
                    }
                })?;
            slots.push(record);
        }
        let entries = chain::follow(slots, chain::stored_ends(image, kind.ends_address()))
            .map_err(|fault| match fault {
                ChainFault::Broken { index } => RepeaterListError::ChainBroken { kind, index },
                ChainFault::EndsMismatch { chain, stored } => RepeaterListError::EndsMismatch {
                    kind,
                    chain,
                    stored,
                },
                ChainFault::Unchained { index } => {
                    RepeaterListError::UnchainedRecord { kind, index }
                }
            })?
            .into_iter()
            .map(|(index, record)| (RepeaterSlot { kind, index }, record))
            .collect();
        Ok(Self { kind, entries })
    }

    /// The list.
    #[must_use]
    pub const fn kind(&self) -> RepeaterListKind {
        self.kind
    }

    /// The number of populated records.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no record is populated.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The records in chain order with their slots.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (RepeaterSlot, &RepeaterRecord)> {
        self.entries.iter().map(|(slot, record)| (*slot, record))
    }

    /// The record in `slot`, when populated.
    #[must_use]
    pub fn get(&self, slot: RepeaterSlot) -> Option<&RepeaterRecord> {
        self.entries
            .iter()
            .find(|(stored, _)| *stored == slot)
            .map(|(_, record)| record)
    }

    /// A list of `records` in order, stored compact from slot 0.
    ///
    /// Each record's links are replaced to chain the slots in order; every
    /// other byte is kept, so a record derived from a stored one carries that
    /// record's bytes without a field accessor.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::TooManyRecords`] beyond the list's
    /// capacity.
    pub fn from_records(
        kind: RepeaterListKind,
        records: Vec<RepeaterRecord>,
    ) -> Result<Self, RepeaterListError> {
        let count = records.len();
        if count > usize::from(kind.capacity()) {
            return Err(RepeaterListError::TooManyRecords { kind, count });
        }
        let mut entries = Vec::with_capacity(count);
        for (index, mut record) in (0..kind.capacity()).zip(records) {
            let next = index + 1;
            record.set_links(
                index.checked_sub(1),
                (usize::from(next) < count).then_some(next),
            );
            entries.push((RepeaterSlot { kind, index }, record));
        }
        Ok(Self { kind, entries })
    }

    /// The page writes that store this list in place of `image`'s list.
    ///
    /// Each record goes to its own slot, every other slot of the list takes
    /// the empty-slot form, and the stored ends name the first and last
    /// records; every other byte of the table's pages is kept. Only pages
    /// whose bytes change are returned. Apply them with
    /// [`crate::radio::McpSession::compare_exchange_pages`], which compares
    /// each page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(
        &self,
        image: &MemoryImage,
    ) -> Result<Vec<PageReplacement>, crate::Error> {
        let kind = self.kind;
        let start = kind.records_address();
        let pages = u32::from(kind.capacity() / RECORDS_PER_PAGE);
        let mut table = chain::stored_bytes(image, start, pages * PAGE_SIZE_U32);
        let mut by_slot = vec![None; usize::from(kind.capacity())];
        for (slot, record) in &self.entries {
            if let Some(entry) = by_slot.get_mut(usize::from(slot.index)) {
                *entry = Some(record);
            }
        }
        for (index, record) in (0..kind.capacity()).zip(by_slot) {
            let slot = RepeaterSlot { kind, index };
            let stored = record.map_or(EMPTY_SLOT, RepeaterRecord::to_bytes);
            chain::put(&mut table, slot.address() - start, &stored);
        }
        let [first, last] =
            [self.entries.first(), self.entries.last()].map(|end| end.map(|(slot, _)| slot.index));
        chain::put(
            &mut table,
            kind.ends_address() - start,
            &chain::ends_bytes(first, last),
        );
        chain::changed_pages(image, start, &table)
    }
}

/// The world-region, country and group name tables.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeaterNames {
    regions: Vec<StoredText<REPEATER_TEXT_SIZE>>,
    countries: Vec<StoredText<REPEATER_TEXT_SIZE>>,
    groups: Vec<StoredText<REPEATER_TEXT_SIZE>>,
}

impl RepeaterNames {
    /// Decode the three tables; an unnamed entry is empty text.
    ///
    /// # Errors
    ///
    /// Returns [`RepeaterListError::MalformedName`] for an entry that is not
    /// a stored text form.
    pub fn read(image: &MemoryImage) -> Result<Self, RepeaterListError> {
        Ok(Self {
            regions: name_table(
                image,
                REPEATER_REGION_NAMES_ADDRESS,
                u16::from(RegionIndex::COUNT),
            )?,
            countries: name_table(
                image,
                REPEATER_COUNTRY_NAMES_ADDRESS,
                u16::from(CountryIndex::COUNT),
            )?,
            groups: name_table(image, REPEATER_GROUP_NAMES_ADDRESS, GroupIndex::COUNT)?,
        })
    }

    /// The name of world region `index`.
    #[must_use]
    pub fn region(&self, index: RegionIndex) -> &StoredText<REPEATER_TEXT_SIZE> {
        named(&self.regions, usize::from(index.get()))
    }

    /// The name of country `index`.
    #[must_use]
    pub fn country(&self, index: CountryIndex) -> &StoredText<REPEATER_TEXT_SIZE> {
        named(&self.countries, usize::from(index.get()))
    }

    /// The name of group `index`.
    #[must_use]
    pub fn group(&self, index: GroupIndex) -> &StoredText<REPEATER_TEXT_SIZE> {
        named(&self.groups, usize::from(index.get()))
    }

    /// Replace the name of world region `index`.
    pub fn set_region(&mut self, index: RegionIndex, name: StoredText<REPEATER_TEXT_SIZE>) {
        rename(&mut self.regions, usize::from(index.get()), name);
    }

    /// Replace the name of country `index`.
    pub fn set_country(&mut self, index: CountryIndex, name: StoredText<REPEATER_TEXT_SIZE>) {
        rename(&mut self.countries, usize::from(index.get()), name);
    }

    /// Replace the name of group `index`.
    pub fn set_group(&mut self, index: GroupIndex, name: StoredText<REPEATER_TEXT_SIZE>) {
        rename(&mut self.groups, usize::from(index.get()), name);
    }

    /// The page writes that store these tables in place of `image`'s,
    /// keeping every byte between and after the tables; only pages whose
    /// bytes change are returned.
    ///
    /// # Errors
    ///
    /// Returns the [`PageReplacement::new`] errors.
    pub fn page_replacements(
        &self,
        image: &MemoryImage,
    ) -> Result<Vec<PageReplacement>, crate::Error> {
        let start = REPEATER_REGION_NAMES_ADDRESS;
        let end = REPEATER_GROUP_NAMES_ADDRESS + u32::from(GroupIndex::COUNT) * TEXT_SIZE_U32;
        let mut tables =
            chain::stored_bytes(image, start, end.next_multiple_of(PAGE_SIZE_U32) - start);
        for (address, names) in [
            (REPEATER_REGION_NAMES_ADDRESS, &self.regions),
            (REPEATER_COUNTRY_NAMES_ADDRESS, &self.countries),
            (REPEATER_GROUP_NAMES_ADDRESS, &self.groups),
        ] {
            for (offset, name) in (0..).step_by(REPEATER_TEXT_SIZE).zip(names) {
                chain::put(&mut tables, address - start + offset, name.as_bytes());
            }
        }
        chain::changed_pages(image, start, &tables)
    }
}

impl RepeaterRecord {
    /// The page write that stores this record in populated `slot` of `image`,
    /// keeping every other byte of the page.
    ///
    /// The record must carry the slot's stored links, so the edit leaves the
    /// list order unchanged. Apply it with
    /// [`crate::radio::McpSession::compare_exchange_pages`], which compares
    /// the page with a fresh read before writing.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::RepeaterList`] holding
    /// [`RepeaterListError::EmptySlot`], [`RepeaterListError::LinksChanged`]
    /// or the stored record's decoding error, and the
    /// [`PageReplacement::new`] errors.
    pub fn page_replacement(
        &self,
        image: &MemoryImage,
        slot: RepeaterSlot,
    ) -> Result<PageReplacement, crate::Error> {
        let stored = Self::from_bytes(stored_record(image, slot))?
            .ok_or(RepeaterListError::EmptySlot { slot })?;
        if (stored.previous(), stored.next()) != (self.previous(), self.next()) {
            return Err(RepeaterListError::LinksChanged { slot }.into());
        }
        let page_address = slot.page_address();
        let page = Page::new(Address::new(page_address)?, PAGE_SIZE)?;
        let start = page_address as usize;
        let expected = image
            .as_bytes()
            .get(start..start + PAGE_SIZE)
            .unwrap_or_default();
        let mut replacement = expected.to_vec();
        let offset = (slot.address() - page_address) as usize;
        for (destination, source) in replacement.iter_mut().skip(offset).zip(self.to_bytes()) {
            *destination = source;
        }
        Ok(PageReplacement::new(page, expected, &replacement)?)
    }
}

/// A stored repeater or hotspot list, name or record that fails its domain,
/// or an in-place edit the list cannot take.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RepeaterListError {
    /// A slot number at or past its list's capacity.
    #[error("{kind} slot {index} is outside the list's {capacity} slots", capacity = kind.capacity())]
    SlotOutOfRange {
        /// The list.
        kind: RepeaterListKind,
        /// The rejected slot number.
        index: u16,
    },
    /// Text without a stored form, or a stored name that is not one.
    #[error(transparent)]
    Text(#[from] StoredTextError),
    /// A name-table entry that is not a stored text form.
    #[error("the name at {address:#07X} is not text followed by NUL padding")]
    MalformedName {
        /// The entry's image address.
        address: u32,
    },
    /// A callsign field that is not graphic ASCII or space followed by NUL
    /// padding.
    #[error("callsign field {bytes:02X?} is not graphic ASCII or space followed by NUL padding")]
    MalformedCallsign {
        /// The stored bytes.
        bytes: [u8; 8],
    },
    /// A numeric field outside its domain.
    #[error("{field} {value} is outside its domain (maximum {max})")]
    FieldOutOfRange {
        /// The field.
        field: &'static str,
        /// The rejected value.
        value: u32,
        /// The largest accepted value.
        max: u32,
    },
    /// Text that is not a decimal megahertz value with at most six fractional
    /// digits within `u32` hertz.
    #[error("{text:?} is not a decimal megahertz value with at most six fractional digits")]
    InvalidMegahertzText {
        /// The rejected text.
        text: String,
    },
    /// A UTC offset that is not a multiple of 15 minutes from -840 to 840.
    #[error("UTC offset {minutes} minutes is not a multiple of 15 from -840 to 840")]
    UtcOffsetOutOfRange {
        /// The rejected offset in minutes.
        minutes: i16,
    },
    /// A populated record whose fields fail their domains.
    #[error("{slot}: {source}")]
    Record {
        /// The record's slot.
        slot: RepeaterSlot,
        /// The field failure.
        source: Box<Self>,
    },
    /// The chain reaches an empty slot, a slot outside the list, or a record
    /// whose back link names another slot.
    #[error("the {kind} list chain is broken at slot {index}")]
    ChainBroken {
        /// The list.
        kind: RepeaterListKind,
        /// The slot number the chain reached.
        index: u16,
    },
    /// The chain ends other than at the stored last slot.
    #[error("the {kind} list chain ends at {chain:?}, not the stored last slot {stored:?}")]
    EndsMismatch {
        /// The list.
        kind: RepeaterListKind,
        /// The slot number of the chain's last record.
        chain: Option<u16>,
        /// The stored last slot number.
        stored: Option<u16>,
    },
    /// A populated record the chain from the stored first slot never reaches.
    #[error("{kind} slot {index} holds a record outside the list chain")]
    UnchainedRecord {
        /// The list.
        kind: RepeaterListKind,
        /// The record's slot number.
        index: u16,
    },
    /// An in-place edit of an empty slot.
    #[error("{slot} holds no record to edit")]
    EmptySlot {
        /// The addressed slot.
        slot: RepeaterSlot,
    },
    /// An in-place edit whose record carries links other than the slot's.
    #[error("an in-place edit of {slot} must keep its stored links")]
    LinksChanged {
        /// The addressed slot.
        slot: RepeaterSlot,
    },
    /// More records than the list has slots.
    #[error("{count} records exceed the {kind} list's {capacity} slots", capacity = kind.capacity())]
    TooManyRecords {
        /// The list.
        kind: RepeaterListKind,
        /// The number of records supplied.
        count: usize,
    },
}

impl Linked for RepeaterRecord {
    fn links(&self) -> [Option<u16>; 2] {
        [self.previous(), self.next()]
    }
}

/// The 80 bytes of `slot` in `image`.
fn stored_record(image: &MemoryImage, slot: RepeaterSlot) -> [u8; REPEATER_RECORD_SIZE] {
    chain::stored_field(image, slot.address())
}

/// The `count` names of the table at `address`.
fn name_table(
    image: &MemoryImage,
    address: u32,
    count: u16,
) -> Result<Vec<StoredText<REPEATER_TEXT_SIZE>>, RepeaterListError> {
    (0..u32::from(count))
        .map(|entry| {
            let entry_address = address + entry * TEXT_SIZE_U32;
            StoredText::from_bytes(chain::stored_field(image, entry_address)).map_err(
                |_malformed| RepeaterListError::MalformedName {
                    address: entry_address,
                },
            )
        })
        .collect()
}

fn named(
    names: &[StoredText<REPEATER_TEXT_SIZE>],
    index: usize,
) -> &StoredText<REPEATER_TEXT_SIZE> {
    names.get(index).unwrap_or(&StoredText::EMPTY)
}

fn rename(
    names: &mut [StoredText<REPEATER_TEXT_SIZE>],
    index: usize,
    name: StoredText<REPEATER_TEXT_SIZE>,
) {
    if let Some(entry) = names.get_mut(index) {
        *entry = name;
    }
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod tests;
