//! Typed access to the GPS settings region of the memory image.
//!
//! Kenwood's official MCP-D75 serializer places `GpsMenuData` at
//! `0x1100`-`0x11C0`. This span includes GPS receiver settings, track-log
//! settings, NMEA output selection, five My Position records, and the
//! active My Position selector.
//!
//! # Offset provenance
//!
//! The menu offsets come from the generated
//! [`MCP_D75_MENU_FIELDS`](super::MCP_D75_MENU_FIELDS) registry, produced
//! from the reviewed official MCP-D75 serializers. The scalar and track-log
//! anchor values are also pinned against the retained physical-radio MCP
//! image in `tests/fixtures/memory_dump.bin`.
//!
//! The My Position records at `0x1120..0x11C0` and selector at `0x11C0`
//! remain opaque because the retained evidence does not yet prove their
//! hemisphere polarity or selector semantics. No GPS waypoint-storage offset
//! has been verified; the retained image identifies `0x4D000` as paired-device
//! data, not a waypoint index.

use crate::types::GpsRadioMode;
use crate::types::aprs::PositionAmbiguity;
use crate::types::gps::{
    GpsBatterySaver, GpsSettings, NmeaSentences, TrackDistanceHundredths, TrackIntervalSeconds,
    TrackLogSettings, TrackRecordMethod,
};

/// First byte written by the official `GpsMenuData` serializer.
const GPS_MENU_OFFSET: usize = 0x1100;

/// Exclusive end of the official `GpsMenuData` field span.
const GPS_MENU_END: usize = 0x11C1;

/// Size of the official `GpsMenuData` field span.
const GPS_MENU_SIZE: usize = GPS_MENU_END - GPS_MENU_OFFSET;

// MCP-D75 serializer field offsets. Tests bind each constant to the generated
// registry and independently pin the literal addresses against a radio dump.

/// `gps.BuiltInGps` (1 byte, 0 = off, 1 = on).
const GPS_ENABLED_OFFSET: usize = 0x1100;

/// `gps.PositionAmbiguity` (1 byte, 0-4).
const GPS_POSITION_AMBIGUITY_OFFSET: usize = 0x1101;

/// `gps.OperatingMode` (1 byte, 0 = Normal, 1 = GPS Receiver).
const GPS_OPERATING_MODE_OFFSET: usize = 0x1102;

/// `gps.BatterySaver` (1 byte, 0-5).
const GPS_BATTERY_SAVER_OFFSET: usize = 0x1103;

/// `gps.PcOutput` (1 byte, 0 = off, 1 = on).
const GPS_PC_OUTPUT_OFFSET: usize = 0x1104;

/// `gps.Sentence_*` shared bit field (bits 0-5).
const GPS_NMEA_FLAGS_OFFSET: usize = 0x1105;

/// `gps.TrackLog` (1 byte, 0 = off, 1 = on).
const GPS_TRACK_LOG_OFFSET: usize = 0x1106;

/// `gps.RecodeMethod` (1 byte, 0 = Time, 1 = Distance, 2 = Beacon).
const GPS_TRACK_RECORD_METHOD_OFFSET: usize = 0x1108;

/// `gps.Interval` (little-endian u16 seconds, 2-1800).
const GPS_TRACK_INTERVAL_OFFSET: usize = 0x1110;

/// `gps.Distance` (little-endian u16 hundredths, 1-999).
const GPS_TRACK_DISTANCE_OFFSET: usize = 0x1112;

