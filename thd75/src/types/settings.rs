//! Radio-wide system, audio, and display settings for the TH-D75.
//!
//! These types cover the radio's global configuration accessible through
//! the Configuration, Audio, and Display menu groups. Aggregate structs use
//! the exact scalar domains in the MCP-D75 menu registry; they deliberately
//! omit action rows, read-only information, blobs, and list editors.

use crate::error::ValidationError;
use crate::types::gps::{CoordinateFormat, GridSquareFormat};

// ---------------------------------------------------------------------------
// Display settings
// ---------------------------------------------------------------------------

/// Display and illumination settings.
///
/// Exact editable values in Configuration > Display (Menus No. 900-907).
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
    /// Backlight auto-off timer.
    pub backlight_timer: BacklightTimer,
    /// LCD brightness.
    pub lcd_brightness: LcdBrightness,
    /// Background color theme.
    pub background_color: BackgroundColor,
    /// Power-on message displayed at startup (up to 16 characters).
    pub power_on_message: PowerOnMessage,
    /// Information shown in single-band mode.
    pub single_band_display: SingleBandDisplay,
    /// S-meter and power meter display type.
    pub meter_type: MeterType,
    /// Backlight behavior for APRS/D-STAR notifications and scan pauses.
    pub information_backlight: InformationBacklight,
}

/// LCD backlight control mode (Menu No. 900).
///
/// Per User Manual Chapter 12: temporary lighting can also be triggered
/// by pressing `[Power]`, which illuminates the display and keys for the
/// timer duration (Menu No. 901). Pressing `[Power]` while lit turns
/// the light off immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BacklightControl {
    /// Backlight is toggled manually with the assigned backlight key.
    Manual = 0,
    /// Backlight always on while power is on.
    On = 1,
    /// Backlight auto (turns on with key press or encoder rotation,
    /// off after the timer in Menu No. 901 expires). Also lights on
    /// APRS interrupt reception and scan pause/stop.
    Auto = 2,
    /// Automatic on battery and continuously on while DC input is present.
    AutoDcIn = 3,
}

impl BacklightControl {
    /// Number of CAT/MCP values accepted by Menu No. 900 (`0..=3`).
    pub const COUNT: u8 = 4;

    /// Every backlight control mode, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Manual, Self::On, Self::Auto, Self::AutoDcIn];
}

impl std::fmt::Display for BacklightControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Manual => "Manual",
            Self::On => "On",
            Self::Auto => "Auto",
            Self::AutoDcIn => "Auto (DC-IN)",
        })
    }
}

/// LCD brightness (Menu No. 902, `radio.LcdBrightness`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum LcdBrightness {
    /// Low brightness.
    Low = 0,
    /// Medium brightness.
    Medium = 1,
    /// High brightness.
    High = 2,
}

impl LcdBrightness {
    /// Number of LCD brightness choices (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every LCD brightness choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Low, Self::Medium, Self::High];
}

impl std::fmt::Display for LcdBrightness {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        })
    }
}

/// Text displayed for approximately two seconds when the TH-D75 powers on.
///
/// Menu No. 903 and the corresponding MCP field share one exact domain: zero
/// to sixteen printable ASCII bytes (`0x20`-`0x7E`). Spaces are data,
/// including leading and trailing spaces, and are never trimmed. The memory
/// image stores shorter messages with trailing NUL padding, which is not part
/// of the semantic text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PowerOnMessage(String);

impl PowerOnMessage {
    /// Maximum encoded message length.
    pub const MAX_LEN: usize = 16;

    /// Width of the NUL-padded MCP memory-image field.
    pub const WIRE_LEN: usize = 16;

    /// Construct a power-on message from user-visible text.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::PowerOnMessageTooLong`] when `text` exceeds
    /// sixteen encoded bytes. Returns
    /// [`ValidationError::InvalidPowerOnMessageByte`] at the first byte
    /// outside printable ASCII (`0x20`-`0x7E`).
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        if text.len() > Self::MAX_LEN {
            return Err(ValidationError::PowerOnMessageTooLong { len: text.len() });
        }

        if let Some((offset, value)) = text
            .bytes()
            .enumerate()
            .find(|(_, value)| !is_printable_ascii(*value))
        {
            return Err(ValidationError::InvalidPowerOnMessageByte { offset, value });
        }

        Ok(Self(text.to_owned()))
    }

    /// Decode the exact 16-byte MCP memory-image field.
    ///
    /// A full-width message needs no terminator. A shorter message must be
    /// followed exclusively by NUL padding.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::PowerOnMessageDataAfterNul`] for nonzero
    /// data after the first NUL. Returns
    /// [`ValidationError::InvalidPowerOnMessageByte`] for a semantic byte
    /// outside printable ASCII.
    pub fn try_from_wire(bytes: [u8; Self::WIRE_LEN]) -> Result<Self, ValidationError> {
        let text_len = bytes
            .iter()
            .position(|&value| value == 0)
            .unwrap_or(Self::WIRE_LEN);

        if let Some((offset, &value)) = bytes
            .iter()
            .enumerate()
            .skip(text_len.saturating_add(1))
            .find(|(_, value)| **value != 0)
        {
            return Err(ValidationError::PowerOnMessageDataAfterNul {
                terminator_offset: text_len,
                offset,
                value,
            });
        }

        if let Some((offset, &value)) = bytes
            .iter()
            .take(text_len)
            .enumerate()
            .find(|(_, value)| !is_printable_ascii(**value))
        {
            return Err(ValidationError::InvalidPowerOnMessageByte { offset, value });
        }

        let text = bytes
            .iter()
            .take(text_len)
            .map(|&value| char::from(value))
            .collect();
        Ok(Self(text))
    }

    /// Encode the exact 16-byte, NUL-padded MCP memory-image field.
    #[must_use]
    pub fn to_wire_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut bytes = [0; Self::WIRE_LEN];
        bytes
            .iter_mut()
            .zip(self.0.bytes())
            .for_each(|(destination, source)| *destination = source);
        bytes
    }

    /// Return the message text without trimming spaces.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return `true` when the message contains no bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return the encoded message length in bytes.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
}

impl TryFrom<&str> for PowerOnMessage {
    type Error = ValidationError;

    fn try_from(text: &str) -> Result<Self, Self::Error> {
        Self::new(text)
    }
}

impl std::fmt::Display for PowerOnMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for PowerOnMessage {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

const fn is_printable_ascii(value: u8) -> bool {
    value == b' ' || value.is_ascii_graphic()
}

/// Information shown in single-band mode (Menu No. 904).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SingleBandDisplay {
    /// Do not show supplemental information.
    Off = 0,
    /// Show GPS altitude.
    GpsAltitude = 1,
    /// Show GPS ground speed.
    GpsGroundSpeed = 2,
    /// Show the date.
    Date = 3,
    /// Show the demodulation mode.
    DemodulationMode = 4,
}

impl SingleBandDisplay {
    /// Number of single-band display choices (raw `0..=4`).
    pub const COUNT: u8 = 5;

    /// Every single-band display choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Off,
        Self::GpsAltitude,
        Self::GpsGroundSpeed,
        Self::Date,
        Self::DemodulationMode,
    ];
}

impl std::fmt::Display for SingleBandDisplay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::GpsAltitude => "GPS (Altitude)",
            Self::GpsGroundSpeed => "GPS (GS)",
            Self::Date => "Date",
            Self::DemodulationMode => "Demodulation Mode",
        })
    }
}

/// S-meter and power-meter design (Menu No. 905).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MeterType {
    /// Meter design Type 1.
    Type1 = 0,
    /// Meter design Type 2.
    Type2 = 1,
    /// Meter design Type 3.
    Type3 = 2,
}

impl MeterType {
    /// Number of meter designs (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every meter design, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Type1, Self::Type2, Self::Type3];
}

impl std::fmt::Display for MeterType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Type1 => "Type1",
            Self::Type2 => "Type2",
            Self::Type3 => "Type3",
        })
    }
}

/// LCD background color (Menu No. 906).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BackgroundColor {
    /// Black background.
    Black = 0,
    /// White background.
    White = 1,
}

impl BackgroundColor {
    /// Number of background color choices (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every background color choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Black, Self::White];
}

impl std::fmt::Display for BackgroundColor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Black => "Black",
            Self::White => "White",
        })
    }
}

/// Notification backlight behavior (Menu No. 907).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InformationBacklight {
    /// Do not illuminate for notifications.
    Off = 0,
    /// Illuminate only the LCD.
    Lcd = 1,
    /// Illuminate the LCD and keys.
    LcdAndKeys = 2,
}

impl InformationBacklight {
    /// Number of notification backlight choices (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every notification backlight choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Off, Self::Lcd, Self::LcdAndKeys];
}

impl std::fmt::Display for InformationBacklight {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Lcd => "LCD",
            Self::LcdAndKeys => "LCD + Key",
        })
    }
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

// ---------------------------------------------------------------------------
// Audio settings
// ---------------------------------------------------------------------------

/// Exact editable values in Configuration > Audio (Menus No. 910-91A).
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
    /// Balance between Band A and Band B.
    pub balance: AudioBalance,
    /// TX equalizer enables and four independently adjustable bands.
    pub tx_equalizer: TxEqualizer,
    /// RX equalizer enable and five independently adjustable bands.
    pub rx_equalizer: RxEqualizer,
    /// Key beep on/off.
    pub beep: bool,
    /// Key-beep volume or main-volume link.
    pub beep_volume: LinkedVolumeLevel,
    /// Voice announcement behavior.
    pub voice_announce: VoiceAnnounceMode,
    /// Voice-guidance volume or main-volume link.
    pub voice_announce_volume: LinkedVolumeLevel,
    /// Voice guidance speed.
    pub voice_guidance_speed: VoiceGuideSpeed,
    /// Callsign pronunciation method.
    pub callsign_readout: CallsignReadout,
    /// USB audio output level.
    pub usb_audio_output_level: UsbAudioOutputLevel,
}

/// Audio balance between Band A and Band B (Menu No. 910).
///
/// These are ten discrete menu choices, not a percentage slider. In choices
/// 0-4 Band A remains at 100 while Band B rises; in choices 5-8 Band B
/// remains at 100 while Band A falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AudioBalance {
    /// A:100 / B:0.
    A100B0 = 0,
    /// A:100 / B:25.
    A100B25 = 1,
    /// A:100 / B:50.
    A100B50 = 2,
    /// A:100 / B:75.
    A100B75 = 3,
    /// A:100 / B:100.
    A100B100 = 4,
    /// A:75 / B:100.
    A75B100 = 5,
    /// A:50 / B:100.
    A50B100 = 6,
    /// A:25 / B:100.
    A25B100 = 7,
    /// A:0 / B:100.
    A0B100 = 8,
    /// Output only the current operation band.
    OperationBandOnly = 9,
}

impl AudioBalance {
    /// Number of audio balance choices (raw `0..=9`).
    pub const COUNT: u8 = 10;

    /// Every audio balance choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::A100B0,
        Self::A100B25,
        Self::A100B50,
        Self::A100B75,
        Self::A100B100,
        Self::A75B100,
        Self::A50B100,
        Self::A25B100,
        Self::A0B100,
        Self::OperationBandOnly,
    ];
}

