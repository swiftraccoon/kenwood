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
///
/// # Menu numbers (per Operating Tips §5.2, User Manual Chapter 12)
///
/// - Menu No. 900: Backlight control, one of `Auto` (keys/encoder turn
///   on, timer turns off; also lights on APRS interrupt or scan pause),
///   `Auto (DC-IN)` (same as Auto on battery, always-on on DC),
///   `Manual` (only `[Power]` toggles), `On` (always on).
/// - Menu No. 901: Backlight timer, 3 to 60 seconds, default 10.
/// - Menu No. 902: LCD brightness (High / Medium / Low).
/// - Menu No. 903: Power-on message, up to 16 characters, default
///   "HELLO !!". Displayed for approximately 2 seconds at power-on.
///   MCP-D75 software can also set a custom bitmap graphic.
/// - Menu No. 904: Single Band Display (Off / GPS(Altitude) /
///   GPS(GS) / Date / Demodulation Mode).
/// - Menu No. 905: Meter Type (Type 1 / Type 2 / Type 3, the S/RF
///   meter design variants).
/// - Menu No. 906: Background Color (Black / White).
/// - Menu No. 907: Info Backlight (Off / LCD / LCD+Key). Controls
///   whether the backlight turns on for APRS or D-STAR interrupt
///   display and scan pause/stop events.
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
            led_control: LedControl::new(true, false),
            info_backlight: true,
            display_hold_time: DisplayHoldTime::Sec3,
        }
    }
}

/// LCD backlight control mode (Menu No. 900).
///
/// Per User Manual Chapter 12: temporary lighting can also be triggered
/// by pressing `[Power]`, which illuminates the display and keys for the
/// timer duration (Menu No. 901). Pressing `[Power]` while lit turns
/// the light off immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BacklightControl {
    /// Backlight always on while power is on.
    On,
    /// Backlight auto (turns on with key press or encoder rotation,
    /// off after the timer in Menu No. 901 expires). Also lights on
    /// APRS interrupt reception and scan pause/stop.
    Auto,
    /// Backlight always off (only `[Power]` can trigger temporary
    /// lighting in Manual mode, per User Manual Chapter 12).
    Off,
}

/// Background color theme for the LCD display (Menu No. 906).
///
/// Per User Manual Chapter 12: the user manual defines only Black
/// and White options. The Operating Tips previously referenced Amber,
/// Green, Blue, and White. The actual available values depend on
/// firmware version.
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

/// Independent LED indicator controls (Menu No. 181).
///
/// MCP byte `0x1028` stores the RX LED in bit `0x01` and the FM Radio
/// LED in bit `0x02`. They are separate checkboxes on firmware V1.03;
/// neither bit implies the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LedControl {
    /// Light the receive LED while a signal is being received.
    pub receive: bool,
    /// Light the LED while the FM broadcast radio is playing.
    pub fm_radio: bool,
}

impl Default for LedControl {
    fn default() -> Self {
        Self::new(true, false)
    }
}

impl LedControl {
    /// Both LED indicators disabled.
    pub const ALL_OFF: Self = Self {
        receive: false,
        fm_radio: false,
    };

    /// Both LED indicators enabled.
    pub const ALL_ON: Self = Self {
        receive: true,
        fm_radio: true,
    };

