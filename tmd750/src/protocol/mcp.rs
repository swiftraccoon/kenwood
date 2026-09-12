//! MCP programming-mode framing and the address regions the official
//! program transfers.
//!
//! Every page moves under a five-byte header: a command byte, a 24-bit
//! big-endian address, and a length byte in which 0 means 256. A read
//! request (`R`) is answered with a header whose command is `W` (data
//! follows) or `Z` (one fill byte follows: the whole page is that byte);
//! both sides then exchange `0x06`. A write (`W`) carries its data after
//! the header and is acknowledged with `0x06`. `Z` from the host fills a
//! range and is recorded here but unused.

use std::collections::BTreeMap;

use crate::error::{ProtocolError, SchemaError};
use crate::types::{Address, PAGE_SIZE, Page};

/// Baud rate of the whole programming session.
pub const BAUD: u32 = 9600;
/// Programming-mode entry sent by the official program, without a leading CR.
pub const ENTER: &[u8] = b"0M PROGRAM\r";
/// Exact reply line the official program requires after [`ENTER`].
///
/// This excludes the carriage-return terminator. The exchange still requires
/// qualification against the connected radio's firmware.
pub const ENTER_RESPONSE: &[u8] = b"0M";
/// Exit byte.
pub const EXIT: u8 = b'E';
/// Acknowledge byte exchanged after each page and after exit.
pub const ACK: u8 = 0x06;
/// Host read request command byte.
pub const READ: u8 = b'R';
/// Write command byte (host writes; the radio also uses it to prefix read data).
pub const WRITE: u8 = b'W';
/// Fill command byte (host fills; the radio also uses it to report a uniform page).
pub const FILL: u8 = b'Z';
/// Header length in bytes.
pub const HEADER_LEN: usize = 5;

/// A five-byte page header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Command byte.
    pub command: u8,
    /// Page address.
    pub address: Address,
    /// Page length, 1..=256.
    pub len: usize,
}

impl Header {
    /// Encode as `command, address[23:16], address[15:8], address[7:0], len (256 as 0)`.
    #[must_use]
    pub fn encode(self) -> [u8; HEADER_LEN] {
        let [_, high, middle, low] = self.address.as_u32().to_be_bytes();
        let len = if self.len == PAGE_SIZE {
            0
        } else {
            u8::try_from(self.len).unwrap_or(0)
        };
        [self.command, high, middle, low, len]
    }

    /// Decode a header the radio sent.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::UnknownHeaderCommand`] for a command byte other
    /// than `R`, `W`, or `Z`, and [`ProtocolError::FieldParse`] when the
    /// address leaves the image.
    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, ProtocolError> {
        let [command, high, middle, low, len] = *bytes;
        if !matches!(command, READ | WRITE | FILL) {
            return Err(ProtocolError::UnknownHeaderCommand { command });
        }
        let raw = u32::from_be_bytes([0, high, middle, low]);
        let address = Address::new(raw).map_err(|error| ProtocolError::FieldParse {
            command: "MCP header",
            field: "address",
            detail: error.to_string(),
        })?;
        let len = if len == 0 {
            PAGE_SIZE
        } else {
            usize::from(len)
        };
        Ok(Self {
            command,
            address,
            len,
        })
    }
}

/// The header for reading `page`.
#[must_use]
pub fn read_request(page: Page) -> [u8; HEADER_LEN] {
    Header {
        command: READ,
        address: page.address(),
        len: page.len(),
    }
    .encode()
}

/// The header for writing `page`; the data follows it on the wire.
#[must_use]
pub fn write_request(page: Page) -> [u8; HEADER_LEN] {
    Header {
        command: WRITE,
        address: page.address(),
        len: page.len(),
    }
    .encode()
}

/// One masked byte update inside a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BytePatch {
    offset: u8,
    mask: u8,
    value: u8,
}

impl BytePatch {
    /// Construct one nonempty, already-masked bit assignment.
    ///
    /// The containing [`PagePatch`] additionally checks the offset against its
    /// page length. Construction performs no I/O and grants no write authority.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::InvalidBytePatch`] for a zero mask or value bits
    /// outside the declared mask; input bits are never silently discarded.
    pub const fn new(offset: u8, mask: u8, value: u8) -> Result<Self, SchemaError> {
        if mask == 0 || value & !mask != 0 {
            Err(SchemaError::InvalidBytePatch { offset, mask })
        } else {
            Ok(Self {
                offset,
                mask,
                value,
            })
        }
    }

