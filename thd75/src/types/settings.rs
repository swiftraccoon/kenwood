//! Radio-wide system, audio, and display settings for the TH-D75.
//!
//! These types cover the radio's global configuration accessible through
//! the menu system (Configuration, Audio, Display sections). They model
//! settings from the capability gap analysis features 123-197 that are
//! not subsystem-specific (not APRS, D-STAR, or GPS).

use crate::error::ValidationError;

// ---------------------------------------------------------------------------
// Display settings
// ---------------------------------------------------------------------------

/// Display and illumination settings.
///
/// Controls the TH-D75's LCD backlight, color theme, power-on message,
/// and meter display. Derived from capability gap analysis features 159-169.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySettings {
    /// LCD backlight control mode.
    pub backlight_control: BacklightControl,
    /// Backlight auto-off timer in seconds (0 = always on).
    pub backlight_timer: u8,
    /// LCD brightness level (1-6, 1 = dimmest, 6 = brightest).
    pub lcd_brightness: u8,
    /// Background color theme.
    pub background_color: BackgroundColor,
    /// Power-on message displayed at startup (up to 16 characters).
    pub power_on_message: PowerOnMessage,
    /// Single-band display mode (show only one band at a time).
    pub single_band_display: bool,
    /// S-meter and power meter display type.
    pub meter_type: MeterType,
    /// Display method for the dual-band screen.
    pub display_method: DisplayMethod,
    /// LED indicator control.
    pub led_control: LedControl,
    /// Info backlight on receive.
    pub info_backlight: bool,
    /// Display hold time for transient information (seconds).
    pub display_hold_time: DisplayHoldTime,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            backlight_control: BacklightControl::Auto,
            backlight_timer: 5,
            lcd_brightness: 4,
            background_color: BackgroundColor::Blue,
            power_on_message: PowerOnMessage::default(),
            single_band_display: false,
            meter_type: MeterType::Bar,
            display_method: DisplayMethod::Dual,
            led_control: LedControl::On,
            info_backlight: true,
            display_hold_time: DisplayHoldTime::Sec3,
        }
    }
}

/// LCD backlight control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BacklightControl {
    /// Backlight always on.
    On,
    /// Backlight auto (turns on with key press, off after timer).
    Auto,
    /// Backlight always off.
    Off,
}

/// Background color theme for the LCD display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackgroundColor {
    /// Amber / warm color theme.
    Amber,
    /// Green color theme.
    Green,
    /// Blue color theme (default).
    Blue,
    /// White color theme.
    White,
}

/// Power-on message text (up to 16 characters).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PowerOnMessage(String);

impl PowerOnMessage {
    /// Maximum length of the power-on message.
    pub const MAX_LEN: usize = 16;

    /// Creates a new power-on message.
    ///
    /// # Errors
    ///
    /// Returns `None` if the text exceeds 16 characters.
    #[must_use]
    pub fn new(text: &str) -> Option<Self> {
        if text.len() <= Self::MAX_LEN {
            Some(Self(text.to_owned()))
        } else {
            None
        }
    }

    /// Returns the power-on message as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// S-meter and power meter display type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeterType {
    /// Bar graph meter display.
    Bar,
    /// Numeric (digital) meter display.
    Numeric,
}

/// Display method for the main screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayMethod {
    /// Show both bands simultaneously.
    Dual,
    /// Show single band only.
    Single,
}

/// LED indicator control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LedControl {
    /// LED indicators enabled.
    On,
    /// LED indicators disabled.
    Off,
}

/// Display hold time for transient information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayHoldTime {
    /// 3 second hold time.
    Sec3,
    /// 5 second hold time.
    Sec5,
    /// 10 second hold time.
    Sec10,
    /// Continuous (hold until dismissed).
    Continuous,
}

// ---------------------------------------------------------------------------
// Audio settings
// ---------------------------------------------------------------------------

