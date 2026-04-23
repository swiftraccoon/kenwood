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
#[expect(
    clippy::cast_possible_truncation,
    reason = "`TOTAL_ENTRIES = 1200`, so `TOTAL_ENTRIES - 1 = 1199`. u16::MAX = 65535. The \
              const cast is lossless and evaluated at compile time."
)]
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
        self.image
            .get(offset)
            .copied()
            .is_some_and(|b| b != FLAG_EMPTY)
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
        self.image
            .get(offset..offset + NAME_ENTRY_SIZE)
            .map(programming::extract_name)
            .unwrap_or_default()
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
        let slice = self.image.get(offset..offset + FLAG_RECORD_SIZE)?;
        programming::parse_channel_flag(slice)
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
        let slice = self.image.get(offset..offset + CHANNEL_RECORD_SIZE)?;
        FlashChannel::from_bytes(slice).ok()
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
        self.image
            .get(offset..offset + NAME_ENTRY_SIZE)
            .map(programming::extract_name)
            .unwrap_or_default()
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
        let flag_bytes = self
            .image
            .get_mut(offset..offset + FLAG_RECORD_SIZE)
            .ok_or(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            })?;
        let [byte0, byte1, ..] = flag_bytes else {
            return Err(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            });
        };

        if used {
            // Preserve the existing band indicator if already set.
            // Transitioning from empty to used defaults to 0x00 (VHF).
            if *byte0 == FLAG_EMPTY {
                *byte0 = 0x00;
            }
        } else {
            *byte0 = FLAG_EMPTY;
        }

        // Byte 1: lockout in bit 0, preserve other bits.
        if lockout {
            *byte1 |= 0x01;
        } else {
            *byte1 &= !0x01;
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
        let dst = self
            .image
            .get_mut(offset..offset + CHANNEL_RECORD_SIZE)
            .ok_or(MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            })?;

        let bytes = memory.to_bytes();
        dst.copy_from_slice(&bytes);
        Ok(())
    }

    /// Write a channel display name (up to 16 bytes, null-padded).
    fn set_name(&mut self, number: u16, name: &str) -> Result<(), MemoryError> {
        let number_usize = number as usize;
        let offset = NAMES_OFFSET + number_usize * NAME_ENTRY_SIZE;
        let dst = self.image.get_mut(offset..offset + NAME_ENTRY_SIZE).ok_or(
            MemoryError::ChannelOutOfRange {
                channel: number,
                max: MAX_ENTRY_INDEX,
            },
        )?;

        let mut buf = [0u8; NAME_ENTRY_SIZE];
        // Zip is bounded by the shorter of buf (NAME_ENTRY_SIZE) and src — no indexing.
        buf.iter_mut()
            .zip(name.as_bytes().iter())
            .for_each(|(b, &s)| *b = s);
        dst.copy_from_slice(&buf);
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
        let dst = self
            .image
            .get_mut(offset..offset + NAME_ENTRY_SIZE)
            .ok_or_else(|| MemoryError::ChannelOutOfRange {
                channel: u16::from(group),
                max: 29,
            })?;

        let mut buf = [0u8; NAME_ENTRY_SIZE];
        buf.iter_mut()
            .zip(name.as_bytes().iter())
            .for_each(|(b, &s)| *b = s);
        dst.copy_from_slice(&buf);
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

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
            TOTAL_ENTRIES * NAME_ENTRY_SIZE,
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
        // URCALL: 24 bytes of zeros (empty callsign)
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
        assert!(ch.is_used(0));
        assert!(!ch.is_used(1));
        assert!(ch.is_used(5));
        Ok(())
    }

    #[test]
    fn channel_count() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.count(), 2); // channels 0 and 5
        Ok(())
    }

    #[test]
    fn channel_get_name() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.name(0), "2M CALL");
        assert_eq!(ch.name(5), "UHF CHAN");
        assert_eq!(ch.name(1), ""); // empty channel
        Ok(())
    }

    #[test]
    fn channel_get_entry() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();

        let entry0 = ch.get(0).ok_or("ch.get(0) returned None")?;
        assert!(entry0.used);
        assert!(!entry0.lockout);
        assert_eq!(entry0.name, "2M CALL");
        assert_eq!(entry0.flash.rx_frequency.as_hz(), 146_520_000);

        let entry5 = ch.get(5).ok_or("ch.get(5) returned None")?;
        assert!(entry5.used);
        assert!(entry5.lockout);
        assert_eq!(entry5.name, "UHF CHAN");
        assert_eq!(entry5.flash.rx_frequency.as_hz(), 446_000_000);
        Ok(())
    }

    #[test]
    fn channel_get_out_of_range() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert!(ch.get(1200).is_none());
        Ok(())
    }

    #[test]
    fn channel_all_returns_only_used() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        let all = ch.all();
        assert_eq!(all.len(), 2);
        assert_eq!(all.first().ok_or("all[0] missing")?.number, 0);
        assert_eq!(all.get(1).ok_or("all[1] missing")?.number, 5);
        Ok(())
    }

    #[test]
    fn channel_flag() -> TestResult {
        let image = make_test_image()?;
        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();

        let ch0_flag = ch.flag(0).ok_or("channel 0 flag missing")?;
        assert_eq!(ch0_flag.used, 0x00); // VHF
        assert!(!ch0_flag.lockout);
        assert_eq!(ch0_flag.group, 0);

        let ch5_flag = ch.flag(5).ok_or("channel 5 flag missing")?;
        assert_eq!(ch5_flag.used, 0x02); // UHF
        assert!(ch5_flag.lockout);
        assert_eq!(ch5_flag.group, 3);
        Ok(())
    }

    #[test]
    fn channel_group_names() -> TestResult {
        let mut image = make_test_image()?;
        // Write a group name at index 1152 (group 0).
        write_slice(&mut image, 0x10000 + 1152 * 16, b"Ham Radio\0\0\0\0\0\0\0")?;

        let mi = super::super::MemoryImage::from_raw(image)?;
        let ch = mi.channels();
        assert_eq!(ch.group_name(0), "Ham Radio");
        assert_eq!(ch.group_name(1), ""); // no name set
        Ok(())
    }

    #[test]
    fn channel_writer_set() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

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
            writer.set(&entry)?;
        }

        let ch = mi.channels();
        assert!(ch.is_used(10));
        let read_back = ch.get(10).ok_or("ch.get(10) returned None after write")?;
        assert!(read_back.used);
        assert_eq!(read_back.name, "TEST CH");
        assert_eq!(read_back.flash.rx_frequency.as_hz(), 145_000_000);
        Ok(())
    }

    #[test]
    fn channel_writer_group_name() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

        {
            let mut writer = ChannelWriter::new(mi.as_raw_mut());
            writer.set_group_name(0, "My Group")?;
        }

        let ch = mi.channels();
        assert_eq!(ch.group_name(0), "My Group");
        Ok(())
    }

    #[test]
    fn channel_writer_out_of_range() -> TestResult {
        let image = make_test_image()?;
        let mut mi = super::super::MemoryImage::from_raw(image)?;

        let entry = ChannelEntry {
            number: 1200,
            name: String::new(),
            flash: FlashChannel::default(),
            used: false,
            lockout: false,
        };

        let mut writer = ChannelWriter::new(mi.as_raw_mut());
        let result = writer.set(&entry);
        assert!(
            result.is_err(),
            "expected out-of-range set to fail: {result:?}"
        );
        Ok(())
    }
}