impl std::fmt::Display for AudioBalance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::A100B0 => "A:100/B:0",
            Self::A100B25 => "A:100/B:25",
            Self::A100B50 => "A:100/B:50",
            Self::A100B75 => "A:100/B:75",
            Self::A100B100 => "A:100/B:100",
            Self::A75B100 => "A:75/B:100",
            Self::A50B100 => "A:50/B:100",
            Self::A25B100 => "A:25/B:100",
            Self::A0B100 => "A:0/B:100",
            Self::OperationBandOnly => "Operation Band Only",
        })
    }
}

/// Callsign pronunciation method (Menu No. 919).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CallsignReadout {
    /// Read letters normally.
    Standard = 0,
    /// Read the complete callsign phonetically.
    FullPhonetics = 1,
    /// Read only the suffix phonetically.
    SuffixPhonetics = 2,
}

impl CallsignReadout {
    /// Number of callsign readout choices (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every callsign readout choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Standard, Self::FullPhonetics, Self::SuffixPhonetics];
}

impl std::fmt::Display for CallsignReadout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Standard => "Standard",
            Self::FullPhonetics => "Phonetics (Full)",
            Self::SuffixPhonetics => "Phonetics (Suffix)",
        })
    }
}

/// USB audio capture level (Menu No. 91A).
///
/// The stored value is zero-based even though the radio labels the choices
/// Level 1 through Level 7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UsbAudioOutputLevel {
    /// Level 1.
    Level1 = 0,
    /// Level 2.
    Level2 = 1,
    /// Level 3.
    Level3 = 2,
    /// Level 4.
    Level4 = 3,
    /// Level 5.
    Level5 = 4,
    /// Level 6.
    Level6 = 5,
    /// Level 7.
    Level7 = 6,
}

impl UsbAudioOutputLevel {
    /// Number of USB audio output levels (raw `0..=6`).
    pub const COUNT: u8 = 7;

    /// Every USB audio output level, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Level1,
        Self::Level2,
        Self::Level3,
        Self::Level4,
        Self::Level5,
        Self::Level6,
        Self::Level7,
    ];
}

impl std::fmt::Display for UsbAudioOutputLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Level1 => "Level 1",
            Self::Level2 => "Level 2",
            Self::Level3 => "Level 3",
            Self::Level4 => "Level 4",
            Self::Level5 => "Level 5",
            Self::Level6 => "Level 6",
            Self::Level7 => "Level 7",
        })
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

    /// Creates a TX equalizer level.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `db` is in
    /// `-9..=3`.
    pub const fn new(db: i8) -> Result<Self, ValidationError> {
        if db >= Self::MIN_DB && db <= Self::MAX_DB {
            Ok(Self(db))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "TX EQ level",
                value: db as i64,
                detail: "must be -9 through +3 dB",
            })
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

    /// Creates an RX equalizer level.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::IntegerOutOfRange`] unless `db` is in
    /// `-9..=9`.
    pub const fn new(db: i8) -> Result<Self, ValidationError> {
        if db >= Self::MIN_DB && db <= Self::MAX_DB {
            Ok(Self(db))
        } else {
            Err(ValidationError::IntegerOutOfRange {
                name: "RX EQ level",
                value: db as i64,
                detail: "must be -9 through +9 dB",
            })
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
#[repr(u8)]
pub enum MicSensitivity {
    /// High sensitivity.
    High = 0,
    /// Medium sensitivity (default).
    Medium = 1,
    /// Low sensitivity.
    Low = 2,
}

impl MicSensitivity {
    /// Number of microphone sensitivity choices (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every microphone sensitivity choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::High, Self::Medium, Self::Low];
}

impl std::fmt::Display for MicSensitivity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
        })
    }
}

/// Voice guidance speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VoiceGuideSpeed {
    /// Voice guidance speed 1.
    Speed1 = 0,
    /// Voice guidance speed 2.
    Speed2 = 1,
    /// Voice guidance speed 3.
    Speed3 = 2,
    /// Voice guidance speed 4.
    Speed4 = 3,
}

impl VoiceGuideSpeed {
    /// Number of voice guidance speeds (raw `0..=3`).
    pub const COUNT: u8 = 4;

    /// Every voice guidance speed, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Speed1, Self::Speed2, Self::Speed3, Self::Speed4];
}

impl std::fmt::Display for VoiceGuideSpeed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Speed1 => "Speed 1",
            Self::Speed2 => "Speed 2",
            Self::Speed3 => "Speed 3",
            Self::Speed4 => "Speed 4",
        })
    }
}

/// Receiver beat-shift type (`radio.BeatShift`, raw 0-7).
///
/// The D75 offers eight beat-shift types (Type 1 through Type 8) that
/// move internally generated spurious beats out of the receive
/// passband. This is a type selector, not an on/off switch: the
/// MCP-D75 serializer stores one byte with domain 0-7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BeatShift {
    /// Beat shift type 1.
    Type1 = 0,
    /// Beat shift type 2.
    Type2 = 1,
    /// Beat shift type 3.
    Type3 = 2,
    /// Beat shift type 4.
    Type4 = 3,
    /// Beat shift type 5.
    Type5 = 4,
    /// Beat shift type 6.
    Type6 = 5,
    /// Beat shift type 7.
    Type7 = 6,
    /// Beat shift type 8.
    Type8 = 7,
}

impl BeatShift {
    /// Number of beat-shift types (raw `0..=7`).
    pub const COUNT: u8 = 8;

    /// Every beat-shift type, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Type1,
        Self::Type2,
        Self::Type3,
        Self::Type4,
        Self::Type5,
        Self::Type6,
        Self::Type7,
        Self::Type8,
    ];
}

impl std::fmt::Display for BeatShift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Type1 => "Type1",
            Self::Type2 => "Type2",
            Self::Type3 => "Type3",
            Self::Type4 => "Type4",
            Self::Type5 => "Type5",
            Self::Type6 => "Type6",
            Self::Type7 => "Type7",
            Self::Type8 => "Type8",
        })
    }
}

/// Transmit time-out timer selection (`radio.TimeOutTimer`).
///
/// Each variant is one exact menu selection. The stored byte is an index,
/// not a duration in a fixed unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TransmitTimeout {
    /// 30 seconds.
    Seconds30 = 0,
    /// 60 seconds.
    Seconds60 = 1,
    /// 90 seconds.
    Seconds90 = 2,
    /// 120 seconds.
    Seconds120 = 3,
    /// 150 seconds.
    Seconds150 = 4,
    /// 180 seconds.
    Seconds180 = 5,
    /// 210 seconds.
    Seconds210 = 6,
    /// 240 seconds.
    Seconds240 = 7,
    /// 270 seconds.
    Seconds270 = 8,
    /// 300 seconds.
    Seconds300 = 9,
    /// 600 seconds.
    Seconds600 = 10,
}

impl TransmitTimeout {
    /// Number of transmit time-out selections (raw `0..=10`).
    pub const COUNT: u8 = 11;

    /// Every transmit time-out selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Seconds30,
        Self::Seconds60,
        Self::Seconds90,
        Self::Seconds120,
        Self::Seconds150,
        Self::Seconds180,
        Self::Seconds210,
        Self::Seconds240,
        Self::Seconds270,
        Self::Seconds300,
        Self::Seconds600,
    ];

    /// Returns the selected transmit limit in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u16 {
        match self {
            Self::Seconds30 => 30,
            Self::Seconds60 => 60,
            Self::Seconds90 => 90,
            Self::Seconds120 => 120,
            Self::Seconds150 => 150,
            Self::Seconds180 => 180,
            Self::Seconds210 => 210,
            Self::Seconds240 => 240,
            Self::Seconds270 => 270,
            Self::Seconds300 => 300,
            Self::Seconds600 => 600,
        }
    }
}

impl std::fmt::Display for TransmitTimeout {
    /// Renders the menu label, which the radio states in minutes.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Seconds30 => "0.5 min",
            Self::Seconds60 => "1.0 min",
            Self::Seconds90 => "1.5 min",
            Self::Seconds120 => "2.0 min",
            Self::Seconds150 => "2.5 min",
            Self::Seconds180 => "3.0 min",
            Self::Seconds210 => "3.5 min",
            Self::Seconds240 => "4.0 min",
            Self::Seconds270 => "4.5 min",
            Self::Seconds300 => "5.0 min",
            Self::Seconds600 => "10.0 min",
        })
    }
}

/// SSB receive high-cut filter selection (`radio.SsbHighCut`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SsbHighCut {
    /// 2.2 kHz.
    Khz2_2 = 0,
    /// 2.4 kHz.
    Khz2_4 = 1,
    /// 2.6 kHz.
    Khz2_6 = 2,
    /// 2.8 kHz.
    Khz2_8 = 3,
    /// 3.0 kHz.
    Khz3_0 = 4,
}

impl SsbHighCut {
    /// Number of SSB high-cut selections (raw `0..=4`).
    pub const COUNT: u8 = 5;

    /// Every SSB high-cut selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Khz2_2,
        Self::Khz2_4,
        Self::Khz2_6,
        Self::Khz2_8,
        Self::Khz3_0,
    ];
}

impl std::fmt::Display for SsbHighCut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Khz2_2 => "2.2 kHz",
            Self::Khz2_4 => "2.4 kHz",
            Self::Khz2_6 => "2.6 kHz",
            Self::Khz2_8 => "2.8 kHz",
            Self::Khz3_0 => "3.0 kHz",
        })
    }
}

/// CW receive filter width selection (`radio.CwWidth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CwFilterWidth {
    /// 0.3 kHz.
    Khz0_3 = 0,
    /// 0.5 kHz.
    Khz0_5 = 1,
    /// 1.0 kHz.
    Khz1_0 = 2,
    /// 1.5 kHz.
    Khz1_5 = 3,
    /// 2.0 kHz.
    Khz2_0 = 4,
}

impl CwFilterWidth {
    /// Number of CW filter width selections (raw `0..=4`).
    pub const COUNT: u8 = 5;

    /// Every CW filter width selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Khz0_3,
        Self::Khz0_5,
        Self::Khz1_0,
        Self::Khz1_5,
        Self::Khz2_0,
    ];
}

impl std::fmt::Display for CwFilterWidth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Khz0_3 => "0.3 kHz",
            Self::Khz0_5 => "0.5 kHz",
            Self::Khz1_0 => "1.0 kHz",
            Self::Khz1_5 => "1.5 kHz",
            Self::Khz2_0 => "2.0 kHz",
        })
    }
}

/// AM receive high-cut filter selection (`radio.AmHighCut`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AmHighCut {
    /// 3.0 kHz.
    Khz3_0 = 0,
    /// 4.5 kHz.
    Khz4_5 = 1,
    /// 6.0 kHz.
    Khz6_0 = 2,
    /// 7.5 kHz.
    Khz7_5 = 3,
}

impl AmHighCut {
    /// Number of AM high-cut selections (raw `0..=3`).
    pub const COUNT: u8 = 4;

    /// Every AM high-cut selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Khz3_0, Self::Khz4_5, Self::Khz6_0, Self::Khz7_5];
}

