//! GPS (Global Positioning System) configuration and data types.
//!
//! The TH-D75 has a built-in GPS receiver that provides position data in
//! NMEA (National Marine Electronics Association) format. GPS data is used
//! for APRS position beaconing, D-STAR position reporting, waypoint
//! navigation, track logging, and manual position storage.
//!
//! These types model every GPS setting accessible through the TH-D75's
//! menu system (Chapter 13 of the user manual) and CAT commands (GP, GM, GS).

// ---------------------------------------------------------------------------
// Top-level GPS configuration
// ---------------------------------------------------------------------------

/// Complete GPS configuration for the TH-D75.
///
/// Covers all settings from the radio's GPS menu tree, including
/// receiver control, output format, track logging, and position memory.
/// Derived from the capability gap analysis features 95-109.
#[derive(Debug, Clone, PartialEq)]
pub struct GpsConfig {
    /// Built-in GPS receiver on/off.
    pub enabled: bool,
    /// GPS PC output mode (send NMEA data to the serial port).
    pub pc_output: bool,
    /// GPS operating mode.
    pub operating_mode: GpsOperatingMode,
    /// GPS battery saver (reduce GPS power consumption by cycling
    /// the receiver on and off).
    pub battery_saver: bool,
    /// NMEA sentence output selection (which sentences to include in
    /// PC output).
    pub sentence_output: NmeaSentences,
    /// Track log recording configuration.
    pub track_log: TrackLogConfig,
    /// Manual position memory slots (5 available: "My Position 1"
    /// through "My Position 5").
    pub my_positions: [PositionMemory; 5],
    /// Position ambiguity level (shared with APRS, but configured
    /// in GPS menu).
    pub position_ambiguity: GpsPositionAmbiguity,
    /// GPS data TX configuration (auto-transmit position on DV mode).
    pub data_tx: GpsDataTx,
    /// Target point for navigation (bearing/distance display).
    pub target_point: Option<TargetPoint>,
}

impl Default for GpsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pc_output: false,
            operating_mode: GpsOperatingMode::Standalone,
            battery_saver: false,
            sentence_output: NmeaSentences::default(),
            track_log: TrackLogConfig::default(),
            my_positions: Default::default(),
            position_ambiguity: GpsPositionAmbiguity::Full,
            data_tx: GpsDataTx::default(),
            target_point: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Operating mode
// ---------------------------------------------------------------------------

/// GPS receiver operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpsOperatingMode {
    /// Standalone GPS receiver (internal GPS only).
    Standalone,
    /// SBAS (Satellite Based Augmentation System) enabled.
    /// Uses WAAS/EGNOS/MSAS for improved accuracy.
    Sbas,
    /// Manual position entry (GPS receiver off, use stored coordinates).
    Manual,
}

impl TryFrom<u8> for GpsOperatingMode {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Standalone),
            1 => Ok(Self::Sbas),
            2 => Ok(Self::Manual),
            _ => Err("GPS operating mode out of range (must be 0-2)"),
        }
    }
}

// ---------------------------------------------------------------------------
// NMEA sentences
// ---------------------------------------------------------------------------

/// NMEA sentence output selection.
///
/// Controls which NMEA 0183 sentences are included when GPS data is
/// output to the PC serial port. Each sentence provides different
/// navigation data.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NmeaSentences {
    /// GGA -- Global Positioning System Fix Data.
    /// Contains time, position, fix quality, number of satellites, HDOP,
    /// altitude, and geoid separation.
    pub gga: bool,
    /// GLL -- Geographic Position (latitude/longitude).
    /// Contains position and time with status.
    pub gll: bool,
    /// GSA -- GPS DOP (Dilution of Precision) and Active Satellites.
    /// Contains fix mode, satellite PRNs, PDOP, HDOP, VDOP.
    pub gsa: bool,
    /// GSV -- GPS Satellites in View.
    /// Contains satellite PRN, elevation, azimuth, and SNR for each
    /// visible satellite.
    pub gsv: bool,
    /// RMC -- Recommended Minimum Specific GNSS Data.
    /// Contains time, status, position, speed, course, date, and
    /// magnetic variation. This is the most commonly used sentence.
    pub rmc: bool,
    /// VTG -- Course Over Ground and Ground Speed.
    /// Contains true/magnetic course and speed in knots/km/h.
    pub vtg: bool,
}

