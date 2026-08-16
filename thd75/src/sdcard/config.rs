//! Parser for `.d75` configuration files.
//!
//! These files contain the complete radio configuration and can be
//! saved (Menu No. 800) and loaded (Menu No. 810) from the microSD card.
//! The data format is the same as the MCP-D75 PC application uses.
//!
//! Per Operating Tips §5.14.3: it is recommended to export and save the
//! configuration before performing a firmware upgrade, as the upgrade
//! process may reset settings.
//!
//! The file format is a 256-byte header followed by a raw memory image
//! identical to what the MCP programming protocol reads.
//!
//! # File Layout
//!
//! | Offset | Size | Content |
//! |--------|------|---------|
//! | 0x000 | 0x100 | File header (model ID, metadata) |
//! | 0x100 | ... | MCP memory image (settings, channels, names, etc.) |
//!
//! Channel data lives at `.d75 offset 0x100 + MCP offset` and follows the
//! canonical TH-D75 MCP page geometry in [`crate::protocol::programming`].

use super::SdCardError;
use crate::error::ValidationError;
use crate::memory::{ChannelAccess, ChannelWriter, MemoryImage};
use crate::protocol::programming;
use crate::types::{
    ChannelDisplayName, MemoryChannelBand, MemoryGroup, RegularChannel, StoredChannel,
};

pub use crate::memory::ChannelEntry;

/// Size of the `.d75` file header in bytes.
pub const HEADER_SIZE: usize = 0x100;

/// Size of the model identifier at the start of a `.d75` header.
pub const MODEL_IDENTIFIER_SIZE: usize = 16;

/// Maximum number of memory channels on the TH-D75.
pub const MAX_CHANNELS: usize = 1000;

/// Size of each channel memory entry in bytes.
const CHANNEL_ENTRY_SIZE: usize = programming::CHANNEL_RECORD_SIZE; // 40

/// `.d75` file offset to the channel flags table.
///
/// Each channel has a 4-byte flags entry. This precedes the channel
/// memory data in the file layout.
///
/// File offset = `HEADER_SIZE + 0x2000 = 0x2100`.
#[cfg(test)]
const CHANNEL_FLAGS_OFFSET: usize = HEADER_SIZE + 0x2000;

/// `.d75` file offset to the channel memory data section.
///
/// Each 256-byte MCP memgroup contains six 40-byte channel records followed
/// by 16 bytes of padding. Channel 6 therefore starts on the next page rather
/// than immediately after channel 5.
///
/// File offset = `HEADER_SIZE + 0x4000 = 0x4100`.
#[cfg(test)]
const CHANNEL_DATA_OFFSET: usize = HEADER_SIZE + 0x4000;

/// Size of each channel flags entry in bytes.
#[cfg(test)]
const CHANNEL_FLAGS_SIZE: usize = 4;

const _: () = assert!(
    CHANNEL_ENTRY_SIZE == StoredChannel::BYTE_SIZE,
    "StoredChannel size must match the canonical MCP channel record size"
);
const _: () = assert!(
    programming::CHANNELS_PER_MEMGROUP * CHANNEL_ENTRY_SIZE + programming::MEMGROUP_PADDING
        == programming::PAGE_SIZE,
    "MCP channel records and padding must exactly fill one page"
);

const TH_D75A_IDENTIFIER: [u8; MODEL_IDENTIFIER_SIZE] = *b"Data For TH-D75A";
const TH_D75E_IDENTIFIER: [u8; MODEL_IDENTIFIER_SIZE] = *b"Data For TH-D75E";
const TH_D75_IDENTIFIER: [u8; MODEL_IDENTIFIER_SIZE] = *b"Data For TH-D75\0";

/// Radio model identifier accepted by the `.d75` configuration format.
///
/// The on-disk field is exactly 16 bytes. The region-neutral identifier is
/// 15 ASCII bytes followed by one NUL byte; the regional identifiers occupy
/// all 16 bytes and have no terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigFileModel {
    /// Americas model (`Data For TH-D75A`).
    ThD75A,
    /// European model (`Data For TH-D75E`).
    ThD75E,
    /// Region-neutral model (`Data For TH-D75` followed by NUL padding).
    RegionNeutral,
}