impl std::fmt::Display for AmHighCut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Khz3_0 => "3.0 kHz",
            Self::Khz4_5 => "4.5 kHz",
            Self::Khz6_0 => "6.0 kHz",
            Self::Khz7_5 => "7.5 kHz",
        })
    }
}

/// Scan restart delay from 1 through 10 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScanRestartDelay(u8);

impl ScanRestartDelay {
    /// Creates a scan restart delay.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `seconds` is in
    /// `1..=10`.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds >= 1 && seconds <= 10 {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "scan restart delay",
                value: seconds,
                detail: "must be 1-10 seconds",
            })
        }
    }

    /// Returns the delay in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

/// Function assigned to the repeater CALL key (`radio.CallKey`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RepeaterCallKey {
    /// Recall the CALL channel.
    CallChannel = 0,
    /// Transmit a 1,750 Hz tone.
    Tone1750Hz = 1,
}

impl RepeaterCallKey {
    /// Number of repeater CALL key functions (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every repeater CALL key function, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::CallChannel, Self::Tone1750Hz];
}

impl std::fmt::Display for RepeaterCallKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CallChannel => "CALL",
            Self::Tone1750Hz => "1,750 [Hz]",
        })
    }
}

/// DTMF tone duration selection (`radio.DtmfSpeed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DtmfToneDuration {
    /// 50 ms per digit.
    Ms50 = 0,
    /// 100 ms per digit.
    Ms100 = 1,
    /// 150 ms per digit.
    Ms150 = 2,
}

impl DtmfToneDuration {
    /// Number of DTMF tone duration selections (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every DTMF tone duration selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Ms50, Self::Ms100, Self::Ms150];

    /// Returns the duration of each DTMF tone in milliseconds.
    #[must_use]
    pub const fn as_milliseconds(self) -> u16 {
        match self {
            Self::Ms50 => 50,
            Self::Ms100 => 100,
            Self::Ms150 => 150,
        }
    }
}

impl std::fmt::Display for DtmfToneDuration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Ms50 => "50 ms",
            Self::Ms100 => "100 ms",
            Self::Ms150 => "150 ms",
        })
    }
}

/// Automatic mute return delay from 1 through 10 seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AutoMuteReturnDelay(u8);

impl AutoMuteReturnDelay {
    /// Creates a return delay.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `seconds` is in
    /// `1..=10`.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds >= 1 && seconds <= 10 {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "automatic mute return delay",
                value: seconds,
                detail: "must be 1-10 seconds",
            })
        }
    }

    /// Returns the delay in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

/// Backlight timer from 3 through 60 seconds (`radio.BacklightTimer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BacklightTimer(u8);

impl BacklightTimer {
    /// Creates a backlight timer.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `seconds` is in
    /// `3..=60`.
    pub const fn new(seconds: u8) -> Result<Self, ValidationError> {
        if seconds >= 3 && seconds <= 60 {
            Ok(Self(seconds))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "backlight timer",
                value: seconds,
                detail: "must be 3-60 seconds",
            })
        }
    }

    /// Returns the timer duration in seconds.
    #[must_use]
    pub const fn as_seconds(self) -> u8 {
        self.0
    }
}

/// A volume selection that may follow the main volume or use Level 1-7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkedVolumeLevel(u8);

impl LinkedVolumeLevel {
    /// Follow the radio's main volume control.
    pub const VOLUME_LINK: Self = Self(0);

    /// Creates a fixed Level 1-7 selection.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::SettingOutOfRange`] unless `level` is in
    /// `1..=7`.
    pub const fn fixed(level: u8) -> Result<Self, ValidationError> {
        if level >= 1 && level <= 7 {
            Ok(Self(level))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "linked volume fixed level",
                value: level,
                detail: "must be 1-7",
            })
        }
    }

    /// Returns the fixed level, or `None` when linked to main volume.
    #[must_use]
    pub const fn fixed_level(self) -> Option<u8> {
        if self.0 == 0 { None } else { Some(self.0) }
    }

    /// Returns the MCP representation, where zero means main-volume link.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// Voice announcement mode (`radio.VoiceAnnounce`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum VoiceAnnounceMode {
    /// Voice announcements disabled.
    Off = 0,
    /// Announce only when requested manually.
    Manual = 1,
    /// Automatic announcement mode 1.
    Auto1 = 2,
    /// Automatic announcement mode 2.
    Auto2 = 3,
}

impl VoiceAnnounceMode {
    /// Number of voice announcement modes (raw `0..=3`).
    pub const COUNT: u8 = 4;

    /// Every voice announcement mode, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Off, Self::Manual, Self::Auto1, Self::Auto2];
}

impl std::fmt::Display for VoiceAnnounceMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Manual => "Manual",
            Self::Auto1 => "Auto1",
            Self::Auto2 => "Auto2",
        })
    }
}

/// Battery saver interval (`radio.BatterySaver`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BatterySaverInterval {
    /// Battery saver disabled.
    Off = 0,
    /// 0.2 seconds.
    Seconds0_2 = 1,
    /// 0.4 seconds.
    Seconds0_4 = 2,
    /// 0.6 seconds.
    Seconds0_6 = 3,
    /// 0.8 seconds.
    Seconds0_8 = 4,
    /// 1.0 seconds.
    Seconds1 = 5,
    /// 2.0 seconds.
    Seconds2 = 6,
    /// 3.0 seconds.
    Seconds3 = 7,
    /// 4.0 seconds.
    Seconds4 = 8,
    /// 5.0 seconds.
    Seconds5 = 9,
}

impl BatterySaverInterval {
    /// Number of battery saver selections (raw `0..=9`).
    pub const COUNT: u8 = 10;

    /// Every battery saver selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Off,
        Self::Seconds0_2,
        Self::Seconds0_4,
        Self::Seconds0_6,
        Self::Seconds0_8,
        Self::Seconds1,
        Self::Seconds2,
        Self::Seconds3,
        Self::Seconds4,
        Self::Seconds5,
    ];
}

impl std::fmt::Display for BatterySaverInterval {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Seconds0_2 => "0.2 s",
            Self::Seconds0_4 => "0.4 s",
            Self::Seconds0_6 => "0.6 s",
            Self::Seconds0_8 => "0.8 s",
            Self::Seconds1 => "1.0 s",
            Self::Seconds2 => "2.0 s",
            Self::Seconds3 => "3.0 s",
            Self::Seconds4 => "4.0 s",
            Self::Seconds5 => "5.0 s",
        })
    }
}

/// Host interface selected for GPS or APRS PC output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PcOutputInterface {
    /// USB CDC interface.
    Usb = 0,
    /// Bluetooth serial interface.
    Bluetooth = 1,
}

impl PcOutputInterface {
    /// Number of PC output interface choices (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every PC output interface choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Usb, Self::Bluetooth];
}

impl std::fmt::Display for PcOutputInterface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Usb => "USB",
            Self::Bluetooth => "Bluetooth",
        })
    }
}

/// One official front-panel PF key function.
///
/// The discriminants match the gapped `radio.Pf1PfKey` and
/// `radio.Pf2PfKey` menu domain exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum FrontPanelPfFunction {
    /// Recording.
    Recording = 0,
    /// Voice message 1.
    VoiceMessage1 = 1,
    /// Voice message 2.
    VoiceMessage2 = 2,
    /// Voice message 3.
    VoiceMessage3 = 3,
    /// Voice message 4.
    VoiceMessage4 = 4,
    /// Voice guidance.
    VoiceGuidance = 6,
    /// Battery level announcement.
    BatteryLevel = 7,
    /// VOX.
    Vox = 8,
    /// Group name.
    GroupName = 9,
    /// Audio balance.
    Balance = 10,
    /// GPS.
    Gps = 11,
    /// Track log.
    TrackLog = 12,
    /// Squelch.
    Squelch = 13,
    /// Repeater shift.
    Shift = 14,
    /// Tuning step.
    Step = 15,
    /// Transmit power.
    Power = 16,
    /// Key lock.
    KeyLock = 17,
    /// Memory lockout.
    Lockout = 18,
    /// Memory-to-VFO transfer.
    MemoryToVfo = 19,
    /// Tone selection.
    ToneSelect = 20,
    /// New memory entry.
    NewMemory = 21,
    /// Voice alert.
    VoiceAlert = 22,
    /// LCD brightness.
    LcdBrightness = 24,
    /// DTMF memory channel 0.
    DtmfChannel0 = 27,
    /// `EchoLink` memory channel 0.
    EcholinkChannel0 = 28,
    /// 1,750 Hz tone.
    Tone1750Hz = 29,
    /// Memory registration.
    MemoryInput = 30,
}

impl FrontPanelPfFunction {
    /// Number of official front-panel PF selections in the gapped raw
    /// domain (`0..=30`).
    pub const COUNT: u8 = 27;

    /// Every official front-panel PF function, in ascending raw-value
    /// (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Recording,
        Self::VoiceMessage1,
        Self::VoiceMessage2,
        Self::VoiceMessage3,
        Self::VoiceMessage4,
        Self::VoiceGuidance,
        Self::BatteryLevel,
        Self::Vox,
        Self::GroupName,
        Self::Balance,
        Self::Gps,
        Self::TrackLog,
        Self::Squelch,
        Self::Shift,
        Self::Step,
        Self::Power,
        Self::KeyLock,
        Self::Lockout,
        Self::MemoryToVfo,
        Self::ToneSelect,
        Self::NewMemory,
        Self::VoiceAlert,
        Self::LcdBrightness,
        Self::DtmfChannel0,
        Self::EcholinkChannel0,
        Self::Tone1750Hz,
        Self::MemoryInput,
    ];
}

impl std::fmt::Display for FrontPanelPfFunction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Recording => "Recording",
            Self::VoiceMessage1 => "Voice Message 1",
            Self::VoiceMessage2 => "Voice Message 2",
            Self::VoiceMessage3 => "Voice Message 3",
            Self::VoiceMessage4 => "Voice Message 4",
            Self::VoiceGuidance => "Voice Guidance",
            Self::BatteryLevel => "Battery Level",
            Self::Vox => "VOX",
            Self::GroupName => "Group Name",
            Self::Balance => "Balance",
            Self::Gps => "GPS",
            Self::TrackLog => "Track LOG",
            Self::Squelch => "SQL",
            Self::Shift => "SHIFT",
            Self::Step => "STEP",
            Self::Power => "LOW",
            Self::KeyLock => "Key Lock",
            Self::Lockout => "Lockout",
            Self::MemoryToVfo => "M>V",
            Self::ToneSelect => "T.SEL",
            Self::NewMemory => "NEW",
            Self::VoiceAlert => "Voice Alert",
            Self::LcdBrightness => "LCD Brightness",
            Self::DtmfChannel0 => "DTMF CH0",
            Self::EcholinkChannel0 => "ECHOLINK CH0",
            Self::Tone1750Hz => "1,750 [Hz] Tone",
            Self::MemoryInput => "M.IN",
        })
    }
}

