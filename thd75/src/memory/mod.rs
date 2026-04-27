//! Typed access to the TH-D75 memory image.
//!
//! Parses raw memory bytes (from MCP programming or `.d75` files) into
//! structured Rust types for every radio subsystem. The memory image is
//! 500,480 bytes (1,955 pages of 256 bytes) and is identical whether
//! read via the MCP binary protocol or extracted from a `.d75` SD card
//! config file (after stripping the 256-byte file header).
//!
//! # Design
//!
//! [`MemoryImage`] owns the raw byte buffer and hands out lightweight
//! accessor structs ([`ChannelAccess`], [`SettingsAccess`], etc.) that
//! borrow into it. No data is copied on access — parsing happens
//! on-demand when you call methods on the accessors.
//!
//! Mutation works the same way: call a mutable accessor, modify a
//! field, and the change is written directly into the backing buffer.
//! When you are done, call [`MemoryImage::into_raw`] to get the bytes
//! back for writing to the radio or saving to a `.d75` file.

pub mod aprs;
pub mod channels;
pub mod dstar;
pub mod gps;
pub mod settings;

use crate::sdcard::SdCardError;

use std::fmt;

use crate::protocol::programming;
use crate::sdcard::config::{self as d75, ConfigHeader};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when working with a memory image.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MemoryError {
    /// The raw data is not the expected size.
    InvalidSize {
        /// The actual size in bytes.
        actual: usize,
        /// The expected size in bytes.
        expected: usize,
    },
    /// A channel number is out of range.
    ChannelOutOfRange {
        /// The requested channel number.
        channel: u16,
        /// The maximum valid channel number (inclusive).
        max: u16,
    },
    /// A region could not be parsed.
    ParseError {
        /// The region name (e.g. "channel 42 data").
        region: String,
        /// Human-readable detail.
        detail: String,
    },
    /// The `.d75` file is invalid.
    D75Error {
        /// Human-readable detail.
        detail: String,
    },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSize { actual, expected } => {
                write!(
                    f,
                    "invalid memory image size: {actual} bytes (expected {expected})"
                )
            }
            Self::ChannelOutOfRange { channel, max } => {
                write!(f, "channel {channel} out of range (max {max})")
            }
            Self::ParseError { region, detail } => {
                write!(f, "failed to parse {region}: {detail}")
            }
            Self::D75Error { detail } => {
                write!(f, "invalid .d75 file: {detail}")
            }
        }
    }
}

impl std::error::Error for MemoryError {}

// ---------------------------------------------------------------------------
// Re-exports for convenience
// ---------------------------------------------------------------------------

pub use aprs::AprsAccess;
pub use channels::{ChannelAccess, ChannelWriter};
pub use dstar::DstarAccess;
pub use gps::GpsAccess;
pub use settings::{SettingsAccess, SettingsWriter};

// ---------------------------------------------------------------------------
// MemoryImage
// ---------------------------------------------------------------------------

/// A parsed TH-D75 memory image providing typed access to all settings.
///
/// The image is exactly [`programming::TOTAL_SIZE`] bytes (500,480).
/// Create one from a raw MCP dump, or from a `.d75` file via
/// [`from_d75_file`](Self::from_d75_file).
///
/// # Examples
///
/// ```rust,no_run
/// use kenwood_thd75::memory::MemoryImage;
///
/// # fn example(raw: Vec<u8>) -> Result<(), kenwood_thd75::memory::MemoryError> {
/// let image = MemoryImage::from_raw(raw)?;
///
/// // Read channel 0.
/// let channels = image.channels();
/// if channels.is_used(0) {
///     if let Some(entry) = channels.get(0) {
///         println!("Ch 0: {} — {} Hz", entry.name, entry.flash.rx_frequency.as_hz());
///     }
/// }
///
/// // Get the raw bytes back for writing.
/// let bytes = image.into_raw();
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct MemoryImage {
    raw: Vec<u8>,
}

