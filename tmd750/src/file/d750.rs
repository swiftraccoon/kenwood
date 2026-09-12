//! Validated `.d750` containers with exact stored-byte coverage.
//!
//! A 256-byte header declares either the full 1,929,472-byte image or the
//! 393,216-byte prefix before the startup-screen area. The signature, model
//! marker, reserved byte, and exact signature-selected length are validated.
//! All other header bytes and every stored image byte are preserved exactly.
//! Short files retain only their actual bytes; no erased tail is invented.
//!
//! These are container checks, not firmware, radio-type, or settings validation.
//! Parsing and serialization neither normalize the image nor establish radio
//! read coverage, application acceptance, or permission to write any bytes.

use crate::error::FileError;
use crate::types::{IMAGE_LENGTH, RadioType};

/// Header length.
pub const HEADER_SIZE: usize = 256;
/// First byte of the startup-screen area; the short file layout ends here.
pub const STARTUP_SCREEN_START: usize = 393_216;
/// Length of a file that carries the whole image.
pub const FILE_SIZE_FULL: usize = HEADER_SIZE + IMAGE_LENGTH;
/// Length of a file saved without the startup-screen area.
pub const FILE_SIZE_WITHOUT_STARTUP_SCREEN: usize = HEADER_SIZE + STARTUP_SCREEN_START;

const FULL_SIGNATURE: &[u8; 8] = b"MCP-D750";
const MODEL_MARKER: &[u8; 7] = b"TM-D750";

/// Image coverage selected by a validated configuration header's signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLayout {
    /// `MCP-D750` at header offset zero: header plus the whole image.
    Full,
    /// `TM-D750` at header offset zero: only the image prefix before the
    /// startup-screen area. Bytes beyond the prefix are absent, not erased.
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

/// A validated 256-byte header with an immutable, signature-selected layout.
///
/// [`TryFrom`] checks the known signature, `TM-D750` model marker at offset 16,
/// and zero reserved byte at offset 32. The short signature occupies seven
/// bytes; its eighth byte remains opaque. Version, radio-type, comment, and
/// other metadata bytes are retained without interpreting their semantics.
/// Header validation does not qualify a firmware layout or a particular radio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHeader {
    raw: [u8; HEADER_SIZE],
    layout: FileLayout,
}

