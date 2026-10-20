//! The D-STAR callsign list held in a captured image: up to 300 named
//! destination callsigns with memos, in display order.
//!
//! The list is a table of 300 slots of 64 bytes from
//! [`CALLSIGN_LIST_ADDRESS`], four to each 256-byte page. An entry stores its
//! name in bytes 0 to 15 and its memo in bytes 24 to 55, both
//! [`StoredText`], its callsign in bytes 16 to 23, NUL padded, a zero byte
//! 59, and the slot numbers of the previous and next entries in bytes 60 to
//! 63, little-endian with `FFFF` for none. Bytes 56 to 58 have no established
//! meaning and entries carry them unchanged. An empty slot holds NUL through
//! byte 55 and `FF` after it. The slot numbers of the first and last entries
//! are stored little-endian at [`CALLSIGN_LIST_ENDS_ADDRESS`], in the page that
//! ends the repeater list. The table and its ends lie inside the standard
//! backup schedule. Store a list with [`CallsignList::from_entries`] and
//! [`CallsignList::page_replacements`].
//!
//! On firmware 1.02, type `K,2,1`, a backup whose list was empty held the
//! empty-slot form in every slot and `FFFF` in both ends. An entry written
//! over the operation-panel USB read back in place in a fresh session,
//! changing only its name, callsign, memo and byte 59 and the four end bytes,
//! and storing the list without it restored every byte of the standard
//! backup. How the radio displays a written entry is unverified.

use std::fmt;

use crate::memory::chain::{self, ChainFault, Linked};
use crate::memory::{MemoryImage, StoredText, StoredTextError};
use crate::radio::programming::PageReplacement;
use crate::types::{PAGE_SIZE_U32, StoredCallsign};

#[cfg(test)]
#[path = "callsign_list_tests.rs"]
mod tests;

/// Address of slot 0.
pub const CALLSIGN_LIST_ADDRESS: u32 = 0x2_5000;
/// Address of the stored slot numbers of the first and last entries, two
/// little-endian `u16`s; `FFFF` stores none.
pub const CALLSIGN_LIST_ENDS_ADDRESS: u32 = 0x4_93F4;
/// Slots in the list.
pub const CALLSIGN_LIST_CAPACITY: u16 = 300;
/// Bytes per slot.
pub const CALLSIGN_ENTRY_SIZE: usize = 64;
/// Bytes of an entry's name.
pub const CALLSIGN_NAME_SIZE: usize = 16;
/// Bytes of an entry's memo.
pub const CALLSIGN_MEMO_SIZE: usize = 32;

const ENTRY_SIZE_U32: u32 = 64;
const NAME: usize = 0;
const CALLSIGN: usize = 16;
const MEMO: usize = 24;
const STATE: usize = 59;
const PREVIOUS: usize = 60;
const NEXT: usize = 62;

/// An empty slot as stored: NUL through byte 55, then `FF`.
const EMPTY_SLOT: [u8; CALLSIGN_ENTRY_SIZE] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
];

/// A slot of the callsign list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallsignSlot(u16);

integer_formats!(CallsignSlot, index);

impl TryFrom<u16> for CallsignSlot {
    type Error = CallsignListError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CallsignSlot> for u16 {
    fn from(value: CallsignSlot) -> Self {
        value.index()
    }
}

impl CallsignSlot {
    /// Validate a slot number.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::SlotOutOfRange`] at or past
    /// [`CALLSIGN_LIST_CAPACITY`].
    pub const fn new(index: u16) -> Result<Self, CallsignListError> {
        if index < CALLSIGN_LIST_CAPACITY {
            Ok(Self(index))
        } else {
            Err(CallsignListError::SlotOutOfRange { index })
        }
    }

    /// The slot number.
    #[must_use]
    pub const fn index(self) -> u16 {
        self.0
    }

    /// The image address of the slot.
    #[must_use]
    pub fn address(self) -> u32 {
        CALLSIGN_LIST_ADDRESS + u32::from(self.0) * ENTRY_SIZE_U32
    }
}

impl fmt::Display for CallsignSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "callsign list slot {}", self.0)
    }
}

/// One populated slot, held as its 64 stored bytes.
///
/// [`Self::from_bytes`] and [`Self::new`] validate every field, so the
/// accessors cannot fail; each setter rewrites only its own field's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallsignEntry {
    bytes: [u8; CALLSIGN_ENTRY_SIZE],
}

