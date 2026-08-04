//! APRS (Automatic Packet Reporting System) settings types.
//!
//! APRS is a tactical real-time digital communications protocol used by ham
//! radio operators for position reporting, messaging, and telemetry. The
//! TH-D75 supports APRS on VHF with features including position beaconing,
//! two-way messaging, `SmartBeaconing`, digipeater path settings,
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
//! These types describe the documented menu domains and the corresponding
//! fields identified in the generated MCP schema. They do not imply that a
//! live MCP writer exists for every field or that every write has been
//! qualified on hardware.

use std::{collections::HashSet, fmt};

use ax25_codec::{Ax25Address, Ax25Error, Callsign, Ssid};

use crate::error::ValidationError;
use crate::types::{
    radio_params::{BeaconMode, PacketDataRate},
    settings::SpeedDistanceUnit,
    tone::ToneCode,
};

// ---------------------------------------------------------------------------
// Top-level APRS settings
// ---------------------------------------------------------------------------

/// APRS settings identified for the TH-D75.
///
/// Covers all settings from the radio's APRS menu tree, including station
/// identity, beaconing, messaging, filtering, digipeating, and notification
/// options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsSettings {
    /// APRS station callsign with optional SSID, or `None` when the station
    /// identity has not been configured.
    pub my_callsign: Option<AprsCallsign>,
    /// APRS map icon (symbol table + symbol code pair).
    pub icon: AprsIcon,
    /// Position comment (selected from 15 predefined phrases).
    pub position_comment: PositionComment,
    /// Status text slots (5 configurable messages, up to 42 bytes each).
    pub status_texts: [StoredStatusText; 5],
    /// Active status text slot.
    pub active_status_text: StatusTextSlot,
    /// Digipeater packet path settings.
    pub packet_path: PacketPath,
    /// APRS packet-data rate (1200 or 9600 bps).
    pub data_rate: PacketDataRate,
    /// Band used for APRS data transmission.
    pub data_band: AprsBand,
    /// DCD (Data Carrier Detect) sense mode.
    pub dcd_sense: DcdSense,
    /// TX delay before packet transmission (Menu No. 508).
    pub tx_delay: TxDelay,
    /// Beacon transmission control settings.
    pub beacon_control: BeaconControl,
    /// Stored `SmartBeaconing` settings (speed-adaptive beaconing).
    pub smart_beaconing: StoredSmartBeaconingSettings,
    /// Independent APRS frequency, PTT, and APRS-key locks.
    pub aprs_lock: AprsLock,
    /// Position ambiguity level (0 = full precision, 1-4 = progressively
    /// less precise, each level removes one decimal digit).
    pub position_ambiguity: PositionAmbiguity,
    /// Waypoint output settings.
    pub waypoint: WaypointSettings,
    /// Packet filter settings.
    pub packet_filter: PacketFilter,
    /// Message-composition clipboard phrases (Menu No. 560).
    pub user_phrases: [UserPhrase; 20],
    /// Auto-reply message settings.
    pub auto_reply: AutoReplySettings,
    /// Notification sound settings.
    pub notification: NotificationSettings,
    /// Digipeater settings.
    pub digipeater: StoredDigipeaterSettings,
    /// QSY (frequency change) information settings.
    pub qsy: QsySettings,
    /// Enable APRS object functions (transmit/edit objects).
    pub object_functions: bool,
    /// Voice alert (transmit CTCSS tone with APRS packets to alert
    /// nearby stations monitoring with tone squelch).
    pub voice_alert: VoiceAlertSettings,
    /// Message group code filters (up to six 9-byte codes).
    pub message_group_codes: MessageGroupCodes,
    /// Bulletin group code filters (up to six 5-byte codes).
    pub bulletin_group_codes: BulletinGroupCodes,
    /// NAVITRA (navigation/tracking) settings.
    pub navitra: NavitraSettings,
    /// APRS network identifier.
    pub network: AprsNetwork,
    /// Display area setting for incoming APRS packets.
    pub display_area: DisplayArea,
    /// Interrupt time for incoming APRS data display (seconds).
    pub interrupt_time: InterruptTime,
    /// APRS voice announcement on receive.
    pub aprs_voice: bool,
}

// ---------------------------------------------------------------------------
// Station identity
// ---------------------------------------------------------------------------

/// Canonical APRS station identity used by the TH-D75 `CS` command.
///
/// The base callsign contains one to six uppercase ASCII letters or digits.
/// A nonzero SSID is written as a decimal `-1` through `-15` suffix. SSID
/// zero has no suffix, so inputs such as `N0CALL-0` and `N0CALL-07` are
/// rejected rather than silently normalized.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsCallsign(Ax25Address);

impl AprsCallsign {
    /// Maximum encoded length of a canonical APRS callsign with SSID.
    pub const MAX_LEN: usize = 9;

    /// Parse a canonical APRS station identity.
    ///
    /// # Errors
    ///
    /// Returns an [`Ax25Error`] if `value` is not canonical AX.25 address
    /// text. An empty value is not a callsign; CAT readers represent an empty
    /// radio slot as `None`.
    pub fn new(value: &str) -> Result<Self, Ax25Error> {
        Ax25Address::from_canonical_str(value).map(Self)
    }

    /// Create a radio callsign from an already validated AX.25 address.
    #[must_use]
    pub const fn from_address(address: Ax25Address) -> Self {
        Self(address)
    }

    /// Return the validated AX.25 address.
    #[must_use]
    pub const fn address(&self) -> &Ax25Address {
        &self.0
    }

    /// Consume this value and return the validated AX.25 address.
    #[must_use]
    pub fn into_address(self) -> Ax25Address {
        self.0
    }

    /// Return the base callsign without an SSID suffix.
    #[must_use]
    pub const fn base_callsign(&self) -> &Callsign {
        &self.0.callsign
    }

    /// Return the station's validated SSID.
    #[must_use]
    pub const fn ssid(&self) -> Ssid {
        self.0.ssid
    }
}

impl fmt::Display for AprsCallsign {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for AprsCallsign {
    type Err = Ax25Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl From<Ax25Address> for AprsCallsign {
    fn from(address: Ax25Address) -> Self {
        Self::from_address(address)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsIcon {
    /// House (primary table `/`).
    House,
    /// Car / automobile (primary table `/`).
    Car,
    /// Portable / HT (primary table `/`).
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
    /// Custom icon specified by validated table and symbol values.
    Custom {
        /// Symbol table identifier accepted by the TH-D75 MCP field.
        table: AprsSymbolTable,
        /// Printable ASCII symbol code.
        code: AprsSymbolCode,
    },
}

impl AprsIcon {
    /// Creates a custom APRS icon from the radio's two encoded characters.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCharacter`] when the table is not
    /// `/`, `0` through `9`, or `A` through `Z`, or when the symbol code is
    /// not printable ASCII.
    pub fn custom(table: char, code: char) -> Result<Self, ValidationError> {
        Ok(Self::Custom {
            table: AprsSymbolTable::new(table)?,
            code: AprsSymbolCode::new(code)?,
        })
    }
}

/// APRS symbol-table byte accepted by the TH-D75 MCP field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsSymbolTable(char);

impl AprsSymbolTable {
    /// Validates a symbol-table character.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCharacter`] unless `value` is `/`,
    /// `0` through `9`, or `A` through `Z`.
    pub const fn new(value: char) -> Result<Self, ValidationError> {
        if matches!(value, '/' | '0'..='9' | 'A'..='Z') {
            Ok(Self(value))
        } else {
            Err(ValidationError::InvalidCharacter {
                name: "APRS symbol table",
                value,
                detail: "must be '/', 0-9, or A-Z",
            })
        }
    }

    /// Returns the encoded table character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0
    }
}

/// Printable ASCII APRS symbol code accepted by the TH-D75 MCP field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AprsSymbolCode(char);

impl AprsSymbolCode {
    /// Validates an APRS symbol code (`!` through `~`).
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidCharacter`] unless `value` is
    /// printable ASCII from `!` through `~`.
    pub const fn new(value: char) -> Result<Self, ValidationError> {
        if value.is_ascii_graphic() {
            Ok(Self(value))
        } else {
            Err(ValidationError::InvalidCharacter {
                name: "APRS symbol code",
                value,
                detail: "must be printable ASCII from '!' through '~'",
            })
        }
    }

    /// Returns the encoded symbol character.
    #[must_use]
    pub const fn as_char(self) -> char {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Data band / DCD
// ---------------------------------------------------------------------------

/// Band used for APRS data transmission and reception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsBand {
    /// Band A only.
    A,
    /// Band B only.
    B,
}

impl AprsBand {
    /// Returns the documented Menu No. 506 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::A
    }
}

impl TryFrom<u8> for AprsBand {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS data band",
                value,
                detail: "must be 0 (Band A) or 1 (Band B)",
            }),
        }
    }
}

impl From<AprsBand> for u8 {
    fn from(value: AprsBand) -> Self {
        match value {
            AprsBand::A => 0,
            AprsBand::B => 1,
        }
    }
}

/// DCD (Data Carrier Detect) sense mode.
///
/// Controls how the radio detects channel activity before transmitting
/// APRS packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DcdSense {
    /// Inhibit transmission while the data band is busy.
    Busy,
    /// Inhibit transmission only when packet data is detected.
    DetectData,
    /// Ignore data-carrier detection when deciding whether to transmit.
    Off,
}

impl DcdSense {
    /// Returns the documented Menu No. 507 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::Busy
    }
}

impl TryFrom<u8> for DcdSense {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Busy),
            1 => Ok(Self::DetectData),
            2 => Ok(Self::Off),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS DCD sense",
                value,
                detail: "must be 0 (Busy), 1 (Detect Data), or 2 (Off)",
            }),
        }
    }
}

impl From<DcdSense> for u8 {
    fn from(value: DcdSense) -> Self {
        match value {
            DcdSense::Busy => 0,
            DcdSense::DetectData => 1,
            DcdSense::Off => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// TX delay
// ---------------------------------------------------------------------------

/// APRS TX delay before packet transmission.
///
/// MCP offset `0x120F` stores one of the eight choices exposed by Menu
/// No. 508. It is an enum index, not a duration in 10 ms units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxDelay {
    /// 100 ms (raw `0`).
    Ms100,
    /// 150 ms (raw `1`).
    Ms150,
    /// 200 ms (raw `2`, firmware V1.03 default).
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
    /// Returns the documented firmware V1.03 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::Ms200
    }

    /// Creates a TX-delay choice from its displayed duration.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `milliseconds`
    /// is one of 100, 150, 200, 300, 400, 500, 750, or 1000.
    pub const fn new(milliseconds: u16) -> Result<Self, ValidationError> {
        match milliseconds {
            100 => Ok(Self::Ms100),
            150 => Ok(Self::Ms150),
            200 => Ok(Self::Ms200),
            300 => Ok(Self::Ms300),
            400 => Ok(Self::Ms400),
            500 => Ok(Self::Ms500),
            750 => Ok(Self::Ms750),
            1000 => Ok(Self::Ms1000),
            _ => Err(ValidationError::IntegerOutOfRange {
                name: "APRS TX delay",
                value: milliseconds as i64,
                detail: "must be 100, 150, 200, 300, 400, 500, 750, or 1000 ms",
            }),
        }
    }

    /// Returns the displayed delay in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
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
    type Error = ValidationError;

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
            _ => Err(ValidationError::SettingOutOfRange {
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
    pub method: BeaconMode,
    /// Initial beacon interval (Menu No. 511).
    pub initial_interval: BeaconInterval,
    /// Enable beacon decay algorithm (doubles interval after each
    /// transmission until reaching 30 minutes).
    pub decay: bool,
    /// Enable proportional pathing (vary digipeater path based on
    /// elapsed time since last beacon).
    pub proportional_pathing: bool,
}

impl BeaconControl {
    /// Returns the documented firmware V1.03 factory settings.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self {
            method: BeaconMode::Auto,
            initial_interval: BeaconInterval::factory_default(),
            decay: true,
            proportional_pathing: true,
        }
    }
}

/// Initial beacon interval stored at MCP offset `0x136B` (Menu No. 511).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeaconInterval {
    /// 0.2 minutes / 12 seconds (raw `0`).
    Sec12,
    /// 0.5 minutes / 30 seconds (raw `1`).
    Sec30,
    /// 1 minute (raw `2`, firmware V1.03 default).
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
    /// Returns the documented firmware V1.03 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::Min1
    }

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
    type Error = ValidationError;

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
            _ => Err(ValidationError::SettingOutOfRange {
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
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value & !0x07 == 0 {
            Ok(Self {
                frequency: value & 0x01 != 0,
                ptt: value & 0x02 != 0,
                aprs_key: value & 0x04 != 0,
            })
        } else {
            Err(ValidationError::SettingOutOfRange {
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

/// Stored `SmartBeaconing` settings.
///
/// `SmartBeaconing` adapts the beacon interval based on speed and course
/// changes. At high speed, beacons are sent more frequently; at low speed,
/// less frequently. Course changes trigger immediate beacons.
///
/// Settings correspond to the 7 parameters under the
/// APRS > `SmartBeaconing` menu on the TH-D75.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredSmartBeaconingSettings {
    /// Menu No. 970 unit in which `low_speed`, `high_speed`, and
    /// `turn_slope` are interpreted.
    pub speed_distance_unit: SpeedDistanceUnit,
    /// Low speed threshold in the configured speed unit (raw range 2-30).
    /// Below this speed, beacons are sent at `slow_rate`.
    pub low_speed: StoredLowSpeed,
    /// High speed threshold in the configured speed unit (range 2-90). At or
    /// above this speed, beacons are sent at `fast_rate`.
    pub high_speed: StoredHighSpeed,
    /// Stored slow beacon rate in whole minutes (range 1-100).
    pub slow_rate: StoredSlowRateMinutes,
    /// Fast beacon rate in seconds (range 10-180 seconds).
    pub fast_rate: StoredFastRateSeconds,
    /// Minimum course change in degrees to trigger a beacon (range 5-90).
    pub turn_angle: StoredTurnAngleDegrees,
    /// Turn slope factor (range 1-255). Higher values require more speed
    /// before a turn triggers a beacon.
    pub turn_slope: StoredTurnSlope,
    /// Minimum time between turn-triggered beacons in seconds (range 5-180).
    pub turn_time: StoredTurnTimeSeconds,
}

impl StoredSmartBeaconingSettings {
    /// Return the radio's factory `SmartBeaconing` settings for a speed unit.
    ///
    /// Requiring the Menu 970 unit keeps the interpretation of each stored
    /// threshold explicit.
    #[must_use]
    pub const fn factory_default(speed_distance_unit: SpeedDistanceUnit) -> Self {
        Self {
            speed_distance_unit,
            low_speed: StoredLowSpeed::factory_default(),
            high_speed: StoredHighSpeed::factory_default(),
            slow_rate: StoredSlowRateMinutes::factory_default(),
            fast_rate: StoredFastRateSeconds::factory_default(),
            turn_angle: StoredTurnAngleDegrees::factory_default(),
            turn_slope: StoredTurnSlope::factory_default(),
            turn_time: StoredTurnTimeSeconds::factory_default(),
        }
    }
}

/// Stored low-speed threshold in the Menu No. 970 unit (Menu No. 530, raw
/// `2..=30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredLowSpeed(u8);

impl StoredLowSpeed {
    /// Minimum encoded low-speed threshold in the configured unit.
    pub const MIN: u8 = 2;
    /// Maximum encoded low-speed threshold in the configured unit.
    pub const MAX: u8 = 30;
    /// Firmware V1.03 factory value in the configured unit.
    pub const FACTORY_DEFAULT_CONFIGURED_UNITS: u8 = 5;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_CONFIGURED_UNITS)
    }

    /// Creates a low-speed threshold in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless
    /// `configured_units` is in `2..=30`.
    pub const fn new(configured_units: u8) -> Result<Self, ValidationError> {
        if configured_units >= Self::MIN && configured_units <= Self::MAX {
            Ok(Self(configured_units))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing low speed",
                value: configured_units,
                detail: "must be 2-30 in the Menu 970 speed unit",
            })
        }
    }