/// An invalid or unavailable value in the GPS region of an MCP image.
///
/// Memory access is deliberately strict: damaged, truncated, or incompatible
/// images are reported instead of being silently converted into plausible
/// settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GpsValueError {
    /// The image did not contain a complete required byte range.
    MissingRange {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute starting byte offset in the MCP image.
        offset: usize,
        /// Required byte count.
        len: usize,
    },
    /// A stored byte was outside the setting's declared domain.
    InvalidByte {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute byte offset in the MCP image.
        offset: usize,
        /// Invalid stored byte.
        value: u8,
        /// Human-readable accepted domain.
        detail: &'static str,
    },
    /// A stored little-endian unsigned value was outside its domain.
    InvalidU16 {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute starting byte offset in the MCP image.
        offset: usize,
        /// Invalid decoded value.
        value: u16,
        /// Human-readable accepted domain.
        detail: &'static str,
    },
    /// A stored little-endian signed value was outside its domain.
    ///
    /// This form is reserved for future verified GPS fields whose storage is
    /// signed; the quarantined My Position records are not decoded today.
    InvalidI32 {
        /// Registry setting name.
        setting: &'static str,
        /// Absolute starting byte offset in the MCP image.
        offset: usize,
        /// Invalid decoded value.
        value: i32,
        /// Human-readable accepted domain.
        detail: &'static str,
    },
}

impl std::fmt::Display for GpsValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRange {
                setting,
                offset,
                len,
            } => write!(
                f,
                "{setting} needs {len} byte(s) at MCP offset 0x{offset:04X}, but the range is missing"
            ),
            Self::InvalidByte {
                setting,
                offset,
                value,
                detail,
            } => write!(
                f,
                "{setting} has invalid byte {value} at MCP offset 0x{offset:04X} ({detail})"
            ),
            Self::InvalidU16 {
                setting,
                offset,
                value,
                detail,
            } => write!(
                f,
                "{setting} has invalid u16 value {value} at MCP offset 0x{offset:04X} ({detail})"
            ),
            Self::InvalidI32 {
                setting,
                offset,
                value,
                detail,
            } => write!(
                f,
                "{setting} has invalid i32 value {value} at MCP offset 0x{offset:04X} ({detail})"
            ),
        }
    }
}

impl std::error::Error for GpsValueError {}

/// Read-only access to verified GPS settings fields.
///
/// My Position records, their selector, and waypoint storage intentionally
/// have no typed accessors until their semantics are independently verified.
#[derive(Debug)]
pub struct GpsAccess<'a> {
    image: &'a [u8],
}

impl<'a> GpsAccess<'a> {
    /// Create a new GPS accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the opaque official `GpsMenuData` serializer span.
    ///
    /// Returns bytes `0x1100..0x11C1`, including gaps and reserved records,
    /// unchanged. Callers must not infer semantics beyond the typed accessors.
    #[must_use]
    pub fn menu_region(&self) -> Option<&[u8]> {
        self.image.get(GPS_MENU_OFFSET..GPS_MENU_END)
    }

    /// Get the size of the official `GpsMenuData` field span in bytes.
    #[must_use]
    pub const fn menu_region_size(&self) -> usize {
        GPS_MENU_SIZE
    }

