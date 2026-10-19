//! The six-entry D-STAR MY callsign list (`DC`) and its selection (`DS`) of
//! one Programmable-Memory slot, decoded from a captured image.
//!
//! The list lies outside the generated registry. With PM Off active on
//! firmware 1.02, `DC` slot N was stored 12 × (N − 1) bytes past
//! [`MY_CALLSIGN_LIST_ADDRESS`]: eight callsign bytes, then four memo bytes,
//! each NUL padded; [`MY_CALLSIGN_SELECTION_ADDRESS`] holds the selected slot
//! counted from zero. Each other PM slot keeps its list at
//! the same offsets one [`SLOT_STRIDE`] per slot further on. This is the list
//! of ordinary DV operation, not the DV Gateway MY list of the
//! [`super::TextSetting::DvGatewayMyCallsign1`] family; write it over CAT
//! with [`crate::Radio::set_dstar_callsign`] and
//! [`crate::Radio::set_dstar_slot`].
//!
//! [`SLOT_STRIDE`]: crate::types::SLOT_STRIDE

use crate::error::ValidationError;
use crate::memory::MemoryImage;
use crate::types::{DstarCallsignEntry, DstarSlot, Region, SlotIndex};

/// Address of PM Off's selection byte.
pub const MY_CALLSIGN_SELECTION_ADDRESS: u32 = 0x5_0003;
/// Address of PM Off's first entry.
pub const MY_CALLSIGN_LIST_ADDRESS: u32 = 0x5_0004;
/// Bytes per entry: the callsign, then the memo.
pub const MY_CALLSIGN_ENTRY_SIZE: u32 = 12;

const ENTRY_COUNT: u32 = 6;
const CALLSIGN_SIZE: usize = DstarCallsignEntry::MAX_CALLSIGN_LEN;
const SELECTION_MAX: u8 = DstarSlot::MAX - DstarSlot::MIN;

/// The decoded list and selection of one PM slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MyCallsignList {
    entries: [DstarCallsignEntry; 6],
    selected: DstarSlot,
}

impl MyCallsignList {
    /// The image bytes the list of `slot` occupies: the selection byte and
    /// the six entries.
    ///
    /// # Errors
    ///
    /// Never fails for the fixed layout; the `Result` is the region
    /// constructor's.
    pub const fn region(slot: SlotIndex) -> Result<Region, ValidationError> {
        let start = MY_CALLSIGN_SELECTION_ADDRESS + slot.offset();
        Region::new(
            start,
            MY_CALLSIGN_LIST_ADDRESS + slot.offset() + ENTRY_COUNT * MY_CALLSIGN_ENTRY_SIZE,
        )
    }

    /// Decode the list of `slot` from `image`.
    ///
    /// A field is its text followed only by NUL padding.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCallsignText`] for a field whose
    /// bytes are not text then padding (erased `0xFF` bytes included) or
    /// whose text [`DstarCallsignEntry::new`] refuses, and
    /// [`ValidationError::SettingOutOfRange`] for a selection byte above 5.
    pub fn read(image: &MemoryImage, slot: SlotIndex) -> Result<Self, ValidationError> {
        let region = Self::region(slot)?;
        let bytes = image
            .as_bytes()
            .get(region.start() as usize..region.end() as usize)
            .unwrap_or_default();
        let Some((&selection, entries)) = bytes.split_first() else {
            return Err(ValidationError::ImageLength {
                actual: image.as_bytes().len(),
                expected: crate::types::IMAGE_LENGTH,
            });
        };
        if selection > SELECTION_MAX {
            return Err(ValidationError::SettingOutOfRange {
                setting: "MY callsign selection",
                value: u64::from(selection),
                max: u64::from(SELECTION_MAX),
            });
        }
        let mut decoded = Vec::with_capacity(ENTRY_COUNT as usize);
        for (number, entry) in (DstarSlot::MIN..=DstarSlot::MAX)
            .zip(entries.chunks_exact(MY_CALLSIGN_ENTRY_SIZE as usize))
        {
            let (callsign, memo) = entry.split_at(CALLSIGN_SIZE);
            decoded.push(DstarCallsignEntry::new(
                DstarSlot::new(number)?,
                &padded_text(callsign)?,
                &padded_text(memo)?,
            )?);
        }
        let entries = <[DstarCallsignEntry; 6]>::try_from(decoded).map_err(|_decoded| {
            ValidationError::ImageLength {
                actual: image.as_bytes().len(),
                expected: crate::types::IMAGE_LENGTH,
            }
        })?;
        Ok(Self {
            entries,
            selected: DstarSlot::new(selection + DstarSlot::MIN)?,
        })
    }

    /// The six entries, slot 1 first.
    #[must_use]
    pub const fn entries(&self) -> &[DstarCallsignEntry; 6] {
        &self.entries
    }

    /// The entry of `slot`.
    #[must_use]
    pub const fn entry(&self, slot: DstarSlot) -> &DstarCallsignEntry {
        let [one, two, three, four, five, six] = &self.entries;
        match slot.as_raw() {
            1 => one,
            2 => two,
            3 => three,
            4 => four,
            5 => five,
            _ => six,
        }
    }

    /// The selected slot.
    #[must_use]
    pub const fn selected(&self) -> DstarSlot {
        self.selected
    }
}

/// The text of a NUL-padded field.
fn padded_text(field: &[u8]) -> Result<String, ValidationError> {
    let length = field
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(field.len());
    let (text, padding) = field.split_at(length);
    if padding.iter().any(|&byte| byte != 0) || !text.is_ascii() {
        return Err(ValidationError::InvalidCallsignText {
            text: String::from_utf8_lossy(field).into_owned(),
        });
    }
    String::from_utf8(text.to_vec()).map_err(|_| ValidationError::InvalidCallsignText {
        text: String::from_utf8_lossy(field).into_owned(),
    })
}

#[cfg(test)]
#[path = "my_callsigns_tests.rs"]
mod tests;
