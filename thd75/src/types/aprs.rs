//! APRS (Automatic Packet Reporting System) configuration types.
//!
//! APRS is a tactical real-time digital communications protocol used by ham
//! radio operators for position reporting, messaging, and telemetry. The
//! TH-D75 supports APRS on VHF with features including position beaconing,
//! two-way messaging, `SmartBeaconing`, digipeater path configuration,
//! packet filtering, and QSY information exchange.
//!
//! # QSY function (per Operating Tips §2.3.3-§2.3.5)
//!
//! APRS beacons can embed a voice frequency (QSY information) so that
//! other stations can tune directly to a voice channel. In FM mode, the
//! beacon includes the current Band A or B voice frequency. In D-STAR DR
//! mode, the beacon also includes the repeater callsign; in DV mode, only
//! the frequency is included. Per Operating Tips §2.3.4.
//!
//! QSY display distance can be restricted via Menu No. 523 (per Operating
//! Tips §2.3.5), limiting which QSY beacons are shown based on the
//! transmitting station's distance from the receiver.
//!
//! # Fixed-position beacon during GPS track logging (per Operating Tips §2.3.6)
//!
//! When GPS track logging is active, APRS beacons can be transmitted from
//! a fixed position (set via Menu No. 401) instead of the live GPS position.
//! This is useful when operating from a known location while still logging
//! a GPS track.
//!
//! # Digipeated beacon registration (per Operating Tips §2.3.7)
//!
//! Beacons received via digipeaters are registered in the station list.
//! The station list shows the digipeater path used.
//!
//! # `VoiceAlert` (per Operating Tips §5.3)
//!
//! `VoiceAlert` is a CTCSS-based mechanism: APRS beacons are transmitted
//! with a CTCSS tone so that stations monitoring the APRS frequency with
//! matching tone squelch hear an audible alert, enabling quick voice
//! contact. Menu No. 910 controls the balance between `VoiceAlert` audio
//! and normal APRS audio.
//!
//! These types model every APRS setting accessible through the TH-D75's
//! menu system (Chapter 14 of the user manual) and MCP programming memory
//! (pages 0x0151+ in the memory map).

use crate::types::{settings::SpeedDistanceUnit, tone::ToneCode};

// ---------------------------------------------------------------------------
// Top-level APRS configuration
// ---------------------------------------------------------------------------

/// Complete APRS configuration for the TH-D75.
///
/// Covers all settings from the radio's APRS menu tree, including station
/// identity, beaconing, messaging, filtering, digipeating, and notification
/// options. Derived from the capability gap analysis features 63-94.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsConfig {
    /// APRS station callsign with optional SSID (up to 9 characters,
    /// e.g. "N0CALL-9"). Stored in MCP memory at the APRS settings region.
    pub my_callsign: AprsCallsign,
    /// APRS map icon (symbol table + symbol code pair).
    pub icon: AprsIcon,
    /// Position comment (selected from 15 predefined phrases).
    pub position_comment: PositionComment,
    /// Status text slots (5 configurable messages, up to 62 characters each).
    pub status_texts: [StatusText; 5],
    /// Active status text slot index (0-4).
    pub active_status_text: u8,
    /// Digipeater packet path configuration.
    pub packet_path: PacketPath,
    /// APRS data speed (1200 or 9600 bps).
    pub data_speed: AprsDataSpeed,
    /// Band used for APRS data transmission.
    pub data_band: AprsBand,
    /// DCD (Data Carrier Detect) sense mode.
    pub dcd_sense: DcdSense,
    /// TX delay before packet transmission (Menu No. 508).
    pub tx_delay: TxDelay,
    /// Beacon transmission control settings.
    pub beacon_control: BeaconControl,
    /// `SmartBeaconing` configuration (speed-adaptive beaconing).
    pub smart_beaconing: McpSmartBeaconingConfig,
    /// Independent APRS frequency, PTT, and APRS-key locks.
    pub aprs_lock: AprsLock,
    /// Position ambiguity level (0 = full precision, 1-4 = progressively
    /// less precise, each level removes one decimal digit).
    pub position_ambiguity: PositionAmbiguity,
    /// Waypoint output configuration.
    pub waypoint: WaypointConfig,
    /// Packet filter settings.
    pub packet_filter: PacketFilter,
    /// Message-composition clipboard phrases (Menu No. 560).
    pub user_phrases: [UserPhrase; 20],
    /// Auto-reply message configuration.
    pub auto_reply: AutoReplyConfig,
    /// Notification sound configuration.
    pub notification: NotificationConfig,
    /// Digipeater configuration.
    pub digipeat: DigipeatConfig,
    /// QSY (frequency change) information configuration.
    pub qsy: QsyConfig,
    /// Enable APRS object functions (transmit/edit objects).
    pub object_functions: bool,
    /// Voice alert (transmit CTCSS tone with APRS packets to alert
    /// nearby stations monitoring with tone squelch).
    pub voice_alert: VoiceAlertConfig,
    /// Message group code filter string (up to 9 characters).
    pub message_group_code: GroupCode,
    /// Bulletin group code filter string (up to 9 characters).
    pub bulletin_group_code: GroupCode,
    /// NAVITRA (navigation/tracking) settings.
    pub navitra: NavitraConfig,
    /// APRS network identifier.
    pub network: AprsNetwork,
    /// Display area setting for incoming APRS packets.
    pub display_area: DisplayArea,
    /// Interrupt time for incoming APRS data display (seconds).
    pub interrupt_time: InterruptTime,
    /// APRS voice announcement on receive.
    pub aprs_voice: bool,
}

impl Default for AprsConfig {
    fn default() -> Self {
        Self {
            my_callsign: AprsCallsign::default(),
            icon: AprsIcon::default(),
            position_comment: PositionComment::OffDuty,
            status_texts: Default::default(),
            active_status_text: 0,
            packet_path: PacketPath::default(),
            data_speed: AprsDataSpeed::Bps1200,
            data_band: AprsBand::A,
            dcd_sense: DcdSense::Both,
            tx_delay: TxDelay::default(),
            beacon_control: BeaconControl::default(),
            smart_beaconing: McpSmartBeaconingConfig::default(),
            aprs_lock: AprsLock::default(),
            position_ambiguity: PositionAmbiguity::Full,
            waypoint: WaypointConfig::default(),
            packet_filter: PacketFilter::default(),
            user_phrases: Default::default(),
            auto_reply: AutoReplyConfig::default(),
            notification: NotificationConfig::default(),
            digipeat: DigipeatConfig::default(),
            qsy: QsyConfig::default(),
            object_functions: false,
            voice_alert: VoiceAlertConfig::default(),
            message_group_code: GroupCode::default(),
            bulletin_group_code: GroupCode::default(),
            navitra: NavitraConfig::default(),
            network: AprsNetwork::default(),
            display_area: DisplayArea::EntireDisplay,
            interrupt_time: InterruptTime::Sec10,
            aprs_voice: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Station identity
// ---------------------------------------------------------------------------

/// APRS callsign with optional SSID (up to 9 characters, e.g. "N0CALL-9").
///
/// The SSID suffix (0-15) conventionally indicates the station type:
/// -0 fixed, -1 digi, -7 handheld, -9 mobile, -15 generic.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AprsCallsign(String);

impl AprsCallsign {
    /// Maximum length of an APRS callsign with SSID.
    pub const MAX_LEN: usize = 9;

    /// Creates a new APRS callsign.
    ///
    /// # Errors
    ///
    /// Returns `None` if the value exceeds nine bytes, is non-ASCII, or
    /// contains an ASCII control character. The CAT protocol is CR-delimited,
    /// so rejecting controls also prevents a callsign from injecting a second
    /// command onto the wire.
    #[must_use]
    pub fn new(callsign: &str) -> Option<Self> {
        if callsign.len() <= Self::MAX_LEN
            && callsign.is_ascii()
            && !callsign.bytes().any(|byte| byte.is_ascii_control())
        {
            Some(Self(callsign.to_owned()))
        } else {
            None
        }
    }

    /// Returns the callsign as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Icon / symbol
// ---------------------------------------------------------------------------

/// APRS map icon (symbol table + symbol code).
///
/// APRS uses a two-character encoding: the first character selects the
/// symbol table (`/` for primary, `\` for alternate), and the second
/// character selects the specific icon within that table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AprsIcon {
    /// House (primary table `/`).
    House,
    /// Car / automobile (primary table `/`).
    Car,
    /// Portable / HT (primary table `/`).
    #[default]
    Portable,
    /// Jogger / runner (primary table `/`).
    Jogger,
    /// Bicycle (primary table `/`).
    Bicycle,
    /// Motorcycle (primary table `/`).
    Motorcycle,
    /// Yacht / sailboat (primary table `/`).
    Yacht,
    /// Ambulance (primary table `/`).
    Ambulance,
    /// Fire truck (primary table `/`).
    FireTruck,
    /// Helicopter (primary table `/`).
    Helicopter,
    /// Aircraft / small plane (primary table `/`).
    Aircraft,
    /// Weather station (primary table `/`).
    WeatherStation,
    /// Digipeater (primary table `/`).
    Digipeater,
    /// `IGate` (alternate table `\`).
    IGate,
    /// Truck (primary table `/`).
    Truck,
    /// Custom icon specified by raw table and code characters.
    Custom {
        /// Symbol table identifier (`/` = primary, `\` = alternate,
        /// or overlay character `0`-`9`, `A`-`Z`).
        table: char,
        /// Symbol code character (ASCII 0x21-0x7E).
        code: char,
    },
}

// ---------------------------------------------------------------------------
// Data speed / band / DCD
// ---------------------------------------------------------------------------

/// APRS data transmission speed.
///
/// Most APRS activity on VHF uses 1200 bps (AFSK on 144.390 MHz in North
/// America). 9600 bps is used for high-speed data on UHF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsDataSpeed {
    /// 1200 bps (standard VHF APRS).
    Bps1200,
    /// 9600 bps (UHF high-speed data).
    Bps9600,
}

/// Band used for APRS data transmission and reception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsBand {
    /// Band A only.
    A,
    /// Band B only.
    B,
    /// Both bands A and B.
    Both,
}

/// DCD (Data Carrier Detect) sense mode.
///
/// Controls how the radio detects channel activity before transmitting
/// APRS packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DcdSense {
    /// Sense both voice and data activity on the channel.
    Both,
    /// Sense data activity only (ignore voice signals).
    DataOnly,
}

