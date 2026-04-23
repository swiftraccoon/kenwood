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
//! Channel data lives at `.d75 offset 0x100 + MCP offset`. The exact
//! section layout is inferred from D74 development notes and adapted
//! for the D75's expanded feature set.

use super::SdCardError;
use crate::types::channel::FlashChannel;

/// Size of the `.d75` file header in bytes.
pub const HEADER_SIZE: usize = 0x100;

/// Maximum number of memory channels on the TH-D75.
pub const MAX_CHANNELS: usize = 1000;

/// Size of each channel memory entry in bytes.
const CHANNEL_ENTRY_SIZE: usize = FlashChannel::BYTE_SIZE; // 40

/// Size of each channel name entry in bytes.
const CHANNEL_NAME_SIZE: usize = 16;

/// `.d75` file offset to the channel flags table.
///
/// Each channel has a 4-byte flags entry. This precedes the channel
/// memory data in the file layout.
///
/// File offset = `HEADER_SIZE + 0x2000 = 0x2100`.
const CHANNEL_FLAGS_OFFSET: usize = HEADER_SIZE + 0x2000;

/// `.d75` file offset to the channel memory data section.
///
/// Each channel is a 40-byte structure.
///
/// File offset = `HEADER_SIZE + 0x4000 = 0x4100`.
const CHANNEL_DATA_OFFSET: usize = HEADER_SIZE + 0x4000;

/// `.d75` file offset to the channel name table.
///
/// Channel names are 16-byte null-padded strings.
///
/// File offset = `HEADER_SIZE + 0x10000 = 0x10100`.
const CHANNEL_NAME_OFFSET: usize = HEADER_SIZE + 0x10000;

/// Size of each channel flags entry in bytes.
const CHANNEL_FLAGS_SIZE: usize = 4;

/// Known model identification strings found at offset 0 of the header.
const KNOWN_MODELS: &[&str] = &["Data For TH-D75A", "Data For TH-D75E", "Data For TH-D75"];

/// Parsed `.d75` configuration file header (256 bytes).
///
/// The header contains the model identification string and metadata
/// fields. The radio rejects files with unrecognised model strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigHeader {
    /// Model identification string (e.g., `"Data For TH-D75A"`).
    ///
    /// Null-terminated, stored at offset 0x00 (up to 16 bytes).
    pub model: String,

    /// Version/checksum bytes at offset 0x14 (4 bytes).
    ///
    /// Observed as `0x95C48F42` for the TH-D75A; exact semantics unknown.
    pub version_bytes: [u8; 4],

    /// Raw header bytes preserved for round-trip fidelity.
    ///
    /// Always exactly 256 bytes. Fields above are parsed views into
    /// this buffer.
    pub raw: [u8; HEADER_SIZE],
}

/// Complete radio configuration from a `.d75` file.
///
/// This is the top-level structure returned by [`parse_config`].
#[derive(Debug, Clone)]
pub struct RadioConfig {
    /// The 256-byte file header.
    pub header: ConfigHeader,

    /// Parsed memory channels (up to 1000).
    ///
    /// Each entry pairs the channel data with its display name and
    /// flags. Unused channels (all-`0xFF` frequency) are still
    /// present; check [`ChannelEntry::used`] to filter.
    pub channels: Vec<ChannelEntry>,

    /// Raw settings bytes (everything outside the channel regions).
    ///
    /// This preserves all data between the header and the channel
    /// sections, and after the channel name table, enabling
    /// round-trip write-back of settings we do not yet parse.
    pub raw_image: Vec<u8>,
}

/// A single memory channel combining frequency data, display name, and flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelEntry {
    /// Channel number (0--999).
    pub number: u16,

    /// User-assigned display name (up to 16 bytes, ASCII).
    pub name: String,

    /// The 40-byte flash channel data (frequency, mode, tone, offset, etc.).
    ///
    /// Uses the flash memory encoding ([`FlashChannel`]) which differs from
    /// the CAT wire format ([`crate::types::ChannelMemory`]). Key differences
    /// include the mode field (8 modes vs 4) and structured tone/duplex bit
    /// packing.
    pub flash: FlashChannel,

    /// Whether this channel slot contains valid data.
    ///
    /// A channel is considered unused when its RX frequency is
    /// `0x00000000` or `0xFFFFFFFF`.
    pub used: bool,

    /// Channel lockout state from the flags table.
    pub lockout: bool,
}