    /// Creates independent RX and FM Radio LED controls.
    #[must_use]
    pub const fn new(receive: bool, fm_radio: bool) -> Self {
        Self { receive, fm_radio }
    }
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
///
/// # Audio equalizer (per User Manual Chapter 12)
///
/// The TH-D75 has independent TX and RX parametric equalizers:
///
/// - **TX EQ** (Menu No. 911/912): 4-band (0.4/0.8/1.6/3.2 kHz),
///   range -9 to +3 dB per band. Separate enable for FM/NFM and DV modes.
/// - **RX EQ** (Menu No. 911/913): 5-band (0.4/0.8/1.6/3.2/6.4 kHz),
///   range -9 to +9 dB per band. The 6.4 kHz band has no effect in
///   DV/DR mode since digital audio bandwidth is limited to 4 kHz.
///
/// # Volume balance (per User Manual Chapter 5)
///
/// Menu No. 910 controls audio balance between Band A and Band B.
/// The `Operation Band Only` setting outputs sound only from the
/// operation band when both bands are simultaneously busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioSettings {
    /// Key beep on/off.
    pub beep: bool,
    /// Beep volume level (1-7).
    pub beep_volume: u8,
    /// TX equalizer enables and four independently adjustable bands.
    pub tx_equalizer: TxEqualizer,
    /// RX equalizer enable and five independently adjustable bands.
    pub rx_equalizer: RxEqualizer,
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
            tx_equalizer: TxEqualizer::default(),
            rx_equalizer: RxEqualizer::default(),
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

/// One TH-D75 TX equalizer band level, from -9 through +3 dB.
///
/// MCP stores these levels as `0..=12`, with raw zero representing
/// -9 dB and raw nine representing 0 dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxEqLevel(i8);

impl TxEqLevel {
    /// Minimum TX equalizer level in dB.
    pub const MIN_DB: i8 = -9;
    /// Maximum TX equalizer level in dB.
    pub const MAX_DB: i8 = 3;
    /// Flat (0 dB) TX equalizer level.
    pub const FLAT: Self = Self(0);

    /// Creates a TX equalizer level if `db` is in `-9..=3`.
    #[must_use]
    pub const fn new(db: i8) -> Option<Self> {
        if db >= Self::MIN_DB && db <= Self::MAX_DB {
            Some(Self(db))
        } else {
            None
        }
    }

    /// Decodes the MCP representation (`0..=12`).
    #[must_use]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        if raw <= 12 {
            Some(Self(raw.cast_signed() - 9))
        } else {
            None
        }
    }

    /// Returns this level in dB.
    #[must_use]
    pub const fn as_db(self) -> i8 {
        self.0
    }

    /// Returns the MCP representation (`0..=12`).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        (self.0 + 9).cast_unsigned()
    }
}

impl Default for TxEqLevel {
    fn default() -> Self {
        Self::FLAT
    }
}

impl TryFrom<u8> for TxEqLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_raw(value).ok_or(ValidationError::SettingOutOfRange {
            name: "TX EQ level",
            value,
            detail: "must be raw 0-12 (-9 through +3 dB)",
        })
    }
}

impl From<TxEqLevel> for u8 {
    fn from(value: TxEqLevel) -> Self {
        value.as_raw()
    }
}

/// One TH-D75 RX equalizer band level, from -9 through +9 dB.
///
/// MCP stores these levels as `0..=18`, with raw zero representing
/// -9 dB and raw nine representing 0 dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RxEqLevel(i8);

impl RxEqLevel {
    /// Minimum RX equalizer level in dB.
    pub const MIN_DB: i8 = -9;
    /// Maximum RX equalizer level in dB.
    pub const MAX_DB: i8 = 9;
    /// Flat (0 dB) RX equalizer level.
    pub const FLAT: Self = Self(0);

    /// Creates an RX equalizer level if `db` is in `-9..=9`.
    #[must_use]
    pub const fn new(db: i8) -> Option<Self> {
        if db >= Self::MIN_DB && db <= Self::MAX_DB {
            Some(Self(db))
        } else {
            None
        }
    }

    /// Decodes the MCP representation (`0..=18`).
    #[must_use]
    pub const fn from_raw(raw: u8) -> Option<Self> {
        if raw <= 18 {
            Some(Self(raw.cast_signed() - 9))
        } else {
            None
        }
    }

    /// Returns this level in dB.
    #[must_use]
    pub const fn as_db(self) -> i8 {
        self.0
    }

    /// Returns the MCP representation (`0..=18`).
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        (self.0 + 9).cast_unsigned()
    }
}

impl Default for RxEqLevel {
    fn default() -> Self {
        Self::FLAT
    }
}

impl TryFrom<u8> for RxEqLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_raw(value).ok_or(ValidationError::SettingOutOfRange {
            name: "RX EQ level",
            value,
            detail: "must be raw 0-18 (-9 through +9 dB)",
        })
    }
}

