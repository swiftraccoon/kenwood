//! Typed access to the APRS configuration region of the memory image.
//!
//! The APRS configuration occupies pages `0x0151`+ in the MCP address
//! space. This includes the APRS message status header (256 bytes at
//! page `0x0151`), followed by APRS messages, settings, and extended
//! configuration data.
//!
//! # Verification status
//!
//! ⚠ **Field-level offsets inside the APRS data region are not
//! hardware-verified on a TH-D75.** This module exposes only the region-
//! boundary readers (`status_header`, `data_region`, `position_data_*`)
//! and a generic `read_bytes` accessor. Field-level typed accessors
//! (`my_callsign`, `beacon_interval`, `packet_path`) were previously
//! present but **have been removed**: their offsets were ported from
//! TH-D74 development notes during the April 2026 extraction, and
//! D74 confirmations are not confirmations on D75. Returning typed
//! values from unverified
//! offsets violates the validation contract: the type system promised
//! "this is the callsign" while in practice the bytes might come from
//! a completely unrelated field.
//!
//! ## How to reintroduce typed accessors
//!
//! Each field needs *both* of the following before a typed accessor is
//! added back:
//!
//! 1. **Firmware-RE confirmation.** Trace the MCP address-decoder
//!    function in the TH-D75 firmware and confirm the candidate offset
//!    is a D75 finding, not a D74 trace.
//! 2. **Hardware round-trip.** Capture an MCP image from a known-state
//!    radio, set the field via the menu, re-capture, then diff the byte
//!    at the candidate offset and confirm it tracks the change.
//!
//! Only when both pass should a typed accessor land, with the source
//! comment recording the firmware function address and the hardware
//! capture date that verified it.
//!
//! # Cross-references
//!
//! The region-boundary constants (`APRS_STATUS_PAGE`, `APRS_START`,
//! `DSTAR_RPT_START`) live in [`crate::protocol::programming`] and are
//! considered verified at the page level. Only sub-page field offsets
//! were unverified.

use crate::protocol::programming;

/// Byte offset of the APRS message status header (`0x15100`).
pub const APRS_STATUS_OFFSET: usize =
    programming::APRS_STATUS_PAGE as usize * programming::PAGE_SIZE;

/// Byte offset of the APRS messages and settings region (`0x15200`).
pub const APRS_DATA_OFFSET: usize = programming::APRS_START as usize * programming::PAGE_SIZE;

/// Estimated end of the APRS region (before D-STAR repeater list).
pub const APRS_END_OFFSET: usize = programming::DSTAR_RPT_START as usize * programming::PAGE_SIZE;

// ---------------------------------------------------------------------------
// Sub-page field offsets within the APRS data region
//
// Intentionally empty. See the module-level "Verification status"
// section: typed field accessors were removed when their offsets could
// not be confirmed against D75 firmware RE or hardware. Add `const`
// offsets back here only when the corresponding accessor has both a
// firmware-RE citation and a hardware-capture date.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// APRS/GPS position data region
//
// The APRS/GPS position data occupies 0x4B00 bytes (19,200 bytes) starting
// at byte offset 0x25100 in the MCP memory image.
// ---------------------------------------------------------------------------

/// Byte offset of the APRS/GPS position data region (`0x25100`).
///
/// 0x4B00 bytes of APRS/GPS position data starting at offset 0x25100.
pub const APRS_POSITION_DATA_OFFSET: usize = 0x2_5100;

/// Size of the APRS/GPS position data region in bytes.
pub const APRS_POSITION_DATA_SIZE: usize = 0x4B00;

