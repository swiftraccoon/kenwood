//! D-STAR (Digital Smart Technologies for Amateur Radio) configuration types.
//!
//! D-STAR is a digital voice and data protocol for amateur radio developed
//! by JARL (Japan Amateur Radio League). The TH-D75 supports DV (Digital
//! Voice) mode with features including reflector linking, callsign routing,
//! gateway access, and DR (D-STAR Repeater) mode for simplified operation.
//!
//! These types model every D-STAR setting accessible through the TH-D75's
//! menu system (Chapter 16 of the user manual) and MCP programming memory
//! (pages 0x02A1+ in the memory map, plus system settings at 0x03F0).

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Top-level D-STAR configuration
// ---------------------------------------------------------------------------

/// Complete D-STAR configuration for the TH-D75.
///
/// Covers all settings from the radio's D-STAR menu tree, including
/// callsign configuration, repeater routing, digital squelch, auto-reply,
/// and data options. Derived from the capability gap analysis features 40-62.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstarConfig {
    /// MY callsign (up to 8 characters). This is the station's own
    /// callsign transmitted in every D-STAR frame header.
    pub my_callsign: DstarCallsign,
    /// MY callsign extension / suffix (up to 4 characters).
    /// Used for additional station identification (e.g. "/P" for portable).
    pub my_suffix: DstarSuffix,
    /// UR callsign (8 characters). The destination callsign.
    /// "CQCQCQ" for general CQ calls, a specific callsign for
    /// callsign routing, or a reflector command.
    pub ur_call: DstarCallsign,
    /// RPT1 callsign (8 characters). The access repeater (local).
    pub rpt1: DstarCallsign,
    /// RPT2 callsign (8 characters). The gateway/linked repeater.
    pub rpt2: DstarCallsign,
    /// DV/DR mode selection.
    pub dv_mode: DvDrMode,
    /// Digital squelch configuration.
    pub digital_squelch: DigitalSquelch,
    /// Auto-reply configuration for D-STAR messages.
    pub auto_reply: DstarAutoReply,
    /// RX AFC (Automatic Frequency Control) for DV mode.
    /// Compensates for frequency drift on received signals.
    pub rx_afc: bool,
    /// Automatically detect FM signals when in DV mode.
    /// Allows receiving analog FM on a DV-mode channel.
    pub fm_auto_detect_on_dv: bool,
    /// Output D-STAR data frames to the serial port.
    pub data_frame_output: bool,
    /// Include GPS position information in DV frame headers.
    pub gps_info_in_frame: bool,
    /// Standby beep when a DV transmission ends.
    pub standby_beep: bool,
    /// Enable break-in call (interrupt an ongoing QSO).
    pub break_call: bool,
    /// Voice announcement of received callsigns.
    pub callsign_announce: bool,
    /// EMR (Emergency) volume level (0-9, 0 = off).
    pub emr_volume: EmrVolume,
    /// Gateway mode for DV operation.
    pub gateway_mode: GatewayMode,
    /// Enable fast data mode (high-speed DV data).
    pub fast_data: bool,
}

