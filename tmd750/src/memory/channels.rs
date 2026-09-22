//! Offline access to the stored memory channels of a captured image.
//!
//! The flag, record and name tables lie inside the standard configuration
//! schedule (the `2048..86016` region), so a standard backup converted to a
//! [`MemoryImage`] holds every channel. Decoding describes the supplied
//! bytes only.

use crate::error::ValidationError;
use crate::memory::MemoryImage;
use crate::types::{
    CHANNEL_DATA_OFFSET, CHANNEL_FLAGS_OFFSET, CHANNEL_NAME_SIZE, CHANNEL_NAMES_OFFSET,
    CHANNEL_RECORD_SIZE, FLAG_RECORD_SIZE, PHYSICAL_CHANNEL_COUNT, PhysicalChannel, Region,
    StoredChannelEntry,
};

/// One decoded slot, or the reason its bytes did not decode.
pub type EntryResult = Result<StoredChannelEntry, ValidationError>;

fn image_address(value: usize) -> Result<u32, ValidationError> {
    u32::try_from(value).map_err(|_| ValidationError::AddressOutOfRange {
        address: u64::try_from(value).unwrap_or(u64::MAX),
        image_length: crate::types::IMAGE_LENGTH,
    })
}

/// Borrowed image bytes covering the three channel tables.
#[derive(Debug, Clone, Copy)]
pub struct ChannelAccess<'a> {
    image: &'a [u8],
}

impl<'a> ChannelAccess<'a> {
    /// First image length that holds every table.
    pub const REQUIRED_LENGTH: usize =
        CHANNEL_NAMES_OFFSET + CHANNEL_NAME_SIZE * PHYSICAL_CHANNEL_COUNT as usize;

    /// The three table regions, in address order: flags, records, names.
    ///
    /// # Errors
    ///
    /// Never fails for the fixed table bounds; the `Result` is the region
    /// constructor's.
    pub fn regions() -> Result<[Region; 3], ValidationError> {
        let last = PhysicalChannel::new(PHYSICAL_CHANNEL_COUNT - 1)?;
        let flags = Region::new(
            image_address(CHANNEL_FLAGS_OFFSET)?,
            image_address(last.flag_address() + FLAG_RECORD_SIZE)?,
        )?;
        let records = Region::new(
            image_address(CHANNEL_DATA_OFFSET)?,
            image_address(last.data_address() + CHANNEL_RECORD_SIZE)?,
        )?;
        let names = Region::new(
            image_address(CHANNEL_NAMES_OFFSET)?,
            image_address(Self::REQUIRED_LENGTH)?,
        )?;
        Ok([flags, records, names])
    }

    /// Borrow image bytes that start at image address zero.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ImageTooShortForChannels`] below
    /// [`Self::REQUIRED_LENGTH`].
    pub const fn new(image: &'a [u8]) -> Result<Self, ValidationError> {
        if image.len() < Self::REQUIRED_LENGTH {
            Err(ValidationError::ImageTooShortForChannels {
                actual: image.len(),
                required: Self::REQUIRED_LENGTH,
            })
        } else {
            Ok(Self { image })
        }
    }

    /// Borrow a full-sized image.
    #[must_use]
    pub fn from_memory_image(image: &'a MemoryImage) -> Self {
        Self {
            image: image.as_bytes(),
        }
    }

    /// Decode one slot.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`StoredChannelEntry::from_records`].
    pub fn entry(&self, index: PhysicalChannel) -> Result<StoredChannelEntry, ValidationError> {
        let slice =
            |start: usize, len: usize| self.image.get(start..start + len).unwrap_or_default();
        StoredChannelEntry::from_records(
            index,
            slice(index.flag_address(), FLAG_RECORD_SIZE),
            slice(index.data_address(), CHANNEL_RECORD_SIZE),
            slice(index.name_address(), CHANNEL_NAME_SIZE),
        )
    }

