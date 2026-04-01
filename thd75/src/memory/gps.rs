//! Typed access to the GPS configuration region of the memory image.
//!
//! The GPS configuration is estimated to occupy ~4,096 bytes around
//! byte offset `0x19000` in the MCP address space. This includes GPS
//! receiver settings, position memory slots, track log configuration,
//! and NMEA output selection.
//!
//! # Offset confidence
//!
//! The GPS region boundaries are estimated from the overall memory
//! layout analysis. No GPS offsets have been confirmed via differential
//! dump on a D75. All typed accessors in this module are marked with
//! `# Verification` in their doc comments.

use crate::types::gps::{GpsOperatingMode, GpsPositionAmbiguity};

/// Estimated byte offset of the GPS configuration region.
///
/// This is an estimate based on the overall memory layout analysis.
/// The actual start may differ by a few pages.
const GPS_ESTIMATED_OFFSET: usize = 0x19000;

/// Estimated size of the GPS configuration region.
const GPS_ESTIMATED_SIZE: usize = 0x1000; // 4 KB

// ---------------------------------------------------------------------------
// Estimated field offsets within the GPS region
//
// These offsets are relative to GPS_ESTIMATED_OFFSET and are rough
// estimates based on menu ordering and typical Kenwood layout patterns.
// None have been verified on hardware.
// ---------------------------------------------------------------------------

/// Estimated offset for GPS enabled (1 byte, 0 = off, 1 = on).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_ENABLED_REL: usize = 0x00;

/// Estimated offset for GPS PC output (1 byte, 0 = off, 1 = on).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_PC_OUTPUT_REL: usize = 0x01;

/// Estimated offset for GPS operating mode (1 byte, enum index).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_OPERATING_MODE_REL: usize = 0x02;

/// Estimated offset for GPS battery saver (1 byte, 0 = off, 1 = on).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_BATTERY_SAVER_REL: usize = 0x03;

/// Estimated offset for position ambiguity (1 byte, 0-4).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_POSITION_AMBIGUITY_REL: usize = 0x04;

/// Estimated offset for NMEA sentence flags (1 byte, bit field:
/// bit 0 = GGA, bit 1 = GLL, bit 2 = GSA, bit 3 = GSV,
/// bit 4 = RMC, bit 5 = VTG).
/// Relative to `GPS_ESTIMATED_OFFSET`.
const GPS_NMEA_FLAGS_REL: usize = 0x05;

// ---------------------------------------------------------------------------
// GPS channel index
//
// The GPS channel index at byte offset 0x4D000 contains 100 entries of
// 1 byte each. A value of 0xFF indicates an unused slot; other values
// are indices into the waypoint data area.
//
// Waypoint data for entry with index value V is located at:
//   (V + 0x2608) * 0x20
// Each waypoint record is 0x20 (32) bytes.
// ---------------------------------------------------------------------------

/// Byte offset of the GPS channel index (100 x 1 byte).
const GPS_CHANNEL_INDEX_OFFSET: usize = 0x4_D000;

/// Number of GPS channel index entries.
const GPS_CHANNEL_INDEX_COUNT: usize = 100;

/// Marker value for unused GPS channel index entries.
const GPS_INDEX_UNUSED: u8 = 0xFF;

/// Base offset for waypoint data address calculation.
///
/// Waypoint data address = `(index_value + GPS_WAYPOINT_BASE_INDEX) * GPS_WAYPOINT_RECORD_SIZE`.
const GPS_WAYPOINT_BASE_INDEX: usize = 0x2608;

/// Size of a single GPS waypoint record in bytes.
const GPS_WAYPOINT_RECORD_SIZE: usize = 0x20;

// ---------------------------------------------------------------------------
// GpsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the GPS configuration region.
///
/// Provides raw byte access and typed field accessors for the estimated
/// GPS settings region. All offsets are estimates and need verification
/// via differential memory dumps.
///
/// # Known settings (from menu analysis, offsets estimated)
///
/// - Built-in GPS on/off
/// - My Position (5 manual slots, each with lat/lon/alt)
/// - Position ambiguity setting
/// - GPS operating mode (standalone/SBAS)
/// - PC output format (NMEA sentences enabled/disabled)
/// - Track log settings (record method, interval, distance)
/// - GPS data TX settings (auto TX, interval)
#[derive(Debug)]
pub struct GpsAccess<'a> {
    image: &'a [u8],
}