    /// Offset inside the containing page.
    #[must_use]
    pub const fn offset(self) -> u8 {
        self.offset
    }

    /// Nonzero set of bits owned by this assignment.
    #[must_use]
    pub const fn mask(self) -> u8 {
        self.mask
    }

    /// Desired bits, already positioned within [`Self::mask`].
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

/// Nonempty, disjoint masked updates within one complete transfer page.
///
/// Construction validates shape only. The programming layer independently
/// enforces the applicable writable-region and firmware policies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePatch {
    page: Page,
    bytes: Vec<BytePatch>,
}

impl PagePatch {
    /// Validate all bit claims and coalesce distinct bits at the same offset.
    ///
    /// Claims are retained in ascending offset order. Repeating any owned bit,
    /// even with the same desired value, is rejected rather than overwritten.
    ///
    /// # Errors
    ///
    /// Rejects an empty patch, offsets outside the page, and overlapping claims.
    pub fn new(page: Page, bytes: Vec<BytePatch>) -> Result<Self, SchemaError> {
        if bytes.is_empty() {
            return Err(SchemaError::EmptyPagePatch {
                address: page.address().as_u32(),
            });
        }
        let mut merged: BTreeMap<u8, BytePatch> = BTreeMap::new();
        for patch in bytes {
            if usize::from(patch.offset) >= page.len() {
                return Err(SchemaError::PatchOffsetOutOfBounds {
                    address: page.address().as_u32(),
                    offset: patch.offset,
                    len: page.len(),
                });
            }
            if let Some(existing) = merged.get_mut(&patch.offset) {
                if existing.mask & patch.mask != 0 {
                    return Err(SchemaError::OverlappingPatchBits {
                        address: page.address().as_u32(),
                        offset: patch.offset,
                    });
                }
                existing.mask |= patch.mask;
                existing.value |= patch.value;
            } else {
                let _previous = merged.insert(patch.offset, patch);
            }
        }
        Ok(Self {
            page,
            bytes: merged.into_values().collect(),
        })
    }

    /// Complete transfer page containing every bit claim.
    #[must_use]
    pub const fn page(&self) -> Page {
        self.page
    }

    /// Validated, coalesced byte updates in ascending offset order.
    #[must_use]
    pub fn bytes(&self) -> &[BytePatch] {
        &self.bytes
    }

    /// Apply every update to an exact complete page buffer.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::PatchBufferLength`] before changing any byte when
    /// the buffer does not exactly match this page's length.
    pub fn apply(&self, data: &mut [u8]) -> Result<(), SchemaError> {
        self.validate_buffer(data)?;
        let actual = data.len();
        for patch in &self.bytes {
            let byte = data
                .get_mut(usize::from(patch.offset))
                .ok_or_else(|| self.buffer_error(actual))?;
            *byte = (*byte & !patch.mask) | patch.value;
        }
        Ok(())
    }

    /// Whether an exact complete page buffer carries every update.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::PatchBufferLength`] for missing or excess bytes.
    pub fn is_applied(&self, data: &[u8]) -> Result<bool, SchemaError> {
        self.validate_buffer(data)?;
        Ok(self.bytes.iter().all(|patch| {
            data.get(usize::from(patch.offset))
                .is_some_and(|byte| byte & patch.mask == patch.value)
        }))
    }

    const fn validate_buffer(&self, data: &[u8]) -> Result<(), SchemaError> {
        if data.len() == self.page.len() {
            Ok(())
        } else {
            Err(self.buffer_error(data.len()))
        }
    }

    const fn buffer_error(&self, actual: usize) -> SchemaError {
        SchemaError::PatchBufferLength {
            address: self.page.address().as_u32(),
            expected: self.page.len(),
            actual,
        }
    }
}

/// The address regions the official program transfers.
pub mod regions {
    use crate::types::{Address, PAGE_SIZE_U32, Page, Region, SLOT_STRIDE, SlotIndex};