/// Audio and sound settings.
///
/// Controls the TH-D75's beep, equalizer, microphone sensitivity,
/// and voice guidance features. Derived from capability gap analysis
/// features 123-148.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    /// Key beep on/off.
    pub beep: bool,
    /// Beep volume level (1-7).
    pub beep_volume: u8,
    /// TX audio equalizer preset (for FM/NFM mode).
    pub tx_equalizer_fm: EqSetting,
    /// TX audio equalizer preset (for DV mode).
    pub tx_equalizer_dv: EqSetting,
    /// RX audio equalizer preset.
    pub rx_equalizer: EqSetting,
    /// Microphone sensitivity level.
    pub mic_sensitivity: MicSensitivity,
    /// Voice guidance on/off.
    pub voice_guidance: bool,
    /// Voice guidance volume (1-7).
    pub voice_guidance_volume: u8,
    /// Voice guidance speed.
    pub voice_guidance_speed: VoiceGuideSpeed,
    /// Audio balance between Band A and Band B (0 = A only, 50 = equal,
    /// 100 = B only).
    pub balance: u8,
    /// TX monitor on/off (hear own transmit audio).
    pub tx_monitor: bool,
    /// USB audio output level.
    pub usb_audio_output_level: u8,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            beep: true,
            beep_volume: 4,
            tx_equalizer_fm: EqSetting::Off,
            tx_equalizer_dv: EqSetting::Off,
            rx_equalizer: EqSetting::Off,
            mic_sensitivity: MicSensitivity::Medium,
            voice_guidance: false,
            voice_guidance_volume: 4,
            voice_guidance_speed: VoiceGuideSpeed::Normal,
            balance: 50,
            tx_monitor: false,
            usb_audio_output_level: 4,
        }
    }
}

/// Audio equalizer setting (TX or RX).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EqSetting {
    /// Equalizer disabled (flat response).
    Off,
    /// High-boost preset.
    HighBoost,
    /// Low-boost preset.
    LowBoost,
    /// Full-boost preset.
    FullBoost,
}

/// Microphone sensitivity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MicSensitivity {
    /// Low sensitivity.
    Low,
    /// Medium sensitivity (default).
    Medium,
    /// High sensitivity.
    High,
}

/// Voice guidance speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VoiceGuideSpeed {
    /// Slow voice guidance.
    Slow,
    /// Normal speed voice guidance.
    Normal,
    /// Fast voice guidance.
    Fast,
}

// ---------------------------------------------------------------------------
// System settings
// ---------------------------------------------------------------------------

/// System-wide radio settings.
///
/// Covers global configuration such as power management, key lock,
/// display units, language, and programmable function keys.
/// Derived from capability gap analysis features 170-197.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSettings {
    /// Battery saver on/off (reduce power in standby by cycling the
    /// receiver).
    pub battery_saver: bool,
    /// Auto power off timer.
    pub auto_power_off: AutoPowerOff,
    /// Key lock enabled.
    pub key_lock: bool,
    /// Key lock type (which keys are affected).
    pub key_lock_type: KeyLockType,
    /// Volume lock (prevent accidental volume changes).
    pub volume_lock: bool,
    /// DTMF key lock (lock the DTMF keypad separately).
    pub dtmf_lock: bool,
    /// Mic key lock (lock microphone keys).
    pub mic_lock: bool,
    /// Display unit system.
    pub display_units: DisplayUnits,
    /// Language selection.
    pub language: Language,
    /// Time-out timer in seconds (0 = disabled, 30-600).
    /// Automatically stops TX after the timeout.
    pub time_out_timer: u16,
    /// Programmable function key PF1 (front panel) assignment.
    pub pf1_key: PfKeyFunction,
    /// Programmable function key PF2 (front panel) assignment.
    pub pf2_key: PfKeyFunction,
    /// Programmable function key PF1 (mic) assignment.
    pub pf1_mic: PfKeyFunction,
    /// Programmable function key PF2 (mic) assignment.
    pub pf2_mic: PfKeyFunction,
    /// Programmable function key PF3 (mic) assignment.
    pub pf3_mic: PfKeyFunction,
    /// WX alert on/off (automatic weather channel scan; TH-D75A only).
    pub wx_alert: bool,
    /// Secret access code enabled (require code to power on).
    pub secret_access_code: bool,
    /// Date format.
    pub date_format: DateFormat,
    /// Time zone offset from UTC (e.g. -5 for EST).
    pub time_zone_offset: i8,
}