impl<'a> GpsAccess<'a> {
    /// Create a new GPS accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the raw bytes at the estimated GPS region.
    ///
    /// Returns the bytes at offset `0x19000` through `0x19FFF`. These
    /// boundaries are estimates and may not perfectly align with the
    /// actual GPS configuration data.
    #[must_use]
    pub fn estimated_region(&self) -> Option<&[u8]> {
        let end = GPS_ESTIMATED_OFFSET + GPS_ESTIMATED_SIZE;
        if end <= self.image.len() {
            Some(&self.image[GPS_ESTIMATED_OFFSET..end])
        } else {
            None
        }
    }

    /// Read an arbitrary byte range from the image.
    ///
    /// The offset is an absolute MCP byte address. Returns `None` if
    /// the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        if end <= self.image.len() {
            Some(&self.image[offset..end])
        } else {
            None
        }
    }

    /// Get the estimated size of the GPS region in bytes.
    #[must_use]
    pub const fn estimated_region_size(&self) -> usize {
        GPS_ESTIMATED_SIZE
    }

    // -----------------------------------------------------------------------
    // Typed GPS accessors (estimated offsets)
    // -----------------------------------------------------------------------

    /// Read GPS enabled setting.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19000` (first byte of the GPS region).
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn gps_enabled(&self) -> bool {
        self.image
            .get(GPS_ESTIMATED_OFFSET + GPS_ENABLED_REL)
            .is_some_and(|&b| b != 0)
    }

    /// Read GPS PC output setting.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19001`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn pc_output(&self) -> bool {
        self.image
            .get(GPS_ESTIMATED_OFFSET + GPS_PC_OUTPUT_REL)
            .is_some_and(|&b| b != 0)
    }

    /// Read GPS operating mode.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19002`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn operating_mode(&self) -> GpsOperatingMode {
        match self
            .image
            .get(GPS_ESTIMATED_OFFSET + GPS_OPERATING_MODE_REL)
            .copied()
            .unwrap_or(0)
        {
            1 => GpsOperatingMode::Sbas,
            2 => GpsOperatingMode::Manual,
            _ => GpsOperatingMode::Standalone,
        }
    }

    /// Read GPS battery saver setting.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19003`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn battery_saver(&self) -> bool {
        self.image
            .get(GPS_ESTIMATED_OFFSET + GPS_BATTERY_SAVER_REL)
            .is_some_and(|&b| b != 0)
    }

    /// Read GPS position ambiguity level.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19004`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn position_ambiguity(&self) -> GpsPositionAmbiguity {
        match self
            .image
            .get(GPS_ESTIMATED_OFFSET + GPS_POSITION_AMBIGUITY_REL)
            .copied()
            .unwrap_or(0)
        {
            1 => GpsPositionAmbiguity::Level1,
            2 => GpsPositionAmbiguity::Level2,
            3 => GpsPositionAmbiguity::Level3,
            4 => GpsPositionAmbiguity::Level4,
            _ => GpsPositionAmbiguity::Full,
        }
    }

    /// Read NMEA sentence output flags as a raw byte.
    ///
    /// Bit field: bit 0 = GGA, bit 1 = GLL, bit 2 = GSA, bit 3 = GSV,
    /// bit 4 = RMC, bit 5 = VTG. Returns `0x3F` (all enabled) if
    /// unreadable.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19005`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn nmea_sentence_flags(&self) -> u8 {
        self.image
            .get(GPS_ESTIMATED_OFFSET + GPS_NMEA_FLAGS_REL)
            .copied()
            .unwrap_or(0x3F)
    }

    /// Check if a specific NMEA sentence is enabled.
    ///
    /// `bit` selects the sentence: 0 = GGA, 1 = GLL, 2 = GSA,
    /// 3 = GSV, 4 = RMC, 5 = VTG.
    ///
    /// # Offset
    ///
    /// Estimated at `0x19005`.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn nmea_sentence_enabled(&self, bit: u8) -> bool {
        if bit > 5 {
            return false;
        }
        (self.nmea_sentence_flags() >> bit) & 1 != 0
    }

    // -----------------------------------------------------------------------
    // GPS channel index accessors
    // -----------------------------------------------------------------------

    /// Get the raw GPS channel index (100 bytes at `0x4D000`).
    ///
    /// Each byte is either `0xFF` (unused) or an index into the waypoint
    /// data area.
    ///
    /// Returns `None` if the region extends past the image.
    #[must_use]
    pub fn channel_index_raw(&self) -> Option<&[u8]> {
        let end = GPS_CHANNEL_INDEX_OFFSET + GPS_CHANNEL_INDEX_COUNT;
        if end <= self.image.len() {
            Some(&self.image[GPS_CHANNEL_INDEX_OFFSET..end])
        } else {
            None
        }
    }

    /// Get the GPS channel index value for a given slot (0-99).
    ///
    /// Returns `None` if the slot is unused (`0xFF`) or out of range.
    /// Otherwise returns the waypoint data index.
    #[must_use]
    pub fn channel_index(&self, slot: u8) -> Option<u8> {
        let slot_usize = slot as usize;
        if slot_usize >= GPS_CHANNEL_INDEX_COUNT {
            return None;
        }
        let offset = GPS_CHANNEL_INDEX_OFFSET + slot_usize;
        let value = self.image.get(offset).copied()?;
        if value == GPS_INDEX_UNUSED {
            None
        } else {
            Some(value)
        }
    }

    /// Count the number of active (non-empty) GPS waypoint slots.
    ///
    /// Iterates the 100-entry GPS channel index and counts entries that
    /// are not `0xFF`.
    #[must_use]
    pub fn waypoint_count(&self) -> usize {
        (0..GPS_CHANNEL_INDEX_COUNT)
            .filter(|&i| {
                let offset = GPS_CHANNEL_INDEX_OFFSET + i;
                self.image
                    .get(offset)
                    .is_some_and(|&b| b != GPS_INDEX_UNUSED)
            })
            .count()
    }

    /// Get the raw waypoint record for a given channel index slot (0-99).
    ///
    /// Looks up the waypoint data index from the GPS channel index, then
    /// reads the 32-byte waypoint record at the calculated address:
    /// `(index_value + 0x2608) * 0x20`.
    ///
    /// Returns `None` if the slot is unused, out of range, or the record
    /// extends past the image.
    #[must_use]
    pub fn waypoint_raw(&self, slot: u8) -> Option<&[u8]> {
        let index_value = self.channel_index(slot)? as usize;
        let data_offset = (index_value + GPS_WAYPOINT_BASE_INDEX) * GPS_WAYPOINT_RECORD_SIZE;
        let end = data_offset + GPS_WAYPOINT_RECORD_SIZE;
        if end <= self.image.len() {
            Some(&self.image[data_offset..end])
        } else {
            None
        }
    }

    /// Read the name field from a GPS waypoint record (up to 8 characters).
    ///
    /// Returns an empty string if the slot is unused or the record cannot
    /// be read. The name is at offset 0x10 within the 32-byte record,
    /// 9 bytes (8 characters + null terminator). A first byte of `0xFE`
    /// indicates an unused name.
    #[must_use]
    pub fn waypoint_name(&self, slot: u8) -> String {
        let Some(record) = self.waypoint_raw(slot) else {
            return String::new();
        };

        // Name is at record offset 0x10, 9 bytes.
        if record.len() < 0x19 {
            return String::new();
        }
        let name_bytes = &record[0x10..0x19];
        // 0xFE in the first byte means unused.
        if name_bytes[0] == 0xFE {
            return String::new();
        }
        let nul = name_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_bytes.len());
        String::from_utf8_lossy(&name_bytes[..nul])
            .trim()
            .to_owned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;
    use crate::types::gps::{GpsOperatingMode, GpsPositionAmbiguity};

    fn make_gps_image() -> Vec<u8> {
        vec![0u8; TOTAL_SIZE]
    }

    #[test]
    fn gps_estimated_region_accessible() {
        let image = vec![0xBB_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let gps = mi.gps();
        let region = gps.estimated_region().unwrap();
        assert_eq!(region.len(), GPS_ESTIMATED_SIZE);
        assert!(region.iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn gps_read_bytes() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET..GPS_ESTIMATED_OFFSET + 4]
            .copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let gps = mi.gps();
        let bytes = gps.read_bytes(GPS_ESTIMATED_OFFSET, 4).unwrap();
        assert_eq!(bytes, &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn gps_region_size() {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let gps = mi.gps();
        assert_eq!(gps.estimated_region_size(), 0x1000);
    }

    #[test]
    fn gps_enabled() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_ENABLED_REL] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().gps_enabled());
    }

    #[test]
    fn gps_enabled_off() {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.gps().gps_enabled());
    }

    #[test]
    fn gps_pc_output() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_PC_OUTPUT_REL] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().pc_output());
    }

    #[test]
    fn gps_operating_mode() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_OPERATING_MODE_REL] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::Sbas);
    }

    #[test]
    fn gps_operating_mode_manual() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_OPERATING_MODE_REL] = 2;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::Manual);
    }

    #[test]
    fn gps_operating_mode_default() {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::Standalone);
    }

    #[test]
    fn gps_battery_saver() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_BATTERY_SAVER_REL] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().battery_saver());
    }

    #[test]
    fn gps_position_ambiguity() {
        let mut image = make_gps_image();
        image[GPS_ESTIMATED_OFFSET + GPS_POSITION_AMBIGUITY_REL] = 3;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().position_ambiguity(), GpsPositionAmbiguity::Level3);
    }

    #[test]
    fn gps_position_ambiguity_default() {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().position_ambiguity(), GpsPositionAmbiguity::Full);
    }

    #[test]
    fn gps_nmea_flags() {
        let mut image = make_gps_image();
        // Enable GGA (bit 0) and RMC (bit 4) = 0b00010001 = 0x11.
        image[GPS_ESTIMATED_OFFSET + GPS_NMEA_FLAGS_REL] = 0x11;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let gps = mi.gps();
        assert_eq!(gps.nmea_sentence_flags(), 0x11);
        assert!(gps.nmea_sentence_enabled(0)); // GGA
        assert!(!gps.nmea_sentence_enabled(1)); // GLL
        assert!(!gps.nmea_sentence_enabled(2)); // GSA
        assert!(!gps.nmea_sentence_enabled(3)); // GSV
        assert!(gps.nmea_sentence_enabled(4)); // RMC
        assert!(!gps.nmea_sentence_enabled(5)); // VTG
    }

    #[test]
    fn gps_nmea_sentence_out_of_range() {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.gps().nmea_sentence_enabled(6));
        assert!(!mi.gps().nmea_sentence_enabled(255));
    }

    // -----------------------------------------------------------------------
    // GPS channel index tests
    // -----------------------------------------------------------------------

    #[test]
    fn gps_channel_index_raw_accessible() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let gps = mi.gps();
        let index = gps.channel_index_raw().unwrap();
        assert_eq!(index.len(), GPS_CHANNEL_INDEX_COUNT);
        // All 0xFF = unused.
        assert!(index.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn gps_channel_index_unused() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().channel_index(0).is_none());
        assert!(mi.gps().channel_index(99).is_none());
    }

    #[test]
    fn gps_channel_index_out_of_range() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().channel_index(100).is_none());
        assert!(mi.gps().channel_index(255).is_none());
    }

    #[test]
    fn gps_channel_index_populated() {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        // Set slot 0 to waypoint index 5.
        image[GPS_CHANNEL_INDEX_OFFSET] = 5;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().channel_index(0), Some(5));
    }

    #[test]
    fn gps_waypoint_count_all_empty() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().waypoint_count(), 0);
    }

    #[test]
    fn gps_waypoint_count_with_entries() {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        // Set 3 slots as used.
        image[GPS_CHANNEL_INDEX_OFFSET] = 0;
        image[GPS_CHANNEL_INDEX_OFFSET + 1] = 1;
        image[GPS_CHANNEL_INDEX_OFFSET + 50] = 10;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().waypoint_count(), 3);
    }

    #[test]
    fn gps_waypoint_raw_empty_slot() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.gps().waypoint_raw(0).is_none());
    }

    #[test]
    fn gps_waypoint_raw_populated() {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        image[GPS_CHANNEL_INDEX_OFFSET] = 0; // Waypoint index 0.
        // Waypoint data at (0 + 0x2608) * 0x20 = 0x4C100.
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            image[wp_offset..wp_offset + 4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
            let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
            let gps = mi.gps();
            let raw = gps.waypoint_raw(0).unwrap();
            assert_eq!(raw.len(), GPS_WAYPOINT_RECORD_SIZE);
            assert_eq!(&raw[..4], &[0x01, 0x02, 0x03, 0x04]);
        }
    }

    #[test]
    fn gps_waypoint_name() {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        image[GPS_CHANNEL_INDEX_OFFSET] = 0;
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            // Write name at waypoint record offset 0x10.
            let name = b"HOME\0\0\0\0\0";
            image[wp_offset + 0x10..wp_offset + 0x19].copy_from_slice(name);
            let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
            assert_eq!(mi.gps().waypoint_name(0), "HOME");
        }
    }

    #[test]
    fn gps_waypoint_name_empty_slot() {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.gps().waypoint_name(0), "");
    }

    #[test]
    fn gps_waypoint_name_unused_marker() {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        image[GPS_CHANNEL_INDEX_OFFSET] = 0;
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            // 0xFE as first byte of name = unused.
            image[wp_offset + 0x10] = 0xFE;
            let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
            assert_eq!(mi.gps().waypoint_name(0), "");
        }
    }
}