/// One official microphone PF key function (Menus No. 942-944).
///
/// The discriminants match the gapped MCP-D75 menu domain exactly. Unlike
/// the front-panel PF keys, microphone PF keys include navigation and APRS
/// operations and do not offer memory registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MicrophonePfFunction {
    /// Recording.
    Recording = 0,
    /// Voice message 1.
    VoiceMessage1 = 1,
    /// Voice message 2.
    VoiceMessage2 = 2,
    /// Voice message 3.
    VoiceMessage3 = 3,
    /// Voice message 4.
    VoiceMessage4 = 4,
    /// Voice guidance.
    VoiceGuidance = 6,
    /// Battery level announcement.
    BatteryLevel = 7,
    /// VOX.
    Vox = 8,
    /// Group name.
    GroupName = 9,
    /// Audio balance.
    Balance = 10,
    /// GPS.
    Gps = 11,
    /// Track log.
    TrackLog = 12,
    /// Squelch.
    Squelch = 13,
    /// Repeater shift.
    Shift = 14,
    /// Tuning step.
    Step = 15,
    /// Transmit power.
    Power = 16,
    /// Key lock.
    KeyLock = 17,
    /// Memory lockout.
    Lockout = 18,
    /// Memory-to-VFO transfer.
    MemoryToVfo = 19,
    /// Tone selection.
    ToneSelect = 20,
    /// New memory entry.
    NewMemory = 21,
    /// Voice alert.
    VoiceAlert = 22,
    /// LCD brightness.
    LcdBrightness = 24,
    /// DTMF memory channel 0.
    DtmfChannel0 = 27,
    /// `EchoLink` memory channel 0.
    EcholinkChannel0 = 28,
    /// 1,750 Hz tone.
    Tone1750Hz = 29,
    /// Screen capture.
    ScreenCapture = 31,
    /// Change demodulation mode.
    Mode = 32,
    /// Open the menu.
    Menu = 33,
    /// Select Band A/B.
    BandSelect = 34,
    /// Enter VFO mode.
    Vfo = 35,
    /// Enter memory-recall mode.
    MemoryRecall = 36,
    /// Recall the CALL channel.
    Call = 37,
    /// Open APRS messages.
    Message = 38,
    /// Open the APRS station list.
    List = 39,
    /// Transmit an APRS beacon.
    Beacon = 40,
    /// Reverse repeater shift.
    Reverse = 41,
    /// Select tone signaling.
    Tone = 42,
    /// Select MHz tuning.
    Megahertz = 44,
    /// Set or inspect a position mark.
    Mark = 45,
    /// Toggle dual-band display.
    Dual = 46,
    /// Open APRS operations.
    Aprs = 47,
    /// Open APRS objects.
    Object = 48,
    /// Toggle attenuation.
    Attenuator = 49,
    /// Toggle fine tuning.
    Fine = 50,
    /// Open position information.
    Position = 51,
    /// Select the operating band.
    Band = 52,
    /// Open squelch/monitor.
    Monitor = 53,
    /// Navigate up.
    Up = 54,
    /// Navigate down.
    Down = 55,
}

impl MicrophonePfFunction {
    /// Number of official microphone PF selections in the gapped raw
    /// domain (`0..=55`).
    pub const COUNT: u8 = 50;

    /// Every official microphone PF function, in ascending raw-value
    /// (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [
        Self::Recording,
        Self::VoiceMessage1,
        Self::VoiceMessage2,
        Self::VoiceMessage3,
        Self::VoiceMessage4,
        Self::VoiceGuidance,
        Self::BatteryLevel,
        Self::Vox,
        Self::GroupName,
        Self::Balance,
        Self::Gps,
        Self::TrackLog,
        Self::Squelch,
        Self::Shift,
        Self::Step,
        Self::Power,
        Self::KeyLock,
        Self::Lockout,
        Self::MemoryToVfo,
        Self::ToneSelect,
        Self::NewMemory,
        Self::VoiceAlert,
        Self::LcdBrightness,
        Self::DtmfChannel0,
        Self::EcholinkChannel0,
        Self::Tone1750Hz,
        Self::ScreenCapture,
        Self::Mode,
        Self::Menu,
        Self::BandSelect,
        Self::Vfo,
        Self::MemoryRecall,
        Self::Call,
        Self::Message,
        Self::List,
        Self::Beacon,
        Self::Reverse,
        Self::Tone,
        Self::Megahertz,
        Self::Mark,
        Self::Dual,
        Self::Aprs,
        Self::Object,
        Self::Attenuator,
        Self::Fine,
        Self::Position,
        Self::Band,
        Self::Monitor,
        Self::Up,
        Self::Down,
    ];
}

impl std::fmt::Display for MicrophonePfFunction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Recording => "Recording",
            Self::VoiceMessage1 => "Voice Message 1",
            Self::VoiceMessage2 => "Voice Message 2",
            Self::VoiceMessage3 => "Voice Message 3",
            Self::VoiceMessage4 => "Voice Message 4",
            Self::VoiceGuidance => "Voice Guidance",
            Self::BatteryLevel => "Battery Level",
            Self::Vox => "VOX",
            Self::GroupName => "Group Name",
            Self::Balance => "Balance",
            Self::Gps => "GPS",
            Self::TrackLog => "Track LOG",
            Self::Squelch => "SQL",
            Self::Shift => "SHIFT",
            Self::Step => "STEP",
            Self::Power => "LOW",
            Self::KeyLock => "Key Lock",
            Self::Lockout => "Lockout",
            Self::MemoryToVfo => "M>V",
            Self::ToneSelect => "T.SEL",
            Self::NewMemory => "NEW",
            Self::VoiceAlert => "Voice Alert",
            Self::LcdBrightness => "LCD Brightness",
            Self::DtmfChannel0 => "DTMF CH0",
            Self::EcholinkChannel0 => "ECHOLINK CH0",
            Self::Tone1750Hz => "1,750 [Hz] Tone",
            Self::ScreenCapture => "Screen Capture",
            Self::Mode => "MODE",
            Self::Menu => "MENU",
            Self::BandSelect => "A/B",
            Self::Vfo => "VFO",
            Self::MemoryRecall => "MR",
            Self::Call => "CALL",
            Self::Message => "MSG",
            Self::List => "LIST",
            Self::Beacon => "BCON",
            Self::Reverse => "REV",
            Self::Tone => "TONE",
            Self::Megahertz => "MHz",
            Self::Mark => "MARK",
            Self::Dual => "DUAL",
            Self::Aprs => "APRS",
            Self::Object => "OBJ",
            Self::Attenuator => "ATT",
            Self::Fine => "FINE",
            Self::Position => "POS",
            Self::Band => "BAND",
            Self::Monitor => "MONI",
            Self::Up => "UP",
            Self::Down => "DOWN",
        })
    }
}

/// Automatic cursor-shift delay for text entry (Menu No. 945).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum CursorShift {
    /// Do not shift the cursor automatically.
    Off = 0,
    /// Shift after 1.0 second.
    Seconds1 = 1,
    /// Shift after 1.5 seconds.
    Seconds1_5 = 2,
    /// Shift after 2.0 seconds.
    Seconds2 = 3,
}

impl CursorShift {
    /// Number of cursor-shift selections (raw `0..=3`).
    pub const COUNT: u8 = 4;

    /// Every cursor-shift selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Off, Self::Seconds1, Self::Seconds1_5, Self::Seconds2];
}

impl std::fmt::Display for CursorShift {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Seconds1 => "1.0 s",
            Self::Seconds1_5 => "1.5 s",
            Self::Seconds2 => "2.0 s",
        })
    }
}

/// Exact stored time-zone selector used by Menu No. 951.
///
/// The MCP-D75 registry proves which selector bytes are accepted, but it does
/// not provide a trustworthy mapping from every byte to a UTC offset. This
/// type therefore preserves and validates the selector without fabricating
/// offset semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StoredTimeZone(u8);

impl StoredTimeZone {
    /// Return the exact MCP selector byte.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// USB device function (Menu No. 980).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UsbFunction {
    /// USB serial control plus AF/IF audio output.
    ComAndAudioOutput = 0,
    /// USB mass-storage access to the memory card.
    MassStorage = 1,
}

impl UsbFunction {
    /// Number of USB device functions (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every USB device function, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::ComAndAudioOutput, Self::MassStorage];
}

impl std::fmt::Display for UsbFunction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ComAndAudioOutput => "COM + AF/IF Output",
            Self::MassStorage => "Mass Storage",
        })
    }
}

/// A stored PF assignment byte that is not an official menu selection.
///
/// This diagnostic type has no public constructor. It exists so reads can
/// preserve an observed off-menu byte without claiming that byte is writable
/// or has an official function assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OffMenuPfCode(u8);

impl OffMenuPfCode {
    pub(crate) const fn from_raw(raw: u8) -> Self {
        Self(raw)
    }

    /// Returns the exact stored byte.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        self.0
    }
}

/// Exact PF assignment read from an MCP image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StoredFrontPanelPfAssignment {
    /// An official writable menu selection.
    Official(FrontPanelPfFunction),
    /// An exact off-menu byte retained for diagnostics.
    OffMenu(OffMenuPfCode),
}

impl StoredFrontPanelPfAssignment {
    /// Returns the exact byte stored in the image.
    #[must_use]
    pub const fn as_raw(self) -> u8 {
        match self {
            Self::Official(function) => function as u8,
            Self::OffMenu(code) => code.as_raw(),
        }
    }
}

// ---------------------------------------------------------------------------
// System settings
// ---------------------------------------------------------------------------