    /// Global settings, transferred by every read and write.
    pub const GLOBAL_SETTINGS: [Region; 9] = [
        Region::const_new(8, 48),
        Region::const_new(56, 256),
        Region::const_new(256, 416),
        Region::const_new(480, 512),
        Region::const_new(512, 2048),
        Region::const_new(2048, 86_016),
        Region::const_new(150_784, 311_296),
        Region::const_new(314_624, 315_136),
        Region::const_new(320_512, 327_424),
    ];

    /// Slot 0's menu blocks; later slots add `SLOT_STRIDE` per index.
    const SLOT_MENU_BASE: [Region; 4] = [
        Region::const_new(327_681, 327_936),
        Region::const_new(327_936, 332_032),
        Region::const_new(332_800, 332_928),
        Region::const_new(333_824, 335_360),
    ];

    /// Startup-screen bitmap, read only when explicitly selected.
    ///
    /// Standard backups omit it, and scalar menu patches cannot write it.
    pub const STARTUP_BITMAP: Region = Region::const_new(393_216, 1_929_216);

    /// Transferred by another feature of the program; purpose recorded in the
    /// crate notes, unused here.
    pub const UNNAMED_GROUP: [Region; 2] = [
        Region::const_new(311_296, 314_624),
        Region::const_new(315_136, 315_392),
    ];

    /// The four menu blocks of `slot`.
    #[must_use]
    pub fn slot_menu(slot: SlotIndex) -> [Region; 4] {
        let shift = u32::from(slot.index()) * SLOT_STRIDE;
        SLOT_MENU_BASE.map(|region| region.offset_by(shift).unwrap_or(region))
    }

    /// Every region the menu slice reads: global settings, then each slot.
    #[must_use]
    pub fn menu_regions() -> Vec<Region> {
        let mut regions = GLOBAL_SETTINGS.to_vec();
        for slot in SlotIndex::all() {
            regions.extend(slot_menu(slot));
        }
        regions
    }

    /// Every region this slice may write (the menu regions).
    #[must_use]
    pub fn writable_regions() -> Vec<Region> {
        menu_regions()
    }

    /// Whether `page` lies entirely inside one writable region.
    #[must_use]
    pub fn is_writable_page(page: Page) -> bool {
        writable_regions()
            .into_iter()
            .any(|region| region.contains_region(page.region()))
    }