    /// Returns the threshold in the Menu No. 970 unit, identical to its stored
    /// byte value.
    #[must_use]
    pub const fn as_configured_units(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredLowSpeed {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredLowSpeed> for u8 {
    fn from(value: StoredLowSpeed) -> Self {
        value.as_configured_units()
    }
}

/// Stored slow beacon rate in whole minutes (Menu No. 531, raw `1..=100`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredSlowRateMinutes(u8);

impl StoredSlowRateMinutes {
    /// Minimum encoded slow rate in minutes.
    pub const MIN: u8 = 1;
    /// Maximum encoded slow rate in minutes.
    pub const MAX: u8 = 100;
    /// Firmware V1.03 factory value in minutes.
    pub const FACTORY_DEFAULT_MINUTES: u8 = 30;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_MINUTES)
    }

    /// Creates a slow rate in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `minutes` is in
    /// `1..=100`.
    pub const fn new(minutes: u8) -> Result<Self, ValidationError> {
        if minutes >= Self::MIN && minutes <= Self::MAX {
            Ok(Self(minutes))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing slow rate",
                value: minutes,
                detail: "must be 1-100 minutes",
            })
        }
    }

    /// Returns the rate in minutes, identical to its stored byte value.
    #[must_use]
    pub const fn as_minutes(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredSlowRateMinutes {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredSlowRateMinutes> for u8 {
    fn from(value: StoredSlowRateMinutes) -> Self {
        value.as_minutes()
    }
}

/// Stored high-speed threshold in the Menu No. 970 unit (Menu No. 530, raw
/// `2..=90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredHighSpeed(u8);

impl StoredHighSpeed {
    /// Minimum encoded high-speed threshold in the configured unit.
    pub const MIN: u8 = 2;
    /// Maximum encoded high-speed threshold in the configured unit.
    pub const MAX: u8 = 90;
    /// Firmware V1.03 factory value in the configured unit.
    pub const FACTORY_DEFAULT_CONFIGURED_UNITS: u8 = 70;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_CONFIGURED_UNITS)
    }

    /// Creates a high-speed threshold in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless
    /// `configured_units` is in `2..=90`.
    pub const fn new(configured_units: u8) -> Result<Self, ValidationError> {
        if configured_units >= Self::MIN && configured_units <= Self::MAX {
            Ok(Self(configured_units))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing high speed",
                value: configured_units,
                detail: "must be 2-90 in the Menu 970 speed unit",
            })
        }
    }

    /// Returns the threshold in the Menu No. 970 unit, identical to its stored
    /// byte value.
    #[must_use]
    pub const fn as_configured_units(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredHighSpeed {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredHighSpeed> for u8 {
    fn from(value: StoredHighSpeed) -> Self {
        value.as_configured_units()
    }
}

/// Stored fast beacon rate in seconds (Menu No. 532, raw `10..=180`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredFastRateSeconds(u8);

impl StoredFastRateSeconds {
    /// Minimum encoded fast rate in seconds.
    pub const MIN: u8 = 10;
    /// Maximum encoded fast rate in seconds.
    pub const MAX: u8 = 180;
    /// Firmware V1.03 factory value in seconds.
    pub const FACTORY_DEFAULT_SECONDS: u8 = 120;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_SECONDS)
    }

    /// Creates a fast rate in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `seconds` is in
    /// `10..=180`.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds >= Self::MIN && seconds <= Self::MAX {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing fast rate",
                value: seconds,
                detail: "must be 10-180 seconds",
            })
        }
    }

    /// Returns the rate in seconds, identical to its stored byte value.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredFastRateSeconds {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredFastRateSeconds> for u8 {
    fn from(value: StoredFastRateSeconds) -> Self {
        value.as_seconds()
    }
}

/// Stored minimum turn angle in degrees (Menu No. 533, raw `5..=90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredTurnAngleDegrees(u8);

impl StoredTurnAngleDegrees {
    /// Minimum encoded turn angle in degrees.
    pub const MIN: u8 = 5;
    /// Maximum encoded turn angle in degrees.
    pub const MAX: u8 = 90;
    /// Firmware V1.03 factory value in degrees.
    pub const FACTORY_DEFAULT_DEGREES: u8 = 28;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_DEGREES)
    }

    /// Creates a turn angle in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `degrees` is in
    /// `5..=90`.
    pub const fn new(degrees: u8) -> Result<Self, ValidationError> {
        if degrees >= Self::MIN && degrees <= Self::MAX {
            Ok(Self(degrees))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing turn angle",
                value: degrees,
                detail: "must be 5-90 degrees",
            })
        }
    }

    /// Returns the angle in degrees, identical to its stored byte value.
    #[must_use]
    pub const fn as_degrees(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredTurnAngleDegrees {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredTurnAngleDegrees> for u8 {
    fn from(value: StoredTurnAngleDegrees) -> Self {
        value.as_degrees()
    }
}

/// Stored turn-slope factor (Menu No. 534, raw `1..=255`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredTurnSlope(u8);

impl StoredTurnSlope {
    /// Minimum encoded turn-slope factor.
    pub const MIN: u8 = 1;
    /// Maximum encoded turn-slope factor.
    pub const MAX: u8 = u8::MAX;
    /// Firmware V1.03 factory turn-slope factor.
    pub const FACTORY_DEFAULT_RAW: u8 = 26;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_RAW)
    }

    /// Creates a nonzero turn-slope factor.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] when `value` is zero.
    pub const fn new(value: u8) -> Result<Self, ValidationError> {
        if value >= Self::MIN {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing turn slope",
                value,
                detail: "must be 1-255",
            })
        }
    }

    /// Returns the factor, identical to its stored byte value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredTurnSlope {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredTurnSlope> for u8 {
    fn from(value: StoredTurnSlope) -> Self {
        value.as_raw()
    }
}

/// Stored minimum turn-beacon interval in seconds (Menu No. 535, raw `5..=180`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredTurnTimeSeconds(u8);

impl StoredTurnTimeSeconds {
    /// Minimum encoded turn time in seconds.
    pub const MIN: u8 = 5;
    /// Maximum encoded turn time in seconds.
    pub const MAX: u8 = 180;
    /// Firmware V1.03 factory value in seconds.
    pub const FACTORY_DEFAULT_SECONDS: u8 = 60;

