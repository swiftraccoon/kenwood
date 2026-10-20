//! A captured configuration: the identity it was read from, the pages that
//! were read and their bytes, with a persisted form.
//!
//! A [`Configuration`] checks captured coverage before typed reads. Its
//! explicitly named [`Configuration::image_with_uncaptured_fill`] escape
//! exposes `0xFF` for unread bytes. Its persisted form round-trips the identity, the pages and
//! their bytes exactly: the eight bytes `TMD750CF`, the format version `1`,
//! the model, firmware and radio-type strings (each a length byte then ASCII),
//! the page count (`u32`), each page's address (`u32`), length (`u16`, 1 to
//! 256) and bytes, and a CRC-32 (IEEE 802.3, reflected, polynomial
//! `0xEDB88320`) of every byte before it; integers are little-endian.

use std::collections::BTreeMap;

use crate::error::ValidationError;
use crate::memory::{
    ConfigurationError, ConfigurationFormatError, MemoryImage, StandardConfiguration,
    StandardConfigurationDiff,
};
use crate::protocol::mcp::regions::menu_regions;
use crate::radio::Identity;
use crate::radio::backup::McpBackupReport;
use crate::radio::menu::MenuFieldSnapshot;
use crate::types::{Address, FirmwareIdentity, PAGE_SIZE, Page, RadioModel, RadioType, Region};

#[cfg(test)]
#[path = "captured_configuration_tests.rs"]
mod tests;

const MAGIC: &[u8; 8] = b"TMD750CF";
const FORMAT_VERSION: u8 = 1;
// At most one nonoverlapping one-byte page per image byte; each header is six
// bytes, plus bounded model/firmware/type metadata and the trailing checksum.
const MAX_STORED_BYTES: usize =
    8 + 1 + (1 + 7) + (1 + 8) + (1 + 255) + 4 + 7 * crate::types::IMAGE_LENGTH + 4;

/// The bytes a backup or probe read, with the identity they were read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configuration {
    identity: Identity,
    image: MemoryImage,
    pages: Vec<Page>,
    coverage: Vec<Region>,
}

impl Configuration {
    /// Collect captured pages; their order does not matter.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::PageLength`] for bytes that do not fill
    /// their page and [`ConfigurationError::PageOverlap`] for a page that
    /// overlaps another.
    pub fn from_pages(
        identity: Identity,
        pages: impl IntoIterator<Item = (Page, Vec<u8>)>,
    ) -> Result<Self, ConfigurationError> {
        Self::from_page_results(identity, pages.into_iter().map(Ok))
    }

    fn from_page_results(
        identity: Identity,
        pages: impl IntoIterator<Item = Result<(Page, Vec<u8>), ConfigurationError>>,
    ) -> Result<Self, ConfigurationError> {
        if identity.radio_type.as_str().len() > usize::from(u8::MAX) {
            return Err(ConfigurationError::RadioTypeTooLong {
                length: identity.radio_type.as_str().len(),
            });
        }
        let mut ordered: BTreeMap<u32, (Page, Vec<u8>)> = BTreeMap::new();
        for (index, candidate) in pages.into_iter().enumerate() {
            let (page, data) = candidate?;
            if data.len() != page.len() {
                return Err(ConfigurationError::PageLength {
                    index,
                    page,
                    actual: data.len(),
                });
            }
            let start = page.address().as_u32();
            if ordered
                .range(..=start)
                .next_back()
                .is_some_and(|(_, (previous, _))| previous.end() > start)
                || ordered
                    .range(start..)
                    .next()
                    .is_some_and(|(&next, _)| next < page.end())
            {
                return Err(ConfigurationError::PageOverlap { page });
            }
            let _previous = ordered.insert(start, (page, data));
        }
        let mut bytes = MemoryImage::blank().into_bytes();
        let mut read = Vec::with_capacity(ordered.len());
        for (page, data) in ordered.into_values() {
            let start = page.address().as_usize();
            for (destination, source) in bytes.iter_mut().skip(start).zip(data) {
                *destination = source;
            }
            read.push(page);
        }
        let coverage = merge(&read)?;
        Ok(Self {
            identity,
            image: MemoryImage::from_bytes(bytes)?,
            pages: read,
            coverage,
        })
    }

