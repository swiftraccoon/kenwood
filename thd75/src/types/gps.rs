//! GPS (Global Positioning System) settings and data types.
//!
//! The TH-D75 has a built-in GPS receiver that provides position data in
//! NMEA (National Marine Electronics Association) format. GPS data is used
//! for APRS position beaconing, D-STAR position reporting, waypoint
//! navigation, track logging, and manual position storage.
//!
//! The types in this module mirror individual TH-D75 storage or CAT domains.
//! They deliberately do not combine unrelated GPS, APRS, and D-STAR settings
//! into one aggregate.

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Top-level GPS settings
// ---------------------------------------------------------------------------

/// Live built-in GPS and PC-output state carried by the `GP` CAT command.
///
/// Other GPS menu settings have independent storage and wire domains and are
/// intentionally not part of this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpsSettings {
    enabled: bool,
    pc_output: bool,
}

impl GpsSettings {
    /// Creates the exact state represented by the two `GP` CAT fields.
    #[must_use]
    pub const fn new(enabled: bool, pc_output: bool) -> Self {
        Self { enabled, pc_output }
    }

    /// Returns whether the built-in GPS receiver is enabled.
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Returns whether the radio outputs GPS data to the selected PC port.
    #[must_use]
    pub const fn pc_output(self) -> bool {
        self.pc_output
    }

    /// Returns the documented TH-D75 factory state for Menu 400 and 405.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::new(true, false)
    }
}

// ---------------------------------------------------------------------------
// Battery saver
// ---------------------------------------------------------------------------

/// GPS receiver battery-saver interval (Menu 404).
///
/// The numeric choices are minutes of GPS off-time. `Auto` progressively
/// increases that off-time from one to eight minutes as documented by the
/// TH-D75 user manual.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpsBatterySaver {
    /// Battery saver disabled.
    Off = 0,
    /// One-minute off-time.
    OneMinute = 1,
    /// Two-minute off-time.
    TwoMinutes = 2,
    /// Four-minute off-time.
    FourMinutes = 3,
    /// Eight-minute off-time.
    EightMinutes = 4,
    /// Automatically increase the off-time from one to eight minutes.
    Auto = 5,
}

impl TryFrom<u8> for GpsBatterySaver {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::OneMinute),
            2 => Ok(Self::TwoMinutes),
            3 => Ok(Self::FourMinutes),
            4 => Ok(Self::EightMinutes),
            5 => Ok(Self::Auto),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "GPS battery saver",
                value,
                detail: "must be 0-5",
            }),
        }
    }
}

impl From<GpsBatterySaver> for u8 {
    fn from(mode: GpsBatterySaver) -> Self {
        mode as Self
    }
}

// ---------------------------------------------------------------------------
// NMEA sentences
// ---------------------------------------------------------------------------

/// A selectable NMEA 0183 sentence emitted by GPS PC output (Menu 406).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NmeaSentence {
    /// Global Positioning System Fix Data.
    Gga,
    /// Geographic Position (latitude and longitude).
    Gll,
    /// DOP and active satellites.
    Gsa,
    /// Satellites in view.
    Gsv,
    /// Recommended Minimum Specific GNSS Data.
    Rmc,
    /// Course over ground and ground speed.
    Vtg,
}

impl NmeaSentence {
    const fn bit(self) -> u8 {
        match self {
            Self::Gga => 1 << 0,
            Self::Gll => 1 << 1,
            Self::Gsa => 1 << 2,
            Self::Gsv => 1 << 3,
            Self::Rmc => 1 << 4,
            Self::Vtg => 1 << 5,
        }
    }

    /// Returns the three-letter sentence identifier without `$GP`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Gga => "GGA",
            Self::Gll => "GLL",
            Self::Gsa => "GSA",
            Self::Gsv => "GSV",
            Self::Rmc => "RMC",
            Self::Vtg => "VTG",
        }
    }
}

/// Nonempty NMEA sentence selection for GPS PC output.
///
/// Only bits 0 through 5 are defined by Menu 406 and the `GS` CAT command.
/// The radio's UI requires at least one sentence to remain selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NmeaSentences(u8);