    /// Returns the documented firmware V1.03 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_SECONDS)
    }

    /// Creates a turn time in the radio's accepted range.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `seconds` is in
    /// `5..=180`.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds >= Self::MIN && seconds <= Self::MAX {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "SmartBeaconing turn time",
                value: seconds,
                detail: "must be 5-180 seconds",
            })
        }
    }

    /// Returns the interval in seconds, identical to its stored byte value.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StoredTurnTimeSeconds {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StoredTurnTimeSeconds> for u8 {
    fn from(value: StoredTurnTimeSeconds) -> Self {
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
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAmbiguity {
    /// Full precision (no ambiguity). Approximately 60 feet.
    Full = 0,
    /// 1 digit removed. Approximately 1/10 mile.
    Level1 = 1,
    /// 2 digits removed. Approximately 1 mile.
    Level2 = 2,
    /// 3 digits removed. Approximately 10 miles.
    Level3 = 3,
    /// 4 digits removed. Approximately 60 miles.
    Level4 = 4,
}

impl TryFrom<u8> for PositionAmbiguity {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Full),
            1 => Ok(Self::Level1),
            2 => Ok(Self::Level2),
            3 => Ok(Self::Level3),
            4 => Ok(Self::Level4),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "position ambiguity",
                value,
                detail: "must be 0-4",
            }),
        }
    }
}

impl From<PositionAmbiguity> for u8 {
    fn from(ambiguity: PositionAmbiguity) -> Self {
        ambiguity as Self
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PacketPath {
    /// New-N path settings.
    NewN(NewNPacketPath),
    /// European relay-style path settings.
    Relay(RelayPacketPath),
    /// Region-abbreviation path settings.
    Region(RegionPacketPath),
    /// First directly entered path, backed by the 79-byte MCP field.
    Others1(Others1PacketPath),
    /// Second directly entered path, backed by a 29-byte MCP field.
    Others2(Others2PacketPath),
    /// Third directly entered path, backed by a 29-byte MCP field.
    Others3(Others3PacketPath),
}

impl PacketPath {
    /// Returns the documented factory packet path: WIDE1-1,WIDE2-1.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::NewN(NewNPacketPath::factory_default())
    }
}

/// Total-hop choice accepted by each structured packet-path mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PacketPathHops(u8);

impl PacketPathHops {
    /// Largest total-hop value accepted by Menu No. 504.
    pub const MAX: u8 = 7;

    /// Creates a total-hop value in the documented `0..=7` domain.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] when `hops` exceeds 7.
    pub const fn new(hops: u8) -> Result<Self, ValidationError> {
        if hops <= Self::MAX {
            Ok(Self(hops))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "APRS packet-path total hops",
                value: hops,
                detail: "must be 0-7",
            })
        }
    }

    /// Returns the configured total-hop value.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for PacketPathHops {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PacketPathHops> for u8 {
    fn from(value: PacketPathHops) -> Self {
        value.as_raw()
    }
}

/// Menu No. 504 New-N packet-path settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NewNPacketPath {
    wide1_1: bool,
    total_hops: PacketPathHops,
}

impl NewNPacketPath {
    /// Returns the documented factory New-N path: WIDE1-1 with two hops.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::new(true, PacketPathHops(2))
    }

    /// Creates a New-N path setting.
    #[must_use]
    pub const fn new(wide1_1: bool, total_hops: PacketPathHops) -> Self {
        Self {
            wide1_1,
            total_hops,
        }
    }

    /// Returns whether the WIDE1-1 fill-in hop is enabled.
    #[must_use]
    pub const fn wide1_1(self) -> bool {
        self.wide1_1
    }

    /// Returns the configured total-hop value.
    #[must_use]
    pub const fn total_hops(self) -> PacketPathHops {
        self.total_hops
    }
}

/// Menu No. 504 relay-style packet-path settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RelayPacketPath {
    relay: bool,
    total_hops: PacketPathHops,
}

impl RelayPacketPath {
    /// Creates a relay-style path setting.
    #[must_use]
    pub const fn new(relay: bool, total_hops: PacketPathHops) -> Self {
        Self { relay, total_hops }
    }

    /// Returns whether the RELAY hop is enabled.
    #[must_use]
    pub const fn relay(self) -> bool {
        self.relay
    }

    /// Returns the configured total-hop value.
    #[must_use]
    pub const fn total_hops(self) -> PacketPathHops {
        self.total_hops
    }
}

/// Region abbreviation stored by Menu No. 504 (up to five printable ASCII
/// bytes).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PacketPathAbbreviation(String);

impl PacketPathAbbreviation {
    /// Maximum encoded length of a region abbreviation.
    pub const MAX_LEN: usize = 5;

    /// Creates a region abbreviation accepted by the menu field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if `value` exceeds
    /// five bytes, or [`ValidationError::InvalidTextByte`] at the first byte
    /// outside printable ASCII.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            value,
            Self::MAX_LEN,
            "APRS packet-path region abbreviation",
            "must be at most 5 encoded bytes",
        )?;
        Ok(Self(value.to_owned()))
    }

    /// Returns the encoded abbreviation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Menu No. 504 region packet-path settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegionPacketPath {
    abbreviation: PacketPathAbbreviation,
    total_hops: PacketPathHops,
}

impl RegionPacketPath {
    /// Creates a region path setting.
    #[must_use]
    pub const fn new(abbreviation: PacketPathAbbreviation, total_hops: PacketPathHops) -> Self {
        Self {
            abbreviation,
            total_hops,
        }
    }

    /// Returns the region abbreviation.
    #[must_use]
    pub const fn abbreviation(&self) -> &PacketPathAbbreviation {
        &self.abbreviation
    }

    /// Returns the configured total-hop value.
    #[must_use]
    pub const fn total_hops(&self) -> PacketPathHops {
        self.total_hops
    }
}

macro_rules! packet_path_text_type {
    ($name:ident, $maximum:expr, $description:literal, $error_name:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
        pub struct $name(String);

        impl $name {
            /// Maximum encoded field length in bytes.
            pub const MAX_LEN: usize = $maximum;

            /// Creates a directly entered path that fits its MCP field.
            ///
            /// # Errors
            ///
            /// Returns [`ValidationError::TextLengthOutOfRange`] if `value`
            /// exceeds this field's encoded width, or
            /// [`ValidationError::InvalidTextByte`] at the first byte outside
            /// printable ASCII.
            pub fn new(value: &str) -> Result<Self, ValidationError> {
                validate_printable_ascii_within(
                    value,
                    Self::MAX_LEN,
                    $error_name,
                    concat!("must be at most ", stringify!($maximum), " encoded bytes"),
                )?;
                Ok(Self(value.to_owned()))
            }

            /// Returns the directly entered path.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

packet_path_text_type!(
    Others1PacketPath,
    79,
    "Menu No. 504 Others1 path stored in the 79-byte MCP field.",
    "APRS Others1 packet path"
);
packet_path_text_type!(
    Others2PacketPath,
    29,
    "Menu No. 504 Others2 path stored in a 29-byte MCP field.",
    "APRS Others2 packet path"
);
packet_path_text_type!(
    Others3PacketPath,
    29,
    "Menu No. 504 Others3 path stored in a 29-byte MCP field.",
    "APRS Others3 packet path"
);

fn validate_printable_ascii_within(
    value: &str,
    maximum_bytes: usize,
    name: &'static str,
    length_detail: &'static str,
) -> Result<(), ValidationError> {
    if value.len() > maximum_bytes {
        return Err(ValidationError::TextLengthOutOfRange {
            name,
            len: value.len(),
            detail: length_detail,
        });
    }
    if let Some((offset, value)) = value
        .bytes()
        .enumerate()
        .find(|(_, byte)| *byte != b' ' && !byte.is_ascii_graphic())
    {
        return Err(ValidationError::InvalidTextByte {
            name,
            offset,
            value,
            detail: "must contain only printable ASCII bytes 0x20-0x7E",
        });
    }
    Ok(())
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

/// TH-D75 APRS status-text menu value (up to 42 printable ASCII bytes).
///
/// The TH-D75 provides 5 status text slots. The active slot is
/// transmitted as part of the APRS status report.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct StoredStatusText(String);

impl StoredStatusText {
    /// Maximum length of a status text message.
    pub const MAX_LEN: usize = 42;

    /// Creates a new status text.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the text exceeds
    /// 42 encoded bytes, or [`ValidationError::InvalidTextByte`] at the first
    /// control or non-ASCII byte that the radio menu cannot enter.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            text,
            Self::MAX_LEN,
            "stored APRS status text",
            "must be at most 42 encoded bytes",
        )?;
        Ok(Self(text.to_owned()))
    }

    /// Returns the status text as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One of the TH-D75's five APRS status-text slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusTextSlot(u8);

impl StatusTextSlot {
    /// Number of status-text slots stored by the radio.
    pub const COUNT: u8 = 5;

    /// Returns the documented Menu No. 503 factory slot.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(0)
    }

    /// Creates a zero-based slot index in `0..5`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `index` is in
    /// `0..=4`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index < Self::COUNT {
            Ok(Self(index))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "APRS status-text slot",
                value: index,
                detail: "must be 0-4",
            })
        }
    }

    /// Returns the zero-based slot index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for StatusTextSlot {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<StatusTextSlot> for u8 {
    fn from(value: StatusTextSlot) -> Self {
        value.as_raw()
    }
}

// ---------------------------------------------------------------------------
// Waypoint settings
// ---------------------------------------------------------------------------

/// Waypoint output settings.
///
/// Controls how APRS waypoint data is formatted and output to external
/// GPS devices or PC software.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WaypointSettings {
    format: WaypointFormat,
    name_length: WaypointNameLength,
    output: WaypointOutput,
}

impl WaypointSettings {
    /// Creates waypoint output settings.
    #[must_use]
    pub const fn new(
        format: WaypointFormat,
        name_length: WaypointNameLength,
        output: WaypointOutput,
    ) -> Self {
        Self {
            format,
            name_length,
            output,
        }
    }

    /// Returns the output sentence format.
    #[must_use]
    pub const fn format(self) -> WaypointFormat {
        self.format
    }

    /// Returns the waypoint-name length.
    #[must_use]
    pub const fn name_length(self) -> WaypointNameLength {
        self.name_length
    }

    /// Returns which received waypoints are output.
    #[must_use]
    pub const fn output(self) -> WaypointOutput {
        self.output
    }
}

/// Waypoint output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointFormat {
    /// NMEA `$GPWPL` sentence format.
    Nmea,
    /// Magellan GPS format.
    Magellan,
    /// Kenwood proprietary format.
    Kenwood,
}

impl TryFrom<u8> for WaypointFormat {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Nmea),
            1 => Ok(Self::Magellan),
            2 => Ok(Self::Kenwood),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS waypoint format",
                value,
                detail: "must be 0 (NMEA), 1 (Magellan), or 2 (Kenwood)",
            }),
        }
    }
}

impl From<WaypointFormat> for u8 {
    fn from(value: WaypointFormat) -> Self {
        match value {
            WaypointFormat::Nmea => 0,
            WaypointFormat::Magellan => 1,
            WaypointFormat::Kenwood => 2,
        }
    }
}

/// Number of characters emitted in a waypoint name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum WaypointNameLength {
    /// Six-character waypoint names.
    Characters6 = 6,
    /// Seven-character waypoint names.
    Characters7 = 7,
    /// Eight-character waypoint names.
    Characters8 = 8,
    /// Nine-character waypoint names.
    Characters9 = 9,
}