impl MemoryImage {
    /// Create from a raw memory dump (from `read_memory_image` or `.d75`
    /// file body).
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::InvalidSize`] if the data is not exactly
    /// 500,480 bytes.
    pub fn from_raw(data: Vec<u8>) -> Result<Self, MemoryError> {
        if data.len() != programming::TOTAL_SIZE {
            return Err(MemoryError::InvalidSize {
                actual: data.len(),
                expected: programming::TOTAL_SIZE,
            });
        }
        Ok(Self { raw: data })
    }

    /// Get the raw bytes (for `write_memory_image`).
    #[must_use]
    pub fn into_raw(self) -> Vec<u8> {
        self.raw
    }

    /// Borrow the raw bytes.
    #[must_use]
    pub fn as_raw(&self) -> &[u8] {
        &self.raw
    }

    /// Mutably borrow the raw bytes.
    #[must_use]
    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        &mut self.raw
    }

    /// Access channel data (read-only).
    #[must_use]
    pub fn channels(&self) -> ChannelAccess<'_> {
        ChannelAccess::new(&self.raw)
    }

    /// Access channel data (mutable, for writing channels).
    #[must_use]
    pub fn channels_mut(&mut self) -> ChannelWriter<'_> {
        ChannelWriter::new(&mut self.raw)
    }

    /// Access system settings (read-only raw bytes for unmapped regions).
    #[must_use]
    pub fn settings(&self) -> SettingsAccess<'_> {
        SettingsAccess::new(&self.raw)
    }

    /// Access system settings (mutable, for writing verified settings).
    #[must_use]
    pub fn settings_mut(&mut self) -> SettingsWriter<'_> {
        SettingsWriter::new(&mut self.raw)
    }

    /// Apply a settings mutation and return the changed byte's MCP offset
    /// and new value.
    ///
    /// The closure receives a `SettingsWriter` to modify exactly one setting.
    /// This method snapshots the settings page before the closure, runs it,
    /// then diffs to find the single changed byte. Returns `Some((offset, value))`
    /// if a byte changed, or `None` if nothing changed.
    ///
    /// # Panics
    ///
    /// Panics if more than one byte changed (the closure should modify
    /// exactly one setting).
    pub fn modify_setting<F>(&mut self, f: F) -> Option<(u16, u8)>
    where
        F: FnOnce(&mut SettingsWriter<'_>),
    {
        // Settings live at offsets 0x0000..0x2000 in the raw image
        // (MCP addresses 0x1000..0x10FF map to image[0x1000..0x10FF])
        const SETTINGS_START: usize = 0x1000;
        const SETTINGS_END: usize = 0x1100;

        // Snapshot the settings region. If the image is shorter than
        // SETTINGS_END (shouldn't happen for a valid MCP image), there's nothing
        // to diff against, so return early via `?` (function returns `Option`).
        let snapshot_src = self.raw.get(SETTINGS_START..SETTINGS_END)?;
        let mut snapshot = [0u8; SETTINGS_END - SETTINGS_START];
        snapshot.copy_from_slice(snapshot_src);

        // Apply the mutation
        f(&mut SettingsWriter::new(&mut self.raw));

        // Diff to find the changed byte
        let mut changed: Option<(u16, u8)> = None;
        for (i, &snap_byte) in snapshot.iter().enumerate() {
            let Some(&current) = self.raw.get(SETTINGS_START + i) else {
                continue;
            };
            if current != snap_byte {
                assert!(
                    changed.is_none(),
                    "modify_setting: more than one byte changed"
                );
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "Settings region is tiny (a few KB within the 500KB MCP image) and \
                              `SETTINGS_START + i` cannot exceed the settings-region size which \
                              fits in u16::MAX. The returned offset is only used to identify \
                              which settings byte changed."
                )]
                let offset = (SETTINGS_START + i) as u16;
                changed = Some((offset, current));
            }
        }
        changed
    }

    /// Access the APRS configuration region (raw bytes).
    #[must_use]
    pub fn aprs(&self) -> AprsAccess<'_> {
        AprsAccess::new(&self.raw)
    }

    /// Access the D-STAR configuration region (raw bytes).
    #[must_use]
    pub fn dstar(&self) -> DstarAccess<'_> {
        DstarAccess::new(&self.raw)
    }

    /// Access the GPS configuration region (raw bytes).
    #[must_use]
    pub fn gps(&self) -> GpsAccess<'_> {
        GpsAccess::new(&self.raw)
    }

    // -----------------------------------------------------------------------
    // .d75 file integration
    // -----------------------------------------------------------------------

    /// Create from a `.d75` config file (strips the 256-byte header).
    ///
    /// The `.d75` file format is a 256-byte file header followed by the
    /// raw MCP memory image. This constructor validates the header and
    /// extracts the image body.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::D75Error`] if the file is too short or
    /// the header model string is not recognised.
    /// Returns [`MemoryError::InvalidSize`] if the body is not the
    /// expected size.
    pub fn from_d75_file(data: &[u8]) -> Result<Self, MemoryError> {
        let min_size = d75::HEADER_SIZE + programming::TOTAL_SIZE;
        if data.len() < min_size {
            return Err(MemoryError::D75Error {
                detail: format!(
                    "file too small: {} bytes (expected at least {})",
                    data.len(),
                    min_size
                ),
            });
        }

        // Validate the header by attempting to parse it.
        let header_result = d75::parse_config(data);
        if let Err(e) = header_result {
            return Err(MemoryError::D75Error {
                detail: e.to_string(),
            });
        }

        let body = data
            .get(d75::HEADER_SIZE..d75::HEADER_SIZE + programming::TOTAL_SIZE)
            .ok_or_else(|| MemoryError::D75Error {
                detail: format!(
                    "file body too short after header: {} bytes, expected {}",
                    data.len().saturating_sub(d75::HEADER_SIZE),
                    programming::TOTAL_SIZE
                ),
            })?
            .to_vec();
        Self::from_raw(body)
    }

    /// Export as a `.d75` config file (prepends header).
    ///
    /// Uses the provided [`ConfigHeader`] to build the file. The header
    /// is preserved as-is (including model string and version bytes) for
    /// round-trip fidelity.
    #[must_use]
    pub fn to_d75_file(&self, header: &ConfigHeader) -> Vec<u8> {
        let mut out = Vec::with_capacity(d75::HEADER_SIZE + self.raw.len());
        out.extend_from_slice(&header.raw);
        out.extend_from_slice(&self.raw);
        out
    }

    /// Export this image as a `.d75` file ready to write to the SD card.
    ///
    /// Uses a default TH-D75A header with the standard version bytes.
    /// For a specific model or custom header, use [`to_d75_file`](Self::to_d75_file).
    ///
    /// # Errors
    ///
    /// Returns a [`SdCardError`] if the built-in model string is rejected
    /// (unreachable under normal operation — `"Data For TH-D75A"` is in the
    /// crate's `KNOWN_MODELS` constant, so this method is effectively
    /// infallible unless that constant changes).
    pub fn to_d75_bytes(&self) -> Result<Vec<u8>, SdCardError> {
        let header = d75::make_header("Data For TH-D75A", [0x95, 0xC4, 0x8F, 0x42])?;
        Ok(self.to_d75_file(&header))
    }

    /// Read a byte range from the image.
    ///
    /// Returns `None` if the range is out of bounds.
    #[must_use]
    pub fn read_region(&self, offset: usize, len: usize) -> Option<&[u8]> {
        self.raw.get(offset..offset + len)
    }

    /// Write bytes into the image at the given offset.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::InvalidSize`] if the write extends past
    /// the end of the image.
    pub fn write_region(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let end = offset + data.len();
        let raw_len = self.raw.len();
        let dst = self
            .raw
            .get_mut(offset..end)
            .ok_or(MemoryError::InvalidSize {
                actual: end,
                expected: raw_len,
            })?;
        dst.copy_from_slice(data);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn to_d75_bytes_round_trip() -> TestResult {
        let raw = vec![0u8; programming::TOTAL_SIZE];
        let image = MemoryImage::from_raw(raw.clone())?;
        let d75_bytes = image.to_d75_bytes()?;

        // Should be header + raw image.
        assert_eq!(d75_bytes.len(), d75::HEADER_SIZE + programming::TOTAL_SIZE);

        // The body portion should match the original raw data.
        assert_eq!(
            d75_bytes
                .get(d75::HEADER_SIZE..)
                .ok_or("d75 bytes too short")?,
            &raw[..]
        );

        // The header should be parseable and identify as D75A.
        let reparsed = MemoryImage::from_d75_file(&d75_bytes)?;
        assert_eq!(reparsed.as_raw(), &raw[..]);
        Ok(())
    }

    #[test]
    fn to_d75_file_with_custom_header() -> TestResult {
        let raw = vec![0u8; programming::TOTAL_SIZE];
        let image = MemoryImage::from_raw(raw)?;
        let header = d75::make_header("Data For TH-D75E", [0x01, 0x02, 0x03, 0x04])?;
        let d75_bytes = image.to_d75_file(&header);

        // Verify header model.
        let reparsed_config = d75::parse_config(&d75_bytes)?;
        assert_eq!(reparsed_config.header.model, "Data For TH-D75E");
        assert_eq!(
            reparsed_config.header.version_bytes,
            [0x01, 0x02, 0x03, 0x04]
        );
        Ok(())
    }

    #[test]
    fn from_raw_wrong_size() -> TestResult {
        let err = MemoryImage::from_raw(vec![0u8; 100])
            .err()
            .ok_or("expected InvalidSize error but got Ok")?;
        assert!(
            matches!(err, MemoryError::InvalidSize { .. }),
            "expected InvalidSize, got {err:?}"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // modify_setting tests
    // -----------------------------------------------------------------------

    #[test]
    fn modify_setting_returns_changed_byte() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        // key_beep lives at offset 0x1071; set it from 0 to 1
        let result = image.modify_setting(|w| {
            w.set_key_beep(true);
        });
        assert_eq!(result, Some((0x1071, 1)));
        Ok(())
    }

    #[test]
    fn modify_setting_no_change_returns_none() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        // beep is already 0 (false); setting it to false again changes nothing
        let result = image.modify_setting(|w| {
            w.set_key_beep(false);
        });
        assert_eq!(result, None);
        Ok(())
    }

    #[test]
    #[should_panic(expected = "more than one byte changed")]
    fn modify_setting_panics_on_multi_byte() {
        let raw = vec![0u8; programming::TOTAL_SIZE];
        // Unwrap is OK here: this test is specifically verifying the panic
        // from modify_setting. #[should_panic] captures it.
        let Ok(mut image) = MemoryImage::from_raw(raw) else {
            unreachable!("TOTAL_SIZE is always a valid memory image size");
        };
        let _ = image.modify_setting(|w| {
            // Change two distinct single-byte settings
            w.set_key_beep(true); // 0x1071
            w.set_beep_volume(5); // 0x1072
        });
    }

    // -----------------------------------------------------------------------
    // write_region error path
    // -----------------------------------------------------------------------

    #[test]
    fn write_region_out_of_bounds() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        assert!(
            image
                .write_region(programming::TOTAL_SIZE - 10, &[0u8; 20])
                .is_err()
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // from_d75_file too-small error
    // -----------------------------------------------------------------------

    #[test]
    fn from_d75_file_too_small() {
        assert!(MemoryImage::from_d75_file(&[0u8; 100]).is_err());
    }

    // -----------------------------------------------------------------------
    // MemoryError variant coverage
    // -----------------------------------------------------------------------

    #[test]
    fn error_channel_out_of_range_display() {
        let err = MemoryError::ChannelOutOfRange {
            channel: 2000,
            max: 1199,
        };
        let msg = err.to_string();
        assert!(msg.contains("2000"));
        assert!(msg.contains("1199"));
    }

    #[test]
    fn error_parse_error_display() {
        let err = MemoryError::ParseError {
            region: "channel 42 data".into(),
            detail: "bad mode byte".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("channel 42 data"));
        assert!(msg.contains("bad mode byte"));
    }
}