impl CallsignEntry {
    /// An entry with `name`, `callsign` and `memo`, its other bytes in the
    /// empty-slot form except the zero byte 59 that marks an entry.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::MalformedCallsign`] unless the callsign
    /// is graphic ASCII or space followed by NUL padding.
    pub fn new(
        name: StoredText<CALLSIGN_NAME_SIZE>,
        callsign: StoredCallsign,
        memo: StoredText<CALLSIGN_MEMO_SIZE>,
    ) -> Result<Self, CallsignListError> {
        let mut entry = Self { bytes: EMPTY_SLOT };
        entry.put(STATE, &[0]);
        entry.set_name(name);
        entry.set_callsign(callsign)?;
        entry.set_memo(memo);
        Ok(entry)
    }

    /// Decode one stored slot; `None` when byte 59 marks it empty.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::Text`] for a name or memo that is not a
    /// stored text form and [`CallsignListError::MalformedCallsign`] for a
    /// callsign that is not graphic ASCII or space followed by NUL padding.
    pub fn from_bytes(bytes: [u8; CALLSIGN_ENTRY_SIZE]) -> Result<Option<Self>, CallsignListError> {
        let entry = Self { bytes };
        if entry.bytes.get(STATE).is_none_or(|&state| state != 0) {
            return Ok(None);
        }
        let _name = StoredText::<CALLSIGN_NAME_SIZE>::from_bytes(entry.field(NAME))?;
        let _memo = StoredText::<CALLSIGN_MEMO_SIZE>::from_bytes(entry.field(MEMO))?;
        validate_callsign(entry.field(CALLSIGN))?;
        Ok(Some(entry))
    }

    /// The stored bytes, including bytes 56 to 58.
    #[must_use]
    pub const fn to_bytes(&self) -> [u8; CALLSIGN_ENTRY_SIZE] {
        self.bytes
    }

    /// The name, bytes 0 to 15.
    #[must_use]
    pub fn name(&self) -> StoredText<CALLSIGN_NAME_SIZE> {
        StoredText::validated(self.field(NAME))
    }

    /// The callsign, bytes 16 to 23.
    #[must_use]
    pub fn callsign(&self) -> StoredCallsign {
        StoredCallsign::from_bytes(self.field(CALLSIGN))
    }

    /// The memo, bytes 24 to 55.
    #[must_use]
    pub fn memo(&self) -> StoredText<CALLSIGN_MEMO_SIZE> {
        StoredText::validated(self.field(MEMO))
    }

    /// The slot number of the previous entry, bytes 60 and 61; `None` for the
    /// first.
    #[must_use]
    pub fn previous(&self) -> Option<u16> {
        link(self.field(PREVIOUS))
    }

    /// The slot number of the next entry, bytes 62 and 63; `None` for the
    /// last.
    #[must_use]
    pub fn next(&self) -> Option<u16> {
        link(self.field(NEXT))
    }

    /// Replace the name.
    pub fn set_name(&mut self, name: StoredText<CALLSIGN_NAME_SIZE>) {
        self.put(NAME, name.as_bytes());
    }

    /// Replace the callsign.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::MalformedCallsign`], leaving the entry
    /// unchanged, unless the bytes are graphic ASCII or space followed by NUL
    /// padding.
    pub fn set_callsign(&mut self, callsign: StoredCallsign) -> Result<(), CallsignListError> {
        validate_callsign(*callsign.as_bytes())?;
        self.put(CALLSIGN, callsign.as_bytes());
        Ok(())
    }

    /// Replace the memo.
    pub fn set_memo(&mut self, memo: StoredText<CALLSIGN_MEMO_SIZE>) {
        self.put(MEMO, memo.as_bytes());
    }

    /// Replace the links; `None` stores `FFFF`.
    fn set_links(&mut self, previous: Option<u16>, next: Option<u16>) {
        self.put(PREVIOUS, &previous.unwrap_or(chain::NO_LINK).to_le_bytes());
        self.put(NEXT, &next.unwrap_or(chain::NO_LINK).to_le_bytes());
    }

    fn field<const N: usize>(&self, offset: usize) -> [u8; N] {
        let mut field = [0; N];
        for (destination, source) in field.iter_mut().zip(self.bytes.iter().skip(offset)) {
            *destination = *source;
        }
        field
    }

    fn put(&mut self, offset: usize, value: &[u8]) {
        for (destination, source) in self.bytes.iter_mut().skip(offset).zip(value) {
            *destination = *source;
        }
    }
}

