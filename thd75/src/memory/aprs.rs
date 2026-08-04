//! Bounded access to the opaque APRS archive region of the memory image.
//!
//! The bounded region begins with the APRS message status header (256 bytes at
//! page `0x0151`) followed by opaque APRS data. It is not the radio menu
//! settings block; known APRS menu fields are decoded through the generated
//! menu-field registry.
//!
//! # Verification status
//!
//! Field-level offsets inside this APRS data region are not verified for the
//! TH-D75. This module therefore exposes only the verified region boundaries
//! (`status_header` and `data_region`) and a bounded `read_bytes` accessor.
//! It does not assign field semantics to opaque bytes.
//!
//! ## How to reintroduce typed accessors
//!
//! A typed field accessor requires an exact TH-D75 offset, an encoded domain,
//! and controlled radio write/readback evidence that the field tracks the
//! named setting. Until all three are available, callers should use the
//! generated menu-field registry for known settings or treat these bytes as
//! opaque.
//!
//! # Cross-references
//!
//! The region-boundary constants (`APRS_STATUS_PAGE`, `APRS_START`,
//! `DSTAR_CALLSIGN_START`) live in [`crate::protocol::programming`] and are
//! considered verified at the page level. Only sub-page field offsets
//! were unverified. Radio menu settings that sit outside this opaque region,
//! including `aprs.MyCallsign`, are decoded through
//! [`MemoryImage::menu_setting`](crate::memory::MemoryImage::menu_setting).

use crate::protocol::programming;

/// Byte offset of the APRS message status header (`0x15100`).
pub const APRS_STATUS_OFFSET: usize =
    programming::APRS_STATUS_PAGE as usize * programming::PAGE_SIZE;

/// Byte offset of the opaque APRS data region (`0x15200`).
pub const APRS_DATA_OFFSET: usize = programming::APRS_START as usize * programming::PAGE_SIZE;

/// End of the opaque APRS region at the proven D-STAR callsign-table start.
pub const APRS_END_OFFSET: usize =
    programming::DSTAR_CALLSIGN_START as usize * programming::PAGE_SIZE;

// ---------------------------------------------------------------------------
// Sub-page field offsets within the APRS data region
//
// Intentionally empty. See the module-level "Verification status"
// section: typed field accessors were removed when their offsets could
// not be confirmed by controlled memory-image diffs and radio readback. Add
// `const` offsets back here only with reproducible evidence for both the
// address and its encoded domain.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// AprsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the opaque APRS archive region.
///
/// Provides **region-boundary** byte access at pages `0x0151`+. The page-level
/// layout (status header and opaque data region) is verified; sub-page field
/// offsets are not; see
/// the module-level "Verification status" section
/// for what was removed and why, and for the criteria a typed accessor
/// must meet before it can be reintroduced.
///
/// # Known sub-regions
///
/// | MCP Offset | Content                                        | Status         |
/// |------------|------------------------------------------------|----------------|
/// | `0x15100`  | APRS message status header (256 bytes)         | page-verified  |
/// | `0x15200`  | Opaque APRS data                                | boundary only  |
/// | `0x25000`  | D-STAR direct-callsign table begins             | table-verified |
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
    /// The page is returned without assigning meanings to its individual
    /// bytes.
    #[must_use]
    pub fn status_header(&self) -> Option<&[u8]> {
        let end = APRS_STATUS_OFFSET + programming::PAGE_SIZE;
        self.image.get(APRS_STATUS_OFFSET..end)
    }

    /// Get the raw APRS data region (pages `0x0152` through the start of
    /// the D-STAR region).
    ///
    /// No field semantics are assigned within this region. Use
    /// [`MemoryImage::menu_setting`](crate::memory::MemoryImage::menu_setting)
    /// for known APRS menu settings.
    #[must_use]
    pub fn data_region(&self) -> Option<&[u8]> {
        self.image.get(APRS_DATA_OFFSET..APRS_END_OFFSET)
    }

    /// Read an arbitrary byte range from the APRS region.
    ///
    /// The offset is an absolute MCP byte address. Returns `None` if the
    /// addition overflows or any byte falls outside the APRS-owned range.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        if offset < APRS_STATUS_OFFSET || end > APRS_END_OFFSET {
            return None;
        }
        self.image.get(offset..end)
    }

    /// Get the total size of the APRS region in bytes.
    #[must_use]
    pub const fn region_size(&self) -> usize {
        APRS_END_OFFSET - APRS_STATUS_OFFSET
    }

    // -----------------------------------------------------------------------
    // Typed sub-page field accessors remain unavailable pending verification.
    //
    // Typed setting and packet-path accessors previously lived here.
    // See the module-level "Verification status" section for the criteria to
    // introduce any of them.
    //
    // Callers needing a generated, verified radio menu field should use
    // [`MemoryImage::menu_setting`](crate::memory::MemoryImage::menu_setting).
    // For uncatalogued APRS bytes, use [`AprsAccess::read_bytes`] only with an
    // absolute offset independently verified for the TH-D75.
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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

    // Tests for typed sub-page accessors were removed alongside the accessors;
    // they exercised synthetic round-trips at unverified offsets, which
    // is exactly the failure mode the deletion was meant to prevent
    // (a green test passing on a wrong offset is worse than no test
    // because it manufactures false confidence). See the module-level
    // "Verification status" section for the criteria a reintroduced
    // accessor and its test must meet.
}
