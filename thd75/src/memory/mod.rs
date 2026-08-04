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
//! borrow into it. No data is copied on access; parsing happens
//! on-demand when you call methods on the accessors.
//!
//! Mutation works the same way: call a mutable accessor, modify a
//! field, and the change is written directly into the backing buffer.
//! When you are done, call [`MemoryImage::into_raw`] to get the bytes
//! back for writing to the radio. A complete `.d75` file is represented by
//! [`crate::sdcard::config::RadioConfig`], which retains its otherwise opaque
//! 256-byte header alongside this image.

pub mod aprs;
pub mod channels;
pub mod dstar;
pub mod gps;
pub mod menu_fields;
mod menu_patch;
pub mod schema;
pub mod settings;

use std::fmt;

use crate::protocol::programming;
use crate::types::{FirmwareIdentity, RadioModel, RegularChannel};

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
    /// A channel entry cannot be represented without violating its typed
    /// memory-image invariants.
    InvalidChannelEntry {
        /// Channel containing the invalid value.
        channel: RegularChannel,
        /// Human-readable validation failure.
        detail: String,
    },
    /// A requested region's end offset cannot be represented by [`usize`].
    RegionEndOverflow {
        /// Byte offset where the region would begin.
        offset: usize,
        /// Requested region length in bytes.
        length: usize,
    },
    /// A region could not be parsed.
    ParseError {
        /// The region name (e.g. "channel 42 data").
        region: String,
        /// Human-readable detail.
        detail: String,
    },
    /// A typed settings read or write rejected a missing or invalid byte.
    SettingsValue {
        /// The underlying typed settings error.
        source: SettingsValueError,
    },
    /// A single-setting mutation changed more than one backing byte.
    MultipleSettingBytesChanged {
        /// First changed MCP byte offset.
        first: u16,
        /// Second changed MCP byte offset.
        second: u16,
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
            Self::InvalidChannelEntry { channel, detail } => {
                write!(f, "invalid channel {channel}: {detail}")
            }
            Self::RegionEndOverflow { offset, length } => write!(
                f,
                "memory region end overflows usize: offset {offset} + length {length}"
            ),
            Self::ParseError { region, detail } => {
                write!(f, "failed to parse {region}: {detail}")
            }
            Self::SettingsValue { source } => source.fmt(f),
            Self::MultipleSettingBytesChanged { first, second } => write!(
                f,
                "single-setting mutation changed MCP bytes 0x{first:04X} and 0x{second:04X}"
            ),
        }
    }
}

impl std::error::Error for MemoryError {}

impl From<SettingsValueError> for MemoryError {
    fn from(source: SettingsValueError) -> Self {
        Self::SettingsValue { source }
    }
}

// ---------------------------------------------------------------------------
// Re-exports for convenience
// ---------------------------------------------------------------------------

pub use aprs::AprsAccess;
pub use channels::{ChannelAccess, ChannelEntry, ChannelWriter};
pub use dstar::DstarAccess;
pub use gps::{GpsAccess, GpsValueError};
pub use menu_fields::{
    MCP_D75_MENU_FIELDS, MCP_D75_SCHEMA_VERSION, MCP_D75_SOURCE_SHA256, MenuField, MenuOption,
    StorageTransform, menu_field,
};
pub use schema::{
    BytePatch, DecodedFieldValue, Endian, FieldCodec, FieldDescriptor, FieldValue, PagePatch,
    PatchPlanner, PatchSet, SchemaError, StringEncoding,
};
pub use settings::{SettingsAccess, SettingsValueError, SettingsWriter};

/// Radio model whose live MCP layout is represented by the generated schema.
pub const MCP_D75_SCHEMA_MODEL: &str = "TH-D75";

/// Firmware version whose live MCP layout is represented by the schema.
pub const MCP_D75_SCHEMA_FIRMWARE: &str = "1.03";

/// Exact CAT `FV` identities whose MCP layout matches the generated schema.
///
/// The vendor identities and V1.03.AZM automation identity share the same MCP
/// layout. No prefix or numeric-version matching is permitted for live access.
pub const MCP_D75_SCHEMA_FIRMWARE_IDENTITIES: &[&str] = &["1.03", "1.03.000", "1.03.AZM"];