impl Default for DstarConfig {
    fn default() -> Self {
        Self {
            my_callsign: DstarCallsign::default(),
            my_suffix: DstarSuffix::default(),
            ur_call: DstarCallsign::cqcqcq(),
            rpt1: DstarCallsign::default(),
            rpt2: DstarCallsign::default(),
            dv_mode: DvDrMode::Dv,
            digital_squelch: DigitalSquelch::default(),
            auto_reply: DstarAutoReply::default(),
            rx_afc: false,
            fm_auto_detect_on_dv: false,
            data_frame_output: false,
            gps_info_in_frame: false,
            standby_beep: true,
            break_call: false,
            callsign_announce: false,
            emr_volume: EmrVolume::default(),
            gateway_mode: GatewayMode::Auto,
            fast_data: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Callsign types
// ---------------------------------------------------------------------------

/// D-STAR callsign (up to 8 characters, space-padded).
///
/// D-STAR callsigns are always exactly 8 characters in the protocol,
/// right-padded with spaces. This type stores the trimmed form and
/// provides padding methods for wire encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DstarCallsign(String);

impl DstarCallsign {
    /// Maximum length of a D-STAR callsign.
    pub const MAX_LEN: usize = 8;

    /// Wire-format width (always 8 characters, space-padded).
    pub const WIRE_LEN: usize = 8;

    /// Creates a new D-STAR callsign.
    ///
    /// # Errors
    ///
    /// Returns `None` if the callsign exceeds 8 characters.
    #[must_use]
    pub fn new(callsign: &str) -> Option<Self> {
        let trimmed = callsign.trim_end();
        if trimmed.len() <= Self::MAX_LEN {
            Some(Self(trimmed.to_owned()))
        } else {
            None
        }
    }

    /// Creates the broadcast CQ callsign ("CQCQCQ").
    #[must_use]
    pub fn cqcqcq() -> Self {
        Self("CQCQCQ".to_owned())
    }

    /// Returns the callsign as a trimmed string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the callsign as an 8-byte space-padded ASCII array
    /// for wire encoding.
    #[must_use]
    pub fn to_wire_bytes(&self) -> [u8; 8] {
        let mut buf = [b' '; 8];
        let src = self.0.as_bytes();
        let len = src.len().min(8);
        buf[..len].copy_from_slice(&src[..len]);
        buf
    }

    /// Decodes a D-STAR callsign from an 8-byte space-padded array.
    #[must_use]
    pub fn from_wire_bytes(bytes: &[u8; 8]) -> Self {
        let s = std::str::from_utf8(bytes)
            .unwrap_or("")
            .trim_end();
        Self(s.to_owned())
    }

    /// Returns `true` if this is the broadcast CQ callsign.
    #[must_use]
    pub fn is_cqcqcq(&self) -> bool {
        self.0 == "CQCQCQ"
    }

    /// Returns `true` if the callsign is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// D-STAR MY callsign suffix (up to 4 characters).
///
/// The suffix is appended to the MY callsign in the D-STAR frame header
/// as additional identification (e.g. "/P" for portable, "/M" for mobile).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DstarSuffix(String);

impl DstarSuffix {
    /// Maximum length of a D-STAR callsign suffix.
    pub const MAX_LEN: usize = 4;

    /// Creates a new D-STAR callsign suffix.
    ///
    /// # Errors
    ///
    /// Returns `None` if the suffix exceeds 4 characters.
    #[must_use]
    pub fn new(suffix: &str) -> Option<Self> {
        if suffix.len() <= Self::MAX_LEN {
            Some(Self(suffix.to_owned()))
        } else {
            None
        }
    }

    /// Returns the suffix as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Mode selection
// ---------------------------------------------------------------------------

/// DV/DR mode selection.
///
/// DV mode provides manual repeater configuration; DR mode simplifies
/// operation with automatic repeater selection from the repeater list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DvDrMode {
    /// DV (Digital Voice) mode -- manual repeater configuration.
    Dv,
    /// DR (D-STAR Repeater) mode -- automatic repeater selection.
    Dr,
}

// ---------------------------------------------------------------------------
// Digital squelch
// ---------------------------------------------------------------------------

/// Validated D-STAR digital squelch code (0-99).
///
/// The TH-D75 uses a numeric code in the range 0-99 for digital code
/// squelch on D-STAR. Only frames with a matching code open the audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DigitalSquelchCode(u8);

impl DigitalSquelchCode {
    /// Creates a new digital squelch code.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::DigitalSquelchCodeOutOfRange`] if `code > 99`.
    pub const fn new(code: u8) -> Result<Self, ValidationError> {
        if code <= 99 {
            Ok(Self(code))
        } else {
            Err(ValidationError::DigitalSquelchCodeOutOfRange(code))
        }
    }

    /// Returns the raw code value (0-99).
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

impl std::fmt::Display for DigitalSquelchCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02}", self.0)
    }
}

/// Digital squelch configuration.
///
/// Digital squelch opens the audio only when the received D-STAR frame
/// header matches specific criteria: a digital code (0-99) or a specific
/// callsign.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DigitalSquelch {
    /// Digital squelch mode.
    pub squelch_type: DigitalSquelchType,
    /// Digital code for code squelch mode (0-99).
    pub code: DigitalSquelchCode,
    /// Callsign for callsign squelch mode.
    pub callsign: DstarCallsign,
}

impl Default for DigitalSquelch {
    fn default() -> Self {
        Self {
            squelch_type: DigitalSquelchType::Off,
            code: DigitalSquelchCode::default(),
            callsign: DstarCallsign::default(),
        }
    }
}

/// Digital squelch type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigitalSquelchType {
    /// Digital squelch disabled -- receive all DV signals.
    Off,
    /// Code squelch -- open audio only when the digital code matches.
    CodeSquelch,
    /// Callsign squelch -- open audio only when the source callsign matches.
    CallsignSquelch,
}

