//! Typed access to channel data within a memory image.
//!
//! Channels are stored across three separate memory regions:
//!
//! - **Flags** at byte offset `0x2000`: 4 bytes per entry, 1,200 entries.
//! - **Data** at byte offset `0x4000`: 40 bytes per channel in 192 memgroups
//!   of 6 channels each (256 bytes per memgroup including 16 bytes padding).
//! - **Names** at byte offset `0x10000`: 16 bytes per name, 1,200 entries.
//!
//! # Address verification
//!
//! These MCP byte offsets are confirmed by the memory dump fixture and are
//! consistent with the memory map documentation. Note that some tools use
//! file-based addressing (offset by +0x100 for the `.d75` file header),
//! so addresses `0x2100`, `0x0100`, `0x10100` correspond to MCP byte
//! addresses `0x2000`, `0x0000`, `0x10000` respectively. Our offsets are
//! MCP byte addresses (no file header offset).
//!
//! The [`ChannelAccess`] struct borrows the raw image and provides methods
//! to read individual channels or iterate over all populated channels.

use crate::error::ValidationError;
use crate::protocol::programming::{
    self, CHANNEL_DATA_RECORD_COUNT, CHANNEL_RECORD_SIZE, CHANNELS_PER_MEMGROUP, FLAG_RECORD_SIZE,
    MEMGROUP_COUNT, NAME_ENTRY_SIZE, PAGE_SIZE,
};
use crate::types::{
    ChannelDisplayName, MemoryChannelBand, MemoryGroup, RegularChannel, StoredChannel,
    StoredChannelData, StoredChannelFlag,
};

use super::MemoryError;

// ---------------------------------------------------------------------------
// Byte offsets within the MCP memory image
// ---------------------------------------------------------------------------

/// Byte offset of channel flags (1,200 entries x 4 bytes).
const FLAGS_OFFSET: usize = 0x2000;

/// Byte offset of channel memory data (192 memgroups x 256 bytes).
const DATA_OFFSET: usize = 0x4000;

/// Byte offset of channel names (1,200 entries x 16 bytes).
const NAMES_OFFSET: usize = 0x10000;

// ---------------------------------------------------------------------------
// ChannelEntry
// ---------------------------------------------------------------------------

/// One regular memory-channel slot and all three records that define it.
///
/// The fields are private so callers cannot pair a programmed flag with an
/// invalid receive frequency or place an unrepresentable raw bit value in a
/// channel that will later be written. Use [`ChannelEntry::new_programmed`] for a
/// new channel and [`ChannelEntry::empty`] to clear a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEntry {
    number: RegularChannel,
    name: ChannelDisplayName,
    data: StoredChannelData,
    flag: StoredChannelFlag,
}

impl ChannelEntry {
    /// Construct an empty regular-channel slot.
    #[must_use]
    pub fn empty(number: RegularChannel) -> Self {
        Self {
            number,
            name: ChannelDisplayName::default(),
            data: StoredChannelData::new_unprogrammed([0xFF; StoredChannel::BYTE_SIZE]),
            flag: StoredChannelFlag::empty_for_regular_channel(number),
        }
    }

    /// Construct a programmed regular-channel slot.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] when the receive frequency is the
    /// zero/erased marker rather than a programmed frequency.
    pub fn new_programmed(
        number: RegularChannel,
        name: ChannelDisplayName,
        stored_channel: StoredChannel,
        band: MemoryChannelBand,
        group: MemoryGroup,
        scan_lockout: bool,
    ) -> Result<Self, ValidationError> {
        let entry = Self {
            number,
            name,
            data: StoredChannelData::new_programmed(stored_channel)?,
            flag: StoredChannelFlag::programmed(band, group, scan_lockout),
        };
        entry.validate()?;
        Ok(entry)
    }

    fn from_stored_parts(
        number: RegularChannel,
        name: ChannelDisplayName,
        data: StoredChannelData,
        flag: StoredChannelFlag,
    ) -> Result<Self, ValidationError> {
        let entry = Self {
            number,
            name,
            data,
            flag,
        };
        entry.validate()?;
        Ok(entry)
    }

    /// Return this slot's regular channel number.
    #[must_use]
    pub const fn number(&self) -> RegularChannel {
        self.number
    }

    /// Return the validated 16-byte display name.
    #[must_use]
    pub const fn name(&self) -> &ChannelDisplayName {
        &self.name
    }