impl NmeaSentences {
    const VALID_BITS: u8 = 0x3F;

    /// Returns all six supported NMEA sentences selected.
    #[must_use]
    pub const fn all() -> Self {
        Self(Self::VALID_BITS)
    }

    /// Returns the documented factory selection: GGA and RMC.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(NmeaSentence::Gga.bit() | NmeaSentence::Rmc.bit())
    }

    /// Returns the six-bit radio representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Returns whether `sentence` is selected.
    #[must_use]
    pub const fn contains(self, sentence: NmeaSentence) -> bool {
        self.0 & sentence.bit() != 0
    }

    /// Returns a selection with `sentence` enabled.
    #[must_use]
    pub const fn with(self, sentence: NmeaSentence) -> Self {
        Self(self.0 | sentence.bit())
    }

    /// Returns a selection with `sentence` disabled.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] when
    /// removing the sentence would leave the selection empty.
    pub const fn without(self, sentence: NmeaSentence) -> Result<Self, ValidationError> {
        let bits = self.0 & !sentence.bit();
        if bits == 0 {
            Err(ValidationError::SettingOutOfRange {
                name: "NMEA sentence selection",
                value: bits,
                detail: "at least one of bits 0-5 must be selected",
            })
        } else {
            Ok(Self(bits))
        }
    }

    /// Builds a typed selection from the six ordered fields carried by the
    /// `GS` protocol response: GGA, GLL, GSA, GSV, RMC, and VTG.
    pub(crate) const fn try_from_flags(
        [gga, gll, gsa, gsv, rmc, vtg]: [bool; 6],
    ) -> Result<Self, ValidationError> {
        let bits = (gga as u8)
            | (gll as u8) << 1
            | (gsa as u8) << 2
            | (gsv as u8) << 3
            | (rmc as u8) << 4
            | (vtg as u8) << 5;
        if bits == 0 {
            Err(ValidationError::SettingOutOfRange {
                name: "NMEA sentence selection",
                value: bits,
                detail: "at least one of bits 0-5 must be selected",
            })
        } else {
            Ok(Self(bits))
        }
    }
}

impl TryFrom<u8> for NmeaSentences {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 0 || value & !Self::VALID_BITS != 0 {
            Err(ValidationError::SettingOutOfRange {
                name: "NMEA sentence selection",
                value,
                detail: "must select at least one of bits 0-5 and set no reserved bits",
            })
        } else {
            Ok(Self(value))
        }
    }
}

impl From<NmeaSentences> for u8 {
    fn from(sentences: NmeaSentences) -> Self {
        sentences.bits()
    }
}

// ---------------------------------------------------------------------------
// Track log
// ---------------------------------------------------------------------------

/// Track log recording settings.
///
/// The TH-D75 records GPS track logs to the microSD card at
/// `/KENWOOD/TH-D75/GPS_LOG/` in NMEA format (`.nme` files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackLogSettings {
    enabled: bool,
    record_method: TrackRecordMethod,
    interval: TrackIntervalSeconds,
    distance: TrackDistanceHundredths,
}

impl TrackLogSettings {
    /// Creates track-log settings from independently validated fields.
    #[must_use]
    pub const fn new(
        enabled: bool,
        record_method: TrackRecordMethod,
        interval: TrackIntervalSeconds,
        distance: TrackDistanceHundredths,
    ) -> Self {
        Self {
            enabled,
            record_method,
            interval,
            distance,
        }
    }

    /// Returns whether track-log recording is enabled (Menu 410).
    #[must_use]
    pub const fn enabled(self) -> bool {
        self.enabled
    }

    /// Returns the acquisition method (Menu 412).
    #[must_use]
    pub const fn record_method(self) -> TrackRecordMethod {
        self.record_method
    }

    /// Returns the stored time interval (Menu 413).
    #[must_use]
    pub const fn interval(self) -> TrackIntervalSeconds {
        self.interval
    }

    /// Returns the stored distance in hundredths of the Menu 970 unit.
    #[must_use]
    pub const fn distance(self) -> TrackDistanceHundredths {
        self.distance
    }