/// Parses a `.d75` configuration file from raw bytes.
///
/// # Errors
///
/// Returns [`SdCardError::FileTooSmall`] if the data is shorter than
/// the minimum required size, or [`SdCardError::InvalidModelString`]
/// if the header model is not recognised.
pub fn parse_config(data: &[u8]) -> Result<RadioConfig, SdCardError> {
    // Minimum size: header + channel names region must be reachable.
    let min_size = CHANNEL_NAME_OFFSET + (MAX_CHANNELS * CHANNEL_NAME_SIZE);
    if data.len() < min_size {
        return Err(SdCardError::FileTooSmall {
            expected: min_size,
            actual: data.len(),
        });
    }

    // --- Parse header ---
    let header_slice = data.get(..HEADER_SIZE).ok_or(SdCardError::FileTooSmall {
        expected: HEADER_SIZE,
        actual: data.len(),
    })?;
    let raw_header =
        <[u8; HEADER_SIZE]>::try_from(header_slice).map_err(|_| SdCardError::FileTooSmall {
            expected: HEADER_SIZE,
            actual: data.len(),
        })?;

    let model_slice = raw_header.get(..16).ok_or(SdCardError::FileTooSmall {
        expected: 16,
        actual: raw_header.len(),
    })?;
    let model = extract_null_terminated(model_slice);
    if !KNOWN_MODELS.contains(&model.as_str()) {
        return Err(SdCardError::InvalidModelString { found: model });
    }

    let version_slice = raw_header
        .get(0x14..0x18)
        .ok_or(SdCardError::FileTooSmall {
            expected: 0x18,
            actual: raw_header.len(),
        })?;
    let version_bytes =
        <[u8; 4]>::try_from(version_slice).map_err(|_| SdCardError::FileTooSmall {
            expected: 0x18,
            actual: raw_header.len(),
        })?;

    let header = ConfigHeader {
        model,
        version_bytes,
        raw: raw_header,
    };

    // --- Parse channels ---
    let mut channels = Vec::with_capacity(MAX_CHANNELS);

    for i in 0..MAX_CHANNELS {
        let ch_offset = CHANNEL_DATA_OFFSET + (i * CHANNEL_ENTRY_SIZE);
        let name_offset = CHANNEL_NAME_OFFSET + (i * CHANNEL_NAME_SIZE);
        let flags_offset = CHANNEL_FLAGS_OFFSET + (i * CHANNEL_FLAGS_SIZE);

        // Channel data: if the file is too short for this channel,
        // treat it as unused rather than erroring (the file may have
        // been truncated after the documented sections).
        let ch_end = ch_offset + CHANNEL_ENTRY_SIZE;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Loop index `i` ranges over 0..MAX_CHANNELS (=1000). u16::MAX is 65535, so \
                      the usize-to-u16 cast is provably lossless."
        )]
        let ch_index = i as u16; // MAX_CHANNELS = 1000, always fits in u16

        let (used, flash) = if let Some(ch_bytes) = data.get(ch_offset..ch_end) {
            // ch_bytes is CHANNEL_ENTRY_SIZE (40) bytes so split_first_chunk::<4> always yields Some.
            let rx_freq = ch_bytes
                .split_first_chunk::<4>()
                .map_or(0, |(head, _)| u32::from_le_bytes(*head));
            let is_used = rx_freq != 0 && rx_freq != 0xFFFF_FFFF;
            let ch = FlashChannel::from_bytes(ch_bytes).map_err(|e| SdCardError::ChannelParse {
                index: ch_index,
                detail: e.to_string(),
            })?;
            (is_used, ch)
        } else {
            (false, FlashChannel::default())
        };

        // Channel name
        let name = data
            .get(name_offset..name_offset + CHANNEL_NAME_SIZE)
            .map_or_else(String::new, extract_null_terminated);

        // Channel flags: bit 0 of byte 0 = lockout
        let lockout = data
            .get(flags_offset..flags_offset + CHANNEL_FLAGS_SIZE)
            .and_then(<[u8]>::first)
            .is_some_and(|b| b & 0x01 != 0);

        channels.push(ChannelEntry {
            number: ch_index,
            name,
            flash,
            used,
            lockout,
        });
    }

    // Preserve the entire memory image (minus header) for round-trip.
    // The FileTooSmall check at the top of this function guarantees data.len() >= HEADER_SIZE.
    let raw_image = data
        .get(HEADER_SIZE..)
        .ok_or(SdCardError::FileTooSmall {
            expected: HEADER_SIZE,
            actual: data.len(),
        })?
        .to_vec();

    Ok(RadioConfig {
        header,
        channels,
        raw_image,
    })
}