    /// Return the decoded record when this slot is programmed.
    #[must_use]
    pub const fn programmed(&self) -> Option<&StoredChannel> {
        self.data.programmed()
    }

    /// Return the exact typed-or-preserved channel data.
    #[must_use]
    pub const fn data(&self) -> &StoredChannelData {
        &self.data
    }

    /// Return the exact decoded flag record, including opaque bits.
    #[must_use]
    pub const fn flag(&self) -> StoredChannelFlag {
        self.flag
    }

    /// Return whether this slot is programmed.
    #[must_use]
    pub const fn is_programmed(&self) -> bool {
        self.flag.is_programmed()
    }

    /// Consume the entry and return its independently encoded records.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        RegularChannel,
        ChannelDisplayName,
        StoredChannelData,
        StoredChannelFlag,
    ) {
        (self.number, self.name, self.data, self.flag)
    }

    /// Validate that this entry can be written without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] when a programmed slot has a zero receive
    /// frequency or the erased `u32::MAX` marker.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if let Some(channel) = self.programmed() {
            channel.validate_programmed()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ChannelAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to channel data within a memory image.
///
/// This struct borrows the raw image bytes and provides methods to
/// read individual channels by number, iterate over populated channels,
/// and check channel status without copying data.
#[derive(Debug)]
pub struct ChannelAccess<'a> {
    image: &'a [u8],
}

impl<'a> ChannelAccess<'a> {
    /// Create a new channel accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the number of populated (non-empty) regular channels (0-999).
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if a regular channel's flag record
    /// is malformed.
    pub fn count(&self) -> Result<usize, MemoryError> {
        let mut count = 0;
        for channel in RegularChannel::all() {
            count += usize::from(self.is_used(channel)?);
        }
        Ok(count)
    }

    /// Check if a channel slot is in use.
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if the channel's flag record is
    /// missing or malformed.
    pub fn is_used(&self, number: RegularChannel) -> Result<bool, MemoryError> {
        Ok(!self.flag(number)?.is_empty())
    }

    /// Get a specific channel by number.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if any populated channel field is
    /// missing or malformed.
    pub fn get(&self, number: RegularChannel) -> Result<ChannelEntry, MemoryError> {
        let flag = self.flag(number)?;
        let data = self.stored_data_with_flag(number, flag)?;
        let name = self.name(number)?;

        ChannelEntry::from_stored_parts(number, name, data, flag).map_err(|error| {
            MemoryError::ParseError {
                region: format!("channel {number}"),
                detail: error.to_string(),
            }
        })
    }