/// Exact scalar values in the remaining Configuration menu groups.
///
/// [`DisplaySettings`] and [`AudioSettings`] cover their corresponding menu
/// groups. This aggregate covers Battery, Bluetooth, Auxiliary, Date & Time,
/// Lock, Units, Interface, and Language. It intentionally omits action rows,
/// read-only information, blobs, list editors, and text fields such as the
/// Bluetooth device name and secret access code.
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
    reason = "Each bool is a distinct MCP menu field; combining unrelated settings into flags \
              would erase their independent names and storage cells."
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemSettings {
    /// Battery-saver receive-off interval (Menu No. 920).
    pub battery_saver: BatterySaverInterval,
    /// Auto power off timer.
    pub auto_power_off: AutoPowerOff,
    /// Permit charging while the radio is powered on (Menu No. 923).
    pub charging_while_powered_on: bool,
    /// Bluetooth radio enabled (Menu No. 930).
    pub bluetooth_enabled: bool,
    /// Reconnect automatically to the most recent Bluetooth device
    /// (Menu No. 936).
    pub bluetooth_auto_connect: bool,
    /// Front-panel PF1 assignment (Menu No. 940).
    pub front_panel_pf1: FrontPanelPfFunction,
    /// Front-panel PF2 assignment (Menu No. 941).
    pub front_panel_pf2: FrontPanelPfFunction,
    /// Microphone PF1 assignment (Menu No. 942).
    pub microphone_pf1: MicrophonePfFunction,
    /// Microphone PF2 assignment (Menu No. 943).
    pub microphone_pf2: MicrophonePfFunction,
    /// Microphone PF3 assignment (Menu No. 944).
    pub microphone_pf3: MicrophonePfFunction,
    /// Automatic cursor-shift delay for text entry (Menu No. 945).
    pub cursor_shift: CursorShift,
    /// Exact stored time-zone selector from the clock editor.
    pub time_zone: StoredTimeZone,
    /// Independent key/frequency lock choices (Menu No. 960).
    pub key_lock: KeyLockSelection,
    /// Lock DTMF keys (Menu No. 961).
    pub dtmf_keys_locked: bool,
    /// Lock microphone keys (Menu No. 962).
    pub microphone_keys_locked: bool,
    /// Lock the volume control (Menu No. 963).
    pub volume_locked: bool,
    /// Display unit system.
    pub display_units: DisplayUnits,
    /// USB device function (Menu No. 980).
    pub usb_function: UsbFunction,
    /// GPS PC-output interface (Menu No. 981).
    pub gps_output_interface: PcOutputInterface,
    /// APRS PC-output interface (Menu No. 982).
    pub aprs_output_interface: PcOutputInterface,
    /// KISS-mode interface (Menu No. 983).
    pub kiss_interface: PcOutputInterface,
    /// DV/DR-mode interface (Menu No. 984).
    pub digital_mode_interface: PcOutputInterface,
    /// DV Gateway-mode interface (Menu No. 985).
    pub dv_gateway_interface: PcOutputInterface,
    /// Language selection.
    pub language: Language,
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
#[repr(u8)]
pub enum AutoPowerOff {
    /// Auto power off disabled.
    Off = 0,
    /// Power off after 15 minutes of inactivity.
    Min15 = 1,
    /// Power off after 30 minutes of inactivity.
    Min30 = 2,
    /// Power off after 60 minutes of inactivity.
    Min60 = 3,
}

impl AutoPowerOff {
    /// Number of auto power off selections (raw `0..=3`).
    pub const COUNT: u8 = 4;

    /// Every auto power off selection, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::Off, Self::Min15, Self::Min30, Self::Min60];
}

impl std::fmt::Display for AutoPowerOff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::Min15 => "15 min",
            Self::Min30 => "30 min",
            Self::Min60 => "60 min",
        })
    }
}

/// Independent lock-type checkboxes (Menu No. 960).
///
/// MCP stores these as two independent bits. All four combinations are valid,
/// including neither checkbox selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyLockSelection {
    /// Select the Key Lock checkbox.
    pub keys: bool,
    /// Select the Frequency Lock checkbox.
    pub frequency: bool,
}

impl KeyLockSelection {
    /// Number of valid combinations of the two lock bits (`0..=3`).
    pub const COUNT: u8 = 4;
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
    /// Latitude/longitude display notation.
    pub coordinates: CoordinateFormat,
    /// Grid-square display system.
    pub grid_square: GridSquareFormat,
}

/// Speed and distance measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SpeedDistanceUnit {
    /// Miles per hour / miles.
    MilesPerHour = 0,
    /// Kilometers per hour / kilometers.
    KilometersPerHour = 1,
    /// Knots / nautical miles.
    Knots = 2,
}

impl SpeedDistanceUnit {
    /// Number of speed and distance unit choices (raw `0..=2`).
    pub const COUNT: u8 = 3;

    /// Every speed and distance unit choice, in ascending raw-value (menu)
    /// order.
    pub const ALL: [Self; Self::COUNT as usize] =
        [Self::MilesPerHour, Self::KilometersPerHour, Self::Knots];
}

impl std::fmt::Display for SpeedDistanceUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MilesPerHour => "mi/h , mile",
            Self::KilometersPerHour => "km/h , km",
            Self::Knots => "knots , nm",
        })
    }
}

/// Altitude and rainfall measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AltitudeRainUnit {
    /// Feet / inches.
    FeetInch = 0,
    /// Meters / millimeters.
    MetersMm = 1,
}

impl AltitudeRainUnit {
    /// Number of altitude and rainfall unit choices (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every altitude and rainfall unit choice, in ascending raw-value (menu)
    /// order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::FeetInch, Self::MetersMm];
}

impl std::fmt::Display for AltitudeRainUnit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::FeetInch => "feet , inch",
            Self::MetersMm => "m , mm",
        })
    }
}

/// Temperature measurement units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TemperatureUnit {
    /// Fahrenheit.
    Fahrenheit = 0,
    /// Celsius.
    Celsius = 1,
}

impl TemperatureUnit {
    /// Number of temperature unit choices (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every temperature unit choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::Fahrenheit, Self::Celsius];
}

impl std::fmt::Display for TemperatureUnit {
    /// Renders the exact menu label, which spells out "degree".
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Fahrenheit => "degree F",
            Self::Celsius => "degree C",
        })
    }
}

/// Language selection (Menu No. 990).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Language {
    /// English.
    English = 0,
    /// Japanese.
    Japanese = 1,
}

impl Language {
    /// Number of language choices (raw `0..=1`).
    pub const COUNT: u8 = 2;

    /// Every language choice, in ascending raw-value (menu) order.
    pub const ALL: [Self; Self::COUNT as usize] = [Self::English, Self::Japanese];
}

impl std::fmt::Display for Language {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::English => "English",
            Self::Japanese => "Japanese",
        })
    }
}

// ---------------------------------------------------------------------------
// TryFrom<u8> implementations for MCP binary parsing
// ---------------------------------------------------------------------------

macro_rules! impl_raw_setting_enum {
    ($type:ty, $name:literal, $detail:literal, {$($raw:literal => $variant:path),+ $(,)?}) => {
        impl TryFrom<u8> for $type {
            type Error = ValidationError;

            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value {
                    $($raw => Ok($variant),)+
                    _ => Err(ValidationError::SettingOutOfRange {
                        name: $name,
                        value,
                        detail: $detail,
                    }),
                }
            }
        }

        impl From<$type> for u8 {
            fn from(value: $type) -> Self {
                match value {
                    $($variant => $raw,)+
                }
            }
        }
    };
}

impl_raw_setting_enum!(LcdBrightness, "LCD brightness", "must be 0-2", {
    0 => LcdBrightness::Low,
    1 => LcdBrightness::Medium,
    2 => LcdBrightness::High,
});

impl_raw_setting_enum!(SingleBandDisplay, "single-band display", "must be 0-4", {
    0 => SingleBandDisplay::Off,
    1 => SingleBandDisplay::GpsAltitude,
    2 => SingleBandDisplay::GpsGroundSpeed,
    3 => SingleBandDisplay::Date,
    4 => SingleBandDisplay::DemodulationMode,
});

impl_raw_setting_enum!(MeterType, "meter type", "must be 0-2", {
    0 => MeterType::Type1,
    1 => MeterType::Type2,
    2 => MeterType::Type3,
});

impl_raw_setting_enum!(BackgroundColor, "background color", "must be 0-1", {
    0 => BackgroundColor::Black,
    1 => BackgroundColor::White,
});

impl_raw_setting_enum!(InformationBacklight, "information backlight", "must be 0-2", {
    0 => InformationBacklight::Off,
    1 => InformationBacklight::Lcd,
    2 => InformationBacklight::LcdAndKeys,
});

impl_raw_setting_enum!(AudioBalance, "audio balance", "must be 0-9", {
    0 => AudioBalance::A100B0,
    1 => AudioBalance::A100B25,
    2 => AudioBalance::A100B50,
    3 => AudioBalance::A100B75,
    4 => AudioBalance::A100B100,
    5 => AudioBalance::A75B100,
    6 => AudioBalance::A50B100,
    7 => AudioBalance::A25B100,
    8 => AudioBalance::A0B100,
    9 => AudioBalance::OperationBandOnly,
});

impl_raw_setting_enum!(CallsignReadout, "callsign readout", "must be 0-2", {
    0 => CallsignReadout::Standard,
    1 => CallsignReadout::FullPhonetics,
    2 => CallsignReadout::SuffixPhonetics,
});

impl_raw_setting_enum!(UsbAudioOutputLevel, "USB audio output level", "must be 0-6", {
    0 => UsbAudioOutputLevel::Level1,
    1 => UsbAudioOutputLevel::Level2,
    2 => UsbAudioOutputLevel::Level3,
    3 => UsbAudioOutputLevel::Level4,
    4 => UsbAudioOutputLevel::Level5,
    5 => UsbAudioOutputLevel::Level6,
    6 => UsbAudioOutputLevel::Level7,
});

impl_raw_setting_enum!(CursorShift, "cursor shift", "must be 0-3", {
    0 => CursorShift::Off,
    1 => CursorShift::Seconds1,
    2 => CursorShift::Seconds1_5,
    3 => CursorShift::Seconds2,
});

impl_raw_setting_enum!(UsbFunction, "USB function", "must be 0-1", {
    0 => UsbFunction::ComAndAudioOutput,
    1 => UsbFunction::MassStorage,
});

impl_raw_setting_enum!(MicrophonePfFunction, "microphone PF function", "must be an official menu code", {
    0 => MicrophonePfFunction::Recording,
    1 => MicrophonePfFunction::VoiceMessage1,
    2 => MicrophonePfFunction::VoiceMessage2,
    3 => MicrophonePfFunction::VoiceMessage3,
    4 => MicrophonePfFunction::VoiceMessage4,
    6 => MicrophonePfFunction::VoiceGuidance,
    7 => MicrophonePfFunction::BatteryLevel,
    8 => MicrophonePfFunction::Vox,
    9 => MicrophonePfFunction::GroupName,
    10 => MicrophonePfFunction::Balance,
    11 => MicrophonePfFunction::Gps,
    12 => MicrophonePfFunction::TrackLog,
    13 => MicrophonePfFunction::Squelch,
    14 => MicrophonePfFunction::Shift,
    15 => MicrophonePfFunction::Step,
    16 => MicrophonePfFunction::Power,
    17 => MicrophonePfFunction::KeyLock,
    18 => MicrophonePfFunction::Lockout,
    19 => MicrophonePfFunction::MemoryToVfo,
    20 => MicrophonePfFunction::ToneSelect,
    21 => MicrophonePfFunction::NewMemory,
    22 => MicrophonePfFunction::VoiceAlert,
    24 => MicrophonePfFunction::LcdBrightness,
    27 => MicrophonePfFunction::DtmfChannel0,
    28 => MicrophonePfFunction::EcholinkChannel0,
    29 => MicrophonePfFunction::Tone1750Hz,
    31 => MicrophonePfFunction::ScreenCapture,
    32 => MicrophonePfFunction::Mode,
    33 => MicrophonePfFunction::Menu,
    34 => MicrophonePfFunction::BandSelect,
    35 => MicrophonePfFunction::Vfo,
    36 => MicrophonePfFunction::MemoryRecall,
    37 => MicrophonePfFunction::Call,
    38 => MicrophonePfFunction::Message,
    39 => MicrophonePfFunction::List,
    40 => MicrophonePfFunction::Beacon,
    41 => MicrophonePfFunction::Reverse,
    42 => MicrophonePfFunction::Tone,
    44 => MicrophonePfFunction::Megahertz,
    45 => MicrophonePfFunction::Mark,
    46 => MicrophonePfFunction::Dual,
    47 => MicrophonePfFunction::Aprs,
    48 => MicrophonePfFunction::Object,
    49 => MicrophonePfFunction::Attenuator,
    50 => MicrophonePfFunction::Fine,
    51 => MicrophonePfFunction::Position,
    52 => MicrophonePfFunction::Band,
    53 => MicrophonePfFunction::Monitor,
    54 => MicrophonePfFunction::Up,
    55 => MicrophonePfFunction::Down,
});

