//! Typed access to the D-STAR configuration region of the memory image.
//!
//! The D-STAR configuration occupies three regions:
//!
//! - **System settings** at byte offset `0x03F0` (~16 bytes): active
//!   D-STAR channel information.
//! - **MY callsign list** (`dv.MyCallsignDvGatewayList`) at byte
//!   offset `0x1CA8`: six 12-byte records, each an 8-byte space-padded
//!   callsign plus a 4-byte NUL-padded memo, with the active-entry
//!   selector (`dv.MyCallsignSelectDvGateway`) at `0x1CA1`.
//! - **Repeater/callsign list** starting at page `0x02A1` (byte offset
//!   `0x2A100`): up to 1,500 repeater entries (108 bytes each) plus
//!   up to 120 callsign entries (8 bytes each).
//!
//! # Offset confidence
//!
//! The MY callsign list offsets come from the MCP-D75 field registry
//! ([`MCP_D75_MENU_FIELDS`](super::MCP_D75_MENU_FIELDS)) and are
//! confirmed by the hardware dump in `tests/memory_golden.rs`. An
//! earlier revision read the MY callsign from `0x1300` ("D74
//! development notes"); that offset is actually
//! `aprs.StatusTextList[4].StatusText` and never held a callsign.
//!
//! The D-STAR channel info at `0x03F0` is from D74 development notes.
//! The repeater list at page `0x02A1` is confirmed from D74 development
//! notes (1,500 + 30 DR channels). Individual field offsets within
//! repeater records are from firmware analysis and are not yet
//! hardware-verified on the D75.

use crate::protocol::programming;
use crate::types::dstar::{DstarCallsign, RepeaterDuplex, RepeaterEntry};

/// Byte offset of the D-STAR channel info within the system settings region.
const DSTAR_CHANNEL_INFO_OFFSET: usize = 0x03F0;

/// Size of the D-STAR channel info field.
const DSTAR_CHANNEL_INFO_SIZE: usize = 16;

/// Byte offset of the D-STAR repeater list and callsign list.
const DSTAR_RPT_OFFSET: usize = programming::DSTAR_RPT_START as usize * programming::PAGE_SIZE;

/// Estimated end of the D-STAR region (before Bluetooth data).
const DSTAR_END_OFFSET: usize = programming::BT_START as usize * programming::PAGE_SIZE;

/// Size of a single D-STAR repeater list record.
const REPEATER_RECORD_SIZE: usize = 108;

/// Maximum number of repeater entries in the D75.
const MAX_REPEATER_ENTRIES: u16 = 1500;

/// Byte offset of the active MY-callsign selector
/// (`dv.MyCallsignSelectDvGateway`, 1 byte, 0-5).
const DV_MY_CALLSIGN_SELECT_OFFSET: usize = 0x1CA1;

/// Byte offset of the MY callsign list (`dv.MyCallsignDvGatewayList`).
///
/// Six records of [`DV_MY_CALLSIGN_STRIDE`] bytes each: an 8-byte
/// space-padded callsign (`MyCallsignDvGateway`) followed by a 4-byte
/// NUL-padded memo (`MemoDvGateway`).
const DV_MY_CALLSIGN_LIST_OFFSET: usize = 0x1CA8;

/// Number of MY-callsign records in the list.
const DV_MY_CALLSIGN_COUNT: usize = 6;

/// Stride between MY-callsign records (callsign + memo).
const DV_MY_CALLSIGN_STRIDE: usize = 12;

/// Length of the callsign portion of a MY-callsign record.
const DV_MY_CALLSIGN_LEN: usize = 8;

/// Length of the memo portion of a MY-callsign record.
const DV_MY_CALLSIGN_MEMO_LEN: usize = 4;

// ---------------------------------------------------------------------------
// Repeater record field offsets (from firmware RE at 0xC001239C)
// ---------------------------------------------------------------------------

/// Offset within a repeater record for the RPT1 callsign (16 bytes).
const RPT_RPT1_OFFSET: usize = 0x00;

/// Offset within a repeater record for the RPT2/gateway callsign (16 bytes).
const RPT_RPT2_OFFSET: usize = 0x10;

/// Offset within a repeater record for the name field (16 bytes).
const RPT_NAME_OFFSET: usize = 0x20;