impl From<RxEqLevel> for u8 {
    fn from(value: RxEqLevel) -> Self {
        value.as_raw()
    }
}

/// TX equalizer state (Menus No. 911 and 912).
///
/// FM/NFM and DV have independent enable bits but share the same four
/// band levels. Each field maps directly to one MCP-D75 setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TxEqualizer {
    /// Enable TX EQ for FM and NFM modulation.
    pub fm_nfm_enabled: bool,
    /// Enable TX EQ for DV modulation.
    pub dv_enabled: bool,
    /// 0.4 kHz band level.
    pub level_400_hz: TxEqLevel,
    /// 0.8 kHz band level.
    pub level_800_hz: TxEqLevel,
    /// 1.6 kHz band level.
    pub level_1_6_khz: TxEqLevel,
    /// 3.2 kHz band level.
    pub level_3_2_khz: TxEqLevel,
}

impl Default for TxEqualizer {
    fn default() -> Self {
        Self {
            fm_nfm_enabled: false,
            dv_enabled: false,
            level_400_hz: TxEqLevel::FLAT,
            level_800_hz: TxEqLevel::FLAT,
            level_1_6_khz: TxEqLevel::FLAT,
            level_3_2_khz: TxEqLevel::FLAT,
        }
    }
}

/// RX equalizer state (Menus No. 911 and 913).
///
/// RX EQ has one enable bit and five independently adjustable bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RxEqualizer {
    /// Enable the RX equalizer.
    pub enabled: bool,
    /// 0.4 kHz band level.
    pub level_400_hz: RxEqLevel,
    /// 0.8 kHz band level.
    pub level_800_hz: RxEqLevel,
    /// 1.6 kHz band level.
    pub level_1_6_khz: RxEqLevel,
    /// 3.2 kHz band level.
    pub level_3_2_khz: RxEqLevel,
    /// 6.4 kHz band level.
    pub level_6_4_khz: RxEqLevel,
}

impl Default for RxEqualizer {
    fn default() -> Self {
        Self {
            enabled: false,
            level_400_hz: RxEqLevel::FLAT,
            level_800_hz: RxEqLevel::FLAT,
            level_1_6_khz: RxEqLevel::FLAT,
            level_3_2_khz: RxEqLevel::FLAT,
            level_6_4_khz: RxEqLevel::FLAT,
        }
    }
}

/// Microphone sensitivity level (Menu No. 112).
///
/// Per User Manual Chapter 12: applies to both the internal microphone
/// and an external microphone. Default: Medium.
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

/// Receiver beat-shift type (`radio.BeatShift`, raw 0-7).
///
/// The D75 offers eight beat-shift types (Type 1 through Type 8) that
/// move internally generated spurious beats out of the receive
/// passband. This is a type selector, not an on/off switch: the
/// MCP-D75 serializer stores one byte with domain 0-7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BeatShift {
    /// Beat shift type 1.
    Type1,
    /// Beat shift type 2.
    Type2,
    /// Beat shift type 3.
    Type3,
    /// Beat shift type 4.
    Type4,
    /// Beat shift type 5.
    Type5,
    /// Beat shift type 6.
    Type6,
    /// Beat shift type 7.
    Type7,
    /// Beat shift type 8.
    Type8,
}

// ---------------------------------------------------------------------------
// System settings
// ---------------------------------------------------------------------------