impl_raw_setting_enum!(TransmitTimeout, "transmit timeout", "must be 0-10", {
    0 => TransmitTimeout::Seconds30,
    1 => TransmitTimeout::Seconds60,
    2 => TransmitTimeout::Seconds90,
    3 => TransmitTimeout::Seconds120,
    4 => TransmitTimeout::Seconds150,
    5 => TransmitTimeout::Seconds180,
    6 => TransmitTimeout::Seconds210,
    7 => TransmitTimeout::Seconds240,
    8 => TransmitTimeout::Seconds270,
    9 => TransmitTimeout::Seconds300,
    10 => TransmitTimeout::Seconds600,
});

impl_raw_setting_enum!(SsbHighCut, "SSB high-cut filter", "must be 0-4", {
    0 => SsbHighCut::Khz2_2,
    1 => SsbHighCut::Khz2_4,
    2 => SsbHighCut::Khz2_6,
    3 => SsbHighCut::Khz2_8,
    4 => SsbHighCut::Khz3_0,
});

impl_raw_setting_enum!(CwFilterWidth, "CW filter width", "must be 0-4", {
    0 => CwFilterWidth::Khz0_3,
    1 => CwFilterWidth::Khz0_5,
    2 => CwFilterWidth::Khz1_0,
    3 => CwFilterWidth::Khz1_5,
    4 => CwFilterWidth::Khz2_0,
});

impl_raw_setting_enum!(AmHighCut, "AM high-cut filter", "must be 0-3", {
    0 => AmHighCut::Khz3_0,
    1 => AmHighCut::Khz4_5,
    2 => AmHighCut::Khz6_0,
    3 => AmHighCut::Khz7_5,
});

impl_raw_setting_enum!(RepeaterCallKey, "repeater CALL key", "must be 0-1", {
    0 => RepeaterCallKey::CallChannel,
    1 => RepeaterCallKey::Tone1750Hz,
});

impl_raw_setting_enum!(DtmfToneDuration, "DTMF tone duration", "must be 0-2", {
    0 => DtmfToneDuration::Ms50,
    1 => DtmfToneDuration::Ms100,
    2 => DtmfToneDuration::Ms150,
});

impl_raw_setting_enum!(VoiceAnnounceMode, "voice announce mode", "must be 0-3", {
    0 => VoiceAnnounceMode::Off,
    1 => VoiceAnnounceMode::Manual,
    2 => VoiceAnnounceMode::Auto1,
    3 => VoiceAnnounceMode::Auto2,
});

impl_raw_setting_enum!(BatterySaverInterval, "battery saver interval", "must be 0-9", {
    0 => BatterySaverInterval::Off,
    1 => BatterySaverInterval::Seconds0_2,
    2 => BatterySaverInterval::Seconds0_4,
    3 => BatterySaverInterval::Seconds0_6,
    4 => BatterySaverInterval::Seconds0_8,
    5 => BatterySaverInterval::Seconds1,
    6 => BatterySaverInterval::Seconds2,
    7 => BatterySaverInterval::Seconds3,
    8 => BatterySaverInterval::Seconds4,
    9 => BatterySaverInterval::Seconds5,
});

impl_raw_setting_enum!(PcOutputInterface, "PC output interface", "must be 0-1", {
    0 => PcOutputInterface::Usb,
    1 => PcOutputInterface::Bluetooth,
});

impl_raw_setting_enum!(FrontPanelPfFunction, "front-panel PF function", "must be an official menu code", {
    0 => FrontPanelPfFunction::Recording,
    1 => FrontPanelPfFunction::VoiceMessage1,
    2 => FrontPanelPfFunction::VoiceMessage2,
    3 => FrontPanelPfFunction::VoiceMessage3,
    4 => FrontPanelPfFunction::VoiceMessage4,
    6 => FrontPanelPfFunction::VoiceGuidance,
    7 => FrontPanelPfFunction::BatteryLevel,
    8 => FrontPanelPfFunction::Vox,
    9 => FrontPanelPfFunction::GroupName,
    10 => FrontPanelPfFunction::Balance,
    11 => FrontPanelPfFunction::Gps,
    12 => FrontPanelPfFunction::TrackLog,
    13 => FrontPanelPfFunction::Squelch,
    14 => FrontPanelPfFunction::Shift,
    15 => FrontPanelPfFunction::Step,
    16 => FrontPanelPfFunction::Power,
    17 => FrontPanelPfFunction::KeyLock,
    18 => FrontPanelPfFunction::Lockout,
    19 => FrontPanelPfFunction::MemoryToVfo,
    20 => FrontPanelPfFunction::ToneSelect,
    21 => FrontPanelPfFunction::NewMemory,
    22 => FrontPanelPfFunction::VoiceAlert,
    24 => FrontPanelPfFunction::LcdBrightness,
    27 => FrontPanelPfFunction::DtmfChannel0,
    28 => FrontPanelPfFunction::EcholinkChannel0,
    29 => FrontPanelPfFunction::Tone1750Hz,
    30 => FrontPanelPfFunction::MemoryInput,
});

impl TryFrom<u8> for ScanRestartDelay {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ScanRestartDelay> for u8 {
    fn from(value: ScanRestartDelay) -> Self {
        value.as_seconds()
    }
}

impl TryFrom<u8> for AutoMuteReturnDelay {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AutoMuteReturnDelay> for u8 {
    fn from(value: AutoMuteReturnDelay) -> Self {
        value.as_seconds()
    }
}

impl TryFrom<u8> for BacklightTimer {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<BacklightTimer> for u8 {
    fn from(value: BacklightTimer) -> Self {
        value.as_seconds()
    }
}

impl TryFrom<u8> for LinkedVolumeLevel {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 7 {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "linked volume level",
                value,
                detail: "must be 0-7, where 0 means VOL Link",
            })
        }
    }
}

impl From<LinkedVolumeLevel> for u8 {
    fn from(value: LinkedVolumeLevel) -> Self {
        value.as_raw()
    }
}

impl From<u8> for StoredFrontPanelPfAssignment {
    fn from(raw: u8) -> Self {
        FrontPanelPfFunction::try_from(raw).map_or_else(
            |_| Self::OffMenu(OffMenuPfCode::from_raw(raw)),
            Self::Official,
        )
    }
}

impl TryFrom<u8> for BacklightControl {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Manual),
            1 => Ok(Self::On),
            2 => Ok(Self::Auto),
            3 => Ok(Self::AutoDcIn),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "backlight control",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<BacklightControl> for u8 {
    fn from(control: BacklightControl) -> Self {
        control as Self
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

impl TryFrom<u8> for MicSensitivity {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::High),
            1 => Ok(Self::Medium),
            2 => Ok(Self::Low),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "mic sensitivity",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<MicSensitivity> for u8 {
    fn from(value: MicSensitivity) -> Self {
        value as Self
    }
}

impl TryFrom<u8> for VoiceGuideSpeed {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Speed1),
            1 => Ok(Self::Speed2),
            2 => Ok(Self::Speed3),
            3 => Ok(Self::Speed4),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "voice guide speed",
                value,
                detail: "must be 0-3",
            }),
        }
    }
}

impl From<VoiceGuideSpeed> for u8 {
    fn from(value: VoiceGuideSpeed) -> Self {
        value as Self
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

impl TryFrom<u8> for KeyLockSelection {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value & !0x03 == 0 {
            Ok(Self {
                keys: value & 0x01 != 0,
                frequency: value & 0x02 != 0,
            })
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "key lock selection",
                value,
                detail: "must contain only Key Lock (0x01) and Frequency Lock (0x02) bits",
            })
        }
    }
}

impl From<KeyLockSelection> for u8 {
    fn from(value: KeyLockSelection) -> Self {
        Self::from(value.keys) | (Self::from(value.frequency) << 1)
    }
}

impl TryFrom<u8> for StoredTimeZone {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if matches!(value, 0..=52 | 56..=112 | 201..=203) {
            Ok(Self(value))
        } else {
            Err(ValidationError::SettingOutOfRange {
                name: "stored time-zone selector",
                value,
                detail: "must be 0-52, 56-112, or 201-203",
            })
        }
    }
}

impl From<StoredTimeZone> for u8 {
    fn from(value: StoredTimeZone) -> Self {
        value.as_raw()
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

impl From<Language> for u8 {
    fn from(value: Language) -> Self {
        value as Self
    }
}

impl TryFrom<u8> for SpeedDistanceUnit {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::MilesPerHour),
            1 => Ok(Self::KilometersPerHour),
            2 => Ok(Self::Knots),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "speed and distance unit",
                value,
                detail: "must be 0-2",
            }),
        }
    }
}

impl From<SpeedDistanceUnit> for u8 {
    fn from(value: SpeedDistanceUnit) -> Self {
        value as Self
    }
}

impl TryFrom<u8> for AltitudeRainUnit {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::FeetInch),
            1 => Ok(Self::MetersMm),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "altitude and rain unit",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<AltitudeRainUnit> for u8 {
    fn from(value: AltitudeRainUnit) -> Self {
        value as Self
    }
}

impl TryFrom<u8> for TemperatureUnit {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Fahrenheit),
            1 => Ok(Self::Celsius),
            _ => Err(ValidationError::SettingOutOfRange {
                name: "temperature unit",
                value,
                detail: "must be 0-1",
            }),
        }
    }
}

impl From<TemperatureUnit> for u8 {
    fn from(value: TemperatureUnit) -> Self {
        value as Self
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_settings_cover_menus_900_through_907() -> Result<(), ValidationError> {
        let settings = DisplaySettings {
            backlight_control: BacklightControl::Auto,
            backlight_timer: BacklightTimer::try_from(10)?,
            lcd_brightness: LcdBrightness::High,
            background_color: BackgroundColor::Black,
            power_on_message: PowerOnMessage::new("HELLO !!")?,
            single_band_display: SingleBandDisplay::Off,
            meter_type: MeterType::Type1,
            information_backlight: InformationBacklight::LcdAndKeys,
        };

        assert_eq!(settings.power_on_message.as_str(), "HELLO !!");
        assert_eq!(u8::from(settings.lcd_brightness), 2);
        assert_eq!(u8::from(settings.background_color), 0);
        Ok(())
    }

    #[test]
    fn audio_settings_cover_menus_910_through_91a() -> Result<(), ValidationError> {
        let settings = AudioSettings {
            balance: AudioBalance::A100B100,
            tx_equalizer: TxEqualizer::default(),
            rx_equalizer: RxEqualizer::default(),
            beep: true,
            beep_volume: LinkedVolumeLevel::try_from(4)?,
            voice_announce: VoiceAnnounceMode::Off,
            voice_announce_volume: LinkedVolumeLevel::VOLUME_LINK,
            voice_guidance_speed: VoiceGuideSpeed::Speed1,
            callsign_readout: CallsignReadout::Standard,
            usb_audio_output_level: UsbAudioOutputLevel::Level1,
        };

        assert!(settings.beep);
        assert_eq!(settings.beep_volume.fixed_level(), Some(4));
        assert_eq!(u8::from(settings.balance), 4);
        Ok(())
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
            let level = TxEqLevel::new(db)?;
            assert_eq!(TxEqLevel::from_raw(level.as_raw()), Some(level));
            assert_eq!(TxEqLevel::try_from(u8::from(level))?, level);
            assert_eq!(level.as_db(), db);
        }
        assert!(TxEqLevel::new(-10).is_err());
        assert!(TxEqLevel::new(4).is_err());
        assert!(TxEqLevel::from_raw(13).is_none());
        assert!(TxEqLevel::try_from(13).is_err());
        Ok(())
    }