/// Offset within a repeater record for the area/sub-name field (16 bytes).
const RPT_AREA_OFFSET: usize = 0x30;

/// Offset within a repeater record for the frequency (4 bytes, uint32 LE, Hz).
const RPT_FREQ_OFFSET: usize = 0x58;

// ---------------------------------------------------------------------------
// DstarAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the D-STAR configuration region.
///
/// Provides raw byte access and typed field accessors for D-STAR settings
/// stored in the system settings area (channel info at `0x03F0`), the MY
/// callsign list at `0x1CA8`, and the repeater/callsign list starting at
/// page `0x02A1`.
///
/// # Known sub-regions
///
/// | MCP Offset | Content |
/// |-----------|---------|
/// | `0x003F0` | D-STAR channel info (16 bytes) |
/// | `0x01CA1` | Active MY-callsign selector (1 byte, 0-5) |
/// | `0x01CA8` | MY callsign list (6 × 12-byte records: callsign + memo) |
/// | `0x2A100` | Repeater list (108-byte records) |
/// | varies | Callsign list (8-byte entries, up to 120) |
#[derive(Debug)]
pub struct DstarAccess<'a> {
    image: &'a [u8],
}

impl<'a> DstarAccess<'a> {
    /// Create a new D-STAR accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the D-STAR channel info bytes (16 bytes at offset `0x03F0`).
    ///
    /// Contains the active D-STAR slot configuration.
    #[must_use]
    pub fn channel_info(&self) -> Option<&[u8]> {
        let end = DSTAR_CHANNEL_INFO_OFFSET + DSTAR_CHANNEL_INFO_SIZE;
        self.image.get(DSTAR_CHANNEL_INFO_OFFSET..end)
    }

    /// Get the raw repeater/callsign list region.
    ///
    /// This region spans from page `0x02A1` to page `0x04D0` (before
    /// the Bluetooth data). It contains both the repeater list and the
    /// callsign list.
    #[must_use]
    pub fn repeater_callsign_region(&self) -> Option<&[u8]> {
        self.image.get(DSTAR_RPT_OFFSET..DSTAR_END_OFFSET)
    }

    /// Get the total size of the D-STAR repeater/callsign region in bytes.
    #[must_use]
    pub const fn region_size(&self) -> usize {
        DSTAR_END_OFFSET - DSTAR_RPT_OFFSET
    }

    /// Read a repeater record by index (raw 108 bytes).
    ///
    /// Each record is 108 bytes. Returns `None` if the index is out of
    /// bounds or the record extends past the region.
    #[must_use]
    pub fn repeater_record(&self, index: usize) -> Option<&[u8]> {
        let offset = DSTAR_RPT_OFFSET + index * REPEATER_RECORD_SIZE;
        let end = offset + REPEATER_RECORD_SIZE;
        if end > DSTAR_END_OFFSET {
            return None;
        }
        self.image.get(offset..end)
    }