    /// Returns the documented TH-D75 factory track-log settings.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::new(
            false,
            TrackRecordMethod::Time,
            TrackIntervalSeconds::new_unchecked(10),
            TrackDistanceHundredths::new_unchecked(1),
        )
    }
}

/// Track log recording trigger method.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackRecordMethod {
    /// Record at a fixed time interval.
    Time = 0,
    /// Record when the distance threshold is exceeded.
    Distance = 1,
    /// Record with APRS beacon transmissions.
    Beacon = 2,
}

impl TryFrom<u8> for TrackRecordMethod {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Time),
            1 => Ok(Self::Distance),
            2 => Ok(Self::Beacon),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "track record method",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<TrackRecordMethod> for u8 {
    fn from(method: TrackRecordMethod) -> Self {
        method as Self
    }
}

/// Track-log time interval in seconds (Menu 413, raw `2..=1800`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackIntervalSeconds(u16);

impl TrackIntervalSeconds {
    /// Minimum accepted interval in seconds.
    pub const MIN: u16 = 2;
    /// Maximum accepted interval in seconds.
    pub const MAX: u16 = 1800;

    const fn new_unchecked(seconds: u16) -> Self {
        Self(seconds)
    }

    /// Creates an interval in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `seconds` is in
    /// `2..=1800`.
    pub const fn new(seconds: u16) -> Result<Self, ValidationError> {
        if seconds >= Self::MIN && seconds <= Self::MAX {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "track-log time interval",
                value: seconds as i64,
                detail: "must be 2-1800 seconds",
            })
        }
    }

    /// Returns the interval in seconds, identical to its MCP value.
    #[must_use]
    pub const fn as_seconds(self) -> u16 {
        self.0
    }
}

impl From<TrackIntervalSeconds> for u16 {
    fn from(interval: TrackIntervalSeconds) -> Self {
        interval.as_seconds()
    }
}

/// Track-log distance in hundredths of the selected Menu 970 unit.
///
/// Raw `1` means `0.01`, while raw `999` means `9.99`. The unit is selected
/// elsewhere and may be miles, kilometres, or nautical miles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackDistanceHundredths(u16);

impl TrackDistanceHundredths {
    /// Minimum encoded distance (`0.01` selected units).
    pub const MIN: u16 = 1;
    /// Maximum encoded distance (`9.99` selected units).
    pub const MAX: u16 = 999;

    const fn new_unchecked(hundredths: u16) -> Self {
        Self(hundredths)
    }

    /// Creates a distance accepted by the radio.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `hundredths` is
    /// in `1..=999`.
    pub const fn new(hundredths: u16) -> Result<Self, ValidationError> {
        if hundredths >= Self::MIN && hundredths <= Self::MAX {
            Ok(Self(hundredths))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "track-log distance",
                value: hundredths as i64,
                detail: "must be 1-999 hundredths of the selected unit",
            })
        }
    }

    /// Returns the encoded hundredths of the selected unit.
    #[must_use]
    pub const fn as_hundredths(self) -> u16 {
        self.0
    }
}

impl From<TrackDistanceHundredths> for u16 {
    fn from(distance: TrackDistanceHundredths) -> Self {
        distance.as_hundredths()
    }
}

// ---------------------------------------------------------------------------
// My Position
// ---------------------------------------------------------------------------

/// One of the radio's five stored "My Position" values.
///
/// The TH-D75 provides 5 "My Position" slots ("My Position 1" through
/// "My Position 5") for storing known locations. These can be used as
/// manual position references when GPS is unavailable.
///
/// Per Operating Tips §5.14.4: the radio also has 100 general-purpose
/// position memory slots (separate from these 5 "My Position" entries)
/// that store latitude, longitude, altitude, timestamp, name, and APRS
/// icon. A position memory entry can be copied to one of these "My
/// Position" slots (§5.14.5) or to an APRS Object for transmission.
#[derive(Debug, Clone, PartialEq)]
pub struct MyPosition {
    name: PositionName,
    latitude: aprs::Latitude,
    longitude: aprs::Longitude,
    altitude: PositionAltitudeMeters,
}