    /// Every slot in index order.
    pub fn entries(
        &self,
    ) -> impl Iterator<Item = Result<StoredChannelEntry, ValidationError>> + '_ {
        PhysicalChannel::all().map(move |index| self.entry(index))
    }

    /// The slots whose flag marks them programmed, in index order.
    pub fn programmed(
        &self,
    ) -> impl Iterator<Item = Result<StoredChannelEntry, ValidationError>> + '_ {
        self.entries().filter(|entry| {
            entry.as_ref().is_ok_and(|entry| entry.channel().is_some()) || entry.is_err()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CatMemoryChannelRecord, StoredChannel, StoredChannelFlag};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn put(image: &mut [u8], offset: usize, bytes: &[u8]) -> TestResult {
        image
            .get_mut(offset..offset + bytes.len())
            .ok_or("offset inside the image")?
            .copy_from_slice(bytes);
        Ok(())
    }

    type Slot = (PhysicalChannel, [u8; 4], [u8; 40], [u8; 16]);

    fn image_with(entries: &[Slot]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut image = vec![0; ChannelAccess::REQUIRED_LENGTH];
        let mut erased = [0xFF; 40];
        erased[15..].copy_from_slice(b"CQCQCQ\0\0DIRECT\0\0DIRECT\0\0\0");
        for index in PhysicalChannel::all() {
            put(&mut image, index.flag_address(), &[0xFF, 0x00, 0x00, 0xFF])?;
            put(&mut image, index.data_address(), &erased)?;
        }
        for (index, flag, record, name) in entries {
            put(&mut image, index.flag_address(), flag)?;
            put(&mut image, index.data_address(), record)?;
            put(&mut image, index.name_address(), name)?;
        }
        Ok(image)
    }

    #[test]
    fn programmed_slots_are_found_and_decoded() -> TestResult {
        let (_, cat) = crate::protocol::channel::parse_memory_channel(
            "997,0446000000,0005000000,9,9,0,0,1,0,0,1,0,1,12,15,010,3,W1AW,1,42,0",
        )?;
        let record = StoredChannel::from_cat_record(&cat)?.to_bytes();
        let flag = StoredChannelFlag::from_bytes(&[0x08, 0x00, 0x09, 0x00])?.to_bytes();
        let image = image_with(&[(
            PhysicalChannel::new(997)?,
            flag,
            record,
            *b"Test UHF\0\0\0\0\0\0\0\0",
        )])?;
        let access = ChannelAccess::new(&image)?;
        let programmed: Vec<_> = access.programmed().collect::<Result<_, _>>()?;
        assert_eq!(programmed.len(), 1);
        let entry = programmed.first().ok_or("one programmed slot")?;
        assert_eq!(entry.index().index(), 997);
        assert_eq!(entry.name().text(), "Test UHF");
        let channel = entry.channel().ok_or("programmed")?;
        assert_eq!(channel.cat_record()?, cat.channel);
        assert_eq!(entry.to_string(), "997: 446.000000 MHz FM \"Test UHF\"");
        assert!(access.entry(PhysicalChannel::new(0)?)?.channel().is_none());
        assert_eq!(
            access.entries().count(),
            usize::from(PHYSICAL_CHANNEL_COUNT)
        );
        let _: &CatMemoryChannelRecord = &cat;
        Ok(())
    }

    #[test]
    fn short_images_and_corrupt_records_are_errors() -> TestResult {
        let short = vec![0; ChannelAccess::REQUIRED_LENGTH - 1];
        assert!(matches!(
            ChannelAccess::new(&short),
            Err(ValidationError::ImageTooShortForChannels { .. })
        ));
        let mut image = image_with(&[])?;
        let index = PhysicalChannel::new(5)?;
        put(&mut image, index.flag_address(), &[0x05, 0x00, 0x00, 0x00])?;
        let access = ChannelAccess::new(&image)?;
        assert!(
            access.entry(index).is_err(),
            "a programmed flag over an erased record must not decode silently"
        );
        assert_eq!(
            access.programmed().count(),
            1,
            "the failing slot is reported, not skipped"
        );
        let regions = ChannelAccess::regions()?;
        assert_eq!(regions[0].start(), 0x2000);
        assert_eq!(regions[0].end(), 0x2000 + 4 * 1102);
        assert_eq!(regions[1].start(), 0x4000);
        assert_eq!(regions[1].end(), 0xF778 + 40);
        assert_eq!(regions[2].end() as usize, ChannelAccess::REQUIRED_LENGTH);
        Ok(())
    }
}