    /// Get all populated regular channels (0-999).
    ///
    /// Skips empty channel slots. The returned entries are in channel
    /// number order.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] when any populated channel record,
    /// name, or flag is missing or malformed.
    pub fn all(&self) -> Result<Vec<ChannelEntry>, MemoryError> {
        let mut entries = Vec::new();
        for channel in RegularChannel::all() {
            let entry = self.get(channel)?;
            if entry.is_programmed() {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Get all channel entries (0-999), including empty slots.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] when any channel record, name, or
    /// flag is missing or malformed.
    pub fn all_slots(&self) -> Result<Vec<ChannelEntry>, MemoryError> {
        let mut entries = Vec::with_capacity(RegularChannel::COUNT);
        for channel in RegularChannel::all() {
            entries.push(self.get(channel)?);
        }
        Ok(entries)
    }

    /// Get the display name for a channel.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if the fixed-width field is not a
    /// valid channel display name.
    pub fn name(&self, number: RegularChannel) -> Result<ChannelDisplayName, MemoryError> {
        let number_usize = usize::from(number);
        let offset = NAMES_OFFSET + number_usize * NAME_ENTRY_SIZE;
        let bytes = self
            .image
            .get(offset..offset + NAME_ENTRY_SIZE)
            .ok_or_else(|| MemoryError::ParseError {
                region: format!("channel {number} name"),
                detail: "name entry is outside the memory image".to_owned(),
            })?;
        let wire: [u8; NAME_ENTRY_SIZE] =
            bytes.try_into().map_err(|_| MemoryError::ParseError {
                region: format!("channel {number} name"),
                detail: format!("expected {NAME_ENTRY_SIZE} bytes, got {}", bytes.len()),
            })?;
        programming::decode_channel_display_name(wire).map_err(|error| MemoryError::ParseError {
            region: format!("channel {number} name"),
            detail: error.to_string(),
        })
    }

    /// Get the channel flag (used/band, lockout, group) for a channel.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if the record is missing, has an
    /// unverified programmed-channel band code, or assigns a populated
    /// channel outside groups 0-29.
    pub fn flag(&self, number: RegularChannel) -> Result<StoredChannelFlag, MemoryError> {
        let number_usize = usize::from(number);
        let offset = FLAGS_OFFSET + number_usize * FLAG_RECORD_SIZE;
        let slice = self
            .image
            .get(offset..offset + FLAG_RECORD_SIZE)
            .ok_or_else(|| MemoryError::ParseError {
                region: format!("channel {number} flag"),
                detail: "flag record is outside the memory image".to_owned(),
            })?;
        programming::parse_channel_flag(slice).map_err(|error| MemoryError::ParseError {
            region: format!("channel {number} flag"),
            detail: error.to_string(),
        })
    }

    /// Get the exact 40-byte stored data for a channel.
    ///
    /// Programmed records are decoded as [`StoredChannel`]; empty records are
    /// returned as preserved raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] if the record is missing or
    /// malformed.
    pub fn stored_data(&self, number: RegularChannel) -> Result<StoredChannelData, MemoryError> {
        let flag = self.flag(number)?;
        self.stored_data_with_flag(number, flag)
    }

    fn stored_data_with_flag(
        &self,
        number: RegularChannel,
        flag: StoredChannelFlag,
    ) -> Result<StoredChannelData, MemoryError> {
        let number_usize = usize::from(number);

        // Channel data layout: memgroup = ch / 6, slot = ch % 6
        // byte_offset = 0x4000 + memgroup * 256 + slot * 40
        let memgroup = number_usize / CHANNELS_PER_MEMGROUP;
        let slot = number_usize % CHANNELS_PER_MEMGROUP;

        debug_assert!(
            memgroup < MEMGROUP_COUNT,
            "validated regular channel must map inside the channel memory groups"
        );

        let offset = DATA_OFFSET + memgroup * PAGE_SIZE + slot * CHANNEL_RECORD_SIZE;
        let slice = self
            .image
            .get(offset..offset + CHANNEL_RECORD_SIZE)
            .ok_or_else(|| MemoryError::ParseError {
                region: format!("channel {number} data"),
                detail: "channel record is outside the memory image".to_owned(),
            })?;
        StoredChannelData::from_bytes(slice, flag).map_err(|error| MemoryError::ParseError {
            region: format!("channel {number} data"),
            detail: error.to_string(),
        })
    }

    /// Get all validated regular-channel names (0-999).
    ///
    /// Empty names are represented as empty strings.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] when any fixed-width name field is
    /// missing or invalid.
    pub fn names(&self) -> Result<Vec<ChannelDisplayName>, MemoryError> {
        let mut names = Vec::with_capacity(RegularChannel::COUNT);
        for channel in RegularChannel::all() {
            names.push(self.name(channel)?);
        }
        Ok(names)
    }

    /// Get a group name by group index (0-29).
    ///
    /// Group names are stored at name indices 1152-1181.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] when the fixed-width group-name
    /// field is missing or invalid.
    pub fn group_name(&self, group: MemoryGroup) -> Result<ChannelDisplayName, MemoryError> {
        let name_index = 1152 + usize::from(group);
        let offset = NAMES_OFFSET + name_index * NAME_ENTRY_SIZE;
        let bytes = self
            .image
            .get(offset..offset + NAME_ENTRY_SIZE)
            .ok_or_else(|| MemoryError::ParseError {
                region: format!("group {group} name"),
                detail: "group-name entry is outside the memory image".to_owned(),
            })?;
        let wire: [u8; NAME_ENTRY_SIZE] =
            bytes.try_into().map_err(|_| MemoryError::ParseError {
                region: format!("group {group} name"),
                detail: format!("expected {NAME_ENTRY_SIZE} bytes, got {}", bytes.len()),
            })?;
        programming::decode_channel_display_name(wire).map_err(|error| MemoryError::ParseError {
            region: format!("group {group} name"),
            detail: error.to_string(),
        })
    }

    /// Get all 30 group names.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ParseError`] when any fixed-width group-name
    /// field is missing or invalid.
    pub fn group_names(&self) -> Result<Vec<ChannelDisplayName>, MemoryError> {
        let mut names = Vec::with_capacity(MemoryGroup::COUNT);
        for group in MemoryGroup::all() {
            names.push(self.group_name(group)?);
        }
        Ok(names)
    }
}

// ---------------------------------------------------------------------------
// ChannelWriter (mutable access)
// ---------------------------------------------------------------------------

/// Mutable access to channel data within a memory image.
///
/// Created via [`MemoryImage::channels_mut`](super::MemoryImage).
#[derive(Debug)]
pub struct ChannelWriter<'a> {
    image: &'a mut [u8],
}

impl<'a> ChannelWriter<'a> {
    /// Create a new mutable channel accessor.
    pub(crate) const fn new(image: &'a mut [u8]) -> Self {
        Self { image }
    }

