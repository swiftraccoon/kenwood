//! Addresses, regions, pages, and Programmable-Memory slots of the image.

use crate::error::ValidationError;

/// Bytes in the TM-D750 memory image.
pub const IMAGE_LENGTH: usize = 1_929_472;
/// [`IMAGE_LENGTH`] as an address bound.
pub const IMAGE_LENGTH_U32: u32 = 1_929_472;
/// Largest page the MCP transfer moves at once.
pub const PAGE_SIZE: usize = 256;
/// [`PAGE_SIZE`] for address arithmetic.
pub const PAGE_SIZE_U32: u32 = 256;
/// [`PAGE_SIZE`] as a page length bound.
const PAGE_SIZE_U16: u16 = 256;
/// Programmable-Memory slots (PM off plus PM1 to PM5).
pub const SLOT_COUNT: u8 = 6;
/// Bytes between one slot's menu block and the next.
pub const SLOT_STRIDE: u32 = 8192;

/// A byte address inside the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Address(u32);

impl Address {
    /// Validate an address below [`IMAGE_LENGTH`].
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AddressOutOfRange`] at or past the image end.
    pub const fn new(value: u32) -> Result<Self, ValidationError> {
        if value < IMAGE_LENGTH_U32 {
            Ok(Self(value))
        } else {
            Err(ValidationError::AddressOutOfRange {
                address: value as u64,
                image_length: IMAGE_LENGTH,
            })
        }
    }

    /// The raw address.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// The raw address as an image index.
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// This address plus `offset`, still inside the image.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::AddressOutOfRange`] when the sum leaves the image.
    pub const fn checked_add(self, offset: u32) -> Result<Self, ValidationError> {
        match self.0.checked_add(offset) {
            Some(sum) => Self::new(sum),
            None => Err(ValidationError::AddressOutOfRange {
                address: self.0 as u64 + offset as u64,
                image_length: IMAGE_LENGTH,
            }),
        }
    }
}

/// A half-open byte range `start..end` inside the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Region {
    start: u32,
    end: u32,
}

impl Region {
    /// Validate a non-empty range inside the image.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidRegion`] when `start >= end` or `end > IMAGE_LENGTH`.
    pub const fn new(start: u32, end: u32) -> Result<Self, ValidationError> {
        if start < end && end <= IMAGE_LENGTH_U32 {
            Ok(Self { start, end })
        } else {
            Err(ValidationError::InvalidRegion {
                start,
                end,
                image_length: IMAGE_LENGTH,
            })
        }
    }

    /// A region checked at compile time; used for the pinned region tables.
    ///
    /// # Panics
    ///
    /// Panics (at compile time when used in a `const`) if `start >= end` or
    /// `end` exceeds [`IMAGE_LENGTH_U32`].
    #[must_use]
    pub const fn const_new(start: u32, end: u32) -> Self {
        assert!(
            start < end && end <= IMAGE_LENGTH_U32,
            "region constant must be a non-empty range inside the image"
        );
        Self { start, end }
    }

    /// First byte.
    #[must_use]
    pub const fn start(self) -> u32 {
        self.start
    }

    /// One past the last byte.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.end
    }

    /// Byte count.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the region is empty (never, by construction).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Whether `address` lies inside.
    #[must_use]
    pub const fn contains(self, address: Address) -> bool {
        self.start <= address.0 && address.0 < self.end
    }

    /// Whether `other` lies entirely inside.
    #[must_use]
    pub const fn contains_region(self, other: Self) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    /// This region shifted by `offset` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidRegion`] when the shifted range leaves the image.
    pub const fn offset_by(self, offset: u32) -> Result<Self, ValidationError> {
        match (self.start.checked_add(offset), self.end.checked_add(offset)) {
            (Some(start), Some(end)) => Self::new(start, end),
            _ => Err(ValidationError::InvalidRegion {
                start: self.start,
                end: self.end,
                image_length: IMAGE_LENGTH,
            }),
        }
    }

    /// The pages the transfer walks: 256 bytes each from `start`, then the remainder.
    #[must_use]
    pub fn pages(self) -> Vec<Page> {
        let mut pages = Vec::new();
        let mut cursor = self.start;
        while cursor < self.end {
            let len = (self.end - cursor).min(PAGE_SIZE_U32);
            pages.push(Page {
                address: Address(cursor),
                len: u16::try_from(len).unwrap_or(u16::MAX),
            });
            cursor += len;
        }
        pages
    }
}

/// One transfer unit: up to 256 bytes at an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Page {
    address: Address,
    len: u16,
}