    /// Read an arbitrary byte range from the D-STAR region.
    ///
    /// The offset is an absolute MCP byte address. Returns `None` if
    /// the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        self.image.get(offset..end)
    }

    // -----------------------------------------------------------------------
    // Typed D-STAR accessors
    // -----------------------------------------------------------------------

    /// Read the index of the active MY-callsign record
    /// (`dv.MyCallsignSelectDvGateway`, 0-5).
    ///
    /// MCP offset `0x1CA1`. Out-of-range bytes are clamped to 5;
    /// unreadable images return 0.
    #[must_use]
    pub fn my_callsign_select(&self) -> usize {
        self.image
            .get(DV_MY_CALLSIGN_SELECT_OFFSET)
            .copied()
            .map_or(0, |b| usize::from(b.min(5)))
    }

    /// Read one MY-callsign record's callsign
    /// (`dv.MyCallsignDvGatewayList[index].MyCallsignDvGateway`).
    ///
    /// Records live at `0x1CA8 + index * 12` (8 bytes, space-padded).
    /// Returns `None` if `index` is 6 or more, or the record extends
    /// past the image; an unprogrammed record reads as an empty
    /// string.
    #[must_use]
    pub fn my_callsign_entry(&self, index: usize) -> Option<String> {
        if index >= DV_MY_CALLSIGN_COUNT {
            return None;
        }
        let offset = DV_MY_CALLSIGN_LIST_OFFSET + index * DV_MY_CALLSIGN_STRIDE;
        let slice = self.image.get(offset..offset + DV_MY_CALLSIGN_LEN)?;
        // Callsigns are space-padded; unprogrammed records are NULs.
        let s = std::str::from_utf8(slice).unwrap_or("");
        Some(s.trim_end_matches([' ', '\0']).to_owned())
    }

    /// Read one MY-callsign record's memo
    /// (`dv.MyCallsignDvGatewayList[index].MemoDvGateway`).
    ///
    /// The memo is the 4-byte NUL-padded field after the callsign
    /// (offset `0x1CA8 + index * 12 + 8`); the hardware dump shows
    /// "D75A" here. Returns `None` if `index` is 6 or more, or the
    /// record extends past the image.
    #[must_use]
    pub fn my_callsign_memo(&self, index: usize) -> Option<String> {
        if index >= DV_MY_CALLSIGN_COUNT {
            return None;
        }
        let offset =
            DV_MY_CALLSIGN_LIST_OFFSET + index * DV_MY_CALLSIGN_STRIDE + DV_MY_CALLSIGN_LEN;
        let slice = self.image.get(offset..offset + DV_MY_CALLSIGN_MEMO_LEN)?;
        let s = std::str::from_utf8(slice).unwrap_or("");
        Some(s.trim_end_matches([' ', '\0']).to_owned())
    }

    /// Read the active D-STAR MY callsign (up to 8 characters).
    ///
    /// Resolves the selector at `0x1CA1` and reads that record from
    /// the MY callsign list at `0x1CA8` (`dv.MyCallsignDvGatewayList`,
    /// registry-verified and confirmed against a hardware dump).
    /// Returns an empty string if the record is unprogrammed or the
    /// image is too short.
    #[must_use]
    pub fn my_callsign(&self) -> String {
        self.my_callsign_entry(self.my_callsign_select())
            .unwrap_or_default()
    }

    /// Read the active D-STAR MY callsign as a typed [`DstarCallsign`].
    ///
    /// Returns `None` if the callsign is empty or invalid. See
    /// [`my_callsign`](Self::my_callsign) for the offset provenance.
    #[must_use]
    pub fn my_callsign_typed(&self) -> Option<DstarCallsign> {
        let raw = self.my_callsign();
        if raw.is_empty() {
            return None;
        }
        DstarCallsign::new(&raw)
    }

    /// Read a repeater entry by index, parsed into a [`RepeaterEntry`].
    ///
    /// Returns `None` if the index is out of range, the record is
    /// all-`0xFF` (empty), or the record cannot be parsed.
    ///
    /// # Offset
    ///
    /// Repeater records start at `0x2A100` (page `0x02A1`), each 108
    /// bytes. Record N is at offset `0x2A100 + N * 108`.
    ///
    /// # Verification
    ///
    /// Region boundary confirmed. Internal field layout from firmware RE
    /// offset is estimated, not hardware-verified.
    ///
    /// # Field provenance: placeholders, not parsed data
    ///
    /// Only the callsign and frequency fields are read from the image.
    /// The following fields of the returned [`RepeaterEntry`] are
    /// hardcoded placeholders whose flash offsets have not been
    /// reverse-engineered yet: `duplex` (always `Minus`), `offset`
    /// (always 0), `module` (always `B`), `latitude`/`longitude`
    /// (always 0.0), and `position_accuracy` (always `Invalid`). Do
    /// NOT program DR-mode operation from these fields.
    #[must_use]
    pub fn repeater_entry(&self, index: u16) -> Option<RepeaterEntry> {
        if index >= MAX_REPEATER_ENTRIES {
            return None;
        }
        let record = self.repeater_record(index as usize)?;

        // Check for empty record (all 0xFF).
        if record.iter().all(|&b| b == 0xFF) {
            return None;
        }
        // Check for all-zero record (unused).
        if record.iter().all(|&b| b == 0x00) {
            return None;
        }

        let rpt1 = record
            .get(RPT_RPT1_OFFSET..RPT_RPT1_OFFSET + 8)
            .map(extract_dstar_callsign)
            .unwrap_or_default();
        let rpt2 = record
            .get(RPT_RPT2_OFFSET..RPT_RPT2_OFFSET + 8)
            .map(extract_dstar_callsign)
            .unwrap_or_default();
        let name = extract_string_field(record, RPT_NAME_OFFSET, 16);
        let sub_name = extract_string_field(record, RPT_AREA_OFFSET, 16);

        let frequency = record
            .get(RPT_FREQ_OFFSET..RPT_FREQ_OFFSET + 4)
            .and_then(|s| <[u8; 4]>::try_from(s).ok())
            .map_or(0, u32::from_le_bytes);

        Some(RepeaterEntry {
            group_name: String::new(), // Group name not in the 108-byte record
            name,
            sub_name,
            callsign_rpt1: rpt1,
            gateway_rpt2: rpt2,
            frequency,
            duplex: RepeaterDuplex::Minus, // Typical default for D-STAR
            offset: 0,
            module: crate::types::dstar::DstarModule::B,
            latitude: 0.0,
            longitude: 0.0,
            utc_offset: String::new(),
            position_accuracy: crate::types::dstar::PositionAccuracy::Invalid,
            lockout: false,
        })
    }

    /// Count the number of non-empty repeater entries.
    ///
    /// Iterates through the repeater list region and counts entries that
    /// are not all-`0xFF` or all-`0x00`.
    #[must_use]
    pub fn repeater_count(&self) -> u16 {
        let mut count: u16 = 0;
        for i in 0..MAX_REPEATER_ENTRIES {
            if self.repeater_entry(i).is_some() {
                count = count.saturating_add(1);
            }
        }
        count
    }
}