    /// Collect the pages of a backup report, its position memory pages
    /// included.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::NoIdentity`] for a report that never
    /// identified the radio, and the [`Self::from_pages`] errors.
    pub fn from_backup(report: &McpBackupReport) -> Result<Self, ConfigurationError> {
        let identity = report
            .identity
            .clone()
            .ok_or(ConfigurationError::NoIdentity)?;
        Self::from_pages(
            identity,
            report
                .segments
                .iter()
                .chain(&report.position_memory)
                .map(|segment| (segment.page, segment.data.clone())),
        )
    }

    /// The identity the pages were read from.
    #[must_use]
    pub const fn identity(&self) -> &Identity {
        &self.identity
    }

    /// The pages read, in address order.
    #[must_use]
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// The regions read, merged and in address order.
    #[must_use]
    pub fn coverage(&self) -> &[Region] {
        &self.coverage
    }

    /// Whether every byte of `region` was read.
    #[must_use]
    pub fn covers(&self, region: Region) -> bool {
        self.coverage
            .iter()
            .any(|covered| covered.contains_region(region))
    }

    /// The bytes of `region`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::Uncovered`] when a byte was not read.
    pub fn bytes(&self, region: Region) -> Result<&[u8], ConfigurationError> {
        let uncovered = ConfigurationError::Uncovered {
            start: region.start(),
            end: region.end(),
        };
        if !self.covers(region) {
            return Err(uncovered);
        }
        self.image
            .as_bytes()
            .get(region.start() as usize..region.end() as usize)
            .ok_or(uncovered)
    }

    /// The full image, for a decoder of `region`, after checking that every
    /// byte of it was read; bytes outside the coverage are `0xFF`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::Uncovered`] when a byte was not read.
    pub fn image_with_uncaptured_fill(
        &self,
        region: Region,
    ) -> Result<&MemoryImage, ConfigurationError> {
        let _bytes = self.bytes(region)?;
        Ok(&self.image)
    }

    /// Whether the whole standard backup schedule was read.
    #[must_use]
    pub fn is_standard(&self) -> bool {
        menu_regions().into_iter().all(|region| self.covers(region))
    }