impl WaypointNameLength {
    /// Returns the selected character count.
    #[must_use]
    pub const fn as_characters(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for WaypointNameLength {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            6 => Ok(Self::Characters6),
            7 => Ok(Self::Characters7),
            8 => Ok(Self::Characters8),
            9 => Ok(Self::Characters9),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS waypoint-name length",
                value,
                detail: "must be 6, 7, 8, or 9 characters",
            }),
        }
    }
}

impl From<WaypointNameLength> for u8 {
    fn from(value: WaypointNameLength) -> Self {
        value.as_characters()
    }
}

/// Which received station waypoints are output by the radio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaypointOutput {
    /// Output every received waypoint.
    All,
    /// Output stations within the configured position limit.
    Local,
    /// Output waypoints accepted by the packet filter.
    Filtered,
}

impl TryFrom<u8> for WaypointOutput {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::All),
            1 => Ok(Self::Local),
            2 => Ok(Self::Filtered),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS waypoint output",
                value,
                detail: "must be 0 (All), 1 (Local), or 2 (Filtered)",
            }),
        }
    }
}

impl From<WaypointOutput> for u8 {
    fn from(value: WaypointOutput) -> Self {
        match value {
            WaypointOutput::All => 0,
            WaypointOutput::Local => 1,
            WaypointOutput::Filtered => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Packet filter
// ---------------------------------------------------------------------------

/// APRS packet filter settings.
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
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1..=250 => Ok(Self::Distance(PacketFilterDistance(value))),
            _ => Err(ValidationError::SettingOutOfRange {
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

    /// Creates a distance in the stepped Menu No. 550 domain.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless
    /// `configured_units` is in `10..=2500` and divisible by 10.
    pub fn new(configured_units: u16) -> Result<Self, ValidationError> {
        if (Self::MIN..=Self::MAX).contains(&configured_units)
            && configured_units.is_multiple_of(Self::STEP)
        {
            u8::try_from(configured_units / Self::STEP)
                .map(Self)
                .map_err(|_| ValidationError::IntegerOutOfRange {
                    name: "APRS packet-filter position distance",
                    value: i64::from(configured_units),
                    detail: "must be 10-2500 configured distance units in steps of 10",
                })
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "APRS packet-filter position distance",
                value: i64::from(configured_units),
                detail: "must be 10-2500 configured distance units in steps of 10",
            })
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
    type Error = ValidationError;

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
            Err(ValidationError::SettingOutOfRange {
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
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the UTF-8
    /// representation exceeds 32 bytes.
    pub fn new(phrase: &str) -> Result<Self, ValidationError> {
        if phrase.len() <= Self::MAX_LEN {
            Ok(Self(phrase.to_owned()))
        } else {
            Err(ValidationError::TextLengthOutOfRange {
                name: "APRS user phrase",
                len: phrase.len(),
                detail: "must be at most 32 encoded bytes",
            })
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

/// APRS auto-reply message settings.
///
/// When enabled, the radio automatically replies to incoming APRS
/// messages with a configured response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReplySettings {
    enabled: bool,
    reply_to: AutoReplyTarget,
    delay_time: AutoReplyDelay,
    message: ReplyMessage,
}

impl AutoReplySettings {
    /// Creates auto-reply settings.
    #[must_use]
    pub const fn new(
        enabled: bool,
        reply_to: AutoReplyTarget,
        delay_time: AutoReplyDelay,
        message: ReplyMessage,
    ) -> Self {
        Self {
            enabled,
            reply_to,
            delay_time,
            message,
        }
    }

    /// Returns whether automatic replies are enabled.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Returns the callsign filter that may trigger a reply.
    #[must_use]
    pub const fn reply_to(&self) -> &AutoReplyTarget {
        &self.reply_to
    }

    /// Returns the configured wait time.
    #[must_use]
    pub const fn delay_time(&self) -> AutoReplyDelay {
        self.delay_time
    }

    /// Returns the automatic reply text.
    #[must_use]
    pub const fn message(&self) -> &ReplyMessage {
        &self.message
    }
}

impl Default for AutoReplySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            reply_to: AutoReplyTarget::Any,
            delay_time: AutoReplyDelay::None,
            message: ReplyMessage::default(),
        }
    }
}

/// Callsign filter stored by Menu No. 562.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AutoReplyTarget {
    /// Reply to every station (`*` in the radio field).
    #[default]
    Any,
    /// Reply only to one canonical APRS station address.
    Exact(AprsCallsign),
    /// Reply to callsigns beginning with this prefix.
    Prefix(AutoReplyCallsignPrefix),
}

impl AutoReplyTarget {
    /// Parses the TH-D75's canonical exact or trailing-wildcard syntax.
    ///
    /// # Errors
    ///
    /// Returns a [`ValidationError`] unless `value` is `*`, one canonical
    /// APRS station address, or a valid uppercase callsign prefix followed by
    /// `*`.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        if value == "*" {
            return Ok(Self::Any);
        }
        if let Some(prefix) = value.strip_suffix('*') {
            return AutoReplyCallsignPrefix::new(prefix).map(Self::Prefix);
        }
        AprsCallsign::new(value).map(Self::Exact).map_err(|error| {
            ValidationError::InvalidTextValue {
                name: "APRS auto-reply target",
                value: value.to_owned(),
                detail: "must be '*', a canonical APRS callsign, or an uppercase prefix followed by '*'",
                reason: error.to_string(),
            }
        })
    }
}

impl fmt::Display for AutoReplyTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => formatter.write_str("*"),
            Self::Exact(callsign) => callsign.fmt(formatter),
            Self::Prefix(prefix) => write!(formatter, "{}*", prefix.as_str()),
        }
    }
}

/// Prefix used by a Menu No. 562 trailing-wildcard callsign filter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AutoReplyCallsignPrefix(String);

impl AutoReplyCallsignPrefix {
    /// Maximum prefix length before the trailing wildcard.
    pub const MAX_LEN: usize = 8;

    /// Creates a nonempty uppercase callsign prefix.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] unless `value` is
    /// one to eight bytes, or [`ValidationError::InvalidTextByte`] at the
    /// first byte outside uppercase ASCII letters, digits, and hyphen.
    pub fn new(value: &str) -> Result<Self, ValidationError> {
        if value.is_empty() || value.len() > Self::MAX_LEN {
            return Err(ValidationError::TextLengthOutOfRange {
                name: "APRS auto-reply callsign prefix",
                len: value.len(),
                detail: "must be 1-8 encoded bytes before the trailing wildcard",
            });
        }
        if let Some((offset, value)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_uppercase() && !byte.is_ascii_digit() && *byte != b'-')
        {
            return Err(ValidationError::InvalidTextByte {
                name: "APRS auto-reply callsign prefix",
                offset,
                value,
                detail: "must contain only uppercase ASCII letters, digits, or hyphen",
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the prefix without its trailing wildcard.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Auto-reply delay time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AutoReplyDelay {
    /// No delay.
    #[default]
    None,
    /// 10 second delay.
    Sec10,
    /// 20 second delay.
    Sec20,
    /// 30 second delay.
    Sec30,
    /// 60 second delay.
    Sec60,
}

impl AutoReplyDelay {
    /// Returns the wait time in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Sec10 => 10,
            Self::Sec20 => 20,
            Self::Sec30 => 30,
            Self::Sec60 => 60,
        }
    }

    /// Returns the MCP enum index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Sec10 => 1,
            Self::Sec20 => 2,
            Self::Sec30 => 3,
            Self::Sec60 => 4,
        }
    }
}

impl TryFrom<u8> for AutoReplyDelay {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Sec10),
            2 => Ok(Self::Sec20),
            3 => Ok(Self::Sec30),
            4 => Ok(Self::Sec60),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS auto-reply delay",
                value,
                detail: "must be raw 0-4 (0, 10, 20, 30, or 60 seconds)",
            }),
        }
    }
}

impl From<AutoReplyDelay> for u8 {
    fn from(value: AutoReplyDelay) -> Self {
        value.as_raw()
    }
}

/// APRS reply message text entered through Menu No. 564.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ReplyMessage(String);

impl ReplyMessage {
    /// Maximum length of a reply message.
    pub const MAX_LEN: usize = 50;

    /// Creates a new reply message.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the text exceeds
    /// the documented 50-byte menu limit, or
    /// [`ValidationError::InvalidTextByte`] at the first control or non-ASCII
    /// byte. The underlying 67-byte MCP field is storage capacity, not the
    /// radio's user-input limit.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            text,
            Self::MAX_LEN,
            "APRS auto-reply message",
            "must be at most 50 encoded bytes",
        )?;
        Ok(Self(text.to_owned()))
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

/// APRS notification sound and display settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NotificationSettings {
    rx_beep: RxBeep,
    tx_beep: bool,
    special_call: Option<AprsCallsign>,
}

impl NotificationSettings {
    /// Creates APRS notification settings.
    #[must_use]
    pub const fn new(rx_beep: RxBeep, tx_beep: bool, special_call: Option<AprsCallsign>) -> Self {
        Self {
            rx_beep,
            tx_beep,
            special_call,
        }
    }

    /// Returns which received packets produce a beep.
    #[must_use]
    pub const fn rx_beep(&self) -> RxBeep {
        self.rx_beep
    }

    /// Returns whether nonmanual beacon transmission produces a beep.
    #[must_use]
    pub const fn tx_beep(&self) -> bool {
        self.tx_beep
    }

    /// Returns the station whose messages produce the special-call sound.
    #[must_use]
    pub const fn special_call(&self) -> Option<&AprsCallsign> {
        self.special_call.as_ref()
    }
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            rx_beep: RxBeep::Off,
            tx_beep: false,
            special_call: None,
        }
    }
}

/// Received-packet classes that produce the APRS notification beep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RxBeep {
    /// Beep for every packet, including duplicates and invalid data.
    All,
    /// Beep for directed messages and newly received packet data.
    AllNew,
    /// Beep for directed messages and a digipeated copy of this station's data.
    Mine,
    /// Beep only for a message addressed to this station.
    MessageOnly,
    /// Do not emit an APRS receive beep.
    #[default]
    Off,
}

impl TryFrom<u8> for RxBeep {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::All),
            1 => Ok(Self::AllNew),
            2 => Ok(Self::Mine),
            3 => Ok(Self::MessageOnly),
            4 => Ok(Self::Off),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS RX beep",
                value,
                detail: "must be raw 0-4 (All, All New, Mine, Message Only, or Off)",
            }),
        }
    }
}

impl From<RxBeep> for u8 {
    fn from(value: RxBeep) -> Self {
        match value {
            RxBeep::All => 0,
            RxBeep::AllNew => 1,
            RxBeep::Mine => 2,
            RxBeep::MessageOnly => 3,
            RxBeep::Off => 4,
        }
    }
}

/// Display area setting for incoming APRS data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayArea {
    /// Use the full display for new, duplicate, and own-station data.
    EntireAlways,
    /// Use the full display only for newly received data.
    EntireDisplay,
    /// Show received data on one line at the top of the display.
    OneLine,
}