impl ConfigHeader {
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
    /// Returns [`FileError::UnsupportedRadioType`] unless `radio_type` consists
    /// of exactly three non-comma graphic ASCII bytes, separated by commas.
    /// Other shapes remain valid opaque [`RadioType`] values but have no
    /// supported representation in a newly constructed header.
    pub fn for_mcp_d750(radio_type: &RadioType) -> Result<Self, FileError> {
        let [first, b',', second, b',', third] = radio_type.as_str().as_bytes() else {
            return Err(FileError::UnsupportedRadioType {
                payload: radio_type.as_str().to_owned(),
            });
        };
        let components = [*first, *second, *third];
        if components.contains(&b',') {
            return Err(FileError::UnsupportedRadioType {
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
        Ok(Self {
            raw: bytes,
            layout: FileLayout::Full,
        })
    }

    /// Every original header byte, including uninterpreted metadata.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; HEADER_SIZE] {
        &self.raw
    }

    /// The layout declared by this header's validated signature.
    #[must_use]
    pub const fn layout(&self) -> FileLayout {
        self.layout
    }
}

impl TryFrom<[u8; HEADER_SIZE]> for ConfigHeader {
    type Error = FileError;

    fn try_from(raw: [u8; HEADER_SIZE]) -> Result<Self, Self::Error> {
        let signature = raw
            .first_chunk::<8>()
            .copied()
            .unwrap_or_else(|| unreachable!("a fixed-size header contains its signature"));
        let layout = if &signature == FULL_SIGNATURE {
            FileLayout::Full
        } else if signature.starts_with(MODEL_MARKER) {
            FileLayout::WithoutStartupScreen
        } else {
            return Err(FileError::InvalidSignature { found: signature });
        };
        let model: [u8; 7] = raw
            .get(16..23)
            .and_then(|bytes| bytes.try_into().ok())
            .unwrap_or_else(|| unreachable!("a fixed-size header contains its model marker"));
        if &model != MODEL_MARKER {
            return Err(FileError::InvalidModel { found: model });
        }
        let reserved = raw
            .get(32)
            .copied()
            .unwrap_or_else(|| unreachable!("a fixed-size header contains its reserved byte"));
        if reserved != 0 {
            return Err(FileError::NonzeroReservedByte { value: reserved });
        }
        Ok(Self { raw, layout })
    }
}

/// A validated header and exactly the image bytes declared by its signature.
///
/// Header and payload lengths cannot diverge after construction. Short files
/// retain no synthetic startup-screen bytes and cannot be silently promoted
/// to full files. To construct a different container, supply a matching
/// validated header and complete payload to [`Self::new`].
///
/// ```compile_fail
/// use kenwood_tmd750::{FileLayout, RadioConfig};
/// fn change_layout(config: &mut RadioConfig) {
///     config.layout = FileLayout::Full;
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioConfig {
    header: ConfigHeader,
    image: Vec<u8>,
}

impl RadioConfig {
    /// Combine a validated header with its complete, exact-length image payload.
    ///
    /// This performs no settings interpretation, radio-type compatibility
    /// check, gap filling, or image normalization. The caller must establish
    /// the provenance and suitability of every supplied byte.
    ///
    /// # Errors
    ///
    /// Returns [`FileError::ImageLength`] unless `image` contains exactly the
    /// image length declared by `header.layout()`.
    pub fn new(header: ConfigHeader, image: Vec<u8>) -> Result<Self, FileError> {
        let expected = header.layout().image_bytes();
        if image.len() != expected {
            return Err(FileError::ImageLength {
                actual: image.len(),
                expected,
            });
        }
        Ok(Self { header, image })
    }

    /// Borrow the validated header and its unchanged opaque metadata.
    #[must_use]
    pub const fn header(&self) -> &ConfigHeader {
        &self.header
    }

    /// The immutable layout selected by the header signature.
    #[must_use]
    pub const fn layout(&self) -> FileLayout {
        self.header.layout()
    }

    /// Borrow only image bytes actually stored in the file, excluding the header.
    ///
    /// The slice length is [`FileLayout::image_bytes`] for this configuration's
    /// layout. Image address zero is slice offset zero; short-file bytes at or
    /// beyond [`STARTUP_SCREEN_START`] do not exist.
    #[must_use]
    pub fn image_bytes(&self) -> &[u8] {
        &self.image
    }

    /// Mutably borrow the fixed-length stored payload without changing coverage.
    ///
    /// Mutation does not validate setting values or qualify radio writes.
    /// Header bytes and payload length remain immutable, and serialization
    /// retains every supplied payload byte without normalization.
    #[must_use]
    pub fn image_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.image
    }

    /// Consume the container without changing its validated header or payload.
    #[must_use]
    pub fn into_parts(self) -> (ConfigHeader, Vec<u8>) {
        (self.header, self.image)
    }

    /// Serialize the validated header followed by the exact stored payload.
    ///
    /// Unmodified parsed files round-trip byte for byte. No image normalization,
    /// padding, gap filling, or layout conversion occurs.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.layout().file_size());
        bytes.extend_from_slice(self.header.as_bytes());
        bytes.extend_from_slice(&self.image);
        bytes
    }
}