impl Linked for CallsignEntry {
    fn links(&self) -> [Option<u16>; 2] {
        [self.previous(), self.next()]
    }
}

/// The entries of the callsign list in display order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallsignList {
    entries: Vec<(CallsignSlot, CallsignEntry)>,
}

impl CallsignList {
    /// An empty callsign list.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Decode every slot and follow the chain from the stored first slot.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::Entry`] for an entry whose fields fail
    /// their domains, [`CallsignListError::ChainBroken`] when the chain
    /// reaches an empty slot, a slot outside the list, a slot it already
    /// passed or an entry whose previous link names another slot,
    /// [`CallsignListError::EndsMismatch`] when it ends other than at the
    /// stored last slot, and [`CallsignListError::UnchainedEntry`] for an
    /// entry the chain never reaches.
    pub fn read(image: &MemoryImage) -> Result<Self, CallsignListError> {
        let mut slots = Vec::with_capacity(usize::from(CALLSIGN_LIST_CAPACITY));
        for index in 0..CALLSIGN_LIST_CAPACITY {
            let slot = CallsignSlot(index);
            let entry = CallsignEntry::from_bytes(chain::stored_field(image, slot.address()))
                .map_err(|source| CallsignListError::Entry {
                    slot,
                    source: Box::new(source),
                })?;
            slots.push(entry);
        }
        let entries = chain::follow(slots, chain::stored_ends(image, CALLSIGN_LIST_ENDS_ADDRESS))
            .map_err(|fault| match fault {
                ChainFault::Broken { index } => CallsignListError::ChainBroken { index },
                ChainFault::EndsMismatch { chain, stored } => {
                    CallsignListError::EndsMismatch { chain, stored }
                }
                ChainFault::Unchained { index } => CallsignListError::UnchainedEntry { index },
            })?
            .into_iter()
            .map(|(index, entry)| (CallsignSlot(index), entry))
            .collect();
        Ok(Self { entries })
    }

    /// A list of `entries` in order, stored compact from slot 0.
    ///
    /// Each entry's links are replaced to chain the slots in order; every
    /// other byte is kept.
    ///
    /// # Errors
    ///
    /// Returns [`CallsignListError::TooManyEntries`] beyond
    /// [`CALLSIGN_LIST_CAPACITY`].
    pub fn from_entries(entries: Vec<CallsignEntry>) -> Result<Self, CallsignListError> {
        let count = entries.len();
        if count > usize::from(CALLSIGN_LIST_CAPACITY) {
            return Err(CallsignListError::TooManyEntries { count });
        }
        let mut chained = Vec::with_capacity(count);
        for (index, mut entry) in (0..CALLSIGN_LIST_CAPACITY).zip(entries) {
            let next = index + 1;
            entry.set_links(
                index.checked_sub(1),
                (usize::from(next) < count).then_some(next),
            );
            chained.push((CallsignSlot(index), entry));
        }
        Ok(Self { entries: chained })
    }

    /// The number of entries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the list holds no entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries in display order with their slots.
    #[must_use]
    pub fn iter(&self) -> CallsignListIter<'_> {
        CallsignListIter(self.entries.iter())
    }

    /// The entry in `slot`, when populated.
    #[must_use]
    pub fn get(&self, slot: CallsignSlot) -> Option<&CallsignEntry> {
        self.entries
            .iter()
            .find(|(stored, _)| *stored == slot)
            .map(|(_, entry)| entry)
    }

    /// The page writes that store this list in place of `image`'s list.
    ///
    /// Each entry goes to its own slot, every other slot takes the
    /// empty-slot form, and the stored ends name the first and last entries;
    /// every other byte of the table's pages and of the ends page is kept.
    /// Only pages whose bytes change are returned. Apply them with
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
        let start = CALLSIGN_LIST_ADDRESS;
        let mut table = chain::stored_bytes(
            image,
            start,
            u32::from(CALLSIGN_LIST_CAPACITY) * ENTRY_SIZE_U32,
        );
        let mut by_slot = vec![None; usize::from(CALLSIGN_LIST_CAPACITY)];
        for (slot, entry) in &self.entries {
            if let Some(stored) = by_slot.get_mut(usize::from(slot.0)) {
                *stored = Some(entry);
            }
        }
        for (index, entry) in (0..CALLSIGN_LIST_CAPACITY).zip(by_slot) {
            let stored = entry.map_or(EMPTY_SLOT, CallsignEntry::to_bytes);
            chain::put(&mut table, CallsignSlot(index).address() - start, &stored);
        }
        let mut replacements = chain::changed_pages(image, start, &table)?;
        let ends_page = CALLSIGN_LIST_ENDS_ADDRESS - CALLSIGN_LIST_ENDS_ADDRESS % PAGE_SIZE_U32;
        let mut page = chain::stored_bytes(image, ends_page, PAGE_SIZE_U32);
        let [first, last] =
            [self.entries.first(), self.entries.last()].map(|end| end.map(|(slot, _)| slot.0));
        chain::put(
            &mut page,
            CALLSIGN_LIST_ENDS_ADDRESS - ends_page,
            &chain::ends_bytes(first, last),
        );
        replacements.extend(chain::changed_pages(image, ends_page, &page)?);
        Ok(replacements)
    }
}