/// Canonicalize an exact supported CAT firmware identity.
///
/// Returns [`MCP_D75_SCHEMA_FIRMWARE`] for any exact identity in
/// [`MCP_D75_SCHEMA_FIRMWARE_IDENTITIES`]. All other strings are rejected,
/// including later build suffixes such as `1.03.001`.
#[must_use]
pub fn canonicalize_mcp_d75_schema_firmware(firmware: &FirmwareIdentity) -> Option<&'static str> {
    MCP_D75_SCHEMA_FIRMWARE_IDENTITIES
        .contains(&firmware.as_str())
        .then_some(MCP_D75_SCHEMA_FIRMWARE)
}

/// Whether a live CAT identity exactly matches the MCP-D75 schema target.
#[must_use]
pub fn is_supported_mcp_d75_schema_target(model: RadioModel, firmware: &FirmwareIdentity) -> bool {
    model == RadioModel::ThD75 && canonicalize_mcp_d75_schema_firmware(firmware).is_some()
}

// ---------------------------------------------------------------------------
// MemoryImage
// ---------------------------------------------------------------------------

/// A parsed TH-D75 memory image providing typed access to all settings.
///
/// The image is exactly [`programming::TOTAL_SIZE`] bytes (500,480). Create one
/// from a raw MCP dump. Parse a complete `.d75` file with
/// [`crate::sdcard::config::parse_config`] so its opaque header remains paired
/// with the image in a [`crate::sdcard::config::RadioConfig`].
///
/// # Examples
///
/// ```rust,no_run
/// use kenwood_thd75::{RegularChannel, memory::MemoryImage};
///
/// # fn example(raw: Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
/// let image = MemoryImage::from_raw(raw)?;
///
/// // Read channel 0.
/// let channels = image.channels();
/// let channel = RegularChannel::new(0)?;
/// let entry = channels.get(channel)?;
/// if let Some(programmed) = entry.programmed() {
///     println!(
///         "Ch 0: {} ({} Hz)",
///         entry.name(),
///         programmed.receive_frequency.as_hz()
///     );
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

    /// Apply a schema-generated menu patch set to this complete image.
    ///
    /// This is the offline counterpart to
    /// [`Radio::apply_menu_patches_via_mcp`](crate::radio::Radio::apply_menu_patches_via_mcp).
    /// Masked bit fields preserve unrelated bits already present in the
    /// image. Patch sets built by [`PatchPlanner`] are validated at plan
    /// time, so they can never address bytes outside the radio's memory
    /// image or inside the write-protected factory-calibration region, and
    /// application is all-or-nothing.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError::OutOfBounds`] if a patch references bytes
    /// outside this image; the image is unmodified in that case.
    pub fn apply_menu_patches(&mut self, patches: &PatchSet) -> Result<(), SchemaError> {
        patches.apply_to_image(&mut self.raw)
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
    /// The closure receives a [`SettingsWriter`] and must modify at most one
    /// stored byte. The mutation is transactional: a rejected value or a
    /// multi-byte change restores the original settings region.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::SettingsValue`] if the typed setter rejects the
    /// operation, [`MemoryError::MultipleSettingBytesChanged`] if the closure
    /// changes more than one byte, or [`MemoryError::InvalidSize`] if the
    /// complete settings region is unavailable.
    pub fn modify_setting<F>(&mut self, f: F) -> Result<Option<(u16, u8)>, MemoryError>
    where
        F: FnOnce(&mut SettingsWriter<'_>) -> Result<(), SettingsValueError>,
    {
        // Settings-bearing bytes span 0x0000..0x2000 in the raw image:
        // band state (power level 0x0359, attenuator 0x035C, dual band
        // 0x0396), the radio menu block (0x1000..0x10E0, including the
        // power-on message string at 0x10C0), the APRS lock bits at
        // 0x120A, and the DV EMR volume at 0x1A03. The diff window
        // must cover ALL of them: a setter outside the window mutates
        // the image but reports "nothing changed", and the caller
        // silently skips the radio write-back.
        const SETTINGS_START: usize = 0x0000;
        const SETTINGS_END: usize = 0x2000;

        let snapshot_src =
            self.raw
                .get(SETTINGS_START..SETTINGS_END)
                .ok_or(MemoryError::InvalidSize {
                    actual: self.raw.len(),
                    expected: SETTINGS_END,
                })?;
        let mut snapshot = [0u8; SETTINGS_END - SETTINGS_START];
        snapshot.copy_from_slice(snapshot_src);
        let restore = |raw: &mut [u8]| {
            let region = raw
                .get_mut(SETTINGS_START..SETTINGS_END)
                .unwrap_or_else(|| {
                    unreachable!("settings writer cannot resize its borrowed memory image")
                });
            region.copy_from_slice(&snapshot);
        };

        if let Err(error) = f(&mut SettingsWriter::new(&mut self.raw)) {
            restore(&mut self.raw);
            return Err(error.into());
        }

        let current_region =
            self.raw
                .get(SETTINGS_START..SETTINGS_END)
                .ok_or(MemoryError::InvalidSize {
                    actual: self.raw.len(),
                    expected: SETTINGS_END,
                })?;
        let mut changes = snapshot.iter().zip(current_region).enumerate().filter_map(
            |(index, (&before, &after))| {
                (before != after).then_some((SETTINGS_START + index, after))
            },
        );
        let first = changes.next();
        let second = changes.next();
        drop(changes);

        let typed_change = |(offset, value): (usize, u8)| {
            let offset = u16::try_from(offset)
                .unwrap_or_else(|_| unreachable!("settings-region offset fits in u16"));
            (offset, value)
        };
        if let Some(second) = second {
            let (first, _) = typed_change(
                first.unwrap_or_else(|| unreachable!("a second change requires a first change")),
            );
            let (second, _) = typed_change(second);
            restore(&mut self.raw);
            return Err(MemoryError::MultipleSettingBytesChanged { first, second });
        }
        Ok(first.map(typed_change))
    }

    /// Access the opaque APRS archive region (raw bytes).
    #[must_use]
    pub fn aprs(&self) -> AprsAccess<'_> {
        AprsAccess::new(&self.raw)
    }

    /// Decode a named radio menu setting from the generated MCP-D75 catalog.
    ///
    /// This is the field-level path for settings that live outside their
    /// subsystem's opaque bulk region. For example, APRS My Callsign is
    /// available as `image.menu_setting("aprs.MyCallsign")` even though its
    /// byte offset precedes the region exposed by [`Self::aprs`].
    ///
    /// Returns `Ok(None)` when the catalog has no field with `name`.
    ///
    /// # Errors
    ///
    /// Returns [`SchemaError`] when the stored bytes do not satisfy the
    /// catalog field's codec or declared value domain.
    pub fn menu_setting(&self, name: &str) -> Result<Option<DecodedFieldValue>, SchemaError> {
        menu_field(name)
            .map(|field| field.read(&self.raw))
            .transpose()
    }

    /// Access the D-STAR settings region (raw bytes).
    #[must_use]
    pub fn dstar(&self) -> DstarAccess<'_> {
        DstarAccess::new(&self.raw)
    }

    /// Access the official MCP-D75 GPS menu fields and mapped waypoint data.
    #[must_use]
    pub fn gps(&self) -> GpsAccess<'_> {
        GpsAccess::new(&self.raw)
    }

    /// Read a byte range from the image.
    ///
    /// Returns `None` if the range is out of bounds or computing its end
    /// offset would overflow [`usize`].
    #[must_use]
    pub fn read_region(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.raw.get(offset..end)
    }

    /// Write bytes into the image at the given offset.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::RegionEndOverflow`] if computing the write's end
    /// offset would overflow [`usize`], or [`MemoryError::InvalidSize`] if the
    /// write extends past the end of the image.
    pub fn write_region(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let end = offset
            .checked_add(data.len())
            .ok_or(MemoryError::RegionEndOverflow {
                offset,
                length: data.len(),
            })?;
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
    fn schema_firmware_identity_canonicalization_is_exact() -> TestResult {
        assert!(MCP_D75_SCHEMA_FIRMWARE_IDENTITIES.contains(&"1.03.AZM"));
        for supported in MCP_D75_SCHEMA_FIRMWARE_IDENTITIES {
            let identity = FirmwareIdentity::new(supported)?;
            assert_eq!(
                canonicalize_mcp_d75_schema_firmware(&identity),
                Some(MCP_D75_SCHEMA_FIRMWARE),
                "supported CAT identity was not canonicalized: {supported:?}"
            );
            assert!(
                is_supported_mcp_d75_schema_target(RadioModel::ThD75, &identity),
                "supported schema target was rejected: {supported:?}"
            );
        }

        for rejected in ["1.03.001", "1.04", "1.03.0"] {
            let identity = FirmwareIdentity::new(rejected)?;
            assert_eq!(
                canonicalize_mcp_d75_schema_firmware(&identity),
                None,
                "unsupported CAT identity was accepted: {rejected:?}"
            );
        }
        for malformed in [" 1.03", "1.03 "] {
            assert!(FirmwareIdentity::new(malformed).is_err());
        }
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
        let result = image.modify_setting(|w| w.set_key_beep(true))?;
        assert_eq!(result, Some((0x1071, 1)));
        Ok(())
    }

    #[test]
    fn menu_setting_reaches_aprs_callsign_outside_aprs_bulk_region() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        image.write_region(0x1200, b"N0CALL\0\0\0")?;

        assert_eq!(
            image.menu_setting("aprs.MyCallsign")?,
            Some(DecodedFieldValue::Text("N0CALL".to_owned()))
        );
        assert_eq!(image.menu_setting("aprs.DoesNotExist")?, None);
        Ok(())
    }

    #[test]
    fn modify_setting_sees_settings_below_0x1000() -> TestResult {
        // dual_band lives at 0x0396, outside the old diff window
        // [0x1000, 0x1100), where the mutation happened but was
        // reported as "nothing changed", silently skipping the radio
        // write-back.
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        let result = image.modify_setting(|w| w.set_band_mode(crate::types::BandMode::Dual))?;
        assert_eq!(result, Some((0x0396, 1)));
        Ok(())
    }

    #[test]
    fn modify_setting_no_change_returns_none() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        // beep is already 0 (false); setting it to false again changes nothing
        let result = image.modify_setting(|w| w.set_key_beep(false))?;
        assert_eq!(result, None);
        Ok(())
    }

    #[test]
    fn modify_setting_rejects_and_rolls_back_multi_byte_mutation() -> TestResult {
        use crate::types::LinkedVolumeLevel;

        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        let volume = LinkedVolumeLevel::fixed(5)?;
        let error = image
            .modify_setting(|writer| {
                writer.set_key_beep(true)?;
                writer.set_beep_volume(volume)
            })
            .err()
            .ok_or("two-byte mutation should be rejected")?;

        assert!(matches!(
            error,
            MemoryError::MultipleSettingBytesChanged {
                first: 0x1071,
                second: 0x1072
            }
        ));
        assert_eq!(image.as_raw().get(0x1071..=0x1072), Some([0, 0].as_slice()));
        Ok(())
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

    #[test]
    fn read_region_returns_none_when_end_overflows_usize() -> TestResult {
        let image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;

        assert_eq!(image.read_region(usize::MAX, 1), None);
        assert_eq!(image.read_region(1, usize::MAX), None);
        Ok(())
    }

    #[test]
    fn write_region_reports_end_overflow_before_slicing() -> TestResult {
        let mut image = MemoryImage::from_raw(vec![0u8; programming::TOTAL_SIZE])?;
        let error = image
            .write_region(usize::MAX, &[0xA5])
            .err()
            .ok_or("overflowing write should fail")?;

        assert_eq!(
            error,
            MemoryError::RegionEndOverflow {
                offset: usize::MAX,
                length: 1,
            }
        );
        Ok(())
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