// ---------------------------------------------------------------------------
// TX delay
// ---------------------------------------------------------------------------

/// APRS TX delay before packet transmission.
///
/// MCP offset `0x120F` stores one of the eight choices exposed by Menu
/// No. 508. It is an enum index, not a duration in 10 ms units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TxDelay {
    /// 100 ms (raw `0`).
    Ms100,
    /// 150 ms (raw `1`).
    Ms150,
    /// 200 ms (raw `2`, firmware V1.03 default).
    #[default]
    Ms200,
    /// 300 ms (raw `3`).
    Ms300,
    /// 400 ms (raw `4`).
    Ms400,
    /// 500 ms (raw `5`).
    Ms500,
    /// 750 ms (raw `6`).
    Ms750,
    /// 1000 ms (raw `7`).
    Ms1000,
}

impl TxDelay {
    /// Creates a TX-delay choice from its displayed duration.
    #[must_use]
    pub const fn new(milliseconds: u16) -> Option<Self> {
        match milliseconds {
            100 => Some(Self::Ms100),
            150 => Some(Self::Ms150),
            200 => Some(Self::Ms200),
            300 => Some(Self::Ms300),
            400 => Some(Self::Ms400),
            500 => Some(Self::Ms500),
            750 => Some(Self::Ms750),
            1000 => Some(Self::Ms1000),
            _ => None,
        }
    }

    /// Returns the displayed delay in milliseconds.
    #[must_use]
    pub const fn as_ms(self) -> u16 {
        match self {
            Self::Ms100 => 100,
            Self::Ms150 => 150,
            Self::Ms200 => 200,
            Self::Ms300 => 300,
            Self::Ms400 => 400,
            Self::Ms500 => 500,
            Self::Ms750 => 750,
            Self::Ms1000 => 1000,
        }
    }

    /// Returns the MCP enum index (`0..=7`).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Ms100 => 0,
            Self::Ms150 => 1,
            Self::Ms200 => 2,
            Self::Ms300 => 3,
            Self::Ms400 => 4,
            Self::Ms500 => 5,
            Self::Ms750 => 6,
            Self::Ms1000 => 7,
        }
    }
}

impl TryFrom<u8> for TxDelay {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Ms100),
            1 => Ok(Self::Ms150),
            2 => Ok(Self::Ms200),
            3 => Ok(Self::Ms300),
            4 => Ok(Self::Ms400),
            5 => Ok(Self::Ms500),
            6 => Ok(Self::Ms750),
            7 => Ok(Self::Ms1000),
            _ => Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS TX delay",
                value,
                detail: "must be raw 0-7 (100, 150, 200, 300, 400, 500, 750, or 1000 ms)",
            }),
        }
    }
}

impl From<TxDelay> for u8 {
    fn from(value: TxDelay) -> Self {
        value.as_raw()
    }
}

// ---------------------------------------------------------------------------
// Beacon control
// ---------------------------------------------------------------------------

/// Beacon transmission control settings.
///
/// Controls how and when APRS position beacons are transmitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BeaconControl {
    /// Beacon transmission method.
    pub method: BeaconMethod,
    /// Initial beacon interval (Menu No. 511).
    pub initial_interval: BeaconInterval,
    /// Enable beacon decay algorithm (doubles interval after each
    /// transmission until reaching 30 minutes).
    pub decay: bool,
    /// Enable proportional pathing (vary digipeater path based on
    /// elapsed time since last beacon).
    pub proportional_pathing: bool,
}

impl Default for BeaconControl {
    fn default() -> Self {
        Self {
            method: BeaconMethod::Auto,
            initial_interval: BeaconInterval::Min1,
            decay: true,
            proportional_pathing: true,
        }
    }
}

/// Initial beacon interval stored at MCP offset `0x136B` (Menu No. 511).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BeaconInterval {
    /// 0.2 minutes / 12 seconds (raw `0`).
    Sec12,
    /// 0.5 minutes / 30 seconds (raw `1`).
    Sec30,
    /// 1 minute (raw `2`, firmware V1.03 default).
    #[default]
    Min1,
    /// 2 minutes (raw `3`).
    Min2,
    /// 3 minutes (raw `4`).
    Min3,
    /// 5 minutes (raw `5`).
    Min5,
    /// 10 minutes (raw `6`).
    Min10,
    /// 20 minutes (raw `7`).
    Min20,
    /// 30 minutes (raw `8`).
    Min30,
    /// 60 minutes (raw `9`).
    Min60,
}

impl BeaconInterval {
    /// Returns the interval in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u16 {
        match self {
            Self::Sec12 => 12,
            Self::Sec30 => 30,
            Self::Min1 => 60,
            Self::Min2 => 120,
            Self::Min3 => 180,
            Self::Min5 => 300,
            Self::Min10 => 600,
            Self::Min20 => 1200,
            Self::Min30 => 1800,
            Self::Min60 => 3600,
        }
    }

    /// Returns the MCP enum index (`0..=9`).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Sec12 => 0,
            Self::Sec30 => 1,
            Self::Min1 => 2,
            Self::Min2 => 3,
            Self::Min3 => 4,
            Self::Min5 => 5,
            Self::Min10 => 6,
            Self::Min20 => 7,
            Self::Min30 => 8,
            Self::Min60 => 9,
        }
    }
}

impl TryFrom<u8> for BeaconInterval {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Sec12),
            1 => Ok(Self::Sec30),
            2 => Ok(Self::Min1),
            3 => Ok(Self::Min2),
            4 => Ok(Self::Min3),
            5 => Ok(Self::Min5),
            6 => Ok(Self::Min10),
            7 => Ok(Self::Min20),
            8 => Ok(Self::Min30),
            9 => Ok(Self::Min60),
            _ => Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS initial beacon interval",
                value,
                detail: "must be raw 0-9 (0.2, 0.5, 1, 2, 3, 5, 10, 20, 30, or 60 minutes)",
            }),
        }
    }
}