    /// Write a channel entry into the memory image.
    ///
    /// Updates the flag, memory data, and name regions for the given
    /// channel number.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ChannelOutOfRange`] if the channel number
    /// has no corresponding 40-byte channel data record or if the backing
    /// image does not contain every destination. The image is unchanged on
    /// error.
    pub fn set(&mut self, entry: &ChannelEntry) -> Result<(), MemoryError> {
        entry
            .validate()
            .map_err(|error| MemoryError::InvalidChannelEntry {
                channel: entry.number(),
                detail: error.to_string(),
            })?;

        let number = usize::from(entry.number());
        if number >= CHANNEL_DATA_RECORD_COUNT {
            return Err(MemoryError::ChannelOutOfRange {
                channel: entry.number().as_raw(),
                max: RegularChannel::MAX,
            });
        }

        let out_of_range = || MemoryError::ChannelOutOfRange {
            channel: entry.number().as_raw(),
            max: RegularChannel::MAX,
        };

        let flag_offset = FLAGS_OFFSET + number * FLAG_RECORD_SIZE;
        let flag_end = flag_offset + FLAG_RECORD_SIZE;

        let memgroup = number / CHANNELS_PER_MEMGROUP;
        let slot = number % CHANNELS_PER_MEMGROUP;
        let data_offset = DATA_OFFSET + memgroup * PAGE_SIZE + slot * CHANNEL_RECORD_SIZE;
        let data_end = data_offset + CHANNEL_RECORD_SIZE;

        let name_offset = NAMES_OFFSET + number * NAME_ENTRY_SIZE;
        let name_end = name_offset + NAME_ENTRY_SIZE;

        // Obtain all three disjoint destinations before changing any of them.
        // This makes a short/corrupt backing image an all-or-nothing failure.
        let Some((flags_region, data_and_names)) = self.image.split_at_mut_checked(DATA_OFFSET)
        else {
            return Err(out_of_range());
        };
        let Some((data_region, names_region)) =
            data_and_names.split_at_mut_checked(NAMES_OFFSET - DATA_OFFSET)
        else {
            return Err(out_of_range());
        };

        let Some(flag_bytes) = flags_region.get_mut(flag_offset..flag_end) else {
            return Err(out_of_range());
        };
        let data_relative = data_offset - DATA_OFFSET;
        let Some(data_bytes) = data_region.get_mut(data_relative..data_end - DATA_OFFSET) else {
            return Err(out_of_range());
        };
        let name_relative = name_offset - NAMES_OFFSET;
        let Some(name_bytes) = names_region.get_mut(name_relative..name_end - NAMES_OFFSET) else {
            return Err(out_of_range());
        };

        let next_flag = entry.flag().to_wire_bytes();
        let next_data = entry.data().to_bytes();
        let next_name = entry.name().to_wire_bytes();

        // All fallible validation and destination acquisition completed above;
        // these three fixed-size copies are the transaction's commit point.
        flag_bytes.copy_from_slice(&next_flag);
        data_bytes.copy_from_slice(&next_data);
        name_bytes.copy_from_slice(&next_name);

        Ok(())
    }