/// System-wide radio settings.
///
/// Covers global configuration such as power management, key lock,
/// display units, language, and programmable function keys.
/// Derived from capability gap analysis features 170-197.
///
/// # USB charging (per Operating Tips §5.1)
///
/// The TH-D75 charges via USB but does not support USB Power Delivery
/// (PD). It always draws 5V from USB; an internal DC-DC converter
/// boosts this to 7.4V for the battery. Two charging current modes:
/// - 1.5A: approximately 5.5 hours to full charge
/// - 0.5A: approximately 13 hours to full charge
///
/// **Power must be off during charging.** Menu No. 923 can disable
/// charging at power-on to prevent unintended charge sessions.
///
/// # Battery saver (per Operating Tips §5.1)
///
/// Menu No. 920 controls the battery saver, which cycles the receiver
/// on and off to reduce power consumption. In DV/DR mode, the off
/// duration is fixed at 200 ms regardless of the configured value.
/// Battery saver is automatically disabled when APRS or KISS mode
/// is active.
///
/// # Auto Power Off (per Operating Tips §5.1)
///
/// Menu No. 921 controls Auto Power Off. Default is 30 minutes.
/// The radio powers off automatically after the configured period
/// of inactivity.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Maps 1:1 to the D75 System menu tree; each bool is a discrete on/off menu item \
              (battery saver, key lock, PC output NMEA, GPS on). Collapsing to bitflags would \
              lose the per-field User Manual Menu-number documentation."
)]
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
    ///
    /// Menu No. 111. Per User Manual Chapter 12: available values are
    /// 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5, 4.0, 4.5, 5.0, and 10.0
    /// minutes. Default: 10.0 minutes. This function cannot be turned
    /// off entirely -- it protects the transceiver from thermal damage.
    /// A warning beep sounds just before TX is cut off. After timeout,
    /// the transceiver beeps even if beep is disabled.
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

/// Auto power off timer duration (Menu No. 921).
///
/// Per User Manual Chapter 12: after the time limit with no operations,
/// APO turns the power off. One minute before power-off, "APO" blinks
/// on the display and a warning tone sounds (even if beep is disabled).
/// APO does not operate during scanning.
///
/// The User Manual menu table lists options: Off / 15 / 30 / 60 minutes
/// (default: 30), and the MCP-D75 serializer (`radio.AutoPowerOff`,
/// raw 0-3) agrees exactly. Earlier revisions of this enum invented
/// 90/120-minute variants that do not exist on the D75.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutoPowerOff {
    /// Auto power off disabled.
    Off,
    /// Power off after 15 minutes of inactivity.
    Min15,
    /// Power off after 30 minutes of inactivity.
    Min30,
    /// Power off after 60 minutes of inactivity.
    Min60,
}

/// Key lock type -- which controls are affected by key lock (Menu No. 960).
///
/// Per User Manual Chapter 12: key lock is toggled by pressing and
/// holding `[F]`. The `[MONI]`, `[PTT]`, `[Power]`, and `[VOL]`
/// controls can never be locked.
///
/// The User Manual lists options as `Key Lock` and/or `Frequency Lock`
/// (checkboxes), with different combined behaviors:
/// - Key Lock only: locks all front panel keys.
/// - Frequency Lock only: locks frequency/channel controls.
/// - Both: locks all keys and the encoder control.
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

/// Language selection (Menu No. 990).
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
/// The TH-D75 has 2 front-panel PF keys (Menu No. 940/941) and 3
/// microphone PF keys (Menu No. 942/943/944), each assignable to one
/// of these functions.
///
/// Per User Manual Chapter 12: the microphone PF keys support a larger
/// set of functions than the front-panel keys, including MODE, MENU,
/// A/B, VFO, MR, CALL, MSG, LIST, BCON, REV, TONE, MHz, MARK, DUAL,
/// APRS, OBJ, ATT, FINE, POS, BAND, MONI, UP, DOWN, and Screen Capture.
/// Front-panel PF keys additionally support M.IN (memory registration).
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
        if value & !0x03 == 0 {
            Ok(Self {
                receive: value & 0x01 != 0,
                fm_radio: value & 0x02 != 0,
            })
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "LED control",
                value,
                detail: "must contain only RX (0x01) and FM Radio (0x02) bits",
            })
        }
    }
}

impl From<LedControl> for u8 {
    fn from(value: LedControl) -> Self {
        Self::from(value.receive) | (Self::from(value.fm_radio) << 1)
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

impl TryFrom<u8> for BeatShift {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Type1),
            1 => Ok(Self::Type2),
            2 => Ok(Self::Type3),
            3 => Ok(Self::Type4),
            4 => Ok(Self::Type5),
            5 => Ok(Self::Type6),
            6 => Ok(Self::Type7),
            7 => Ok(Self::Type8),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "beat shift",
                value,
                detail: "must be 0-7",
            }),
        }
    }
}