impl From<BeaconInterval> for u8 {
    fn from(value: BeaconInterval) -> Self {
        value.as_raw()
    }
}

/// Beacon transmission method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconMethod {
    /// Manual beacon only (press button to transmit).
    Manual,
    /// Beacon on PTT release.
    Ptt,
    /// Automatic periodic beaconing at the configured interval.
    Auto,
    /// `SmartBeaconing` (speed and course-adaptive intervals).
    SmartBeaconing,
}

// ---------------------------------------------------------------------------
// APRS lock
// ---------------------------------------------------------------------------

/// Independent APRS lock controls (Menu No. 509).
///
/// MCP byte `0x120A` stores Frequency in bit `0x01`, PTT in bit `0x02`,
/// and APRS Key in bit `0x04`. Firmware V1.03 presents all three as
/// independent checkboxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AprsLock {
    /// Prevent frequency changes while APRS lock is active.
    pub frequency: bool,
    /// Lock PTT operation.
    pub ptt: bool,
    /// Lock the APRS key.
    pub aprs_key: bool,
}

impl AprsLock {
    /// No APRS controls locked.
    pub const NONE: Self = Self {
        frequency: false,
        ptt: false,
        aprs_key: false,
    };

    /// All three APRS controls locked.
    pub const ALL: Self = Self {
        frequency: true,
        ptt: true,
        aprs_key: true,
    };

    /// Creates an APRS lock value from its three independent controls.
    #[must_use]
    pub const fn new(frequency: bool, ptt: bool, aprs_key: bool) -> Self {
        Self {
            frequency,
            ptt,
            aprs_key,
        }
    }
}

impl TryFrom<u8> for AprsLock {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value & !0x07 == 0 {
            Ok(Self {
                frequency: value & 0x01 != 0,
                ptt: value & 0x02 != 0,
                aprs_key: value & 0x04 != 0,
            })
        } else {
            Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS lock",
                value,
                detail: "must contain only Frequency (0x01), PTT (0x02), and APRS Key (0x04) bits",
            })
        }
    }
}

impl From<AprsLock> for u8 {
    fn from(value: AprsLock) -> Self {
        Self::from(value.frequency)
            | (Self::from(value.ptt) << 1)
            | (Self::from(value.aprs_key) << 2)
    }
}

// ---------------------------------------------------------------------------
// SmartBeaconing
// ---------------------------------------------------------------------------

/// `SmartBeaconing` configuration.
///
/// `SmartBeaconing` adapts the beacon interval based on speed and course
/// changes. At high speed, beacons are sent more frequently; at low speed,
/// less frequently. Course changes trigger immediate beacons.
///
/// Settings correspond to the 7 parameters under the
/// APRS > `SmartBeaconing` menu on the TH-D75.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpSmartBeaconingConfig {
    /// Menu No. 970 unit in which `low_speed`, `high_speed`, and
    /// `turn_slope` are interpreted.
    pub speed_distance_unit: SpeedDistanceUnit,
    /// Low speed threshold in the configured speed unit (raw range 2-30).
    /// Below this speed, beacons are sent at `slow_rate`.
    pub low_speed: McpLowSpeed,
    /// High speed threshold in the configured speed unit (range 2-90). At or
    /// above this speed, beacons are sent at `fast_rate`.
    pub high_speed: McpHighSpeed,
    /// Slow beacon rate as stored by MCP, in whole minutes (range 1-100).
    pub slow_rate: McpSlowRateMinutes,
    /// Fast beacon rate in seconds (range 10-180 seconds).
    pub fast_rate: McpFastRateSeconds,
    /// Minimum course change in degrees to trigger a beacon (range 5-90).
    pub turn_angle: McpTurnAngleDegrees,
    /// Turn slope factor (range 1-255). Higher values require more speed
    /// before a turn triggers a beacon.
    pub turn_slope: McpTurnSlope,
    /// Minimum time between turn-triggered beacons in seconds (range 5-180).
    pub turn_time: McpTurnTimeSeconds,
}

impl Default for McpSmartBeaconingConfig {
    fn default() -> Self {
        Self {
            // TH-D75A V1.03 default. TH-D75E defaults Menu 970 to km/h,
            // which callers must represent explicitly here.
            speed_distance_unit: SpeedDistanceUnit::MilesPerHour,
            low_speed: McpLowSpeed::default(),
            high_speed: McpHighSpeed::default(),
            slow_rate: McpSlowRateMinutes::default(),
            fast_rate: McpFastRateSeconds::default(),
            turn_angle: McpTurnAngleDegrees::default(),
            turn_slope: McpTurnSlope::default(),
            turn_time: McpTurnTimeSeconds::default(),
        }
    }
}

/// MCP low-speed threshold in the Menu No. 970 unit (Menu No. 530, raw
/// `2..=30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpLowSpeed(u8);

impl McpLowSpeed {
    /// Minimum encoded low-speed threshold in the configured unit.
    pub const MIN: u8 = 2;
    /// Maximum encoded low-speed threshold in the configured unit.
    pub const MAX: u8 = 30;
    /// Firmware V1.03 default in the configured unit.
    pub const DEFAULT: u8 = 5;

    /// Creates a low-speed threshold in the radio's accepted range.
    #[must_use]
    pub const fn new(configured_units: u8) -> Option<Self> {
        if configured_units >= Self::MIN && configured_units <= Self::MAX {
            Some(Self(configured_units))
        } else {
            None
        }
    }

    /// Returns the threshold in the Menu No. 970 unit, identical to its MCP
    /// byte value.
    #[must_use]
    pub const fn as_configured_units(self) -> u8 {
        self.0
    }
}

impl Default for McpLowSpeed {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpLowSpeed {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing low speed",
            value,
            detail: "must be 2-30 in the Menu 970 speed unit",
        })
    }
}

impl From<McpLowSpeed> for u8 {
    fn from(value: McpLowSpeed) -> Self {
        value.as_configured_units()
    }
}

/// MCP slow beacon rate in whole minutes (Menu No. 531, raw `1..=100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpSlowRateMinutes(u8);

impl McpSlowRateMinutes {
    /// Minimum encoded slow rate in minutes.
    pub const MIN: u8 = 1;
    /// Maximum encoded slow rate in minutes.
    pub const MAX: u8 = 100;
    /// Firmware V1.03 default in minutes.
    pub const DEFAULT: u8 = 30;

    /// Creates a slow rate in the radio's accepted range.
    #[must_use]
    pub const fn new(minutes: u8) -> Option<Self> {
        if minutes >= Self::MIN && minutes <= Self::MAX {
            Some(Self(minutes))
        } else {
            None
        }
    }

    /// Returns the rate in minutes, identical to its MCP byte value.
    #[must_use]
    pub const fn as_minutes(self) -> u8 {
        self.0
    }
}

impl Default for McpSlowRateMinutes {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpSlowRateMinutes {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing slow rate",
            value,
            detail: "must be 1-100 minutes",
        })
    }
}

impl From<McpSlowRateMinutes> for u8 {
    fn from(value: McpSlowRateMinutes) -> Self {
        value.as_minutes()
    }
}

/// MCP high-speed threshold in the Menu No. 970 unit (Menu No. 530, raw
/// `2..=90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpHighSpeed(u8);

impl McpHighSpeed {
    /// Minimum encoded high-speed threshold in the configured unit.
    pub const MIN: u8 = 2;
    /// Maximum encoded high-speed threshold in the configured unit.
    pub const MAX: u8 = 90;
    /// Firmware V1.03 default in the configured unit.
    pub const DEFAULT: u8 = 70;

    /// Creates a high-speed threshold in the radio's accepted range.
    #[must_use]
    pub const fn new(configured_units: u8) -> Option<Self> {
        if configured_units >= Self::MIN && configured_units <= Self::MAX {
            Some(Self(configured_units))
        } else {
            None
        }
    }

    /// Returns the threshold in the Menu No. 970 unit, identical to its MCP
    /// byte value.
    #[must_use]
    pub const fn as_configured_units(self) -> u8 {
        self.0
    }
}