    #[test]
    fn rx_eq_level_validates_and_round_trips_raw() -> Result<(), Box<dyn std::error::Error>> {
        for db in RxEqLevel::MIN_DB..=RxEqLevel::MAX_DB {
            let level = RxEqLevel::new(db)?;
            assert_eq!(RxEqLevel::from_raw(level.as_raw()), Some(level));
            assert_eq!(RxEqLevel::try_from(u8::from(level))?, level);
            assert_eq!(level.as_db(), db);
        }
        assert!(RxEqLevel::new(-10).is_err());
        assert!(RxEqLevel::new(10).is_err());
        assert!(RxEqLevel::from_raw(19).is_none());
        assert!(RxEqLevel::try_from(19).is_err());
        Ok(())
    }

    #[test]
    fn power_on_message_preserves_printable_ascii_and_spaces() -> Result<(), ValidationError> {
        let msg = PowerOnMessage::new(" TH-D75~ ")?;
        assert_eq!(msg.as_str(), " TH-D75~ ");
        assert_eq!(msg.as_ref(), " TH-D75~ ");
        assert_eq!(msg.to_string(), " TH-D75~ ");
        assert_eq!(msg.len(), 9);
        assert!(!msg.is_empty());

        let empty = PowerOnMessage::try_from("")?;
        assert!(empty.is_empty());
        assert_eq!(empty.to_wire_bytes(), [0; PowerOnMessage::WIRE_LEN]);
        Ok(())
    }

    #[test]
    fn power_on_message_accepts_exact_text_byte_boundaries() -> Result<(), ValidationError> {
        let msg = PowerOnMessage::new(" ~")?;
        assert_eq!(msg.as_str(), " ~");
        assert_eq!(msg.to_wire_bytes().get(..2), Some(&b" ~"[..]));
        Ok(())
    }

    #[test]
    fn power_on_message_max_length_round_trips_without_a_terminator() -> Result<(), ValidationError>
    {
        let msg = PowerOnMessage::new("1234567890123456")?;
        assert_eq!(msg.len(), PowerOnMessage::MAX_LEN);
        assert_eq!(msg.to_wire_bytes(), *b"1234567890123456");
        assert_eq!(PowerOnMessage::try_from_wire(msg.to_wire_bytes())?, msg);
        Ok(())
    }

    #[test]
    fn power_on_message_short_wire_value_round_trips_with_nul_padding()
    -> Result<(), ValidationError> {
        let msg = PowerOnMessage::new("TH-D75 Ready")?;
        assert_eq!(msg.as_str(), "TH-D75 Ready");
        assert_eq!(msg.to_wire_bytes(), *b"TH-D75 Ready\0\0\0\0");
        assert_eq!(PowerOnMessage::try_from_wire(msg.to_wire_bytes())?, msg);
        Ok(())
    }

    #[test]
    fn power_on_message_reports_exact_length_error() {
        assert!(matches!(
            PowerOnMessage::new("12345678901234567"),
            Err(ValidationError::PowerOnMessageTooLong { len: 17 })
        ));
    }

    #[test]
    fn power_on_message_rejects_every_non_printable_boundary_with_exact_offset() {
        for (text, offset, value) in [
            ("\u{1f}", 0, 0x1F),
            ("D75\nREADY", 3, b'\n'),
            ("D75\0READY", 3, 0),
            ("\u{7f}", 0, 0x7F),
            ("D75 ✓", 4, 0xE2),
        ] {
            assert!(matches!(
                PowerOnMessage::new(text),
                Err(ValidationError::InvalidPowerOnMessageByte {
                    offset: actual_offset,
                    value: actual_value,
                }) if actual_offset == offset && actual_value == value
            ));
        }
    }

    #[test]
    fn power_on_message_wire_decode_rejects_control_bytes_and_data_after_nul() {
        let mut control = [0; PowerOnMessage::WIRE_LEN];
        control[0] = b'D';
        control[1] = 0x1F;
        assert!(matches!(
            PowerOnMessage::try_from_wire(control),
            Err(ValidationError::InvalidPowerOnMessageByte {
                offset: 1,
                value: 0x1F,
            })
        ));

        let mut data_after_nul = [0; PowerOnMessage::WIRE_LEN];
        data_after_nul[..5].copy_from_slice(b"D\0BAD");
        assert!(matches!(
            PowerOnMessage::try_from_wire(data_after_nul),
            Err(ValidationError::PowerOnMessageDataAfterNul {
                terminator_offset: 1,
                offset: 2,
                value: b'B',
            })
        ));
    }

    #[test]
    fn exact_selector_types_round_trip_and_reject_out_of_domain_values()
    -> Result<(), ValidationError> {
        for raw in 0..=10 {
            let value = TransmitTimeout::try_from(raw)?;
            assert_eq!(u8::from(value), raw);
        }
        assert_eq!(TransmitTimeout::Seconds600.as_seconds(), 600);
        assert!(TransmitTimeout::try_from(11).is_err());

        for raw in 0..=4 {
            assert_eq!(u8::from(SsbHighCut::try_from(raw)?), raw);
            assert_eq!(u8::from(CwFilterWidth::try_from(raw)?), raw);
        }
        assert!(SsbHighCut::try_from(5).is_err());
        assert!(CwFilterWidth::try_from(5).is_err());

        for raw in 0..=3 {
            assert_eq!(u8::from(AmHighCut::try_from(raw)?), raw);
            assert_eq!(u8::from(VoiceAnnounceMode::try_from(raw)?), raw);
            assert_eq!(u8::from(VoiceGuideSpeed::try_from(raw)?), raw);
        }
        assert!(AmHighCut::try_from(4).is_err());
        assert!(VoiceAnnounceMode::try_from(4).is_err());
        assert!(VoiceGuideSpeed::try_from(4).is_err());

        for raw in 0..=9 {
            assert_eq!(u8::from(BatterySaverInterval::try_from(raw)?), raw);
            assert_eq!(u8::from(AudioBalance::try_from(raw)?), raw);
        }
        assert!(BatterySaverInterval::try_from(10).is_err());
        assert!(AudioBalance::try_from(10).is_err());

        for raw in 0..=2 {
            assert_eq!(u8::from(DtmfToneDuration::try_from(raw)?), raw);
            assert_eq!(u8::from(MicSensitivity::try_from(raw)?), raw);
            assert_eq!(u8::from(LcdBrightness::try_from(raw)?), raw);
            assert_eq!(u8::from(MeterType::try_from(raw)?), raw);
            assert_eq!(u8::from(InformationBacklight::try_from(raw)?), raw);
            assert_eq!(u8::from(CallsignReadout::try_from(raw)?), raw);
        }
        assert!(DtmfToneDuration::try_from(3).is_err());
        assert!(MicSensitivity::try_from(3).is_err());
        assert!(LcdBrightness::try_from(3).is_err());
        assert!(MeterType::try_from(3).is_err());
        assert!(InformationBacklight::try_from(3).is_err());
        assert!(CallsignReadout::try_from(3).is_err());

        for raw in 0..=4 {
            assert_eq!(u8::from(SingleBandDisplay::try_from(raw)?), raw);
        }
        assert!(SingleBandDisplay::try_from(5).is_err());

        for raw in 0..=6 {
            assert_eq!(u8::from(UsbAudioOutputLevel::try_from(raw)?), raw);
        }
        assert!(UsbAudioOutputLevel::try_from(7).is_err());

        for raw in 0..KeyLockSelection::COUNT {
            assert_eq!(u8::from(KeyLockSelection::try_from(raw)?), raw);
        }
        assert!(KeyLockSelection::try_from(KeyLockSelection::COUNT).is_err());
        Ok(())
    }

    #[test]
    fn microphone_pf_domain_preserves_registry_gaps() -> Result<(), ValidationError> {
        const GAPS: &[u8] = &[5, 23, 25, 26, 30, 43];
        for raw in 0..=55 {
            if GAPS.contains(&raw) {
                assert!(MicrophonePfFunction::try_from(raw).is_err());
            } else {
                assert_eq!(u8::from(MicrophonePfFunction::try_from(raw)?), raw);
            }
        }
        assert!(MicrophonePfFunction::try_from(56).is_err());
        Ok(())
    }

    #[test]
    fn stored_time_zone_validates_registry_selector_domain() -> Result<(), ValidationError> {
        for raw in (0..=52).chain(56..=112).chain(201..=203) {
            assert_eq!(u8::from(StoredTimeZone::try_from(raw)?), raw);
        }
        for raw in [53, 54, 55, 113, 200, 204, u8::MAX] {
            assert!(StoredTimeZone::try_from(raw).is_err());
        }
        Ok(())
    }

    #[test]
    fn bounded_settings_newtypes_reject_values_instead_of_clamping() -> Result<(), ValidationError>
    {
        assert!(ScanRestartDelay::new(0).is_err());
        assert_eq!(ScanRestartDelay::try_from(1)?.as_seconds(), 1);
        assert_eq!(ScanRestartDelay::try_from(10)?.as_seconds(), 10);
        assert!(ScanRestartDelay::new(11).is_err());

        assert!(AutoMuteReturnDelay::new(0).is_err());
        assert_eq!(AutoMuteReturnDelay::try_from(1)?.as_seconds(), 1);
        assert_eq!(AutoMuteReturnDelay::try_from(10)?.as_seconds(), 10);
        assert!(AutoMuteReturnDelay::new(11).is_err());

        assert!(BacklightTimer::new(2).is_err());
        assert_eq!(BacklightTimer::try_from(3)?.as_seconds(), 3);
        assert_eq!(BacklightTimer::try_from(60)?.as_seconds(), 60);
        assert!(BacklightTimer::new(61).is_err());

        assert_eq!(
            LinkedVolumeLevel::try_from(0)?,
            LinkedVolumeLevel::VOLUME_LINK
        );
        assert_eq!(LinkedVolumeLevel::try_from(7)?.fixed_level(), Some(7));
        assert!(LinkedVolumeLevel::try_from(8).is_err());
        Ok(())
    }

    #[test]
    fn pf_assignments_preserve_off_menu_bytes_without_making_them_writable()
    -> Result<(), ValidationError> {
        for raw in 0..=30 {
            let official = FrontPanelPfFunction::try_from(raw);
            if matches!(raw, 5 | 23 | 25 | 26) {
                assert!(official.is_err());
                assert!(matches!(
                    StoredFrontPanelPfAssignment::from(raw),
                    StoredFrontPanelPfAssignment::OffMenu(_)
                ));
            } else {
                let value = official?;
                assert_eq!(u8::from(value), raw);
                assert_eq!(
                    StoredFrontPanelPfAssignment::from(raw),
                    StoredFrontPanelPfAssignment::Official(value)
                );
            }
        }
        let off_menu = StoredFrontPanelPfAssignment::from(0xFF);
        assert_eq!(off_menu.as_raw(), 0xFF);
        assert!(matches!(off_menu, StoredFrontPanelPfAssignment::OffMenu(_)));
        Ok(())
    }

