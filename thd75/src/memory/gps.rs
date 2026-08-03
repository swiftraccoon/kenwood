//! Typed access to the GPS configuration region of the memory image.
//!
//! Kenwood's official MCP-D75 serializer places `GpsMenuData` at
//! `0x1100`-`0x11C0`. This span includes GPS receiver settings, five
//! My Position records, track-log configuration, NMEA output selection,
//! and the active My Position selector.
//!
//! # Offset provenance
//!
//! The menu offsets come from the generated
//! [`MCP_D75_MENU_FIELDS`](super::MCP_D75_MENU_FIELDS) registry, produced
//! from the reviewed official MCP-D75 serializers. The scalar anchor values
//! at `0x1100`-`0x1105` are also pinned against the retained physical-radio
//! MCP image in `tests/fixtures/memory_dump.bin`.
//!
//! The GPS channel-index and waypoint regions near `0x4D000` are separate
//! from `GpsMenuData`; their retained reverse-engineering evidence is
//! documented alongside those constants below.

use crate::types::gps::{GpsBatterySaver, GpsOperatingMode, GpsPositionAmbiguity};

/// First byte written by the official `GpsMenuData` serializer.
const GPS_MENU_OFFSET: usize = 0x1100;

/// Exclusive end of the official `GpsMenuData` field span.
///
/// The final public field is the one-byte `gps.MyPositionSelect` at
/// `0x11C0`.
const GPS_MENU_END: usize = 0x11C1;

/// Size of the official `GpsMenuData` field span.
const GPS_MENU_SIZE: usize = GPS_MENU_END - GPS_MENU_OFFSET;

// ---------------------------------------------------------------------------
// MCP-D75 serializer field offsets
//
// These absolute offsets are intentionally named after their generated
// registry fields. A test below binds every constant to the registry, while
// literal anchor tests independently reject the legacy 0x19000 guess.
// ---------------------------------------------------------------------------

/// `gps.BuiltInGps` (1 byte, 0 = off, 1 = on).
const GPS_ENABLED_OFFSET: usize = 0x1100;

/// `gps.PositionAmbiguity` (1 byte, 0-4 = Off through 4-Digit).
const GPS_POSITION_AMBIGUITY_OFFSET: usize = 0x1101;

/// `gps.OperatingMode` (1 byte, 0 = Normal, 1 = GPS Receiver).
const GPS_OPERATING_MODE_OFFSET: usize = 0x1102;

/// `gps.BatterySaver` (1 byte, 0-5 = Off, 1/2/4/8 minutes, Auto).
const GPS_BATTERY_SAVER_OFFSET: usize = 0x1103;

/// `gps.PcOutput` (1 byte, 0 = off, 1 = on).
const GPS_PC_OUTPUT_OFFSET: usize = 0x1104;

/// `gps.Sentence_*` shared byte (bit field:
/// bit 0 = GGA, bit 1 = GLL, bit 2 = GSA, bit 3 = GSV,
/// bit 4 = RMC, bit 5 = VTG).
const GPS_NMEA_FLAGS_OFFSET: usize = 0x1105;

// ---------------------------------------------------------------------------
// GPS channel index
//
// The GPS channel index at byte offset 0x4D000 contains 100 entries of
// 1 byte each. A value of 0xFF indicates an unused slot; other values
// are indices into the waypoint data area.
//
// Waypoint data for entry with index value V is located at:
//   (V + 0x2608) * 0x20
// Each waypoint record is 0x20 (32) bytes.
// ---------------------------------------------------------------------------

/// Byte offset of the GPS channel index (100 x 1 byte).
const GPS_CHANNEL_INDEX_OFFSET: usize = 0x4_D000;

/// Number of GPS channel index entries.
const GPS_CHANNEL_INDEX_COUNT: usize = 100;

/// Marker value for unused GPS channel index entries.
const GPS_INDEX_UNUSED: u8 = 0xFF;

/// Base offset for waypoint data address calculation.
///
/// Waypoint data address = `(index_value + GPS_WAYPOINT_BASE_INDEX) * GPS_WAYPOINT_RECORD_SIZE`.
const GPS_WAYPOINT_BASE_INDEX: usize = 0x2608;

/// Size of a single GPS waypoint record in bytes.
const GPS_WAYPOINT_RECORD_SIZE: usize = 0x20;