impl Default for McpHighSpeed {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpHighSpeed {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing high speed",
            value,
            detail: "must be 2-90 in the Menu 970 speed unit",
        })
    }
}

impl From<McpHighSpeed> for u8 {
    fn from(value: McpHighSpeed) -> Self {
        value.as_configured_units()
    }
}

/// MCP fast beacon rate in seconds (Menu No. 532, raw `10..=180`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpFastRateSeconds(u8);

impl McpFastRateSeconds {
    /// Minimum encoded fast rate in seconds.
    pub const MIN: u8 = 10;
    /// Maximum encoded fast rate in seconds.
    pub const MAX: u8 = 180;
    /// Firmware V1.03 default in seconds.
    pub const DEFAULT: u8 = 120;

    /// Creates a fast rate in the radio's accepted range.
    #[must_use]
    pub const fn new(seconds: u8) -> Option<Self> {
        if seconds >= Self::MIN && seconds <= Self::MAX {
            Some(Self(seconds))
        } else {
            None
        }
    }

    /// Returns the rate in seconds, identical to its MCP byte value.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

impl Default for McpFastRateSeconds {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpFastRateSeconds {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing fast rate",
            value,
            detail: "must be 10-180 seconds",
        })
    }
}

impl From<McpFastRateSeconds> for u8 {
    fn from(value: McpFastRateSeconds) -> Self {
        value.as_seconds()
    }
}

/// MCP minimum turn angle in degrees (Menu No. 533, raw `5..=90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpTurnAngleDegrees(u8);

impl McpTurnAngleDegrees {
    /// Minimum encoded turn angle in degrees.
    pub const MIN: u8 = 5;
    /// Maximum encoded turn angle in degrees.
    pub const MAX: u8 = 90;
    /// Firmware V1.03 default in degrees.
    pub const DEFAULT: u8 = 28;

    /// Creates a turn angle in the radio's accepted range.
    #[must_use]
    pub const fn new(degrees: u8) -> Option<Self> {
        if degrees >= Self::MIN && degrees <= Self::MAX {
            Some(Self(degrees))
        } else {
            None
        }
    }

    /// Returns the angle in degrees, identical to its MCP byte value.
    #[must_use]
    pub const fn as_degrees(self) -> u8 {
        self.0
    }
}

impl Default for McpTurnAngleDegrees {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpTurnAngleDegrees {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing turn angle",
            value,
            detail: "must be 5-90 degrees",
        })
    }
}

impl From<McpTurnAngleDegrees> for u8 {
    fn from(value: McpTurnAngleDegrees) -> Self {
        value.as_degrees()
    }
}

/// MCP turn-slope factor (Menu No. 534, raw `1..=255`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpTurnSlope(u8);

impl McpTurnSlope {
    /// Minimum encoded turn-slope factor.
    pub const MIN: u8 = 1;
    /// Maximum encoded turn-slope factor.
    pub const MAX: u8 = u8::MAX;
    /// Firmware V1.03 default turn-slope factor.
    pub const DEFAULT: u8 = 26;

    /// Creates a nonzero turn-slope factor.
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= Self::MIN {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Returns the factor, identical to its MCP byte value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl Default for McpTurnSlope {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpTurnSlope {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing turn slope",
            value,
            detail: "must be 1-255",
        })
    }
}

impl From<McpTurnSlope> for u8 {
    fn from(value: McpTurnSlope) -> Self {
        value.as_raw()
    }
}

/// MCP minimum turn-beacon interval in seconds (Menu No. 535, raw `5..=180`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpTurnTimeSeconds(u8);

impl McpTurnTimeSeconds {
    /// Minimum encoded turn time in seconds.
    pub const MIN: u8 = 5;
    /// Maximum encoded turn time in seconds.
    pub const MAX: u8 = 180;
    /// Firmware V1.03 default in seconds.
    pub const DEFAULT: u8 = 60;

    /// Creates a turn time in the radio's accepted range.
    #[must_use]
    pub const fn new(seconds: u8) -> Option<Self> {
        if seconds >= Self::MIN && seconds <= Self::MAX {
            Some(Self(seconds))
        } else {
            None
        }
    }

    /// Returns the interval in seconds, identical to its MCP byte value.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

impl Default for McpTurnTimeSeconds {
    fn default() -> Self {
        Self(Self::DEFAULT)
    }
}

impl TryFrom<u8> for McpTurnTimeSeconds {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(crate::error::ValidationError::SettingOutOfRange {
            name: "SmartBeaconing turn time",
            value,
            detail: "must be 5-180 seconds",
        })
    }
}

impl From<McpTurnTimeSeconds> for u8 {
    fn from(value: McpTurnTimeSeconds) -> Self {
        value.as_seconds()
    }
}

// ---------------------------------------------------------------------------
// Position ambiguity
// ---------------------------------------------------------------------------

/// Position ambiguity level for APRS position reports.
///
/// Each level removes one digit of precision from the transmitted
/// latitude/longitude, progressively obscuring the station's exact
/// location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAmbiguity {
    /// Full precision (no ambiguity). Approximately 60 feet.
    Full,
    /// 1 digit removed. Approximately 1/10 mile.
    Level1,
    /// 2 digits removed. Approximately 1 mile.
    Level2,
    /// 3 digits removed. Approximately 10 miles.
    Level3,
    /// 4 digits removed. Approximately 60 miles.
    Level4,
}

// ---------------------------------------------------------------------------
// Packet path
// ---------------------------------------------------------------------------

/// Digipeater packet path for APRS transmissions.
///
/// The packet path determines which digipeaters relay the station's
/// packets. Common paths include WIDE1-1,WIDE2-1 for typical VHF
/// APRS operation.
///
/// # New-N Paradigm (per Operating Tips §2.6.1)
///
/// The TH-D75 defaults to the New-N Paradigm with WIDE1-1 On and
/// Total Hops = 2 (i.e. WIDE1-1,WIDE2-1). When the user configures
/// a total hop count greater than 2, the radio displays a warning
/// because excessive hops congest the APRS network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PacketPath {
    /// Off (no digipeater path).
    Off,
    /// WIDE1-1 (one hop via fill-in digipeaters).
    Wide1_1,
    /// WIDE1-1,WIDE2-1 (standard two-hop path).
    #[default]
    Wide1_1Wide2_1,
    /// WIDE1-1,WIDE2-2 (three-hop path).
    Wide1_1Wide2_2,
    /// Path 1 (user-configurable, stored in MCP memory).
    User1,
    /// Path 2 (user-configurable, stored in MCP memory).
    User2,
    /// Path 3 (user-configurable, stored in MCP memory).
    User3,
}

// ---------------------------------------------------------------------------
// Position comment
// ---------------------------------------------------------------------------

/// Predefined APRS position comment phrases.
///
/// The TH-D75 provides 15 selectable position comment phrases that are
/// transmitted as part of the APRS position report. These match the
/// standard APRS position comment codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionComment {
    /// "Off Duty" -- station is not actively monitoring.
    OffDuty,
    /// "En Route" -- station is in transit.
    EnRoute,
    /// "In Service" -- station is actively operating.
    InService,
    /// "Returning" -- station is returning to base.
    Returning,
    /// "Committed" -- station is committed to a task.
    Committed,
    /// "Special" -- special event or activity.
    Special,
    /// "Priority" -- priority traffic.
    Priority,
    /// "Custom 0" -- user-defined comment slot 0.
    Custom0,
    /// "Custom 1" -- user-defined comment slot 1.
    Custom1,
    /// "Custom 2" -- user-defined comment slot 2.
    Custom2,
    /// "Custom 3" -- user-defined comment slot 3.
    Custom3,
    /// "Custom 4" -- user-defined comment slot 4.
    Custom4,
    /// "Custom 5" -- user-defined comment slot 5.
    Custom5,
    /// "Custom 6" -- user-defined comment slot 6.
    Custom6,
    /// "Emergency" -- distress / emergency.
    Emergency,
}

// ---------------------------------------------------------------------------
// Status text
// ---------------------------------------------------------------------------