impl Default for NmeaSentences {
    fn default() -> Self {
        Self {
            gga: true,
            gll: true,
            gsa: true,
            gsv: true,
            rmc: true,
            vtg: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Track log
// ---------------------------------------------------------------------------

/// Track log recording configuration.
///
/// The TH-D75 records GPS track logs to the microSD card at
/// `/KENWOOD/TH-D75/GPS_LOG/` in NMEA format (`.nme` files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackLogConfig {
    /// Track log recording method.
    pub record_method: TrackRecordMethod,
    /// Recording interval in seconds (range 1-9999).
    /// Used when `record_method` is `Interval`.
    pub interval_seconds: u16,
    /// Recording distance in meters (range 10-9999).
    /// Used when `record_method` is `Distance`.
    pub distance_meters: u16,
}

impl Default for TrackLogConfig {
    fn default() -> Self {
        Self {
            record_method: TrackRecordMethod::Off,
            interval_seconds: 5,
            distance_meters: 100,
        }
    }
}

/// Track log recording trigger method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackRecordMethod {
    /// Track log recording disabled.
    Off,
    /// Record at a fixed time interval.
    Interval,
    /// Record when the distance threshold is exceeded.
    Distance,
}

impl TryFrom<u8> for TrackRecordMethod {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Interval),
            2 => Ok(Self::Distance),
            _ => Err("track record method out of range (must be 0-2)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Position memory
// ---------------------------------------------------------------------------

/// GPS position memory slot.
///
/// The TH-D75 provides 5 position memory slots ("My Position 1" through
/// "My Position 5") for storing known locations. These can be used as
/// manual position references when GPS is unavailable.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionMemory {
    /// Descriptive name for the position (up to 8 characters).
    pub name: PositionName,
    /// Latitude in decimal degrees (positive = North, negative = South).
    /// Range: -90.0 to +90.0.
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = East, negative = West).
    /// Range: -180.0 to +180.0.
    pub longitude: f64,
    /// Altitude in meters above mean sea level.
    pub altitude: f64,
}

impl Default for PositionMemory {
    fn default() -> Self {
        Self {
            name: PositionName::default(),
            latitude: 0.0,
            longitude: 0.0,
            altitude: 0.0,
        }
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
    /// Returns `None` if the name exceeds 8 characters.
    #[must_use]
    pub fn new(name: &str) -> Option<Self> {
        if name.len() <= Self::MAX_LEN {
            Some(Self(name.to_owned()))
        } else {
            None
        }
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Position ambiguity (GPS-specific)
// ---------------------------------------------------------------------------

/// GPS position ambiguity level.
///
/// Each level removes one digit of precision from the transmitted
/// position, progressively obscuring the exact location. Identical
/// in concept to APRS position ambiguity but configured via the GPS menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpsPositionAmbiguity {
    /// Full precision (approximately 60 feet).
    Full,
    /// 1 digit removed (approximately 1/10 mile).
    Level1,
    /// 2 digits removed (approximately 1 mile).
    Level2,
    /// 3 digits removed (approximately 10 miles).
    Level3,
    /// 4 digits removed (approximately 60 miles).
    Level4,
}

// ---------------------------------------------------------------------------
// GPS data TX
// ---------------------------------------------------------------------------

/// GPS data TX configuration for D-STAR mode.
///
/// Controls automatic transmission of GPS position data in D-STAR DV
/// frame headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GpsDataTx {
    /// Enable automatic GPS data transmission on DV mode.
    pub auto_tx: bool,
    /// Auto TX interval in seconds (range 1-9999).
    pub interval_seconds: u16,
}

impl Default for GpsDataTx {
    fn default() -> Self {
        Self {
            auto_tx: false,
            interval_seconds: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// GPS position (parsed from NMEA)
// ---------------------------------------------------------------------------

/// Parsed GPS position from the receiver.
///
/// Represents the current GPS fix data as parsed from NMEA sentences
/// (GGA, RMC, etc.). This is a read-only data type populated by the
/// GPS receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct GpsPosition {
    /// Latitude in decimal degrees (positive = North, negative = South).
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = East, negative = West).
    pub longitude: f64,
    /// Altitude above mean sea level in meters.
    pub altitude: f64,
    /// Ground speed in km/h.
    pub speed: f64,
    /// Course over ground in degrees (0.0 = true north, 90.0 = east).
    pub course: f64,
    /// GPS fix quality.
    pub fix: GpsFix,
    /// Number of satellites used in the fix.
    pub satellites: u8,
    /// Horizontal dilution of precision (HDOP). Lower is better.
    /// Typical values: 1.0 = excellent, 2.0 = good, 5.0 = moderate.
    pub hdop: f64,
    /// UTC timestamp in "`HHMMSSss`" format (hours, minutes, seconds,
    /// hundredths), or `None` if time is not available.
    pub timestamp: Option<String>,
    /// UTC date in "DDMMYY" format, or `None` if date is not available.
    pub date: Option<String>,
    /// Maidenhead grid square locator (4 or 6 characters).
    pub grid_square: Option<String>,
}

impl Default for GpsPosition {
    fn default() -> Self {
        Self {
            latitude: 0.0,
            longitude: 0.0,
            altitude: 0.0,
            speed: 0.0,
            course: 0.0,
            fix: GpsFix::NoFix,
            satellites: 0,
            hdop: 99.9,
            timestamp: None,
            date: None,
            grid_square: None,
        }
    }
}

/// GPS fix quality/type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpsFix {
    /// No fix available.
    NoFix,
    /// 2D fix (latitude and longitude only, no altitude).
    Fix2D,
    /// 3D fix (latitude, longitude, and altitude).
    Fix3D,
    /// Differential GPS fix (DGPS/SBAS-corrected position).
    DGps,
}

// ---------------------------------------------------------------------------
// Target point
// ---------------------------------------------------------------------------

/// Navigation target point.
///
/// When set, the radio displays bearing and distance from the current
/// GPS position to the target point. The firmware outputs `$GPWPL` NMEA
/// sentences for waypoint data (handler at `0xC00D0FA0`).
#[derive(Debug, Clone, PartialEq)]
pub struct TargetPoint {
    /// Target latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Target longitude in decimal degrees (positive = East).
    pub longitude: f64,
    /// Optional descriptive name for the target.
    pub name: Option<String>,
}

// ---------------------------------------------------------------------------
// Coordinate display format
// ---------------------------------------------------------------------------

/// Latitude/longitude display format.
///
/// Controls how coordinates are displayed on the radio's screen.
/// Configured in the "Units" menu section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CoordinateFormat {
    /// Degrees, minutes, seconds (DD MM'SS").
    Dms,
    /// Degrees, decimal minutes (DD MM.MMM').
    Dmm,
    /// Decimal degrees (DD.DDDDD).
    Dd,
}