impl MyPosition {
    /// Creates a stored My Position value from validated components.
    #[must_use]
    pub const fn new(
        name: PositionName,
        latitude: aprs::Latitude,
        longitude: aprs::Longitude,
        altitude: PositionAltitudeMeters,
    ) -> Self {
        Self {
            name,
            latitude,
            longitude,
            altitude,
        }
    }

    /// Returns the fixed-width position name.
    #[must_use]
    pub const fn name(&self) -> &PositionName {
        &self.name
    }

    /// Returns the validated latitude.
    #[must_use]
    pub const fn latitude(&self) -> aprs::Latitude {
        self.latitude
    }

    /// Returns the validated longitude.
    #[must_use]
    pub const fn longitude(&self) -> aprs::Longitude {
        self.longitude
    }

    /// Returns the validated altitude.
    #[must_use]
    pub const fn altitude(&self) -> PositionAltitudeMeters {
        self.altitude
    }
}

/// Position memory name (up to 8 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PositionName(String);

impl PositionName {
    /// Maximum length of a position name.
    pub const MAX_LEN: usize = 8;

    /// Creates a new position name.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the name exceeds
    /// eight bytes, or [`ValidationError::InvalidTextByte`] at the first
    /// non-ASCII or NUL byte. Spaces are retained exactly as supplied.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        if name.len() > Self::MAX_LEN {
            return Err(ValidationError::TextLengthOutOfRange {
                name: "position name",
                len: name.len(),
                detail: "must be at most 8 encoded bytes",
            });
        }
        if let Some((offset, value)) = name
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii() || *byte == 0)
        {
            return Err(ValidationError::InvalidTextByte {
                name: "position name",
                offset,
                value,
                detail: "must contain only non-NUL ASCII bytes",
            });
        }
        Ok(Self(name.to_owned()))
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Altitude stored by a My Position record, in whole metres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionAltitudeMeters(i32);

impl PositionAltitudeMeters {
    /// Minimum altitude accepted by the stock MCP schema.
    pub const MIN: i32 = -500;
    /// Maximum altitude accepted by the stock MCP schema.
    pub const MAX: i32 = 15_000;

    /// Creates an altitude in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `meters` is in
    /// `-500..=15000`.
    pub const fn new(meters: i32) -> Result<Self, ValidationError> {
        if meters >= Self::MIN && meters <= Self::MAX {
            Ok(Self(meters))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "My Position altitude",
                value: meters as i64,
                detail: "must be -500 through 15000 metres",
            })
        }
    }

    /// Returns whole metres, identical to the signed MCP value.
    #[must_use]
    pub const fn as_meters(self) -> i32 {
        self.0
    }
}

impl From<PositionAltitudeMeters> for i32 {
    fn from(altitude: PositionAltitudeMeters) -> Self {
        altitude.as_meters()
    }
}

// ---------------------------------------------------------------------------
// Coordinate display format
// ---------------------------------------------------------------------------

/// Latitude/longitude display format.
///
/// Controls how coordinates are displayed on the radio's screen.
/// Configured in the "Units" menu section.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoordinateFormat {
    /// Degrees, decimal minutes (DD MM.MMM').
    Dmm = 0,
    /// Degrees, minutes, seconds (DD MM'SS").
    Dms = 1,
}

impl TryFrom<u8> for CoordinateFormat {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dmm),
            1 => Ok(Self::Dms),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "coordinate display format",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<CoordinateFormat> for u8 {
    fn from(format: CoordinateFormat) -> Self {
        format as Self
    }
}

/// Grid display format configured in the radio Units menu.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridSquareFormat {
    /// Maidenhead grid locator.
    Maidenhead = 0,
    /// Search-and-rescue conventional grid.
    SarConv = 1,
    /// Search-and-rescue cellular grid.
    SarCell = 2,
}