impl ConfigFileModel {
    /// Return the human-readable model identifier without padding.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ThD75A => "Data For TH-D75A",
            Self::ThD75E => "Data For TH-D75E",
            Self::RegionNeutral => "Data For TH-D75",
        }
    }

    /// Return the exact 16 bytes stored in a `.d75` header.
    #[must_use]
    pub const fn identifier(self) -> [u8; MODEL_IDENTIFIER_SIZE] {
        match self {
            Self::ThD75A => TH_D75A_IDENTIFIER,
            Self::ThD75E => TH_D75E_IDENTIFIER,
            Self::RegionNeutral => TH_D75_IDENTIFIER,
        }
    }
}

impl TryFrom<[u8; MODEL_IDENTIFIER_SIZE]> for ConfigFileModel {
    type Error = SdCardError;

    fn try_from(identifier: [u8; MODEL_IDENTIFIER_SIZE]) -> Result<Self, Self::Error> {
        match identifier {
            TH_D75A_IDENTIFIER => Ok(Self::ThD75A),
            TH_D75E_IDENTIFIER => Ok(Self::ThD75E),
            TH_D75_IDENTIFIER => Ok(Self::RegionNeutral),
            found => Err(SdCardError::InvalidModelIdentifier { found }),
        }
    }
}

impl std::fmt::Display for ConfigFileModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Return the `.d75` file offset for a channel's 40-byte MCP record.
///
/// MCP channel data is page-shaped, not a flat array: each page holds six
/// records and ends with 16 bytes of padding.
#[cfg(test)]
const fn channel_data_offset(channel: usize) -> usize {
    let memgroup = channel / programming::CHANNELS_PER_MEMGROUP;
    let slot = channel % programming::CHANNELS_PER_MEMGROUP;
    CHANNEL_DATA_OFFSET
        + memgroup * programming::PAGE_SIZE
        + slot * programming::CHANNEL_RECORD_SIZE
}

/// Parsed `.d75` configuration file header (256 bytes).
///
/// The header contains a validated model identifier and otherwise preserves
/// every byte verbatim. Metadata accessors read directly from the bytes that
/// [`write_config`] emits, so their views cannot diverge from the serialized
/// header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHeader {
    raw: [u8; HEADER_SIZE],
}

impl ConfigHeader {
    /// Create a zero-filled header with the selected model and metadata bytes.
    #[must_use]
    pub fn new(model: ConfigFileModel, version_bytes: [u8; 4]) -> Self {
        let mut raw = [0u8; HEADER_SIZE];
        raw[..MODEL_IDENTIFIER_SIZE].copy_from_slice(&model.identifier());
        raw[0x14..0x18].copy_from_slice(&version_bytes);
        Self { raw }
    }

    /// Return the validated model represented by the serialized identifier.
    #[must_use]
    pub fn model(&self) -> ConfigFileModel {
        let identifier = self
            .raw
            .first_chunk::<MODEL_IDENTIFIER_SIZE>()
            .copied()
            .unwrap_or_else(|| unreachable!("a fixed-size header contains its model identifier"));
        ConfigFileModel::try_from(identifier)
            .unwrap_or_else(|_| unreachable!("ConfigHeader construction validates its model"))
    }

    /// Return the four metadata bytes at offset `0x14`.
    ///
    /// These bytes are commonly described as a version or checksum, but their
    /// exact semantics are not known.
    #[must_use]
    pub const fn version_bytes(&self) -> [u8; 4] {
        [
            self.raw[0x14],
            self.raw[0x15],
            self.raw[0x16],
            self.raw[0x17],
        ]
    }

    /// Return the exact 256 serialized header bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HEADER_SIZE] {
        &self.raw
    }
}

impl TryFrom<[u8; HEADER_SIZE]> for ConfigHeader {
    type Error = SdCardError;

    fn try_from(raw: [u8; HEADER_SIZE]) -> Result<Self, Self::Error> {
        let identifier = raw
            .first_chunk::<MODEL_IDENTIFIER_SIZE>()
            .copied()
            .unwrap_or_else(|| unreachable!("a fixed-size header contains its model identifier"));
        let _validated_model = ConfigFileModel::try_from(identifier)?;
        Ok(Self { raw })
    }
}