/// Generates a `.d75` file from a [`RadioConfig`].
///
/// The output is the header concatenated with the raw memory image,
/// with channel data, names, and flags patched in from the
/// [`RadioConfig::channels`] entries.
#[must_use]
pub fn write_config(config: &RadioConfig) -> Vec<u8> {
    let image_size = config.raw_image.len();
    let total_size = HEADER_SIZE + image_size;
    let mut out = vec![0u8; total_size];

    // Write header. `out` was just sized to `HEADER_SIZE + image_size`, so the 0..HEADER_SIZE
    // split always succeeds; `copy_from_slice` panics only on length mismatch, which is
    // impossible here since both halves are fixed-size HEADER_SIZE bytes.
    if let Some(dst) = out.get_mut(..HEADER_SIZE) {
        dst.copy_from_slice(&config.header.raw);
    }

    // Write raw image as the base (preserves all settings)
    if let Some(dst) = out.get_mut(HEADER_SIZE..) {
        dst.copy_from_slice(&config.raw_image);
    }

    // Patch channel data, names, and flags
    for entry in &config.channels {
        let i = entry.number as usize;
        if i >= MAX_CHANNELS {
            continue;
        }

        // Channel memory (40 bytes)
        let ch_offset = CHANNEL_DATA_OFFSET + (i * CHANNEL_ENTRY_SIZE);
        let ch_end = ch_offset + CHANNEL_ENTRY_SIZE;
        if let Some(dst) = out.get_mut(ch_offset..ch_end) {
            let bytes = entry.flash.to_bytes();
            dst.copy_from_slice(&bytes);
        }

        // Channel name (16 bytes, null-padded)
        let name_offset = CHANNEL_NAME_OFFSET + (i * CHANNEL_NAME_SIZE);
        let name_end = name_offset + CHANNEL_NAME_SIZE;
        if let Some(dst) = out.get_mut(name_offset..name_end) {
            let mut name_buf = [0u8; CHANNEL_NAME_SIZE];
            let src = entry.name.as_bytes();
            let copy_len = src.len().min(CHANNEL_NAME_SIZE);
            // Both halves are bounded by copy_len, so these slices exist by construction.
            if let (Some(dst_head), Some(src_head)) =
                (name_buf.get_mut(..copy_len), src.get(..copy_len))
            {
                dst_head.copy_from_slice(src_head);
            }
            dst.copy_from_slice(&name_buf);
        }

        // Channel flags (4 bytes, bit 0 = lockout)
        let flags_offset = CHANNEL_FLAGS_OFFSET + (i * CHANNEL_FLAGS_SIZE);
        if let Some(flag_byte) = out.get_mut(flags_offset) {
            // Guard that the full 4-byte flags region is within bounds before touching byte 0.
            if flags_offset + CHANNEL_FLAGS_SIZE <= total_size {
                // Preserve existing flag bits; only toggle lockout bit 0.
                if entry.lockout {
                    *flag_byte |= 0x01;
                } else {
                    *flag_byte &= !0x01;
                }
            }
        }
    }

    out
}

/// Extracts a null-terminated ASCII string from a byte slice.
fn extract_null_terminated(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    // `position` guarantees `end <= bytes.len()`, so `.get(..end)` always returns `Some`.
    bytes.get(..end).map_or_else(String::new, |valid| {
        String::from_utf8_lossy(valid).into_owned()
    })
}