/// APRS status text message (up to 62 characters).
///
/// The TH-D75 provides 5 status text slots. The active slot is
/// transmitted as part of the APRS status report.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StatusText(String);

impl StatusText {
    /// Maximum length of a status text message.
    pub const MAX_LEN: usize = 62;

    /// Creates a new status text.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 62 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// Returns the status text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Waypoint configuration
// ---------------------------------------------------------------------------

/// Waypoint output configuration.
///
/// Controls how APRS waypoint data is formatted and output to external
/// GPS devices or PC software.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaypointConfig {
    /// Waypoint output format.
    pub format: WaypointFormat,
    /// Number of waypoints to output (range 1-99, or 0 for all).
    pub length: u8,
    /// Enable waypoint output to the serial port.
    pub output: bool,
}

impl Default for WaypointConfig {
    fn default() -> Self {
        Self {
            format: WaypointFormat::Kenwood,
            length: 0,
            output: false,
        }
    }
}

/// Waypoint output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointFormat {
    /// Kenwood proprietary format.
    Kenwood,
    /// Magellan GPS format.
    Magellan,
    /// NMEA `$GPWPL` sentence format.
    Nmea,
}

// ---------------------------------------------------------------------------
// Packet filter
// ---------------------------------------------------------------------------

/// APRS packet filter configuration.
///
/// Controls which received APRS packets are displayed and processed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketFilter {
    /// Position-distance limit (Menu No. 550).
    pub position_limit: PacketFilterPositionLimit,
    /// Independently enabled received-packet categories (Menu No. 551).
    pub filter_types: PacketFilterFlags,
}

impl Default for PacketFilter {
    fn default() -> Self {
        Self {
            position_limit: PacketFilterPositionLimit::Off,
            filter_types: PacketFilterFlags::ALL,
        }
    }
}

/// APRS packet position limit (Menu No. 550, MCP offset `0x1365`).
///
/// Raw zero disables the limit. Raw `1..=250` represents `10..=2500`
/// in the distance unit selected elsewhere by the radio. The unit is not
/// encoded in this byte, so this type deliberately preserves the magnitude
/// as "configured distance units" rather than assuming miles or kilometres.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PacketFilterPositionLimit {
    /// Do not reject packets based on their distance.
    #[default]
    Off,
    /// Maximum accepted distance in the radio's configured distance unit.
    Distance(PacketFilterDistance),
}

impl TryFrom<u8> for PacketFilterPositionLimit {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1..=250 => Ok(Self::Distance(PacketFilterDistance(value))),
            _ => Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS packet-filter position limit",
                value,
                detail: "must be raw 0 (Off) or 1-250 (10-2500 configured distance units)",
            }),
        }
    }
}

impl From<PacketFilterPositionLimit> for u8 {
    fn from(value: PacketFilterPositionLimit) -> Self {
        match value {
            PacketFilterPositionLimit::Off => 0,
            PacketFilterPositionLimit::Distance(distance) => distance.as_raw(),
        }
    }
}

/// A Menu No. 550 distance, from 10 through 2500 in steps of 10.
///
/// The numeric value uses whichever distance unit is configured globally on
/// the radio; that unit is not part of the packet-filter MCP field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketFilterDistance(u8);

impl PacketFilterDistance {
    /// Minimum position-limit distance in configured units.
    pub const MIN: u16 = 10;
    /// Maximum position-limit distance in configured units.
    pub const MAX: u16 = 2500;
    /// Position-limit distance step in configured units.
    pub const STEP: u16 = 10;

    /// Creates a distance if it is in `10..=2500` and divisible by 10.
    #[must_use]
    pub fn new(configured_units: u16) -> Option<Self> {
        if (Self::MIN..=Self::MAX).contains(&configured_units)
            && configured_units.is_multiple_of(Self::STEP)
        {
            u8::try_from(configured_units / Self::STEP).ok().map(Self)
        } else {
            None
        }
    }

    /// Returns the magnitude in the radio's configured distance unit.
    #[must_use]
    pub fn as_configured_units(self) -> u16 {
        u16::from(self.0) * Self::STEP
    }

    /// Returns the MCP byte (`1..=250`).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// Independent received-packet category filters (Menu No. 551).
///
/// MCP byte `0x1366` stores these as seven independent bits. This is not
/// a mutually exclusive packet-type selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the V1.03 UI and MCP byte expose seven independent checkbox bits"
)]
pub struct PacketFilterFlags {
    /// Accept weather packets (`0x01`).
    pub weather: bool,
    /// Accept digipeater packets (`0x02`).
    pub digipeater: bool,
    /// Accept mobile-station packets (`0x04`).
    pub mobile: bool,
    /// Accept object and item packets (`0x08`).
    pub object_item: bool,
    /// Accept NAVITRA packets (`0x10`).
    pub navitra: bool,
    /// Accept one-way packets (`0x20`).
    pub one_way: bool,
    /// Accept all other packet categories (`0x40`).
    pub others: bool,
}

impl PacketFilterFlags {
    /// Reject every packet category represented by Menu No. 551.
    pub const NONE: Self = Self {
        weather: false,
        digipeater: false,
        mobile: false,
        object_item: false,
        navitra: false,
        one_way: false,
        others: false,
    };

    /// Accept every packet category represented by Menu No. 551.
    pub const ALL: Self = Self {
        weather: true,
        digipeater: true,
        mobile: true,
        object_item: true,
        navitra: true,
        one_way: true,
        others: true,
    };
}

impl Default for PacketFilterFlags {
    fn default() -> Self {
        Self::ALL
    }
}

impl TryFrom<u8> for PacketFilterFlags {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value & !0x7F == 0 {
            Ok(Self {
                weather: value & 0x01 != 0,
                digipeater: value & 0x02 != 0,
                mobile: value & 0x04 != 0,
                object_item: value & 0x08 != 0,
                navitra: value & 0x10 != 0,
                one_way: value & 0x20 != 0,
                others: value & 0x40 != 0,
            })
        } else {
            Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS packet filter flags",
                value,
                detail: "must contain only the seven Menu 551 category bits (0x01-0x40)",
            })
        }
    }
}

impl From<PacketFilterFlags> for u8 {
    fn from(value: PacketFilterFlags) -> Self {
        Self::from(value.weather)
            | (Self::from(value.digipeater) << 1)
            | (Self::from(value.mobile) << 2)
            | (Self::from(value.object_item) << 3)
            | (Self::from(value.navitra) << 4)
            | (Self::from(value.one_way) << 5)
            | (Self::from(value.others) << 6)
    }
}

// ---------------------------------------------------------------------------
// User phrases
// ---------------------------------------------------------------------------

/// User-defined APRS message-composition phrase (one 32-byte Menu No. 560
/// clipboard slot).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct UserPhrase(String);

impl UserPhrase {
    /// Maximum encoded length of a user phrase.
    pub const MAX_LEN: usize = 32;

    /// Creates a new message-composition phrase.
    ///
    /// # Errors
    ///
    /// Returns `None` if the UTF-8 representation exceeds 32 bytes.
    #[must_use]
    pub fn new(phrase: &str) -> Option<Self> {
        if phrase.len() <= Self::MAX_LEN {
            Some(Self(phrase.to_owned()))
        } else {
            None
        }
    }

    /// Returns the user phrase as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Auto-reply
// ---------------------------------------------------------------------------

/// APRS auto-reply message configuration.
///
/// When enabled, the radio automatically replies to incoming APRS
/// messages with a configured response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReplyConfig {
    /// Enable auto-reply.
    pub enabled: bool,
    /// Auto-reply type.
    pub reply_type: AutoReplyType,
    /// Reply-to callsign filter (reply only to this callsign, or empty
    /// for any station).
    pub reply_to: AprsCallsign,
    /// Delay time before sending the reply (seconds).
    pub delay_time: AutoReplyDelay,
    /// Reply message text (up to 45 characters).
    pub message: ReplyMessage,
}

impl Default for AutoReplyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reply_type: AutoReplyType::Reply,
            reply_to: AprsCallsign::default(),
            delay_time: AutoReplyDelay::Sec30,
            message: ReplyMessage::default(),
        }
    }
}