impl TryFrom<u8> for DigitalSquelchType {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::CodeSquelch),
            2 => Ok(Self::CallsignSquelch),
            _ => Err("digital squelch type out of range (must be 0-2)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-reply
// ---------------------------------------------------------------------------

/// D-STAR auto-reply configuration.
///
/// When enabled, the radio automatically responds to incoming D-STAR
/// slow-data messages with a configured text reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DstarAutoReply {
    /// Auto-reply mode.
    pub mode: DstarAutoReplyMode,
    /// Auto-reply message text (up to 20 characters).
    pub message: DstarMessage,
}

impl Default for DstarAutoReply {
    fn default() -> Self {
        Self {
            mode: DstarAutoReplyMode::Off,
            message: DstarMessage::default(),
        }
    }
}

/// D-STAR auto-reply mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DstarAutoReplyMode {
    /// Auto-reply disabled.
    Off,
    /// Reply with the configured message text.
    Reply,
    /// Reply with the current GPS position.
    Position,
    /// Reply with both message text and GPS position.
    Both,
}

impl TryFrom<u8> for DstarAutoReplyMode {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Reply),
            2 => Ok(Self::Position),
            3 => Ok(Self::Both),
            _ => Err("D-STAR auto reply mode out of range (must be 0-3)"),
        }
    }
}

impl TryFrom<u8> for GatewayMode {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Auto),
            1 => Ok(Self::Manual),
            _ => Err("gateway mode out of range (must be 0-1)"),
        }
    }
}

/// D-STAR slow-data message text (up to 20 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct DstarMessage(String);

impl DstarMessage {
    /// Maximum length of a D-STAR message.
    pub const MAX_LEN: usize = 20;

    /// Creates a new D-STAR message.
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

    /// Returns the message as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Gateway and EMR
// ---------------------------------------------------------------------------

/// D-STAR gateway mode.
///
/// Controls how the radio selects the gateway repeater for callsign
/// routing via the D-STAR network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GatewayMode {
    /// Automatic gateway selection based on the repeater list.
    Auto,
    /// Manual gateway configuration (user sets RPT2 directly).
    Manual,
}

/// EMR (Emergency) volume level (0-9).
///
/// When EMR mode is activated by the remote station, the radio increases
/// volume to the configured EMR level. 0 disables EMR volume override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmrVolume(u8);

#[allow(clippy::derivable_impls)]
impl Default for EmrVolume {
    fn default() -> Self {
        Self(0)
    }
}

impl EmrVolume {
    /// Maximum EMR volume level.
    pub const MAX: u8 = 9;

    /// Creates a new EMR volume level.
    ///
    /// # Errors
    ///
    /// Returns `None` if the value exceeds 9.
    #[must_use]
    pub const fn new(level: u8) -> Option<Self> {
        if level <= Self::MAX {
            Some(Self(level))
        } else {
            None
        }
    }

    /// Returns the EMR volume level.
    #[must_use]
    pub const fn level(self) -> u8 {
        self.0
    }
}

// ---------------------------------------------------------------------------
// Repeater list entry
// ---------------------------------------------------------------------------

/// D-STAR repeater list entry.
///
/// Stored in MCP memory at pages 0x02A1+ as 108-byte records, and
/// importable/exportable via TSV files on the SD card at
/// `/KENWOOD/TH-D75/SETTINGS/RPT_LIST/`.
///
/// The TH-D75 supports up to 1500 repeater entries.
#[derive(Debug, Clone, PartialEq)]
pub struct RepeaterEntry {
    /// Group name / region (up to 16 characters).
    pub group_name: String,
    /// Repeater name / description (up to 16 characters).
    pub name: String,
    /// Sub-name / area description (up to 16 characters).
    pub sub_name: String,
    /// RPT1 callsign (access repeater, 8-character D-STAR format).
    pub callsign_rpt1: DstarCallsign,
    /// RPT2 / gateway callsign (8-character D-STAR format).
    pub gateway_rpt2: DstarCallsign,
    /// Operating frequency in Hz.
    pub frequency: u32,
    /// Duplex direction.
    pub duplex: RepeaterDuplex,
    /// TX offset frequency in Hz.
    pub offset: u32,
    /// D-STAR module letter (A = 23 cm, B = 70 cm, C = 2 m).
    pub module: DstarModule,
    /// Repeater latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Repeater longitude in decimal degrees (positive = East).
    pub longitude: f64,
    /// UTC offset / time zone string (e.g. "+09:00").
    pub utc_offset: String,
    /// Position accuracy indicator.
    pub position_accuracy: PositionAccuracy,
    /// Lockout this repeater from DR scan.
    pub lockout: bool,
}

