//! Typed access to the APRS configuration region of the memory image.
//!
//! The APRS configuration occupies pages `0x0151`+ in the MCP address
//! space. This includes the APRS message status header (256 bytes at
//! page `0x0151`), followed by APRS messages, settings, and extended
//! configuration data.
//!
//! # Offset confidence
//!
//! The APRS region boundaries (page `0x0151` for the status header,
//! page `0x0152` for the data region) are confirmed from D74 development
//! notes. Individual field offsets within the data region are estimated
//! and marked with `# Verification` in the doc comments.

use crate::protocol::programming;
use crate::types::aprs::AprsCallsign;

/// Byte offset of the APRS message status header.
const APRS_STATUS_OFFSET: usize = programming::APRS_STATUS_PAGE as usize * programming::PAGE_SIZE;

/// Byte offset of the APRS messages and settings region.
const APRS_DATA_OFFSET: usize = programming::APRS_START as usize * programming::PAGE_SIZE;

/// Estimated end of the APRS region (before D-STAR repeater list).
const APRS_END_OFFSET: usize = programming::DSTAR_RPT_START as usize * programming::PAGE_SIZE;

// ---------------------------------------------------------------------------
// Estimated field offsets within the APRS data region
//
// The APRS data region starts at 0x15200.  Field offsets below are
// relative to that base and are estimated from D74 layout conventions.
// None of these offsets have been hardware-verified on a D75 yet.
// ---------------------------------------------------------------------------

/// Estimated offset of the APRS MY callsign (10 bytes, null-terminated
/// ASCII including SSID, e.g. "N0CALL-9\0").
///
/// Relative to the start of the APRS data region (`0x15200`).
const APRS_MY_CALLSIGN_REL: usize = 0x0000;

/// Maximum callsign field length including null terminator.
const APRS_CALLSIGN_FIELD_LEN: usize = 10;

/// Estimated offset of the beacon interval (2 bytes, little-endian,
/// value in seconds).
///
/// Relative to the start of the APRS data region (`0x15200`).
const APRS_BEACON_INTERVAL_REL: usize = 0x000A;

/// Estimated offset of the packet path selection (1 byte, enum index).
///
/// Relative to the start of the APRS data region (`0x15200`).
const APRS_PACKET_PATH_REL: usize = 0x000C;

// ---------------------------------------------------------------------------
// APRS/GPS position data region
//
// The APRS/GPS position data occupies 0x4B00 bytes (19,200 bytes) starting
// at byte offset 0x25100 in the MCP memory image.
// ---------------------------------------------------------------------------

/// Byte offset of the APRS/GPS position data region.
///
/// 0x4B00 bytes of APRS/GPS position data starting at offset 0x25100.
const APRS_POSITION_DATA_OFFSET: usize = 0x2_5100;

/// Size of the APRS/GPS position data region in bytes.
const APRS_POSITION_DATA_SIZE: usize = 0x4B00;

// ---------------------------------------------------------------------------
// AprsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the APRS configuration region.
///
/// Provides raw byte access and typed field accessors for the APRS
/// settings region at pages `0x0151`+. The region boundaries are
/// confirmed from D74 development notes; individual field offsets within
/// the data region are estimated.
///
/// # Known sub-regions
///
/// | MCP Offset | Content |
/// |-----------|---------|
/// | `0x15100` | APRS message status header (256 bytes) |
/// | `0x15200` | APRS messages and settings (~16 KB) |
/// | ~`0x19000` | APRS extended config / GPS settings |
#[derive(Debug)]
pub struct AprsAccess<'a> {
    image: &'a [u8],
}

impl<'a> AprsAccess<'a> {
    /// Create a new APRS accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the raw APRS message status header (256 bytes at page `0x0151`).
    ///
    /// Contains metadata for APRS messages: count, read/unread flags,
    /// index pointers.
    #[must_use]
    pub fn status_header(&self) -> Option<&[u8]> {
        let end = APRS_STATUS_OFFSET + programming::PAGE_SIZE;
        if end <= self.image.len() {
            Some(&self.image[APRS_STATUS_OFFSET..end])
        } else {
            None
        }
    }

    /// Get the raw APRS data region (pages `0x0152` through the start of
    /// the D-STAR region).
    ///
    /// Contains APRS messages, callsign, status texts, packet path,
    /// `SmartBeaconing` parameters, digipeater config, and more.
    #[must_use]
    pub fn data_region(&self) -> Option<&[u8]> {
        if APRS_END_OFFSET <= self.image.len() {
            Some(&self.image[APRS_DATA_OFFSET..APRS_END_OFFSET])
        } else {
            None
        }
    }

    /// Read an arbitrary byte range from the APRS region.
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

    /// Get the total size of the APRS region in bytes.
    #[must_use]
    pub const fn region_size(&self) -> usize {
        APRS_END_OFFSET - APRS_STATUS_OFFSET
    }

    // -----------------------------------------------------------------------
    // Typed APRS accessors (estimated offsets)
    // -----------------------------------------------------------------------