impl Default for SystemSettings {
    fn default() -> Self {
        Self {
            battery_saver: true,
            auto_power_off: AutoPowerOff::Off,
            key_lock: false,
            key_lock_type: KeyLockType::KeyOnly,
            volume_lock: false,
            dtmf_lock: false,
            mic_lock: false,
            display_units: DisplayUnits::default(),
            language: Language::English,
            time_out_timer: 0,
            pf1_key: PfKeyFunction::Monitor,
            pf2_key: PfKeyFunction::VoiceAlert,
            pf1_mic: PfKeyFunction::Monitor,
            pf2_mic: PfKeyFunction::VoiceAlert,
            pf3_mic: PfKeyFunction::VoiceAlert,
            wx_alert: false,
            secret_access_code: false,
            date_format: DateFormat::YearMonthDay,
            time_zone_offset: 0,
        }
    }
}

/// Auto power off timer duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoPowerOff {
    /// Auto power off disabled.
    Off,
    /// Power off after 30 minutes of inactivity.
    Min30,
    /// Power off after 60 minutes of inactivity.
    Min60,
    /// Power off after 90 minutes of inactivity.
    Min90,
    /// Power off after 120 minutes of inactivity.
    Min120,
}

/// Key lock type -- which controls are affected by key lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyLockType {
    /// Lock front panel keys only.
    KeyOnly,
    /// Lock front panel keys and PTT.
    KeyAndPtt,
    /// Lock front panel keys, PTT, and dial.
    KeyPttAndDial,
}

/// Display unit preferences.
///
/// Controls measurement units displayed on the radio screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DisplayUnits {
    /// Speed and distance units.
    pub speed_distance: SpeedDistanceUnit,
    /// Altitude and rainfall units.
    pub altitude_rain: AltitudeRainUnit,
    /// Temperature units.
    pub temperature: TemperatureUnit,
}

impl Default for DisplayUnits {
    fn default() -> Self {
        Self {
            speed_distance: SpeedDistanceUnit::MilesPerHour,
            altitude_rain: AltitudeRainUnit::FeetInch,
            temperature: TemperatureUnit::Fahrenheit,
        }
    }
}

/// Speed and distance measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpeedDistanceUnit {
    /// Miles per hour / miles.
    MilesPerHour,
    /// Kilometers per hour / kilometers.
    KilometersPerHour,
    /// Knots / nautical miles.
    Knots,
}

/// Altitude and rainfall measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AltitudeRainUnit {
    /// Feet / inches.
    FeetInch,
    /// Meters / millimeters.
    MetersMm,
}

/// Temperature measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemperatureUnit {
    /// Fahrenheit.
    Fahrenheit,
    /// Celsius.
    Celsius,
}

/// Language selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    /// English.
    English,
    /// Japanese.
    Japanese,
}

/// Date display format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateFormat {
    /// Year/Month/Day (e.g. 2026/03/28).
    YearMonthDay,
    /// Month/Day/Year (e.g. 03/28/2026).
    MonthDayYear,
    /// Day/Month/Year (e.g. 28/03/2026).
    DayMonthYear,
}

/// Programmable function key assignment.
///
/// The TH-D75 has 2 front-panel PF keys and 3 microphone PF keys,
/// each assignable to one of these functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PfKeyFunction {
    /// Monitor (open squelch).
    Monitor,
    /// Voice alert toggle.
    VoiceAlert,
    /// Weather channel.
    Wx,
    /// Scan start/stop.
    Scan,
    /// Frequency direct entry.
    DirectEntry,
    /// VFO/Memory mode toggle.
    VfoMr,
    /// Screen capture (save to SD card).
    ScreenCapture,
    /// Backlight toggle.
    Backlight,
    /// Voice guidance toggle.
    VoiceGuidance,
    /// Lock toggle.
    Lock,
    /// 1750 Hz tone burst.
    Tone1750,
    /// APRS beacon transmit.
    AprsBeacon,
    /// Recording start/stop.
    Recording,
}