impl TryFrom<u8> for DisplayArea {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::OneLine),
            1 => Ok(Self::EntireDisplay),
            2 => Ok(Self::EntireAlways),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS display area",
                value,
                detail: "must be raw 0 (One Line), 1 (Entire Display), or 2 (Entire Always)",
            }),
        }
    }
}

impl From<DisplayArea> for u8 {
    fn from(value: DisplayArea) -> Self {
        match value {
            DisplayArea::OneLine => 0,
            DisplayArea::EntireDisplay => 1,
            DisplayArea::EntireAlways => 2,
        }
    }
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
    /// 20 second interrupt.
    Sec20,
    /// 30 second interrupt.
    Sec30,
    /// 60 second interrupt.
    Sec60,
    /// Hold the indication until the operator cancels it.
    Infinite,
}

impl InterruptTime {
    /// Returns the finite duration, or `None` for [`Self::Infinite`].
    #[must_use]
    pub const fn as_seconds(self) -> Option<u8> {
        match self {
            Self::Sec3 => Some(3),
            Self::Sec5 => Some(5),
            Self::Sec10 => Some(10),
            Self::Sec20 => Some(20),
            Self::Sec30 => Some(30),
            Self::Sec60 => Some(60),
            Self::Infinite => None,
        }
    }
}

impl TryFrom<u8> for InterruptTime {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Sec3),
            1 => Ok(Self::Sec5),
            2 => Ok(Self::Sec10),
            3 => Ok(Self::Sec20),
            4 => Ok(Self::Sec30),
            5 => Ok(Self::Sec60),
            6 => Ok(Self::Infinite),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS interrupt time",
                value,
                detail: "must be raw 0-6 (3, 5, 10, 20, 30, 60 seconds, or Infinite)",
            }),
        }
    }
}

impl From<InterruptTime> for u8 {
    fn from(value: InterruptTime) -> Self {
        match value {
            InterruptTime::Sec3 => 0,
            InterruptTime::Sec5 => 1,
            InterruptTime::Sec10 => 2,
            InterruptTime::Sec20 => 3,
            InterruptTime::Sec30 => 4,
            InterruptTime::Sec60 => 5,
            InterruptTime::Infinite => 6,
        }
    }
}

// ---------------------------------------------------------------------------
// Digipeater
// ---------------------------------------------------------------------------

/// Radio-resident APRS digipeater settings from Menus No. 580 through 588.
///
/// Each field corresponds to one menu entry. The three UI digipeating modes
/// are independent, as is callsign digipeating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredDigipeaterSettings {
    /// Independently enabled digipeater functions from Menus No. 580, 582,
    /// 584, and 587.
    pub enabled_functions: HashSet<DigipeaterFunction>,
    /// Duplicate-suppression interval from Menu No. 581.
    pub ui_check: UiCheckSeconds,
    /// Four Menu No. 583 `UIdigi` alias slots.
    pub ui_digi_aliases: [UiDigiAlias; 4],
    /// Menu No. 585 `UIflood` alias.
    pub ui_flood_alias: FloodAlias,
    /// Menu No. 586 callsign-substitution behavior.
    pub ui_flood_substitution: UiFloodSubstitution,
    /// Menu No. 588 `UItrace` alias.
    pub ui_trace_alias: TraceAlias,
}

/// One independently enabled APRS digipeater function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigipeaterFunction {
    /// Menu No. 580 `Digipeat(MyCall)`.
    MyCallsign,
    /// Menu No. 582 `UIdigipeat`.
    UiDigipeat,
    /// Menu No. 584 `UIflood`.
    UiFlood,
    /// Menu No. 587 `UItrace`.
    UiTrace,
}

/// Menu No. 581 duplicate-suppression interval in seconds.
///
/// The User Manual summary table says 1 through 250 seconds, while its
/// detailed Menu No. 581 instructions and the memory schema admit 0 through
/// 250. Zero is preserved as an exact value; its runtime meaning has not been
/// qualified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UiCheckSeconds(u8);

impl UiCheckSeconds {
    /// Largest value accepted by Menu No. 581.
    pub const MAX: u8 = 250;
    /// Documented factory interval in seconds.
    pub const FACTORY_DEFAULT_SECONDS: u8 = 28;

    /// Returns the documented Menu No. 581 factory value.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self(Self::FACTORY_DEFAULT_SECONDS)
    }

    /// Creates a duplicate-suppression interval.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] when `seconds` exceeds
    /// 250.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds <= Self::MAX {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "APRS UIcheck interval",
                value: seconds,
                detail: "must be 0-250 seconds",
            })
        }
    }

    /// Returns the interval in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }

    /// Returns the MCP byte.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for UiCheckSeconds {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<UiCheckSeconds> for u8 {
    fn from(value: UiCheckSeconds) -> Self {
        value.as_raw()
    }
}

/// One Menu No. 583 `UIdigi` alias slot.
///
/// The radio stores four comma-separated aliases in a 39-byte field, so each
/// slot accepts zero to nine printable ASCII bytes and excludes the comma
/// delimiter. The manual does not publish a narrower character domain.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct UiDigiAlias(String);

impl UiDigiAlias {
    /// Maximum encoded alias length.
    pub const MAX_LEN: usize = 9;

    /// Creates a `UIdigi` alias that fits one Menu No. 583 slot.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the alias exceeds
    /// nine bytes, or [`ValidationError::InvalidTextByte`] for non-printable
    /// ASCII or the comma storage delimiter.
    pub fn new(alias: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            alias,
            Self::MAX_LEN,
            "APRS UIdigi alias",
            "must be at most 9 encoded bytes",
        )?;
        if let Some(offset) = alias.bytes().position(|value| value == b',') {
            return Err(ValidationError::InvalidTextByte {
                name: "APRS UIdigi alias",
                offset,
                value: b',',
                detail: "must not contain the comma storage delimiter",
            });
        }
        Ok(Self(alias.to_owned()))
    }

    /// Returns the alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `UIflood` alias (up to five uppercase ASCII letters or digits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FloodAlias(String);

impl FloodAlias {
    /// Maximum length of a flood alias.
    pub const MAX_LEN: usize = 5;

    /// Creates a new flood alias.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the alias exceeds
    /// five encoded bytes, or [`ValidationError::InvalidTextByte`] at the
    /// first byte outside `A` through `Z` or `0` through `9`.
    pub fn new(alias: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            alias,
            Self::MAX_LEN,
            "APRS UIflood alias",
            "must be at most 5 encoded bytes",
        )?;
        if let Some((offset, value)) = alias
            .bytes()
            .enumerate()
            .find(|(_, value)| !value.is_ascii_uppercase() && !value.is_ascii_digit())
        {
            return Err(ValidationError::InvalidTextByte {
                name: "APRS UIflood alias",
                offset,
                value,
                detail: "must contain only uppercase ASCII letters or digits",
            });
        }
        Ok(Self(alias.to_owned()))
    }

    /// Returns the flood alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `UItrace` alias (up to five printable ASCII bytes, e.g. `WIDE2`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceAlias(String);

impl TraceAlias {
    /// Maximum length of a trace alias.
    pub const MAX_LEN: usize = 5;

    /// Returns the fallback alias documented for Menu No. 588.
    #[must_use]
    pub fn factory_default() -> Self {
        Self("TEMP".to_owned())
    }

    /// Creates a new trace alias.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the alias exceeds
    /// five encoded bytes, or [`ValidationError::InvalidTextByte`] at the
    /// first control or non-ASCII byte the radio menu cannot represent.
    pub fn new(alias: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            alias,
            Self::MAX_LEN,
            "APRS UItrace alias",
            "must be at most 5 encoded bytes",
        )?;
        Ok(Self(alias.to_owned()))
    }

    /// Returns the trace alias as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Menu No. 586 `UIflood` callsign-substitution behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UiFloodSubstitution {
    /// Always insert this station's callsign, replacing an existing one.
    Id,
    /// Never insert or replace a callsign.
    NoId,
    /// Insert this station's callsign only when no callsign is present.
    First,
}

impl UiFloodSubstitution {
    /// Returns the documented Menu No. 586 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::First
    }

    /// Returns the MCP representation.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Id => 0,
            Self::NoId => 1,
            Self::First => 2,
        }
    }
}

impl TryFrom<u8> for UiFloodSubstitution {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Id),
            1 => Ok(Self::NoId),
            2 => Ok(Self::First),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS UIflood substitution",
                value,
                detail: "must be 0 (ID), 1 (NOID), or 2 (First)",
            }),
        }
    }
}

impl From<UiFloodSubstitution> for u8 {
    fn from(value: UiFloodSubstitution) -> Self {
        value.as_raw()
    }
}

// ---------------------------------------------------------------------------
// QSY information
// ---------------------------------------------------------------------------

/// QSY (frequency change) information settings.
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
pub struct QsySettings {
    info_in_status: bool,
    tone_narrow: bool,
    shift_offset: bool,
    limit_distance: QsyLimitDistance,
}

impl QsySettings {
    /// Creates QSY information settings.
    #[must_use]
    pub const fn new(
        info_in_status: bool,
        tone_narrow: bool,
        shift_offset: bool,
        limit_distance: QsyLimitDistance,
    ) -> Self {
        Self {
            info_in_status,
            tone_narrow,
            shift_offset,
            limit_distance,
        }
    }

    /// Returns whether status beacons include QSY information.
    #[must_use]
    pub const fn info_in_status(self) -> bool {
        self.info_in_status
    }

    /// Returns whether QSY data includes tone and narrow-FM information.
    #[must_use]
    pub const fn tone_narrow(self) -> bool {
        self.tone_narrow
    }

    /// Returns whether QSY data includes repeater shift and offset.
    #[must_use]
    pub const fn shift_offset(self) -> bool {
        self.shift_offset
    }

    /// Returns the configured QSY display-distance limit.
    #[must_use]
    pub const fn limit_distance(self) -> QsyLimitDistance {
        self.limit_distance
    }
}

/// Menu No. 523 QSY display-distance limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum QsyLimitDistance {
    /// Do not limit received QSY information by distance.
    #[default]
    Off,
    /// Maximum distance in the unit selected by Menu No. 970.
    Distance(QsyDistance),
}

/// A Menu No. 523 QSY limit from 10 through 2500 in steps of 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QsyDistance(u8);

impl QsyDistance {
    /// Minimum QSY limit in configured distance units.
    pub const MIN: u16 = 10;
    /// Maximum QSY limit in configured distance units.
    pub const MAX: u16 = 2500;
    /// QSY distance step in configured units.
    pub const STEP: u16 = 10;

    /// Creates a QSY distance in the radio's stepped menu domain.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless
    /// `configured_units` is in `10..=2500` and divisible by 10.
    pub fn new(configured_units: u16) -> Result<Self, ValidationError> {
        if (Self::MIN..=Self::MAX).contains(&configured_units)
            && configured_units.is_multiple_of(Self::STEP)
        {
            u8::try_from(configured_units / Self::STEP)
                .map(Self)
                .map_err(|_| ValidationError::IntegerOutOfRange {
                    name: "APRS QSY limit distance",
                    value: i64::from(configured_units),
                    detail: "must be 10-2500 configured distance units in steps of 10",
                })
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "APRS QSY limit distance",
                value: i64::from(configured_units),
                detail: "must be 10-2500 configured distance units in steps of 10",
            })
        }
    }

    /// Returns the limit in the distance unit selected by Menu No. 970.
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

