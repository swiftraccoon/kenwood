//! Typed access to the system settings region of the memory image.
//!
//! The system settings occupy bytes `0x0000`-`0x1FFF` (32 pages, 8,192
//! bytes). This region stores the radio's global configuration including
//! VFO state, squelch levels, display settings, audio settings, and more.
//!
//! # Known offsets
//!
//! | Region | Offset | Confidence |
//! |--------|--------|------------|
//! | Power-on message | `0x11C0` | D74 dev notes |
//! | Model name | `0x11D0` | D74 dev notes |
//! | Callsign data | `0x1300` | D74 dev notes |
//! | Power level A | `0x0359` | Hardware verified |
//! | Attenuator A | `0x035C` | Hardware verified |
//! | Dual band | `0x0396` | Hardware verified |
//! | VOX enable | `0x101B` | Hardware verified |
//! | VOX gain | `0x101C` | Hardware verified |
//! | Lock | `0x1060` | Hardware verified |
//! | Key beep | `0x1071` | Hardware verified |
//! | Bluetooth | `0x1078` | Hardware verified |
//! | Squelch A | `0x100D` | Firmware analysis |
//! | Squelch B | `0x100E` | Firmware analysis |
//! | Language | `0x1006` | Firmware analysis |
//! | Beep volume | `0x1072` | Firmware analysis |
//! | Backlight control | `0x1069` | Firmware analysis |
//! | Auto power off | `0x10D0` | Firmware analysis |
//! | Battery saver | `0x10C0` | Firmware analysis |
//! | Key lock type | `0x1061` | Firmware analysis |
//! | VOX delay | `0x101D` | Firmware analysis |

use crate::protocol::programming;
use crate::types::settings::{
    AltitudeRainUnit, AutoPowerOff, DisplayUnits, KeyLockType, Language, SpeedDistanceUnit,
    TemperatureUnit,
};
use crate::types::{Frequency, MemoryMode, PowerLevel};

// ---------------------------------------------------------------------------
// System settings region (0x0000 - 0x1FFF)
// ---------------------------------------------------------------------------

/// Byte offset of the system settings region.
const SETTINGS_OFFSET: usize = 0x0000;

/// Size of the system settings region in bytes.
const SETTINGS_SIZE: usize = (programming::SETTINGS_END as usize + 1
    - programming::SETTINGS_START as usize)
    * programming::PAGE_SIZE;

/// Byte offset of the power-on message (16 bytes, null-terminated ASCII).
const POWER_ON_MESSAGE_OFFSET: usize = 0x11C0;

/// Size of the power-on message field.
const POWER_ON_MESSAGE_SIZE: usize = 16;

/// Byte offset of the internal model name (16 bytes, null-terminated ASCII).
const MODEL_NAME_OFFSET: usize = 0x11D0;

/// Size of the model name field.
const MODEL_NAME_SIZE: usize = 16;

/// Byte offset of callsign data.
const CALLSIGN_OFFSET: usize = 0x1300;

// ---------------------------------------------------------------------------
// Hardware-verified settings offsets
//
// Each of these offsets was confirmed on a real TH-D75 by toggling the
// setting individually and identifying the changed byte in the MCP image.
// ---------------------------------------------------------------------------

/// Hardware-verified offset for Band A power level (1 byte, 0=Hi, 1=Mid, 2=Lo, 3=EL).
const POWER_LEVEL_A_OFFSET: usize = 0x0359;

/// Hardware-verified offset for Band A attenuator on/off (1 byte, 0=off, 1=on).
const ATTENUATOR_A_OFFSET: usize = 0x035C;

/// Hardware-verified offset for dual-band display (1 byte, 0=single, 1=dual).
const DUAL_BAND_OFFSET: usize = 0x0396;

/// Hardware-verified offset for VOX enabled (1 byte, 0=off, 1=on).
const VOX_ENABLED_OFFSET: usize = 0x101B;

/// Hardware-verified offset for VOX gain (1 byte, range 0-9).
const VOX_GAIN_OFFSET: usize = 0x101C;

/// Hardware-verified offset for lock on/off (1 byte, 0=unlocked, 1=locked).
const LOCK_OFFSET: usize = 0x1060;

/// Hardware-verified offset for key beep on/off (1 byte, 0=off, 1=on).
const KEY_BEEP_OFFSET: usize = 0x1071;

/// Hardware-verified offset for Bluetooth on/off (1 byte, 0=off, 1=on).
const BLUETOOTH_OFFSET: usize = 0x1078;

// ---------------------------------------------------------------------------
// Settings offsets from firmware analysis
// ---------------------------------------------------------------------------

// --- RX Settings ---
/// Band A squelch level (1 byte, range 0-6).
const SQUELCH_A_OFFSET: usize = 0x100D;
/// Band B squelch level (1 byte, range 0-6).
const SQUELCH_B_OFFSET: usize = 0x100E;
/// FM narrow setting (1 byte).
const FM_NARROW_OFFSET: usize = 0x100F;
/// SSB high cut filter (1 byte).
const SSB_HIGH_CUT_OFFSET: usize = 0x1011;
/// CW high cut filter (1 byte).
const CW_HIGH_CUT_OFFSET: usize = 0x1012;
/// AM high cut filter (1 byte).
const AM_HIGH_CUT_OFFSET: usize = 0x1013;
/// Auto filter setting (1 byte).
const AUTO_FILTER_OFFSET: usize = 0x100C;

// --- Scan ---
/// Scan resume setting (1 byte).
const SCAN_RESUME_OFFSET: usize = 0x1007;
/// Digital scan resume setting (1 byte).
const DIGITAL_SCAN_RESUME_OFFSET: usize = 0x1008;
/// Scan restart time (1 byte).
const SCAN_RESTART_TIME_OFFSET: usize = 0x1009;
/// Scan restart carrier setting (1 byte).
const SCAN_RESTART_CARRIER_OFFSET: usize = 0x100A;

// --- TX ---
/// Timeout timer (1 byte).
const TIMEOUT_TIMER_OFFSET: usize = 0x1018;
/// TX inhibit setting (1 byte).
const TX_INHIBIT_OFFSET: usize = 0x1019;
/// Beat shift setting (1 byte).
const BEAT_SHIFT_OFFSET: usize = 0x101A;

// --- VOX (0x101B and 0x101C hardware-verified above) ---
/// VOX delay (1 byte, in 100 ms units).
const VOX_DELAY_OFFSET: usize = 0x101D;
/// VOX TX on busy setting (1 byte, 0=off, 1=on).
const VOX_TX_ON_BUSY_OFFSET: usize = 0x101E;

// --- CW ---
/// CW break-in setting (1 byte).
const CW_BREAK_IN_OFFSET: usize = 0x101F;
/// CW delay time (1 byte).
const CW_DELAY_TIME_OFFSET: usize = 0x1020;
/// CW pitch (1 byte).
const CW_PITCH_OFFSET: usize = 0x1021;

// --- DTMF ---
/// DTMF speed (1 byte).
const DTMF_SPEED_OFFSET: usize = 0x1024;
/// DTMF pause time (1 byte).
const DTMF_PAUSE_TIME_OFFSET: usize = 0x1026;
/// DTMF TX hold setting (1 byte).
const DTMF_TX_HOLD_OFFSET: usize = 0x1027;

// --- Repeater ---
/// Repeater auto offset setting (1 byte).
const REPEATER_AUTO_OFFSET_OFFSET: usize = 0x1030;
/// Repeater call key setting (1 byte).
const REPEATER_CALL_KEY_OFFSET: usize = 0x1031;

// --- Auxiliary ---
/// Microphone sensitivity (1 byte).
const MIC_SENSITIVITY_OFFSET: usize = 0x1040;
/// PF key 1 assignment (1 byte).
const PF_KEY1_OFFSET: usize = 0x1041;
/// PF key 2 assignment (1 byte).
const PF_KEY2_OFFSET: usize = 0x1042;

// --- Lock (0x1060 hardware-verified above) ---
/// Key lock type (1 byte, enum index).
const KEY_LOCK_TYPE_OFFSET: usize = 0x1061;
/// Lock key A setting (1 byte).
const LOCK_KEY_A_OFFSET: usize = 0x1062;
/// Lock key B setting (1 byte).
const LOCK_KEY_B_OFFSET: usize = 0x1063;
/// Lock key C setting (1 byte).
const LOCK_KEY_C_OFFSET: usize = 0x1064;
/// Lock PTT key setting (1 byte).
const LOCK_KEY_PTT_OFFSET: usize = 0x1065;
/// APRS lock setting (1 byte).
const APRS_LOCK_OFFSET: usize = 0x1097;

// --- Display ---
/// Dual display size (1 byte).
const DUAL_DISPLAY_SIZE_OFFSET: usize = 0x1066;
/// Display area (1 byte).
const DISPLAY_AREA_OFFSET: usize = 0x1067;
/// Info line setting (1 byte).
const INFO_LINE_OFFSET: usize = 0x1068;
/// Backlight control (1 byte).
const BACKLIGHT_CONTROL_OFFSET: usize = 0x1069;
/// Backlight timer (1 byte).
const BACKLIGHT_TIMER_OFFSET: usize = 0x106A;
/// Display hold time (1 byte).
const DISPLAY_HOLD_TIME_OFFSET: usize = 0x106B;
/// Display method (1 byte).
const DISPLAY_METHOD_OFFSET: usize = 0x106C;
/// Power-on display setting (1 byte).
const POWER_ON_DISPLAY_OFFSET: usize = 0x106D;