/// Complete, fixed-size radio configuration from a `.d75` file.
///
/// This is the top-level structure returned by [`parse_config`]. The channel
/// and settings views all borrow the same canonical [`MemoryImage`], so a
/// serialized configuration cannot diverge from a second cached copy.
#[derive(Debug, Clone)]
pub struct RadioConfig {
    header: ConfigHeader,
    memory_image: MemoryImage,
}

impl RadioConfig {
    /// Combine a validated header and exact-size MCP memory image.
    ///
    /// # Errors
    ///
    /// Returns [`SdCardError::ChannelParse`] if any regular-channel slot in
    /// the image has malformed flags, name bytes, channel data, or a
    /// programmed marker paired with an invalid receive frequency.
    pub fn new(header: ConfigHeader, memory_image: MemoryImage) -> Result<Self, SdCardError> {
        validate_regular_channels(&memory_image)?;
        Ok(Self {
            header,
            memory_image,
        })
    }

    /// Borrow the exact 256-byte `.d75` header.
    #[must_use]
    pub const fn header(&self) -> &ConfigHeader {
        &self.header
    }

    /// Borrow the canonical fixed-size MCP memory image.
    #[must_use]
    pub const fn memory_image(&self) -> &MemoryImage {
        &self.memory_image
    }

    /// Mutably borrow the canonical fixed-size MCP memory image.
    ///
    /// Prefer its typed subsystem writers. Direct raw-byte mutation remains
    /// available for diagnostics, and subsequent typed reads report any
    /// malformed values rather than normalizing them.
    #[must_use]
    pub const fn memory_image_mut(&mut self) -> &mut MemoryImage {
        &mut self.memory_image
    }

    /// Access regular channels from the canonical memory image.
    #[must_use]
    pub fn channels(&self) -> ChannelAccess<'_> {
        self.memory_image.channels()
    }

    /// Mutate regular channels in the canonical memory image.
    #[must_use]
    pub fn channels_mut(&mut self) -> ChannelWriter<'_> {
        self.memory_image.channels_mut()
    }

    /// Consume the configuration and return its header and memory image.
    #[must_use]
    pub fn into_parts(self) -> (ConfigHeader, MemoryImage) {
        (self.header, self.memory_image)
    }

    /// Consume the configuration and return its memory image.
    ///
    /// This intentionally discards the `.d75` header. Use [`Self::into_parts`]
    /// when the image may later be serialized as a complete configuration.
    #[must_use]
    pub fn into_memory_image(self) -> MemoryImage {
        self.memory_image
    }
}

/// Parses a `.d75` configuration file from raw bytes.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the data is shorter than
/// the minimum required size, or [`SdCardError::InvalidModelIdentifier`]
/// if the header model is not recognised.
pub fn parse_config(data: &[u8]) -> Result<RadioConfig, SdCardError> {
    let expected_size = HEADER_SIZE + programming::TOTAL_SIZE;
    if data.len() < expected_size {
        return Err(SdCardError::FileTooSmall {
            expected: expected_size,
            actual: data.len(),
        });
    }
    if data.len() > expected_size {
        return Err(SdCardError::UnexpectedFileSize {
            file_type: ".d75 configuration",
            expected: expected_size,
            actual: data.len(),
        });
    }

    let header = parse_header(data)?;
    let raw_image = data
        .get(HEADER_SIZE..)
        .ok_or(SdCardError::FileTooSmall {
            expected: HEADER_SIZE,
            actual: data.len(),
        })?
        .to_vec();
    let memory_image =
        MemoryImage::from_raw(raw_image).map_err(|error| SdCardError::InvalidMemoryImage {
            detail: error.to_string(),
        })?;

    RadioConfig::new(header, memory_image)
}

/// Parse and validate the fixed-size `.d75` file header.
fn parse_header(data: &[u8]) -> Result<ConfigHeader, SdCardError> {
    let header_slice = data.get(..HEADER_SIZE).ok_or(SdCardError::FileTooSmall {
        expected: HEADER_SIZE,
        actual: data.len(),
    })?;
    let raw_header =
        <[u8; HEADER_SIZE]>::try_from(header_slice).map_err(|_| SdCardError::FileTooSmall {
            expected: HEADER_SIZE,
            actual: data.len(),
        })?;

    ConfigHeader::try_from(raw_header)
}