    /// Write a validated group name.
    ///
    /// Group indices are 0-29.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ChannelOutOfRange`] if the backing image does
    /// not contain the group's fixed-width name field.
    pub fn set_group_name(
        &mut self,
        group: MemoryGroup,
        name: &ChannelDisplayName,
    ) -> Result<(), MemoryError> {
        let name_index = 1152 + usize::from(group);
        let offset = NAMES_OFFSET + name_index * NAME_ENTRY_SIZE;
        let dst = self
            .image
            .get_mut(offset..offset + NAME_ENTRY_SIZE)
            .ok_or_else(|| MemoryError::ChannelOutOfRange {
                channel: u16::from(group.as_raw()),
                max: u16::from(MemoryGroup::MAX),
            })?;

        dst.copy_from_slice(&name.to_wire_bytes());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;
    use crate::types::{Frequency, MemoryChannelBand};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
        let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
        wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
        StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
            unreachable!("fixed all-zero synthetic channel record must decode: {error}")
        })
    }

    /// Set a single byte at `offset` in a mutable slice, returning an error if out of range.
    fn set_byte(image: &mut [u8], offset: usize, value: u8) -> Result<(), BoxErr> {
        let img_len = image.len();
        *image
            .get_mut(offset)
            .ok_or_else(|| format!("set_byte: offset {offset} out of range (len={img_len})"))? =
            value;
        Ok(())
    }

    /// Copy `data` into `image` starting at `offset`.
    fn write_slice(image: &mut [u8], offset: usize, data: &[u8]) -> Result<(), BoxErr> {
        let end = offset + data.len();
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("write_slice: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .copy_from_slice(data);
        Ok(())
    }

    /// Fill `len` bytes starting at `offset` with `value`.
    fn fill_range(image: &mut [u8], offset: usize, len: usize, value: u8) -> Result<(), BoxErr> {
        let end = offset + len;
        let img_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("fill_range: range {offset}..{end} out of bounds (len={img_len})")
            })?
            .fill(value);
        Ok(())
    }

    /// Create a test image with known channel data.
    fn make_test_image() -> Result<Vec<u8>, BoxErr> {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];

        // Zero out the names region (real radio uses null bytes for empty names).
        fill_range(
            &mut image,
            NAMES_OFFSET,
            programming::TOTAL_CHANNEL_ENTRIES * NAME_ENTRY_SIZE,
            0x00,
        )?;

        // Set up channel 0 as a used VHF channel.
        // Flag at 0x2000: [0x00 (VHF), 0x00 (no lockout), 0x00 (group 0), 0xFF]
        set_byte(&mut image, 0x2000, 0x00)?; // used = VHF
        set_byte(&mut image, 0x2001, 0x00)?; // no lockout
        set_byte(&mut image, 0x2002, 0x00)?; // group 0
        set_byte(&mut image, 0x2003, 0xFF)?;

        // Channel 0 data at memgroup 0, slot 0 = offset 0x4000.
        // Write a valid 40-byte channel record with 146.520 MHz.
        let freq: u32 = 146_520_000;
        write_slice(&mut image, 0x4000, &freq.to_le_bytes())?;
        // TX offset = 0
        write_slice(&mut image, 0x4004, &[0, 0, 0, 0])?;
        // Step size 0 (5 kHz) | shift 0 (simplex)
        set_byte(&mut image, 0x4008, 0x00)?;
        // Mode/flags byte 0x09: all zero (FM, no reverse, no tone, CTCSS off)
        set_byte(&mut image, 0x4009, 0x00)?;
        // Byte 0x0A: DCS off, etc.
        set_byte(&mut image, 0x400A, 0x00)?;
        // Tone/CTCSS/DCS indices
        set_byte(&mut image, 0x400B, 0x00)?;
        set_byte(&mut image, 0x400C, 0x00)?;
        set_byte(&mut image, 0x400D, 0x00)?;
        // Data speed / lockout
        set_byte(&mut image, 0x400E, 0x00)?;
        // D-STAR callsigns: 24 bytes of zeros (three empty NUL-padded fields).
        fill_range(&mut image, 0x400F, 24, 0x00)?;
        // data_mode
        set_byte(&mut image, 0x4027, 0x00)?;

        // Channel 0 name at 0x10000: "2M CALL"
        write_slice(&mut image, 0x10000, b"2M CALL\0\0\0\0\0\0\0\0\0")?;

        // Set up channel 1 as empty (default 0xFF in flags is already there).

        // Set up channel 5 as used UHF (to test crossing memgroup boundary
        // -- ch 5 is still in memgroup 0, slot 5).
        set_byte(&mut image, 0x2000 + 5 * 4, 0x02)?; // used = UHF
        set_byte(&mut image, 0x2000 + 5 * 4 + 1, 0x01)?; // lockout = yes
        set_byte(&mut image, 0x2000 + 5 * 4 + 2, 0x03)?; // group 3
        set_byte(&mut image, 0x2000 + 5 * 4 + 3, 0xFF)?;

        // Channel 5 data at memgroup 0, slot 5 = offset 0x4000 + 5 * 40 = 0x40C8.
        let ch5_freq: u32 = 446_000_000;
        write_slice(&mut image, 0x40C8, &ch5_freq.to_le_bytes())?;
        write_slice(&mut image, 0x40CC, &[0, 0, 0, 0])?;
        set_byte(&mut image, 0x40D0, 0x00)?;
        set_byte(&mut image, 0x40D1, 0x00)?;
        set_byte(&mut image, 0x40D2, 0x00)?;
        set_byte(&mut image, 0x40D3, 0x00)?;
        set_byte(&mut image, 0x40D4, 0x00)?;
        set_byte(&mut image, 0x40D5, 0x00)?;
        set_byte(&mut image, 0x40D6, 0x00)?;
        fill_range(&mut image, 0x40D7, 24, 0x00)?;
        set_byte(&mut image, 0x40EF, 0x00)?;

        // Channel 5 name.
        write_slice(&mut image, 0x10000 + 5 * 16, b"UHF CHAN\0\0\0\0\0\0\0\0")?;

        Ok(image)
    }

    #[test]
    fn from_raw_valid_size() {
        let image = vec![0u8; TOTAL_SIZE];
        assert!(super::super::MemoryImage::from_raw(image).is_ok());
    }

    #[test]
    fn from_raw_invalid_size() -> TestResult {
        let image = vec![0u8; 1000];
        let err = super::super::MemoryImage::from_raw(image)
            .err()
            .ok_or("expected InvalidSize error but got Ok")?;
        assert!(
            matches!(err, MemoryError::InvalidSize { .. }),
            "expected InvalidSize, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn channel_is_used() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert!(ch.is_used(RegularChannel::new(0)?)?);
        assert!(!ch.is_used(RegularChannel::new(1)?)?);
        assert!(ch.is_used(RegularChannel::new(5)?)?);
        Ok(())
    }

    #[test]
    fn channel_count() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.count()?, 2); // channels 0 and 5
        Ok(())
    }

    #[test]
    fn channel_get_name() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.name(RegularChannel::new(0)?)?.as_str(), "2M CALL");
        assert_eq!(ch.name(RegularChannel::new(5)?)?.as_str(), "UHF CHAN");
        assert!(ch.name(RegularChannel::new(1)?)?.is_empty());
        Ok(())
    }

    #[test]
    fn channel_get_entry() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();

        let entry0 = ch.get(RegularChannel::new(0)?)?;
        assert!(entry0.is_programmed());
        assert_eq!(entry0.flag().scan_lockout(), Some(false));
        assert_eq!(entry0.name().as_str(), "2M CALL");
        assert_eq!(
            entry0
                .programmed()
                .ok_or("channel 0 should be programmed")?
                .receive_frequency
                .as_hz(),
            146_520_000,
        );

        let entry5 = ch.get(RegularChannel::new(5)?)?;
        assert!(entry5.is_programmed());
        assert_eq!(entry5.flag().scan_lockout(), Some(true));
        assert_eq!(entry5.name().as_str(), "UHF CHAN");
        assert_eq!(
            entry5
                .programmed()
                .ok_or("channel 5 should be programmed")?
                .receive_frequency
                .as_hz(),
            446_000_000,
        );
        Ok(())
    }

    #[test]
    fn channel_all_returns_only_used() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        let all = ch.all()?;
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.first().ok_or("all[0] missing")?.number(),
            RegularChannel::new(0)?
        );
        assert_eq!(
            all.get(1).ok_or("all[1] missing")?.number(),
            RegularChannel::new(5)?
        );
        Ok(())
    }

    #[test]
    fn channel_flag() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();

        let ch0_flag = ch.flag(RegularChannel::new(0)?)?;
        assert_eq!(ch0_flag.band(), Some(MemoryChannelBand::Vhf));
        assert_eq!(ch0_flag.scan_lockout(), Some(false));
        assert_eq!(ch0_flag.group(), Some(MemoryGroup::new(0)?));
        assert_eq!(ch0_flag.to_wire_bytes(), [0x00, 0x00, 0x00, 0xFF]);

        let ch5_flag = ch.flag(RegularChannel::new(5)?)?;
        assert_eq!(ch5_flag.band(), Some(MemoryChannelBand::Uhf));
        assert_eq!(ch5_flag.scan_lockout(), Some(true));
        assert_eq!(ch5_flag.group(), Some(MemoryGroup::new(3)?));
        assert_eq!(ch5_flag.to_wire_bytes(), [0x02, 0x01, 0x03, 0xFF]);
        Ok(())
    }

    #[test]
    fn channel_group_names() -> TestResult {
        let mut image = make_test_image()?;
        // Write a group name at index 1152 (group 0).
        write_slice(&mut image, 0x10000 + 1152 * 16, b"Ham Radio\0\0\0\0\0\0\0")?;

        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.group_name(MemoryGroup::new(0)?)?.as_str(), "Ham Radio");
        assert!(ch.group_name(MemoryGroup::new(1)?)?.is_empty());
        Ok(())
    }

    #[test]
    fn channel_writer_set() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

        let entry = ChannelEntry::new_programmed(
            RegularChannel::new(10)?,
            ChannelDisplayName::new("TEST CH")?,
            synthetic_stored_channel(Frequency::new(145_000_000)),
            MemoryChannelBand::Vhf,
            MemoryGroup::new(0)?,
            false,
        )?;

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set(&entry)?;
        }

        let ch = mi.channels();
        let channel = RegularChannel::new(10)?;
        assert!(ch.is_used(channel)?);
        let read_back = ch.get(channel)?;
        assert!(read_back.is_programmed());
        assert_eq!(read_back.name().as_str(), "TEST CH");
        assert_eq!(
            read_back
                .programmed()
                .ok_or("written channel should be programmed")?
                .receive_frequency
                .as_hz(),
            145_000_000,
        );
        Ok(())
    }

    #[test]
    fn channel_writer_group_name() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set_group_name(MemoryGroup::new(0)?, &ChannelDisplayName::new("My Group")?)?;
        }

        let ch = mi.channels();
        assert_eq!(ch.group_name(MemoryGroup::new(0)?)?.as_str(), "My Group");
        Ok(())
    }

    #[test]
    fn channel_writer_accepts_last_regular_channel() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

        let entry = ChannelEntry::new_programmed(
            RegularChannel::new(RegularChannel::MAX)?,
            ChannelDisplayName::new("LAST DATA SLOT")?,
            synthetic_stored_channel(Frequency::new(433_920_000)),
            MemoryChannelBand::Uhf,
            MemoryGroup::new(0)?,
            true,
        )?;

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set(&entry)?;
        }

        let read_back = mi
            .channels()
            .get(RegularChannel::new(RegularChannel::MAX)?)?;
        assert!(read_back.is_programmed());
        assert_eq!(read_back.flag().scan_lockout(), Some(true));
        assert_eq!(read_back.name().as_str(), "LAST DATA SLOT");
        assert_eq!(
            read_back
                .programmed()
                .ok_or("last regular channel should be programmed")?
                .receive_frequency
                .as_hz(),
            433_920_000,
        );
        Ok(())
    }

    #[test]
    fn channel_writer_short_image_error_is_transactional() -> TestResult {
        // Flags and channel data for channel 10 exist, but its name destination
        // does not. The writer must discover that before changing the earlier
        // regions.
        let mut image = vec![0x7C; NAMES_OFFSET];
        let before = image.clone();
        let entry = ChannelEntry::new_programmed(
            RegularChannel::new(10)?,
            ChannelDisplayName::new("NO DESTINATION")?,
            synthetic_stored_channel(Frequency::new(145_000_000)),
            MemoryChannelBand::Vhf,
            MemoryGroup::new(0)?,
            true,
        )?;

        let err = {
            let mut writer = ChannelWriter::new(&mut image);
            writer
                .set(&entry)
                .err()
                .ok_or("expected short image channel write to fail")?
        };
        assert_eq!(
            err,
            MemoryError::ChannelOutOfRange {
                channel: 10,
                max: RegularChannel::MAX,
            }
        );
        assert_eq!(image, before, "failed channel write mutated short image");
        Ok(())
    }
}