impl TryFrom<u8> for QsyLimitDistance {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1..=250 => Ok(Self::Distance(QsyDistance(value))),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS QSY limit distance",
                value,
                detail: "must be raw 0 (Off) or 1-250 (10-2500 configured distance units)",
            }),
        }
    }
}

impl From<QsyLimitDistance> for u8 {
    fn from(value: QsyLimitDistance) -> Self {
        match value {
            QsyLimitDistance::Off => 0,
            QsyLimitDistance::Distance(distance) => distance.as_raw(),
        }
    }
}

// ---------------------------------------------------------------------------
// Voice alert
// ---------------------------------------------------------------------------

/// Voice alert settings.
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
pub struct VoiceAlertSettings {
    mode: VoiceAlertMode,
    tone_code: ToneCode,
}

impl VoiceAlertSettings {
    /// Returns the documented Menu Nos. 592 and 593 factory choices.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self {
            mode: VoiceAlertMode::Off,
            tone_code: ToneCode::TONE_100HZ,
        }
    }

    /// Creates voice-alert settings.
    #[must_use]
    pub const fn new(mode: VoiceAlertMode, tone_code: ToneCode) -> Self {
        Self { mode, tone_code }
    }

    /// Returns the voice-alert operating mode.
    #[must_use]
    pub const fn mode(self) -> VoiceAlertMode {
        self.mode
    }

    /// Returns the voice-alert CTCSS tone.
    #[must_use]
    pub const fn tone_code(self) -> ToneCode {
        self.tone_code
    }
}

/// Menu No. 592 voice-alert operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VoiceAlertMode {
    /// Voice alert is disabled.
    #[default]
    Off,
    /// Add the selected tone to transmitted packets and monitor the tone.
    On,
    /// Monitor the selected tone without adding it to transmitted packets.
    RxOnly,
}

impl TryFrom<u8> for VoiceAlertMode {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::On),
            2 => Ok(Self::RxOnly),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "APRS voice-alert mode",
                value,
                detail: "must be 0 (Off), 1 (On), or 2 (RX Only)",
            }),
        }
    }
}

impl From<VoiceAlertMode> for u8 {
    fn from(value: VoiceAlertMode) -> Self {
        match value {
            VoiceAlertMode::Off => 0,
            VoiceAlertMode::On => 1,
            VoiceAlertMode::RxOnly => 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Group codes
// ---------------------------------------------------------------------------

/// One Menu No. 594 message group code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageGroupCode(String);

impl MessageGroupCode {
    /// Maximum encoded length of one message group code.
    pub const MAX_LEN: usize = 9;

    /// Creates a nonempty message group code from the documented character
    /// set: uppercase letters, digits, hyphen, and wildcard.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] unless `code` is one
    /// to nine bytes, or [`ValidationError::InvalidTextByte`] at the first
    /// byte outside the documented character set.
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        if code.is_empty() || code.len() > Self::MAX_LEN {
            return Err(ValidationError::TextLengthOutOfRange {
                name: "APRS message group code",
                len: code.len(),
                detail: "must be 1-9 encoded bytes",
            });
        }
        if let Some((offset, value)) = code.bytes().enumerate().find(|(_, byte)| {
            !byte.is_ascii_uppercase() && !byte.is_ascii_digit() && !matches!(*byte, b'-' | b'*')
        }) {
            return Err(ValidationError::InvalidTextByte {
                name: "APRS message group code",
                offset,
                value,
                detail: "must contain only uppercase ASCII letters, digits, hyphen, or wildcard",
            });
        }
        Ok(Self(code.to_owned()))
    }

    /// Returns this group code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Up to six message group codes stored by Menu No. 594.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageGroupCodes(Vec<MessageGroupCode>);

impl MessageGroupCodes {
    /// Maximum number of simultaneous message group codes.
    pub const MAX_CODES: usize = 6;

    /// Returns the documented factory message groups: ALL, QST, CQ, and KWD.
    #[must_use]
    pub fn factory_default() -> Self {
        Self(
            ["ALL", "QST", "CQ", "KWD"]
                .into_iter()
                .map(|code| MessageGroupCode(code.to_owned()))
                .collect(),
        )
    }

    /// Creates a group-code list with no more than six entries.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CollectionLengthOutOfRange`] when `codes`
    /// contains more than six entries.
    pub fn new(codes: Vec<MessageGroupCode>) -> Result<Self, ValidationError> {
        if codes.len() <= Self::MAX_CODES {
            Ok(Self(codes))
        } else {
            Err(ValidationError::CollectionLengthOutOfRange {
                name: "APRS message group code list",
                len: codes.len(),
                detail: "must contain at most 6 entries",
            })
        }
    }

    /// Parses the comma-separated MCP representation.
    ///
    /// # Errors
    ///
    /// Returns a [`ValidationError`] when any code is invalid or the list
    /// contains more than six entries.
    pub fn from_csv(value: &str) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Ok(Self(Vec::new()));
        }
        let codes = value
            .split(',')
            .map(MessageGroupCode::new)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(codes)
    }

    /// Returns the configured codes in radio order.
    #[must_use]
    pub fn as_slice(&self) -> &[MessageGroupCode] {
        &self.0
    }
}

impl fmt::Display for MessageGroupCodes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, code) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            formatter.write_str(code.as_str())?;
        }
        Ok(())
    }
}

/// One Menu No. 595 bulletin group code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BulletinGroupCode(String);

impl BulletinGroupCode {
    /// Maximum encoded length of one bulletin group code.
    pub const MAX_LEN: usize = 5;

    /// Creates a nonempty bulletin group code from uppercase letters,
    /// digits, and hyphen.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] unless `code` is one
    /// to five bytes, or [`ValidationError::InvalidTextByte`] at the first
    /// byte outside the documented character set.
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        if code.is_empty() || code.len() > Self::MAX_LEN {
            return Err(ValidationError::TextLengthOutOfRange {
                name: "APRS bulletin group code",
                len: code.len(),
                detail: "must be 1-5 encoded bytes",
            });
        }
        if let Some((offset, value)) = code
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_uppercase() && !byte.is_ascii_digit() && *byte != b'-')
        {
            return Err(ValidationError::InvalidTextByte {
                name: "APRS bulletin group code",
                offset,
                value,
                detail: "must contain only uppercase ASCII letters, digits, or hyphen",
            });
        }
        Ok(Self(code.to_owned()))
    }

    /// Returns this group code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Up to six bulletin group codes stored by Menu No. 595.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct BulletinGroupCodes(Vec<BulletinGroupCode>);

impl BulletinGroupCodes {
    /// Maximum number of simultaneous bulletin group codes.
    pub const MAX_CODES: usize = 6;

    /// Creates a group-code list with no more than six entries.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::CollectionLengthOutOfRange`] when `codes`
    /// contains more than six entries.
    pub fn new(codes: Vec<BulletinGroupCode>) -> Result<Self, ValidationError> {
        if codes.len() <= Self::MAX_CODES {
            Ok(Self(codes))
        } else {
            Err(ValidationError::CollectionLengthOutOfRange {
                name: "APRS bulletin group code list",
                len: codes.len(),
                detail: "must contain at most 6 entries",
            })
        }
    }

    /// Parses the comma-separated MCP representation.
    ///
    /// # Errors
    ///
    /// Returns a [`ValidationError`] when any code is invalid or the list
    /// contains more than six entries.
    pub fn from_csv(value: &str) -> Result<Self, ValidationError> {
        if value.is_empty() {
            return Ok(Self(Vec::new()));
        }
        let codes = value
            .split(',')
            .map(BulletinGroupCode::new)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(codes)
    }

    /// Returns the configured codes in radio order.
    #[must_use]
    pub fn as_slice(&self) -> &[BulletinGroupCode] {
        &self.0
    }
}

impl fmt::Display for BulletinGroupCodes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, code) in self.0.iter().enumerate() {
            if index != 0 {
                formatter.write_str(",")?;
            }
            formatter.write_str(code.as_str())?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NAVITRA
// ---------------------------------------------------------------------------

/// NAVITRA (navigation/tracking) settings.
///
/// NAVITRA is a Japanese APRS-like system for position tracking.
/// The TH-D75 supports NAVITRA alongside standard APRS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavitraSettings {
    group_mode: NavitraGroupMode,
    group_code: NavitraGroupCode,
    messages: [NavitraMessage; 5],
    active_message: NavitraMessageSlot,
}

impl NavitraSettings {
    /// Creates NAVITRA settings with all five message slots.
    #[must_use]
    pub const fn new(
        mode: NavitraGroupMode,
        code: NavitraGroupCode,
        messages: [NavitraMessage; 5],
        active_message: NavitraMessageSlot,
    ) -> Self {
        Self {
            group_mode: mode,
            group_code: code,
            messages,
            active_message,
        }
    }

    /// Returns the NAVITRA group-filter mode.
    #[must_use]
    pub const fn group_mode(&self) -> NavitraGroupMode {
        self.group_mode
    }

    /// Returns the NAVITRA group code.
    #[must_use]
    pub const fn group_code(&self) -> &NavitraGroupCode {
        &self.group_code
    }

    /// Returns all five stored NAVITRA messages.
    #[must_use]
    pub const fn messages(&self) -> &[NavitraMessage; 5] {
        &self.messages
    }

    /// Returns the selected NAVITRA message slot.
    #[must_use]
    pub const fn active_message(&self) -> NavitraMessageSlot {
        self.active_message
    }
}

/// NAVITRA group filtering mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum NavitraGroupMode {
    /// NAVITRA group filtering disabled.
    #[default]
    Off,
    /// Show only stations in the configured NAVITRA group.
    On,
}

/// Three-byte NAVITRA group code identified in the MCP schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct NavitraGroupCode(String);

impl NavitraGroupCode {
    /// Maximum encoded group-code length.
    pub const MAX_LEN: usize = 3;

    /// Creates a NAVITRA group code that fits the MCP field.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if `code` exceeds
    /// three bytes, or [`ValidationError::InvalidTextByte`] at the first byte
    /// outside printable ASCII.
    pub fn new(code: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            code,
            Self::MAX_LEN,
            "NAVITRA group code",
            "must be at most 3 encoded bytes",
        )?;
        Ok(Self(code.to_owned()))
    }

    /// Returns the NAVITRA group code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One of the five NAVITRA message slots identified in the MCP schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NavitraMessageSlot(u8);

impl NavitraMessageSlot {
    /// Number of NAVITRA message slots.
    pub const COUNT: u8 = 5;

    /// Creates a zero-based slot index in `0..5`.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `index` is in
    /// `0..=4`.
    pub const fn new(index: u8) -> Result<Self, ValidationError> {
        if index < Self::COUNT {
            Ok(Self(index))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "NAVITRA message slot",
                value: index,
                detail: "must be 0-4",
            })
        }
    }

    /// Returns the zero-based slot index.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for NavitraMessageSlot {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<NavitraMessageSlot> for u8 {
    fn from(value: NavitraMessageSlot) -> Self {
        value.as_raw()
    }
}

/// NAVITRA message text stored in one 20-byte MCP field.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct NavitraMessage(String);

impl NavitraMessage {
    /// Maximum length of a NAVITRA message.
    pub const MAX_LEN: usize = 20;