/// Validate all one thousand regular-channel slots in numerical order.
fn validate_regular_channels(memory_image: &MemoryImage) -> Result<(), SdCardError> {
    let channels = memory_image.channels();
    for number in RegularChannel::all() {
        let _validated_entry = channels
            .get(number)
            .map_err(|error| SdCardError::ChannelParse {
                index: number.as_raw(),
                detail: error.to_string(),
            })?;
    }
    Ok(())
}

/// Generates a `.d75` file from a [`RadioConfig`].
///
/// The output is the header concatenated with the configuration's canonical,
/// fixed-size memory image.
#[must_use]
pub fn write_config(config: &RadioConfig) -> Vec<u8> {
    serialize_config_parts(config.memory_image(), config.header())
}

/// Create a minimal valid `.d75` header for a supported model.
///
/// Useful for generating new configuration files from scratch.
#[must_use]
pub fn make_header(model: ConfigFileModel, version_bytes: [u8; 4]) -> ConfigHeader {
    ConfigHeader::new(model, version_bytes)
}

/// Creates an empty [`ChannelEntry`] for the given channel number.
#[must_use]
pub fn empty_channel(number: RegularChannel) -> ChannelEntry {
    ChannelEntry::empty(number)
}

/// Creates a [`ChannelEntry`] with the given stored channel data.
///
/// # Errors
///
/// Returns [`ValidationError`] if `name` is not a valid channel display name.
pub fn make_channel(
    number: RegularChannel,
    name: &str,
    stored_channel: StoredChannel,
    band: MemoryChannelBand,
    group: MemoryGroup,
) -> Result<ChannelEntry, ValidationError> {
    ChannelEntry::new_programmed(
        number,
        ChannelDisplayName::new(name)?,
        stored_channel,
        band,
        group,
        false,
    )
}