// --- Audio (0x1071 key beep hardware-verified above) ---
/// EMR volume level (1 byte).
const EMR_VOLUME_LEVEL_OFFSET: usize = 0x106E;
/// Auto mute return time (1 byte).
const AUTO_MUTE_RETURN_TIME_OFFSET: usize = 0x106F;
/// Announce setting (1 byte).
const ANNOUNCE_OFFSET: usize = 0x1070;
/// Beep volume (1 byte, range 1-7).
const BEEP_VOLUME_OFFSET: usize = 0x1072;
/// Voice language (1 byte).
const VOICE_LANGUAGE_OFFSET: usize = 0x1073;
/// Voice volume (1 byte).
const VOICE_VOLUME_OFFSET: usize = 0x1074;
/// Voice speed (1 byte).
const VOICE_SPEED_OFFSET: usize = 0x1075;
/// Volume lock (1 byte).
const VOLUME_LOCK_OFFSET: usize = 0x1076;

// --- Units ---
/// Speed/distance unit (1 byte, enum index).
const SPEED_DISTANCE_UNIT_OFFSET: usize = 0x1077;
/// Altitude/rain unit (1 byte, enum index).
const ALTITUDE_RAIN_UNIT_OFFSET: usize = 0x1083;
/// Temperature unit (1 byte, enum index).
const TEMPERATURE_UNIT_OFFSET: usize = 0x1084;

// --- Bluetooth (0x1078 hardware-verified above) ---
/// Bluetooth auto-connect setting (1 byte).
const BT_AUTO_CONNECT_OFFSET: usize = 0x1079;

// --- Interface ---
/// GPS Bluetooth interface (1 byte).
const GPS_BT_INTERFACE_OFFSET: usize = 0x1080;
/// PC output mode (1 byte).
const PC_OUTPUT_MODE_OFFSET: usize = 0x1085;
/// APRS USB mode (1 byte).
const APRS_USB_MODE_OFFSET: usize = 0x1086;
/// USB audio output setting (1 byte).
const USB_AUDIO_OUTPUT_OFFSET: usize = 0x1094;
/// Internet link setting (1 byte).
const INTERNET_LINK_OFFSET: usize = 0x1095;

// --- System ---
/// Power-on message flag (1 byte).
const POWER_ON_MESSAGE_FLAG_OFFSET: usize = 0x1087;
/// Language setting (1 byte, 0=English, 1=Japanese).
const LANGUAGE_OFFSET: usize = 0x1006;

// --- Battery ---
/// Battery saver (1 byte, 0=off, 1=on).
const BATTERY_SAVER_OFFSET: usize = 0x10C0;
/// Auto power off (1 byte, enum index).
const AUTO_POWER_OFF_OFFSET: usize = 0x10D0;

// --- DualBand (also at 0x0396 in VFO region) ---
/// Dual band MCP setting (1 byte).
const DUAL_BAND_MCP_OFFSET: usize = 0x1096;

// ---------------------------------------------------------------------------
// VFO data block (confirmed via memory dump analysis)
//
// The VFO data block at 0x0020 contains 6 VFO entries, each 40 bytes
// (same format as channel memory data). These represent the current
// VFO state for each band. Confirmed by both the memory map document
// and visual inspection of the memory dump (valid frequency data at
// 0x0020).
// ---------------------------------------------------------------------------

/// Byte offset of the VFO data block (6 entries x 40 bytes).
const VFO_DATA_OFFSET: usize = 0x0020;

/// Number of VFO entries in the VFO data block.
const VFO_ENTRY_COUNT: usize = 6;

/// Size of each VFO entry in bytes (same as channel record).
const VFO_ENTRY_SIZE: usize = programming::CHANNEL_RECORD_SIZE; // 40

// ---------------------------------------------------------------------------
// SettingsAccess (read-only)
// ---------------------------------------------------------------------------

/// Read-only access to the system settings region.
///
/// Provides raw byte access and typed field accessors for the settings
/// region at bytes `0x0000`-`0x1FFF`. Hardware-verified offsets are
/// confirmed on a real TH-D75; remaining offsets are from firmware
/// analysis and marked accordingly.
#[derive(Debug)]
pub struct SettingsAccess<'a> {
    image: &'a [u8],
}

impl<'a> SettingsAccess<'a> {
    /// Create a new settings accessor borrowing the raw image.
    pub(crate) const fn new(image: &'a [u8]) -> Self {
        Self { image }
    }

    /// Get the raw system settings bytes (0x0000-0x1FFF).
    ///
    /// Returns `None` if the image is too small.
    #[must_use]
    pub fn raw(&self) -> Option<&[u8]> {
        let end = SETTINGS_OFFSET + SETTINGS_SIZE;
        if end <= self.image.len() {
            Some(&self.image[SETTINGS_OFFSET..end])
        } else {
            None
        }
    }

    /// Get the power-on message (up to 16 characters).
    ///
    /// Stored at MCP offset `0x11C0`. Returns the null-terminated ASCII
    /// string.
    #[must_use]
    pub fn power_on_message(&self) -> String {
        extract_string(self.image, POWER_ON_MESSAGE_OFFSET, POWER_ON_MESSAGE_SIZE)
    }

    /// Get the internal model name (up to 16 characters).
    ///
    /// Stored at MCP offset `0x11D0`.
    #[must_use]
    pub fn model_name(&self) -> String {
        extract_string(self.image, MODEL_NAME_OFFSET, MODEL_NAME_SIZE)
    }

    /// Get the raw callsign data at MCP offset `0x1300`.
    ///
    /// The exact structure of this region is not yet fully mapped.
    /// Returns up to `len` bytes, or `None` if out of bounds.
    #[must_use]
    pub fn callsign_raw(&self, len: usize) -> Option<&[u8]> {
        let end = CALLSIGN_OFFSET + len;
        if end <= self.image.len() {
            Some(&self.image[CALLSIGN_OFFSET..end])
        } else {
            None
        }
    }