    /// The standard schedule's pages, for comparison.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::Uncovered`] for the first standard
    /// region not read.
    pub fn standard(&self) -> Result<StandardConfiguration<'_>, ConfigurationError> {
        let pages = menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| Ok((page, self.bytes(page.region())?)))
            .collect::<Result<Vec<_>, ConfigurationError>>()?;
        StandardConfiguration::new(&self.identity, pages)
    }

    /// The standard schedule's pages as a coverage-checked menu snapshot.
    ///
    /// # Errors
    ///
    /// Returns the [`Self::standard`] errors and the snapshot's schema
    /// errors.
    pub fn menu_snapshot(&self) -> Result<MenuFieldSnapshot, ConfigurationError> {
        let pages = menu_regions()
            .into_iter()
            .flat_map(Region::pages)
            .map(|page| Ok((page, self.bytes(page.region())?.to_vec())))
            .collect::<Result<Vec<_>, ConfigurationError>>()?;
        Ok(MenuFieldSnapshot::from_pages(pages)?)
    }

    /// Compare the standard schedule byte for byte with a later capture.
    ///
    /// # Errors
    ///
    /// Returns the [`Self::standard`] errors of either capture and
    /// [`ConfigurationError::IdentityMismatch`].
    pub fn diff(&self, after: &Self) -> Result<StandardConfigurationDiff, ConfigurationError> {
        StandardConfigurationDiff::between(&self.standard()?, &after.standard()?)
    }

    /// The persisted form described in the module documentation.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut stored = MAGIC.to_vec();
        stored.push(FORMAT_VERSION);
        for text in [
            self.identity.model.to_string(),
            self.identity.firmware.to_string(),
            self.identity.radio_type.to_string(),
        ] {
            stored.push(
                u8::try_from(text.len())
                    .unwrap_or_else(|_| unreachable!("capture identity length was checked")),
            );
            stored.extend_from_slice(text.as_bytes());
        }
        stored.extend_from_slice(
            &u32::try_from(self.pages.len())
                .unwrap_or_else(|_| unreachable!("nonoverlapping image pages fit u32"))
                .to_le_bytes(),
        );
        for page in &self.pages {
            stored.extend_from_slice(&page.address().as_u32().to_le_bytes());
            stored.extend_from_slice(
                &u16::try_from(page.len())
                    .unwrap_or_else(|_| unreachable!("page length is at most 256"))
                    .to_le_bytes(),
            );
            stored.extend_from_slice(
                self.bytes(page.region()).unwrap_or_else(|_| {
                    unreachable!("retained page has complete capture coverage")
                }),
            );
        }
        let checksum = crc32(&stored);
        stored.extend_from_slice(&checksum.to_le_bytes());
        stored
    }

    /// Read the persisted form.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigurationError::Format`] for a wrong magic or version,
    /// truncated input, trailing bytes, an invalid identity string or a page
    /// outside the image, [`ConfigurationError::Checksum`] when the CRC
    /// differs, and the [`Self::from_pages`] errors.
    pub fn from_bytes(stored: &[u8]) -> Result<Self, ConfigurationError> {
        if stored.len() > MAX_STORED_BYTES {
            return Err(ConfigurationFormatError::TooLong {
                length: stored.len(),
                maximum: MAX_STORED_BYTES,
            }
            .into());
        }
        let outside = |invalid: ValidationError| {
            ConfigurationError::from(ConfigurationFormatError::InvalidPage(invalid))
        };
        let (body, checksum) = stored
            .split_last_chunk::<4>()
            .ok_or(ConfigurationFormatError::UnexpectedEnd)?;
        let mut reader = Reader { rest: body };
        if reader.take(MAGIC.len())? != MAGIC {
            return Err(ConfigurationFormatError::WrongMagic.into());
        }
        if reader.take(1)? != [FORMAT_VERSION] {
            return Err(ConfigurationFormatError::UnsupportedVersion.into());
        }
        let computed = crc32(body);
        let stated = u32::from_le_bytes(*checksum);
        if stated != computed {
            return Err(ConfigurationError::Checksum { stated, computed });
        }
        let identity = Identity {
            model: RadioModel::try_from(reader.text()?).map_err(invalid_identity)?,
            firmware: FirmwareIdentity::new(reader.text()?).map_err(invalid_identity)?,
            radio_type: RadioType::new(reader.text()?).map_err(invalid_identity)?,
        };
        let count = u32::from_le_bytes(reader.array()?);
        let captured = Self::from_page_results(
            identity,
            (0..count).map(|_| {
                let address = Address::new(u32::from_le_bytes(reader.array()?)).map_err(outside)?;
                let length = usize::from(u16::from_le_bytes(reader.array()?));
                if length == 0 || length > PAGE_SIZE {
                    return Err(ConfigurationFormatError::InvalidPageLength.into());
                }
                let page = Page::new(address, length).map_err(outside)?;
                Ok((page, reader.take(length)?.to_vec()))
            }),
        )?;
        if !reader.rest.is_empty() {
            return Err(ConfigurationFormatError::TrailingBytes.into());
        }
        Ok(captured)
    }
}

fn invalid_identity(error: ValidationError) -> ConfigurationError {
    ConfigurationFormatError::InvalidIdentity(error).into()
}

/// Sequential reads from the persisted form.
struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], ConfigurationError> {
        let (taken, rest) = self
            .rest
            .split_at_checked(length)
            .ok_or(ConfigurationFormatError::UnexpectedEnd)?;
        self.rest = rest;
        Ok(taken)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ConfigurationError> {
        let taken = self.take(N)?;
        taken
            .try_into()
            .map_err(|_short| ConfigurationFormatError::UnexpectedEnd.into())
    }

    fn text(&mut self) -> Result<&'a str, ConfigurationError> {
        let [length] = self.array::<1>()?;
        std::str::from_utf8(self.take(usize::from(length))?)
            .map_err(|error| ConfigurationFormatError::InvalidUtf8(error).into())
    }
}

/// Regions spanned by sorted, non-overlapping `pages`, adjacent ones merged.
fn merge(pages: &[Page]) -> Result<Vec<Region>, ConfigurationError> {
    let mut regions: Vec<Region> = Vec::new();
    for page in pages {
        let region = page.region();
        match regions.last_mut() {
            Some(last) if last.end() == region.start() => {
                *last = Region::new(last.start(), region.end())?;
            }
            _ => regions.push(region),
        }
    }
    Ok(regions)
}

/// CRC-32 (IEEE 802.3): reflected, polynomial `0xEDB88320`, initial and final
/// value `0xFFFFFFFF`.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}