impl From<BeatShift> for u8 {
    fn from(bs: BeatShift) -> Self {
        bs as Self
    }
}

impl TryFrom<u8> for AutoPowerOff {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Off),
            1 => Ok(Self::Min15),
            2 => Ok(Self::Min30),
            3 => Ok(Self::Min60),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "auto power off",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<AutoPowerOff> for u8 {
    fn from(apo: AutoPowerOff) -> Self {
        apo as Self
    }
}

impl KeyLockType {
    /// Number of valid key lock type values (0-2).
    pub const COUNT: u8 = 3;
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

impl From<KeyLockType> for u8 {
    fn from(klt: KeyLockType) -> Self {
        klt as Self
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
        assert_eq!(ds.led_control, LedControl::new(true, false));
        assert_eq!(ds.led_control, LedControl::default());
    }

    #[test]
    fn audio_settings_default() {
        let a = AudioSettings::default();
        assert!(a.beep);
        assert_eq!(a.beep_volume, 4);
        assert_eq!(a.mic_sensitivity, MicSensitivity::Medium);
        assert_eq!(a.tx_equalizer, TxEqualizer::default());
        assert_eq!(a.rx_equalizer, RxEqualizer::default());
    }

    #[test]
    fn led_control_preserves_independent_bits() -> Result<(), ValidationError> {
        for raw in 0..=3 {
            let controls = LedControl::try_from(raw)?;
            assert_eq!(u8::from(controls), raw);
        }
        assert_eq!(LedControl::try_from(1)?, LedControl::new(true, false));
        assert_eq!(LedControl::try_from(2)?, LedControl::new(false, true));
        assert!(LedControl::try_from(4).is_err());
        Ok(())
    }

    #[test]
    fn tx_eq_level_validates_and_round_trips_raw() -> Result<(), Box<dyn std::error::Error>> {
        for db in TxEqLevel::MIN_DB..=TxEqLevel::MAX_DB {
            let level = TxEqLevel::new(db).ok_or("in-range TX EQ level rejected")?;
            assert_eq!(TxEqLevel::from_raw(level.as_raw()), Some(level));
            assert_eq!(TxEqLevel::try_from(u8::from(level))?, level);
            assert_eq!(level.as_db(), db);
        }
        assert!(TxEqLevel::new(-10).is_none());
        assert!(TxEqLevel::new(4).is_none());
        assert!(TxEqLevel::from_raw(13).is_none());
        assert!(TxEqLevel::try_from(13).is_err());
        Ok(())
    }

    #[test]
    fn rx_eq_level_validates_and_round_trips_raw() -> Result<(), Box<dyn std::error::Error>> {
        for db in RxEqLevel::MIN_DB..=RxEqLevel::MAX_DB {
            let level = RxEqLevel::new(db).ok_or("in-range RX EQ level rejected")?;
            assert_eq!(RxEqLevel::from_raw(level.as_raw()), Some(level));
            assert_eq!(RxEqLevel::try_from(u8::from(level))?, level);
            assert_eq!(level.as_db(), db);
        }
        assert!(RxEqLevel::new(-10).is_none());
        assert!(RxEqLevel::new(10).is_none());
        assert!(RxEqLevel::from_raw(19).is_none());
        assert!(RxEqLevel::try_from(19).is_err());
        Ok(())
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
    fn power_on_message_valid() -> Result<(), Box<dyn std::error::Error>> {
        let msg = PowerOnMessage::new("TH-D75 Ready").ok_or("valid message rejected")?;
        assert_eq!(msg.as_str(), "TH-D75 Ready");
        Ok(())
    }

    #[test]
    fn power_on_message_max_length() -> Result<(), Box<dyn std::error::Error>> {
        let msg =
            PowerOnMessage::new("1234567890123456").ok_or("valid 16-char message rejected")?;
        assert_eq!(msg.as_str().len(), 16);
        Ok(())
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
