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

use crate::protocol::programming::{
    self, CHANNEL_RECORD_SIZE, CHANNELS_PER_MEMGROUP, ChannelFlag, FLAG_EMPTY, FLAG_RECORD_SIZE,
    MEMGROUP_COUNT, NAME_ENTRY_SIZE, PAGE_SIZE,
};
use crate::sdcard::config::ChannelEntry;
use crate::types::channel::FlashChannel;

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

/// Maximum regular channel number (0-999).
const MAX_REGULAR_CHANNEL: u16 = 999;

/// Total channel entries including extended channels.
const TOTAL_ENTRIES: usize = programming::TOTAL_CHANNEL_ENTRIES; // 1200

/// Maximum channel index as u16 (1199). `TOTAL_ENTRIES` is 1200, which
/// always fits in u16, so this truncation is safe.
#[allow(clippy::cast_possible_truncation)]
const MAX_ENTRY_INDEX: u16 = (TOTAL_ENTRIES - 1) as u16;

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
    #[must_use]
    pub fn count(&self) -> usize {
        (0..=MAX_REGULAR_CHANNEL)
            .filter(|&ch| self.is_used(ch))
            .count()
    }

    /// Check if a channel slot is in use.
    ///
    /// Returns `false` for out-of-range channel numbers.
    #[must_use]
    pub fn is_used(&self, number: u16) -> bool {
        let number_usize = number as usize;
        if number_usize >= TOTAL_ENTRIES {
            return false;
        }
        let offset = FLAGS_OFFSET + number_usize * FLAG_RECORD_SIZE;
        if offset >= self.image.len() {
            return false;
        }
        self.image[offset] != FLAG_EMPTY
    }

    /// Get a specific channel by number.
    ///
    /// Returns `None` if the channel number is out of range or if the
    /// channel data cannot be read from the image.
    #[must_use]
    pub fn get(&self, number: u16) -> Option<ChannelEntry> {
        let number_usize = number as usize;
        if number_usize >= TOTAL_ENTRIES {
            return None;
        }

        let flag = self.flag(number)?;
        let used = flag.used != FLAG_EMPTY;
        let flash = self.flash(number)?;
        let name = self.name(number);

        Some(ChannelEntry {
            number,
            name,
            flash,
            used,
            lockout: flag.lockout,
        })
    }

    /// Get all populated regular channels (0-999).
    ///
    /// Skips empty channel slots. The returned entries are in channel
    /// number order.
    #[must_use]
    pub fn all(&self) -> Vec<ChannelEntry> {
        (0..=MAX_REGULAR_CHANNEL)
            .filter_map(|ch| {
                let entry = self.get(ch)?;
                if entry.used { Some(entry) } else { None }
            })
            .collect()
    }

    /// Get all channel entries (0-999), including empty slots.
    #[must_use]
    pub fn all_slots(&self) -> Vec<ChannelEntry> {
        (0..=MAX_REGULAR_CHANNEL)
            .filter_map(|ch| self.get(ch))
            .collect()
    }

    /// Get the display name for a channel.
    ///
    /// Returns an empty string for channels without a user-assigned name
    /// or for out-of-range channel numbers.
    #[must_use]
    pub fn name(&self, number: u16) -> String {
        let number_usize = number as usize;
        if number_usize >= TOTAL_ENTRIES {
            return String::new();
        }
        let offset = NAMES_OFFSET + number_usize * NAME_ENTRY_SIZE;
        if offset + NAME_ENTRY_SIZE > self.image.len() {
            return String::new();
        }
        programming::extract_name(&self.image[offset..offset + NAME_ENTRY_SIZE])
    }

    /// Get the channel flag (used/band, lockout, group) for a channel.
    ///
    /// Returns `None` for out-of-range channel numbers.
    #[must_use]
    pub fn flag(&self, number: u16) -> Option<ChannelFlag> {
        let number_usize = number as usize;
        if number_usize >= TOTAL_ENTRIES {
            return None;
        }
        let offset = FLAGS_OFFSET + number_usize * FLAG_RECORD_SIZE;
        if offset + FLAG_RECORD_SIZE > self.image.len() {
            return None;
        }
        programming::parse_channel_flag(&self.image[offset..])
    }

    /// Get the 40-byte flash channel record for a channel.
    ///
    /// Returns `None` for out-of-range channel numbers or if the data
    /// cannot be parsed. Uses the flash memory encoding ([`FlashChannel`])
    /// which includes all 8 operating modes and structured D-STAR fields.
    #[must_use]
    pub fn flash(&self, number: u16) -> Option<FlashChannel> {
        let number_usize = number as usize;
        if number_usize >= TOTAL_ENTRIES {
            return None;
        }

        // Channel data layout: memgroup = ch / 6, slot = ch % 6
        // byte_offset = 0x4000 + memgroup * 256 + slot * 40
        let memgroup = number_usize / CHANNELS_PER_MEMGROUP;
        let slot = number_usize % CHANNELS_PER_MEMGROUP;

        if memgroup >= MEMGROUP_COUNT {
            return None;
        }

        let offset = DATA_OFFSET + memgroup * PAGE_SIZE + slot * CHANNEL_RECORD_SIZE;
        if offset + CHANNEL_RECORD_SIZE > self.image.len() {
            return None;
        }
        FlashChannel::from_bytes(&self.image[offset..]).ok()
    }

    /// Get all channel names (0-999) as a vector of strings.
    ///
    /// Empty names are represented as empty strings.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        (0..=MAX_REGULAR_CHANNEL).map(|ch| self.name(ch)).collect()
    }

    /// Get a group name by group index (0-29).
    ///
    /// Group names are stored at name indices 1152-1181.
    #[must_use]
    pub fn group_name(&self, group: u8) -> String {
        if group >= 30 {
            return String::new();
        }
        let name_index = 1152 + group as usize;
        let offset = NAMES_OFFSET + name_index * NAME_ENTRY_SIZE;
        if offset + NAME_ENTRY_SIZE > self.image.len() {
            return String::new();
        }
        programming::extract_name(&self.image[offset..offset + NAME_ENTRY_SIZE])
    }

    /// Get all 30 group names.
    #[must_use]
    pub fn group_names(&self) -> Vec<String> {
        (0..30).map(|g| self.group_name(g)).collect()
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
    /// exceeds the maximum.
    pub fn set(&mut self, entry: &ChannelEntry) -> Result<(), MemoryError> {
        let number = entry.number as usize;
        if number >= TOTAL_ENTRIES {
            return Err(MemoryError::ChannelOutOfRange {
                channel: entry.number,
                max: MAX_ENTRY_INDEX,
            });
        }

        // Write flag.
        self.set_flag(entry.number, entry.used, entry.lockout)?;

        // Write flash channel data.
        self.set_flash(entry.number, &entry.flash)?;

        // Write name.
        self.set_name(entry.number, &entry.name)?;

        Ok(())
    }

    /// Write a channel flag.
    fn set_flag(&mut self, number: u16, used: bool, lockout: bool) -> Result<(), MemoryError> {
        let number_usize = number as usize;
        let offset = FLAGS_OFFSET + number_usize * FLAG_RECORD_SIZE;
        if offset + FLAG_RECORD_SIZE > self.image.len() {
            return Err(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            });
        }

        if used {
            // Preserve the existing band indicator (byte 0) if already set.
            // If transitioning from empty to used, default to 0x00 (VHF).
            if self.image[offset] == FLAG_EMPTY {
                self.image[offset] = 0x00; // VHF default
            }
        } else {
            self.image[offset] = FLAG_EMPTY;
        }

        // Byte 1: lockout in bit 0, preserve other bits.
        if lockout {
            self.image[offset + 1] |= 0x01;
        } else {
            self.image[offset + 1] &= !0x01;
        }

        Ok(())
    }

    /// Write the 40-byte flash channel record.
    fn set_flash(&mut self, number: u16, memory: &FlashChannel) -> Result<(), MemoryError> {
        let number_usize = number as usize;
        let memgroup = number_usize / CHANNELS_PER_MEMGROUP;
        let slot = number_usize % CHANNELS_PER_MEMGROUP;

        if memgroup >= MEMGROUP_COUNT {
            return Err(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            });
        }

        let offset = DATA_OFFSET + memgroup * PAGE_SIZE + slot * CHANNEL_RECORD_SIZE;
        if offset + CHANNEL_RECORD_SIZE > self.image.len() {
            return Err(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            });
        }

        let bytes = memory.to_bytes();
        self.image[offset..offset + CHANNEL_RECORD_SIZE].copy_from_slice(&bytes);
        Ok(())
    }

    /// Write a channel display name (up to 16 bytes, null-padded).
    fn set_name(&mut self, number: u16, name: &str) -> Result<(), MemoryError> {
        let number_usize = number as usize;
        let offset = NAMES_OFFSET + number_usize * NAME_ENTRY_SIZE;
        if offset + NAME_ENTRY_SIZE > self.image.len() {
            return Err(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            });
        }

        let mut buf = [0u8; NAME_ENTRY_SIZE];
        let src = name.as_bytes();
        let copy_len = src.len().min(NAME_ENTRY_SIZE);
        buf[..copy_len].copy_from_slice(&src[..copy_len]);
        self.image[offset..offset + NAME_ENTRY_SIZE].copy_from_slice(&buf);
        Ok(())
    }

    /// Write a group name (up to 16 bytes, null-padded).
    ///
    /// Group indices are 0-29.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::ChannelOutOfRange`] if the group index
    /// is out of range.
    pub fn set_group_name(&mut self, group: u8, name: &str) -> Result<(), MemoryError> {
        if group >= 30 {
            return Err(MemoryError::ChannelOutOfRange {
                channel: u16::from(group),
                max: 29,
            });
        }
        let name_index = 1152 + group as usize;
        let offset = NAMES_OFFSET + name_index * NAME_ENTRY_SIZE;
        if offset + NAME_ENTRY_SIZE > self.image.len() {
            return Err(MemoryError::ChannelOutOfRange {
                channel: u16::from(group),
                max: 29,
            });
        }

        let mut buf = [0u8; NAME_ENTRY_SIZE];
        let src = name.as_bytes();
        let copy_len = src.len().min(NAME_ENTRY_SIZE);
        buf[..copy_len].copy_from_slice(&src[..copy_len]);
        self.image[offset..offset + NAME_ENTRY_SIZE].copy_from_slice(&buf);
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
    use crate::types::Frequency;

    /// Create a test image with known channel data.
    fn make_test_image() -> Vec<u8> {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];

        // Zero out the names region (real radio uses null bytes for empty names).
        let names_end = NAMES_OFFSET + TOTAL_ENTRIES * NAME_ENTRY_SIZE;
        image[NAMES_OFFSET..names_end].fill(0x00);

        // Set up channel 0 as a used VHF channel.
        // Flag at 0x2000: [0x00 (VHF), 0x00 (no lockout), 0x00 (group 0), 0xFF]
        image[0x2000] = 0x00; // used = VHF
        image[0x2001] = 0x00; // no lockout
        image[0x2002] = 0x00; // group 0
        image[0x2003] = 0xFF;

        // Channel 0 data at memgroup 0, slot 0 = offset 0x4000.
        // Write a valid 40-byte channel record with 146.520 MHz.
        let freq: u32 = 146_520_000;
        let freq_bytes = freq.to_le_bytes();
        image[0x4000..0x4004].copy_from_slice(&freq_bytes);
        // TX offset = 0
        image[0x4004..0x4008].copy_from_slice(&[0, 0, 0, 0]);
        // Step size 0 (5 kHz) | shift 0 (simplex)
        image[0x4008] = 0x00;
        // Mode/flags byte 0x09: all zero (FM, no reverse, no tone, CTCSS off)
        image[0x4009] = 0x00;
        // Byte 0x0A: DCS off, etc.
        image[0x400A] = 0x00;
        // Tone/CTCSS/DCS indices
        image[0x400B] = 0x00;
        image[0x400C] = 0x00;
        image[0x400D] = 0x00;
        // Data speed / lockout
        image[0x400E] = 0x00;
        // URCALL: 24 bytes of zeros (empty callsign)
        // data_mode
        image[0x4027] = 0x00;

        // Channel 0 name at 0x10000: "2M CALL"
        let name = b"2M CALL\0\0\0\0\0\0\0\0\0";
        image[0x10000..0x10010].copy_from_slice(name);

        // Set up channel 1 as empty (default 0xFF in flags is already there).

        // Set up channel 5 as used UHF (to test crossing memgroup boundary
        // -- ch 5 is still in memgroup 0, slot 5).
        image[0x2000 + 5 * 4] = 0x02; // used = UHF
        image[0x2000 + 5 * 4 + 1] = 0x01; // lockout = yes
        image[0x2000 + 5 * 4 + 2] = 0x03; // group 3
        image[0x2000 + 5 * 4 + 3] = 0xFF;

        // Channel 5 data at memgroup 0, slot 5 = offset 0x4000 + 5 * 40 = 0x40C8.
        let freq5: u32 = 446_000_000;
        let freq5_bytes = freq5.to_le_bytes();
        image[0x40C8..0x40CC].copy_from_slice(&freq5_bytes);
        image[0x40CC..0x40D0].copy_from_slice(&[0, 0, 0, 0]);
        image[0x40D0] = 0x00;
        image[0x40D1] = 0x00;
        image[0x40D2] = 0x00;
        image[0x40D3] = 0x00;
        image[0x40D4] = 0x00;
        image[0x40D5] = 0x00;
        image[0x40D6] = 0x00;
        image[0x40EF] = 0x00;

        // Channel 5 name.
        let name5 = b"UHF CHAN\0\0\0\0\0\0\0\0";
        image[0x10000 + 5 * 16..0x10000 + 5 * 16 + 16].copy_from_slice(name5);

        image
    }

    #[test]
    fn from_raw_valid_size() {
        let image = vec![0u8; TOTAL_SIZE];
        assert!(super::super::MemoryImage::from_raw(image).is_ok());
    }

    #[test]
    fn from_raw_invalid_size() {
        let image = vec![0u8; 1000];
        let err = super::super::MemoryImage::from_raw(image).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidSize { .. }));
    }

    #[test]
    fn channel_is_used() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        assert!(ch.is_used(0));
        assert!(!ch.is_used(1));
        assert!(ch.is_used(5));
    }

    #[test]
    fn channel_count() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        assert_eq!(ch.count(), 2); // channels 0 and 5
    }

    #[test]
    fn channel_get_name() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        assert_eq!(ch.name(0), "2M CALL");
        assert_eq!(ch.name(5), "UHF CHAN");
        assert_eq!(ch.name(1), ""); // empty channel
    }

    #[test]
    fn channel_get_entry() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();

        let entry0 = ch.get(0).unwrap();
        assert!(entry0.used);
        assert!(!entry0.lockout);
        assert_eq!(entry0.name, "2M CALL");
        assert_eq!(entry0.flash.rx_frequency.as_hz(), 146_520_000);

        let entry5 = ch.get(5).unwrap();
        assert!(entry5.used);
        assert!(entry5.lockout);
        assert_eq!(entry5.name, "UHF CHAN");
        assert_eq!(entry5.flash.rx_frequency.as_hz(), 446_000_000);
    }

    #[test]
    fn channel_get_out_of_range() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        assert!(ch.get(1200).is_none());
    }

    #[test]
    fn channel_all_returns_only_used() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        let all = ch.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].number, 0);
        assert_eq!(all[1].number, 5);
    }

    #[test]
    fn channel_flag() {
        let image = make_test_image();
        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();

        let flag0 = ch.flag(0).unwrap();
        assert_eq!(flag0.used, 0x00); // VHF
        assert!(!flag0.lockout);
        assert_eq!(flag0.group, 0);

        let flag5 = ch.flag(5).unwrap();
        assert_eq!(flag5.used, 0x02); // UHF
        assert!(flag5.lockout);
        assert_eq!(flag5.group, 3);
    }

    #[test]
    fn channel_group_names() {
        let mut image = make_test_image();
        // Write a group name at index 1152 (group 0).
        let name = b"Ham Radio\0\0\0\0\0\0\0";
        let offset = 0x10000 + 1152 * 16;
        image[offset..offset + 16].copy_from_slice(name);

        let mi = super::super::MemoryImage::from_raw(image).unwrap();
        let ch = mi.channels();
        assert_eq!(ch.group_name(0), "Ham Radio");
        assert_eq!(ch.group_name(1), ""); // no name set
    }

    #[test]
    fn channel_writer_set() {
        let image = make_test_image();
        let mut mi = super::super::MemoryImage::from_raw(image).unwrap();

        let entry = ChannelEntry {
            number: 10,
            name: "TEST CH".to_owned(),
            flash: FlashChannel {
                rx_frequency: Frequency::new(145_000_000),
                ..FlashChannel::default()
            },
            used: true,
            lockout: false,
        };

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set(&entry).unwrap();
        }

        let ch = mi.channels();
        assert!(ch.is_used(10));
        let read_back = ch.get(10).unwrap();
        assert!(read_back.used);
        assert_eq!(read_back.name, "TEST CH");
        assert_eq!(read_back.flash.rx_frequency.as_hz(), 145_000_000);
    }

    #[test]
    fn channel_writer_group_name() {
        let image = make_test_image();
        let mut mi = super::super::MemoryImage::from_raw(image).unwrap();

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set_group_name(0, "My Group").unwrap();
        }

        let ch = mi.channels();
        assert_eq!(ch.group_name(0), "My Group");
    }

    #[test]
    fn channel_writer_out_of_range() {
        let image = make_test_image();
        let mut mi = super::super::MemoryImage::from_raw(image).unwrap();

        let entry = ChannelEntry {
            number: 1200,
            name: String::new(),
            flash: FlashChannel::default(),
            used: false,
            lockout: false,
        };

        let mut writer = ChannelWriter::new(mi.as_raw_mut());
        let result = writer.set(&entry);
        assert!(result.is_err());
    }
}