/// Auto-reply type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoReplyType {
    /// Reply with the configured message.
    Reply,
    /// Reply with the current position.
    Position,
}

/// Auto-reply delay time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoReplyDelay {
    /// No delay.
    None,
    /// 10 second delay.
    Sec10,
    /// 30 second delay.
    Sec30,
    /// 60 second delay.
    Sec60,
}

/// APRS reply message text (up to 45 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ReplyMessage(String);

impl ReplyMessage {
    /// Maximum length of a reply message.
    pub const MAX_LEN: usize = 45;

    /// Creates a new reply message.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 45 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// Returns the reply message as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Notification
// ---------------------------------------------------------------------------

/// APRS notification sound and display configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NotificationConfig {
    /// Beep on receiving an APRS packet.
    pub rx_beep: bool,
    /// Beep on transmitting an APRS beacon.
    pub tx_beep: bool,
    /// Special beep for directed messages (addressed to this station).
    pub special_call: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            rx_beep: true,
            tx_beep: false,
            special_call: true,
        }
    }
}

/// Display area setting for incoming APRS data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayArea {
    /// Show APRS data on the entire display.
    EntireDisplay,
    /// Show APRS data in the lower portion only.
    LowerOnly,
}

/// Interrupt time for APRS data display (how long the display shows
/// incoming APRS data before returning to normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterruptTime {
    /// 3 second interrupt.
    Sec3,
    /// 5 second interrupt.
    Sec5,
    /// 10 second interrupt.
    Sec10,
    /// 30 second interrupt.
    Sec30,
    /// Continuous (hold until dismissed).
    Continuous,
}

// ---------------------------------------------------------------------------
// Digipeater
// ---------------------------------------------------------------------------

/// APRS digipeater (digital repeater) configuration.
///
/// The TH-D75 can function as a fill-in digipeater, relaying packets
/// from other APRS stations.
///
/// # Menu numbers (per Operating Tips §2.5)
///
/// - Menu No. 580: `UIdigipeat` on/off
/// - Menu No. 581: `UIflood` alias
/// - Menu No. 582: `UIflood` substitution
/// - Menu No. 583: `UItrace` alias
/// - Menu No. 584-587: My Alias 1-4
/// - Menu No. 588: `UIcheck`
///
/// `UIdigipeat` enables relaying of received UI (Unnumbered Information)
/// frames. `UIflood` handles the "flood" style of digipeating where
/// the hop count is decremented but the alias is not changed (unless
/// substitution is on). `UItrace` handles "trace" style digipeating
/// where the digipeater inserts its own callsign into the path.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DigipeatConfig {
    /// Enable `UIdigipeat` (relay UI frames).
    pub ui_digipeat: bool,
    /// Enable `UIcheck` (display frames before relaying).
    pub ui_check: bool,
    /// `UIflood` alias (e.g. "WIDE1") for New-N paradigm digipeating.
    pub ui_flood: FloodAlias,
    /// `UIflood` substitution (replace alias with own callsign).
    pub ui_flood_substitute: bool,
    /// `UItrace` alias (e.g. "WIDE2") for traced digipeating.
    pub ui_trace: TraceAlias,
    /// Digipeater MY alias slots (up to 4 additional aliases).
    pub my_alias: [DigipeatAlias; 4],
}

/// `UIflood` alias (up to 5 characters, e.g. "WIDE1").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FloodAlias(String);

impl FloodAlias {
    /// Maximum length of a flood alias.
    pub const MAX_LEN: usize = 5;

    /// Creates a new flood alias.
    ///
    /// # Errors
    ///
    /// Returns `None` if the alias exceeds 5 characters.
    #[must_use]
    pub fn new(alias: &str) -> Option<Self> {
        if alias.len() <= Self::MAX_LEN {
            Some(Self(alias.to_owned()))
        } else {
            None
        }
    }

    /// Returns the flood alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `UItrace` alias (up to 5 characters, e.g. "WIDE2").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TraceAlias(String);

impl TraceAlias {
    /// Maximum length of a trace alias.
    pub const MAX_LEN: usize = 5;

    /// Creates a new trace alias.
    ///
    /// # Errors
    ///
    /// Returns `None` if the alias exceeds 5 characters.
    #[must_use]
    pub fn new(alias: &str) -> Option<Self> {
        if alias.len() <= Self::MAX_LEN {
            Some(Self(alias.to_owned()))
        } else {
            None
        }
    }

    /// Returns the trace alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Digipeater MY alias (up to 5 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DigipeatAlias(String);

impl DigipeatAlias {
    /// Maximum length of a digipeater alias.
    pub const MAX_LEN: usize = 5;

    /// Creates a new digipeater alias.
    ///
    /// # Errors
    ///
    /// Returns `None` if the alias exceeds 5 characters.
    #[must_use]
    pub fn new(alias: &str) -> Option<Self> {
        if alias.len() <= Self::MAX_LEN {
            Some(Self(alias.to_owned()))
        } else {
            None
        }
    }

    /// Returns the digipeater alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// QSY information
// ---------------------------------------------------------------------------

/// QSY (frequency change) information configuration.
///
/// QSY information allows APRS stations to advertise an alternate
/// voice frequency so other operators can contact them directly.
///
/// Per Operating Tips §2.3.3: the voice frequency from Band A or B is
/// embedded in the APRS beacon. In D-STAR DR mode, the beacon also
/// includes the repeater callsign (§2.3.4); in DV mode, only the
/// frequency is included.
///
/// Menu No. 523 controls the QSY distance restriction (§2.3.5):
/// only QSY beacons from stations within the configured distance
/// are displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct QsyConfig {
    /// Include QSY information in APRS status text.
    pub info_in_status: bool,
    /// Include tone and narrow FM settings in QSY information.
    pub tone_narrow: bool,
    /// Include repeater shift and offset in QSY information.
    pub shift_offset: bool,
    /// Limit distance for QSY display (0 = no limit, 1-2500 km).
    pub limit_distance: u16,
}

// ---------------------------------------------------------------------------
// Voice alert
// ---------------------------------------------------------------------------

/// Voice alert configuration.
///
/// Voice alert transmits a CTCSS tone with APRS packets. Stations
/// monitoring the APRS frequency with matching tone squelch will hear
/// the alert, enabling a quick voice QSO.
///
/// Per Operating Tips §5.3: `VoiceAlert` is CTCSS-based. The radio
/// transmits a CTCSS tone on the APRS frequency; stations with
/// matching tone squelch hear an audible alert. Menu No. 910
/// controls the volume balance between `VoiceAlert` audio and normal
/// receive audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceAlertConfig {
    /// Enable voice alert.
    pub enabled: bool,
    /// Voice alert CTCSS tone code (index into the CTCSS frequency table).
    /// Default is tone code 12 (100.0 Hz).
    pub tone_code: ToneCode,
}

impl Default for VoiceAlertConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tone_code: ToneCode::TONE_100HZ,
        }
    }
}

// ---------------------------------------------------------------------------
// Group codes
// ---------------------------------------------------------------------------

/// Message or bulletin group code (up to 9 characters).
///
/// Group codes filter incoming APRS messages and bulletins so only
/// messages addressed to matching group identifiers are displayed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct GroupCode(String);

impl GroupCode {
    /// Maximum length of a group code.
    pub const MAX_LEN: usize = 9;

    /// Creates a new group code.
    ///
    /// # Errors
    ///
    /// Returns `None` if the code exceeds 9 characters.
    #[must_use]
    pub fn new(code: &str) -> Option<Self> {
        if code.len() <= Self::MAX_LEN {
            Some(Self(code.to_owned()))
        } else {
            None
        }
    }

    /// Returns the group code as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// NAVITRA
// ---------------------------------------------------------------------------

/// NAVITRA (navigation/tracking) configuration.
///
/// NAVITRA is a Japanese APRS-like system for position tracking.
/// The TH-D75 supports NAVITRA alongside standard APRS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavitraConfig {
    /// NAVITRA group mode.
    pub group_mode: NavitraGroupMode,
    /// NAVITRA group code (up to 9 characters).
    pub group_code: GroupCode,
    /// NAVITRA message text (up to 20 characters).
    pub message: NavitraMessage,
}