// ---------------------------------------------------------------------------
// TryFrom<u8> implementations for MCP binary parsing
// ---------------------------------------------------------------------------

impl TryFrom<u8> for BacklightControl {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::On),
            1 => Ok(Self::Auto),
            2 => Ok(Self::Off),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "backlight control",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl TryFrom<u8> for BackgroundColor {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Amber),
            1 => Ok(Self::Green),
            2 => Ok(Self::Blue),
            3 => Ok(Self::White),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "background color",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl TryFrom<u8> for MeterType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Bar),
            1 => Ok(Self::Numeric),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "meter type",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl TryFrom<u8> for DisplayMethod {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Dual),
            1 => Ok(Self::Single),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "display method",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl TryFrom<u8> for LedControl {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::On),
            1 => Ok(Self::Off),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "LED control",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl TryFrom<u8> for DisplayHoldTime {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Sec3),
            1 => Ok(Self::Sec5),
            2 => Ok(Self::Sec10),
            3 => Ok(Self::Continuous),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "display hold time",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl TryFrom<u8> for EqSetting {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::HighBoost),
            2 => Ok(Self::LowBoost),
            3 => Ok(Self::FullBoost),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "EQ setting",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl TryFrom<u8> for MicSensitivity {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Low),
            1 => Ok(Self::Medium),
            2 => Ok(Self::High),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "mic sensitivity",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl TryFrom<u8> for VoiceGuideSpeed {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Slow),
            1 => Ok(Self::Normal),
            2 => Ok(Self::Fast),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "voice guide speed",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl TryFrom<u8> for AutoPowerOff {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Min30),
            2 => Ok(Self::Min60),
            3 => Ok(Self::Min90),
            4 => Ok(Self::Min120),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "auto power off",
                value,
                detail: "must be 0-4",
            }),
        }
    }
}

impl TryFrom<u8> for KeyLockType {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::KeyOnly),
            1 => Ok(Self::KeyAndPtt),
            2 => Ok(Self::KeyPttAndDial),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "key lock type",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl TryFrom<u8> for Language {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::English),
            1 => Ok(Self::Japanese),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "language",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl TryFrom<u8> for DateFormat {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::YearMonthDay),
            1 => Ok(Self::MonthDayYear),
            2 => Ok(Self::DayMonthYear),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "date format",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_settings_default() {
        let ds = DisplaySettings::default();
        assert_eq!(ds.backlight_control, BacklightControl::Auto);
        assert_eq!(ds.background_color, BackgroundColor::Blue);
    }

    #[test]
    fn audio_settings_default() {
        let a = AudioSettings::default();
        assert!(a.beep);
        assert_eq!(a.beep_volume, 4);
        assert_eq!(a.mic_sensitivity, MicSensitivity::Medium);
    }

    #[test]
    fn system_settings_default() {
        let s = SystemSettings::default();
        assert!(s.battery_saver);
        assert_eq!(s.auto_power_off, AutoPowerOff::Off);
        assert_eq!(s.language, Language::English);
        assert_eq!(s.time_out_timer, 0);
    }

    #[test]
    fn power_on_message_valid() {
        let msg = PowerOnMessage::new("TH-D75 Ready").unwrap();
        assert_eq!(msg.as_str(), "TH-D75 Ready");
    }

    #[test]
    fn power_on_message_max_length() {
        let msg = PowerOnMessage::new("1234567890123456").unwrap();
        assert_eq!(msg.as_str().len(), 16);
    }

    #[test]
    fn power_on_message_too_long() {
        assert!(PowerOnMessage::new("12345678901234567").is_none());
    }

    #[test]
    fn display_units_default() {
        let u = DisplayUnits::default();
        assert_eq!(u.speed_distance, SpeedDistanceUnit::MilesPerHour);
        assert_eq!(u.temperature, TemperatureUnit::Fahrenheit);
    }
}
