//! `.d750` files: a 256-byte header followed by the memory image.
//!
//! The official program accepts two lengths: the full 1,929,472-byte image,
//! or only the bytes before the startup-screen area (393,216 bytes) when the
//! file was saved without it. Parsed headers are carried opaquely and reproduced
//! byte for byte. [`ConfigHeader::for_mcp_d750`] constructs the known blank-comment
//! header for the full file layout; it does not validate firmware compatibility.

use crate::error::FileError;
use crate::memory::MemoryImage;
use crate::types::{IMAGE_LENGTH, RadioType};

/// Header length.
pub const HEADER_SIZE: usize = 256;
/// First byte of the startup-screen area; the short file layout ends here.
pub const STARTUP_SCREEN_START: usize = 393_216;
/// Length of a file that carries the whole image.
pub const FILE_SIZE_FULL: usize = HEADER_SIZE + IMAGE_LENGTH;
/// Length of a file saved without the startup-screen area.
pub const FILE_SIZE_WITHOUT_STARTUP_SCREEN: usize = HEADER_SIZE + STARTUP_SCREEN_START;

/// Which of the two accepted file lengths a file uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLayout {
    /// Header plus the whole image.
    Full,
    /// Header plus the bytes before the startup-screen area; the rest of
    /// the image is not in the file and reads as erased (`0xFF`).
    WithoutStartupScreen,
}

impl FileLayout {
    /// Image bytes stored in a file of this layout.
    #[must_use]
    pub const fn image_bytes(self) -> usize {
        match self {
            Self::Full => IMAGE_LENGTH,
            Self::WithoutStartupScreen => STARTUP_SCREEN_START,
        }
    }

    /// Total file length.
    #[must_use]
    pub const fn file_size(self) -> usize {
        HEADER_SIZE + self.image_bytes()
    }
}

/// Failure to construct a compatibility header from an opaque radio type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigHeaderError {
    /// The radio type is not three single-byte components separated by commas.
    #[error(
        "cannot construct a configuration header from radio type {payload:?}: expected three single-byte components separated by commas"
    )]
    UnsupportedRadioType {
        /// The unchanged radio-type payload that could not be represented.
        payload: String,
    },
}

/// A 256-byte header, preserved opaquely when read from an existing file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHeader([u8; HEADER_SIZE]);

impl ConfigHeader {
    /// Wrap raw header bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; HEADER_SIZE]) -> Self {
        Self(bytes)
    }

    /// Construct the blank-comment MCP-D750 V1.00 compatibility header.
    ///
    /// This header is for [`FileLayout::Full`]. It contains `MCP-D750` at offset
    /// 0, the format's application-version marker `V1.00` at offset 8, `TM-D750`
    /// at offset 16, zero at offset 32, and the three radio-type components at
    /// offset 128 without their separators. All other bytes are `0xFF`.
    /// The application marker describes the reproduced file format, not this
    /// library's version or the radio's firmware.
    ///
    /// No component meanings are inferred. For example, `K,2,1` is stored as
    /// the three ASCII bytes `K21`. Construction neither qualifies a memory
    /// schema for that radio nor proves that the official application can open
    /// the eventual file. It does not alter or validate an image, establish read
    /// coverage, or perform the official writer's image-byte normalization.
    /// A radio-backed export still requires complete intended-region coverage
    /// and a matching, validated template for bytes the radio read omits.
    /// Filling every unread byte with `0xFF` is not an established substitute.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigHeaderError::UnsupportedRadioType`] unless `radio_type`
    /// consists of exactly three non-comma printable ASCII bytes, separated by
    /// commas. Other shapes remain valid opaque [`RadioType`] values but have
    /// no supported representation in a newly constructed header.
    pub fn for_mcp_d750(radio_type: &RadioType) -> Result<Self, ConfigHeaderError> {
        let [first, b',', second, b',', third] = radio_type.as_str().as_bytes() else {
            return Err(ConfigHeaderError::UnsupportedRadioType {
                payload: radio_type.as_str().to_owned(),
            });
        };
        let components = [*first, *second, *third];
        if components.contains(&b',') {
            return Err(ConfigHeaderError::UnsupportedRadioType {
                payload: radio_type.as_str().to_owned(),
            });
        }

        let mut bytes = [0xFF; HEADER_SIZE];
        for (offset, value) in [
            (0, b"MCP-D750".as_slice()),
            (8, b"V1.00".as_slice()),
            (16, b"TM-D750".as_slice()),
            (32, &[0]),
            (128, components.as_slice()),
        ] {
            for (destination, source) in bytes.iter_mut().skip(offset).zip(value) {
                *destination = *source;
            }
        }
        Ok(Self(bytes))
    }

    /// The raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HEADER_SIZE] {
        &self.0
    }
}

/// A parsed `.d750` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioConfig {
    /// The header.
    pub header: ConfigHeader,
    /// The memory image (erased beyond the file's bytes for the short layout).
    pub image: MemoryImage,
    /// The length the file was read with and is written with.
    pub layout: FileLayout,
}