    /// Read the built-in GPS enabled setting.
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] if `gps.BuiltInGps` is missing or is not
    /// exactly `0` or `1`.
    pub fn gps_enabled(&self) -> Result<bool, GpsValueError> {
        self.strict_bool("gps.BuiltInGps", GPS_ENABLED_OFFSET)
    }

    /// Read the GPS PC-output setting.
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] if `gps.PcOutput` is missing or is not
    /// exactly `0` or `1`.
    pub fn pc_output(&self) -> Result<bool, GpsValueError> {
        self.strict_bool("gps.PcOutput", GPS_PC_OUTPUT_OFFSET)
    }

    /// Read the two fields represented by the live `GP` CAT command.
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] if either stored boolean is missing or
    /// malformed.
    pub fn settings(&self) -> Result<GpsSettings, GpsValueError> {
        Ok(GpsSettings::new(self.gps_enabled()?, self.pc_output()?))
    }

    /// Read GPS/Radio operating mode (`gps.OperatingMode`).
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] for a missing byte or a value outside `0..=1`.
    pub fn operating_mode(&self) -> Result<GpsRadioMode, GpsValueError> {
        let value = self.byte("gps.OperatingMode", GPS_OPERATING_MODE_OFFSET)?;
        GpsRadioMode::try_from(value).map_err(|_| GpsValueError::InvalidByte {
            setting: "gps.OperatingMode",
            offset: GPS_OPERATING_MODE_OFFSET,
            value,
            detail: "expected 0..=1",
        })
    }

    /// Read the GPS battery-saver interval (`gps.BatterySaver`).
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] for a missing byte or a value outside `0..=5`.
    pub fn battery_saver(&self) -> Result<GpsBatterySaver, GpsValueError> {
        let value = self.byte("gps.BatterySaver", GPS_BATTERY_SAVER_OFFSET)?;
        GpsBatterySaver::try_from(value).map_err(|_| GpsValueError::InvalidByte {
            setting: "gps.BatterySaver",
            offset: GPS_BATTERY_SAVER_OFFSET,
            value,
            detail: "expected 0..=5",
        })
    }

    /// Read GPS position ambiguity (`gps.PositionAmbiguity`).
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] for a missing byte or a value outside `0..=4`.
    pub fn position_ambiguity(&self) -> Result<PositionAmbiguity, GpsValueError> {
        let value = self.byte("gps.PositionAmbiguity", GPS_POSITION_AMBIGUITY_OFFSET)?;
        PositionAmbiguity::try_from(value).map_err(|_| GpsValueError::InvalidByte {
            setting: "gps.PositionAmbiguity",
            offset: GPS_POSITION_AMBIGUITY_OFFSET,
            value,
            detail: "expected 0..=4",
        })
    }

    /// Read the nonempty NMEA sentence selection (`gps.Sentence_*`).
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] if the byte is missing, no sentence is
    /// selected, or a reserved bit is set.
    pub fn nmea_sentences(&self) -> Result<NmeaSentences, GpsValueError> {
        let value = self.byte("gps.Sentence_*", GPS_NMEA_FLAGS_OFFSET)?;
        NmeaSentences::try_from(value).map_err(|_| GpsValueError::InvalidByte {
            setting: "gps.Sentence_*",
            offset: GPS_NMEA_FLAGS_OFFSET,
            value,
            detail: "expected a nonempty selection using only bits 0..=5",
        })
    }

    /// Read the complete track-log settings.
    ///
    /// # Errors
    ///
    /// Returns [`GpsValueError`] if any field is missing or outside its
    /// declared storage domain.
    pub fn track_log(&self) -> Result<TrackLogSettings, GpsValueError> {
        let enabled = self.strict_bool("gps.TrackLog", GPS_TRACK_LOG_OFFSET)?;

        let method_value = self.byte("gps.RecodeMethod", GPS_TRACK_RECORD_METHOD_OFFSET)?;
        let record_method =
            TrackRecordMethod::try_from(method_value).map_err(|_| GpsValueError::InvalidByte {
                setting: "gps.RecodeMethod",
                offset: GPS_TRACK_RECORD_METHOD_OFFSET,
                value: method_value,
                detail: "expected 0..=2",
            })?;

        let interval_value = self.u16_le("gps.Interval", GPS_TRACK_INTERVAL_OFFSET)?;
        let interval =
            TrackIntervalSeconds::new(interval_value).map_err(|_| GpsValueError::InvalidU16 {
                setting: "gps.Interval",
                offset: GPS_TRACK_INTERVAL_OFFSET,
                value: interval_value,
                detail: "expected 2..=1800 seconds",
            })?;

        let distance_value = self.u16_le("gps.Distance", GPS_TRACK_DISTANCE_OFFSET)?;
        let distance = TrackDistanceHundredths::new(distance_value).map_err(|_| {
            GpsValueError::InvalidU16 {
                setting: "gps.Distance",
                offset: GPS_TRACK_DISTANCE_OFFSET,
                value: distance_value,
                detail: "expected 1..=999 hundredths of the selected unit",
            }
        })?;

        Ok(TrackLogSettings::new(
            enabled,
            record_method,
            interval,
            distance,
        ))
    }

    fn strict_bool(&self, setting: &'static str, offset: usize) -> Result<bool, GpsValueError> {
        let value = self.byte(setting, offset)?;
        match value {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(GpsValueError::InvalidByte {
                setting,
                offset,
                value,
                detail: "expected 0 or 1",
            }),
        }
    }

    fn byte(&self, setting: &'static str, offset: usize) -> Result<u8, GpsValueError> {
        self.image
            .get(offset)
            .copied()
            .ok_or(GpsValueError::MissingRange {
                setting,
                offset,
                len: 1,
            })
    }

    fn u16_le(&self, setting: &'static str, offset: usize) -> Result<u16, GpsValueError> {
        let end = offset.checked_add(2).ok_or(GpsValueError::MissingRange {
            setting,
            offset,
            len: 2,
        })?;
        let bytes = self
            .image
            .get(offset..end)
            .ok_or(GpsValueError::MissingRange {
                setting,
                offset,
                len: 2,
            })?;
        let wire: [u8; 2] = bytes
            .try_into()
            .unwrap_or_else(|_| unreachable!("two-byte range must convert to a two-byte array"));
        Ok(u16::from_le_bytes(wire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::menu_fields::menu_field;
    use crate::memory::{Endian, FieldCodec};
    use crate::protocol::programming::TOTAL_SIZE;
    use crate::types::gps::NmeaSentence;

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type BoxErr = Box<dyn std::error::Error>;

    fn set_byte(image: &mut [u8], offset: usize, value: u8) -> Result<(), BoxErr> {
        let image_len = image.len();
        *image
            .get_mut(offset)
            .ok_or_else(|| format!("offset 0x{offset:X} is outside image length {image_len}"))? =
            value;
        Ok(())
    }

    fn set_u16(image: &mut [u8], offset: usize, value: u16) -> Result<(), BoxErr> {
        let end = offset + 2;
        let image_len = image.len();
        image
            .get_mut(offset..end)
            .ok_or_else(|| {
                format!("range 0x{offset:X}..0x{end:X} is outside image length {image_len}")
            })?
            .copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn valid_image() -> Result<Vec<u8>, BoxErr> {
        let mut image = vec![0_u8; TOTAL_SIZE];
        set_byte(&mut image, GPS_NMEA_FLAGS_OFFSET, 0x11)?;
        set_u16(&mut image, GPS_TRACK_INTERVAL_OFFSET, 10)?;
        set_u16(&mut image, GPS_TRACK_DISTANCE_OFFSET, 1)?;
        Ok(image)
    }

    fn rejected_value<T>(result: &Result<T, GpsValueError>) -> Result<GpsValueError, BoxErr> {
        match result {
            Ok(_) => Err("malformed GPS value was accepted".into()),
            Err(error) => Ok(*error),
        }
    }

    #[test]
    fn gps_menu_region_uses_official_literal_span() -> TestResult {
        let mut image = valid_image()?;
        set_byte(&mut image, 0x1100, 0xA1)?;
        set_byte(&mut image, 0x11C0, 0xB2)?;
        set_byte(&mut image, 0x19000, 0xCC)?;

        let memory = crate::memory::MemoryImage::from_raw(image)?;
        let gps = memory.gps();
        let region = gps.menu_region().ok_or("GPS menu region missing")?;
        assert_eq!(gps.menu_region_size(), 0xC1);
        assert_eq!(region.len(), 0xC1);
        assert_eq!(region.first(), Some(&0xA1));
        assert_eq!(region.last(), Some(&0xB2));
        assert!(!region.contains(&0xCC));
        Ok(())
    }

    #[test]
    fn gps_accessors_use_official_literal_addresses() -> TestResult {
        let mut image = valid_image()?;
        set_byte(&mut image, 0x1100, 1)?;
        set_byte(&mut image, 0x1101, 4)?;
        set_byte(&mut image, 0x1102, 1)?;
        set_byte(&mut image, 0x1103, 5)?;
        set_byte(&mut image, 0x1104, 1)?;
        set_byte(&mut image, 0x1105, 0x15)?;
        set_byte(&mut image, 0x1106, 1)?;
        set_byte(&mut image, 0x1108, 2)?;
        set_u16(&mut image, 0x1110, 1800)?;
        set_u16(&mut image, 0x1112, 999)?;

        let memory = crate::memory::MemoryImage::from_raw(image)?;
        let gps = memory.gps();
        assert!(gps.gps_enabled()?);
        assert_eq!(gps.position_ambiguity()?, PositionAmbiguity::Level4);
        assert_eq!(gps.operating_mode()?, GpsRadioMode::GpsReceiver);
        assert_eq!(gps.battery_saver()?, GpsBatterySaver::Auto);
        assert!(gps.pc_output()?);
        assert_eq!(gps.nmea_sentences()?.bits(), 0x15);

        let settings = gps.settings()?;
        assert!(settings.enabled());
        assert!(settings.pc_output());

        let track = gps.track_log()?;
        assert!(track.enabled());
        assert_eq!(track.record_method(), TrackRecordMethod::Beacon);
        assert_eq!(track.interval().as_seconds(), 1800);
        assert_eq!(track.distance().as_hundredths(), 999);
        Ok(())
    }

    #[test]
    fn gps_constants_bind_official_registry_fields() -> TestResult {
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
            ("gps.TrackLog", GPS_TRACK_LOG_OFFSET, FieldCodec::Bool),
            (
                "gps.RecodeMethod",
                GPS_TRACK_RECORD_METHOD_OFFSET,
                FieldCodec::Byte { min: 0, max: 2 },
            ),
            (
                "gps.Interval",
                GPS_TRACK_INTERVAL_OFFSET,
                FieldCodec::Unsigned {
                    width: 2,
                    endian: Endian::Little,
                    min: 2,
                    max: 1800,
                },
            ),
            (
                "gps.Distance",
                GPS_TRACK_DISTANCE_OFFSET,
                FieldCodec::Unsigned {
                    width: 2,
                    endian: Endian::Little,
                    min: 1,
                    max: 999,
                },
            ),
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

    #[test]
    fn retained_radio_dump_matches_all_exposed_gps_fields() -> TestResult {
        let image = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/memory_dump.bin"
        ))?;
        let memory = crate::memory::MemoryImage::from_raw(image)?;
        let gps = memory.gps();

        assert!(!gps.gps_enabled()?);
        assert_eq!(gps.position_ambiguity()?, PositionAmbiguity::Level2);
        assert_eq!(gps.operating_mode()?, GpsRadioMode::Normal);
        assert_eq!(gps.battery_saver()?, GpsBatterySaver::EightMinutes);
        assert!(!gps.pc_output()?);
        assert_eq!(gps.nmea_sentences()?.bits(), 0x3F);

        let track = gps.track_log()?;
        assert!(!track.enabled());
        assert_eq!(track.record_method(), TrackRecordMethod::Time);
        assert_eq!(track.interval().as_seconds(), 10);
        assert_eq!(track.distance().as_hundredths(), 1);
        Ok(())
    }

    #[test]
    fn strict_scalar_domains_accept_every_declared_value() -> TestResult {
        let mut image = valid_image()?;

        for raw in 0..=1 {
            set_byte(&mut image, GPS_ENABLED_OFFSET, raw)?;
            set_byte(&mut image, GPS_PC_OUTPUT_OFFSET, raw)?;
            let gps = GpsAccess::new(&image);
            assert_eq!(gps.gps_enabled()?, raw == 1);
            assert_eq!(gps.pc_output()?, raw == 1);
        }
        for raw in 0..=4 {
            set_byte(&mut image, GPS_POSITION_AMBIGUITY_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image).position_ambiguity()?,
                PositionAmbiguity::try_from(raw)?
            );
        }
        for raw in 0..=1 {
            set_byte(&mut image, GPS_OPERATING_MODE_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image).operating_mode()?,
                GpsRadioMode::try_from(raw)?
            );
        }
        for raw in 0..=5 {
            set_byte(&mut image, GPS_BATTERY_SAVER_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image).battery_saver()?,
                GpsBatterySaver::try_from(raw)?
            );
        }
        for raw in 1..=0x3F {
            set_byte(&mut image, GPS_NMEA_FLAGS_OFFSET, raw)?;
            assert_eq!(GpsAccess::new(&image).nmea_sentences()?.bits(), raw);
        }
        Ok(())
    }

    #[test]
    fn nmea_sentence_selection_is_typed() -> TestResult {
        let mut image = valid_image()?;
        set_byte(&mut image, GPS_NMEA_FLAGS_OFFSET, 0x11)?;
        let sentences = GpsAccess::new(&image).nmea_sentences()?;
        assert!(sentences.contains(NmeaSentence::Gga));
        assert!(sentences.contains(NmeaSentence::Rmc));
        assert!(!sentences.contains(NmeaSentence::Gll));
        assert!(!sentences.contains(NmeaSentence::Gsa));
        assert!(!sentences.contains(NmeaSentence::Gsv));
        assert!(!sentences.contains(NmeaSentence::Vtg));
        Ok(())
    }

    #[test]
    fn track_log_accepts_every_method_and_numeric_boundary() -> TestResult {
        let mut image = valid_image()?;
        for raw in 0..=2 {
            set_byte(&mut image, GPS_TRACK_RECORD_METHOD_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image).track_log()?.record_method(),
                TrackRecordMethod::try_from(raw)?
            );
        }
        for raw in [TrackIntervalSeconds::MIN, TrackIntervalSeconds::MAX] {
            set_u16(&mut image, GPS_TRACK_INTERVAL_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image).track_log()?.interval().as_seconds(),
                raw
            );
        }
        set_u16(&mut image, GPS_TRACK_INTERVAL_OFFSET, 10)?;
        for raw in [TrackDistanceHundredths::MIN, TrackDistanceHundredths::MAX] {
            set_u16(&mut image, GPS_TRACK_DISTANCE_OFFSET, raw)?;
            assert_eq!(
                GpsAccess::new(&image)
                    .track_log()?
                    .distance()
                    .as_hundredths(),
                raw
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_scalar_bytes_are_errors_not_defaults() -> TestResult {
        const CASES: &[(usize, u8, GpsValueError)] = &[
            (
                GPS_ENABLED_OFFSET,
                2,
                GpsValueError::InvalidByte {
                    setting: "gps.BuiltInGps",
                    offset: GPS_ENABLED_OFFSET,
                    value: 2,
                    detail: "expected 0 or 1",
                },
            ),
            (
                GPS_PC_OUTPUT_OFFSET,
                2,
                GpsValueError::InvalidByte {
                    setting: "gps.PcOutput",
                    offset: GPS_PC_OUTPUT_OFFSET,
                    value: 2,
                    detail: "expected 0 or 1",
                },
            ),
            (
                GPS_POSITION_AMBIGUITY_OFFSET,
                5,
                GpsValueError::InvalidByte {
                    setting: "gps.PositionAmbiguity",
                    offset: GPS_POSITION_AMBIGUITY_OFFSET,
                    value: 5,
                    detail: "expected 0..=4",
                },
            ),
            (
                GPS_OPERATING_MODE_OFFSET,
                2,
                GpsValueError::InvalidByte {
                    setting: "gps.OperatingMode",
                    offset: GPS_OPERATING_MODE_OFFSET,
                    value: 2,
                    detail: "expected 0..=1",
                },
            ),
            (
                GPS_BATTERY_SAVER_OFFSET,
                6,
                GpsValueError::InvalidByte {
                    setting: "gps.BatterySaver",
                    offset: GPS_BATTERY_SAVER_OFFSET,
                    value: 6,
                    detail: "expected 0..=5",
                },
            ),
        ];

        for &(offset, raw, expected) in CASES {
            let mut image = valid_image()?;
            set_byte(&mut image, offset, raw)?;
            let gps = GpsAccess::new(&image);
            let actual = match offset {
                GPS_ENABLED_OFFSET => rejected_value(&gps.gps_enabled())?,
                GPS_PC_OUTPUT_OFFSET => rejected_value(&gps.pc_output())?,
                GPS_POSITION_AMBIGUITY_OFFSET => rejected_value(&gps.position_ambiguity())?,
                GPS_OPERATING_MODE_OFFSET => rejected_value(&gps.operating_mode())?,
                GPS_BATTERY_SAVER_OFFSET => rejected_value(&gps.battery_saver())?,
                unknown => {
                    return Err(
                        format!("test table contains unknown GPS offset 0x{unknown:X}").into(),
                    );
                }
            };
            assert_eq!(actual, expected);
        }
        Ok(())
    }

    #[test]
    fn malformed_nmea_values_are_errors() -> TestResult {
        for raw in [0, 0x40, 0x80, 0xFF] {
            let mut image = valid_image()?;
            set_byte(&mut image, GPS_NMEA_FLAGS_OFFSET, raw)?;
            assert_eq!(
                rejected_value(&GpsAccess::new(&image).nmea_sentences())?,
                GpsValueError::InvalidByte {
                    setting: "gps.Sentence_*",
                    offset: GPS_NMEA_FLAGS_OFFSET,
                    value: raw,
                    detail: "expected a nonempty selection using only bits 0..=5",
                }
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_track_log_values_are_errors() -> TestResult {
        let mut image = valid_image()?;
        set_byte(&mut image, GPS_TRACK_LOG_OFFSET, 2)?;
        assert_eq!(
            rejected_value(&GpsAccess::new(&image).track_log())?,
            GpsValueError::InvalidByte {
                setting: "gps.TrackLog",
                offset: GPS_TRACK_LOG_OFFSET,
                value: 2,
                detail: "expected 0 or 1",
            }
        );

        let mut image = valid_image()?;
        set_byte(&mut image, GPS_TRACK_RECORD_METHOD_OFFSET, 3)?;
        assert_eq!(
            rejected_value(&GpsAccess::new(&image).track_log())?,
            GpsValueError::InvalidByte {
                setting: "gps.RecodeMethod",
                offset: GPS_TRACK_RECORD_METHOD_OFFSET,
                value: 3,
                detail: "expected 0..=2",
            }
        );

        for raw in [1, 1801, u16::MAX] {
            let mut image = valid_image()?;
            set_u16(&mut image, GPS_TRACK_INTERVAL_OFFSET, raw)?;
            assert_eq!(
                rejected_value(&GpsAccess::new(&image).track_log())?,
                GpsValueError::InvalidU16 {
                    setting: "gps.Interval",
                    offset: GPS_TRACK_INTERVAL_OFFSET,
                    value: raw,
                    detail: "expected 2..=1800 seconds",
                }
            );
        }

        for raw in [0, 1000, u16::MAX] {
            let mut image = valid_image()?;
            set_u16(&mut image, GPS_TRACK_DISTANCE_OFFSET, raw)?;
            assert_eq!(
                rejected_value(&GpsAccess::new(&image).track_log())?,
                GpsValueError::InvalidU16 {
                    setting: "gps.Distance",
                    offset: GPS_TRACK_DISTANCE_OFFSET,
                    value: raw,
                    detail: "expected 1..=999 hundredths of the selected unit",
                }
            );
        }
        Ok(())
    }

    #[test]
    fn missing_scalar_and_u16_ranges_are_reported() -> TestResult {
        assert_eq!(
            rejected_value(&GpsAccess::new(&[]).gps_enabled())?,
            GpsValueError::MissingRange {
                setting: "gps.BuiltInGps",
                offset: GPS_ENABLED_OFFSET,
                len: 1,
            }
        );

        let image = vec![0_u8; GPS_TRACK_INTERVAL_OFFSET + 1];
        assert_eq!(
            rejected_value(&GpsAccess::new(&image).track_log())?,
            GpsValueError::MissingRange {
                setting: "gps.Interval",
                offset: GPS_TRACK_INTERVAL_OFFSET,
                len: 2,
            }
        );
        Ok(())
    }

    #[test]
    fn gps_value_errors_preserve_signed_raw_context() {
        let error = GpsValueError::InvalidI32 {
            setting: "future.signedGpsField",
            offset: 0x1120,
            value: -501,
            detail: "expected -500..=15000",
        };
        assert_eq!(
            error.to_string(),
            "future.signedGpsField has invalid i32 value -501 at MCP offset 0x1120 (expected -500..=15000)"
        );
    }
}