    /// Creates a new NAVITRA message.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if the text exceeds
    /// 20 bytes, or [`ValidationError::InvalidTextByte`] at the first byte
    /// that cannot be represented by the radio's menu-facing ASCII model.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            text,
            Self::MAX_LEN,
            "NAVITRA message",
            "must be at most 20 encoded bytes",
        )?;
        Ok(Self(text.to_owned()))
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
/// radio's separate NAVITRA settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AprsNetwork {
    /// Active network type.
    pub network_type: AprsNetworkType,
    /// Address used when `network_type` is [`AprsNetworkType::Altnet`].
    pub altnet_address: AltnetAddress,
}

/// APRS network type stored at MCP offset `0x1460`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsNetworkType {
    /// Normal APRS operation using the fixed `APK005` destination.
    AprsApk005,
    /// Alternate-network operation using a user-supplied address.
    Altnet,
}

impl AprsNetworkType {
    /// Returns the documented Menu No. 591 factory choice.
    #[must_use]
    pub const fn factory_default() -> Self {
        Self::AprsApk005
    }
}

impl TryFrom<u8> for AprsNetworkType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::AprsApk005),
            1 => Ok(Self::Altnet),
            _ => Err(ValidationError::SettingOutOfRange {
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

/// Printable ASCII Altnet address stored in the six-byte MCP field at `0x1461`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AltnetAddress(String);

impl AltnetAddress {
    /// Maximum encoded address length in bytes.
    pub const MAX_LEN: usize = 6;

    /// Creates an Altnet address that fits the radio's six-byte field and can
    /// be represented by its single-byte text editor.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::TextLengthOutOfRange`] if `address` exceeds
    /// six bytes, or [`ValidationError::InvalidTextByte`] at the first byte
    /// outside printable ASCII.
    pub fn new(address: &str) -> Result<Self, ValidationError> {
        validate_printable_ascii_within(
            address,
            Self::MAX_LEN,
            "APRS Altnet address",
            "must be at most 6 encoded bytes",
        )?;
        Ok(Self(address.to_owned()))
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
pub struct StoredAprsMessage {
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
        let cs = AprsCallsign::new("N0CALL-9")?;
        assert_eq!(cs.to_string(), "N0CALL-9");
        assert_eq!(cs.base_callsign().as_str(), "N0CALL");
        assert_eq!(cs.ssid(), Ssid::new(9)?);
        Ok(())
    }

    #[test]
    fn aprs_callsign_max_length() -> TestResult {
        let cs = AprsCallsign::new("N0CALL-15")?;
        assert_eq!(cs.to_string(), "N0CALL-15");
        Ok(())
    }

    #[test]
    fn aprs_callsign_rejects_noncanonical_and_wire_unsafe_text() {
        for invalid in [
            "",
            "n0call-7",
            "N0 CALL",
            "N0CALL!",
            "N0CALL-0",
            "N0CALL-07",
            "N0CALL-16",
            "N0CALL-150",
            "NØCALL",
            "N0CALL\rID",
            "N0CALL\n",
        ] {
            assert!(
                AprsCallsign::new(invalid).is_err(),
                "accepted invalid APRS callsign {invalid:?}"
            );
        }
    }

    #[test]
    fn status_text_valid() -> TestResult {
        let st = StoredStatusText::new("Testing 1 2 3")?;
        assert_eq!(st.as_str(), "Testing 1 2 3");
        Ok(())
    }

    #[test]
    fn status_text_max_length() {
        let text = "a".repeat(42);
        assert!(StoredStatusText::new(&text).is_ok());
    }

    #[test]
    fn status_text_rejects_unrepresentable_input() {
        assert!(StoredStatusText::new(&"a".repeat(43)).is_err());
        assert!(StoredStatusText::new("line\nbreak").is_err());
        assert!(StoredStatusText::new("café").is_err());
    }

    #[test]
    fn tx_delay_accepts_exact_menu_choices() -> TestResult {
        for (raw, milliseconds) in [100, 150, 200, 300, 400, 500, 750, 1000]
            .into_iter()
            .enumerate()
        {
            let delay = TxDelay::new(milliseconds)?;
            assert_eq!(delay.as_milliseconds(), milliseconds);
            assert_eq!(usize::from(u8::from(delay)), raw);
            assert_eq!(TxDelay::try_from(u8::try_from(raw)?)?, delay);
        }
        Ok(())
    }

    #[test]
    fn tx_delay_rejects_non_menu_values() {
        assert!(TxDelay::new(0).is_err());
        assert!(TxDelay::new(250).is_err());
        assert!(TxDelay::new(1001).is_err());
        assert!(TxDelay::try_from(8).is_err());
    }

    #[test]
    fn tx_delay_factory_default_is_200ms() {
        let d = TxDelay::factory_default();
        assert_eq!(d, TxDelay::Ms200);
        assert_eq!(d.as_milliseconds(), 200);
        assert_eq!(d.as_raw(), 2);
    }

    #[test]
    fn beacon_control_uses_v103_defaults_and_discrete_intervals() -> TestResult {
        let control = BeaconControl::factory_default();
        assert_eq!(control.method, BeaconMode::Auto);
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
    fn smart_beaconing_factory_defaults_require_explicit_units() {
        let sb = StoredSmartBeaconingSettings::factory_default(SpeedDistanceUnit::MilesPerHour);
        assert_eq!(sb.speed_distance_unit, SpeedDistanceUnit::MilesPerHour);
        assert_eq!(sb.low_speed.as_configured_units(), 5);
        assert_eq!(sb.high_speed.as_configured_units(), 70);
        assert_eq!(sb.fast_rate.as_seconds(), 120);
        assert_eq!(sb.slow_rate.as_minutes(), 30);
        assert_eq!(sb.turn_angle.as_degrees(), 28);
        assert_eq!(sb.turn_slope.as_raw(), 26);
        assert_eq!(sb.turn_time.as_seconds(), 60);
        assert_eq!(
            StoredSmartBeaconingSettings::factory_default(SpeedDistanceUnit::KilometersPerHour)
                .speed_distance_unit,
            SpeedDistanceUnit::KilometersPerHour
        );
    }

    #[test]
    fn smart_beaconing_low_speed_enforces_stored_domain() -> TestResult {
        for raw in StoredLowSpeed::MIN..=StoredLowSpeed::MAX {
            let speed = StoredLowSpeed::try_from(raw)?;
            assert_eq!(speed.as_configured_units(), raw);
            assert_eq!(u8::from(speed), raw);
        }
        assert!(StoredLowSpeed::try_from(1).is_err());
        assert!(StoredLowSpeed::try_from(31).is_err());
        Ok(())
    }

    #[test]
    fn smart_beaconing_slow_rate_enforces_stored_minutes() -> TestResult {
        for raw in StoredSlowRateMinutes::MIN..=StoredSlowRateMinutes::MAX {
            let rate = StoredSlowRateMinutes::try_from(raw)?;
            assert_eq!(rate.as_minutes(), raw);
            assert_eq!(u8::from(rate), raw);
        }
        assert!(StoredSlowRateMinutes::try_from(0).is_err());
        assert!(StoredSlowRateMinutes::try_from(101).is_err());
        Ok(())
    }

    #[test]
    fn remaining_smart_beaconing_fields_enforce_stored_domains() -> TestResult {
        for raw in StoredHighSpeed::MIN..=StoredHighSpeed::MAX {
            let value = StoredHighSpeed::try_from(raw)?;
            assert_eq!(value.as_configured_units(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(StoredHighSpeed::try_from(1).is_err());
        assert!(StoredHighSpeed::try_from(91).is_err());

        for raw in StoredFastRateSeconds::MIN..=StoredFastRateSeconds::MAX {
            let value = StoredFastRateSeconds::try_from(raw)?;
            assert_eq!(value.as_seconds(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(StoredFastRateSeconds::try_from(9).is_err());
        assert!(StoredFastRateSeconds::try_from(181).is_err());

        for raw in StoredTurnAngleDegrees::MIN..=StoredTurnAngleDegrees::MAX {
            let value = StoredTurnAngleDegrees::try_from(raw)?;
            assert_eq!(value.as_degrees(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(StoredTurnAngleDegrees::try_from(4).is_err());
        assert!(StoredTurnAngleDegrees::try_from(91).is_err());

        for raw in StoredTurnSlope::MIN..=StoredTurnSlope::MAX {
            let value = StoredTurnSlope::try_from(raw)?;
            assert_eq!(value.as_raw(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(StoredTurnSlope::try_from(0).is_err());

        for raw in StoredTurnTimeSeconds::MIN..=StoredTurnTimeSeconds::MAX {
            let value = StoredTurnTimeSeconds::try_from(raw)?;
            assert_eq!(value.as_seconds(), raw);
            assert_eq!(u8::from(value), raw);
        }
        assert!(StoredTurnTimeSeconds::try_from(4).is_err());
        assert!(StoredTurnTimeSeconds::try_from(181).is_err());
        Ok(())
    }

    #[test]
    fn user_phrase_valid() -> TestResult {
        let phrase = UserPhrase::new("On my way")?;
        assert_eq!(phrase.as_str(), "On my way");
        Ok(())
    }

    #[test]
    fn user_phrase_too_long() {
        assert!(UserPhrase::new(&"x".repeat(33)).is_err());
    }

    #[test]
    fn user_phrase_accepts_exact_32_byte_slot() -> TestResult {
        let phrase = UserPhrase::new(&"x".repeat(32))?;
        assert_eq!(phrase.as_str().len(), 32);
        Ok(())
    }

    #[test]
    fn packet_filter_position_limit_round_trips_stored_domain() -> TestResult {
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
        let minimum = PacketFilterDistance::new(10)?;
        let maximum = PacketFilterDistance::new(2500)?;
        assert_eq!(minimum.as_raw(), 1);
        assert_eq!(maximum.as_raw(), 250);
        assert!(PacketFilterDistance::new(0).is_err());
        assert!(PacketFilterDistance::new(15).is_err());
        assert!(PacketFilterDistance::new(2510).is_err());
        Ok(())
    }

    #[test]
    fn position_ambiguity_round_trips_exact_stored_domain() -> TestResult {
        let values = [
            PositionAmbiguity::Full,
            PositionAmbiguity::Level1,
            PositionAmbiguity::Level2,
            PositionAmbiguity::Level3,
            PositionAmbiguity::Level4,
        ];
        for (raw, expected) in (0_u8..=4).zip(values) {
            assert_eq!(PositionAmbiguity::try_from(raw)?, expected);
            assert_eq!(u8::from(expected), raw);
        }
        assert!(PositionAmbiguity::try_from(5).is_err());
        Ok(())
    }

    #[test]
    fn reply_message_valid() -> TestResult {
        let rm = ReplyMessage::new("I am away")?;
        assert_eq!(rm.as_str(), "I am away");
        Ok(())
    }

    #[test]
    fn reply_message_too_long() {
        assert!(ReplyMessage::new(&"a".repeat(50)).is_ok());
        assert!(ReplyMessage::new(&"a".repeat(51)).is_err());
        assert!(ReplyMessage::new("bad\ntext").is_err());
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
        assert_eq!(
            AprsNetworkType::factory_default(),
            AprsNetworkType::AprsApk005
        );
        assert_eq!(AprsNetworkType::try_from(0)?, AprsNetworkType::AprsApk005);
        assert_eq!(AprsNetworkType::try_from(1)?, AprsNetworkType::Altnet);
        assert_eq!(u8::from(AprsNetworkType::AprsApk005), 0);
        assert_eq!(u8::from(AprsNetworkType::Altnet), 1);
        assert!(AprsNetworkType::try_from(2).is_err());

        let address = AltnetAddress::new("ALTNET")?;
        assert_eq!(address.as_str(), "ALTNET");
        assert!(AltnetAddress::new("TOO-LONG").is_err());
        Ok(())
    }

    #[test]
    fn digipeater_settings_match_menus_580_through_588() -> TestResult {
        assert_eq!(UiCheckSeconds::factory_default().as_seconds(), 28);
        assert_eq!(UiCheckSeconds::new(0)?.as_raw(), 0);
        assert_eq!(UiCheckSeconds::new(250)?.as_raw(), 250);
        assert!(UiCheckSeconds::new(251).is_err());

        let ui_digi_aliases = [
            UiDigiAlias::new("WIDE1-1")?,
            UiDigiAlias::new("WIDE2-1")?,
            UiDigiAlias::default(),
            UiDigiAlias::default(),
        ];
        let flood = FloodAlias::new("WIDE1")?;
        let trace = TraceAlias::new("TRACE")?;
        let settings = StoredDigipeaterSettings {
            enabled_functions: HashSet::from([
                DigipeaterFunction::UiDigipeat,
                DigipeaterFunction::UiTrace,
            ]),
            ui_check: UiCheckSeconds::factory_default(),
            ui_digi_aliases,
            ui_flood_alias: flood.clone(),
            ui_flood_substitution: UiFloodSubstitution::First,
            ui_trace_alias: trace.clone(),
        };
        assert!(
            !settings
                .enabled_functions
                .contains(&DigipeaterFunction::MyCallsign)
        );
        assert!(
            settings
                .enabled_functions
                .contains(&DigipeaterFunction::UiDigipeat)
        );
        assert!(
            settings
                .enabled_functions
                .contains(&DigipeaterFunction::UiTrace)
        );
        assert!(
            !settings
                .enabled_functions
                .contains(&DigipeaterFunction::UiFlood)
        );
        let [first_alias, second_alias, _, _] = &settings.ui_digi_aliases;
        assert_eq!(first_alias.as_str(), "WIDE1-1");
        assert_eq!(second_alias.as_str(), "WIDE2-1");
        assert_eq!(flood.as_str(), "WIDE1");
        assert_eq!(trace.as_str(), "TRACE");
        assert_eq!(TraceAlias::factory_default().as_str(), "TEMP");

        for invalid in ["BAD\n", "BAD\0", "é", "123456"] {
            assert!(FloodAlias::new(invalid).is_err());
            assert!(TraceAlias::new(invalid).is_err());
        }
        for invalid in ["lower", "BAD-1"] {
            assert!(FloodAlias::new(invalid).is_err());
        }
        for invalid in ["BAD\r", "BAD\u{7F}", "ネット", "1234567890", "WIDE1,WIDE2"] {
            assert!(UiDigiAlias::new(invalid).is_err());
        }

        assert_eq!(
            UiFloodSubstitution::factory_default(),
            UiFloodSubstitution::First
        );
        for (raw, expected) in [
            (0, UiFloodSubstitution::Id),
            (1, UiFloodSubstitution::NoId),
            (2, UiFloodSubstitution::First),
        ] {
            let substitution = UiFloodSubstitution::try_from(raw)?;
            assert_eq!(substitution, expected);
            assert_eq!(u8::from(substitution), raw);
        }
        assert!(UiFloodSubstitution::try_from(3).is_err());
        Ok(())
    }

    #[test]
    fn group_codes_enforce_count_length_and_character_domains() -> TestResult {
        let messages = MessageGroupCodes::from_csv("ALL,QST,CQ,KWD")?;
        assert_eq!(messages.as_slice().len(), 4);
        assert_eq!(messages.to_string(), "ALL,QST,CQ,KWD");
        assert!(MessageGroupCodes::from_csv("A,B,C,D,E,F,G").is_err());
        assert!(MessageGroupCodes::from_csv("lower").is_err());
        assert!(MessageGroupCode::new("123456789").is_ok());
        assert!(MessageGroupCode::new("1234567890").is_err());

        let bulletins = BulletinGroupCodes::from_csv("WX,ARES")?;
        assert_eq!(bulletins.to_string(), "WX,ARES");
        assert!(BulletinGroupCode::new("12345").is_ok());
        assert!(BulletinGroupCode::new("123456").is_err());
        assert!(BulletinGroupCode::new("WX*").is_err());
        Ok(())
    }

    #[test]
    fn menu_enum_domains_round_trip_exact_schema_values() -> TestResult {
        assert_eq!(AprsBand::factory_default(), AprsBand::A);
        assert_eq!(DcdSense::factory_default(), DcdSense::Busy);
        assert_eq!(StatusTextSlot::factory_default().as_raw(), 0);
        let voice_alert = VoiceAlertSettings::factory_default();
        assert_eq!(voice_alert.mode(), VoiceAlertMode::Off);
        assert_eq!(voice_alert.tone_code(), ToneCode::TONE_100HZ);

        for raw in 0..=1 {
            let band = AprsBand::try_from(raw)?;
            assert_eq!(u8::from(band), raw);
        }
        assert!(AprsBand::try_from(2).is_err());

        for raw in 0..=2 {
            let sense = DcdSense::try_from(raw)?;
            assert_eq!(u8::from(sense), raw);
            let format = WaypointFormat::try_from(raw)?;
            assert_eq!(u8::from(format), raw);
            let output = WaypointOutput::try_from(raw)?;
            assert_eq!(u8::from(output), raw);
            let display = DisplayArea::try_from(raw)?;
            assert_eq!(u8::from(display), raw);
            let voice_alert = VoiceAlertMode::try_from(raw)?;
            assert_eq!(u8::from(voice_alert), raw);
        }
        assert!(DcdSense::try_from(3).is_err());

        for raw in 6..=9 {
            let length = WaypointNameLength::try_from(raw)?;
            assert_eq!(u8::from(length), raw);
        }
        assert!(WaypointNameLength::try_from(5).is_err());
        assert!(WaypointNameLength::try_from(10).is_err());

        for raw in 0..=4 {
            let beep = RxBeep::try_from(raw)?;
            assert_eq!(u8::from(beep), raw);
            let delay = AutoReplyDelay::try_from(raw)?;
            assert_eq!(u8::from(delay), raw);
        }
        assert!(RxBeep::try_from(5).is_err());

        for raw in 0..=6 {
            let interrupt = InterruptTime::try_from(raw)?;
            assert_eq!(u8::from(interrupt), raw);
        }
        assert!(InterruptTime::try_from(7).is_err());
        Ok(())
    }

    #[test]
    fn packet_path_models_every_menu_shape_without_raw_invalid_values() -> TestResult {
        let default_path = PacketPath::factory_default();
        let PacketPath::NewN(new_n) = default_path else {
            return Err("default packet path was not New-N".into());
        };
        assert!(new_n.wide1_1());
        assert_eq!(new_n.total_hops().as_raw(), 2);

        assert_eq!(PacketPathHops::try_from(7)?.as_raw(), 7);
        assert!(PacketPathHops::try_from(8).is_err());
        assert!(PacketPathAbbreviation::new("ABCDE").is_ok());
        assert!(PacketPathAbbreviation::new("ABCDEF").is_err());
        assert!(Others1PacketPath::new(&"A".repeat(79)).is_ok());
        assert!(Others1PacketPath::new(&"A".repeat(80)).is_err());
        assert!(Others2PacketPath::new(&"A".repeat(29)).is_ok());
        assert!(Others2PacketPath::new(&"A".repeat(30)).is_err());
        assert!(Others3PacketPath::new("A\nB").is_err());
        Ok(())
    }

    #[test]
    fn waypoint_constructor_and_notification_neutral_default() {
        let waypoint = WaypointSettings::new(
            WaypointFormat::Nmea,
            WaypointNameLength::Characters6,
            WaypointOutput::All,
        );
        assert_eq!(waypoint.format(), WaypointFormat::Nmea);
        assert_eq!(waypoint.name_length(), WaypointNameLength::Characters6);
        assert_eq!(waypoint.output(), WaypointOutput::All);

        let notification = NotificationSettings::default();
        assert_eq!(notification.rx_beep(), RxBeep::Off);
        assert!(!notification.tx_beep());
        assert!(notification.special_call().is_none());
    }

    #[test]
    fn auto_reply_target_supports_exact_and_trailing_wildcard_forms() -> TestResult {
        assert_eq!(AutoReplyTarget::new("*")?.to_string(), "*");
        assert_eq!(AutoReplyTarget::new("N0CALL-7")?.to_string(), "N0CALL-7",);
        assert_eq!(AutoReplyTarget::new("JA1*")?.to_string(), "JA1*",);
        assert!(AutoReplyTarget::new("ja1*").is_err());
        assert!(AutoReplyTarget::new("N0*CALL").is_err());
        assert_eq!(
            AutoReplySettings::default().delay_time(),
            AutoReplyDelay::None
        );
        Ok(())
    }

    #[test]
    fn custom_icon_requires_radio_encodable_symbol_bytes() -> TestResult {
        let AprsIcon::Custom { table, code } = AprsIcon::custom('A', '!')? else {
            return Err("custom icon constructor returned predefined icon".into());
        };
        assert_eq!(table.as_char(), 'A');
        assert_eq!(code.as_char(), '!');
        assert!(AprsIcon::custom('a', '!').is_err());
        assert!(AprsIcon::custom('/', ' ').is_err());
        assert!(AprsIcon::custom('/', 'é').is_err());
        Ok(())
    }

    #[test]
    fn navitra_settings_have_five_bounded_messages_and_three_byte_group_code() -> TestResult {
        let settings = NavitraSettings::new(
            NavitraGroupMode::Off,
            NavitraGroupCode::default(),
            Default::default(),
            NavitraMessageSlot::new(0)?,
        );
        assert_eq!(settings.group_mode(), NavitraGroupMode::Off);
        assert_eq!(settings.group_code().as_str(), "");
        assert_eq!(settings.messages().len(), 5);
        assert_eq!(settings.active_message().as_raw(), 0);
        assert!(NavitraGroupCode::new("123").is_ok());
        assert!(NavitraGroupCode::new("1234").is_err());
        assert!(NavitraMessage::new(&"A".repeat(20)).is_ok());
        assert!(NavitraMessage::new(&"A".repeat(21)).is_err());
        assert!(NavitraMessageSlot::try_from(4).is_ok());
        assert!(NavitraMessageSlot::try_from(5).is_err());
        Ok(())
    }

    #[test]
    fn qsy_settings_defaults() -> TestResult {
        let qsy = QsySettings::default();
        assert!(!qsy.info_in_status());
        assert_eq!(qsy.limit_distance(), QsyLimitDistance::Off);
        assert_eq!(u8::from(QsyLimitDistance::try_from(250)?), 250);
        assert!(QsyLimitDistance::try_from(251).is_err());
        assert_eq!(QsyDistance::new(2500)?.as_raw(), 250,);
        assert!(QsyDistance::new(15).is_err());
        Ok(())
    }
}