    #[test]
    fn display_units_include_coordinate_and_grid_formats() {
        let units = DisplayUnits {
            speed_distance: SpeedDistanceUnit::MilesPerHour,
            altitude_rain: AltitudeRainUnit::FeetInch,
            temperature: TemperatureUnit::Fahrenheit,
            coordinates: CoordinateFormat::Dmm,
            grid_square: GridSquareFormat::Maidenhead,
        };
        assert_eq!(units.coordinates, CoordinateFormat::Dmm);
        assert_eq!(units.grid_square, GridSquareFormat::Maidenhead);
    }

    // -----------------------------------------------------------------------
    // ALL/COUNT/Display affordances for closed settings domains
    // -----------------------------------------------------------------------

    fn assert_closed_settings_domain<T>(
        all: &[T],
        count: u8,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        T: Copy + PartialEq + std::fmt::Debug + TryFrom<u8, Error = ValidationError> + Into<u8>,
    {
        assert_eq!(all.len(), usize::from(count), "ALL length must equal COUNT");
        for (previous, current) in all.iter().zip(all.iter().skip(1)) {
            let previous_raw: u8 = (*previous).into();
            let current_raw: u8 = (*current).into();
            assert!(
                previous_raw < current_raw,
                "ALL must ascend by raw value: {previous:?} then {current:?}"
            );
        }
        for &variant in all {
            let raw: u8 = variant.into();
            let decoded = T::try_from(raw)?;
            assert_eq!(decoded, variant, "raw {raw} must decode back to ALL entry");
        }
        Ok(())
    }

    #[test]
    fn all_constants_cover_each_closed_settings_domain_in_raw_order()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_closed_settings_domain(&BacklightControl::ALL, BacklightControl::COUNT)?;
        assert_closed_settings_domain(&LcdBrightness::ALL, LcdBrightness::COUNT)?;
        assert_closed_settings_domain(&SingleBandDisplay::ALL, SingleBandDisplay::COUNT)?;
        assert_closed_settings_domain(&MeterType::ALL, MeterType::COUNT)?;
        assert_closed_settings_domain(&BackgroundColor::ALL, BackgroundColor::COUNT)?;
        assert_closed_settings_domain(&InformationBacklight::ALL, InformationBacklight::COUNT)?;
        assert_closed_settings_domain(&AudioBalance::ALL, AudioBalance::COUNT)?;
        assert_closed_settings_domain(&CallsignReadout::ALL, CallsignReadout::COUNT)?;
        assert_closed_settings_domain(&UsbAudioOutputLevel::ALL, UsbAudioOutputLevel::COUNT)?;
        assert_closed_settings_domain(&MicSensitivity::ALL, MicSensitivity::COUNT)?;
        assert_closed_settings_domain(&VoiceGuideSpeed::ALL, VoiceGuideSpeed::COUNT)?;
        assert_closed_settings_domain(&BeatShift::ALL, BeatShift::COUNT)?;
        assert_closed_settings_domain(&TransmitTimeout::ALL, TransmitTimeout::COUNT)?;
        assert_closed_settings_domain(&SsbHighCut::ALL, SsbHighCut::COUNT)?;
        assert_closed_settings_domain(&CwFilterWidth::ALL, CwFilterWidth::COUNT)?;
        assert_closed_settings_domain(&AmHighCut::ALL, AmHighCut::COUNT)?;
        assert_closed_settings_domain(&RepeaterCallKey::ALL, RepeaterCallKey::COUNT)?;
        assert_closed_settings_domain(&DtmfToneDuration::ALL, DtmfToneDuration::COUNT)?;
        assert_closed_settings_domain(&VoiceAnnounceMode::ALL, VoiceAnnounceMode::COUNT)?;
        assert_closed_settings_domain(&BatterySaverInterval::ALL, BatterySaverInterval::COUNT)?;
        assert_closed_settings_domain(&PcOutputInterface::ALL, PcOutputInterface::COUNT)?;
        assert_closed_settings_domain(&FrontPanelPfFunction::ALL, FrontPanelPfFunction::COUNT)?;
        assert_closed_settings_domain(&MicrophonePfFunction::ALL, MicrophonePfFunction::COUNT)?;
        assert_closed_settings_domain(&CursorShift::ALL, CursorShift::COUNT)?;
        assert_closed_settings_domain(&UsbFunction::ALL, UsbFunction::COUNT)?;
        assert_closed_settings_domain(&AutoPowerOff::ALL, AutoPowerOff::COUNT)?;
        assert_closed_settings_domain(&SpeedDistanceUnit::ALL, SpeedDistanceUnit::COUNT)?;
        assert_closed_settings_domain(&AltitudeRainUnit::ALL, AltitudeRainUnit::COUNT)?;
        assert_closed_settings_domain(&TemperatureUnit::ALL, TemperatureUnit::COUNT)?;
        assert_closed_settings_domain(&Language::ALL, Language::COUNT)?;
        Ok(())
    }

    #[test]
    fn display_labels_render_display_and_audio_menu_text() {
        assert_eq!(BacklightControl::Manual.to_string(), "Manual");
        assert_eq!(BacklightControl::AutoDcIn.to_string(), "Auto (DC-IN)");
        assert_eq!(LcdBrightness::Low.to_string(), "Low");
        assert_eq!(LcdBrightness::High.to_string(), "High");
        assert_eq!(SingleBandDisplay::GpsAltitude.to_string(), "GPS (Altitude)");
        assert_eq!(
            SingleBandDisplay::DemodulationMode.to_string(),
            "Demodulation Mode"
        );
        assert_eq!(MeterType::Type1.to_string(), "Type1");
        assert_eq!(MeterType::Type3.to_string(), "Type3");
        assert_eq!(BackgroundColor::Black.to_string(), "Black");
        assert_eq!(BackgroundColor::White.to_string(), "White");
        assert_eq!(InformationBacklight::Lcd.to_string(), "LCD");
        assert_eq!(InformationBacklight::LcdAndKeys.to_string(), "LCD + Key");
        assert_eq!(AudioBalance::A100B0.to_string(), "A:100/B:0");
        assert_eq!(
            AudioBalance::OperationBandOnly.to_string(),
            "Operation Band Only"
        );
        assert_eq!(CallsignReadout::Standard.to_string(), "Standard");
        assert_eq!(
            CallsignReadout::SuffixPhonetics.to_string(),
            "Phonetics (Suffix)"
        );
        assert_eq!(UsbAudioOutputLevel::Level1.to_string(), "Level 1");
        assert_eq!(UsbAudioOutputLevel::Level7.to_string(), "Level 7");
        assert_eq!(VoiceAnnounceMode::Off.to_string(), "Off");
        assert_eq!(VoiceAnnounceMode::Auto2.to_string(), "Auto2");
        assert_eq!(VoiceGuideSpeed::Speed1.to_string(), "Speed 1");
        assert_eq!(VoiceGuideSpeed::Speed4.to_string(), "Speed 4");
    }

    #[test]
    fn display_labels_render_txrx_and_system_menu_text() {
        assert_eq!(MicSensitivity::High.to_string(), "High");
        assert_eq!(MicSensitivity::Low.to_string(), "Low");
        assert_eq!(BeatShift::Type1.to_string(), "Type1");
        assert_eq!(BeatShift::Type8.to_string(), "Type8");
        assert_eq!(TransmitTimeout::Seconds30.to_string(), "0.5 min");
        assert_eq!(TransmitTimeout::Seconds600.to_string(), "10.0 min");
        assert_eq!(SsbHighCut::Khz2_2.to_string(), "2.2 kHz");
        assert_eq!(SsbHighCut::Khz3_0.to_string(), "3.0 kHz");
        assert_eq!(CwFilterWidth::Khz0_3.to_string(), "0.3 kHz");
        assert_eq!(CwFilterWidth::Khz2_0.to_string(), "2.0 kHz");
        assert_eq!(AmHighCut::Khz3_0.to_string(), "3.0 kHz");
        assert_eq!(AmHighCut::Khz7_5.to_string(), "7.5 kHz");
        assert_eq!(RepeaterCallKey::CallChannel.to_string(), "CALL");
        assert_eq!(RepeaterCallKey::Tone1750Hz.to_string(), "1,750 [Hz]");
        assert_eq!(DtmfToneDuration::Ms50.to_string(), "50 ms");
        assert_eq!(DtmfToneDuration::Ms150.to_string(), "150 ms");
        assert_eq!(BatterySaverInterval::Off.to_string(), "Off");
        assert_eq!(BatterySaverInterval::Seconds0_2.to_string(), "0.2 s");
        assert_eq!(BatterySaverInterval::Seconds5.to_string(), "5.0 s");
        assert_eq!(AutoPowerOff::Off.to_string(), "Off");
        assert_eq!(AutoPowerOff::Min60.to_string(), "60 min");
        assert_eq!(CursorShift::Off.to_string(), "Off");
        assert_eq!(CursorShift::Seconds1_5.to_string(), "1.5 s");
        assert_eq!(SpeedDistanceUnit::MilesPerHour.to_string(), "mi/h , mile");
        assert_eq!(SpeedDistanceUnit::Knots.to_string(), "knots , nm");
        assert_eq!(AltitudeRainUnit::FeetInch.to_string(), "feet , inch");
        assert_eq!(AltitudeRainUnit::MetersMm.to_string(), "m , mm");
        assert_eq!(TemperatureUnit::Fahrenheit.to_string(), "degree F");
        assert_eq!(TemperatureUnit::Celsius.to_string(), "degree C");
        assert_eq!(Language::English.to_string(), "English");
        assert_eq!(Language::Japanese.to_string(), "Japanese");
    }

    #[test]
    fn display_labels_render_pf_key_and_interface_menu_text() {
        assert_eq!(FrontPanelPfFunction::Squelch.to_string(), "SQL");
        assert_eq!(FrontPanelPfFunction::Power.to_string(), "LOW");
        assert_eq!(
            FrontPanelPfFunction::Tone1750Hz.to_string(),
            "1,750 [Hz] Tone"
        );
        assert_eq!(FrontPanelPfFunction::MemoryInput.to_string(), "M.IN");
        assert_eq!(MicrophonePfFunction::BandSelect.to_string(), "A/B");
        assert_eq!(MicrophonePfFunction::Megahertz.to_string(), "MHz");
        assert_eq!(MicrophonePfFunction::Down.to_string(), "DOWN");
        assert_eq!(PcOutputInterface::Usb.to_string(), "USB");
        assert_eq!(PcOutputInterface::Bluetooth.to_string(), "Bluetooth");
        assert_eq!(
            UsbFunction::ComAndAudioOutput.to_string(),
            "COM + AF/IF Output"
        );
        assert_eq!(UsbFunction::MassStorage.to_string(), "Mass Storage");
    }
}