impl RadioConfig {
    /// Serialize back to the file layout it was read with.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.layout.file_size());
        bytes.extend_from_slice(self.header.as_bytes());
        bytes.extend_from_slice(
            self.image
                .as_bytes()
                .get(..self.layout.image_bytes())
                .unwrap_or_default(),
        );
        bytes
    }
}

/// Parse a `.d750` file of either accepted length.
///
/// # Errors
///
/// Returns [`FileError::Length`] for any other length.
pub fn parse_d750(data: &[u8]) -> Result<RadioConfig, FileError> {
    let layout = match data.len() {
        FILE_SIZE_FULL => FileLayout::Full,
        FILE_SIZE_WITHOUT_STARTUP_SCREEN => FileLayout::WithoutStartupScreen,
        actual => {
            return Err(FileError::Length {
                actual,
                expected: FILE_SIZE_FULL,
            });
        }
    };
    let length_error = || FileError::Length {
        actual: data.len(),
        expected: FILE_SIZE_FULL,
    };
    let (header, stored) = data.split_at(HEADER_SIZE);
    let header: [u8; HEADER_SIZE] = header.try_into().map_err(|_| length_error())?;
    let mut image = vec![0xFF; IMAGE_LENGTH];
    image
        .get_mut(..stored.len())
        .ok_or_else(length_error)?
        .copy_from_slice(stored);
    let image = MemoryImage::from_bytes(image).map_err(|_| length_error())?;
    Ok(RadioConfig {
        header: ConfigHeader::from_bytes(header),
        image,
        layout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample(image_bytes: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut data = vec![0x5A; HEADER_SIZE];
        for index in 0..image_bytes {
            data.push(u8::try_from(index % 251)?);
        }
        Ok(data)
    }

    #[test]
    fn full_files_round_trip() -> TestResult {
        let data = sample(IMAGE_LENGTH)?;
        let config = parse_d750(&data)?;
        assert_eq!(config.layout, FileLayout::Full);
        assert_eq!(config.header.as_bytes().first().copied(), Some(0x5A));
        // 1000 % 251 == 247
        assert_eq!(config.image.as_bytes().get(1000).copied(), Some(247));
        assert_eq!(config.to_bytes(), data);
        Ok(())
    }

    #[test]
    fn short_files_read_as_erased_past_their_bytes_and_round_trip() -> TestResult {
        let data = sample(STARTUP_SCREEN_START)?;
        let config = parse_d750(&data)?;
        assert_eq!(config.layout, FileLayout::WithoutStartupScreen);
        assert_eq!(
            config.image.as_bytes().get(STARTUP_SCREEN_START).copied(),
            Some(0xFF)
        );
        assert_eq!(config.to_bytes(), data);
        Ok(())
    }

    #[test]
    fn other_lengths_are_rejected() -> TestResult {
        let data = sample(IMAGE_LENGTH)?;
        let truncated = data.get(..FILE_SIZE_FULL - 1).ok_or("slice")?;
        let short = parse_d750(truncated);
        assert!(
            matches!(
                short,
                Err(FileError::Length { actual, expected: FILE_SIZE_FULL }) if actual == FILE_SIZE_FULL - 1
            ),
            "{short:?}"
        );
        Ok(())
    }

    #[test]
    fn constructed_header_pins_every_byte_without_touching_an_image() -> TestResult {
        let header = ConfigHeader::for_mcp_d750(&RadioType::new("K,2,1")?)?;
        let mut expected = [0xFF; HEADER_SIZE];
        expected
            .get_mut(..8)
            .ok_or("product field")?
            .copy_from_slice(b"MCP-D750");
        expected
            .get_mut(8..13)
            .ok_or("version field")?
            .copy_from_slice(b"V1.00");
        expected
            .get_mut(16..23)
            .ok_or("model field")?
            .copy_from_slice(b"TM-D750");
        *expected.get_mut(32).ok_or("reserved byte")? = 0;
        expected
            .get_mut(128..131)
            .ok_or("type field")?
            .copy_from_slice(b"K21");
        assert_eq!(header.as_bytes(), &expected);
        Ok(())
    }

    #[test]
    fn constructed_header_preserves_other_single_byte_type_components() -> TestResult {
        let header = ConfigHeader::for_mcp_d750(&RadioType::new("J,0,1")?)?;
        assert_eq!(header.as_bytes().get(128..131), Some(b"J01".as_slice()));
        Ok(())
    }

    #[test]
    fn constructed_header_rejects_unrepresentable_opaque_type_shapes() -> TestResult {
        for payload in ["J", "K,22,1", "K,2", "K,2,1,0", ",,2,1", "K,,,1", "K,2,,"] {
            let result = ConfigHeader::for_mcp_d750(&RadioType::new(payload)?);
            assert_eq!(
                result,
                Err(ConfigHeaderError::UnsupportedRadioType {
                    payload: payload.to_owned(),
                }),
            );
        }
        Ok(())
    }
}