// ---------------------------------------------------------------------------
// GpsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the GPS configuration region.
///
/// Provides raw byte access and typed field accessors for the official
/// MCP-D75 GPS menu span and the separately mapped position-memory index.
///
/// # Known menu settings
///
/// - Built-in GPS on/off
/// - My Position (5 manual slots, each with lat/lon/alt)
/// - Position ambiguity setting
/// - GPS operating mode (Normal/GPS Receiver)
/// - GPS battery-saver interval
/// - PC output format (NMEA sentences enabled/disabled)
/// - Track log settings (record method, interval, distance)
#[derive(Debug)]
pub struct GpsAccess<'a> {
    image: &'a [u8],
}

impl<'a> GpsAccess<'a> {
    /// Create a new GPS accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the raw official `GpsMenuData` field span.
    ///
    /// Returns bytes `0x1100..0x11C1`: from `gps.BuiltInGps` through
    /// the final one-byte `gps.MyPositionSelect` field. Gaps and record
    /// padding inside that span are returned unchanged.
    #[must_use]
    pub fn menu_region(&self) -> Option<&[u8]> {
        self.image.get(GPS_MENU_OFFSET..GPS_MENU_END)
    }

    /// Read an arbitrary byte range from the image.
    ///
    /// The offset is an absolute MCP byte address. Returns `None` if
    /// the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        self.image.get(offset..end)
    }

    /// Get the size of the official `GpsMenuData` field span in bytes.
    #[must_use]
    pub const fn menu_region_size(&self) -> usize {
        GPS_MENU_SIZE
    }

    // -----------------------------------------------------------------------
    // Typed GPS accessors (official MCP-D75 serializer offsets)
    // -----------------------------------------------------------------------

    /// Read GPS enabled setting.
    ///
    /// MCP-D75 field `gps.BuiltInGps` at `0x1100`.
    #[must_use]
    pub fn gps_enabled(&self) -> bool {
        self.image.get(GPS_ENABLED_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read GPS PC output setting.
    ///
    /// MCP-D75 field `gps.PcOutput` at `0x1104`.
    #[must_use]
    pub fn pc_output(&self) -> bool {
        self.image
            .get(GPS_PC_OUTPUT_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read GPS operating mode (`gps.OperatingMode` at `0x1102`).
    ///
    /// The official domain is 0 = Normal and 1 = GPS Receiver. Invalid
    /// bytes are treated as Normal; the MCP-D75 writer constrains the field
    /// to its official 0-1 domain.
    #[must_use]
    pub fn operating_mode(&self) -> GpsOperatingMode {
        match self
            .image
            .get(GPS_OPERATING_MODE_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => GpsOperatingMode::GpsReceiver,
            _ => GpsOperatingMode::Normal,
        }
    }

    /// Read the GPS battery-saver interval (`gps.BatterySaver` at `0x1103`).
    ///
    /// Invalid bytes are treated as Off; the MCP-D75 writer constrains the
    /// field to its official 0-5 domain.
    #[must_use]
    pub fn battery_saver(&self) -> GpsBatterySaver {
        self.image
            .get(GPS_BATTERY_SAVER_OFFSET)
            .copied()
            .and_then(|raw| GpsBatterySaver::try_from(raw).ok())
            .unwrap_or(GpsBatterySaver::Off)
    }

    /// Read GPS position ambiguity level.
    ///
    /// MCP-D75 field `gps.PositionAmbiguity` at `0x1101`.
    #[must_use]
    pub fn position_ambiguity(&self) -> GpsPositionAmbiguity {
        match self
            .image
            .get(GPS_POSITION_AMBIGUITY_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => GpsPositionAmbiguity::Level1,
            2 => GpsPositionAmbiguity::Level2,
            3 => GpsPositionAmbiguity::Level3,
            4 => GpsPositionAmbiguity::Level4,
            _ => GpsPositionAmbiguity::Full,
        }
    }

    /// Read NMEA sentence output flags as a raw byte.
    ///
    /// Bit field: bit 0 = GGA, bit 1 = GLL, bit 2 = GSA, bit 3 = GSV,
    /// bit 4 = RMC, bit 5 = VTG. Returns `0x3F` (all enabled) if
    /// unreadable.
    ///
    /// MCP-D75 shared `gps.Sentence_*` byte at `0x1105`.
    #[must_use]
    pub fn nmea_sentence_flags(&self) -> u8 {
        self.image
            .get(GPS_NMEA_FLAGS_OFFSET)
            .copied()
            .unwrap_or(0x3F)
    }

    /// Check if a specific NMEA sentence is enabled.
    ///
    /// `bit` selects the sentence: 0 = GGA, 1 = GLL, 2 = GSA,
    /// 3 = GSV, 4 = RMC, 5 = VTG.
    ///
    /// MCP-D75 shared `gps.Sentence_*` byte at `0x1105`.
    #[must_use]
    pub fn nmea_sentence_enabled(&self, bit: u8) -> bool {
        if bit > 5 {
            return false;
        }
        (self.nmea_sentence_flags() >> bit) & 1 != 0
    }

    // -----------------------------------------------------------------------
    // GPS channel index accessors
    // -----------------------------------------------------------------------

    /// Get the raw GPS channel index (100 bytes at `0x4D000`).
    ///
    /// Each byte is either `0xFF` (unused) or an index into the waypoint
    /// data area.
    ///
    /// Returns `None` if the region extends past the image.
    #[must_use]
    pub fn channel_index_raw(&self) -> Option<&[u8]> {
        let end = GPS_CHANNEL_INDEX_OFFSET + GPS_CHANNEL_INDEX_COUNT;
        self.image.get(GPS_CHANNEL_INDEX_OFFSET..end)
    }

    /// Get the GPS channel index value for a given slot (0-99).
    ///
    /// Returns `None` if the slot is unused (`0xFF`) or out of range.
    /// Otherwise returns the waypoint data index.
    #[must_use]
    pub fn channel_index(&self, slot: u8) -> Option<u8> {
        let slot_usize = slot as usize;
        if slot_usize >= GPS_CHANNEL_INDEX_COUNT {
            return None;
        }
        let offset = GPS_CHANNEL_INDEX_OFFSET + slot_usize;
        let value = self.image.get(offset).copied()?;
        if value == GPS_INDEX_UNUSED {
            None
        } else {
            Some(value)
        }
    }

    /// Count the number of active (non-empty) GPS waypoint slots.
    ///
    /// Iterates the 100-entry GPS channel index and counts entries that
    /// are not `0xFF`.
    #[must_use]
    pub fn waypoint_count(&self) -> usize {
        (0..GPS_CHANNEL_INDEX_COUNT)
            .filter(|&i| {
                let offset = GPS_CHANNEL_INDEX_OFFSET + i;
                self.image
                    .get(offset)
                    .is_some_and(|&b| b != GPS_INDEX_UNUSED)
            })
            .count()
    }

    /// Get the raw waypoint record for a given channel index slot (0-99).
    ///
    /// Looks up the waypoint data index from the GPS channel index, then
    /// reads the 32-byte waypoint record at the calculated address:
    /// `(index_value + 0x2608) * 0x20`.
    ///
    /// Returns `None` if the slot is unused, out of range, or the record
    /// extends past the image.
    #[must_use]
    pub fn waypoint_raw(&self, slot: u8) -> Option<&[u8]> {
        let index_value = self.channel_index(slot)? as usize;
        let data_offset = (index_value + GPS_WAYPOINT_BASE_INDEX) * GPS_WAYPOINT_RECORD_SIZE;
        let end = data_offset + GPS_WAYPOINT_RECORD_SIZE;
        self.image.get(data_offset..end)
    }

    /// Read the name field from a GPS waypoint record (up to 8 characters).
    ///
    /// Returns an empty string if the slot is unused or the record cannot
    /// be read. The name is at offset 0x10 within the 32-byte record,
    /// 9 bytes (8 characters + null terminator). A first byte of `0xFE`
    /// indicates an unused name.
    #[must_use]
    pub fn waypoint_name(&self, slot: u8) -> String {
        let Some(record) = self.waypoint_raw(slot) else {
            return String::new();
        };

        // Name is at record offset 0x10, 9 bytes.
        let Some(name_bytes) = record.get(0x10..0x19) else {
            return String::new();
        };
        // 0xFE in the first byte means unused.
        if name_bytes.first().copied() == Some(0xFE) {
            return String::new();
        }
        let nul = name_bytes
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_bytes.len());
        let Some(trimmed) = name_bytes.get(..nul) else {
            return String::new();
        };
        String::from_utf8_lossy(trimmed).trim().to_owned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::FieldCodec;
    use crate::memory::menu_fields::menu_field;
    use crate::protocol::programming::TOTAL_SIZE;
    use crate::types::gps::{GpsBatterySaver, GpsOperatingMode, GpsPositionAmbiguity};

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

    fn make_gps_image() -> Vec<u8> {
        vec![0u8; TOTAL_SIZE]
    }

    #[test]
    fn gps_menu_region_accessible() -> TestResult {
        let image = vec![0xBB_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        let region = gps.menu_region().ok_or("gps.menu_region() returned None")?;
        assert_eq!(region.len(), GPS_MENU_SIZE);
        assert!(region.iter().all(|&b| b == 0xBB));
        Ok(())
    }

    #[test]
    fn gps_read_bytes() -> TestResult {
        let mut image = make_gps_image();
        write_slice(&mut image, GPS_MENU_OFFSET, &[0x01, 0x02, 0x03, 0x04])?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        let bytes = gps
            .read_bytes(GPS_MENU_OFFSET, 4)
            .ok_or("gps.read_bytes returned None")?;
        assert_eq!(bytes, &[0x01, 0x02, 0x03, 0x04]);
        Ok(())
    }

    #[test]
    fn gps_region_size() -> TestResult {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        assert_eq!(gps.menu_region_size(), 0xC1);
        Ok(())
    }

    /// Literal addresses deliberately do not use implementation constants or
    /// the generated registry. Poisoning the former 0x19000 guess makes this
    /// fail if any accessor ever regresses to that location.
    #[test]
    fn gps_scalar_accessors_use_official_literal_addresses() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, 0x1100, 1)?;
        set_byte(&mut image, 0x1101, 4)?;
        set_byte(&mut image, 0x1102, 1)?;
        set_byte(&mut image, 0x1103, 5)?;
        set_byte(&mut image, 0x1104, 1)?;
        set_byte(&mut image, 0x1105, 0x15)?;
        write_slice(&mut image, 0x19000, &[0, 0, 0, 0, 0, 0])?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        assert!(gps.gps_enabled());
        assert_eq!(gps.position_ambiguity(), GpsPositionAmbiguity::Level4);
        assert_eq!(gps.operating_mode(), GpsOperatingMode::GpsReceiver);
        assert_eq!(gps.battery_saver(), GpsBatterySaver::Auto);
        assert!(gps.pc_output());
        assert_eq!(gps.nmea_sentence_flags(), 0x15);
        Ok(())
    }

    /// Independent first/last-byte anchors pin the serializer span rather
    /// than merely checking a size constant against itself.
    #[test]
    fn gps_menu_region_uses_official_literal_span() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, 0x1100, 0xA1)?;
        set_byte(&mut image, 0x11C0, 0xB2)?;
        set_byte(&mut image, 0x19000, 0xCC)?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        let region = gps.menu_region().ok_or("GPS menu region missing")?;
        assert_eq!(region.len(), 0xC1);
        assert_eq!(region.first(), Some(&0xA1));
        assert_eq!(region.last(), Some(&0xB2));
        assert!(!region.contains(&0xCC));
        Ok(())
    }

    /// Cross-layer guard: hand-written accessors must remain bound to the
    /// reviewed official MCP-D75 registry fields and domains.
    #[test]
    fn gps_scalar_constants_bind_official_registry_fields() -> TestResult {
        const ANCHORS: &[(&str, usize, FieldCodec)] = &[
            ("gps.BuiltInGps", GPS_ENABLED_OFFSET, FieldCodec::Bool),
            (
                "gps.PositionAmbiguity",
                GPS_POSITION_AMBIGUITY_OFFSET,
                FieldCodec::Byte { min: 0, max: 4 },
            ),
            (
                "gps.OperatingMode",
                GPS_OPERATING_MODE_OFFSET,
                FieldCodec::Byte { min: 0, max: 1 },
            ),
            (
                "gps.BatterySaver",
                GPS_BATTERY_SAVER_OFFSET,
                FieldCodec::Byte { min: 0, max: 5 },
            ),
            ("gps.PcOutput", GPS_PC_OUTPUT_OFFSET, FieldCodec::Bool),
        ];
        const SENTENCE_BITS: &[(&str, u8)] = &[
            ("gps.Sentence_Gpgga", 0x01),
            ("gps.Sentence_Gpgll", 0x02),
            ("gps.Sentence_Gpgsa", 0x04),
            ("gps.Sentence_Gpgsv", 0x08),
            ("gps.Sentence_Gprmc", 0x10),
            ("gps.Sentence_Gpvtg", 0x20),
        ];

        for &(name, offset, codec) in ANCHORS {
            let field = menu_field(name).ok_or_else(|| format!("missing registry field {name}"))?;
            assert_eq!(field.descriptor.offset, offset, "{name} offset");
            assert_eq!(field.descriptor.codec, codec, "{name} codec");
        }

        for &(name, mask) in SENTENCE_BITS {
            let field = menu_field(name).ok_or_else(|| format!("missing registry field {name}"))?;
            assert_eq!(field.descriptor.offset, GPS_NMEA_FLAGS_OFFSET, "{name}");
            assert_eq!(
                field.descriptor.codec,
                FieldCodec::BitBool { mask },
                "{name}"
            );
        }
        Ok(())
    }

    /// The retained full MCP image is independent hardware evidence for all
    /// six scalar cells. These values differ from the bytes at 0x19000.
    #[test]
    fn retained_radio_dump_matches_gps_scalar_accessors() -> TestResult {
        let image = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/memory_dump.bin"
        ))?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        assert!(!gps.gps_enabled());
        assert_eq!(gps.position_ambiguity(), GpsPositionAmbiguity::Level2);
        assert_eq!(gps.operating_mode(), GpsOperatingMode::Normal);
        assert_eq!(gps.battery_saver(), GpsBatterySaver::EightMinutes);
        assert!(!gps.pc_output());
        assert_eq!(gps.nmea_sentence_flags(), 0x3F);
        Ok(())
    }

    #[test]
    fn gps_enabled() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_ENABLED_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.gps().gps_enabled());
        Ok(())
    }

    #[test]
    fn gps_enabled_off() -> TestResult {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.gps().gps_enabled());
        Ok(())
    }

    #[test]
    fn gps_pc_output() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_PC_OUTPUT_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.gps().pc_output());
        Ok(())
    }

    #[test]
    fn gps_operating_mode() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_OPERATING_MODE_OFFSET, 1)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::GpsReceiver);
        Ok(())
    }

    #[test]
    fn gps_operating_mode_invalid_defaults_to_normal() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_OPERATING_MODE_OFFSET, 2)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::Normal);
        Ok(())
    }

    #[test]
    fn gps_operating_mode_default() -> TestResult {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().operating_mode(), GpsOperatingMode::Normal);
        Ok(())
    }

    #[test]
    fn gps_battery_saver() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_BATTERY_SAVER_OFFSET, 4)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().battery_saver(), GpsBatterySaver::EightMinutes);
        Ok(())
    }

    #[test]
    fn gps_position_ambiguity() -> TestResult {
        let mut image = make_gps_image();
        set_byte(&mut image, GPS_POSITION_AMBIGUITY_OFFSET, 3)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().position_ambiguity(), GpsPositionAmbiguity::Level3);
        Ok(())
    }

    #[test]
    fn gps_position_ambiguity_default() -> TestResult {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().position_ambiguity(), GpsPositionAmbiguity::Full);
        Ok(())
    }

    #[test]
    fn gps_nmea_flags() -> TestResult {
        let mut image = make_gps_image();
        // Enable GGA (bit 0) and RMC (bit 4) = 0b00010001 = 0x11.
        set_byte(&mut image, GPS_NMEA_FLAGS_OFFSET, 0x11)?;
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        assert_eq!(gps.nmea_sentence_flags(), 0x11);
        assert!(gps.nmea_sentence_enabled(0)); // GGA
        assert!(!gps.nmea_sentence_enabled(1)); // GLL
        assert!(!gps.nmea_sentence_enabled(2)); // GSA
        assert!(!gps.nmea_sentence_enabled(3)); // GSV
        assert!(gps.nmea_sentence_enabled(4)); // RMC
        assert!(!gps.nmea_sentence_enabled(5)); // VTG
        Ok(())
    }

    #[test]
    fn gps_nmea_sentence_out_of_range() -> TestResult {
        let image = make_gps_image();
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(!mi.gps().nmea_sentence_enabled(6));
        assert!(!mi.gps().nmea_sentence_enabled(255));
        Ok(())
    }

    // -----------------------------------------------------------------------
    // GPS channel index tests
    // -----------------------------------------------------------------------

    #[test]
    fn gps_channel_index_raw_accessible() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        let gps = mi.gps();
        let index = gps
            .channel_index_raw()
            .ok_or("channel_index_raw returned None")?;
        assert_eq!(index.len(), GPS_CHANNEL_INDEX_COUNT);
        // All 0xFF = unused.
        assert!(index.iter().all(|&b| b == 0xFF));
        Ok(())
    }

    #[test]
    fn gps_channel_index_unused() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.gps().channel_index(0).is_none());
        assert!(mi.gps().channel_index(99).is_none());
        Ok(())
    }

    #[test]
    fn gps_channel_index_out_of_range() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.gps().channel_index(100).is_none());
        assert!(mi.gps().channel_index(255).is_none());
        Ok(())
    }

    #[test]
    fn gps_channel_index_populated() -> TestResult {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        // Set slot 0 to waypoint index 5.
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET, 5)?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().channel_index(0), Some(5));
        Ok(())
    }

    #[test]
    fn gps_waypoint_count_all_empty() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().waypoint_count(), 0);
        Ok(())
    }

    #[test]
    fn gps_waypoint_count_with_entries() -> TestResult {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        // Set 3 slots as used.
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET, 0)?;
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET + 1, 1)?;
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET + 50, 10)?;

        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().waypoint_count(), 3);
        Ok(())
    }

    #[test]
    fn gps_waypoint_raw_empty_slot() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert!(mi.gps().waypoint_raw(0).is_none());
        Ok(())
    }

    #[test]
    fn gps_waypoint_raw_populated() -> TestResult {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET, 0)?; // Waypoint index 0.
        // Waypoint data at (0 + 0x2608) * 0x20 = 0x4C100.
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            write_slice(&mut image, wp_offset, &[0x01, 0x02, 0x03, 0x04])?;
            let mi = crate::memory::MemoryImage::from_raw(image)?;
            let gps = mi.gps();
            let raw = gps.waypoint_raw(0).ok_or("waypoint_raw(0) returned None")?;
            assert_eq!(raw.len(), GPS_WAYPOINT_RECORD_SIZE);
            assert_eq!(
                raw.get(..4).ok_or("waypoint raw too short")?,
                &[0x01, 0x02, 0x03, 0x04]
            );
        }
        Ok(())
    }

    #[test]
    fn gps_waypoint_name() -> TestResult {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET, 0)?;
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            // Write name at waypoint record offset 0x10.
            write_slice(&mut image, wp_offset + 0x10, b"HOME\0\0\0\0\0")?;
            let mi = crate::memory::MemoryImage::from_raw(image)?;
            assert_eq!(mi.gps().waypoint_name(0), "HOME");
        }
        Ok(())
    }

    #[test]
    fn gps_waypoint_name_empty_slot() -> TestResult {
        let image = vec![0xFF_u8; TOTAL_SIZE];
        let mi = crate::memory::MemoryImage::from_raw(image)?;
        assert_eq!(mi.gps().waypoint_name(0), "");
        Ok(())
    }

    #[test]
    fn gps_waypoint_name_unused_marker() -> TestResult {
        let mut image = vec![0xFF_u8; TOTAL_SIZE];
        set_byte(&mut image, GPS_CHANNEL_INDEX_OFFSET, 0)?;
        let wp_offset = GPS_WAYPOINT_BASE_INDEX * GPS_WAYPOINT_RECORD_SIZE;
        if wp_offset + GPS_WAYPOINT_RECORD_SIZE <= image.len() {
            // 0xFE as first byte of name = unused.
            set_byte(&mut image, wp_offset + 0x10, 0xFE)?;
            let mi = crate::memory::MemoryImage::from_raw(image)?;
            assert_eq!(mi.gps().waypoint_name(0), "");
        }
        Ok(())
    }
}