/// Repeater duplex direction (from TSV "Dup" column).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RepeaterDuplex {
    /// Simplex (no shift).
    Simplex,
    /// Positive shift.
    Plus,
    /// Negative shift.
    Minus,
}

/// D-STAR module letter.
///
/// Each D-STAR repeater has up to 3 RF modules and 1 gateway module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DstarModule {
    /// Module A (1.2 GHz / 23 cm band).
    A,
    /// Module B (430 MHz / 70 cm band).
    B,
    /// Module C (144 MHz / 2 m band).
    C,
    /// Gateway module (internet linking).
    G,
}

/// Position accuracy for repeater list entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionAccuracy {
    /// Position data is invalid or not available.
    Invalid,
    /// Position is approximate (city-level).
    Approximate,
    /// Position is exact (surveyed coordinates).
    Exact,
}

// ---------------------------------------------------------------------------
// Hotspot entry
// ---------------------------------------------------------------------------

/// D-STAR hotspot list entry.
///
/// The TH-D75 supports up to 30 hotspot entries for personal D-STAR
/// access points (e.g. DVAP, `DV4mini`, MMDVM).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotEntry {
    /// Hotspot name (up to 16 characters).
    pub name: String,
    /// Sub-name / description (up to 16 characters).
    pub sub_name: String,
    /// RPT1 callsign (8-character D-STAR format).
    pub callsign_rpt1: DstarCallsign,
    /// Gateway / RPT2 callsign (8-character D-STAR format).
    pub gateway_rpt2: DstarCallsign,
    /// Operating frequency in Hz.
    pub frequency: u32,
    /// Lockout this hotspot from scanning.
    pub lockout: bool,
}

// ---------------------------------------------------------------------------
// Callsign list entry
// ---------------------------------------------------------------------------

/// D-STAR callsign list entry (URCALL memory).
///
/// Stored on the SD card at `/KENWOOD/TH-D75/SETTINGS/CALLSIGN_LIST/`
/// and in MCP memory as part of the repeater/callsign region.
/// The TH-D75 supports up to 120 callsign entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallsignEntry {
    /// D-STAR destination callsign (8 characters, space-padded).
    pub callsign: DstarCallsign,
}

// ---------------------------------------------------------------------------
// Reflector operations
// ---------------------------------------------------------------------------

/// D-STAR reflector operation command.
///
/// Reflector operations are performed by setting specific URCALL values.
/// The TH-D75 provides dedicated menu items for these operations.
/// Handler at firmware address `0xC005D460`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReflectorCommand {
    /// Link to a reflector module.
    Link,
    /// Unlink from the current reflector.
    Unlink,
    /// Echo test (transmit and receive back your own audio).
    Echo,
    /// Request reflector status information.
    Info,
    /// Use the currently linked reflector.
    Use,
}

// ---------------------------------------------------------------------------
// Destination / route select
// ---------------------------------------------------------------------------

/// D-STAR destination selection method.
///
/// In DR mode, the radio can select destinations from multiple sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationSelect {
    /// Select from the repeater list.
    RepeaterList,
    /// Select from the callsign list.
    CallsignList,
    /// Select from TX/RX history.
    History,
    /// Direct callsign input.
    DirectInput,
}

/// D-STAR route selection for gateway linking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RouteSelect {
    /// Automatic route selection via the gateway.
    Auto,
    /// Use a specific repeater as the gateway destination.
    Specified,
}

// ---------------------------------------------------------------------------
// QSO log entry (D-STAR specific fields)
// ---------------------------------------------------------------------------