/// Creates a minimal valid `.d75` header for the given model string.
///
/// Useful for generating new configuration files from scratch.
///
/// # Errors
///
/// Returns [`SdCardError::InvalidModelString`] if the model string
/// is not one of the known variants.
pub fn make_header(model: &str, version_bytes: [u8; 4]) -> Result<ConfigHeader, SdCardError> {
    if !KNOWN_MODELS.contains(&model) {
        return Err(SdCardError::InvalidModelString {
            found: model.to_owned(),
        });
    }

    let mut raw = [0u8; HEADER_SIZE];
    let model_bytes = model.as_bytes();
    let copy_len = model_bytes.len().min(16);
    // copy_len <= 16 <= HEADER_SIZE, so both slices are in bounds by construction.
    if let (Some(dst), Some(src)) = (raw.get_mut(..copy_len), model_bytes.get(..copy_len)) {
        dst.copy_from_slice(src);
    }
    // 0x18 <= HEADER_SIZE (0x100), so this 4-byte slot is in bounds by construction.
    if let Some(dst) = raw.get_mut(0x14..0x18) {
        dst.copy_from_slice(&version_bytes);
    }

    Ok(ConfigHeader {
        model: model.to_owned(),
        version_bytes,
        raw,
    })
}

/// Creates an empty [`ChannelEntry`] for the given channel number.
#[must_use]
pub fn empty_channel(number: u16) -> ChannelEntry {
    ChannelEntry {
        number,
        name: String::new(),
        flash: FlashChannel::default(),
        used: false,
        lockout: false,
    }
}

/// Creates a [`ChannelEntry`] with the given flash channel data.
///
/// The channel is automatically marked as `used = true` if the RX
/// frequency is nonzero.
#[must_use]
pub fn make_channel(number: u16, name: &str, flash: FlashChannel) -> ChannelEntry {
    let used = flash.rx_frequency.as_hz() != 0;
    ChannelEntry {
        number,
        name: name.to_owned(),
        flash,
        used,
        lockout: false,
    }
}