impl TryFrom<u8> for GridSquareFormat {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Maidenhead),
            1 => Ok(Self::SarConv),
            2 => Ok(Self::SarCell),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "grid display format",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<GridSquareFormat> for u8 {
    fn from(format: GridSquareFormat) -> Self {
        format as Self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_settings_model_only_the_gp_fields() {
        let settings = GpsSettings::new(false, true);
        assert!(!settings.enabled());
        assert!(settings.pc_output());

        let factory = GpsSettings::factory_default();
        assert!(factory.enabled());
        assert!(!factory.pc_output());
    }

    #[test]
    fn gps_battery_saver_matches_official_raw_domain() -> Result<(), Box<dyn std::error::Error>> {
        let battery_modes = [
            GpsBatterySaver::Off,
            GpsBatterySaver::OneMinute,
            GpsBatterySaver::TwoMinutes,
            GpsBatterySaver::FourMinutes,
            GpsBatterySaver::EightMinutes,
            GpsBatterySaver::Auto,
        ];
        for (raw, expected) in (0_u8..=5).zip(battery_modes) {
            assert_eq!(GpsBatterySaver::try_from(raw)?, expected);
            assert_eq!(u8::from(expected), raw);
        }
        assert!(GpsBatterySaver::try_from(6).is_err());
        Ok(())
    }

    #[test]
    fn nmea_sentences_enforce_exact_nonempty_six_bit_domain()
    -> Result<(), Box<dyn std::error::Error>> {
        let sentences = NmeaSentences::try_from(0x15)?;
        assert_eq!(sentences.bits(), 0x15);
        assert!(sentences.contains(NmeaSentence::Gga));
        assert!(!sentences.contains(NmeaSentence::Gll));
        assert!(sentences.contains(NmeaSentence::Gsa));
        assert!(sentences.contains(NmeaSentence::Rmc));
        assert_eq!(u8::from(sentences), 0x15);

        assert!(NmeaSentences::try_from(0).is_err());
        assert!(NmeaSentences::try_from(0x40).is_err());
        assert!(NmeaSentences::try_from(0x80).is_err());
        Ok(())
    }

    #[test]
    fn nmea_sentence_mutation_preserves_nonempty_invariant()
    -> Result<(), Box<dyn std::error::Error>> {
        let factory = NmeaSentences::factory_default();
        assert_eq!(factory.bits(), 0x11);
        assert!(factory.contains(NmeaSentence::Gga));
        assert!(factory.contains(NmeaSentence::Rmc));
        assert_eq!(NmeaSentences::all().bits(), 0x3F);

        let only_rmc = factory.without(NmeaSentence::Gga)?;
        assert_eq!(only_rmc.bits(), 0x10);
        assert!(only_rmc.without(NmeaSentence::Rmc).is_err());
        assert_eq!(only_rmc.with(NmeaSentence::Vtg).bits(), 0x30);

        let from_wire = NmeaSentences::try_from_flags([true, false, false, false, true, false])?;
        assert_eq!(from_wire, factory);
        assert!(NmeaSentences::try_from_flags([false; 6]).is_err());
        Ok(())
    }

    #[test]
    fn nmea_sentence_labels_are_protocol_identifiers() {
        assert_eq!(NmeaSentence::Gga.label(), "GGA");
        assert_eq!(NmeaSentence::Gll.label(), "GLL");
        assert_eq!(NmeaSentence::Gsa.label(), "GSA");
        assert_eq!(NmeaSentence::Gsv.label(), "GSV");
        assert_eq!(NmeaSentence::Rmc.label(), "RMC");
        assert_eq!(NmeaSentence::Vtg.label(), "VTG");
    }

    #[test]
    fn track_log_method_and_ranges_match_menu_410_through_414()
    -> Result<(), Box<dyn std::error::Error>> {
        let methods = [
            TrackRecordMethod::Time,
            TrackRecordMethod::Distance,
            TrackRecordMethod::Beacon,
        ];
        for (raw, expected) in (0_u8..=2).zip(methods) {
            assert_eq!(TrackRecordMethod::try_from(raw)?, expected);
            assert_eq!(u8::from(expected), raw);
        }
        assert!(TrackRecordMethod::try_from(3).is_err());

        assert!(TrackIntervalSeconds::new(1).is_err());
        assert_eq!(TrackIntervalSeconds::new(2)?.as_seconds(), 2);
        assert_eq!(TrackIntervalSeconds::new(1800)?.as_seconds(), 1800);
        assert!(TrackIntervalSeconds::new(1801).is_err());

        assert!(TrackDistanceHundredths::new(0).is_err());
        assert_eq!(TrackDistanceHundredths::new(1)?.as_hundredths(), 1);
        assert_eq!(TrackDistanceHundredths::new(999)?.as_hundredths(), 999);
        assert!(TrackDistanceHundredths::new(1000).is_err());
        Ok(())
    }

    #[test]
    fn track_log_factory_default_is_explicit_and_valid() {
        let track = TrackLogSettings::factory_default();
        assert!(!track.enabled());
        assert_eq!(track.record_method(), TrackRecordMethod::Time);
        assert_eq!(track.interval().as_seconds(), 10);
        assert_eq!(track.distance().as_hundredths(), 1);
    }

    #[test]
    fn position_name_valid() -> Result<(), Box<dyn std::error::Error>> {
        let name = PositionName::new("Home")?;
        assert_eq!(name.as_str(), "Home");
        Ok(())
    }

    #[test]
    fn position_name_max_length() -> Result<(), Box<dyn std::error::Error>> {
        let name = PositionName::new("12345678")?;
        assert_eq!(name.as_str(), "12345678");
        Ok(())
    }

    #[test]
    fn position_name_too_long() {
        assert!(PositionName::new("123456789").is_err());
    }

    #[test]
    fn position_name_rejects_unsafe_storage_bytes_and_preserves_spaces()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(PositionName::new("café").is_err());
        assert!(PositionName::new("ab\0cd").is_err());

        let spaced = PositionName::new(" A B ")?;
        assert_eq!(spaced.as_str(), " A B ");
        Ok(())
    }

    #[test]
    fn position_memory_reuses_validated_coordinates_and_altitude()
    -> Result<(), Box<dyn std::error::Error>> {
        let latitude = aprs::Latitude::new(35.6762)?;
        let longitude = aprs::Longitude::new(139.6503)?;
        let altitude = PositionAltitudeMeters::new(40)?;
        let memory = MyPosition::new(PositionName::new(" Tokyo ")?, latitude, longitude, altitude);

        assert_eq!(memory.name().as_str(), " Tokyo ");
        assert_eq!(memory.latitude(), latitude);
        assert_eq!(memory.longitude(), longitude);
        assert_eq!(memory.altitude(), altitude);
        assert!(PositionAltitudeMeters::new(-501).is_err());
        assert_eq!(PositionAltitudeMeters::new(-500)?.as_meters(), -500);
        assert_eq!(PositionAltitudeMeters::new(15_000)?.as_meters(), 15_000);
        assert!(PositionAltitudeMeters::new(15_001).is_err());
        Ok(())
    }

    #[test]
    fn coordinate_and_grid_formats_match_official_raw_domains()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(CoordinateFormat::try_from(0)?, CoordinateFormat::Dmm);
        assert_eq!(CoordinateFormat::try_from(1)?, CoordinateFormat::Dms);
        assert_eq!(u8::from(CoordinateFormat::Dmm), 0);
        assert_eq!(u8::from(CoordinateFormat::Dms), 1);
        assert!(CoordinateFormat::try_from(2).is_err());

        assert_eq!(GridSquareFormat::try_from(0)?, GridSquareFormat::Maidenhead);
        assert_eq!(GridSquareFormat::try_from(1)?, GridSquareFormat::SarConv);
        assert_eq!(GridSquareFormat::try_from(2)?, GridSquareFormat::SarCell);
        assert_eq!(u8::from(GridSquareFormat::Maidenhead), 0);
        assert_eq!(u8::from(GridSquareFormat::SarConv), 1);
        assert_eq!(u8::from(GridSquareFormat::SarCell), 2);
        assert!(GridSquareFormat::try_from(3).is_err());
        Ok(())
    }
}