// ---------------------------------------------------------------------------
// AprsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the APRS configuration region.
///
/// Provides **region-boundary** byte access for the APRS settings
/// region at pages `0x0151`+. The page-level layout (status header,
/// data region, position data region) is verified; sub-page field
/// offsets are not; see the module-level "Verification status" section
/// for what was removed and why, and for the criteria a typed accessor
/// must meet before it can be reintroduced.
///
/// # Known sub-regions
///
/// | MCP Offset | Content                                        | Status         |
/// |------------|------------------------------------------------|----------------|
/// | `0x15100`  | APRS message status header (256 bytes)         | page-verified  |
/// | `0x15200`  | APRS messages and settings (~16 KB)            | page-verified  |
/// | `0x25100`  | APRS/GPS position data region (0x4B00 bytes)   | page-verified  |
/// | (any sub-page field)                                        | **unverified** |
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
        self.image.get(APRS_STATUS_OFFSET..end)
    }

    /// Get the raw APRS data region (pages `0x0152` through the start of
    /// the D-STAR region).
    ///
    /// Contains APRS messages, callsign, status texts, packet path,
    /// `SmartBeaconing` parameters, digipeater config, and more.
    #[must_use]
    pub fn data_region(&self) -> Option<&[u8]> {
        self.image.get(APRS_DATA_OFFSET..APRS_END_OFFSET)
    }

    /// Read an arbitrary byte range from the APRS region.
    ///
    /// The offset is an absolute MCP byte address. Returns `None` if
    /// the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        self.image.get(offset..end)
    }

    /// Get the total size of the APRS region in bytes.
    #[must_use]
    pub const fn region_size(&self) -> usize {
        APRS_END_OFFSET - APRS_STATUS_OFFSET
    }

    // -----------------------------------------------------------------------
    // Typed sub-page field accessors: REMOVED pending verification.
    //
    // `my_callsign`, `my_callsign_typed`, `beacon_interval`,
    // `packet_path_index`, and `packet_path` previously lived here.
    // They were removed because the sub-page offsets they relied on
    // were imported from D74 development notes and never confirmed
    // against D75 firmware or hardware. See the module-level
    // "Verification status" section for the criteria to reintroduce
    // any of them.
    //
    // Callers needing field-level access today should use
    // [`AprsAccess::read_bytes`] with an absolute offset they have
    // verified themselves, then parse the bytes locally.
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // APRS/GPS position data region (page-verified address)
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
        self.image.get(APRS_POSITION_DATA_OFFSET..end)
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
        self.image.get(abs_offset..abs_offset + len)
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

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn set_byte(image: &mut [u8], offset: usize, value: u8) -> Result<(), BoxErr> {
        let img_len = image.len();
        *image
            .get_mut(offset)
            .ok_or_else(|| format!("set_byte: offset {offset} out of range (len={img_len})"))? =
            value;
        Ok(())
    }

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

    #[test]
    fn aprs_status_header_accessible() -> TestResult {
        let image = vec![0xAA_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let aprs = mi.aprs();
        let header = aprs
            .status_header()
            .ok_or("aprs.status_header() returned None")?;
        assert_eq!(header.len(), programming::PAGE_SIZE);
        assert!(header.iter().all(|&b| b == 0xAA));
        Ok(())
    }

    #[test]
    fn aprs_data_region_accessible() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let aprs = mi.aprs();
        let data = aprs
            .data_region()
            .ok_or("aprs.data_region() returned None")?;
        assert!(!data.is_empty());
        // Region should span from APRS_DATA_OFFSET to APRS_END_OFFSET.
        let expected_size = APRS_END_OFFSET - APRS_DATA_OFFSET;
        assert_eq!(data.len(), expected_size);
        Ok(())
    }

    #[test]
    fn aprs_region_size() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let aprs = mi.aprs();
        // Region should be non-trivial (several KB).
        assert!(aprs.region_size() > 1000);
        Ok(())
    }

    // Tests for typed sub-page accessors (my_callsign, beacon_interval,
    // packet_path) were removed alongside the accessors themselves;
    // they exercised synthetic round-trips at unverified offsets, which
    // is exactly the failure mode the deletion was meant to prevent
    // (a green test passing on a wrong offset is worse than no test
    // because it manufactures false confidence). See the module-level
    // "Verification status" section for the criteria a reintroduced
    // accessor and its test must meet.

    // -----------------------------------------------------------------------
    // APRS/GPS position data region tests (page-verified address)
    // -----------------------------------------------------------------------

    #[test]
    fn aprs_position_data_region_accessible() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let aprs = mi.aprs();
        let region = aprs
            .position_data_region()
            .ok_or("position_data_region returned None")?;
        assert_eq!(region.len(), APRS_POSITION_DATA_SIZE);
        Ok(())
    }

    #[test]
    fn aprs_position_data_size() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.aprs().position_data_size(), 0x4B00);
        Ok(())
    }

    #[test]
    fn aprs_position_data_bytes() -> TestResult {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Write known data at the start of the position data region.
        write_slice(
            &mut image,
            APRS_POSITION_DATA_OFFSET,
            &[0x01, 0x02, 0x03, 0x04],
        )?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let aprs = mi.aprs();
        let bytes = aprs
            .position_data_bytes(0, 4)
            .ok_or("position_data_bytes(0, 4) returned None")?;
        assert_eq!(bytes, &[0x01, 0x02, 0x03, 0x04]);
        Ok(())
    }

    #[test]
    fn aprs_position_data_bytes_past_region() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        // Try to read past the end of the position data region.
        assert!(
            mi.aprs()
                .position_data_bytes(APRS_POSITION_DATA_SIZE, 1)
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn aprs_has_position_data_empty() -> TestResult {
        let image = vec![0u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.aprs().has_position_data());
        Ok(())
    }

    #[test]
    fn aprs_has_position_data_populated() -> TestResult {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Write non-zero data in the position data region.
        set_byte(&mut image, APRS_POSITION_DATA_OFFSET + 100, 0x42)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.aprs().has_position_data());
        Ok(())
    }

    #[test]
    fn aprs_has_position_data_all_ff() -> TestResult {
        let mut image = vec![0u8; TOTAL_SIZE];
        // Fill with 0xFF (common empty marker) -- should not count.
        fill_range(
            &mut image,
            APRS_POSITION_DATA_OFFSET,
            APRS_POSITION_DATA_SIZE,
            0xFF,
        )?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.aprs().has_position_data());
        Ok(())
    }
}