impl TryFrom<Vec<CallsignEntry>> for CallsignList {
    type Error = CallsignListError;
    fn try_from(entries: Vec<CallsignEntry>) -> Result<Self, Self::Error> {
        Self::from_entries(entries)
    }
}

impl IntoIterator for CallsignList {
    type Item = (CallsignSlot, CallsignEntry);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl<'a> IntoIterator for &'a CallsignList {
    type Item = (CallsignSlot, &'a CallsignEntry);
    type IntoIter = CallsignListIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Borrowed callsign-list entries and their slots in display order.
#[derive(Debug, Clone)]
pub struct CallsignListIter<'a>(std::slice::Iter<'a, (CallsignSlot, CallsignEntry)>);

impl<'a> Iterator for CallsignListIter<'a> {
    type Item = (CallsignSlot, &'a CallsignEntry);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(slot, entry)| (*slot, entry))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl DoubleEndedIterator for CallsignListIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.0.next_back().map(|(slot, entry)| (*slot, entry))
    }
}

impl ExactSizeIterator for CallsignListIter<'_> {}
impl std::iter::FusedIterator for CallsignListIter<'_> {}

/// A stored callsign list, or an entry or list it refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CallsignListError {
    /// A slot number at or past [`CALLSIGN_LIST_CAPACITY`].
    #[error("callsign list slot {index} is outside the list's 300 slots")]
    SlotOutOfRange {
        /// The rejected slot number.
        index: u16,
    },
    /// Text without a stored form, or a stored name or memo that is not one.
    #[error(transparent)]
    Text(#[from] StoredTextError),
    /// A callsign field that is not graphic ASCII or space followed by NUL
    /// padding.
    #[error("callsign field {bytes:02X?} is not graphic ASCII or space followed by NUL padding")]
    MalformedCallsign {
        /// The stored bytes.
        bytes: [u8; 8],
    },
    /// An entry whose fields fail their domains.
    #[error("{slot}: {source}")]
    Entry {
        /// The entry's slot.
        slot: CallsignSlot,
        /// The field failure.
        source: Box<Self>,
    },
    /// The chain reaches an empty slot, a slot outside the list, a slot it
    /// already passed, or an entry whose previous link names another slot.
    #[error("the callsign list chain is broken at slot {index}")]
    ChainBroken {
        /// The slot number the chain reached.
        index: u16,
    },
    /// The chain ends other than at the stored last slot.
    #[error("the callsign list chain ends at {chain:?}, not the stored last slot {stored:?}")]
    EndsMismatch {
        /// The slot number of the chain's last entry.
        chain: Option<u16>,
        /// The stored last slot number.
        stored: Option<u16>,
    },
    /// An entry the chain from the stored first slot never reaches.
    #[error("callsign list slot {index} holds an entry outside the list chain")]
    UnchainedEntry {
        /// The entry's slot number.
        index: u16,
    },
    /// More entries than the list has slots.
    #[error("{count} entries exceed the callsign list's 300 slots")]
    TooManyEntries {
        /// The number of entries supplied.
        count: usize,
    },
}

/// Graphic ASCII or space, then NUL padding.
fn validate_callsign(field: [u8; 8]) -> Result<(), CallsignListError> {
    if StoredCallsign::from_bytes(field).is_padded_text() {
        Ok(())
    } else {
        Err(CallsignListError::MalformedCallsign { bytes: field })
    }
}

/// A stored link; `FFFF` names no slot.
fn link(bytes: [u8; 2]) -> Option<u16> {
    Some(u16::from_le_bytes(bytes)).filter(|&raw| raw != chain::NO_LINK)
}
