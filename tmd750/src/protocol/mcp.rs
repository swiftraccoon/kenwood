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

use crate::error::ProtocolError;
use crate::types::{Address, PAGE_SIZE, Page};

/// Baud rate of the whole programming session.
pub const BAUD: u32 = 9600;
/// Programming-mode entry; the leading `\r` clears any stale partial line.
pub const ENTER: &[u8] = b"\r0M PROGRAM\r";
/// Line (without terminator) the radio answers to [`ENTER`].
///
/// The TH-D75 answers `0M`; the TM-D750's reply text is a day-one hardware
/// finding and this value is provisional until then.
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
    /// Offset inside the page.
    pub offset: u8,
    /// Bits this patch owns.
    pub mask: u8,
    /// Bit values (already masked).
    pub value: u8,
}

/// Masked updates to one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePatch {
    /// The page.
    pub page: Page,
    /// Updates, in ascending offset order.
    pub bytes: Vec<BytePatch>,
}

impl PagePatch {
    /// Apply the updates to `data` (the page's current bytes).
    pub fn apply(&self, data: &mut [u8]) {
        for patch in &self.bytes {
            if let Some(byte) = data.get_mut(usize::from(patch.offset)) {
                *byte = (*byte & !patch.mask) | (patch.value & patch.mask);
            }
        }
    }

    /// Whether `data` already carries every update.
    #[must_use]
    pub fn is_applied(&self, data: &[u8]) -> bool {
        self.bytes.iter().all(|patch| {
            data.get(usize::from(patch.offset))
                .is_some_and(|byte| byte & patch.mask == patch.value & patch.mask)
        })
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

    /// Startup-screen bitmap, transferred only on explicit request; never by this crate.
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
        let patch = PagePatch {
            page: Page::new(Address::new(8)?, 40)?,
            bytes: vec![
                BytePatch {
                    offset: 0,
                    mask: 0x0F,
                    value: 0x05,
                },
                BytePatch {
                    offset: 3,
                    mask: 0xFF,
                    value: 0xAA,
                },
            ],
        };
        let mut data = vec![0xF0; 40];
        assert!(!patch.is_applied(&data));
        patch.apply(&mut data);
        assert_eq!(data.first().copied(), Some(0xF5));
        assert_eq!(data.get(3).copied(), Some(0xAA));
        assert!(patch.is_applied(&data));
        Ok(())
    }
}