    /// The page of the writable region walk that holds `address`, if any.
    #[must_use]
    pub fn writable_page_for(address: Address) -> Option<Page> {
        let region = writable_regions()
            .into_iter()
            .find(|region| region.contains(address))?;
        let offset = (address.as_u32() - region.start()) / PAGE_SIZE_U32 * PAGE_SIZE_U32;
        let start = Address::new(region.start() + offset).ok()?;
        let len = (region.end() - start.as_u32()).min(PAGE_SIZE_U32);
        Page::new(start, usize::try_from(len).ok()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::regions::{
        GLOBAL_SETTINGS, is_writable_page, menu_regions, slot_menu, writable_page_for,
    };
    use super::*;
    use crate::types::{Region, SlotIndex};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn headers_round_trip_with_256_as_zero() -> TestResult {
        let page = Page::new(Address::new(0x05_00_01)?, 256)?;
        let request = read_request(page);
        assert_eq!(request, [b'R', 0x05, 0x00, 0x01, 0x00]);
        let decoded = Header::decode(&request)?;
        assert_eq!(decoded.command, READ);
        assert_eq!(decoded.address.as_u32(), 0x05_00_01);
        assert_eq!(decoded.len, 256);
        let short = write_request(Page::new(Address::new(8)?, 40)?);
        assert_eq!(short, [b'W', 0x00, 0x00, 0x08, 40]);
        let unknown = Header::decode(&[b'Q', 0, 0, 0, 0]);
        assert!(
            matches!(
                unknown,
                Err(ProtocolError::UnknownHeaderCommand { command: b'Q' })
            ),
            "{unknown:?}"
        );
        let outside = Header::decode(&[b'W', 0xFF, 0xFF, 0xFF, 0]);
        assert!(
            matches!(outside, Err(ProtocolError::FieldParse { .. })),
            "{outside:?}"
        );
        Ok(())
    }

    #[test]
    fn slot_regions_shift_by_the_stride() -> TestResult {
        let slot5 = slot_menu(SlotIndex::new(5)?);
        assert_eq!(slot5.first().copied(), Some(Region::new(368_641, 368_896)?));
        assert_eq!(menu_regions().len(), GLOBAL_SETTINGS.len() + 6 * 4);
        Ok(())
    }

    #[test]
    fn writable_pages_follow_the_region_walk() -> TestResult {
        let inside = writable_page_for(Address::new(327_700)?).ok_or("page missing")?;
        assert_eq!(inside.address().as_u32(), 327_681);
        assert_eq!(inside.len(), 255);
        assert!(is_writable_page(inside));
        let straddling = Page::new(Address::new(327_680)?, 256)?;
        assert!(!is_writable_page(straddling));
        assert!(writable_page_for(Address::new(400_000)?).is_none());
        Ok(())
    }

    #[test]
    fn page_patches_apply_masked_bits() -> TestResult {
        let patch = PagePatch::new(
            Page::new(Address::new(8)?, 40)?,
            vec![
                BytePatch::new(0, 0x0F, 0x05)?,
                BytePatch::new(3, 0xFF, 0xAA)?,
            ],
        )?;
        let mut data = vec![0xF0; 40];
        assert!(
            !patch.is_applied(&data)?,
            "original bytes must not satisfy the new claims"
        );
        patch.apply(&mut data)?;
        assert_eq!(
            data.first().copied(),
            Some(0xF5),
            "unowned high bits must remain unchanged"
        );
        assert_eq!(
            data.get(3).copied(),
            Some(0xAA),
            "full-byte assignment must apply exactly"
        );
        assert!(
            patch.is_applied(&data)?,
            "complete applied page must satisfy every claim"
        );
        Ok(())
    }

    #[test]
    fn page_patch_construction_rejects_invalid_claims() -> TestResult {
        let page = Page::new(Address::new(8)?, 40)?;
        assert!(
            BytePatch::new(0, 0, 0).is_err(),
            "zero mask must not become a meaningless intent"
        );
        assert!(
            BytePatch::new(0, 1, 2).is_err(),
            "value bits outside the mask must be rejected"
        );
        assert!(
            PagePatch::new(page, vec![]).is_err(),
            "empty patch must not prove an intended change"
        );
        assert!(
            PagePatch::new(page, vec![BytePatch::new(40, 0xFF, 7)?]).is_err(),
            "first offset past the fragment must be rejected at construction"
        );
        assert!(
            PagePatch::new(
                page,
                vec![BytePatch::new(0, 1, 1)?, BytePatch::new(0, 1, 1)?]
            )
            .is_err(),
            "overlapping claims must be rejected even when their values agree"
        );
        let patch = PagePatch::new(
            page,
            vec![
                BytePatch::new(3, 1, 1)?,
                BytePatch::new(0, 1, 1)?,
                BytePatch::new(0, 2, 0)?,
            ],
        )?;
        assert_eq!(
            patch.page(),
            page,
            "construction must preserve the transfer page"
        );
        assert_eq!(
            patch.bytes(),
            &[BytePatch::new(0, 3, 1)?, BytePatch::new(3, 1, 1)?],
            "disjoint bit claims must coalesce in sorted offset order"
        );
        assert_eq!(
            patch.bytes().first().copied().map(BytePatch::offset),
            Some(0),
            "first coalesced offset must remain accessible"
        );
        assert_eq!(
            patch.bytes().first().copied().map(BytePatch::mask),
            Some(3),
            "coalesced mask must retain all owned bits"
        );
        assert_eq!(
            patch.bytes().first().copied().map(BytePatch::value),
            Some(1),
            "coalesced value must remain already masked"
        );
        Ok(())
    }

    #[test]
    fn page_patch_buffer_errors_leave_every_byte_unchanged() -> TestResult {
        let patch = PagePatch::new(
            Page::new(Address::new(8)?, 40)?,
            vec![BytePatch::new(0, 0xFF, 7)?],
        )?;
        for len in [0, 1, 39, 41] {
            let mut data = vec![0xA5; len];
            assert!(
                patch.apply(&mut data).is_err(),
                "{len}-byte buffer must not substitute for the complete page"
            );
            assert_eq!(
                data,
                vec![0xA5; len],
                "buffer validation must precede every mutation"
            );
            assert!(
                patch.is_applied(&data).is_err(),
                "partial or oversized data must not become verification evidence"
            );
        }
        Ok(())
    }
}
