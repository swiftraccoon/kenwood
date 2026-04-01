//! APRS (Automatic Packet Reporting System) configuration types.
//!
//! APRS is a tactical real-time digital communications protocol used by ham
//! radio operators for position reporting, messaging, and telemetry. The
//! TH-D75 supports APRS on VHF with features including position beaconing,
//! two-way messaging, `SmartBeaconing`, digipeater path configuration,
//! packet filtering, and QSY information exchange.
//!
//! These types model every APRS setting accessible through the TH-D75's
//! menu system (Chapter 14 of the user manual) and MCP programming memory
//! (pages 0x0151+ in the memory map).

use crate::types::tone::ToneCode;

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
    /// TX delay before packet transmission (in 10 ms units, range 1-50,
    /// representing 10-500 ms).
    pub tx_delay: TxDelay,
    /// Beacon transmission control settings.
    pub beacon_control: BeaconControl,
    /// `SmartBeaconing` configuration (speed-adaptive beaconing).
    pub smart_beaconing: SmartBeaconingConfig,
    /// APRS lock (prevent accidental APRS setting changes).
    pub aprs_lock: bool,
    /// Position ambiguity level (0 = full precision, 1-4 = progressively
    /// less precise, each level removes one decimal digit).
    pub position_ambiguity: PositionAmbiguity,
    /// Waypoint output configuration.
    pub waypoint: WaypointConfig,
    /// Packet filter settings.
    pub packet_filter: PacketFilter,
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
            smart_beaconing: SmartBeaconingConfig::default(),
            aprs_lock: false,
            position_ambiguity: PositionAmbiguity::Full,
            waypoint: WaypointConfig::default(),
            packet_filter: PacketFilter::default(),
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
    /// Returns `None` if the callsign exceeds 9 characters.
    #[must_use]
    pub fn new(callsign: &str) -> Option<Self> {
        if callsign.len() <= Self::MAX_LEN {
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
    /// Custom icon specified by raw table and code characters.
    Custom {
        /// Symbol table identifier (`/` = primary, `\` = alternate,
        /// or overlay character `0`-`9`, `A`-`Z`).
        table: char,
        /// Symbol code character (ASCII 0x21-0x7E).
        code: char,
    },
}

impl Default for AprsIcon {
    fn default() -> Self {
        Self::Portable
    }
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
/// Delay is specified in 10 ms increments. The valid range is 100-500 ms
/// (values 1-50 in 10 ms units). Default is 300 ms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxDelay(u8);

impl TxDelay {
    /// Minimum TX delay value (10 ms units), representing 100 ms.
    pub const MIN: u8 = 1;
    /// Maximum TX delay value (10 ms units), representing 500 ms.
    pub const MAX: u8 = 50;

    /// Creates a new TX delay value.
    ///
    /// # Errors
    ///
    /// Returns `None` if the value is outside the range 1-50.
    #[must_use]
    pub const fn new(units_10ms: u8) -> Option<Self> {
        if units_10ms >= Self::MIN && units_10ms <= Self::MAX {
            Some(Self(units_10ms))
        } else {
            None
        }
    }

    /// Returns the delay in 10 ms units.
    #[must_use]
    pub const fn as_units(self) -> u8 {
        self.0
    }

    /// Returns the delay in milliseconds.
    #[must_use]
    pub const fn as_ms(self) -> u16 {
        self.0 as u16 * 10
    }
}

impl Default for TxDelay {
    fn default() -> Self {
        // Default TX delay: 300 ms = 30 units of 10 ms.
        Self(30)
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
    /// Initial beacon interval in seconds (range 30-9999).
    pub initial_interval: u16,
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
            method: BeaconMethod::Manual,
            initial_interval: 180,
            decay: false,
            proportional_pathing: false,
        }
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
pub struct SmartBeaconingConfig {
    /// Low speed threshold in mph (range 1-30). Below this speed,
    /// beacons are sent at `slow_rate`.
    pub low_speed: u8,
    /// High speed threshold in mph (range 2-90). At or above this speed,
    /// beacons are sent at `fast_rate`.
    pub high_speed: u8,
    /// Slow beacon rate in seconds (range 1-100 minutes, stored as seconds).
    pub slow_rate: u16,
    /// Fast beacon rate in seconds (range 10-180 seconds).
    pub fast_rate: u8,
    /// Minimum course change in degrees to trigger a beacon (range 5-90).
    pub turn_angle: u8,
    /// Turn slope factor (range 1-255). Higher values require more speed
    /// before a turn triggers a beacon.
    pub turn_slope: u8,
    /// Minimum time between turn-triggered beacons in seconds (range 5-180).
    pub turn_time: u8,
}

impl Default for SmartBeaconingConfig {
    fn default() -> Self {
        Self {
            low_speed: 5,
            high_speed: 60,
            slow_rate: 1800,
            fast_rate: 60,
            turn_angle: 28,
            turn_slope: 26,
            turn_time: 30,
        }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketPath {
    /// Off (no digipeater path).
    Off,
    /// WIDE1-1 (one hop via fill-in digipeaters).
    Wide1_1,
    /// WIDE1-1,WIDE2-1 (standard two-hop path).
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

impl Default for PacketPath {
    fn default() -> Self {
        Self::Wide1_1Wide2_1
    }
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
    /// Enable position limit filter (only show stations within a
    /// certain distance).
    pub position_limit: bool,
    /// Packet filter type selection.
    pub filter_type: PacketFilterType,
    /// User-defined filter phrases (up to 3 phrases, each up to 9 characters).
    pub user_phrases: [FilterPhrase; 3],
}

impl Default for PacketFilter {
    fn default() -> Self {
        Self {
            position_limit: false,
            filter_type: PacketFilterType::All,
            user_phrases: Default::default(),
        }
    }
}

/// Packet filter type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PacketFilterType {
    /// Accept all packet types.
    All,
    /// Position packets only.
    Position,
    /// Weather packets only.
    Weather,
    /// Message packets only (directed to this station).
    Message,
    /// Other packet types.
    Other,
}

/// User-defined APRS filter phrase (up to 9 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FilterPhrase(String);

impl FilterPhrase {
    /// Maximum length of a filter phrase.
    pub const MAX_LEN: usize = 9;

    /// Creates a new filter phrase.
    ///
    /// # Errors
    ///
    /// Returns `None` if the phrase exceeds 9 characters.
    #[must_use]
    pub fn new(phrase: &str) -> Option<Self> {
        if phrase.len() <= Self::MAX_LEN {
            Some(Self(phrase.to_owned()))
        } else {
            None
        }
    }

    /// Returns the filter phrase as a string slice.
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
            // SAFETY: 12 is within valid range 0-49.
            tone_code: ToneCode::new(12).expect("default tone code 12 is valid"),
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

/// APRS network identifier.
///
/// Selects the APRS-IS network for internet gateway connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsNetwork {
    /// APRS standard network (e.g. 144.390 MHz in North America).
    Aprs,
    /// NAVITRA network (Japanese navigation/tracking system).
    Navitra,
}

impl Default for AprsNetwork {
    fn default() -> Self {
        Self::Aprs
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

    #[test]
    fn aprs_callsign_valid() {
        let cs = AprsCallsign::new("N0CALL-9").unwrap();
        assert_eq!(cs.as_str(), "N0CALL-9");
    }

    #[test]
    fn aprs_callsign_max_length() {
        let cs = AprsCallsign::new("N0CALL-15").unwrap();
        assert_eq!(cs.as_str(), "N0CALL-15");
    }

    #[test]
    fn aprs_callsign_too_long() {
        assert!(AprsCallsign::new("N0CALL-150").is_none());
    }

    #[test]
    fn status_text_valid() {
        let st = StatusText::new("Testing 1 2 3").unwrap();
        assert_eq!(st.as_str(), "Testing 1 2 3");
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
    fn tx_delay_valid_range() {
        assert!(TxDelay::new(1).is_some());
        assert!(TxDelay::new(30).is_some());
        assert!(TxDelay::new(50).is_some());
    }

    #[test]
    fn tx_delay_invalid() {
        assert!(TxDelay::new(0).is_none());
        assert!(TxDelay::new(51).is_none());
    }

    #[test]
    fn tx_delay_default_300ms() {
        let d = TxDelay::default();
        assert_eq!(d.as_ms(), 300);
        assert_eq!(d.as_units(), 30);
    }

    #[test]
    fn smart_beaconing_defaults() {
        let sb = SmartBeaconingConfig::default();
        assert_eq!(sb.low_speed, 5);
        assert_eq!(sb.high_speed, 60);
        assert_eq!(sb.fast_rate, 60);
        assert_eq!(sb.slow_rate, 1800);
        assert_eq!(sb.turn_angle, 28);
    }

    #[test]
    fn filter_phrase_valid() {
        let fp = FilterPhrase::new("N0CALL").unwrap();
        assert_eq!(fp.as_str(), "N0CALL");
    }

    #[test]
    fn filter_phrase_too_long() {
        assert!(FilterPhrase::new("0123456789").is_none());
    }

    #[test]
    fn reply_message_valid() {
        let rm = ReplyMessage::new("I am away").unwrap();
        assert_eq!(rm.as_str(), "I am away");
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
        assert!(!cfg.aprs_lock);
    }

    #[test]
    fn group_code_valid() {
        let gc = GroupCode::new("ARES").unwrap();
        assert_eq!(gc.as_str(), "ARES");
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