/// Extract a D-STAR callsign from the first 8 bytes of a slice.
fn extract_dstar_callsign(slice: &[u8]) -> DstarCallsign {
    let Some(prefix) = slice.first_chunk::<8>() else {
        return DstarCallsign::default();
    };
    let mut bytes = *prefix;
    // Replace null bytes with spaces for D-STAR wire format.
    for b in &mut bytes {
        if *b == 0 {
            *b = b' ';
        }
    }
    DstarCallsign::from_wire_bytes(&bytes)
}

/// Extract a null-terminated string field from a record.
fn extract_string_field(record: &[u8], offset: usize, max_len: usize) -> String {
    let end = (offset + max_len).min(record.len());
    let Some(slice) = record.get(offset..end) else {
        return String::new();
    };
    let nul = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    let Some(trimmed) = slice.get(..nul) else {
        return String::new();
    };
    String::from_utf8_lossy(trimmed).trim().to_string()
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

    fn make_dstar_image() -> Vec<u8> {
        vec![0u8; TOTAL_SIZE]
    }

    #[test]
    fn dstar_channel_info_accessible() -> TestResult {
        let mut image = make_dstar_image();
        // Write a known pattern at the D-STAR channel info offset.
        write_slice(
            &mut image,
            DSTAR_CHANNEL_INFO_OFFSET,
            &[0xDE, 0xAD, 0xBE, 0xEF],
        )?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let info = dstar.channel_info().ok_or("channel_info returned None")?;
        assert_eq!(info.len(), DSTAR_CHANNEL_INFO_SIZE);
        assert_eq!(
            info.get(..4).ok_or("info too short")?,
            &[0xDE, 0xAD, 0xBE, 0xEF]
        );
        Ok(())
    }

    #[test]
    fn dstar_repeater_region_accessible() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let region = dstar
            .repeater_callsign_region()
            .ok_or("repeater_callsign_region returned None")?;
        assert!(!region.is_empty());
        assert_eq!(region.len(), dstar.region_size());
        Ok(())
    }

    #[test]
    fn dstar_repeater_record() -> TestResult {
        let mut image = make_dstar_image();
        // Write a pattern at the first repeater record.
        write_slice(&mut image, DSTAR_RPT_OFFSET, b"JR6YPR B")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let record = dstar
            .repeater_record(0)
            .ok_or("repeater_record(0) returned None")?;
        assert_eq!(record.len(), REPEATER_RECORD_SIZE);
        assert_eq!(record.get(..8).ok_or("record too short")?, b"JR6YPR B");
        Ok(())
    }

    #[test]
    fn dstar_region_size_positive() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        // D-STAR region should be substantial (>100 KB).
        assert!(dstar.region_size() > 100_000);
        Ok(())
    }

    #[test]
    fn dstar_my_callsign() -> TestResult {
        let mut image = make_dstar_image();
        // Record 0 of dv.MyCallsignDvGatewayList (selector stays 0).
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  ")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign_select(), 0);
        assert_eq!(dstar.my_callsign(), "N0CALL");
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_follows_the_selector() -> TestResult {
        let mut image = make_dstar_image();
        // Record 0 and record 2 hold different callsigns; the selector
        // points at record 2.
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  ")?;
        write_slice(
            &mut image,
            DV_MY_CALLSIGN_LIST_OFFSET + 2 * DV_MY_CALLSIGN_STRIDE,
            b"W1AW    ",
        )?;
        write_slice(&mut image, DV_MY_CALLSIGN_SELECT_OFFSET, &[2])?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign_select(), 2);
        assert_eq!(dstar.my_callsign(), "W1AW");
        assert_eq!(dstar.my_callsign_entry(0).as_deref(), Some("N0CALL"));
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_memo() -> TestResult {
        let mut image = make_dstar_image();
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"N0CALL  D75A")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign_memo(0).as_deref(), Some("D75A"));
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_typed() -> TestResult {
        let mut image = make_dstar_image();
        write_slice(&mut image, DV_MY_CALLSIGN_LIST_OFFSET, b"W1AW    ")?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let typed = dstar
            .my_callsign_typed()
            .ok_or("my_callsign_typed returned None")?;
        assert_eq!(typed.as_str(), "W1AW");
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_empty() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.my_callsign(), "");
        assert!(dstar.my_callsign_typed().is_none());
        Ok(())
    }

    #[test]
    fn dstar_my_callsign_entry_out_of_range() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert!(dstar.my_callsign_entry(DV_MY_CALLSIGN_COUNT).is_none());
        assert!(dstar.my_callsign_memo(DV_MY_CALLSIGN_COUNT).is_none());
        Ok(())
    }

    #[test]
    fn dstar_repeater_entry_empty_record() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        // All-zero record should be None.
        assert!(dstar.repeater_entry(0).is_none());
        Ok(())
    }

    #[test]
    fn dstar_repeater_entry_populated() -> TestResult {
        let mut image = make_dstar_image();
        let offset = DSTAR_RPT_OFFSET;

        // Write RPT1 callsign at record offset 0x00.
        write_slice(&mut image, offset, b"JR6YPR B")?;
        // Write RPT2/gateway at record offset 0x10.
        write_slice(&mut image, offset + 0x10, b"JR6YPR G")?;
        // Write name at record offset 0x20.
        write_slice(&mut image, offset + 0x20, b"Test Rptr\0\0\0\0\0\0\0")?;
        // Write frequency at record offset 0x58: 439.01 MHz = 439010000 Hz.
        let freq: u32 = 439_010_000;
        write_slice(&mut image, offset + 0x58, &freq.to_le_bytes())?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        let entry = dstar
            .repeater_entry(0)
            .ok_or("repeater_entry(0) returned None")?;
        assert_eq!(entry.callsign_rpt1.as_str(), "JR6YPR B");
        assert_eq!(entry.gateway_rpt2.as_str(), "JR6YPR G");
        assert_eq!(entry.name, "Test Rptr");
        assert_eq!(entry.frequency, 439_010_000);
        Ok(())
    }

    #[test]
    fn dstar_repeater_entry_out_of_range() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert!(dstar.repeater_entry(MAX_REPEATER_ENTRIES).is_none());
        Ok(())
    }

    #[test]
    fn dstar_repeater_count_all_empty() -> TestResult {
        let image = make_dstar_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.repeater_count(), 0);
        Ok(())
    }

    #[test]
    fn dstar_repeater_count_with_entries() -> TestResult {
        let mut image = make_dstar_image();
        // Populate 3 repeater entries.
        for i in 0..3 {
            write_slice(
                &mut image,
                DSTAR_RPT_OFFSET + i * REPEATER_RECORD_SIZE,
                b"TESTCALL",
            )?;
        }

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let dstar = mi.dstar();
        assert_eq!(dstar.repeater_count(), 3);
        Ok(())
    }
}