/// Grid square format for Maidenhead locator display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GridSquareFormat {
    /// 4-character grid square (e.g. "EM85").
    Four,
    /// 6-character grid square (e.g. "`EM85qd`").
    Six,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gps_config_default() {
        let cfg = GpsConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.pc_output);
        assert_eq!(cfg.operating_mode, GpsOperatingMode::Standalone);
    }

    #[test]
    fn nmea_sentences_default_all_enabled() {
        let s = NmeaSentences::default();
        assert!(s.gga);
        assert!(s.gll);
        assert!(s.gsa);
        assert!(s.gsv);
        assert!(s.rmc);
        assert!(s.vtg);
    }

    #[test]
    fn track_log_default_off() {
        let tl = TrackLogConfig::default();
        assert_eq!(tl.record_method, TrackRecordMethod::Off);
        assert_eq!(tl.interval_seconds, 5);
        assert_eq!(tl.distance_meters, 100);
    }

    #[test]
    fn position_memory_default() {
        let pm = PositionMemory::default();
        assert_eq!(pm.name.as_str(), "");
        assert!((pm.latitude - 0.0).abs() < f64::EPSILON);
        assert!((pm.longitude - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn position_name_valid() {
        let name = PositionName::new("Home").unwrap();
        assert_eq!(name.as_str(), "Home");
    }

    #[test]
    fn position_name_max_length() {
        let name = PositionName::new("12345678").unwrap();
        assert_eq!(name.as_str(), "12345678");
    }

    #[test]
    fn position_name_too_long() {
        assert!(PositionName::new("123456789").is_none());
    }

    #[test]
    fn gps_position_default_no_fix() {
        let pos = GpsPosition::default();
        assert_eq!(pos.fix, GpsFix::NoFix);
        assert_eq!(pos.satellites, 0);
    }

    #[test]
    fn gps_data_tx_default() {
        let dtx = GpsDataTx::default();
        assert!(!dtx.auto_tx);
        assert_eq!(dtx.interval_seconds, 60);
    }

    #[test]
    fn gps_fix_variants() {
        assert_ne!(GpsFix::NoFix, GpsFix::Fix2D);
        assert_ne!(GpsFix::Fix2D, GpsFix::Fix3D);
        assert_ne!(GpsFix::Fix3D, GpsFix::DGps);
    }

    #[test]
    fn target_point_construction() {
        let tp = TargetPoint {
            latitude: 35.6762,
            longitude: 139.6503,
            name: Some("Tokyo".to_owned()),
        };
        assert!((tp.latitude - 35.6762).abs() < 0.0001);
    }
}