impl Page {
    /// Validate a page of `len` bytes (1..=256) that stays inside the image.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidPageLength`] for a length outside 1..=256
    /// and [`ValidationError::AddressOutOfRange`] when the page leaves the image.
    pub fn new(address: Address, len: usize) -> Result<Self, ValidationError> {
        let bounded = u16::try_from(len)
            .ok()
            .filter(|len| (1..=PAGE_SIZE_U16).contains(len))
            .ok_or(ValidationError::InvalidPageLength { len })?;
        match address.0.checked_add(u32::from(bounded)) {
            Some(end) if end <= IMAGE_LENGTH_U32 => Ok(Self {
                address,
                len: bounded,
            }),
            _ => Err(ValidationError::AddressOutOfRange {
                address: u64::from(address.0) + u64::from(bounded),
                image_length: IMAGE_LENGTH,
            }),
        }
    }

    /// First byte.
    #[must_use]
    pub const fn address(self) -> Address {
        self.address
    }

    /// Byte count (1..=256).
    #[must_use]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    /// Whether the page is empty (never, by construction).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        false
    }

    /// One past the last byte.
    #[must_use]
    pub const fn end(self) -> u32 {
        self.address.0 + self.len as u32
    }

    /// The region this page covers.
    #[must_use]
    pub const fn region(self) -> Region {
        Region {
            start: self.address.0,
            end: self.end(),
        }
    }
}

/// A Programmable-Memory slot index, `0..SLOT_COUNT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SlotIndex(u8);

impl SlotIndex {
    /// Validate a slot index.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SlotOutOfRange`] at or past [`SLOT_COUNT`].
    pub const fn new(slot: u8) -> Result<Self, ValidationError> {
        if slot < SLOT_COUNT {
            Ok(Self(slot))
        } else {
            Err(ValidationError::SlotOutOfRange {
                slot,
                count: SLOT_COUNT,
            })
        }
    }

    /// Every slot in order.
    #[must_use]
    pub const fn all() -> [Self; SLOT_COUNT as usize] {
        [Self(0), Self(1), Self(2), Self(3), Self(4), Self(5)]
    }

    /// The raw index.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.0
    }

    /// Bytes this slot's menu block sits past slot 0's.
    #[must_use]
    pub const fn offset(self) -> u32 {
        SLOT_STRIDE * self.0 as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn addresses_stay_inside_the_image() -> TestResult {
        assert_eq!(
            Address::new(IMAGE_LENGTH_U32 - 1)?.as_u32(),
            IMAGE_LENGTH_U32 - 1
        );
        let past = Address::new(IMAGE_LENGTH_U32);
        assert!(
            matches!(past, Err(ValidationError::AddressOutOfRange { .. })),
            "{past:?}"
        );
        let sum = Address::new(IMAGE_LENGTH_U32 - 1)?.checked_add(1);
        assert!(
            matches!(sum, Err(ValidationError::AddressOutOfRange { .. })),
            "{sum:?}"
        );
        Ok(())
    }

    #[test]
    fn regions_walk_pages_with_the_remainder_last() -> TestResult {
        let region = Region::new(327_681, 327_936)?;
        let pages = region.pages();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages.first().map(|page| page.len()), Some(255));
        let long = Region::new(8, 600)?.pages();
        let lengths: Vec<usize> = long.iter().map(|page| page.len()).collect();
        assert_eq!(lengths, vec![256, 256, 80]);
        assert_eq!(long.first().map(|page| page.address().as_u32()), Some(8));
        assert_eq!(long.last().map(|page| page.end()), Some(600));
        let reversed = Region::new(10, 10);
        assert!(
            matches!(reversed, Err(ValidationError::InvalidRegion { .. })),
            "{reversed:?}"
        );
        Ok(())
    }

    #[test]
    fn regions_contain_addresses_and_shift() -> TestResult {
        let region = Region::new(100, 200)?;
        assert!(region.contains(Address::new(199)?));
        assert!(!region.contains(Address::new(200)?));
        assert!(region.contains_region(Region::new(150, 200)?));
        assert!(!region.contains_region(Region::new(150, 201)?));
        assert_eq!(region.offset_by(8192)?, Region::new(8292, 8392)?);
        Ok(())
    }

    #[test]
    fn pages_and_slots_are_bounded() -> TestResult {
        let page = Page::new(Address::new(0)?, 256)?;
        assert_eq!(page.end(), 256);
        assert_eq!(page.region(), Region::new(0, 256)?);
        let too_long = Page::new(Address::new(0)?, 257);
        assert!(
            matches!(
                too_long,
                Err(ValidationError::InvalidPageLength { len: 257 })
            ),
            "{too_long:?}"
        );
        let past = Page::new(Address::new(IMAGE_LENGTH_U32 - 1)?, 2);
        assert!(
            matches!(past, Err(ValidationError::AddressOutOfRange { .. })),
            "{past:?}"
        );
        assert_eq!(SlotIndex::new(5)?.offset(), 40_960);
        let slot = SlotIndex::new(6);
        assert!(
            matches!(
                slot,
                Err(ValidationError::SlotOutOfRange { slot: 6, count: 6 })
            ),
            "{slot:?}"
        );
        assert_eq!(SlotIndex::all().len(), 6);
        Ok(())
    }
}