    /// Read an arbitrary byte range from the settings region.
    ///
    /// The offset is relative to the start of the image (MCP byte
    /// address). Returns `None` if the range extends past the image.
    #[must_use]
    pub fn read_bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset + len;
        if end <= self.image.len() {
            Some(&self.image[offset..end])
        } else {
            None
        }
    }

    // -----------------------------------------------------------------------
    // Typed settings accessors
    // -----------------------------------------------------------------------

    /// Read key beep setting (0=off, 1=on).
    ///
    /// MCP offset `0x1071`.
    #[must_use]
    pub fn key_beep(&self) -> bool {
        self.image.get(KEY_BEEP_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read beep volume (1-7, 0 if unreadable).
    ///
    /// MCP offset `0x1072`.
    #[must_use]
    pub fn beep_volume(&self) -> u8 {
        self.image
            .get(BEEP_VOLUME_OFFSET)
            .copied()
            .map_or(0, |b| b.min(7))
    }

    /// Read LCD backlight control setting (0 if unreadable).
    ///
    /// MCP offset `0x1069`.
    #[must_use]
    pub fn backlight(&self) -> u8 {
        self.image
            .get(BACKLIGHT_CONTROL_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read auto power off setting.
    ///
    /// MCP offset `0x10D0`.
    #[must_use]
    pub fn auto_power_off(&self) -> AutoPowerOff {
        match self.image.get(AUTO_POWER_OFF_OFFSET).copied().unwrap_or(0) {
            1 => AutoPowerOff::Min30,
            2 => AutoPowerOff::Min60,
            3 => AutoPowerOff::Min90,
            4 => AutoPowerOff::Min120,
            _ => AutoPowerOff::Off,
        }
    }

    /// Read battery saver setting.
    ///
    /// MCP offset `0x10C0`.
    #[must_use]
    pub fn battery_saver(&self) -> bool {
        self.image
            .get(BATTERY_SAVER_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read key lock type.
    ///
    /// MCP offset `0x1061`.
    #[must_use]
    pub fn key_lock_type(&self) -> KeyLockType {
        match self.image.get(KEY_LOCK_TYPE_OFFSET).copied().unwrap_or(0) {
            1 => KeyLockType::KeyAndPtt,
            2 => KeyLockType::KeyPttAndDial,
            _ => KeyLockType::KeyOnly,
        }
    }

    /// Read display unit settings.
    ///
    /// MCP offsets `0x1077` (speed/distance), `0x1083` (altitude/rain),
    /// `0x1084` (temperature).
    #[must_use]
    pub fn display_units(&self) -> DisplayUnits {
        let speed_distance = match self
            .image
            .get(SPEED_DISTANCE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => SpeedDistanceUnit::KilometersPerHour,
            2 => SpeedDistanceUnit::Knots,
            _ => SpeedDistanceUnit::MilesPerHour,
        };

        let altitude_rain = match self
            .image
            .get(ALTITUDE_RAIN_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => AltitudeRainUnit::MetersMm,
            _ => AltitudeRainUnit::FeetInch,
        };

        let temperature = match self
            .image
            .get(TEMPERATURE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
        {
            1 => TemperatureUnit::Celsius,
            _ => TemperatureUnit::Fahrenheit,
        };

        DisplayUnits {
            speed_distance,
            altitude_rain,
            temperature,
        }
    }

    /// Read language setting.
    ///
    /// MCP offset `0x1006`.
    #[must_use]
    pub fn language(&self) -> Language {
        match self.image.get(LANGUAGE_OFFSET).copied().unwrap_or(0) {
            1 => Language::Japanese,
            _ => Language::English,
        }
    }

    /// Read VOX enabled setting (0=off, 1=on).
    ///
    /// MCP offset `0x101B`.
    #[must_use]
    pub fn vox_enabled(&self) -> bool {
        self.image.get(VOX_ENABLED_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read VOX gain (0-9, 0 if unreadable).
    ///
    /// MCP offset `0x101C`.
    #[must_use]
    pub fn vox_gain(&self) -> u8 {
        self.image
            .get(VOX_GAIN_OFFSET)
            .copied()
            .map_or(0, |b| b.min(9))
    }

    /// Read VOX delay (in 100 ms units, 0 if unreadable).
    ///
    /// MCP offset `0x101D`.
    #[must_use]
    pub fn vox_delay(&self) -> u8 {
        self.image.get(VOX_DELAY_OFFSET).copied().unwrap_or(0)
    }

    /// Read squelch level for Band A (0-6, 0 if unreadable).
    ///
    /// MCP offset `0x100D`.
    #[must_use]
    pub fn squelch_a(&self) -> u8 {
        self.image
            .get(SQUELCH_A_OFFSET)
            .copied()
            .map_or(0, |b| b.min(6))
    }

    /// Read squelch level for Band B (0-6, 0 if unreadable).
    ///
    /// MCP offset `0x100E`.
    #[must_use]
    pub fn squelch_b(&self) -> u8 {
        self.image
            .get(SQUELCH_B_OFFSET)
            .copied()
            .map_or(0, |b| b.min(6))
    }

    // -----------------------------------------------------------------------
    // Accessors for settings from firmware analysis
    // -----------------------------------------------------------------------

    /// Read FM narrow setting (0 if unreadable).
    ///
    /// MCP offset `0x100F`.
    #[must_use]
    pub fn fm_narrow(&self) -> u8 {
        self.image.get(FM_NARROW_OFFSET).copied().unwrap_or(0)
    }

    /// Read SSB high-cut filter setting (0 if unreadable).
    ///
    /// MCP offset `0x1011`.
    #[must_use]
    pub fn ssb_high_cut(&self) -> u8 {
        self.image.get(SSB_HIGH_CUT_OFFSET).copied().unwrap_or(0)
    }

    /// Read CW high-cut filter setting (0 if unreadable).
    ///
    /// MCP offset `0x1012`.
    #[must_use]
    pub fn cw_high_cut(&self) -> u8 {
        self.image.get(CW_HIGH_CUT_OFFSET).copied().unwrap_or(0)
    }

    /// Read AM high-cut filter setting (0 if unreadable).
    ///
    /// MCP offset `0x1013`.
    #[must_use]
    pub fn am_high_cut(&self) -> u8 {
        self.image.get(AM_HIGH_CUT_OFFSET).copied().unwrap_or(0)
    }

    /// Read auto filter setting (0 if unreadable).
    ///
    /// MCP offset `0x100C`.
    #[must_use]
    pub fn auto_filter(&self) -> u8 {
        self.image.get(AUTO_FILTER_OFFSET).copied().unwrap_or(0)
    }

    /// Read scan resume setting (0 if unreadable).
    ///
    /// MCP offset `0x1007`.
    #[must_use]
    pub fn scan_resume(&self) -> u8 {
        self.image.get(SCAN_RESUME_OFFSET).copied().unwrap_or(0)
    }

    /// Read digital scan resume setting (0 if unreadable).
    ///
    /// MCP offset `0x1008`.
    #[must_use]
    pub fn digital_scan_resume(&self) -> u8 {
        self.image
            .get(DIGITAL_SCAN_RESUME_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read scan restart time (0 if unreadable).
    ///
    /// MCP offset `0x1009`.
    #[must_use]
    pub fn scan_restart_time(&self) -> u8 {
        self.image
            .get(SCAN_RESTART_TIME_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read scan restart carrier setting (0 if unreadable).
    ///
    /// MCP offset `0x100A`.
    #[must_use]
    pub fn scan_restart_carrier(&self) -> u8 {
        self.image
            .get(SCAN_RESTART_CARRIER_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read timeout timer setting (0 if unreadable).
    ///
    /// MCP offset `0x1018`.
    #[must_use]
    pub fn timeout_timer(&self) -> u8 {
        self.image.get(TIMEOUT_TIMER_OFFSET).copied().unwrap_or(0)
    }

    /// Read TX inhibit setting (false if unreadable).
    ///
    /// MCP offset `0x1019`.
    #[must_use]
    pub fn tx_inhibit(&self) -> bool {
        self.image.get(TX_INHIBIT_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read beat shift setting (false if unreadable).
    ///
    /// MCP offset `0x101A`.
    #[must_use]
    pub fn beat_shift(&self) -> bool {
        self.image.get(BEAT_SHIFT_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read VOX TX-on-busy setting (false if unreadable).
    ///
    /// MCP offset `0x101E`.
    #[must_use]
    pub fn vox_tx_on_busy(&self) -> bool {
        self.image
            .get(VOX_TX_ON_BUSY_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read CW break-in setting (false if unreadable).
    ///
    /// MCP offset `0x101F`.
    #[must_use]
    pub fn cw_break_in(&self) -> bool {
        self.image.get(CW_BREAK_IN_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read CW delay time (0 if unreadable).
    ///
    /// MCP offset `0x1020`.
    #[must_use]
    pub fn cw_delay_time(&self) -> u8 {
        self.image.get(CW_DELAY_TIME_OFFSET).copied().unwrap_or(0)
    }

    /// Read CW pitch (0 if unreadable).
    ///
    /// MCP offset `0x1021`.
    #[must_use]
    pub fn cw_pitch(&self) -> u8 {
        self.image.get(CW_PITCH_OFFSET).copied().unwrap_or(0)
    }

    /// Read DTMF speed (0 if unreadable).
    ///
    /// MCP offset `0x1024`.
    #[must_use]
    pub fn dtmf_speed(&self) -> u8 {
        self.image.get(DTMF_SPEED_OFFSET).copied().unwrap_or(0)
    }

    /// Read DTMF pause time (0 if unreadable).
    ///
    /// MCP offset `0x1026`.
    #[must_use]
    pub fn dtmf_pause_time(&self) -> u8 {
        self.image.get(DTMF_PAUSE_TIME_OFFSET).copied().unwrap_or(0)
    }

    /// Read DTMF TX hold setting (false if unreadable).
    ///
    /// MCP offset `0x1027`.
    #[must_use]
    pub fn dtmf_tx_hold(&self) -> bool {
        self.image.get(DTMF_TX_HOLD_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read repeater auto offset setting (false if unreadable).
    ///
    /// MCP offset `0x1030`.
    #[must_use]
    pub fn repeater_auto_offset(&self) -> bool {
        self.image
            .get(REPEATER_AUTO_OFFSET_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read repeater call key setting (0 if unreadable).
    ///
    /// MCP offset `0x1031`.
    #[must_use]
    pub fn repeater_call_key(&self) -> u8 {
        self.image
            .get(REPEATER_CALL_KEY_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read microphone sensitivity (0 if unreadable).
    ///
    /// MCP offset `0x1040`.
    #[must_use]
    pub fn mic_sensitivity(&self) -> u8 {
        self.image.get(MIC_SENSITIVITY_OFFSET).copied().unwrap_or(0)
    }

    /// Read PF key 1 assignment (0 if unreadable).
    ///
    /// MCP offset `0x1041`.
    #[must_use]
    pub fn pf_key1(&self) -> u8 {
        self.image.get(PF_KEY1_OFFSET).copied().unwrap_or(0)
    }

    /// Read PF key 2 assignment (0 if unreadable).
    ///
    /// MCP offset `0x1042`.
    #[must_use]
    pub fn pf_key2(&self) -> u8 {
        self.image.get(PF_KEY2_OFFSET).copied().unwrap_or(0)
    }

    /// Read lock key A setting (false if unreadable).
    ///
    /// MCP offset `0x1062`.
    #[must_use]
    pub fn lock_key_a(&self) -> bool {
        self.image.get(LOCK_KEY_A_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read lock key B setting (false if unreadable).
    ///
    /// MCP offset `0x1063`.
    #[must_use]
    pub fn lock_key_b(&self) -> bool {
        self.image.get(LOCK_KEY_B_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read lock key C setting (false if unreadable).
    ///
    /// MCP offset `0x1064`.
    #[must_use]
    pub fn lock_key_c(&self) -> bool {
        self.image.get(LOCK_KEY_C_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read lock PTT key setting (false if unreadable).
    ///
    /// MCP offset `0x1065`.
    #[must_use]
    pub fn lock_key_ptt(&self) -> bool {
        self.image.get(LOCK_KEY_PTT_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read APRS lock setting (false if unreadable).
    ///
    /// MCP offset `0x1097`.
    #[must_use]
    pub fn aprs_lock(&self) -> bool {
        self.image.get(APRS_LOCK_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read dual display size (0 if unreadable).
    ///
    /// MCP offset `0x1066`.
    #[must_use]
    pub fn dual_display_size(&self) -> u8 {
        self.image
            .get(DUAL_DISPLAY_SIZE_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read display area setting (0 if unreadable).
    ///
    /// MCP offset `0x1067`.
    #[must_use]
    pub fn display_area(&self) -> u8 {
        self.image.get(DISPLAY_AREA_OFFSET).copied().unwrap_or(0)
    }

    /// Read info line setting (0 if unreadable).
    ///
    /// MCP offset `0x1068`.
    #[must_use]
    pub fn info_line(&self) -> u8 {
        self.image.get(INFO_LINE_OFFSET).copied().unwrap_or(0)
    }

    /// Read backlight control setting (0 if unreadable).
    ///
    /// MCP offset `0x1069`.
    #[must_use]
    pub fn backlight_control(&self) -> u8 {
        self.image
            .get(BACKLIGHT_CONTROL_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read backlight timer (0 if unreadable).
    ///
    /// MCP offset `0x106A`.
    #[must_use]
    pub fn backlight_timer(&self) -> u8 {
        self.image.get(BACKLIGHT_TIMER_OFFSET).copied().unwrap_or(0)
    }

    /// Read display hold time (0 if unreadable).
    ///
    /// MCP offset `0x106B`.
    #[must_use]
    pub fn display_hold_time(&self) -> u8 {
        self.image
            .get(DISPLAY_HOLD_TIME_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read display method (0 if unreadable).
    ///
    /// MCP offset `0x106C`.
    #[must_use]
    pub fn display_method(&self) -> u8 {
        self.image.get(DISPLAY_METHOD_OFFSET).copied().unwrap_or(0)
    }

    /// Read power-on display setting (0 if unreadable).
    ///
    /// MCP offset `0x106D`.
    #[must_use]
    pub fn power_on_display(&self) -> u8 {
        self.image
            .get(POWER_ON_DISPLAY_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read EMR volume level (0 if unreadable).
    ///
    /// MCP offset `0x106E`.
    #[must_use]
    pub fn emr_volume_level(&self) -> u8 {
        self.image
            .get(EMR_VOLUME_LEVEL_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read auto mute return time (0 if unreadable).
    ///
    /// MCP offset `0x106F`.
    #[must_use]
    pub fn auto_mute_return_time(&self) -> u8 {
        self.image
            .get(AUTO_MUTE_RETURN_TIME_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read announce setting (false if unreadable).
    ///
    /// MCP offset `0x1070`.
    #[must_use]
    pub fn announce(&self) -> bool {
        self.image.get(ANNOUNCE_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read voice language setting (0 if unreadable).
    ///
    /// MCP offset `0x1073`.
    #[must_use]
    pub fn voice_language(&self) -> u8 {
        self.image.get(VOICE_LANGUAGE_OFFSET).copied().unwrap_or(0)
    }

    /// Read voice volume (0 if unreadable).
    ///
    /// MCP offset `0x1074`.
    #[must_use]
    pub fn voice_volume(&self) -> u8 {
        self.image.get(VOICE_VOLUME_OFFSET).copied().unwrap_or(0)
    }

    /// Read voice speed (0 if unreadable).
    ///
    /// MCP offset `0x1075`.
    #[must_use]
    pub fn voice_speed(&self) -> u8 {
        self.image.get(VOICE_SPEED_OFFSET).copied().unwrap_or(0)
    }

    /// Read volume lock setting (false if unreadable).
    ///
    /// MCP offset `0x1076`.
    #[must_use]
    pub fn volume_lock(&self) -> bool {
        self.image.get(VOLUME_LOCK_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read Bluetooth auto-connect setting (false if unreadable).
    ///
    /// MCP offset `0x1079`.
    #[must_use]
    pub fn bt_auto_connect(&self) -> bool {
        self.image
            .get(BT_AUTO_CONNECT_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read GPS Bluetooth interface setting (0 if unreadable).
    ///
    /// MCP offset `0x1080`.
    #[must_use]
    pub fn gps_bt_interface(&self) -> u8 {
        self.image
            .get(GPS_BT_INTERFACE_OFFSET)
            .copied()
            .unwrap_or(0)
    }

    /// Read PC output mode (0 if unreadable).
    ///
    /// MCP offset `0x1085`.
    #[must_use]
    pub fn pc_output_mode(&self) -> u8 {
        self.image.get(PC_OUTPUT_MODE_OFFSET).copied().unwrap_or(0)
    }

    /// Read APRS USB mode (0 if unreadable).
    ///
    /// MCP offset `0x1086`.
    #[must_use]
    pub fn aprs_usb_mode(&self) -> u8 {
        self.image.get(APRS_USB_MODE_OFFSET).copied().unwrap_or(0)
    }

    /// Read USB audio output setting (false if unreadable).
    ///
    /// MCP offset `0x1094`.
    #[must_use]
    pub fn usb_audio_output(&self) -> bool {
        self.image
            .get(USB_AUDIO_OUTPUT_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read internet link setting (false if unreadable).
    ///
    /// MCP offset `0x1095`.
    #[must_use]
    pub fn internet_link(&self) -> bool {
        self.image
            .get(INTERNET_LINK_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read power-on message flag (false if unreadable).
    ///
    /// MCP offset `0x1087`.
    #[must_use]
    pub fn power_on_message_flag(&self) -> bool {
        self.image
            .get(POWER_ON_MESSAGE_FLAG_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    /// Read dual band MCP setting (false if unreadable).
    ///
    /// MCP offset `0x1096`.
    #[must_use]
    pub fn dual_band_mcp(&self) -> bool {
        self.image
            .get(DUAL_BAND_MCP_OFFSET)
            .is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // Hardware-verified settings accessors
    // -----------------------------------------------------------------------

    /// Read Band A power level.
    ///
    /// MCP offset `0x0359`.
    /// Returns `High` if the byte is out of range or unreadable.
    #[must_use]
    pub fn power_level_a(&self) -> PowerLevel {
        self.image
            .get(POWER_LEVEL_A_OFFSET)
            .copied()
            .and_then(|b| PowerLevel::try_from(b).ok())
            .unwrap_or(PowerLevel::High)
    }

    /// Read Band A attenuator setting (0=off, 1=on).
    ///
    /// MCP offset `0x035C`.
    #[must_use]
    pub fn attenuator_a(&self) -> bool {
        self.image.get(ATTENUATOR_A_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read dual-band display setting (0=single, 1=dual).
    ///
    /// MCP offset `0x0396`.
    #[must_use]
    pub fn dual_band(&self) -> bool {
        self.image.get(DUAL_BAND_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read lock setting (0=unlocked, 1=locked).
    ///
    /// MCP offset `0x1060`.
    #[must_use]
    pub fn lock(&self) -> bool {
        self.image.get(LOCK_OFFSET).is_some_and(|&b| b != 0)
    }

    /// Read Bluetooth on/off setting (0=off, 1=on).
    ///
    /// MCP offset `0x1078`.
    #[must_use]
    pub fn bluetooth(&self) -> bool {
        self.image.get(BLUETOOTH_OFFSET).is_some_and(|&b| b != 0)
    }

    // -----------------------------------------------------------------------
    // VFO data accessors (confirmed via memory dump analysis)
    // -----------------------------------------------------------------------

    /// Read the raw 40-byte VFO entry for a given index (0-5).
    ///
    /// The VFO data block at `0x0020` contains 6 entries in the same
    /// 40-byte format as channel memory records. Returns `None` if the
    /// index is out of range or the region extends past the image.
    ///
    /// # VFO index mapping (estimated)
    ///
    /// The exact band-to-index mapping needs confirmation via differential
    /// dump, but typical Kenwood convention is:
    ///
    /// | Index | Band |
    /// |-------|------|
    /// | 0 | Band A VHF |
    /// | 1 | Band A 220 MHz |
    /// | 2 | Band A UHF |
    /// | 3 | Band B VHF |
    /// | 4 | Band B 220 MHz |
    /// | 5 | Band B UHF |
    #[must_use]
    pub fn vfo_raw(&self, index: usize) -> Option<&[u8]> {
        if index >= VFO_ENTRY_COUNT {
            return None;
        }
        let offset = VFO_DATA_OFFSET + index * VFO_ENTRY_SIZE;
        let end = offset + VFO_ENTRY_SIZE;
        if end <= self.image.len() {
            Some(&self.image[offset..end])
        } else {
            None
        }
    }

    /// Read the RX frequency from a VFO entry (0-5).
    ///
    /// Returns the frequency in Hz as a [`Frequency`], or `None` if the
    /// index is out of range or the VFO entry is empty (all `0xFF`).
    ///
    /// Located at `0x0020 + index * 40`. Confirmed via memory dump.
    #[must_use]
    pub fn vfo_frequency(&self, index: usize) -> Option<Frequency> {
        let raw = self.vfo_raw(index)?;
        // Check for empty entry (all 0xFF).
        if raw.iter().all(|&b| b == 0xFF) {
            return None;
        }
        // Check for zeroed entry.
        if raw[..4].iter().all(|&b| b == 0x00) {
            return None;
        }
        let freq = Frequency::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        Some(freq)
    }

    /// Read the operating mode from a VFO entry (0-5).
    ///
    /// Returns the flash-encoded mode, or `None` if the index is out of
    /// range or the VFO entry is empty. The mode is in byte 0x09 bits
    /// \[6:4\] of the 40-byte VFO record.
    ///
    /// Located at `0x0020 + index * 40 + 0x09`. Confirmed via memory dump.
    #[must_use]
    pub fn vfo_mode(&self, index: usize) -> Option<MemoryMode> {
        let raw = self.vfo_raw(index)?;
        if raw.iter().all(|&b| b == 0xFF) {
            return None;
        }
        let mode_bits = (raw[0x09] >> 4) & 0x07;
        MemoryMode::try_from(mode_bits).ok()
    }

    /// Read the TX offset or split frequency from a VFO entry (0-5).
    ///
    /// Returns the offset/split frequency in Hz, or `None` if the index
    /// is out of range or the VFO entry is empty.
    ///
    /// Located at `0x0020 + index * 40 + 0x04`. Confirmed via memory dump.
    #[must_use]
    pub fn vfo_tx_offset(&self, index: usize) -> Option<Frequency> {
        let raw = self.vfo_raw(index)?;
        if raw.iter().all(|&b| b == 0xFF) {
            return None;
        }
        let offset = Frequency::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
        Some(offset)
    }

    /// Get the number of non-empty VFO entries (out of 6).
    #[must_use]
    pub fn vfo_count(&self) -> usize {
        (0..VFO_ENTRY_COUNT)
            .filter(|&i| self.vfo_frequency(i).is_some())
            .count()
    }

    // -----------------------------------------------------------------------
    // Raw numeric accessors for enum-typed settings (for TUI +/- cycling)
    // -----------------------------------------------------------------------

    /// Read key lock type as raw byte (0=KeyOnly, 1=KeyAndPtt, 2=KeyPttAndDial).
    ///
    /// MCP offset `0x1061`.
    #[must_use]
    pub fn key_lock_type_raw(&self) -> u8 {
        self.image
            .get(KEY_LOCK_TYPE_OFFSET)
            .copied()
            .unwrap_or(0)
            .min(2)
    }

    /// Read auto power off as raw byte (0=Off, 1=30m, 2=60m, 3=90m, 4=120m).
    ///
    /// MCP offset `0x10D0`.
    #[must_use]
    pub fn auto_power_off_raw(&self) -> u8 {
        self.image
            .get(AUTO_POWER_OFF_OFFSET)
            .copied()
            .unwrap_or(0)
            .min(4)
    }

    /// Read speed/distance unit as raw byte (0=mph, 1=km/h, 2=knots).
    ///
    /// MCP offset `0x1077`.
    #[must_use]
    pub fn speed_distance_unit_raw(&self) -> u8 {
        self.image
            .get(SPEED_DISTANCE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
            .min(2)
    }

    /// Read altitude/rain unit as raw byte (0=ft/in, 1=m/mm).
    ///
    /// MCP offset `0x1083`.
    #[must_use]
    pub fn altitude_rain_unit_raw(&self) -> u8 {
        self.image
            .get(ALTITUDE_RAIN_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
            .min(1)
    }

    /// Read temperature unit as raw byte (0=°F, 1=°C).
    ///
    /// MCP offset `0x1084`.
    #[must_use]
    pub fn temperature_unit_raw(&self) -> u8 {
        self.image
            .get(TEMPERATURE_UNIT_OFFSET)
            .copied()
            .unwrap_or(0)
            .min(1)
    }
}

// ---------------------------------------------------------------------------
// SettingsWriter (mutable access)
// ---------------------------------------------------------------------------

/// Mutable access to the system settings region of the memory image.
///
/// Provides write methods for settings with verified offsets. Only
/// settings with hardware-verified offsets have write accessors to
/// prevent corrupting the memory image with unconfirmed offsets.
#[derive(Debug)]
pub struct SettingsWriter<'a> {
    image: &'a mut [u8],
}

impl<'a> SettingsWriter<'a> {
    /// Create a new mutable settings accessor.
    pub(crate) const fn new(image: &'a mut [u8]) -> Self {
        Self { image }
    }

    /// Set LCD backlight control setting.
    ///
    /// MCP offset `0x1069`.
    pub fn set_backlight(&mut self, level: u8) {
        if let Some(b) = self.image.get_mut(BACKLIGHT_CONTROL_OFFSET) {
            *b = level;
        }
    }

    /// Set backlight control (same as `set_backlight`, named for clarity).
    ///
    /// MCP offset `0x1069`.
    pub fn set_backlight_control(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(BACKLIGHT_CONTROL_OFFSET) {
            *b = value;
        }
    }

    /// Set backlight timer.
    ///
    /// MCP offset `0x106A`.
    pub fn set_backlight_timer(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(BACKLIGHT_TIMER_OFFSET) {
            *b = value;
        }
    }

    /// Set beep volume (1-7).
    ///
    /// Values above 7 are clamped to 7.
    ///
    /// MCP offset `0x1072`.
    pub fn set_beep_volume(&mut self, volume: u8) {
        if let Some(b) = self.image.get_mut(BEEP_VOLUME_OFFSET) {
            *b = volume.min(7);
        }
    }

    /// Set auto power off.
    ///
    /// MCP offset `0x10D0`.
    pub fn set_auto_power_off(&mut self, value: AutoPowerOff) {
        if let Some(b) = self.image.get_mut(AUTO_POWER_OFF_OFFSET) {
            *b = match value {
                AutoPowerOff::Off => 0,
                AutoPowerOff::Min30 => 1,
                AutoPowerOff::Min60 => 2,
                AutoPowerOff::Min90 => 3,
                AutoPowerOff::Min120 => 4,
            };
        }
    }

    /// Set battery saver on/off.
    ///
    /// MCP offset `0x10C0`.
    pub fn set_battery_saver(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(BATTERY_SAVER_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set key lock type.
    ///
    /// MCP offset `0x1061`.
    pub fn set_key_lock_type(&mut self, value: KeyLockType) {
        if let Some(b) = self.image.get_mut(KEY_LOCK_TYPE_OFFSET) {
            *b = match value {
                KeyLockType::KeyOnly => 0,
                KeyLockType::KeyAndPtt => 1,
                KeyLockType::KeyPttAndDial => 2,
            };
        }
    }

    /// Set language.
    ///
    /// MCP offset `0x1006`.
    pub fn set_language(&mut self, value: Language) {
        if let Some(b) = self.image.get_mut(LANGUAGE_OFFSET) {
            *b = match value {
                Language::English => 0,
                Language::Japanese => 1,
            };
        }
    }

    /// Set speed/distance display unit.
    ///
    /// MCP offset `0x1077`.
    pub fn set_speed_distance_unit(&mut self, value: SpeedDistanceUnit) {
        if let Some(b) = self.image.get_mut(SPEED_DISTANCE_UNIT_OFFSET) {
            *b = match value {
                SpeedDistanceUnit::MilesPerHour => 0,
                SpeedDistanceUnit::KilometersPerHour => 1,
                SpeedDistanceUnit::Knots => 2,
            };
        }
    }

    /// Set altitude/rain display unit.
    ///
    /// MCP offset `0x1083`.
    pub fn set_altitude_rain_unit(&mut self, value: AltitudeRainUnit) {
        if let Some(b) = self.image.get_mut(ALTITUDE_RAIN_UNIT_OFFSET) {
            *b = match value {
                AltitudeRainUnit::FeetInch => 0,
                AltitudeRainUnit::MetersMm => 1,
            };
        }
    }

    /// Set temperature display unit.
    ///
    /// MCP offset `0x1084`.
    pub fn set_temperature_unit(&mut self, value: TemperatureUnit) {
        if let Some(b) = self.image.get_mut(TEMPERATURE_UNIT_OFFSET) {
            *b = match value {
                TemperatureUnit::Fahrenheit => 0,
                TemperatureUnit::Celsius => 1,
            };
        }
    }

    /// Set VOX delay (in 100 ms units, clamped to 30).
    ///
    /// MCP offset `0x101D`.
    pub fn set_vox_delay(&mut self, delay: u8) {
        if let Some(b) = self.image.get_mut(VOX_DELAY_OFFSET) {
            *b = delay.min(30);
        }
    }

    /// Set VOX TX-on-busy on/off.
    ///
    /// MCP offset `0x101E`.
    pub fn set_vox_tx_on_busy(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(VOX_TX_ON_BUSY_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set squelch level for Band A (0-6).
    ///
    /// MCP offset `0x100D`.
    pub fn set_squelch_a(&mut self, level: u8) {
        if let Some(b) = self.image.get_mut(SQUELCH_A_OFFSET) {
            *b = level.min(6);
        }
    }

    /// Set squelch level for Band B (0-6).
    ///
    /// MCP offset `0x100E`.
    pub fn set_squelch_b(&mut self, level: u8) {
        if let Some(b) = self.image.get_mut(SQUELCH_B_OFFSET) {
            *b = level.min(6);
        }
    }

    /// Set FM narrow setting.
    ///
    /// MCP offset `0x100F`.
    pub fn set_fm_narrow(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(FM_NARROW_OFFSET) {
            *b = value;
        }
    }

    /// Set auto filter setting.
    ///
    /// MCP offset `0x100C`.
    pub fn set_auto_filter(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(AUTO_FILTER_OFFSET) {
            *b = value;
        }
    }

    /// Set scan resume setting.
    ///
    /// MCP offset `0x1007`.
    pub fn set_scan_resume(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(SCAN_RESUME_OFFSET) {
            *b = value;
        }
    }

    /// Set digital scan resume setting.
    ///
    /// MCP offset `0x1008`.
    pub fn set_digital_scan_resume(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DIGITAL_SCAN_RESUME_OFFSET) {
            *b = value;
        }
    }

    /// Set timeout timer.
    ///
    /// MCP offset `0x1018`.
    pub fn set_timeout_timer(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(TIMEOUT_TIMER_OFFSET) {
            *b = value;
        }
    }

    /// Set TX inhibit on/off.
    ///
    /// MCP offset `0x1019`.
    pub fn set_tx_inhibit(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(TX_INHIBIT_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set beat shift on/off.
    ///
    /// MCP offset `0x101A`.
    pub fn set_beat_shift(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(BEAT_SHIFT_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set CW break-in on/off.
    ///
    /// MCP offset `0x101F`.
    pub fn set_cw_break_in(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(CW_BREAK_IN_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set CW pitch.
    ///
    /// MCP offset `0x1021`.
    pub fn set_cw_pitch(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(CW_PITCH_OFFSET) {
            *b = value;
        }
    }

    /// Set DTMF speed.
    ///
    /// MCP offset `0x1024`.
    pub fn set_dtmf_speed(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DTMF_SPEED_OFFSET) {
            *b = value;
        }
    }

    /// Set mic sensitivity.
    ///
    /// MCP offset `0x1040`.
    pub fn set_mic_sensitivity(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(MIC_SENSITIVITY_OFFSET) {
            *b = value;
        }
    }

    /// Set PF key 1 assignment.
    ///
    /// MCP offset `0x1041`.
    pub fn set_pf_key1(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(PF_KEY1_OFFSET) {
            *b = value;
        }
    }

    /// Set PF key 2 assignment.
    ///
    /// MCP offset `0x1042`.
    pub fn set_pf_key2(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(PF_KEY2_OFFSET) {
            *b = value;
        }
    }

    /// Set APRS lock on/off.
    ///
    /// MCP offset `0x1097`.
    pub fn set_aprs_lock(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(APRS_LOCK_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set dual display size.
    ///
    /// MCP offset `0x1066`.
    pub fn set_dual_display_size(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DUAL_DISPLAY_SIZE_OFFSET) {
            *b = value;
        }
    }

    /// Set display area.
    ///
    /// MCP offset `0x1067`.
    pub fn set_display_area(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DISPLAY_AREA_OFFSET) {
            *b = value;
        }
    }

    /// Set info line setting.
    ///
    /// MCP offset `0x1068`.
    pub fn set_info_line(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(INFO_LINE_OFFSET) {
            *b = value;
        }
    }

    /// Set volume lock on/off.
    ///
    /// MCP offset `0x1076`.
    pub fn set_volume_lock(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(VOLUME_LOCK_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set Bluetooth auto-connect on/off.
    ///
    /// MCP offset `0x1079`.
    pub fn set_bt_auto_connect(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(BT_AUTO_CONNECT_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set PC output mode.
    ///
    /// MCP offset `0x1085`.
    pub fn set_pc_output_mode(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(PC_OUTPUT_MODE_OFFSET) {
            *b = value;
        }
    }

    /// Set APRS USB mode.
    ///
    /// MCP offset `0x1086`.
    pub fn set_aprs_usb_mode(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(APRS_USB_MODE_OFFSET) {
            *b = value;
        }
    }

    /// Set power-on message flag on/off.
    ///
    /// MCP offset `0x1087`.
    pub fn set_power_on_message_flag(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(POWER_ON_MESSAGE_FLAG_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set dual band MCP setting on/off.
    ///
    /// MCP offset `0x1096`.
    pub fn set_dual_band_mcp(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(DUAL_BAND_MCP_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set key beep on/off.
    ///
    /// MCP offset `0x1071`.
    pub fn set_key_beep(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(KEY_BEEP_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set VOX enabled on/off.
    ///
    /// MCP offset `0x101B`.
    pub fn set_vox_enabled(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(VOX_ENABLED_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set VOX gain level (0-9).
    ///
    /// Values above 9 are clamped to 9.
    ///
    /// MCP offset `0x101C`.
    pub fn set_vox_gain(&mut self, gain: u8) {
        if let Some(b) = self.image.get_mut(VOX_GAIN_OFFSET) {
            *b = gain.min(9);
        }
    }

    /// Set lock on/off.
    ///
    /// MCP offset `0x1060`.
    pub fn set_lock(&mut self, locked: bool) {
        if let Some(b) = self.image.get_mut(LOCK_OFFSET) {
            *b = u8::from(locked);
        }
    }

    /// Set dual-band display on/off.
    ///
    /// MCP offset `0x0396`.
    pub fn set_dual_band(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(DUAL_BAND_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set Band A attenuator on/off.
    ///
    /// MCP offset `0x035C`.
    pub fn set_attenuator_a(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(ATTENUATOR_A_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set Band A power level.
    ///
    /// MCP offset `0x0359`.
    pub fn set_power_level_a(&mut self, level: PowerLevel) {
        if let Some(b) = self.image.get_mut(POWER_LEVEL_A_OFFSET) {
            *b = u8::from(level);
        }
    }

    /// Set Bluetooth on/off.
    ///
    /// MCP offset `0x1078`.
    pub fn set_bluetooth(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(BLUETOOTH_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    // -----------------------------------------------------------------------
    // Additional writer methods for settings not yet covered above
    // -----------------------------------------------------------------------

    /// Set SSB high-cut filter setting.
    ///
    /// MCP offset `0x1011`.
    pub fn set_ssb_high_cut(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(SSB_HIGH_CUT_OFFSET) {
            *b = value;
        }
    }

    /// Set CW high-cut filter setting.
    ///
    /// MCP offset `0x1012`.
    pub fn set_cw_high_cut(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(CW_HIGH_CUT_OFFSET) {
            *b = value;
        }
    }

    /// Set AM high-cut filter setting.
    ///
    /// MCP offset `0x1013`.
    pub fn set_am_high_cut(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(AM_HIGH_CUT_OFFSET) {
            *b = value;
        }
    }

    /// Set scan restart time.
    ///
    /// MCP offset `0x1009`.
    pub fn set_scan_restart_time(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(SCAN_RESTART_TIME_OFFSET) {
            *b = value;
        }
    }

    /// Set scan restart carrier setting.
    ///
    /// MCP offset `0x100A`.
    pub fn set_scan_restart_carrier(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(SCAN_RESTART_CARRIER_OFFSET) {
            *b = value;
        }
    }

    /// Set CW delay time.
    ///
    /// MCP offset `0x1020`.
    pub fn set_cw_delay_time(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(CW_DELAY_TIME_OFFSET) {
            *b = value;
        }
    }

    /// Set DTMF pause time.
    ///
    /// MCP offset `0x1026`.
    pub fn set_dtmf_pause_time(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DTMF_PAUSE_TIME_OFFSET) {
            *b = value;
        }
    }

    /// Set DTMF TX hold on/off.
    ///
    /// MCP offset `0x1027`.
    pub fn set_dtmf_tx_hold(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(DTMF_TX_HOLD_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set repeater auto offset on/off.
    ///
    /// MCP offset `0x1030`.
    pub fn set_repeater_auto_offset(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(REPEATER_AUTO_OFFSET_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set repeater call key function.
    ///
    /// MCP offset `0x1031`.
    pub fn set_repeater_call_key(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(REPEATER_CALL_KEY_OFFSET) {
            *b = value;
        }
    }

    /// Set lock key A on/off.
    ///
    /// MCP offset `0x1062`.
    pub fn set_lock_key_a(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(LOCK_KEY_A_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set lock key B on/off.
    ///
    /// MCP offset `0x1063`.
    pub fn set_lock_key_b(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(LOCK_KEY_B_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set lock key C on/off.
    ///
    /// MCP offset `0x1064`.
    pub fn set_lock_key_c(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(LOCK_KEY_C_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set lock PTT key on/off.
    ///
    /// MCP offset `0x1065`.
    pub fn set_lock_key_ptt(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(LOCK_KEY_PTT_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set display hold time.
    ///
    /// MCP offset `0x106B`.
    pub fn set_display_hold_time(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DISPLAY_HOLD_TIME_OFFSET) {
            *b = value;
        }
    }

    /// Set display method.
    ///
    /// MCP offset `0x106C`.
    pub fn set_display_method(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(DISPLAY_METHOD_OFFSET) {
            *b = value;
        }
    }

    /// Set power-on display setting.
    ///
    /// MCP offset `0x106D`.
    pub fn set_power_on_display(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(POWER_ON_DISPLAY_OFFSET) {
            *b = value;
        }
    }

    /// Set EMR volume level.
    ///
    /// MCP offset `0x106E`.
    pub fn set_emr_volume_level(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(EMR_VOLUME_LEVEL_OFFSET) {
            *b = value;
        }
    }

    /// Set auto mute return time.
    ///
    /// MCP offset `0x106F`.
    pub fn set_auto_mute_return_time(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(AUTO_MUTE_RETURN_TIME_OFFSET) {
            *b = value;
        }
    }

    /// Set announce on/off.
    ///
    /// MCP offset `0x1070`.
    pub fn set_announce(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(ANNOUNCE_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set voice language.
    ///
    /// MCP offset `0x1073`.
    pub fn set_voice_language(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(VOICE_LANGUAGE_OFFSET) {
            *b = value;
        }
    }

    /// Set voice volume.
    ///
    /// MCP offset `0x1074`.
    pub fn set_voice_volume(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(VOICE_VOLUME_OFFSET) {
            *b = value;
        }
    }

    /// Set voice speed.
    ///
    /// MCP offset `0x1075`.
    pub fn set_voice_speed(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(VOICE_SPEED_OFFSET) {
            *b = value;
        }
    }

    /// Set USB audio output on/off.
    ///
    /// MCP offset `0x1094`.
    pub fn set_usb_audio_output(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(USB_AUDIO_OUTPUT_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set internet link on/off.
    ///
    /// MCP offset `0x1095`.
    pub fn set_internet_link(&mut self, enabled: bool) {
        if let Some(b) = self.image.get_mut(INTERNET_LINK_OFFSET) {
            *b = u8::from(enabled);
        }
    }

    /// Set GPS/BT interface setting.
    ///
    /// MCP offset `0x1080`.
    pub fn set_gps_bt_interface(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(GPS_BT_INTERFACE_OFFSET) {
            *b = value;
        }
    }

    // -----------------------------------------------------------------------
    // Raw numeric setters for enum-typed settings (for TUI +/- cycling)
    // -----------------------------------------------------------------------

    /// Set key lock type as raw byte (0=KeyOnly, 1=KeyAndPtt, 2=KeyPttAndDial).
    ///
    /// MCP offset `0x1061`.
    pub fn set_key_lock_type_raw(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(KEY_LOCK_TYPE_OFFSET) {
            *b = value.min(2);
        }
    }

    /// Set auto power off as raw byte (0=Off, 1=30m, 2=60m, 3=90m, 4=120m).
    ///
    /// MCP offset `0x10D0`.
    pub fn set_auto_power_off_raw(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(AUTO_POWER_OFF_OFFSET) {
            *b = value.min(4);
        }
    }

    /// Set speed/distance unit as raw byte (0=mph, 1=km/h, 2=knots).
    ///
    /// MCP offset `0x1077`.
    pub fn set_speed_distance_unit_raw(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(SPEED_DISTANCE_UNIT_OFFSET) {
            *b = value.min(2);
        }
    }

    /// Set altitude/rain unit as raw byte (0=ft/in, 1=m/mm).
    ///
    /// MCP offset `0x1083`.
    pub fn set_altitude_rain_unit_raw(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(ALTITUDE_RAIN_UNIT_OFFSET) {
            *b = value.min(1);
        }
    }

    /// Set temperature unit as raw byte (0=°F, 1=°C).
    ///
    /// MCP offset `0x1084`.
    pub fn set_temperature_unit_raw(&mut self, value: u8) {
        if let Some(b) = self.image.get_mut(TEMPERATURE_UNIT_OFFSET) {
            *b = value.min(1);
        }
    }
}

/// Extract a null-terminated ASCII string from the image at a given offset.
fn extract_string(image: &[u8], offset: usize, max_len: usize) -> String {
    let end = offset + max_len;
    if end > image.len() {
        return String::new();
    }
    let slice = &image[offset..end];
    let nul = slice.iter().position(|&b| b == 0).unwrap_or(max_len);
    String::from_utf8_lossy(&slice[..nul]).trim().to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::programming::TOTAL_SIZE;
    use crate::types::settings::{
        AltitudeRainUnit, AutoPowerOff, KeyLockType, Language, SpeedDistanceUnit, TemperatureUnit,
    };

    fn make_settings_image() -> Vec<u8> {
        let mut image = vec![0x00_u8; TOTAL_SIZE];

        // Write a power-on message at 0x11C0.
        let msg = b"Hello D75!\0\0\0\0\0\0";
        image[POWER_ON_MESSAGE_OFFSET..POWER_ON_MESSAGE_OFFSET + 16].copy_from_slice(msg);

        // Write a model name at 0x11D0.
        let model = b"TH-D75A\0\0\0\0\0\0\0\0\0";
        image[MODEL_NAME_OFFSET..MODEL_NAME_OFFSET + 16].copy_from_slice(model);

        image
    }

    #[test]
    fn settings_power_on_message() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        assert_eq!(settings.power_on_message(), "Hello D75!");
    }

    #[test]
    fn settings_model_name() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        assert_eq!(settings.model_name(), "TH-D75A");
    }

    #[test]
    fn settings_raw_not_none() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        let raw = settings.raw().unwrap();
        assert_eq!(raw.len(), SETTINGS_SIZE);
    }

    #[test]
    fn settings_read_bytes() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        // Read the power-on message via raw bytes.
        let bytes = settings.read_bytes(POWER_ON_MESSAGE_OFFSET, 10).unwrap();
        assert_eq!(&bytes[..10], b"Hello D75!");
    }

    // -----------------------------------------------------------------------
    // Read accessor tests (verified offsets)
    // -----------------------------------------------------------------------

    #[test]
    fn settings_key_beep() {
        let mut image = make_settings_image();
        image[KEY_BEEP_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().key_beep());
    }

    #[test]
    fn settings_key_beep_off() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().key_beep());
    }

    #[test]
    fn settings_vox() {
        let mut image = make_settings_image();
        image[VOX_ENABLED_OFFSET] = 1;
        image[VOX_GAIN_OFFSET] = 7;
        image[VOX_DELAY_OFFSET] = 5;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        assert!(settings.vox_enabled());
        assert_eq!(settings.vox_gain(), 7);
        assert_eq!(settings.vox_delay(), 5);
    }

    #[test]
    fn settings_lock() {
        let mut image = make_settings_image();
        image[LOCK_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().lock());
    }

    #[test]
    fn settings_lock_off() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().lock());
    }

    #[test]
    fn settings_dual_band() {
        let mut image = make_settings_image();
        image[DUAL_BAND_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().dual_band());
    }

    #[test]
    fn settings_dual_band_off() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().dual_band());
    }

    #[test]
    fn settings_attenuator_a() {
        let mut image = make_settings_image();
        image[ATTENUATOR_A_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().attenuator_a());
    }

    #[test]
    fn settings_attenuator_a_off() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().attenuator_a());
    }

    #[test]
    fn settings_power_level_a() {
        let mut image = make_settings_image();
        image[POWER_LEVEL_A_OFFSET] = 2; // Lo
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().power_level_a(), PowerLevel::Low);
    }

    #[test]
    fn settings_power_level_a_default() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        // 0x00 maps to High.
        assert_eq!(mi.settings().power_level_a(), PowerLevel::High);
    }

    #[test]
    fn settings_power_level_a_invalid_defaults_to_high() {
        let mut image = make_settings_image();
        image[POWER_LEVEL_A_OFFSET] = 0xFF;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().power_level_a(), PowerLevel::High);
    }

    #[test]
    fn settings_bluetooth() {
        let mut image = make_settings_image();
        image[BLUETOOTH_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().bluetooth());
    }

    #[test]
    fn settings_bluetooth_off() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().bluetooth());
    }

    // -----------------------------------------------------------------------
    // Read accessor tests (firmware analysis offsets)
    // -----------------------------------------------------------------------

    #[test]
    fn settings_beep_volume() {
        let mut image = make_settings_image();
        image[BEEP_VOLUME_OFFSET] = 5;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().beep_volume(), 5);
    }

    #[test]
    fn settings_beep_volume_clamped() {
        let mut image = make_settings_image();
        image[BEEP_VOLUME_OFFSET] = 0xFF;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().beep_volume(), 7);
    }

    #[test]
    fn settings_backlight() {
        let mut image = make_settings_image();
        image[BACKLIGHT_CONTROL_OFFSET] = 4;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().backlight(), 4);
    }

    #[test]
    fn settings_auto_power_off() {
        let mut image = make_settings_image();
        image[AUTO_POWER_OFF_OFFSET] = 2;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().auto_power_off(), AutoPowerOff::Min60);
    }

    #[test]
    fn settings_auto_power_off_unknown_defaults_to_off() {
        let mut image = make_settings_image();
        image[AUTO_POWER_OFF_OFFSET] = 0xFF;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().auto_power_off(), AutoPowerOff::Off);
    }

    #[test]
    fn settings_battery_saver() {
        let mut image = make_settings_image();
        image[BATTERY_SAVER_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().battery_saver());
    }

    #[test]
    fn settings_key_lock_type() {
        let mut image = make_settings_image();
        image[KEY_LOCK_TYPE_OFFSET] = 2;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().key_lock_type(), KeyLockType::KeyPttAndDial);
    }

    #[test]
    fn settings_display_units() {
        let mut image = make_settings_image();
        image[SPEED_DISTANCE_UNIT_OFFSET] = 1; // km/h
        image[ALTITUDE_RAIN_UNIT_OFFSET] = 1; // m/mm
        image[TEMPERATURE_UNIT_OFFSET] = 1; // Celsius
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let units = mi.settings().display_units();
        assert_eq!(units.speed_distance, SpeedDistanceUnit::KilometersPerHour);
        assert_eq!(units.altitude_rain, AltitudeRainUnit::MetersMm);
        assert_eq!(units.temperature, TemperatureUnit::Celsius);
    }

    #[test]
    fn settings_language() {
        let mut image = make_settings_image();
        image[LANGUAGE_OFFSET] = 1;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().language(), Language::Japanese);
    }

    #[test]
    fn settings_squelch() {
        let mut image = make_settings_image();
        image[SQUELCH_A_OFFSET] = 3;
        image[SQUELCH_B_OFFSET] = 4;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        assert_eq!(settings.squelch_a(), 3);
        assert_eq!(settings.squelch_b(), 4);
    }

    #[test]
    fn settings_squelch_clamped() {
        let mut image = make_settings_image();
        image[SQUELCH_A_OFFSET] = 0xFF;
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().squelch_a(), 6);
    }

    // -----------------------------------------------------------------------
    // Write accessor tests (SettingsWriter)
    // -----------------------------------------------------------------------

    #[test]
    fn write_key_beep() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().key_beep());
        mi.settings_mut().set_key_beep(true);
        assert!(mi.settings().key_beep());
        mi.settings_mut().set_key_beep(false);
        assert!(!mi.settings().key_beep());
    }

    #[test]
    fn write_vox_enabled() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().vox_enabled());
        mi.settings_mut().set_vox_enabled(true);
        assert!(mi.settings().vox_enabled());
        mi.settings_mut().set_vox_enabled(false);
        assert!(!mi.settings().vox_enabled());
    }

    #[test]
    fn write_vox_gain() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        mi.settings_mut().set_vox_gain(7);
        assert_eq!(mi.settings().vox_gain(), 7);
    }

    #[test]
    fn write_vox_gain_clamped() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        mi.settings_mut().set_vox_gain(0xFF);
        assert_eq!(mi.settings().vox_gain(), 9);
    }

    #[test]
    fn write_lock() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().lock());
        mi.settings_mut().set_lock(true);
        assert!(mi.settings().lock());
        mi.settings_mut().set_lock(false);
        assert!(!mi.settings().lock());
    }

    #[test]
    fn write_dual_band() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().dual_band());
        mi.settings_mut().set_dual_band(true);
        assert!(mi.settings().dual_band());
        mi.settings_mut().set_dual_band(false);
        assert!(!mi.settings().dual_band());
    }

    #[test]
    fn write_attenuator_a() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().attenuator_a());
        mi.settings_mut().set_attenuator_a(true);
        assert!(mi.settings().attenuator_a());
        mi.settings_mut().set_attenuator_a(false);
        assert!(!mi.settings().attenuator_a());
    }

    #[test]
    fn write_power_level_a() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        mi.settings_mut().set_power_level_a(PowerLevel::Low);
        assert_eq!(mi.settings().power_level_a(), PowerLevel::Low);
        mi.settings_mut().set_power_level_a(PowerLevel::ExtraLow);
        assert_eq!(mi.settings().power_level_a(), PowerLevel::ExtraLow);
        mi.settings_mut().set_power_level_a(PowerLevel::High);
        assert_eq!(mi.settings().power_level_a(), PowerLevel::High);
    }

    #[test]
    fn write_bluetooth() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(!mi.settings().bluetooth());
        mi.settings_mut().set_bluetooth(true);
        assert!(mi.settings().bluetooth());
        mi.settings_mut().set_bluetooth(false);
        assert!(!mi.settings().bluetooth());
    }

    #[test]
    fn write_roundtrip_all_verified() {
        let image = make_settings_image();
        let mut mi = crate::memory::MemoryImage::from_raw(image).unwrap();

        // Set everything to non-default values.
        mi.settings_mut().set_key_beep(true);
        mi.settings_mut().set_vox_enabled(true);
        mi.settings_mut().set_vox_gain(9);
        mi.settings_mut().set_lock(true);
        mi.settings_mut().set_dual_band(true);
        mi.settings_mut().set_attenuator_a(true);
        mi.settings_mut().set_power_level_a(PowerLevel::ExtraLow);
        mi.settings_mut().set_bluetooth(true);

        // Verify reads match.
        let s = mi.settings();
        assert!(s.key_beep());
        assert!(s.vox_enabled());
        assert_eq!(s.vox_gain(), 9);
        assert!(s.lock());
        assert!(s.dual_band());
        assert!(s.attenuator_a());
        assert_eq!(s.power_level_a(), PowerLevel::ExtraLow);
        assert!(s.bluetooth());

        // Verify raw bytes at the verified offsets.
        let raw = mi.as_raw();
        assert_eq!(raw[KEY_BEEP_OFFSET], 1);
        assert_eq!(raw[VOX_ENABLED_OFFSET], 1);
        assert_eq!(raw[VOX_GAIN_OFFSET], 9);
        assert_eq!(raw[LOCK_OFFSET], 1);
        assert_eq!(raw[DUAL_BAND_OFFSET], 1);
        assert_eq!(raw[ATTENUATOR_A_OFFSET], 1);
        assert_eq!(raw[POWER_LEVEL_A_OFFSET], 3); // ExtraLow = 3
        assert_eq!(raw[BLUETOOTH_OFFSET], 1);
    }

    // -----------------------------------------------------------------------
    // VFO data accessor tests
    // -----------------------------------------------------------------------

    #[test]
    fn vfo_raw_accessible() {
        let mut image = make_settings_image();
        // Write a known pattern at VFO entry 0.
        image[VFO_DATA_OFFSET..VFO_DATA_OFFSET + 4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let settings = mi.settings();
        let raw = settings.vfo_raw(0).unwrap();
        assert_eq!(raw.len(), VFO_ENTRY_SIZE);
        assert_eq!(&raw[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn vfo_raw_out_of_range() {
        let image = make_settings_image();
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().vfo_raw(6).is_none());
    }

    #[test]
    fn vfo_frequency_valid() {
        let mut image = make_settings_image();
        // VFO entry 0 at offset 0x0020: 146.520 MHz.
        let freq: u32 = 146_520_000;
        let offset = VFO_DATA_OFFSET;
        image[offset..offset + 4].copy_from_slice(&freq.to_le_bytes());
        // Make the entry non-zero past the frequency.
        image[offset + 8] = 0x50; // step/shift byte

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let f = mi.settings().vfo_frequency(0).unwrap();
        assert_eq!(f.as_hz(), 146_520_000);
    }

    #[test]
    fn vfo_frequency_empty_entry() {
        let mut image = make_settings_image();
        // Fill VFO entry 0 with 0xFF (empty).
        let offset = VFO_DATA_OFFSET;
        image[offset..offset + VFO_ENTRY_SIZE].fill(0xFF);

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert!(mi.settings().vfo_frequency(0).is_none());
    }

    #[test]
    fn vfo_frequency_zeroed_entry() {
        let image = make_settings_image(); // All zeros.
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        // Frequency bytes are all zero -> returns None.
        assert!(mi.settings().vfo_frequency(0).is_none());
    }

    #[test]
    fn vfo_mode_fm() {
        let mut image = make_settings_image();
        let offset = VFO_DATA_OFFSET;
        // Non-empty entry with some frequency data.
        image[offset..offset + 4].copy_from_slice(&146_520_000_u32.to_le_bytes());
        // Byte 0x09: mode bits [6:4] = 0 (FM).
        image[offset + 0x09] = 0x00;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let mode = mi.settings().vfo_mode(0).unwrap();
        assert_eq!(mode, MemoryMode::Fm);
    }

    #[test]
    fn vfo_mode_am() {
        let mut image = make_settings_image();
        let offset = VFO_DATA_OFFSET;
        image[offset..offset + 4].copy_from_slice(&7_100_000_u32.to_le_bytes());
        // Byte 0x09: mode bits [6:4] = 2 (AM in flash encoding).
        image[offset + 0x09] = 0x20;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let mode = mi.settings().vfo_mode(0).unwrap();
        assert_eq!(mode, MemoryMode::Am);
    }

    #[test]
    fn vfo_mode_lsb() {
        let mut image = make_settings_image();
        let offset = VFO_DATA_OFFSET;
        image[offset..offset + 4].copy_from_slice(&7_100_000_u32.to_le_bytes());
        // Byte 0x09: mode bits [6:4] = 3 (LSB in flash encoding).
        image[offset + 0x09] = 0x30;

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let mode = mi.settings().vfo_mode(0).unwrap();
        assert_eq!(mode, MemoryMode::Lsb);
    }

    #[test]
    fn vfo_tx_offset() {
        let mut image = make_settings_image();
        let offset = VFO_DATA_OFFSET;
        image[offset..offset + 4].copy_from_slice(&146_520_000_u32.to_le_bytes());
        image[offset + 4..offset + 8].copy_from_slice(&600_000_u32.to_le_bytes());

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let tx_off = mi.settings().vfo_tx_offset(0).unwrap();
        assert_eq!(tx_off.as_hz(), 600_000);
    }

    #[test]
    fn vfo_count_none_populated() {
        let image = make_settings_image(); // All zeros.
        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().vfo_count(), 0);
    }

    #[test]
    fn vfo_count_with_entries() {
        let mut image = make_settings_image();
        // Populate VFO entries 0 and 2.
        let offset0 = VFO_DATA_OFFSET;
        image[offset0..offset0 + 4].copy_from_slice(&146_520_000_u32.to_le_bytes());
        let offset2 = VFO_DATA_OFFSET + 2 * VFO_ENTRY_SIZE;
        image[offset2..offset2 + 4].copy_from_slice(&446_000_000_u32.to_le_bytes());

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        assert_eq!(mi.settings().vfo_count(), 2);
    }

    #[test]
    fn vfo_second_entry_frequency() {
        let mut image = make_settings_image();
        let offset = VFO_DATA_OFFSET + VFO_ENTRY_SIZE; // Entry 1.
        image[offset..offset + 4].copy_from_slice(&222_100_000_u32.to_le_bytes());

        let mi = crate::memory::MemoryImage::from_raw(image).unwrap();
        let f = mi.settings().vfo_frequency(1).unwrap();
        assert_eq!(f.as_hz(), 222_100_000);
    }
}