impl Default for NavitraConfig {
    fn default() -> Self {
        Self {
            group_mode: NavitraGroupMode::Off,
            group_code: GroupCode::default(),
            message: NavitraMessage::default(),
        }
    }
}

/// NAVITRA group filtering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NavitraGroupMode {
    /// NAVITRA group filtering disabled.
    Off,
    /// Show only stations in the matching group.
    GroupOnly,
}

/// NAVITRA message text (up to 20 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct NavitraMessage(String);

impl NavitraMessage {
    /// Maximum length of a NAVITRA message.
    pub const MAX_LEN: usize = 20;

    /// Creates a new NAVITRA message.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 20 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// Returns the NAVITRA message as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

/// Menu No. 591 network choice and its Altnet address.
///
/// The normal setting transmits the fixed `APK005` destination. Altnet
/// uses the separately stored six-byte address to restrict received
/// packets for special applications. This setting is unrelated to the
/// radio's separate NAVITRA configuration.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AprsNetwork {
    /// Active network type.
    pub network_type: AprsNetworkType,
    /// Address used when `network_type` is [`AprsNetworkType::Altnet`].
    pub altnet_address: AltnetAddress,
}

/// APRS network type stored at MCP offset `0x1460`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AprsNetworkType {
    /// Normal APRS operation using the fixed `APK005` destination.
    #[default]
    AprsApk005,
    /// Alternate-network operation using a user-supplied address.
    Altnet,
}

impl TryFrom<u8> for AprsNetworkType {
    type Error = crate::error::ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AprsApk005),
            1 => Ok(Self::Altnet),
            _ => Err(crate::error::ValidationError::SettingOutOfRange {
                name: "APRS network type",
                value,
                detail: "must be 0 (APRS/APK005) or 1 (Altnet)",
            }),
        }
    }
}

impl From<AprsNetworkType> for u8 {
    fn from(value: AprsNetworkType) -> Self {
        match value {
            AprsNetworkType::AprsApk005 => 0,
            AprsNetworkType::Altnet => 1,
        }
    }
}

/// Altnet address stored in the six-byte MCP field at `0x1461`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AltnetAddress(String);

impl AltnetAddress {
    /// Maximum encoded address length in bytes.
    pub const MAX_LEN: usize = 6;

    /// Creates an Altnet address that fits the radio's six-byte field.
    #[must_use]
    pub fn new(address: &str) -> Option<Self> {
        if address.len() <= Self::MAX_LEN {
            Some(Self(address.to_owned()))
        } else {
            None
        }
    }

    /// Returns the address as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// APRS message (received/transmitted)
// ---------------------------------------------------------------------------

/// An APRS message (for RX history or TX queue).
///
/// APRS messaging supports point-to-point text messages between stations,
/// with acknowledgment. The TH-D75 stores a history of received and
/// transmitted messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsMessage {
    /// Source callsign (who sent the message).
    pub from: AprsCallsign,
    /// Destination callsign (who the message is addressed to).
    pub to: AprsCallsign,
    /// Message text (up to 67 characters per the APRS spec).
    pub text: String,
    /// Message number for acknowledgment (1-99999, or 0 if no ack).
    pub message_number: u32,
    /// Whether this message has been acknowledged.
    pub acknowledged: bool,
}

// ---------------------------------------------------------------------------
// APRS station (received position report)
// ---------------------------------------------------------------------------

/// A received APRS station report from the station list.
///
/// The TH-D75 maintains a list of recently heard APRS stations with
/// their position, status, and other information.
#[derive(Debug, Clone, PartialEq)]
pub struct AprsStation {
    /// Station callsign with SSID.
    pub callsign: AprsCallsign,
    /// Station latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Station longitude in decimal degrees (positive = East).
    pub longitude: f64,
    /// Station altitude in meters (if available).
    pub altitude: Option<f64>,
    /// Station course in degrees (0-360, if moving).
    pub course: Option<f64>,
    /// Station speed in km/h (if moving).
    pub speed: Option<f64>,
    /// Station comment text.
    pub comment: String,
    /// Station APRS icon.
    pub icon: AprsIcon,
    /// Distance from own position in km (calculated by radio).
    pub distance: Option<f64>,
    /// Bearing from own position in degrees (calculated by radio).
    pub bearing: Option<f64>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn aprs_callsign_valid() -> TestResult {
        let cs = AprsCallsign::new("N0CALL-9").ok_or("valid callsign rejected")?;
        assert_eq!(cs.as_str(), "N0CALL-9");
        Ok(())
    }

    #[test]
    fn aprs_callsign_max_length() -> TestResult {
        let cs = AprsCallsign::new("N0CALL-15").ok_or("valid 9-char callsign rejected")?;
        assert_eq!(cs.as_str(), "N0CALL-15");
        Ok(())
    }

    #[test]
    fn aprs_callsign_too_long() {
        assert!(AprsCallsign::new("N0CALL-150").is_none());
    }

    #[test]
    fn aprs_callsign_rejects_non_ascii_and_wire_controls() {
        assert!(AprsCallsign::new("NØCALL").is_none());
        assert!(AprsCallsign::new("N0CALL\rID").is_none());
        assert!(AprsCallsign::new("N0CALL\n").is_none());
    }

    #[test]
    fn status_text_valid() -> TestResult {
        let st = StatusText::new("Testing 1 2 3").ok_or("valid status text rejected")?;
        assert_eq!(st.as_str(), "Testing 1 2 3");
        Ok(())
    }

    #[test]
    fn status_text_max_length() {
        let text = "a".repeat(62);
        assert!(StatusText::new(&text).is_some());
    }

    #[test]
    fn status_text_too_long() {
        let text = "a".repeat(63);
        assert!(StatusText::new(&text).is_none());
    }

    #[test]
    fn tx_delay_accepts_exact_menu_choices() -> TestResult {
        for (raw, milliseconds) in [100, 150, 200, 300, 400, 500, 750, 1000]
            .into_iter()
            .enumerate()
        {
            let delay = TxDelay::new(milliseconds).ok_or("documented TX delay rejected")?;
            assert_eq!(delay.as_ms(), milliseconds);
            assert_eq!(usize::from(u8::from(delay)), raw);
            assert_eq!(TxDelay::try_from(u8::try_from(raw)?)?, delay);
        }
        Ok(())
    }

    #[test]
    fn tx_delay_rejects_non_menu_values() {
        assert!(TxDelay::new(0).is_none());
        assert!(TxDelay::new(250).is_none());
        assert!(TxDelay::new(1001).is_none());
        assert!(TxDelay::try_from(8).is_err());
    }

    #[test]
    fn tx_delay_default_is_200ms() {
        let d = TxDelay::default();
        assert_eq!(d, TxDelay::Ms200);
        assert_eq!(d.as_ms(), 200);
        assert_eq!(d.as_raw(), 2);
    }

    #[test]
    fn beacon_control_uses_v103_defaults_and_discrete_intervals() -> TestResult {
        let control = BeaconControl::default();
        assert_eq!(control.method, BeaconMethod::Auto);
        assert_eq!(control.initial_interval, BeaconInterval::Min1);
        assert!(control.decay);
        assert!(control.proportional_pathing);

        let expected_seconds = [12, 30, 60, 120, 180, 300, 600, 1200, 1800, 3600];
        for (raw, seconds) in expected_seconds.into_iter().enumerate() {
            let interval = BeaconInterval::try_from(u8::try_from(raw)?)?;
            assert_eq!(interval.as_seconds(), seconds);
            assert_eq!(usize::from(u8::from(interval)), raw);
        }
        assert!(BeaconInterval::try_from(10).is_err());
        Ok(())
    }

    #[test]
    fn smart_beaconing_defaults() {
        let sb = McpSmartBeaconingConfig::default();
        assert_eq!(sb.speed_distance_unit, SpeedDistanceUnit::MilesPerHour);
        assert_eq!(sb.low_speed.as_configured_units(), 5);
        assert_eq!(sb.high_speed.as_configured_units(), 70);
        assert_eq!(sb.fast_rate.as_seconds(), 120);
        assert_eq!(sb.slow_rate.as_minutes(), 30);
        assert_eq!(sb.turn_angle.as_degrees(), 28);
        assert_eq!(sb.turn_slope.as_raw(), 26);
        assert_eq!(sb.turn_time.as_seconds(), 60);
    }