/// D-STAR QSO log entry.
///
/// Extends the generic QSO log with D-STAR-specific fields from the
/// 24-column TSV format stored on the SD card at
/// `/KENWOOD/TH-D75/QSO_LOG/`.
#[derive(Debug, Clone, PartialEq)]
pub struct DstarQsoEntry {
    /// TX or RX direction.
    pub direction: QsoDirection,
    /// Source callsign (MYCALL).
    pub caller: DstarCallsign,
    /// Destination callsign (URCALL).
    pub called: DstarCallsign,
    /// RPT1 callsign (link source repeater).
    pub rpt1: DstarCallsign,
    /// RPT2 callsign (link destination repeater).
    pub rpt2: DstarCallsign,
    /// D-STAR slow-data message content.
    pub message: String,
    /// Break-in flag.
    pub break_in: bool,
    /// EMR (emergency) flag.
    pub emr: bool,
    /// Fast data flag.
    pub fast_data: bool,
    /// Remote station latitude (from D-STAR GPS data).
    pub remote_latitude: Option<f64>,
    /// Remote station longitude (from D-STAR GPS data).
    pub remote_longitude: Option<f64>,
    /// Remote station altitude in meters.
    pub remote_altitude: Option<f64>,
    /// Remote station course in degrees.
    pub remote_course: Option<f64>,
    /// Remote station speed in km/h.
    pub remote_speed: Option<f64>,
}

/// QSO log direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QsoDirection {
    /// Transmitted.
    Tx,
    /// Received.
    Rx,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dstar_callsign_valid() {
        let cs = DstarCallsign::new("N0CALL").unwrap();
        assert_eq!(cs.as_str(), "N0CALL");
    }

    #[test]
    fn dstar_callsign_max_length() {
        let cs = DstarCallsign::new("JR6YPR A").unwrap();
        assert_eq!(cs.as_str(), "JR6YPR A");
    }

    #[test]
    fn dstar_callsign_too_long() {
        assert!(DstarCallsign::new("123456789").is_none());
    }

    #[test]
    fn dstar_callsign_trims_trailing_spaces() {
        let cs = DstarCallsign::new("N0CALL  ").unwrap();
        assert_eq!(cs.as_str(), "N0CALL");
    }

    #[test]
    fn dstar_callsign_wire_bytes_padded() {
        let cs = DstarCallsign::new("N0CALL").unwrap();
        let bytes = cs.to_wire_bytes();
        assert_eq!(&bytes, b"N0CALL  ");
    }

    #[test]
    fn dstar_callsign_from_wire_bytes() {
        let bytes = *b"JR6YPR B";
        let cs = DstarCallsign::from_wire_bytes(&bytes);
        assert_eq!(cs.as_str(), "JR6YPR B");
    }

    #[test]
    fn dstar_callsign_cqcqcq() {
        let cs = DstarCallsign::cqcqcq();
        assert!(cs.is_cqcqcq());
        assert_eq!(cs.as_str(), "CQCQCQ");
    }

    #[test]
    fn dstar_suffix_valid() {
        let s = DstarSuffix::new("/P").unwrap();
        assert_eq!(s.as_str(), "/P");
    }

    #[test]
    fn dstar_suffix_too_long() {
        assert!(DstarSuffix::new("12345").is_none());
    }

    #[test]
    fn emr_volume_valid_range() {
        for i in 0u8..=9 {
            assert!(EmrVolume::new(i).is_some());
        }
    }

    #[test]
    fn emr_volume_invalid() {
        assert!(EmrVolume::new(10).is_none());
    }

    #[test]
    fn dstar_message_valid() {
        let msg = DstarMessage::new("Hello D-STAR").unwrap();
        assert_eq!(msg.as_str(), "Hello D-STAR");
    }

    #[test]
    fn dstar_message_too_long() {
        let text = "a".repeat(21);
        assert!(DstarMessage::new(&text).is_none());
    }

    #[test]
    fn dstar_config_default() {
        let cfg = DstarConfig::default();
        assert!(cfg.ur_call.is_cqcqcq());
        assert_eq!(cfg.dv_mode, DvDrMode::Dv);
        assert!(cfg.standby_beep);
        assert!(!cfg.break_call);
    }

    #[test]
    fn digital_squelch_default() {
        let sq = DigitalSquelch::default();
        assert_eq!(sq.squelch_type, DigitalSquelchType::Off);
        assert_eq!(sq.code.value(), 0);
    }
}