/// Write a `.d75` configuration file from a raw memory image and header.
///
/// The `.d75` file format is: 256-byte header + raw MCP memory image.
/// This produces files identical to those exported by Menu No. 800
/// or the MCP-D75 application.
///
/// # Errors
///
/// Returns [`SdCardError::InvalidModelString`] if the header model string
/// is not recognised. Returns [`SdCardError::FileTooSmall`] if the image
/// is smaller than the minimum expected size for channel parsing.
pub fn write_d75(
    image: &crate::memory::MemoryImage,
    header: &ConfigHeader,
) -> Result<Vec<u8>, SdCardError> {
    // Validate the header model string.
    if !KNOWN_MODELS.contains(&header.model.as_str()) {
        return Err(SdCardError::InvalidModelString {
            found: header.model.clone(),
        });
    }

    let raw = image.as_raw();

    // Validate that the image is at least large enough for channel data
    // (this ensures round-trip parse_config will succeed).
    let min_body = CHANNEL_NAME_OFFSET - HEADER_SIZE + (MAX_CHANNELS * CHANNEL_NAME_SIZE);
    if raw.len() < min_body {
        return Err(SdCardError::FileTooSmall {
            expected: min_body,
            actual: raw.len(),
        });
    }

    let mut out = Vec::with_capacity(HEADER_SIZE + raw.len());
    out.extend_from_slice(&header.raw);
    out.extend_from_slice(raw);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::frequency::Frequency;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

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

    #[test]
    fn extract_null_terminated_basic() -> TestResult {
        let mut buf = [0u8; 16];
        buf.get_mut(..5)
            .ok_or("buf too short for 5 bytes")?
            .copy_from_slice(b"hello");
        assert_eq!(extract_null_terminated(&buf), "hello");
        Ok(())
    }

    #[test]
    fn extract_null_terminated_full() {
        let buf = *b"abcdefghijklmnop";
        assert_eq!(extract_null_terminated(&buf), "abcdefghijklmnop");
    }

    #[test]
    fn make_header_valid() -> TestResult {
        let hdr = make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42])?;
        assert_eq!(hdr.model, "Data For TH-D75A");
        assert_eq!(hdr.version_bytes, [0x95, 0xC4, 0x8F, 0x42]);
        assert_eq!(hdr.raw.len(), HEADER_SIZE);
        Ok(())
    }

    #[test]
    fn make_header_invalid_model() -> TestResult {
        let err = make_header("Data For TH-D74A", [0; 4])
            .err()
            .ok_or("expected InvalidModelString error but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidModelString { .. }),
            "expected InvalidModelString, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn empty_channel_defaults() {
        let ch = empty_channel(42);
        assert_eq!(ch.number, 42);
        assert!(!ch.used);
        assert!(!ch.lockout);
        assert_eq!(ch.name, "");
    }

    #[test]
    fn make_channel_marks_used() {
        let flash = FlashChannel {
            rx_frequency: Frequency::new(145_000_000),
            ..FlashChannel::default()
        };
        let ch = make_channel(0, "2M RPT", flash);
        assert!(ch.used);
        assert_eq!(ch.name, "2M RPT");
    }

    #[test]
    fn make_channel_zero_freq_unused() {
        let ch = make_channel(0, "empty", FlashChannel::default());
        assert!(!ch.used);
    }

    #[test]
    fn write_d75_round_trip() -> TestResult {
        use crate::memory::MemoryImage;
        use crate::protocol::programming;

        let header = make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42])?;
        let raw = vec![0u8; programming::TOTAL_SIZE];
        let image = MemoryImage::from_raw(raw)?;

        // Write the .d75 file.
        let d75_bytes = write_d75(&image, &header)?;

        // The output should be header + image.
        assert_eq!(d75_bytes.len(), HEADER_SIZE + programming::TOTAL_SIZE);
        assert_eq!(
            d75_bytes.get(..HEADER_SIZE).ok_or("d75_bytes too short")?,
            &header.raw
        );
        assert_eq!(
            d75_bytes.get(HEADER_SIZE..).ok_or("d75_bytes too short")?,
            image.as_raw()
        );

        // Round-trip: parse it back and verify.
        let parsed = parse_config(&d75_bytes)?;
        assert_eq!(parsed.header.model, "Data For TH-D75A");
        assert_eq!(parsed.header.version_bytes, [0x95, 0xC4, 0x8F, 0x42]);
        assert_eq!(parsed.raw_image.len(), d75_bytes.len() - HEADER_SIZE);
        Ok(())
    }

    #[test]
    fn write_d75_invalid_model_rejected() -> TestResult {
        use crate::memory::MemoryImage;
        use crate::protocol::programming;

        let mut raw_header = [0u8; HEADER_SIZE];
        raw_header
            .get_mut(..17)
            .ok_or("raw_header too short")?
            .copy_from_slice(b"Data For TH-D74A\0");
        let header = ConfigHeader {
            model: "Data For TH-D74A".to_owned(),
            version_bytes: [0; 4],
            raw: raw_header,
        };
        let raw = vec![0u8; programming::TOTAL_SIZE];
        let image = MemoryImage::from_raw(raw)?;

        let err = write_d75(&image, &header)
            .err()
            .ok_or("expected InvalidModelString but got Ok")?;
        assert!(
            matches!(err, SdCardError::InvalidModelString { .. }),
            "expected InvalidModelString, got {err:?}"
        );
        Ok(())
    }

    #[test]
    fn write_d75_preserves_channel_data() -> TestResult {
        use crate::memory::MemoryImage;
        use crate::protocol::programming;

        let header = make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42])?;

        // Build a raw image with some nonzero data in the channel region.
        let mut raw = vec![0u8; programming::TOTAL_SIZE];
        // Put a marker byte at offset 0x4000 (channel data section in the body).
        if raw.len() > 0x4000 {
            set_byte(&mut raw, 0x4000, 0xAB)?;
        }
        let image = MemoryImage::from_raw(raw)?;

        let d75_bytes = write_d75(&image, &header)?;

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
    fn parse_config_channel_parse_error() -> TestResult {
        use crate::protocol::programming;

        let header = make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42])?;

        // Build a valid .d75 file, then corrupt channel 0's step_size byte.
        let mut d75_data = vec![0u8; HEADER_SIZE + programming::TOTAL_SIZE];
        write_slice(&mut d75_data, 0, &header.raw)?;

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