/// Serialize the two validated parts retained by [`RadioConfig`].
fn serialize_config_parts(image: &MemoryImage, header: &ConfigHeader) -> Vec<u8> {
    let raw = image.as_raw();
    let mut out = Vec::with_capacity(HEADER_SIZE + raw.len());
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(raw);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{StoredChannelFlag, frequency::Frequency};

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn synthetic_stored_channel(receive_frequency: Frequency) -> StoredChannel {
        let mut wire = [0_u8; StoredChannel::BYTE_SIZE];
        wire[..4].copy_from_slice(&receive_frequency.to_le_bytes());
        StoredChannel::from_bytes(&wire).unwrap_or_else(|error| {
            unreachable!("fixed all-zero synthetic channel record must decode: {error}")
        })
    }

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

    fn mark_regular_channels_empty(image: &mut [u8], flags_offset: usize) -> Result<(), BoxErr> {
        let flags_end = flags_offset + MAX_CHANNELS * CHANNEL_FLAGS_SIZE;
        let flags = image
            .get_mut(flags_offset..flags_end)
            .ok_or("regular-channel flag table is outside the test image")?;
        for flag in flags.chunks_exact_mut(CHANNEL_FLAGS_SIZE) {
            let marker = flag
                .first_mut()
                .ok_or("channel flag record has no marker byte")?;
            *marker = programming::FLAG_EMPTY;
        }
        Ok(())
    }

    #[test]
    fn model_identifiers_include_exact_padding() {
        assert_eq!(ConfigFileModel::ThD75A.identifier(), *b"Data For TH-D75A");
        assert_eq!(ConfigFileModel::ThD75E.identifier(), *b"Data For TH-D75E");
        assert_eq!(
            ConfigFileModel::RegionNeutral.identifier(),
            *b"Data For TH-D75\0"
        );
    }

    #[test]
    fn make_header_views_match_serialized_bytes() {
        let hdr = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
        assert_eq!(hdr.model(), ConfigFileModel::ThD75A);
        assert_eq!(hdr.version_bytes(), [0x95, 0xC4, 0x8F, 0x42]);
        assert_eq!(hdr.as_bytes().len(), HEADER_SIZE);
        assert_eq!(
            hdr.as_bytes().get(..MODEL_IDENTIFIER_SIZE),
            Some(ConfigFileModel::ThD75A.identifier().as_slice())
        );
        assert_eq!(
            hdr.as_bytes().get(0x14..0x18),
            Some([0x95, 0xC4, 0x8F, 0x42].as_slice())
        );
    }

    #[test]
    fn config_header_rejects_and_preserves_invalid_identifier_bytes() -> TestResult {
        let mut raw = *make_header(ConfigFileModel::RegionNeutral, [0; 4]).as_bytes();
        let padding = raw
            .get_mut(MODEL_IDENTIFIER_SIZE - 1)
            .ok_or("model identifier padding byte missing")?;
        *padding = 0xFF;
        let expected = *raw
            .first_chunk::<MODEL_IDENTIFIER_SIZE>()
            .ok_or("model identifier missing")?;

        let err = ConfigHeader::try_from(raw)
            .err()
            .ok_or("invalid model identifier should be rejected")?;
        assert_eq!(err, SdCardError::InvalidModelIdentifier { found: expected });
        Ok(())
    }

    #[test]
    fn empty_channel_defaults() -> TestResult {
        let ch = empty_channel(RegularChannel::new(42)?);
        assert_eq!(ch.number(), RegularChannel::new(42)?);
        assert_eq!(ch.flag(), StoredChannelFlag::empty());
        assert!(ch.name().is_empty());
        Ok(())
    }

    #[test]
    fn make_channel_marks_used() -> TestResult {
        let stored_channel = synthetic_stored_channel(Frequency::new(145_000_000));
        let ch = make_channel(
            RegularChannel::new(0)?,
            "2M RPT",
            stored_channel,
            MemoryChannelBand::Vhf,
            MemoryGroup::new(0)?,
        )?;
        assert!(ch.is_programmed());
        assert_eq!(ch.name().as_str(), "2M RPT");
        Ok(())
    }

    #[test]
    fn make_channel_rejects_zero_frequency() -> TestResult {
        let error = make_channel(
            RegularChannel::new(0)?,
            "empty",
            synthetic_stored_channel(Frequency::new(0)),
            MemoryChannelBand::Vhf,
            MemoryGroup::new(0)?,
        )
        .err()
        .ok_or("zero-frequency programmed channel should be rejected")?;
        assert!(matches!(
            error,
            ValidationError::IntegerOutOfRange { value: 0, .. }
        ));
        Ok(())
    }

    #[test]
    fn write_config_round_trip() -> TestResult {
        use crate::memory::MemoryImage;
        use crate::protocol::programming;

        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
        let mut raw = vec![0u8; programming::TOTAL_SIZE];
        mark_regular_channels_empty(&mut raw, CHANNEL_FLAGS_OFFSET - HEADER_SIZE)?;
        let config = RadioConfig::new(header, MemoryImage::from_raw(raw)?)?;
        let d75_bytes = write_config(&config);

        // The output should be header + image.
        assert_eq!(d75_bytes.len(), HEADER_SIZE + programming::TOTAL_SIZE);
        assert_eq!(
            d75_bytes.get(..HEADER_SIZE).ok_or("d75_bytes too short")?,
            config.header().as_bytes()
        );
        assert_eq!(
            d75_bytes.get(HEADER_SIZE..).ok_or("d75_bytes too short")?,
            config.memory_image().as_raw()
        );

        // Round-trip: parse it back and verify.
        let parsed = parse_config(&d75_bytes)?;
        assert_eq!(parsed.header().model(), ConfigFileModel::ThD75A);
        assert_eq!(parsed.header().version_bytes(), [0x95, 0xC4, 0x8F, 0x42]);
        assert_eq!(
            parsed.memory_image().as_raw().len(),
            d75_bytes.len() - HEADER_SIZE
        );
        Ok(())
    }

    #[test]
    fn write_config_preserves_every_opaque_header_byte_after_memory_mutation() -> TestResult {
        let mut raw_header = [0xA5; HEADER_SIZE];
        raw_header
            .get_mut(..MODEL_IDENTIFIER_SIZE)
            .ok_or("model identifier range missing from test header")?
            .copy_from_slice(&ConfigFileModel::ThD75E.identifier());
        let header = ConfigHeader::try_from(raw_header)?;

        let mut raw_memory = vec![0u8; programming::TOTAL_SIZE];
        mark_regular_channels_empty(&mut raw_memory, CHANNEL_FLAGS_OFFSET - HEADER_SIZE)?;
        let mut config = RadioConfig::new(header, MemoryImage::from_raw(raw_memory)?)?;
        let last_offset = programming::TOTAL_SIZE
            .checked_sub(1)
            .ok_or("memory image unexpectedly has no bytes")?;
        config
            .memory_image_mut()
            .write_region(last_offset, &[0x5A])?;

        let serialized = write_config(&config);
        assert_eq!(
            serialized
                .get(..HEADER_SIZE)
                .ok_or("serialized configuration is missing its header")?,
            raw_header.as_slice()
        );

        let parsed = parse_config(&serialized)?;
        assert_eq!(parsed.header().as_bytes(), &raw_header);
        assert_eq!(
            parsed
                .memory_image()
                .read_region(last_offset, 1)
                .ok_or("parsed memory image is missing its final byte")?,
            &[0x5A]
        );
        Ok(())
    }

    #[test]
    fn parse_config_rejects_trailing_bytes() -> TestResult {
        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
        let mut raw = vec![0u8; programming::TOTAL_SIZE];
        mark_regular_channels_empty(&mut raw, CHANNEL_FLAGS_OFFSET - HEADER_SIZE)?;
        let config = RadioConfig::new(header, MemoryImage::from_raw(raw)?)?;
        let mut data = write_config(&config);
        data.push(0xA5);

        assert!(matches!(
            parse_config(&data),
            Err(SdCardError::UnexpectedFileSize {
                file_type: ".d75 configuration",
                expected,
                actual,
            }) if expected == HEADER_SIZE + programming::TOTAL_SIZE
                && actual == expected + 1
        ));
        Ok(())
    }

    #[test]
    fn write_config_preserves_channel_data() -> TestResult {
        use crate::memory::MemoryImage;

        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);

        // Build a raw image with some nonzero data in the channel region.
        let mut raw = vec![0u8; programming::TOTAL_SIZE];
        mark_regular_channels_empty(&mut raw, CHANNEL_FLAGS_OFFSET - HEADER_SIZE)?;
        // Put a marker byte at offset 0x4000 (channel data section in the body).
        if raw.len() > 0x4000 {
            set_byte(&mut raw, 0x4000, 0xAB)?;
        }
        let config = RadioConfig::new(header, MemoryImage::from_raw(raw)?)?;
        let d75_bytes = write_config(&config);

        // The marker should be at file offset HEADER_SIZE + 0x4000.
        assert_eq!(
            *d75_bytes
                .get(HEADER_SIZE + 0x4000)
                .ok_or("d75_bytes too short")?,
            0xAB
        );
        Ok(())
    }

    #[test]
    fn parse_config_skips_memgroup_padding_before_channel_six() -> TestResult {
        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
        let mut data = vec![0u8; HEADER_SIZE + programming::TOTAL_SIZE];
        write_slice(&mut data, 0, header.as_bytes())?;
        mark_regular_channels_empty(&mut data, CHANNEL_FLAGS_OFFSET)?;

        let channel_five = synthetic_stored_channel(Frequency::new(446_000_000));
        let channel_six = synthetic_stored_channel(Frequency::new(145_600_000));
        write_slice(&mut data, channel_data_offset(5), &channel_five.to_bytes())?;
        write_slice(&mut data, channel_data_offset(6), &channel_six.to_bytes())?;
        write_slice(
            &mut data,
            CHANNEL_FLAGS_OFFSET + 5 * CHANNEL_FLAGS_SIZE,
            &[programming::FLAG_UHF, 0, 0, 0xFF],
        )?;
        write_slice(
            &mut data,
            CHANNEL_FLAGS_OFFSET + 6 * CHANNEL_FLAGS_SIZE,
            &[programming::FLAG_VHF, 0, 0, 0xFF],
        )?;

        let padding_start = channel_data_offset(5) + CHANNEL_ENTRY_SIZE;
        assert_eq!(
            channel_data_offset(6) - padding_start,
            programming::MEMGROUP_PADDING
        );
        assert_eq!(
            data.get(padding_start..channel_data_offset(6))
                .ok_or("channel memgroup padding missing")?,
            &[0u8; programming::MEMGROUP_PADDING]
        );

        let parsed = parse_config(&data)?;
        let parsed_five = parsed.channels().get(RegularChannel::new(5)?)?;
        let parsed_six = parsed.channels().get(RegularChannel::new(6)?)?;
        assert_eq!(
            parsed_five
                .programmed()
                .ok_or("channel 5 should be programmed")?
                .receive_frequency
                .as_hz(),
            446_000_000
        );
        assert_eq!(
            parsed_six
                .programmed()
                .ok_or("channel 6 should be programmed")?
                .receive_frequency
                .as_hz(),
            145_600_000
        );
        assert!(parsed_five.is_programmed());
        assert!(parsed_six.is_programmed());
        Ok(())
    }

    #[test]
    fn write_config_preserves_memgroup_padding_before_channel_six() -> TestResult {
        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);
        let mut raw_image = vec![0u8; programming::TOTAL_SIZE];
        mark_regular_channels_empty(&mut raw_image, CHANNEL_FLAGS_OFFSET - HEADER_SIZE)?;
        let padding_start = channel_data_offset(5) + CHANNEL_ENTRY_SIZE;
        let padding_end = channel_data_offset(6);
        let padding_body_start = padding_start - HEADER_SIZE;
        let padding_body_end = padding_end - HEADER_SIZE;
        raw_image
            .get_mut(padding_body_start..padding_body_end)
            .ok_or("raw image channel padding missing")?
            .fill(0xA5);

        let channel_five = synthetic_stored_channel(Frequency::new(446_000_000));
        let channel_six = synthetic_stored_channel(Frequency::new(145_600_000));
        let entry_five = make_channel(
            RegularChannel::new(5)?,
            "PAGE ZERO",
            channel_five.clone(),
            MemoryChannelBand::Uhf,
            MemoryGroup::new(0)?,
        )?;
        let entry_six = make_channel(
            RegularChannel::new(6)?,
            "PAGE ONE",
            channel_six.clone(),
            MemoryChannelBand::Vhf,
            MemoryGroup::new(0)?,
        )?;
        let mut memory_image = MemoryImage::from_raw(raw_image)?;
        {
            let mut channels = memory_image.channels_mut();
            channels.set(&entry_five)?;
            channels.set(&entry_six)?;
        }
        let config = RadioConfig::new(header, memory_image)?;

        let written = write_config(&config);
        assert_eq!(
            written
                .get(channel_data_offset(5)..channel_data_offset(5) + CHANNEL_ENTRY_SIZE)
                .ok_or("written channel 5 missing")?,
            &channel_five.to_bytes()
        );
        assert_eq!(
            written
                .get(channel_data_offset(6)..channel_data_offset(6) + CHANNEL_ENTRY_SIZE)
                .ok_or("written channel 6 missing")?,
            &channel_six.to_bytes()
        );
        assert_eq!(
            written
                .get(padding_start..padding_end)
                .ok_or("written channel padding missing")?,
            &[0xA5; programming::MEMGROUP_PADDING]
        );

        let parsed = parse_config(&written)?;
        assert_eq!(
            parsed
                .channels()
                .get(RegularChannel::new(5)?)?
                .programmed()
                .ok_or("channel 5 should be programmed")?
                .receive_frequency
                .as_hz(),
            446_000_000
        );
        assert_eq!(
            parsed
                .channels()
                .get(RegularChannel::new(6)?)?
                .programmed()
                .ok_or("channel 6 should be programmed")?
                .receive_frequency
                .as_hz(),
            145_600_000
        );
        Ok(())
    }

    #[test]
    fn parse_config_channel_parse_error() -> TestResult {
        let header = make_header(ConfigFileModel::ThD75A, [0x95, 0xC4, 0x8F, 0x42]);

        // Build a valid .d75 file, then corrupt channel 0's step_size byte.
        let mut d75_data = vec![0u8; HEADER_SIZE + programming::TOTAL_SIZE];
        write_slice(&mut d75_data, 0, header.as_bytes())?;

        // Channel 0 data starts at file offset CHANNEL_DATA_OFFSET.
        // Give it a nonzero RX frequency so it's "used" and parsed.
        let ch0_offset = CHANNEL_DATA_OFFSET;
        write_slice(&mut d75_data, ch0_offset, &[0x01, 0x00, 0x00, 0x00])?;
        // Byte 0x08 of the channel record: high nibble = step_size.
        // Value 0xF0 => step_size = 15 which is out of range.
        set_byte(&mut d75_data, ch0_offset + 0x08, 0xF0)?;

        let err = parse_config(&d75_data)
            .err()
            .ok_or("expected ChannelParse error but got Ok")?;
        assert!(
            matches!(err, SdCardError::ChannelParse { index: 0, .. }),
            "expected ChannelParse for index 0, got {err:?}"
        );
        Ok(())
    }
}