    #[test]
    fn smart_beaconing_low_speed_enforces_mcp_domain() -> TestResult {
        for raw in McpLowSpeed::MIN..=McpLowSpeed::MAX {
            let speed = McpLowSpeed::try_from(raw)?;
            assert_eq!(speed.as_configured_units(), raw);
            assert_eq!(u8::from(speed), raw);
        }
        assert!(McpLowSpeed::try_from(1).is_err());
        assert!(McpLowSpeed::try_from(31).is_err());
        Ok(())
    }

    #[test]
    fn smart_beaconing_slow_rate_enforces_mcp_minutes() -> TestResult {
        for raw in McpSlowRateMinutes::MIN..=McpSlowRateMinutes::MAX {
            let rate = McpSlowRateMinutes::try_from(raw)?;
            assert_eq!(rate.as_minutes(), raw);
            assert_eq!(u8::from(rate), raw);
        }
        assert!(McpSlowRateMinutes::try_from(0).is_err());
        assert!(McpSlowRateMinutes::try_from(101).is_err());
        Ok(())
    }

    #[test]
    fn remaining_smart_beaconing_fields_enforce_mcp_domains() -> TestResult {
        for raw in McpHighSpeed::MIN..=McpHighSpeed::MAX {
            let value = McpHighSpeed::try_from(raw)?;
            assert_eq!(value.as_configured_units(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(McpHighSpeed::try_from(1).is_err());
        assert!(McpHighSpeed::try_from(91).is_err());

        for raw in McpFastRateSeconds::MIN..=McpFastRateSeconds::MAX {
            let value = McpFastRateSeconds::try_from(raw)?;
            assert_eq!(value.as_seconds(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(McpFastRateSeconds::try_from(9).is_err());
        assert!(McpFastRateSeconds::try_from(181).is_err());

        for raw in McpTurnAngleDegrees::MIN..=McpTurnAngleDegrees::MAX {
            let value = McpTurnAngleDegrees::try_from(raw)?;
            assert_eq!(value.as_degrees(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(McpTurnAngleDegrees::try_from(4).is_err());
        assert!(McpTurnAngleDegrees::try_from(91).is_err());

        for raw in McpTurnSlope::MIN..=McpTurnSlope::MAX {
            let value = McpTurnSlope::try_from(raw)?;
            assert_eq!(value.as_raw(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(McpTurnSlope::try_from(0).is_err());

        for raw in McpTurnTimeSeconds::MIN..=McpTurnTimeSeconds::MAX {
            let value = McpTurnTimeSeconds::try_from(raw)?;
            assert_eq!(value.as_seconds(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(McpTurnTimeSeconds::try_from(4).is_err());
        assert!(McpTurnTimeSeconds::try_from(181).is_err());
        Ok(())
    }

    #[test]
    fn user_phrase_valid() -> TestResult {
        let phrase = UserPhrase::new("On my way").ok_or("valid user phrase rejected")?;
        assert_eq!(phrase.as_str(), "On my way");
        Ok(())
    }

    #[test]
    fn user_phrase_too_long() {
        assert!(UserPhrase::new(&"x".repeat(33)).is_none());
    }

    #[test]
    fn user_phrase_accepts_exact_32_byte_slot() -> TestResult {
        let phrase = UserPhrase::new(&"x".repeat(32)).ok_or("32-byte phrase rejected")?;
        assert_eq!(phrase.as_str().len(), 32);
        Ok(())
    }

    #[test]
    fn packet_filter_position_limit_round_trips_mcp_domain() -> TestResult {
        for raw in 0..=250 {
            let limit = PacketFilterPositionLimit::try_from(raw)?;
            assert_eq!(u8::from(limit), raw);
            match (raw, limit) {
                (0, PacketFilterPositionLimit::Off) => {}
                (1..=250, PacketFilterPositionLimit::Distance(distance)) => {
                    assert_eq!(distance.as_configured_units(), u16::from(raw) * 10);
                }
                _ => return Err("position-limit variant did not match raw value".into()),
            }
        }
        assert!(PacketFilterPositionLimit::try_from(251).is_err());
        assert!(PacketFilterPositionLimit::try_from(u8::MAX).is_err());
        Ok(())
    }

    #[test]
    fn packet_filter_distance_validates_step_and_range() -> TestResult {
        let minimum = PacketFilterDistance::new(10).ok_or("minimum distance rejected")?;
        let maximum = PacketFilterDistance::new(2500).ok_or("maximum distance rejected")?;
        assert_eq!(minimum.as_raw(), 1);
        assert_eq!(maximum.as_raw(), 250);
        assert!(PacketFilterDistance::new(0).is_none());
        assert!(PacketFilterDistance::new(15).is_none());
        assert!(PacketFilterDistance::new(2510).is_none());
        Ok(())
    }

    #[test]
    fn reply_message_valid() -> TestResult {
        let rm = ReplyMessage::new("I am away").ok_or("valid reply message rejected")?;
        assert_eq!(rm.as_str(), "I am away");
        Ok(())
    }

    #[test]
    fn reply_message_too_long() {
        let text = "a".repeat(46);
        assert!(ReplyMessage::new(&text).is_none());
    }

    #[test]
    fn aprs_config_default_compiles() {
        let cfg = AprsConfig::default();
        assert_eq!(cfg.data_speed, AprsDataSpeed::Bps1200);
        assert_eq!(cfg.aprs_lock, AprsLock::NONE);
        assert_eq!(
            cfg.packet_filter.position_limit,
            PacketFilterPositionLimit::Off,
        );
        assert_eq!(cfg.packet_filter.filter_types, PacketFilterFlags::ALL);
        assert_eq!(cfg.user_phrases.len(), 20);
        assert_eq!(cfg.network.network_type, AprsNetworkType::AprsApk005);
    }

    #[test]
    fn aprs_lock_preserves_independent_bits() -> TestResult {
        for raw in 0..=7 {
            let lock = AprsLock::try_from(raw)?;
            assert_eq!(u8::from(lock), raw);
        }
        assert_eq!(AprsLock::try_from(5)?, AprsLock::new(true, false, true));
        assert!(AprsLock::try_from(8).is_err());
        Ok(())
    }

    #[test]
    fn packet_filter_flags_round_trip_every_valid_byte() -> TestResult {
        for raw in 0..=0x7F {
            let flags = PacketFilterFlags::try_from(raw)?;
            assert_eq!(u8::from(flags), raw);
        }
        assert!(PacketFilterFlags::try_from(0x80).is_err());
        Ok(())
    }

    #[test]
    fn aprs_network_is_apk005_or_altnet() -> TestResult {
        assert_eq!(AprsNetworkType::try_from(0)?, AprsNetworkType::AprsApk005);
        assert_eq!(AprsNetworkType::try_from(1)?, AprsNetworkType::Altnet);
        assert_eq!(u8::from(AprsNetworkType::AprsApk005), 0);
        assert_eq!(u8::from(AprsNetworkType::Altnet), 1);
        assert!(AprsNetworkType::try_from(2).is_err());

        let address = AltnetAddress::new("ALTNET").ok_or("six-byte address rejected")?;
        assert_eq!(address.as_str(), "ALTNET");
        assert!(AltnetAddress::new("TOO-LONG").is_none());
        Ok(())
    }

    #[test]
    fn group_code_valid() -> TestResult {
        let gc = GroupCode::new("ARES").ok_or("valid group code rejected")?;
        assert_eq!(gc.as_str(), "ARES");
        Ok(())
    }

    #[test]
    fn group_code_too_long() {
        assert!(GroupCode::new("0123456789").is_none());
    }

    #[test]
    fn qsy_config_defaults() {
        let qsy = QsyConfig::default();
        assert!(!qsy.info_in_status);
        assert_eq!(qsy.limit_distance, 0);
    }
}