    /// Read the APRS MY callsign (station callsign with optional SSID).
    ///
    /// Returns the callsign as a string (up to 9 characters, e.g.
    /// "N0CALL-9"). Returns an empty string if unreadable.
    ///
    /// # Offset
    ///
    /// Estimated at `0x15200` (first bytes of the APRS data region)
    /// based on D74 layout analysis.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn my_callsign(&self) -> String {
        let offset = APRS_DATA_OFFSET + APRS_MY_CALLSIGN_REL;
        let end = offset + APRS_CALLSIGN_FIELD_LEN;
        if end > self.image.len() {
            return String::new();
        }
        let slice = &self.image[offset..end];
        let nul = slice
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(APRS_CALLSIGN_FIELD_LEN);
        let s = std::str::from_utf8(&slice[..nul]).unwrap_or("").trim();
        s.to_owned()
    }

    /// Read the APRS MY callsign as a typed [`AprsCallsign`].
    ///
    /// Returns `None` if the callsign is empty or too long.
    ///
    /// # Offset
    ///
    /// Estimated at `0x15200` (first bytes of the APRS data region).
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn my_callsign_typed(&self) -> Option<AprsCallsign> {
        let raw = self.my_callsign();
        if raw.is_empty() {
            return None;
        }
        AprsCallsign::new(&raw)
    }

    /// Read the beacon interval in seconds.
    ///
    /// Returns the interval as a 16-bit value (range 30-9999 in normal
    /// operation). Returns 0 if unreadable.
    ///
    /// # Offset
    ///
    /// Estimated at `0x1520A` (APRS data region + 0x0A) based on D74 layout analysis.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn beacon_interval(&self) -> u16 {
        let offset = APRS_DATA_OFFSET + APRS_BEACON_INTERVAL_REL;
        if offset + 2 > self.image.len() {
            return 0;
        }
        u16::from_le_bytes([self.image[offset], self.image[offset + 1]])
    }

    /// Read the packet path selection index.
    ///
    /// Returns a raw index value (0 = Off, 1 = WIDE1-1, 2 = WIDE1-1
    /// WIDE2-1, etc.). Returns 0 if unreadable.
    ///
    /// # Offset
    ///
    /// Estimated at `0x1520C` (APRS data region + 0x0C) based on D74 layout analysis.
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn packet_path_index(&self) -> u8 {
        let offset = APRS_DATA_OFFSET + APRS_PACKET_PATH_REL;
        self.image.get(offset).copied().unwrap_or(0)
    }

    /// Read the packet path as a display string.
    ///
    /// Translates the raw index into a human-readable path string.
    ///
    /// # Offset
    ///
    /// Estimated at `0x1520C` (APRS data region + 0x0C).
    ///
    /// # Verification
    ///
    /// Offset is estimated, not hardware-verified.
    #[must_use]
    pub fn packet_path(&self) -> String {
        match self.packet_path_index() {
            0 => "Off".to_owned(),
            1 => "WIDE1-1".to_owned(),
            2 => "WIDE1-1,WIDE2-1".to_owned(),
            3 => "WIDE1-1,WIDE2-2".to_owned(),
            4 => "User 1".to_owned(),
            5 => "User 2".to_owned(),
            6 => "User 3".to_owned(),
            _ => "Unknown".to_owned(),
        }
    }

    // -----------------------------------------------------------------------
    // APRS/GPS position data region (confirmed address)
    // -----------------------------------------------------------------------

    /// Get the raw APRS/GPS position data region (0x4B00 bytes at `0x25100`).
    ///
    /// This region contains APRS position data, stored object data, and
    /// GPS-related configuration.
    ///
    /// Returns `None` if the region extends past the image.
    #[must_use]
    pub fn position_data_region(&self) -> Option<&[u8]> {
        let end = APRS_POSITION_DATA_OFFSET + APRS_POSITION_DATA_SIZE;
        if end <= self.image.len() {
            Some(&self.image[APRS_POSITION_DATA_OFFSET..end])
        } else {
            None
        }
    }

    /// Get the total size of the APRS/GPS position data region in bytes.
    ///
    /// Always returns 0x4B00 (19,200 bytes).
    #[must_use]
    pub const fn position_data_size(&self) -> usize {
        APRS_POSITION_DATA_SIZE
    }

    /// Read a byte range from the APRS/GPS position data region.
    ///
    /// The `rel_offset` is relative to the start of the position data
    /// region (`0x25100`). Returns `None` if the range extends past the
    /// region or the image.
    #[must_use]
    pub fn position_data_bytes(&self, rel_offset: usize, len: usize) -> Option<&[u8]> {
        if rel_offset + len > APRS_POSITION_DATA_SIZE {
            return None;
        }
        let abs_offset = APRS_POSITION_DATA_OFFSET + rel_offset;
        let end = abs_offset + len;
        if end <= self.image.len() {
            Some(&self.image[abs_offset..end])
        } else {
            None
        }
    }

    /// Check if the APRS/GPS position data region contains any non-zero data.
    ///
    /// Returns `true` if any byte in the region is non-zero, indicating
    /// that position data has been stored.
    #[must_use]
    pub fn has_position_data(&self) -> bool {
        self.position_data_region()
            .is_some_and(|data| data.iter().any(|&b| b != 0x00 && b != 0xFF))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;

    fn make_aprs_image() -> Vec<u8> {
        vec![0u8; TOTAL_SIZE]
    }

    #[test]
    fn aprs_status_header_accessible() {
        let image = vec![0xAA_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        let header = aprs.status_header().unwrap();
        assert_eq!(header.len(), programming::PAGE_SIZE);
        assert!(header.iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn aprs_data_region_accessible() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        let data = aprs.data_region().unwrap();
        assert!(!data.is_empty());
        // Region should span from APRS_DATA_OFFSET to APRS_END_OFFSET.
        let expected_size = APRS_END_OFFSET - APRS_DATA_OFFSET;
        assert_eq!(data.len(), expected_size);
    }

    #[test]
    fn aprs_region_size() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        // Region should be non-trivial (several KB).
        assert!(aprs.region_size() > 1000);
    }

    #[test]
    fn aprs_my_callsign() {
        let mut image = make_aprs_image();
        let offset = APRS_DATA_OFFSET + APRS_MY_CALLSIGN_REL;
        let cs = b"N0CALL-9\0\0";
        image[offset..offset + 10].copy_from_slice(cs);

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        assert_eq!(aprs.my_callsign(), "N0CALL-9");
    }

    #[test]
    fn aprs_my_callsign_typed() {
        let mut image = make_aprs_image();
        let offset = APRS_DATA_OFFSET + APRS_MY_CALLSIGN_REL;
        let cs = b"W1AW-7\0\0\0\0";
        image[offset..offset + 10].copy_from_slice(cs);

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        let typed = aprs.my_callsign_typed().unwrap();
        assert_eq!(typed.as_str(), "W1AW-7");
    }

    #[test]
    fn aprs_my_callsign_empty() {
        let image = make_aprs_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        assert_eq!(aprs.my_callsign(), "");
        assert!(aprs.my_callsign_typed().is_none());
    }

    #[test]
    fn aprs_beacon_interval() {
        let mut image = make_aprs_image();
        let offset = APRS_DATA_OFFSET + APRS_BEACON_INTERVAL_REL;
        // 180 seconds = 0x00B4 little-endian
        image[offset] = 0xB4;
        image[offset + 1] = 0x00;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        assert_eq!(aprs.beacon_interval(), 180);
    }

    #[test]
    fn aprs_beacon_interval_zero() {
        let image = make_aprs_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.aprs().beacon_interval(), 0);
    }

    #[test]
    fn aprs_packet_path() {
        let mut image = make_aprs_image();
        let offset = APRS_DATA_OFFSET + APRS_PACKET_PATH_REL;
        image[offset] = 2; // WIDE1-1,WIDE2-1

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        assert_eq!(aprs.packet_path_index(), 2);
        assert_eq!(aprs.packet_path(), "WIDE1-1,WIDE2-1");
    }

    #[test]
    fn aprs_packet_path_off() {
        let image = make_aprs_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.aprs().packet_path(), "Off");
    }

    #[test]
    fn aprs_packet_path_unknown() {
        let mut image = make_aprs_image();
        let offset = APRS_DATA_OFFSET + APRS_PACKET_PATH_REL;
        image[offset] = 0xFF;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.aprs().packet_path(), "Unknown");
    }

    // -----------------------------------------------------------------------
    // APRS/GPS position data region tests (confirmed address)
    // -----------------------------------------------------------------------

    #[test]
    fn aprs_position_data_region_accessible() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        let region = aprs.position_data_region().unwrap();
        assert_eq!(region.len(), APRS_POSITION_DATA_SIZE);
    }

    #[test]
    fn aprs_position_data_size() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.aprs().position_data_size(), 0x4B00);
    }

    #[test]
    fn aprs_position_data_bytes() {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Write known data at the start of the position data region.
        image[APRS_POSITION_DATA_OFFSET..APRS_POSITION_DATA_OFFSET + 4]
            .copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let aprs = mi.aprs();
        let bytes = aprs.position_data_bytes(0, 4).unwrap();
        assert_eq!(bytes, &[0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn aprs_position_data_bytes_past_region() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        // Try to read past the end of the position data region.
        assert!(mi.aprs().position_data_bytes(APRS_POSITION_DATA_SIZE, 1).is_none());
    }

    #[test]
    fn aprs_has_position_data_empty() {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.aprs().has_position_data());
    }

    #[test]
    fn aprs_has_position_data_populated() {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Write non-zero data in the position data region.
        image[APRS_POSITION_DATA_OFFSET + 100] = 0x42;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.aprs().has_position_data());
    }

    #[test]
    fn aprs_has_position_data_all_ff() {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Fill with 0xFF (common empty marker) -- should not count.
        let end = APRS_POSITION_DATA_OFFSET + APRS_POSITION_DATA_SIZE;
        image[APRS_POSITION_DATA_OFFSET..end].fill(0xFF);
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.aprs().has_position_data());
    }
}