/// Parse a structurally valid full or short `.d750` configuration.
///
/// The header signature selects the required payload length; total length
/// alone is not a format discriminator. All opaque header metadata and image
/// bytes are retained. No bytes beyond a short file's coverage are invented.
/// Settings contents, radio-type compatibility, and firmware layout are not
/// validated or normalized.
///
/// # Errors
///
/// Returns [`FileError::HeaderTooShort`] for an incomplete header,
/// [`FileError::InvalidSignature`], [`FileError::InvalidModel`], or
/// [`FileError::NonzeroReservedByte`] for an invalid known header field, and
/// [`FileError::Length`] when the remaining length disagrees with the signature.
pub fn parse_d750(data: &[u8]) -> Result<RadioConfig, FileError> {
    let header_length_error = || FileError::HeaderTooShort {
        actual: data.len(),
        minimum: HEADER_SIZE,
    };
    let header = data
        .first_chunk::<HEADER_SIZE>()
        .copied()
        .ok_or_else(header_length_error)?;
    let header = ConfigHeader::try_from(header)?;
    let expected = header.layout().file_size();
    if data.len() != expected {
        return Err(FileError::Length {
            actual: data.len(),
            expected,
        });
    }
    let image = data
        .get(HEADER_SIZE..)
        .ok_or_else(header_length_error)?
        .to_vec();
    RadioConfig::new(header, image)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample_header(layout: FileLayout) -> Result<[u8; HEADER_SIZE], Box<dyn std::error::Error>> {
        let mut bytes = [0xA5; HEADER_SIZE];
        let signature = match layout {
            FileLayout::Full => b"MCP-D750".as_slice(),
            FileLayout::WithoutStartupScreen => b"TM-D750".as_slice(),
        };
        bytes
            .get_mut(..signature.len())
            .ok_or("signature range missing")?
            .copy_from_slice(signature);
        bytes
            .get_mut(16..23)
            .ok_or("model range missing")?
            .copy_from_slice(b"TM-D750");
        *bytes.get_mut(32).ok_or("reserved byte missing")? = 0;
        Ok(bytes)
    }

    fn sample(layout: FileLayout) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut data = sample_header(layout)?.to_vec();
        for index in 0..layout.image_bytes() {
            data.push(u8::try_from(index % 251)?);
        }
        Ok(data)
    }

    #[test]
    fn full_files_round_trip() -> TestResult {
        let data = sample(FileLayout::Full)?;
        let config = parse_d750(&data)?;
        assert_eq!(
            config.layout(),
            FileLayout::Full,
            "full signature selects full coverage"
        );
        assert_eq!(
            config.image_bytes().len(),
            IMAGE_LENGTH,
            "full files retain the exact image length"
        );
        // 1000 % 251 == 247
        assert_eq!(
            config.image_bytes().get(1000).copied(),
            Some(247),
            "image offsets exclude the header"
        );
        assert_eq!(
            config.to_bytes(),
            data,
            "full files round-trip every header and image byte"
        );
        Ok(())
    }

    #[test]
    fn short_files_preserve_only_their_actual_coverage_and_round_trip() -> TestResult {
        let data = sample(FileLayout::WithoutStartupScreen)?;
        let config = parse_d750(&data)?;
        assert_eq!(
            config.layout(),
            FileLayout::WithoutStartupScreen,
            "short signature selects prefix-only coverage"
        );
        assert_eq!(
            config.image_bytes().len(),
            STARTUP_SCREEN_START,
            "short files must not acquire a synthetic tail"
        );
        assert_eq!(
            config.image_bytes().get(STARTUP_SCREEN_START),
            None,
            "absent startup-screen bytes must not read as erased flash"
        );
        assert_eq!(
            config.to_bytes(),
            data,
            "short files round-trip every stored byte without normalization"
        );
        Ok(())
    }

    #[test]
    fn file_length_errors_name_the_signature_selected_length() -> TestResult {
        for layout in [FileLayout::Full, FileLayout::WithoutStartupScreen] {
            let mut data = sample(layout)?;
            let _removed = data.pop();
            assert_eq!(
                parse_d750(&data),
                Err(FileError::Length {
                    actual: layout.file_size() - 1,
                    expected: layout.file_size(),
                }),
                "length diagnostics must describe the specific header's layout"
            );
        }
        assert_eq!(
            parse_d750(&[0; HEADER_SIZE - 1]),
            Err(FileError::HeaderTooShort {
                actual: HEADER_SIZE - 1,
                minimum: HEADER_SIZE
            }),
            "incomplete-header errors must not guess an image layout"
        );
        Ok(())
    }

    #[test]
    fn headers_validate_each_known_marker_and_preserve_exact_failure_bytes() -> TestResult {
        for layout in [FileLayout::Full, FileLayout::WithoutStartupScreen] {
            let original = sample_header(layout)?;
            let signature_length = if layout == FileLayout::Full { 8 } else { 7 };
            for offset in 0..signature_length {
                let mut bytes = original;
                *bytes.get_mut(offset).ok_or("signature byte missing")? ^= 1;
                let found = bytes
                    .first_chunk::<8>()
                    .copied()
                    .ok_or("signature missing")?;
                assert_eq!(
                    ConfigHeader::try_from(bytes),
                    Err(FileError::InvalidSignature { found }),
                    "every declared signature byte must be validated"
                );
            }
            for offset in 16..23 {
                let mut bytes = original;
                *bytes.get_mut(offset).ok_or("model byte missing")? ^= 1;
                let found = bytes.get(16..23).ok_or("model range missing")?.try_into()?;
                assert_eq!(
                    ConfigHeader::try_from(bytes),
                    Err(FileError::InvalidModel { found }),
                    "every model marker byte must be validated"
                );
            }
            for value in 1..=u8::MAX {
                let mut bytes = original;
                *bytes.get_mut(32).ok_or("reserved byte missing")? = value;
                assert_eq!(
                    ConfigHeader::try_from(bytes),
                    Err(FileError::NonzeroReservedByte { value }),
                    "every nonzero reserved byte is unsupported"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn every_uninterpreted_header_byte_is_preserved() -> TestResult {
        for layout in [FileLayout::Full, FileLayout::WithoutStartupScreen] {
            let signature_length = if layout == FileLayout::Full { 8 } else { 7 };
            for offset in signature_length..HEADER_SIZE {
                if (16..23).contains(&offset) || offset == 32 {
                    continue;
                }
                let mut bytes = sample_header(layout)?;
                *bytes.get_mut(offset).ok_or("metadata byte missing")? = u8::try_from(offset)?;
                let header = ConfigHeader::try_from(bytes)?;
                assert_eq!(
                    header.as_bytes(),
                    &bytes,
                    "metadata at offset {offset} must not be rewritten"
                );
                assert_eq!(
                    header.layout(),
                    layout,
                    "opaque metadata cannot change the declared layout"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn constructor_requires_exact_payload_coverage_for_each_header() -> TestResult {
        for layout in [FileLayout::Full, FileLayout::WithoutStartupScreen] {
            let header = ConfigHeader::try_from(sample_header(layout)?)?;
            let expected = layout.image_bytes();
            for actual in [0, expected - 1, expected + 1] {
                assert_eq!(
                    RadioConfig::new(header.clone(), vec![0; actual]),
                    Err(FileError::ImageLength { actual, expected }),
                    "a configuration cannot be constructed with mismatched header/payload coverage"
                );
            }
            let config = RadioConfig::new(header, vec![0x42; expected])?;
            let bytes = config.to_bytes();
            let (header, image) = config.into_parts();
            assert_eq!(
                image.len(),
                expected,
                "consuming a config preserves exact coverage"
            );
            assert_eq!(
                RadioConfig::new(header, image)?.to_bytes(),
                bytes,
                "reconstructing a config preserves every byte"
            );
        }
        Ok(())
    }

    #[test]
    fn payload_mutation_preserves_the_header_and_all_other_image_bytes() -> TestResult {
        for layout in [FileLayout::Full, FileLayout::WithoutStartupScreen] {
            let original = sample(layout)?;
            let mut config = parse_d750(&original)?;
            let last = layout.image_bytes() - 1;
            *config
                .image_bytes_mut()
                .first_mut()
                .ok_or("first image byte missing")? = 0xE1;
            *config
                .image_bytes_mut()
                .get_mut(last)
                .ok_or("last image byte missing")? = 0xE2;
            let mut expected = original;
            *expected
                .get_mut(HEADER_SIZE)
                .ok_or("first serialized image byte missing")? = 0xE1;
            *expected
                .last_mut()
                .ok_or("last serialized image byte missing")? = 0xE2;
            assert_eq!(
                config.to_bytes(),
                expected,
                "serialization changes only the explicitly edited payload bytes"
            );
            assert_eq!(
                config.header().layout(),
                layout,
                "payload changes cannot alter header layout"
            );
            assert_eq!(
                parse_d750(&expected)?,
                config,
                "modified valid containers round-trip exactly"
            );
        }
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
        assert_eq!(
            header.as_bytes(),
            &expected,
            "the known full-layout constructor pins every header byte"
        );
        assert_eq!(
            header.layout(),
            FileLayout::Full,
            "the official header constructor declares a full image"
        );
        assert_eq!(
            ConfigHeader::try_from(expected)?,
            header,
            "constructed headers satisfy the parsing invariants"
        );
        Ok(())
    }

    #[test]
    fn constructed_header_preserves_other_single_byte_type_components() -> TestResult {
        let header = ConfigHeader::for_mcp_d750(&RadioType::new("J,0,1")?)?;
        assert_eq!(
            header.as_bytes().get(128..131),
            Some(b"J01".as_slice()),
            "type components retain their original ordering"
        );
        Ok(())
    }

    #[test]
    fn constructed_header_rejects_unrepresentable_opaque_type_shapes() -> TestResult {
        for payload in ["J", "K,22,1", "K,2", "K,2,1,0", ",,2,1", "K,,,1", "K,2,,"] {
            let result = ConfigHeader::for_mcp_d750(&RadioType::new(payload)?);
            assert_eq!(
                result,
                Err(FileError::UnsupportedRadioType {
                    payload: payload.to_owned(),
                }),
                "unrepresentable type shapes must not be silently rewritten"
            );
        }
        Ok(())
    }
}
