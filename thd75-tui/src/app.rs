use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use kenwood_thd75::memory::MemoryImage;
use kenwood_thd75::types::{
    AfGainLevel, BatteryLevel, BeaconMode, Frequency, Mode, PowerLevel, SMeterReading,
    SquelchLevel, VoxDelay, VoxGain,
};

/// Path to the MCP cache file.
///
/// Platform cache directories (no `dirs` crate needed):
/// - macOS: `~/Library/Caches`
/// - Linux: `$XDG_CACHE_HOME` or `~/.cache`
/// - Windows: `%LOCALAPPDATA%`
fn cache_path() -> PathBuf {
    let base = cache_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("thd75-tui").join("mcp.bin")
}

/// Platform-specific cache directory.
fn cache_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}

/// Save raw MCP image to disk cache.
///
/// Logs errors but does not propagate — a failed cache write should not
/// block radio operation. The user will see a warning in the log.
pub(crate) fn save_cache(data: &[u8]) {
    let path = cache_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(path = %parent.display(), "failed to create cache dir: {e}");
        return;
    }
    if let Err(e) = std::fs::write(&path, data) {
        tracing::error!(path = %path.display(), "failed to write MCP cache: {e}");
    }
}

/// Load cached MCP image from disk. Returns (image, age).
pub(crate) fn load_cache() -> Option<(MemoryImage, std::time::Duration)> {
    let path = cache_path();
    let data = std::fs::read(&path).ok()?;
    let age = std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default();
    let image = MemoryImage::from_raw(data).ok()?;
    Some((image, age))
}

/// Number of rows in the settings list (must match `SettingRow::ALL.len()`).
pub(crate) const SETTINGS_COUNT: usize = 92;

/// Settings row identifiers for the interactive settings list.
///
/// Organized by the radio's menu groups. Settings backed by CAT commands are
/// noted; all others modify the in-memory MCP image and require an MCP write
/// to take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingRow {
    // --- RX ---
    /// Squelch level Band A (CAT: SQ band,level).
    SquelchA,
    /// Squelch level Band B (CAT: SQ band,level).
    SquelchB,
    /// Step size Band A (CAT: SF read/write).
    StepSizeA,
    /// Step size Band B (CAT: SF read/write).
    StepSizeB,
    /// Fine step (CAT: FS read-only, no band parameter).
    FineStep,
    /// Filter width SSB (CAT: SH read-only).
    FilterWidthSsb,
    /// Filter width CW (CAT: SH read-only).
    FilterWidthCw,
    /// Filter width AM (CAT: SH read-only).
    FilterWidthAm,
    /// FM narrow (MCP only).
    FmNarrow,
    /// SSB high-cut filter (MCP only).
    SsbHighCut,
    /// CW high-cut filter (MCP only).
    CwHighCut,
    /// AM high-cut filter (MCP only).
    AmHighCut,
    /// Auto filter (MCP only).
    AutoFilter,

    // --- Scan ---
    /// Scan resume mode (MCP only).
    ScanResume,
    /// Digital scan resume (MCP only).
    DigitalScanResume,
    /// Scan restart time (MCP only).
    ScanRestartTime,
    /// Scan restart carrier (MCP only).
    ScanRestartCarrier,

    // --- TX ---
    /// Timeout timer (MCP only).
    TimeoutTimer,
    /// TX inhibit (MCP only).
    TxInhibit,
    /// Beat shift (MCP only).
    BeatShift,

    // --- VOX ---
    /// VOX enabled (CAT: VX).
    VoxEnabled,
    /// VOX gain 0-9 (CAT: VG).
    VoxGain,
    /// VOX delay ×100ms (CAT: VD).
    VoxDelay,
    /// VOX TX on busy (MCP only).
    VoxTxOnBusy,

    // --- CW ---
    /// CW break-in (MCP only).
    CwBreakIn,
    /// CW delay time (MCP only).
    CwDelayTime,
    /// CW pitch (MCP only).
    CwPitch,

    // --- DTMF ---
    /// DTMF speed (MCP only).
    DtmfSpeed,
    /// DTMF pause time (MCP only).
    DtmfPauseTime,
    /// DTMF TX hold (MCP only).
    DtmfTxHold,

    // --- Repeater ---
    /// Repeater auto offset (MCP only).
    RepeaterAutoOffset,
    /// Repeater call key function (MCP only).
    RepeaterCallKey,

    // --- Auxiliary ---
    /// Microphone sensitivity (MCP only).
    MicSensitivity,
    /// PF key 1 assignment (MCP only).
    PfKey1,
    /// PF key 2 assignment (MCP only).
    PfKey2,

    // --- Lock ---
    /// Lock (CAT: LC).
    Lock,
    /// Key lock type (MCP only).
    KeyLockType,
    /// Lock key A (MCP only).
    LockKeyA,
    /// Lock key B (MCP only).
    LockKeyB,
    /// Lock key C (MCP only).
    LockKeyC,
    /// Lock PTT (MCP only).
    LockPtt,
    /// APRS lock (MCP only).
    AprsLock,

    // --- Display ---
    /// Dual display size (MCP only).
    DualDisplaySize,
    /// Display area (MCP only).
    DisplayArea,
    /// Info line (MCP only).
    InfoLine,
    /// Backlight control (MCP only).
    BacklightControl,
    /// Backlight timer (MCP only).
    BacklightTimer,
    /// Display hold time (MCP only).
    DisplayHoldTime,
    /// Display method (MCP only).
    DisplayMethod,
    /// Power-on display (MCP only).
    PowerOnDisplay,
    /// Dual band (CAT: DL).
    DualBand,

    // --- Audio ---
    /// EMR volume level (MCP only).
    EmrVolumeLevel,
    /// Auto mute return time (MCP only).
    AutoMuteReturnTime,
    /// Announce (MCP only).
    Announce,
    /// Key beep (MCP only).
    KeyBeep,
    /// Beep volume 1-7 (MCP only).
    BeepVolume,
    /// Voice language (MCP only).
    VoiceLanguage,
    /// Voice volume (MCP only).
    VoiceVolume,
    /// Voice speed (MCP only).
    VoiceSpeed,
    /// Volume lock (MCP only).
    VolumeLock,

    // --- Units ---
    /// Speed/distance unit (MCP only).
    SpeedDistanceUnit,
    /// Altitude/rain unit (MCP only).
    AltitudeRainUnit,
    /// Temperature unit (MCP only).
    TemperatureUnit,

    // --- Bluetooth ---
    /// Bluetooth (CAT: BT).
    Bluetooth,
    /// Bluetooth auto-connect (MCP only).
    BtAutoConnect,

    // --- Interface ---
    /// GPS/BT interface (MCP only).
    GpsBtInterface,
    /// PC output mode (MCP only).
    PcOutputMode,
    /// APRS USB mode (MCP only).
    AprsUsbMode,
    /// USB audio output (MCP only).
    UsbAudioOutput,
    /// Internet link (MCP only).
    InternetLink,

    // --- System ---
    /// Language (MCP only).
    Language,
    /// Power-on message flag (MCP only).
    PowerOnMessageFlag,

    // --- Battery ---
    /// Battery saver (MCP only).
    BatterySaver,
    /// Auto power off (MCP only).
    AutoPowerOff,

    // --- CAT-only Radio Controls ---
    /// Power level Band A (CAT: PC).
    PowerA,
    /// Power level Band B (CAT: PC).
    PowerB,
    /// Attenuator Band A (CAT: RA).
    AttenuatorA,
    /// Attenuator Band B (CAT: RA).
    AttenuatorB,
    /// Mode Band A (CAT: MD).
    ModeA,
    /// Mode Band B (CAT: MD).
    ModeB,
    /// Active band A/B (CAT: BC).
    ActiveBand,
    /// VFO/Memory mode Band A (CAT: VM).
    VfoMemModeA,
    /// VFO/Memory mode Band B (CAT: VM).
    VfoMemModeB,
    /// FM Radio on/off (CAT: FR).
    FmRadio,
    /// TNC baud rate (CAT: AS).
    TncBaud,
    /// Beacon type (CAT: PT).
    BeaconType,
    /// GPS enabled (CAT: GP).
    GpsEnabled,
    /// GPS PC output (CAT: GP).
    GpsPcOutput,
    /// Auto-info notifications (CAT: AI).
    AutoInfo,
    /// D-STAR callsign slot (CAT: CS).
    CallsignSlot,
    /// D-STAR slot (CAT: DS).
    DstarSlot,
    /// Scan resume method (CAT: SR write-only).
    ScanResumeCat,
}

impl SettingRow {
    /// All settings rows in display order.
    pub(crate) const ALL: [Self; SETTINGS_COUNT] = [
        // RX
        Self::SquelchA,
        Self::SquelchB,
        Self::StepSizeA,
        Self::StepSizeB,
        Self::FineStep,
        Self::FilterWidthSsb,
        Self::FilterWidthCw,
        Self::FilterWidthAm,
        Self::FmNarrow,
        Self::SsbHighCut,
        Self::CwHighCut,
        Self::AmHighCut,
        Self::AutoFilter,
        // Scan
        Self::ScanResume,
        Self::DigitalScanResume,
        Self::ScanRestartTime,
        Self::ScanRestartCarrier,
        // TX
        Self::TimeoutTimer,
        Self::TxInhibit,
        Self::BeatShift,
        // VOX
        Self::VoxEnabled,
        Self::VoxGain,
        Self::VoxDelay,
        Self::VoxTxOnBusy,
        // CW
        Self::CwBreakIn,
        Self::CwDelayTime,
        Self::CwPitch,
        // DTMF
        Self::DtmfSpeed,
        Self::DtmfPauseTime,
        Self::DtmfTxHold,
        // Repeater
        Self::RepeaterAutoOffset,
        Self::RepeaterCallKey,
        // Auxiliary
        Self::MicSensitivity,
        Self::PfKey1,
        Self::PfKey2,
        // Lock
        Self::Lock,
        Self::KeyLockType,
        Self::LockKeyA,
        Self::LockKeyB,
        Self::LockKeyC,
        Self::LockPtt,
        Self::AprsLock,
        // Display
        Self::DualDisplaySize,
        Self::DisplayArea,
        Self::InfoLine,
        Self::BacklightControl,
        Self::BacklightTimer,
        Self::DisplayHoldTime,
        Self::DisplayMethod,
        Self::PowerOnDisplay,
        Self::DualBand,
        // Audio
        Self::EmrVolumeLevel,
        Self::AutoMuteReturnTime,
        Self::Announce,
        Self::KeyBeep,
        Self::BeepVolume,
        Self::VoiceLanguage,
        Self::VoiceVolume,
        Self::VoiceSpeed,
        Self::VolumeLock,
        // Units
        Self::SpeedDistanceUnit,
        Self::AltitudeRainUnit,
        Self::TemperatureUnit,
        // Bluetooth
        Self::Bluetooth,
        Self::BtAutoConnect,
        // Interface
        Self::GpsBtInterface,
        Self::PcOutputMode,
        Self::AprsUsbMode,
        Self::UsbAudioOutput,
        Self::InternetLink,
        // System
        Self::Language,
        Self::PowerOnMessageFlag,
        // Battery
        Self::BatterySaver,
        Self::AutoPowerOff,
        // CAT Radio Controls
        Self::PowerA,
        Self::PowerB,
        Self::AttenuatorA,
        Self::AttenuatorB,
        Self::ModeA,
        Self::ModeB,
        Self::ActiveBand,
        Self::VfoMemModeA,
        Self::VfoMemModeB,
        Self::FmRadio,
        Self::TncBaud,
        Self::BeaconType,
        Self::GpsEnabled,
        Self::GpsPcOutput,
        Self::AutoInfo,
        Self::CallsignSlot,
        Self::DstarSlot,
        Self::ScanResumeCat,
    ];

    /// Human-readable label for the setting.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::SquelchA => "Squelch A",
            Self::SquelchB => "Squelch B",
            Self::StepSizeA => "Step Size A",
            Self::StepSizeB => "Step Size B",
            Self::FineStep => "Fine Step",
            Self::FilterWidthSsb => "Filter Width SSB",
            Self::FilterWidthCw => "Filter Width CW",
            Self::FilterWidthAm => "Filter Width AM",
            Self::FmNarrow => "FM Narrow",
            Self::SsbHighCut => "SSB High Cut",
            Self::CwHighCut => "CW High Cut",
            Self::AmHighCut => "AM High Cut",
            Self::AutoFilter => "Auto Filter",
            Self::ScanResume => "Scan Resume",
            Self::DigitalScanResume => "Digital Scan Resume",
            Self::ScanRestartTime => "Scan Restart Time",
            Self::ScanRestartCarrier => "Scan Restart Carrier",
            Self::TimeoutTimer => "Timeout Timer",
            Self::TxInhibit => "TX Inhibit",
            Self::BeatShift => "Beat Shift",
            Self::VoxEnabled => "VOX",
            Self::VoxGain => "VOX Gain",
            Self::VoxDelay => "VOX Delay",
            Self::VoxTxOnBusy => "VOX TX on Busy",
            Self::CwBreakIn => "CW Break-In",
            Self::CwDelayTime => "CW Delay Time",
            Self::CwPitch => "CW Pitch",
            Self::DtmfSpeed => "DTMF Speed",
            Self::DtmfPauseTime => "DTMF Pause Time",
            Self::DtmfTxHold => "DTMF TX Hold",
            Self::RepeaterAutoOffset => "Repeater Auto Offset",
            Self::RepeaterCallKey => "Call Key Function",
            Self::MicSensitivity => "Mic Sensitivity",
            Self::PfKey1 => "PF Key 1",
            Self::PfKey2 => "PF Key 2",
            Self::Lock => "Lock",
            Self::KeyLockType => "Key Lock Type",
            Self::LockKeyA => "Lock Key A",
            Self::LockKeyB => "Lock Key B",
            Self::LockKeyC => "Lock Key C",
            Self::LockPtt => "Lock PTT",
            Self::AprsLock => "APRS Lock",
            Self::DualDisplaySize => "Dual Display Size",
            Self::DisplayArea => "Display Area",
            Self::InfoLine => "Info Line",
            Self::BacklightControl => "Backlight Control",
            Self::BacklightTimer => "Backlight Timer",
            Self::DisplayHoldTime => "Display Hold Time",
            Self::DisplayMethod => "Display Method",
            Self::PowerOnDisplay => "Power-On Display",
            Self::DualBand => "Dual Band",
            Self::EmrVolumeLevel => "EMR Volume Level",
            Self::AutoMuteReturnTime => "Auto Mute Return",
            Self::Announce => "Announce",
            Self::KeyBeep => "Key Beep",
            Self::BeepVolume => "Beep Volume",
            Self::VoiceLanguage => "Voice Language",
            Self::VoiceVolume => "Voice Volume",
            Self::VoiceSpeed => "Voice Speed",
            Self::VolumeLock => "Volume Lock",
            Self::SpeedDistanceUnit => "Speed/Distance Unit",
            Self::AltitudeRainUnit => "Altitude/Rain Unit",
            Self::TemperatureUnit => "Temperature Unit",
            Self::Bluetooth => "Bluetooth",
            Self::BtAutoConnect => "BT Auto Connect",
            Self::GpsBtInterface => "GPS/BT Interface",
            Self::PcOutputMode => "PC Output Mode",
            Self::AprsUsbMode => "APRS USB Mode",
            Self::UsbAudioOutput => "USB Audio Output",
            Self::InternetLink => "Internet Link",
            Self::Language => "Language",
            Self::PowerOnMessageFlag => "Power-On Msg Flag",
            Self::BatterySaver => "Battery Saver",
            Self::AutoPowerOff => "Auto Power Off",
            Self::PowerA => "Power A",
            Self::PowerB => "Power B",
            Self::AttenuatorA => "Attenuator A",
            Self::AttenuatorB => "Attenuator B",
            Self::ModeA => "Mode A",
            Self::ModeB => "Mode B",
            Self::ActiveBand => "Active Band",
            Self::VfoMemModeA => "VFO/Mem A",
            Self::VfoMemModeB => "VFO/Mem B",
            Self::FmRadio => "FM Radio",
            Self::TncBaud => "TNC Baud",
            Self::BeaconType => "Beacon Type",
            Self::GpsEnabled => "GPS Enabled",
            Self::GpsPcOutput => "GPS PC Output",
            Self::AutoInfo => "Auto Info",
            Self::CallsignSlot => "Callsign Slot",
            Self::DstarSlot => "D-STAR Slot",
            Self::ScanResumeCat => "Scan Resume (CAT)",
        }
    }

    /// Section header label shown above this row. `None` means same group as previous row.
    pub(crate) const fn section_header(self) -> Option<&'static str> {
        match self {
            Self::SquelchA => Some("── RX ──"),
            Self::ScanResume => Some("── Scan ──"),
            Self::TimeoutTimer => Some("── TX ──"),
            Self::VoxEnabled => Some("── VOX ──"),
            Self::CwBreakIn => Some("── CW ──"),
            Self::DtmfSpeed => Some("── DTMF ──"),
            Self::RepeaterAutoOffset => Some("── Repeater ──"),
            Self::MicSensitivity => Some("── Auxiliary ──"),
            Self::Lock => Some("── Lock ──"),
            Self::DualDisplaySize => Some("── Display ──"),
            Self::EmrVolumeLevel => Some("── Audio ──"),
            Self::SpeedDistanceUnit => Some("── Units ──"),
            Self::Bluetooth => Some("── Bluetooth ──"),
            Self::GpsBtInterface => Some("── Interface ──"),
            Self::Language => Some("── System ──"),
            Self::BatterySaver => Some("── Battery ──"),
            Self::PowerA => Some("── Radio Controls ──"),
            _ => None,
        }
    }

    /// True if this setting is adjusted with +/- rather than toggled with Enter.
    pub(crate) const fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::SquelchA
                | Self::SquelchB
                | Self::StepSizeA
                | Self::StepSizeB
                | Self::ScanResumeCat
                | Self::FmNarrow
                | Self::SsbHighCut
                | Self::CwHighCut
                | Self::AmHighCut
                | Self::AutoFilter
                | Self::ScanResume
                | Self::DigitalScanResume
                | Self::ScanRestartTime
                | Self::ScanRestartCarrier
                | Self::TimeoutTimer
                | Self::VoxGain
                | Self::VoxDelay
                | Self::CwDelayTime
                | Self::CwPitch
                | Self::DtmfSpeed
                | Self::DtmfPauseTime
                | Self::RepeaterCallKey
                | Self::MicSensitivity
                | Self::PfKey1
                | Self::PfKey2
                | Self::KeyLockType
                | Self::DualDisplaySize
                | Self::DisplayArea
                | Self::InfoLine
                | Self::BacklightControl
                | Self::BacklightTimer
                | Self::DisplayHoldTime
                | Self::DisplayMethod
                | Self::PowerOnDisplay
                | Self::EmrVolumeLevel
                | Self::AutoMuteReturnTime
                | Self::BeepVolume
                | Self::VoiceLanguage
                | Self::VoiceVolume
                | Self::VoiceSpeed
                | Self::SpeedDistanceUnit
                | Self::AltitudeRainUnit
                | Self::TemperatureUnit
                | Self::GpsBtInterface
                | Self::PcOutputMode
                | Self::AprsUsbMode
                | Self::AutoPowerOff
                | Self::PowerA
                | Self::PowerB
                | Self::ModeA
                | Self::ModeB
                | Self::ActiveBand
                | Self::VfoMemModeA
                | Self::VfoMemModeB
                | Self::TncBaud
                | Self::BeaconType
                | Self::CallsignSlot
                | Self::DstarSlot
        )
    }

    /// True if this setting is writable via instant CAT command (no disconnect).
    pub(crate) const fn is_cat(self) -> bool {
        matches!(
            self,
            Self::SquelchA
                | Self::SquelchB
                | Self::StepSizeA
                | Self::StepSizeB
                | Self::FineStep
                | Self::FilterWidthSsb
                | Self::FilterWidthCw
                | Self::FilterWidthAm
                | Self::VoxEnabled
                | Self::VoxGain
                | Self::VoxDelay
                | Self::Lock
                | Self::DualBand
                | Self::Bluetooth
                | Self::PowerA
                | Self::PowerB
                | Self::AttenuatorA
                | Self::AttenuatorB
                | Self::ModeA
                | Self::ModeB
                | Self::ActiveBand
                | Self::VfoMemModeA
                | Self::VfoMemModeB
                | Self::FmRadio
                | Self::TncBaud
                | Self::BeaconType
                | Self::GpsEnabled
                | Self::GpsPcOutput
                | Self::AutoInfo
                | Self::CallsignSlot
                | Self::DstarSlot
                | Self::ScanResumeCat
        )
    }
}

/// Settings that use instant CAT writes (no disconnect).
pub(crate) fn cat_settings() -> Vec<SettingRow> {
    SettingRow::ALL
        .iter()
        .copied()
        .filter(|r| r.is_cat())
        .collect()
}

/// Settings that require MCP page write (~3s, brief disconnect).
pub(crate) fn mcp_settings() -> Vec<SettingRow> {
    SettingRow::ALL
        .iter()
        .copied()
        .filter(|r| !r.is_cat())
        .collect()
}

const fn on_off(b: bool) -> &'static str {
    if b { "On" } else { "Off" }
}

/// Which pane currently has input focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pane {
    BandA,
    BandB,
    Main,
    Detail,
}

impl Pane {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::BandA => Self::BandB,
            Self::BandB => Self::Main,
            Self::Main => Self::Detail,
            Self::Detail => Self::BandA,
        }
    }

    pub(crate) const fn prev(self) -> Self {
        match self {
            Self::BandA => Self::Detail,
            Self::BandB => Self::BandA,
            Self::Main => Self::BandB,
            Self::Detail => Self::Main,
        }
    }
}

/// Which view is shown in the main pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainView {
    Channels,
    /// CAT settings — instant, no disconnect.
    SettingsCat,
    /// MCP settings — ~3s per change, brief disconnect.
    SettingsMcp,
    Aprs,
    DStar,
    Gps,
    Mcp,
    /// FM broadcast radio control (76-108 MHz WFM on Band B).
    FmRadio,
}

/// Which field is selected in channel edit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelEditField {
    Frequency,
    Name,
    Mode,
    ToneMode,
    ToneFreq,
    Duplex,
    Offset,
}

impl ChannelEditField {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Frequency => Self::Name,
            Self::Name => Self::Mode,
            Self::Mode => Self::ToneMode,
            Self::ToneMode => Self::ToneFreq,
            Self::ToneFreq => Self::Duplex,
            Self::Duplex => Self::Offset,
            Self::Offset => Self::Frequency,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Frequency => "Frequency",
            Self::Name => "Name",
            Self::Mode => "Mode",
            Self::ToneMode => "Tone Mode",
            Self::ToneFreq => "Tone Freq",
            Self::Duplex => "Duplex",
            Self::Offset => "Offset",
        }
    }
}

/// Input mode for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InputMode {
    Normal,
    /// Searching channels — buffer holds the search string.
    Search(String),
    /// Entering a frequency — buffer holds digits typed so far.
    FreqInput(String),
}

/// Live state for one band, updated by the radio poller.
#[derive(Debug, Clone)]
pub(crate) struct BandState {
    pub frequency: Frequency,
    pub mode: Mode,
    /// S-meter level (0–5). Driven by AI-pushed BY notifications, not polled.
    pub s_meter: SMeterReading,
    /// Squelch setting (0–6 on D75).
    pub squelch: SquelchLevel,
    pub power_level: PowerLevel,
    /// Squelch is open (receiving). Driven by AI-pushed BY notifications.
    pub busy: bool,
    pub attenuator: bool,
    pub step_size: Option<kenwood_thd75::types::StepSize>,
}

impl Default for BandState {
    fn default() -> Self {
        Self {
            frequency: Frequency::new(145_000_000),
            mode: Mode::Fm,
            s_meter: SMeterReading::new(0).unwrap(),
            squelch: SquelchLevel::new(0).unwrap(),
            power_level: PowerLevel::High,
            busy: false,
            attenuator: false,
            step_size: None,
        }
    }
}

/// Aggregated radio state from the poller.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct RadioState {
    pub band_a: BandState,
    pub band_b: BandState,
    pub battery_level: BatteryLevel,
    pub beep: bool,
    pub lock: bool,
    pub dual_band: bool,
    pub bluetooth: bool,
    pub vox: bool,
    pub vox_gain: VoxGain,
    pub vox_delay: VoxDelay,
    pub af_gain: AfGainLevel,
    pub firmware_version: String,
    pub radio_type: String,
    pub gps_enabled: bool,
    pub gps_pc_output: bool,
    /// NMEA sentence enable flags: (GGA, GLL, GSA, GSV, RMC, VTG).
    pub gps_sentences: Option<(bool, bool, bool, bool, bool, bool)>,
    /// GPS/Radio operating mode (GM read).
    pub gps_mode: Option<kenwood_thd75::types::GpsRadioMode>,
    pub beacon_type: BeaconMode,
    pub fine_step: Option<kenwood_thd75::types::FineStep>,
    pub filter_width_ssb: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_cw: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_am: Option<kenwood_thd75::types::FilterWidthIndex>,
    /// Last-written scan resume method (write-only, not readable from D75).
    pub scan_resume_cat: Option<kenwood_thd75::types::ScanResumeMethod>,
    /// D-STAR URCALL callsign (8-char, space-padded).
    pub dstar_urcall: String,
    /// D-STAR URCALL suffix (4-char, space-padded).
    pub dstar_urcall_suffix: String,
    /// D-STAR RPT1 callsign.
    pub dstar_rpt1: String,
    /// D-STAR RPT1 suffix.
    pub dstar_rpt1_suffix: String,
    /// D-STAR RPT2 callsign.
    pub dstar_rpt2: String,
    /// D-STAR RPT2 suffix.
    pub dstar_rpt2_suffix: String,
    /// D-STAR gateway mode.
    pub dstar_gateway_mode: Option<kenwood_thd75::types::DvGatewayMode>,
    /// Active D-STAR slot.
    pub dstar_slot: Option<kenwood_thd75::types::DstarSlot>,
    /// Active callsign slot.
    pub dstar_callsign_slot: Option<kenwood_thd75::types::CallsignSlot>,
}

impl Default for RadioState {
    fn default() -> Self {
        Self {
            band_a: BandState::default(),
            band_b: BandState::default(),
            battery_level: BatteryLevel::Empty,
            beep: false,
            lock: false,
            dual_band: false,
            bluetooth: false,
            vox: false,
            vox_gain: VoxGain::new(0).unwrap(),
            vox_delay: VoxDelay::new(0).unwrap(),
            af_gain: AfGainLevel::new(0),
            firmware_version: String::new(),
            radio_type: String::new(),
            gps_enabled: false,
            gps_pc_output: false,
            gps_sentences: None,
            gps_mode: None,
            beacon_type: BeaconMode::Off,
            fine_step: None,
            filter_width_ssb: None,
            filter_width_cw: None,
            filter_width_am: None,
            scan_resume_cat: None,
            dstar_urcall: String::new(),
            dstar_urcall_suffix: String::new(),
            dstar_rpt1: String::new(),
            dstar_rpt1_suffix: String::new(),
            dstar_rpt2: String::new(),
            dstar_rpt2_suffix: String::new(),
            dstar_gateway_mode: None,
            dstar_slot: None,
            dstar_callsign_slot: None,
        }
    }
}

/// Whether the D-STAR gateway is active in the radio task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DStarMode {
    /// Not in gateway mode — show CAT config view on the D-STAR panel.
    Inactive,
    /// Gateway mode active — `DStarGateway` is running in the radio task.
    Active,
}

/// Whether the APRS client is active in the radio task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AprsMode {
    /// Not in APRS mode — show MCP config view on the APRS panel.
    Inactive,
    /// APRS mode active — `AprsClient` is running in the radio task.
    Active,
}

/// Tracking state for a sent APRS message.
#[derive(Debug, Clone)]
pub(crate) struct AprsMessageStatus {
    /// Destination callsign.
    pub addressee: String,
    /// Message text.
    pub text: String,
    /// Message ID from the messenger.
    pub message_id: String,
    /// When the message was sent.
    #[allow(dead_code)]
    pub sent_at: Instant,
    /// Delivery state.
    pub state: AprsMessageState,
}

/// Delivery state for a tracked APRS message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AprsMessageState {
    /// Waiting for acknowledgement.
    Pending,
    /// Acknowledged by the remote station.
    Delivered,
    /// Rejected by the remote station.
    Rejected,
    /// Expired after exhausting all retries.
    Expired,
}

/// Cached APRS station for the TUI display.
///
/// The library's `StationEntry` uses `Instant` for timestamps which is
/// not useful for display. This caches the fields we need plus a
/// wall-clock time for "ago" display.
#[derive(Debug, Clone)]
pub(crate) struct AprsStationCache {
    pub callsign: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub speed_knots: Option<u16>,
    pub course_degrees: Option<u16>,
    pub symbol_table: Option<char>,
    pub symbol_code: Option<char>,
    pub comment: Option<String>,
    pub packet_count: u32,
    pub last_path: Vec<String>,
    pub last_heard: Instant,
}

/// MCP programming state machine.
#[derive(Debug)]
pub(crate) enum McpState {
    Idle,
    Reading { page: u16, total: u16 },
    Loaded { image: MemoryImage, modified: bool },
    Writing { page: u16, total: u16 },
    Reconnecting,
}

/// All events that can flow into the update loop.
#[derive(Debug)]
pub(crate) enum Message {
    Key(crossterm::event::KeyEvent),
    RadioUpdate(RadioState),
    RadioError(String),
    Disconnected,
    Reconnected,
    McpProgress {
        page: u16,
        total: u16,
    },
    McpReadComplete(Vec<u8>),
    McpWriteComplete,
    /// A single MCP byte was written successfully — update the in-memory
    /// cache without requiring a full re-read.
    McpByteWritten {
        offset: u16,
        value: u8,
    },
    McpError(String),
    /// The radio task has entered APRS mode successfully.
    AprsStarted,
    /// The radio task has exited APRS mode.
    AprsStopped,
    /// An APRS event was received from the radio task.
    AprsEvent(kenwood_thd75::AprsEvent),
    /// An APRS message was sent and assigned a message ID for tracking.
    AprsMessageSent {
        addressee: String,
        text: String,
        message_id: String,
    },
    /// Error from the APRS subsystem.
    AprsError(String),
    /// The radio task has entered D-STAR gateway mode successfully.
    DStarStarted,
    /// The radio task has exited D-STAR gateway mode.
    DStarStopped,
    /// A D-STAR event was received from the radio task (gateway mode).
    DStarEvent(kenwood_thd75::DStarEvent),
    /// Error from the D-STAR subsystem.
    DStarError(String),
    Quit,
}

/// Central application state.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct App {
    pub connected: bool,
    pub port_path: String,
    pub state: RadioState,
    pub focus: Pane,
    pub main_view: MainView,
    pub input_mode: InputMode,
    pub mcp: McpState,
    pub should_quit: bool,
    pub quit_pending: bool,
    pub status_message: Option<String>,
    pub show_help: bool,
    pub channel_list_index: usize,
    /// Selected row in the CAT settings viewer.
    pub settings_cat_index: usize,
    /// Selected row in the MCP settings viewer.
    pub settings_mcp_index: usize,
    /// Active search filter for channel list (empty = show all).
    pub search_filter: String,
    /// Which band channel-tune and freq-input target (last focused band pane).
    pub target_band: kenwood_thd75::types::Band,
    /// Sender for commands to the radio background task.
    pub cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<crate::event::RadioCommand>>,
    /// APRS mode state.
    pub aprs_mode: AprsMode,
    /// Cached APRS stations, sorted by last heard (most recent first).
    pub aprs_stations: Vec<AprsStationCache>,
    /// Tracked sent APRS messages.
    pub aprs_messages: Vec<AprsMessageStatus>,
    /// Selected station index in the APRS station list.
    pub aprs_station_index: usize,
    /// When set, the APRS message compose prompt is active.
    pub aprs_compose: Option<String>,
    /// D-STAR mode state.
    pub dstar_mode: DStarMode,
    /// D-STAR last heard entries (gateway mode).
    pub dstar_last_heard: Vec<kenwood_thd75::LastHeardEntry>,
    /// Selected index in the D-STAR last heard list.
    pub dstar_last_heard_index: usize,
    /// Current D-STAR text message (from slow data).
    pub dstar_text_message: Option<String>,
    /// Current D-STAR RX header (gateway mode).
    pub dstar_rx_header: Option<dstar_gateway_core::DStarHeader>,
    /// Whether a D-STAR voice transmission is active.
    pub dstar_rx_active: bool,
    /// D-STAR URCALL input buffer (when prompting).
    pub dstar_urcall_input: Option<String>,
    /// D-STAR reflector input buffer (when prompting).
    pub dstar_reflector_input: Option<String>,
    /// Channel edit mode is active.
    pub channel_edit_mode: bool,
    /// Which field is selected in channel edit mode.
    pub channel_edit_field: ChannelEditField,
    /// Text buffer for the currently edited field.
    pub channel_edit_buffer: String,
    /// FM radio status (true = on). Tracked locally since FR is write-only.
    pub fm_radio_on: bool,
}

impl App {
    /// Returns the list of used channel numbers, filtered by `search_filter`.
    pub(crate) fn filtered_channels(&self) -> Vec<u16> {
        if let McpState::Loaded { ref image, .. } = self.mcp {
            let channels = image.channels();
            let filter = self.search_filter.to_uppercase();
            (0u16..1200)
                .filter(|&i| {
                    if !channels.is_used(i) {
                        return false;
                    }
                    if filter.is_empty() {
                        return true;
                    }
                    // Match against channel name or number
                    if let Some(entry) = channels.get(i) {
                        entry.name.to_uppercase().contains(&filter)
                            || i.to_string().contains(&filter)
                    } else {
                        false
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    fn used_channel_count(&self) -> usize {
        self.filtered_channels().len()
    }

    /// Create a new app instance, loading MCP cache from disk if available.
    pub(crate) fn new(port_path: String) -> Self {
        let (mcp, status_message) = match load_cache() {
            Some((image, age)) => {
                let mins = age.as_secs() / 60;
                let msg = if mins < 60 {
                    format!("Loaded cached MCP data ({mins}m ago)")
                } else if mins < 1440 {
                    format!("Loaded cached MCP data ({}h ago)", mins / 60)
                } else {
                    format!("Loaded cached MCP data ({}d ago)", mins / 1440)
                };
                (
                    McpState::Loaded {
                        image,
                        modified: false,
                    },
                    Some(msg),
                )
            }
            None => (McpState::Idle, None),
        };

        Self {
            connected: false,
            port_path,
            state: RadioState::default(),
            focus: Pane::BandA,
            main_view: MainView::Channels,
            input_mode: InputMode::Normal,
            mcp,
            should_quit: false,
            quit_pending: false,
            status_message,
            show_help: false,
            channel_list_index: 0,
            settings_cat_index: 0,
            settings_mcp_index: 0,
            search_filter: String::new(),
            target_band: kenwood_thd75::types::Band::A,
            cmd_tx: None,
            aprs_mode: AprsMode::Inactive,
            aprs_stations: Vec::new(),
            aprs_messages: Vec::new(),
            aprs_station_index: 0,
            aprs_compose: None,
            dstar_mode: DStarMode::Inactive,
            dstar_last_heard: Vec::new(),
            dstar_last_heard_index: 0,
            dstar_text_message: None,
            dstar_rx_header: None,
            dstar_rx_active: false,
            dstar_urcall_input: None,
            dstar_reflector_input: None,
            channel_edit_mode: false,
            channel_edit_field: ChannelEditField::Frequency,
            channel_edit_buffer: String::new(),
            fm_radio_on: false,
        }
    }

    /// Process a message and update state. Returns true if a render is needed.
    pub(crate) fn update(&mut self, msg: Message) -> bool {
        match msg {
            Message::Quit => {
                self.should_quit = true;
                true
            }
            Message::Key(key) => self.handle_key(key),
            Message::RadioUpdate(mut state) => {
                // Preserve static fields that are only read once at connect
                if state.firmware_version.is_empty() {
                    state.firmware_version = std::mem::take(&mut self.state.firmware_version);
                }
                if state.radio_type.is_empty() {
                    state.radio_type = std::mem::take(&mut self.state.radio_type);
                }
                // Preserve write-only fields not readable from radio
                if state.scan_resume_cat.is_none() {
                    state.scan_resume_cat = self.state.scan_resume_cat;
                }
                // Preserve D-STAR state when not provided by poll
                if state.dstar_urcall.is_empty() {
                    state.dstar_urcall = std::mem::take(&mut self.state.dstar_urcall);
                    state.dstar_urcall_suffix = std::mem::take(&mut self.state.dstar_urcall_suffix);
                }
                if state.dstar_rpt1.is_empty() {
                    state.dstar_rpt1 = std::mem::take(&mut self.state.dstar_rpt1);
                    state.dstar_rpt1_suffix = std::mem::take(&mut self.state.dstar_rpt1_suffix);
                }
                if state.dstar_rpt2.is_empty() {
                    state.dstar_rpt2 = std::mem::take(&mut self.state.dstar_rpt2);
                    state.dstar_rpt2_suffix = std::mem::take(&mut self.state.dstar_rpt2_suffix);
                }
                if state.dstar_gateway_mode.is_none() {
                    state.dstar_gateway_mode = self.state.dstar_gateway_mode;
                }
                if state.dstar_slot.is_none() {
                    state.dstar_slot = self.state.dstar_slot;
                }
                if state.dstar_callsign_slot.is_none() {
                    state.dstar_callsign_slot = self.state.dstar_callsign_slot;
                }
                self.state = state;
                self.connected = true;
                true
            }
            Message::RadioError(err) => {
                self.status_message = Some(err);
                true
            }
            Message::Disconnected => {
                self.connected = false;
                self.status_message = Some("Disconnected — reconnecting...".into());
                true
            }
            Message::Reconnected => {
                self.connected = true;
                self.status_message = Some("Reconnected".into());
                true
            }
            Message::McpProgress { page, total } => {
                self.mcp = if matches!(self.mcp, McpState::Writing { .. }) {
                    McpState::Writing { page, total }
                } else {
                    McpState::Reading { page, total }
                };
                true
            }
            Message::McpReadComplete(data) => {
                save_cache(&data);
                match MemoryImage::from_raw(data) {
                    Ok(image) => {
                        self.mcp = McpState::Loaded {
                            image,
                            modified: false,
                        };
                        self.status_message = Some("MCP read complete — cached to disk".into());
                    }
                    Err(e) => {
                        self.mcp = McpState::Idle;
                        self.status_message = Some(format!("MCP parse error: {e}"));
                    }
                }
                true
            }
            Message::McpWriteComplete => {
                self.mcp = McpState::Reconnecting;
                self.status_message = Some("MCP write complete — reconnecting...".into());
                true
            }
            Message::McpByteWritten { offset, value } => {
                // Update the cached memory image with the single byte that
                // was just written via MCP, so the TUI stays in sync without
                // requiring a full re-read after reconnect.
                if let McpState::Loaded { ref mut image, .. } = self.mcp {
                    image.as_raw_mut()[offset as usize] = value;
                    save_cache(image.as_raw());
                }
                true
            }
            Message::McpError(err) => {
                // Only reset to Idle if we don't have a loaded image.
                // A failed MCP write shouldn't destroy the cached data.
                if !matches!(self.mcp, McpState::Loaded { .. }) {
                    self.mcp = McpState::Idle;
                }
                self.status_message = Some(format!("MCP error: {err}"));
                true
            }
            Message::AprsStarted => {
                self.aprs_mode = AprsMode::Active;
                self.status_message = Some("APRS mode active".into());
                true
            }
            Message::AprsStopped => {
                self.aprs_mode = AprsMode::Inactive;
                self.status_message = Some("APRS mode stopped — CAT polling resumed".into());
                true
            }
            Message::AprsEvent(event) => {
                self.handle_aprs_event(event);
                true
            }
            Message::AprsMessageSent {
                addressee,
                text,
                message_id,
            } => {
                self.aprs_messages.push(AprsMessageStatus {
                    addressee,
                    text,
                    message_id,
                    sent_at: Instant::now(),
                    state: AprsMessageState::Pending,
                });
                true
            }
            Message::AprsError(err) => {
                self.status_message = Some(format!("APRS: {err}"));
                true
            }
            Message::DStarStarted => {
                self.dstar_mode = DStarMode::Active;
                self.status_message = Some("D-STAR gateway mode active".into());
                true
            }
            Message::DStarStopped => {
                self.dstar_mode = DStarMode::Inactive;
                self.dstar_rx_active = false;
                self.dstar_rx_header = None;
                self.status_message =
                    Some("D-STAR gateway mode stopped — CAT polling resumed".into());
                true
            }
            Message::DStarEvent(event) => {
                self.handle_dstar_event(event);
                true
            }
            Message::DStarError(err) => {
                self.status_message = Some(format!("D-STAR: {err}"));
                true
            }
        }
    }

    #[allow(clippy::cognitive_complexity)]
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        use crossterm::event::{KeyCode, KeyModifiers};

        // Ctrl-C always quits regardless of mode
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return true;
        }

        // Handle search input mode
        if let InputMode::Search(ref mut buf) = self.input_mode {
            match key.code {
                KeyCode::Esc => {
                    self.search_filter.clear();
                    self.input_mode = InputMode::Normal;
                    self.channel_list_index = 0;
                }
                KeyCode::Enter => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                    self.search_filter = buf.clone();
                    self.channel_list_index = 0;
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    self.search_filter = buf.clone();
                    self.channel_list_index = 0;
                }
                _ => {}
            }
            return true;
        }

        // Handle APRS message compose mode
        if let Some(ref mut buf) = self.aprs_compose {
            match key.code {
                KeyCode::Esc => {
                    self.aprs_compose = None;
                }
                KeyCode::Enter => {
                    let text = buf.clone();
                    self.aprs_compose = None;
                    if !text.is_empty()
                        && let Some(station) = self.aprs_stations.get(self.aprs_station_index)
                        && let Some(ref tx) = self.cmd_tx
                    {
                        let addressee = station.callsign.clone();
                        let _ = tx.send(crate::event::RadioCommand::SendAprsMessage {
                            addressee: addressee.clone(),
                            text: text.clone(),
                        });
                        self.status_message = Some(format!("Sending to {addressee}: {text}"));
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                }
                _ => {}
            }
            return true;
        }

        // Handle D-STAR URCALL input mode
        if let Some(ref mut buf) = self.dstar_urcall_input {
            match key.code {
                KeyCode::Esc => {
                    self.dstar_urcall_input = None;
                }
                KeyCode::Enter => {
                    let input = buf.clone();
                    self.dstar_urcall_input = None;
                    if !input.is_empty()
                        && let Some(ref tx) = self.cmd_tx
                    {
                        let _ = tx.send(crate::event::RadioCommand::SetUrcall {
                            callsign: input.clone(),
                            suffix: String::new(),
                        });
                        self.status_message = Some(format!("URCALL set to {input}"));
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    if buf.len() < 8 {
                        buf.push(c.to_ascii_uppercase());
                    }
                }
                _ => {}
            }
            return true;
        }

        // Handle D-STAR reflector input mode (format: NAME MODULE, e.g. "REF030 C")
        if let Some(ref mut buf) = self.dstar_reflector_input {
            match key.code {
                KeyCode::Esc => {
                    self.dstar_reflector_input = None;
                }
                KeyCode::Enter => {
                    let input = buf.clone();
                    self.dstar_reflector_input = None;
                    // Parse "REF030 C" or "REF030C"
                    let parts: Vec<&str> = input.split_whitespace().collect();
                    let (name, module) = if parts.len() >= 2 {
                        (parts[0].to_string(), parts[1].chars().next().unwrap_or('A'))
                    } else if input.len() > 1 {
                        let module = input.chars().last().unwrap_or('A');
                        let name = &input[..input.len() - 1];
                        (name.trim().to_string(), module)
                    } else {
                        self.status_message = Some("Invalid reflector (e.g. REF030 C)".into());
                        return true;
                    };
                    if let Some(ref tx) = self.cmd_tx {
                        let _ = tx.send(crate::event::RadioCommand::ConnectReflector {
                            name: name.clone(),
                            module,
                        });
                        self.status_message =
                            Some(format!("Connecting to {name} module {module}..."));
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    if buf.len() < 12 {
                        buf.push(c.to_ascii_uppercase());
                    }
                }
                _ => {}
            }
            return true;
        }

        // Handle channel edit mode
        if self.channel_edit_mode {
            match key.code {
                KeyCode::Esc => {
                    self.channel_edit_mode = false;
                    self.channel_edit_buffer.clear();
                    self.status_message = Some("Edit cancelled".into());
                }
                KeyCode::Tab => {
                    self.channel_edit_field = self.channel_edit_field.next();
                    self.channel_edit_buffer.clear();
                    self.status_message = Some(format!(
                        "Editing: {} (type value, Enter to apply)",
                        self.channel_edit_field.label()
                    ));
                }
                KeyCode::Backspace => {
                    let _ = self.channel_edit_buffer.pop();
                }
                KeyCode::Char(c) => {
                    self.channel_edit_buffer.push(c);
                }
                KeyCode::Enter => {
                    let field = self.channel_edit_field;
                    let buf = self.channel_edit_buffer.clone();
                    self.apply_channel_edit(field, &buf);
                    self.channel_edit_buffer.clear();
                }
                _ => {}
            }
            return true;
        }

        // Handle frequency input mode
        if let InputMode::FreqInput(ref mut buf) = self.input_mode {
            match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Enter => {
                    // Parse as MHz (e.g. "145.19" -> 145_190_000 Hz)
                    if let Ok(mhz) = buf.parse::<f64>() {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let hz = (mhz * 1_000_000.0) as u32;
                        if let Some(ref tx) = self.cmd_tx {
                            let _ = tx.send(crate::event::RadioCommand::TuneFreq {
                                band: self.target_band,
                                freq: hz,
                            });
                        }
                        let band_label = if self.target_band == kenwood_thd75::types::Band::B {
                            "B"
                        } else {
                            "A"
                        };
                        self.status_message =
                            Some(format!("Tuning Band {band_label} to {mhz:.6} MHz..."));
                    } else {
                        self.status_message = Some(format!("Invalid frequency: {buf}"));
                    }
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                    buf.push(c);
                }
                _ => {}
            }
            return true;
        }

        // --- Normal mode ---

        // Reset quit confirmation on any key that isn't 'q'
        if key.code != KeyCode::Char('q') {
            self.quit_pending = false;
        }

        match key.code {
            KeyCode::Char('q') => {
                if self.quit_pending {
                    self.should_quit = true;
                } else if let McpState::Loaded { modified: true, .. } = &self.mcp {
                    self.quit_pending = true;
                    self.status_message =
                        Some("Unsaved MCP changes! Press q again to confirm quit.".into());
                } else {
                    self.should_quit = true;
                }
                true
            }
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                true
            }
            KeyCode::Tab => {
                self.focus = self.focus.next();
                if self.focus == Pane::BandA {
                    self.target_band = kenwood_thd75::types::Band::A;
                }
                if self.focus == Pane::BandB {
                    self.target_band = kenwood_thd75::types::Band::B;
                }
                true
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                if self.focus == Pane::BandA {
                    self.target_band = kenwood_thd75::types::Band::A;
                }
                if self.focus == Pane::BandB {
                    self.target_band = kenwood_thd75::types::Band::B;
                }
                true
            }
            KeyCode::Char('1') => {
                self.focus = Pane::BandA;
                self.target_band = kenwood_thd75::types::Band::A;
                true
            }
            KeyCode::Char('2') => {
                self.focus = Pane::BandB;
                self.target_band = kenwood_thd75::types::Band::B;
                true
            }
            KeyCode::Char('3') => {
                self.focus = Pane::Main;
                true
            }
            KeyCode::Char('4') => {
                self.focus = Pane::Detail;
                true
            }
            KeyCode::Char('c') => {
                self.main_view = MainView::Channels;
                self.focus = Pane::Main;
                true
            }
            KeyCode::Char('s') => {
                self.main_view = MainView::SettingsCat;
                self.focus = Pane::Main;
                true
            }
            KeyCode::Char('S') => {
                self.main_view = MainView::SettingsMcp;
                self.focus = Pane::Main;
                true
            }
            // Channel edit mode: press 'e' on channel detail pane
            KeyCode::Char('e')
                if self.main_view == MainView::Channels
                    && matches!(self.focus, Pane::Main | Pane::Detail)
                    && matches!(self.mcp, McpState::Loaded { .. }) =>
            {
                let used = self.filtered_channels();
                if used.get(self.channel_list_index).is_some() {
                    self.channel_edit_mode = true;
                    self.channel_edit_field = ChannelEditField::Frequency;
                    self.channel_edit_buffer.clear();
                    self.status_message =
                        Some("Edit mode: Tab=next field, Enter=apply, Esc=cancel".into());
                }
                true
            }
            // FM Radio panel
            KeyCode::Char('F') => {
                self.main_view = MainView::FmRadio;
                self.focus = Pane::Main;
                true
            }
            // FM Radio toggle (when viewing FM panel)
            KeyCode::Char('f')
                if self.main_view == MainView::FmRadio && self.focus == Pane::Main =>
            {
                self.toggle_fm_radio();
                true
            }
            KeyCode::Char('a') => {
                if self.main_view == MainView::Aprs && self.focus == Pane::Main {
                    // Toggle APRS mode on/off when already viewing APRS panel.
                    self.toggle_aprs_mode();
                } else {
                    self.main_view = MainView::Aprs;
                    self.focus = Pane::Main;
                }
                true
            }
            KeyCode::Char('d') => {
                if self.main_view == MainView::DStar && self.focus == Pane::Main {
                    // Toggle D-STAR gateway mode on/off when already viewing D-STAR panel.
                    self.toggle_dstar_mode();
                } else {
                    self.main_view = MainView::DStar;
                    self.focus = Pane::Main;
                }
                true
            }
            KeyCode::Char('p') if self.main_view == MainView::Gps && self.focus == Pane::Main => {
                self.toggle_gps_pc_output();
                true
            }
            KeyCode::Char('m') => {
                self.main_view = MainView::Mcp;
                self.focus = Pane::Main;
                true
            }
            // Channel search
            KeyCode::Char('/')
                if self.focus == Pane::Main && self.main_view == MainView::Channels =>
            {
                self.input_mode = InputMode::Search(self.search_filter.clone());
                true
            }
            // Frequency direct entry
            KeyCode::Char('f') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                self.input_mode = InputMode::FreqInput(String::new());
                true
            }
            // GPS panel or jump-to-first-channel
            KeyCode::Char('g') if self.focus == Pane::Main => {
                if self.main_view == MainView::Gps {
                    // Toggle GPS on/off when already viewing GPS panel.
                    self.toggle_gps();
                } else if self.main_view == MainView::Channels {
                    // Jump to first channel in channel list.
                    self.channel_list_index = 0;
                } else {
                    // Switch to GPS view from any other panel.
                    self.main_view = MainView::Gps;
                }
                true
            }
            KeyCode::Char('g') => {
                // Switch to GPS view when focus is not on Main pane.
                self.main_view = MainView::Gps;
                self.focus = Pane::Main;
                true
            }
            KeyCode::Char('G') if self.focus == Pane::Main => {
                self.channel_list_index = self.used_channel_count().saturating_sub(1);
                true
            }
            KeyCode::Char('j') | KeyCode::Down => {
                match self.focus {
                    Pane::Main => match self.main_view {
                        MainView::Channels => {
                            let max = self.used_channel_count().saturating_sub(1);
                            self.channel_list_index =
                                self.channel_list_index.saturating_add(1).min(max);
                        }
                        MainView::SettingsCat => {
                            let max = cat_settings().len().saturating_sub(1);
                            self.settings_cat_index =
                                self.settings_cat_index.saturating_add(1).min(max);
                        }
                        MainView::SettingsMcp => {
                            let max = mcp_settings().len().saturating_sub(1);
                            self.settings_mcp_index =
                                self.settings_mcp_index.saturating_add(1).min(max);
                        }
                        MainView::Aprs => {
                            let max = self.aprs_stations.len().saturating_sub(1);
                            self.aprs_station_index =
                                self.aprs_station_index.saturating_add(1).min(max);
                        }
                        MainView::DStar => {
                            let max = self.dstar_last_heard.len().saturating_sub(1);
                            self.dstar_last_heard_index =
                                self.dstar_last_heard_index.saturating_add(1).min(max);
                        }
                        MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _ = tx.send(crate::event::RadioCommand::FreqDown(
                                kenwood_thd75::types::Band::A,
                            ));
                        }
                    }
                    Pane::BandB => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _ = tx.send(crate::event::RadioCommand::FreqDown(
                                kenwood_thd75::types::Band::B,
                            ));
                        }
                    }
                    Pane::Detail => {}
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    Pane::Main => match self.main_view {
                        MainView::Channels => {
                            self.channel_list_index = self.channel_list_index.saturating_sub(1);
                        }
                        MainView::SettingsCat => {
                            self.settings_cat_index = self.settings_cat_index.saturating_sub(1);
                        }
                        MainView::SettingsMcp => {
                            self.settings_mcp_index = self.settings_mcp_index.saturating_sub(1);
                        }
                        MainView::Aprs => {
                            self.aprs_station_index = self.aprs_station_index.saturating_sub(1);
                        }
                        MainView::DStar => {
                            self.dstar_last_heard_index =
                                self.dstar_last_heard_index.saturating_sub(1);
                        }
                        MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _ = tx.send(crate::event::RadioCommand::FreqUp(
                                kenwood_thd75::types::Band::A,
                            ));
                        }
                    }
                    Pane::BandB => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _ = tx.send(crate::event::RadioCommand::FreqUp(
                                kenwood_thd75::types::Band::B,
                            ));
                        }
                    }
                    Pane::Detail => {}
                }
                true
            }
            KeyCode::Enter if self.focus == Pane::Main && self.main_view == MainView::Channels => {
                let used = self.filtered_channels();
                if let Some(&ch_num) = used.get(self.channel_list_index)
                    && let Some(ref tx) = self.cmd_tx
                {
                    let band_label = if self.target_band == kenwood_thd75::types::Band::B {
                        "B"
                    } else {
                        "A"
                    };
                    let _ = tx.send(crate::event::RadioCommand::TuneChannel {
                        band: self.target_band,
                        channel: ch_num,
                    });
                    self.status_message =
                        Some(format!("Tuning Band {band_label} to channel {ch_num}..."));
                }
                true
            }
            // Settings: Enter toggles boolean, +/- adjusts numeric
            KeyCode::Enter
                if self.focus == Pane::Main
                    && matches!(
                        self.main_view,
                        MainView::SettingsCat | MainView::SettingsMcp
                    ) =>
            {
                self.toggle_setting();
                true
            }
            KeyCode::Char('+' | '=')
                if self.focus == Pane::Main
                    && matches!(
                        self.main_view,
                        MainView::SettingsCat | MainView::SettingsMcp
                    ) =>
            {
                self.adjust_setting(1);
                true
            }
            KeyCode::Char('-')
                if self.focus == Pane::Main
                    && matches!(
                        self.main_view,
                        MainView::SettingsCat | MainView::SettingsMcp
                    ) =>
            {
                self.adjust_setting(-1);
                true
            }
            KeyCode::Char('p') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let band = if self.focus == Pane::BandA {
                    kenwood_thd75::types::Band::A
                } else {
                    kenwood_thd75::types::Band::B
                };
                let current = if self.focus == Pane::BandA {
                    &self.state.band_a.power_level
                } else {
                    &self.state.band_b.power_level
                };
                let next = match current {
                    PowerLevel::High => PowerLevel::Medium,
                    PowerLevel::Medium => PowerLevel::Low,
                    PowerLevel::Low => PowerLevel::ExtraLow,
                    PowerLevel::ExtraLow => PowerLevel::High,
                };
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::SetPower { band, level: next });
                }
                true
            }
            // Attenuator toggle on band pane
            KeyCode::Char('t') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (kenwood_thd75::types::Band::A, self.state.band_a.attenuator)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.attenuator)
                };
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::SetAttenuator {
                        band,
                        enabled: !cur,
                    });
                    self.status_message = Some(format!("Attenuator → {}", on_off(!cur)));
                }
                true
            }
            // Squelch adjust on band pane: [ and ]
            KeyCode::Char('[') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (
                        kenwood_thd75::types::Band::A,
                        self.state.band_a.squelch.as_u8(),
                    )
                } else {
                    (
                        kenwood_thd75::types::Band::B,
                        self.state.band_b.squelch.as_u8(),
                    )
                };
                let next = cur.saturating_sub(1);
                if let (Some(tx), Ok(level)) = (&self.cmd_tx, SquelchLevel::new(next)) {
                    let _ = tx.send(crate::event::RadioCommand::SetSquelch { band, level });
                    self.status_message = Some(format!("Squelch → {next}"));
                }
                true
            }
            KeyCode::Char(']') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (
                        kenwood_thd75::types::Band::A,
                        self.state.band_a.squelch.as_u8(),
                    )
                } else {
                    (
                        kenwood_thd75::types::Band::B,
                        self.state.band_b.squelch.as_u8(),
                    )
                };
                let next = cur.saturating_add(1).min(6);
                if let (Some(tx), Ok(level)) = (&self.cmd_tx, SquelchLevel::new(next)) {
                    let _ = tx.send(crate::event::RadioCommand::SetSquelch { band, level });
                    self.status_message = Some(format!("Squelch → {next}"));
                }
                true
            }
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                    return true;
                }
                // Clear search filter
                if !self.search_filter.is_empty() {
                    self.search_filter.clear();
                    self.channel_list_index = 0;
                    return true;
                }
                false
            }
            // APRS: compose message to selected station
            KeyCode::Char('M')
                if self.main_view == MainView::Aprs
                    && self.focus == Pane::Main
                    && self.aprs_mode == AprsMode::Active
                    && !self.aprs_stations.is_empty() =>
            {
                self.aprs_compose = Some(String::new());
                true
            }
            // APRS: manual position beacon
            KeyCode::Char('b')
                if self.main_view == MainView::Aprs
                    && self.focus == Pane::Main
                    && self.aprs_mode == AprsMode::Active =>
            {
                if let Some(ref tx) = self.cmd_tx {
                    // Use 0,0 as placeholder — real GPS position would come from the radio.
                    let _ = tx.send(crate::event::RadioCommand::BeaconPosition {
                        lat: 0.0,
                        lon: 0.0,
                        comment: String::new(),
                    });
                    self.status_message = Some("Beacon sent".into());
                }
                true
            }
            KeyCode::Char('r') if self.main_view == MainView::Mcp => {
                if matches!(self.mcp, McpState::Idle | McpState::Loaded { .. }) {
                    self.mcp = McpState::Reading {
                        page: 0,
                        total: kenwood_thd75::protocol::programming::TOTAL_PAGES,
                    };
                    self.status_message = Some("Starting MCP read...".into());
                    if let Some(ref tx) = self.cmd_tx {
                        let _ = tx.send(crate::event::RadioCommand::ReadMemory);
                    }
                }
                true
            }
            KeyCode::Char('w') if self.main_view == MainView::Mcp => {
                if let McpState::Loaded { ref image, .. } = self.mcp {
                    let data = image.as_raw().to_vec();
                    self.mcp = McpState::Writing {
                        page: 0,
                        total: kenwood_thd75::protocol::programming::TOTAL_PAGES,
                    };
                    self.status_message = Some("Starting MCP write...".into());
                    if let Some(ref tx) = self.cmd_tx {
                        let _ = tx.send(crate::event::RadioCommand::WriteMemory(data));
                    }
                }
                true
            }
            // D-STAR: set CQ (URCALL = CQCQCQ)
            KeyCode::Char('C')
                if self.main_view == MainView::DStar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DStarMode::Inactive =>
            {
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::SetCQ);
                    self.status_message = Some("URCALL set to CQCQCQ".into());
                }
                true
            }
            // D-STAR: set URCALL (prompt)
            KeyCode::Char('u')
                if self.main_view == MainView::DStar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DStarMode::Inactive =>
            {
                self.dstar_urcall_input = Some(String::new());
                true
            }
            // D-STAR: connect reflector (prompt)
            KeyCode::Char('r')
                if self.main_view == MainView::DStar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DStarMode::Inactive =>
            {
                self.dstar_reflector_input = Some(String::new());
                true
            }
            // D-STAR: unlink reflector
            KeyCode::Char('U')
                if self.main_view == MainView::DStar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DStarMode::Inactive =>
            {
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::DisconnectReflector);
                    self.status_message = Some("Unlinking reflector...".into());
                }
                true
            }
            _ => false,
        }
    }

    /// Toggle a boolean setting or show hint for numeric ones.
    fn toggle_setting(&mut self) {
        let (rows, idx) = if self.main_view == MainView::SettingsCat {
            (cat_settings(), self.settings_cat_index)
        } else {
            (mcp_settings(), self.settings_mcp_index)
        };
        let row = match rows.get(idx) {
            Some(r) => *r,
            None => return,
        };

        // CAT-backed boolean settings
        if let Some(ref tx) = self.cmd_tx.clone() {
            match row {
                SettingRow::Lock => {
                    let next = !self.state.lock;
                    let _ = tx.send(crate::event::RadioCommand::SetLock(next));
                    self.status_message = Some(format!("Lock → {}", on_off(next)));
                    return;
                }
                SettingRow::DualBand => {
                    let next = !self.state.dual_band;
                    let _ = tx.send(crate::event::RadioCommand::SetDualBand(next));
                    self.status_message = Some(format!("Dual band → {}", on_off(next)));
                    return;
                }
                SettingRow::Bluetooth => {
                    let next = !self.state.bluetooth;
                    let _ = tx.send(crate::event::RadioCommand::SetBluetooth(next));
                    self.status_message = Some(format!("Bluetooth → {}", on_off(next)));
                    return;
                }
                SettingRow::VoxEnabled => {
                    let next = !self.state.vox;
                    let _ = tx.send(crate::event::RadioCommand::SetVox(next));
                    self.status_message = Some(format!("VOX → {}", on_off(next)));
                    return;
                }
                SettingRow::AttenuatorA => {
                    let next = !self.state.band_a.attenuator;
                    let _ = tx.send(crate::event::RadioCommand::SetAttenuator {
                        band: kenwood_thd75::types::Band::A,
                        enabled: next,
                    });
                    self.status_message = Some(format!("Atten A → {}", on_off(next)));
                    return;
                }
                SettingRow::AttenuatorB => {
                    let next = !self.state.band_b.attenuator;
                    let _ = tx.send(crate::event::RadioCommand::SetAttenuator {
                        band: kenwood_thd75::types::Band::B,
                        enabled: next,
                    });
                    self.status_message = Some(format!("Atten B → {}", on_off(next)));
                    return;
                }
                SettingRow::FmRadio => {
                    let _ = tx.send(crate::event::RadioCommand::SetFmRadio(true));
                    self.status_message =
                        Some("FM Radio: enabled (read-back not available)".into());
                    return;
                }
                SettingRow::GpsEnabled => {
                    let next = !self.state.gps_enabled;
                    let _ = tx.send(crate::event::RadioCommand::SetGpsConfig(next, false));
                    self.status_message = Some(format!("GPS → {}", on_off(next)));
                    return;
                }
                SettingRow::GpsPcOutput => {
                    self.status_message =
                        Some("GPS PC Output: use SetGpsSentences — not yet wired".into());
                    return;
                }
                SettingRow::AutoInfo => {
                    self.status_message = Some("Auto Info: not yet wired".into());
                    return;
                }
                _ => {}
            }
        }

        // Numeric settings: show hint
        if row.is_numeric() {
            self.status_message = Some(format!("{}: use +/- to adjust", row.label()));
            return;
        }

        // MCP-backed boolean settings — write directly to radio via single-page MCP
        let Some(tx) = self.cmd_tx.clone() else {
            return;
        };

        let McpState::Loaded { ref mut image, .. } = self.mcp else {
            self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
            return;
        };

        macro_rules! toggle_bool {
            ($getter:ident, $setter:ident, $label:expr) => {{
                let new_val = !image.settings().$getter();
                if let Some((offset, value)) = image.modify_setting(|w| w.$setter(new_val)) {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message =
                        Some(format!("{} → {} — applying...", $label, on_off(new_val)));
                }
            }};
        }

        match row {
            SettingRow::TxInhibit => toggle_bool!(tx_inhibit, set_tx_inhibit, "TX Inhibit"),
            SettingRow::BeatShift => toggle_bool!(beat_shift, set_beat_shift, "Beat Shift"),
            SettingRow::VoxTxOnBusy => {
                toggle_bool!(vox_tx_on_busy, set_vox_tx_on_busy, "VOX TX Busy");
            }
            SettingRow::CwBreakIn => toggle_bool!(cw_break_in, set_cw_break_in, "CW Break-In"),
            SettingRow::DtmfTxHold => {
                toggle_bool!(dtmf_tx_hold, set_dtmf_tx_hold, "DTMF TX Hold");
            }
            SettingRow::RepeaterAutoOffset => {
                toggle_bool!(
                    repeater_auto_offset,
                    set_repeater_auto_offset,
                    "Rpt Auto Offset"
                );
            }
            SettingRow::LockKeyA => toggle_bool!(lock_key_a, set_lock_key_a, "Lock Key A"),
            SettingRow::LockKeyB => toggle_bool!(lock_key_b, set_lock_key_b, "Lock Key B"),
            SettingRow::LockKeyC => toggle_bool!(lock_key_c, set_lock_key_c, "Lock Key C"),
            SettingRow::LockPtt => toggle_bool!(lock_key_ptt, set_lock_key_ptt, "Lock PTT"),
            SettingRow::AprsLock => toggle_bool!(aprs_lock, set_aprs_lock, "APRS Lock"),
            SettingRow::Announce => toggle_bool!(announce, set_announce, "Announce"),
            SettingRow::KeyBeep => toggle_bool!(key_beep, set_key_beep, "Key Beep"),
            SettingRow::VolumeLock => toggle_bool!(volume_lock, set_volume_lock, "Vol Lock"),
            SettingRow::BtAutoConnect => {
                toggle_bool!(bt_auto_connect, set_bt_auto_connect, "BT Auto Connect");
            }
            SettingRow::UsbAudioOutput => {
                toggle_bool!(usb_audio_output, set_usb_audio_output, "USB Audio Out");
            }
            SettingRow::InternetLink => {
                toggle_bool!(internet_link, set_internet_link, "Internet Link");
            }
            SettingRow::PowerOnMessageFlag => {
                toggle_bool!(
                    power_on_message_flag,
                    set_power_on_message_flag,
                    "PowerOn Msg"
                );
            }
            SettingRow::BatterySaver => {
                toggle_bool!(battery_saver, set_battery_saver, "Battery Saver");
            }
            _ => {
                self.status_message = Some(format!("{}: use +/- to adjust", row.label()));
            }
        }
    }

    /// Adjust a numeric setting by delta with +/-.
    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn adjust_setting(&mut self, delta: i8) {
        let (rows, idx) = if self.main_view == MainView::SettingsCat {
            (cat_settings(), self.settings_cat_index)
        } else {
            (mcp_settings(), self.settings_mcp_index)
        };
        let row = match rows.get(idx) {
            Some(r) => *r,
            None => return,
        };

        // CAT-backed numeric settings
        if let Some(ref tx) = self.cmd_tx.clone() {
            match row {
                SettingRow::SquelchA => {
                    let cur = self.state.band_a.squelch.as_u8();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::A,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch A → {next}"));
                    return;
                }
                SettingRow::SquelchB => {
                    let cur = self.state.band_b.squelch.as_u8();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::B,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch B → {next}"));
                    return;
                }
                SettingRow::VoxGain => {
                    let cur = self.state.vox_gain.as_u8();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(9)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(gain) = VoxGain::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetVoxGain(gain));
                    }
                    self.status_message = Some(format!("VOX Gain → {next}"));
                    return;
                }
                SettingRow::VoxDelay => {
                    let cur = self.state.vox_delay.as_u8();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(30)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(delay) = VoxDelay::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetVoxDelay(delay));
                    }
                    self.status_message = Some(format!("VOX Delay → {next}"));
                    return;
                }
                SettingRow::StepSizeA => {
                    use kenwood_thd75::types::StepSize;
                    let steps = [
                        StepSize::Hz5000,
                        StepSize::Hz6250,
                        StepSize::Hz8330,
                        StepSize::Hz9000,
                        StepSize::Hz10000,
                        StepSize::Hz12500,
                        StepSize::Hz15000,
                        StepSize::Hz20000,
                        StepSize::Hz25000,
                        StepSize::Hz30000,
                        StepSize::Hz50000,
                        StepSize::Hz100000,
                    ];
                    let cur_idx = self
                        .state
                        .band_a
                        .step_size
                        .and_then(|s| steps.iter().position(|&x| x == s))
                        .unwrap_or(0);
                    let next_idx = if delta > 0 {
                        (cur_idx + 1).min(steps.len() - 1)
                    } else {
                        cur_idx.saturating_sub(1)
                    };
                    let next = steps[next_idx];
                    let _ = tx.send(crate::event::RadioCommand::SetStepSize {
                        band: kenwood_thd75::types::Band::A,
                        step: next,
                    });
                    self.status_message = Some(format!("Step A → {next}"));
                    return;
                }
                SettingRow::StepSizeB => {
                    use kenwood_thd75::types::StepSize;
                    let steps = [
                        StepSize::Hz5000,
                        StepSize::Hz6250,
                        StepSize::Hz8330,
                        StepSize::Hz9000,
                        StepSize::Hz10000,
                        StepSize::Hz12500,
                        StepSize::Hz15000,
                        StepSize::Hz20000,
                        StepSize::Hz25000,
                        StepSize::Hz30000,
                        StepSize::Hz50000,
                        StepSize::Hz100000,
                    ];
                    let cur_idx = self
                        .state
                        .band_b
                        .step_size
                        .and_then(|s| steps.iter().position(|&x| x == s))
                        .unwrap_or(0);
                    let next_idx = if delta > 0 {
                        (cur_idx + 1).min(steps.len() - 1)
                    } else {
                        cur_idx.saturating_sub(1)
                    };
                    let next = steps[next_idx];
                    let _ = tx.send(crate::event::RadioCommand::SetStepSize {
                        band: kenwood_thd75::types::Band::B,
                        step: next,
                    });
                    self.status_message = Some(format!("Step B → {next}"));
                    return;
                }
                SettingRow::FineStep => {
                    self.status_message = Some("Fine Step: read-only".into());
                    return;
                }
                SettingRow::FilterWidthSsb
                | SettingRow::FilterWidthCw
                | SettingRow::FilterWidthAm => {
                    self.status_message = Some("Filter Width: read-only".into());
                    return;
                }
                SettingRow::ScanResumeCat => {
                    use kenwood_thd75::types::ScanResumeMethod;
                    let methods = [
                        ScanResumeMethod::TimeOperated,
                        ScanResumeMethod::CarrierOperated,
                        ScanResumeMethod::Seek,
                    ];
                    let cur_idx = self
                        .state
                        .scan_resume_cat
                        .and_then(|m| methods.iter().position(|&x| x == m))
                        .unwrap_or(0);
                    let next_idx = if delta > 0 {
                        (cur_idx + 1) % methods.len()
                    } else {
                        (cur_idx + methods.len() - 1) % methods.len()
                    };
                    let next = methods[next_idx];
                    let _ = tx.send(crate::event::RadioCommand::SetScanResumeCat(next));
                    self.state.scan_resume_cat = Some(next);
                    let label = match next {
                        ScanResumeMethod::TimeOperated => "Time",
                        ScanResumeMethod::CarrierOperated => "Carrier",
                        ScanResumeMethod::Seek => "Seek",
                    };
                    self.status_message = Some(format!("Scan Resume → {label}"));
                    return;
                }
                SettingRow::PowerA => {
                    let next = match self.state.band_a.power_level {
                        PowerLevel::High => PowerLevel::Medium,
                        PowerLevel::Medium => PowerLevel::Low,
                        PowerLevel::Low => PowerLevel::ExtraLow,
                        PowerLevel::ExtraLow => PowerLevel::High,
                    };
                    let _ = tx.send(crate::event::RadioCommand::SetPower {
                        band: kenwood_thd75::types::Band::A,
                        level: next,
                    });
                    self.status_message = Some(format!("Power A → {next}"));
                    return;
                }
                SettingRow::PowerB => {
                    let next = match self.state.band_b.power_level {
                        PowerLevel::High => PowerLevel::Medium,
                        PowerLevel::Medium => PowerLevel::Low,
                        PowerLevel::Low => PowerLevel::ExtraLow,
                        PowerLevel::ExtraLow => PowerLevel::High,
                    };
                    let _ = tx.send(crate::event::RadioCommand::SetPower {
                        band: kenwood_thd75::types::Band::B,
                        level: next,
                    });
                    self.status_message = Some(format!("Power B → {next}"));
                    return;
                }
                SettingRow::ModeA => {
                    use kenwood_thd75::types::Mode;
                    let next = match self.state.band_a.mode {
                        Mode::Fm => Mode::Nfm,
                        Mode::Nfm => Mode::Am,
                        Mode::Am => Mode::Lsb,
                        Mode::Lsb => Mode::Usb,
                        Mode::Usb => Mode::Cw,
                        Mode::Cw => Mode::Dv,
                        Mode::Dv => Mode::Dr,
                        Mode::Dr => Mode::Wfm,
                        Mode::Wfm => Mode::CwReverse,
                        Mode::CwReverse => Mode::Fm,
                    };
                    let _ = tx.send(crate::event::RadioCommand::SetMode {
                        band: kenwood_thd75::types::Band::A,
                        mode: next,
                    });
                    self.status_message = Some(format!("Mode A → {next}"));
                    return;
                }
                SettingRow::ModeB => {
                    use kenwood_thd75::types::Mode;
                    let next = match self.state.band_b.mode {
                        Mode::Fm => Mode::Nfm,
                        Mode::Nfm => Mode::Am,
                        Mode::Am => Mode::Lsb,
                        Mode::Lsb => Mode::Usb,
                        Mode::Usb => Mode::Cw,
                        Mode::Cw => Mode::Dv,
                        Mode::Dv => Mode::Dr,
                        Mode::Dr => Mode::Wfm,
                        Mode::Wfm => Mode::CwReverse,
                        Mode::CwReverse => Mode::Fm,
                    };
                    let _ = tx.send(crate::event::RadioCommand::SetMode {
                        band: kenwood_thd75::types::Band::B,
                        mode: next,
                    });
                    self.status_message = Some(format!("Mode B → {next}"));
                    return;
                }
                SettingRow::ActiveBand => {
                    self.status_message =
                        Some("Active Band: use BC command — not yet wired".into());
                    return;
                }
                SettingRow::VfoMemModeA => {
                    self.status_message = Some("VFO/Mem A: use VM command — not yet wired".into());
                    return;
                }
                SettingRow::VfoMemModeB => {
                    self.status_message = Some("VFO/Mem B: use VM command — not yet wired".into());
                    return;
                }
                SettingRow::TncBaud => {
                    let baud = if delta > 0 {
                        kenwood_thd75::types::TncBaud::Bps9600
                    } else {
                        kenwood_thd75::types::TncBaud::Bps1200
                    };
                    let _ = tx.send(crate::event::RadioCommand::SetTncBaud(baud));
                    self.status_message = Some(format!("TNC Baud → {baud}"));
                    return;
                }
                SettingRow::BeaconType => {
                    let cur = u8::from(self.state.beacon_type);
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(4)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(mode) = BeaconMode::try_from(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetBeaconType(mode));
                        self.status_message = Some(format!("Beacon Type → {mode}"));
                    }
                    return;
                }
                SettingRow::CallsignSlot => {
                    self.status_message =
                        Some("Callsign Slot: not yet polled — cannot adjust".into());
                    return;
                }
                SettingRow::DstarSlot => {
                    self.status_message =
                        Some("D-STAR Slot: not yet polled — cannot adjust".into());
                    return;
                }
                _ => {}
            }
        }

        // MCP-backed numeric settings — write directly via single-page MCP
        let Some(tx) = self.cmd_tx.clone() else {
            return;
        };

        let McpState::Loaded { ref mut image, .. } = self.mcp else {
            self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
            return;
        };

        /// Compute a new numeric value by applying `delta` to the current value,
        /// then write it via `modify_setting`.
        macro_rules! adjust_numeric {
            ($getter:ident, $setter:ident, $label:expr, $image:expr, $delta:expr, $tx:expr) => {{
                let new_val = $image.settings().$getter().saturating_add_signed($delta);
                if let Some((offset, value)) = $image.modify_setting(|w| w.$setter(new_val)) {
                    let _ = $tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message =
                        Some(format!("{} → {} — applying...", $label, new_val));
                }
            }};
        }

        match row {
            SettingRow::FmNarrow => {
                adjust_numeric!(fm_narrow, set_fm_narrow, "FM Narrow", image, delta, tx);
            }
            SettingRow::SsbHighCut => {
                adjust_numeric!(
                    ssb_high_cut,
                    set_ssb_high_cut,
                    "SSB High Cut",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::CwHighCut => {
                adjust_numeric!(
                    cw_high_cut,
                    set_cw_high_cut,
                    "CW High Cut",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::AmHighCut => {
                adjust_numeric!(
                    am_high_cut,
                    set_am_high_cut,
                    "AM High Cut",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::AutoFilter => {
                adjust_numeric!(
                    auto_filter,
                    set_auto_filter,
                    "Auto Filter",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::ScanResume => {
                adjust_numeric!(
                    scan_resume,
                    set_scan_resume,
                    "Scan Resume",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::DigitalScanResume => {
                adjust_numeric!(
                    digital_scan_resume,
                    set_digital_scan_resume,
                    "Dig Scan Resume",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::ScanRestartTime => {
                adjust_numeric!(
                    scan_restart_time,
                    set_scan_restart_time,
                    "Scan Restart Time",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::ScanRestartCarrier => {
                adjust_numeric!(
                    scan_restart_carrier,
                    set_scan_restart_carrier,
                    "Scan Restart Carrier",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::TimeoutTimer => {
                adjust_numeric!(
                    timeout_timer,
                    set_timeout_timer,
                    "Timeout Timer",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::CwDelayTime => {
                adjust_numeric!(
                    cw_delay_time,
                    set_cw_delay_time,
                    "CW Delay",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::CwPitch => {
                adjust_numeric!(cw_pitch, set_cw_pitch, "CW Pitch", image, delta, tx);
            }
            SettingRow::DtmfSpeed => {
                adjust_numeric!(dtmf_speed, set_dtmf_speed, "DTMF Speed", image, delta, tx);
            }
            SettingRow::DtmfPauseTime => {
                adjust_numeric!(
                    dtmf_pause_time,
                    set_dtmf_pause_time,
                    "DTMF Pause",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::RepeaterCallKey => {
                adjust_numeric!(
                    repeater_call_key,
                    set_repeater_call_key,
                    "Call Key",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::MicSensitivity => {
                adjust_numeric!(
                    mic_sensitivity,
                    set_mic_sensitivity,
                    "Mic Sens",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::PfKey1 => {
                adjust_numeric!(pf_key1, set_pf_key1, "PF Key 1", image, delta, tx);
            }
            SettingRow::PfKey2 => {
                adjust_numeric!(pf_key2, set_pf_key2, "PF Key 2", image, delta, tx);
            }
            SettingRow::KeyLockType => {
                let new_val = image
                    .settings()
                    .key_lock_type_raw()
                    .saturating_add_signed(delta)
                    .min(2);
                if let Some((offset, value)) =
                    image.modify_setting(|w| w.set_key_lock_type_raw(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!("Lock Type → {new_val} — applying..."));
                }
            }
            SettingRow::DualDisplaySize => {
                adjust_numeric!(
                    dual_display_size,
                    set_dual_display_size,
                    "Dual Display",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::DisplayArea => {
                adjust_numeric!(
                    display_area,
                    set_display_area,
                    "Display Area",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::InfoLine => {
                adjust_numeric!(info_line, set_info_line, "Info Line", image, delta, tx);
            }
            SettingRow::BacklightControl => {
                adjust_numeric!(
                    backlight_control,
                    set_backlight_control,
                    "Backlight Ctrl",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::BacklightTimer => {
                adjust_numeric!(
                    backlight_timer,
                    set_backlight_timer,
                    "Backlight Timer",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::DisplayHoldTime => {
                adjust_numeric!(
                    display_hold_time,
                    set_display_hold_time,
                    "Display Hold",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::DisplayMethod => {
                adjust_numeric!(
                    display_method,
                    set_display_method,
                    "Display Method",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::PowerOnDisplay => {
                adjust_numeric!(
                    power_on_display,
                    set_power_on_display,
                    "PowerOn Display",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::EmrVolumeLevel => {
                adjust_numeric!(
                    emr_volume_level,
                    set_emr_volume_level,
                    "EMR Vol",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::AutoMuteReturnTime => {
                adjust_numeric!(
                    auto_mute_return_time,
                    set_auto_mute_return_time,
                    "Auto Mute",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::BeepVolume => {
                let cur = image.settings().beep_volume();
                let new_val = if delta > 0 {
                    cur.saturating_add(1).min(7)
                } else {
                    cur.saturating_sub(1).max(1)
                };
                if let Some((offset, value)) = image.modify_setting(|w| w.set_beep_volume(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!("Beep Vol → {new_val} — applying..."));
                }
            }
            SettingRow::VoiceLanguage => {
                adjust_numeric!(
                    voice_language,
                    set_voice_language,
                    "Voice Lang",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::VoiceVolume => {
                adjust_numeric!(
                    voice_volume,
                    set_voice_volume,
                    "Voice Vol",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::VoiceSpeed => {
                adjust_numeric!(
                    voice_speed,
                    set_voice_speed,
                    "Voice Speed",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::SpeedDistanceUnit => {
                let new_val = if delta > 0 {
                    image
                        .settings()
                        .speed_distance_unit_raw()
                        .saturating_add(1)
                        .min(2)
                } else {
                    image.settings().speed_distance_unit_raw().saturating_sub(1)
                };
                if let Some((offset, value)) =
                    image.modify_setting(|w| w.set_speed_distance_unit_raw(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!(
                        "Speed Unit → {} — applying...",
                        ["mph", "km/h", "knots"]
                            .get(new_val as usize)
                            .unwrap_or(&"?")
                    ));
                }
            }
            SettingRow::AltitudeRainUnit => {
                let new_val = if delta > 0 {
                    image
                        .settings()
                        .altitude_rain_unit_raw()
                        .saturating_add(1)
                        .min(1)
                } else {
                    image.settings().altitude_rain_unit_raw().saturating_sub(1)
                };
                if let Some((offset, value)) =
                    image.modify_setting(|w| w.set_altitude_rain_unit_raw(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!(
                        "Alt Unit → {} — applying...",
                        if new_val == 0 { "ft/in" } else { "m/mm" }
                    ));
                }
            }
            SettingRow::TemperatureUnit => {
                let new_val = if delta > 0 {
                    image
                        .settings()
                        .temperature_unit_raw()
                        .saturating_add(1)
                        .min(1)
                } else {
                    image.settings().temperature_unit_raw().saturating_sub(1)
                };
                if let Some((offset, value)) =
                    image.modify_setting(|w| w.set_temperature_unit_raw(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!(
                        "Temp Unit → {} — applying...",
                        if new_val == 0 { "°F" } else { "°C" }
                    ));
                }
            }
            SettingRow::GpsBtInterface => {
                adjust_numeric!(
                    gps_bt_interface,
                    set_gps_bt_interface,
                    "GPS/BT",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::PcOutputMode => {
                adjust_numeric!(
                    pc_output_mode,
                    set_pc_output_mode,
                    "PC Output",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::AprsUsbMode => {
                adjust_numeric!(
                    aprs_usb_mode,
                    set_aprs_usb_mode,
                    "APRS USB",
                    image,
                    delta,
                    tx
                );
            }
            SettingRow::AutoPowerOff => {
                let new_val = if delta > 0 {
                    image
                        .settings()
                        .auto_power_off_raw()
                        .saturating_add(1)
                        .min(4)
                } else {
                    image.settings().auto_power_off_raw().saturating_sub(1)
                };
                if let Some((offset, value)) =
                    image.modify_setting(|w| w.set_auto_power_off_raw(new_val))
                {
                    let _ = tx.send(crate::event::RadioCommand::McpWriteByte { offset, value });
                    self.status_message = Some(format!(
                        "Auto PwrOff → {} — applying...",
                        ["Off", "30m", "60m", "90m", "120m"]
                            .get(new_val as usize)
                            .unwrap_or(&"?")
                    ));
                }
            }
            _ => {
                self.status_message = Some(format!("{}: not adjustable", row.label()));
            }
        }
    }

    /// Process an incoming APRS event from the radio task.
    fn handle_aprs_event(&mut self, event: kenwood_thd75::AprsEvent) {
        use kenwood_thd75::AprsEvent;
        match event {
            AprsEvent::StationHeard(entry) => {
                self.update_station_cache(&entry);
            }
            AprsEvent::PositionReceived { source, position } => {
                // Build a minimal cache entry from position data.
                let idx = self.aprs_stations.iter().position(|s| s.callsign == source);
                if let Some(idx) = idx {
                    let cached = &mut self.aprs_stations[idx];
                    cached.latitude = Some(position.latitude);
                    cached.longitude = Some(position.longitude);
                    cached.speed_knots = position.speed_knots;
                    cached.course_degrees = position.course_degrees;
                    cached.symbol_table = Some(position.symbol_table);
                    cached.symbol_code = Some(position.symbol_code);
                    if !position.comment.is_empty() {
                        cached.comment = Some(position.comment);
                    }
                    cached.last_heard = Instant::now();
                    cached.packet_count = cached.packet_count.saturating_add(1);
                } else {
                    self.aprs_stations.push(AprsStationCache {
                        callsign: source,
                        latitude: Some(position.latitude),
                        longitude: Some(position.longitude),
                        speed_knots: position.speed_knots,
                        course_degrees: position.course_degrees,
                        symbol_table: Some(position.symbol_table),
                        symbol_code: Some(position.symbol_code),
                        comment: if position.comment.is_empty() {
                            None
                        } else {
                            Some(position.comment)
                        },
                        packet_count: 1,
                        last_path: Vec::new(),
                        last_heard: Instant::now(),
                    });
                }
                self.sort_aprs_stations();
            }
            AprsEvent::MessageReceived(msg) => {
                self.status_message =
                    Some(format!("APRS msg from {}: {}", msg.addressee, msg.text));
            }
            AprsEvent::MessageDelivered(id) => {
                if let Some(m) = self.aprs_messages.iter_mut().find(|m| m.message_id == id) {
                    m.state = AprsMessageState::Delivered;
                }
                self.status_message = Some(format!("Message {id} delivered"));
            }
            AprsEvent::MessageRejected(id) => {
                if let Some(m) = self.aprs_messages.iter_mut().find(|m| m.message_id == id) {
                    m.state = AprsMessageState::Rejected;
                }
                self.status_message = Some(format!("Message {id} rejected"));
            }
            AprsEvent::MessageExpired(id) => {
                if let Some(m) = self.aprs_messages.iter_mut().find(|m| m.message_id == id) {
                    m.state = AprsMessageState::Expired;
                }
                self.status_message = Some(format!("Message {id} expired"));
            }
            AprsEvent::WeatherReceived { source, .. } => {
                self.status_message = Some(format!("WX from {source}"));
            }
            AprsEvent::PacketDigipeated { source } => {
                self.status_message = Some(format!("Digipeated packet from {source}"));
            }
            AprsEvent::QueryResponded { to } => {
                self.status_message = Some(format!("Responded to query from {to}"));
            }
            AprsEvent::RawPacket(_) => {
                // Silently ignore raw packets for now.
            }
        }
    }

    /// Update the station cache from a `StationEntry`.
    fn update_station_cache(&mut self, entry: &kenwood_thd75::StationEntry) {
        let cached = AprsStationCache {
            callsign: entry.callsign.clone(),
            latitude: entry.position.as_ref().map(|p| p.latitude),
            longitude: entry.position.as_ref().map(|p| p.longitude),
            speed_knots: entry.position.as_ref().and_then(|p| p.speed_knots),
            course_degrees: entry.position.as_ref().and_then(|p| p.course_degrees),
            symbol_table: entry.position.as_ref().map(|p| p.symbol_table),
            symbol_code: entry.position.as_ref().map(|p| p.symbol_code),
            comment: entry
                .position
                .as_ref()
                .filter(|p| !p.comment.is_empty())
                .map(|p| p.comment.clone()),
            packet_count: entry.packet_count,
            last_path: entry.last_path.clone(),
            last_heard: entry.last_heard,
        };

        if let Some(idx) = self
            .aprs_stations
            .iter()
            .position(|s| s.callsign == cached.callsign)
        {
            self.aprs_stations[idx] = cached;
        } else {
            self.aprs_stations.push(cached);
        }
        self.sort_aprs_stations();
    }

    /// Sort stations by most recently heard.
    fn sort_aprs_stations(&mut self) {
        self.aprs_stations
            .sort_by(|a, b| b.last_heard.cmp(&a.last_heard));
    }

    /// Toggle APRS mode on or off.
    fn handle_dstar_event(&mut self, event: kenwood_thd75::DStarEvent) {
        use kenwood_thd75::DStarEvent;
        match event {
            DStarEvent::VoiceStart(header) => {
                self.dstar_rx_active = true;
                self.dstar_rx_header = Some(header);
                self.dstar_text_message = None;
            }
            DStarEvent::VoiceData(_frame) => {
                // Voice data — no UI action needed.
            }
            DStarEvent::VoiceEnd => {
                self.dstar_rx_active = false;
            }
            DStarEvent::VoiceLost => {
                self.dstar_rx_active = false;
                self.status_message = Some("D-STAR: voice lost (no clean EOT)".into());
            }
            DStarEvent::TextMessage(text) => {
                self.dstar_text_message = Some(text);
            }
            DStarEvent::StationHeard(entry) => {
                // Update the last-heard list (newest first).
                if let Some(idx) = self
                    .dstar_last_heard
                    .iter()
                    .position(|e| e.callsign == entry.callsign)
                {
                    let _ = self.dstar_last_heard.remove(idx);
                }
                self.dstar_last_heard.insert(0, entry);
                // Limit to 100 entries.
                self.dstar_last_heard.truncate(100);
            }
            DStarEvent::UrCallCommand(action) => {
                self.status_message = Some(format!("D-STAR: URCALL command detected: {action:?}"));
            }
            DStarEvent::StatusUpdate(_status) => {
                // Modem status — no UI action needed.
            }
        }
    }

    fn toggle_gps(&mut self) {
        let next = !self.state.gps_enabled;
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(crate::event::RadioCommand::SetGpsConfig(
                next,
                self.state.gps_pc_output,
            ));
            self.status_message =
                Some(format!("GPS {}", if next { "enabled" } else { "disabled" }));
        }
    }

    fn toggle_gps_pc_output(&mut self) {
        let next = !self.state.gps_pc_output;
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(crate::event::RadioCommand::SetGpsConfig(
                self.state.gps_enabled,
                next,
            ));
            self.status_message = Some(format!(
                "GPS PC Output {}",
                if next { "enabled" } else { "disabled" }
            ));
        }
    }

    fn toggle_dstar_mode(&mut self) {
        match self.dstar_mode {
            DStarMode::Inactive => {
                // Build D-STAR config from MCP data if available.
                let callsign = if let McpState::Loaded { ref image, .. } = self.mcp {
                    let cs = image.dstar().my_callsign();
                    if cs.is_empty() {
                        "N0CALL".to_string()
                    } else {
                        cs
                    }
                } else {
                    "N0CALL".to_string()
                };

                let config = kenwood_thd75::DStarGatewayConfig::new(&callsign);
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::EnterDStar { config });
                    self.status_message = Some("Entering D-STAR gateway mode...".into());
                }
            }
            DStarMode::Active => {
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::ExitDStar);
                    self.status_message = Some("Exiting D-STAR gateway mode...".into());
                }
            }
        }
    }

    fn toggle_fm_radio(&mut self) {
        let next = !self.fm_radio_on;
        if let Some(ref tx) = self.cmd_tx {
            let _ = tx.send(crate::event::RadioCommand::SetFmRadio(next));
            self.fm_radio_on = next;
            self.status_message = Some(format!(
                "FM Radio {}",
                if next { "enabled" } else { "disabled" }
            ));
        }
    }

    /// Apply a channel edit from the edit buffer.
    ///
    /// Uses ME (memory channel) write via CAT. This tunes the radio's live
    /// channel, not permanent memory storage (which would require MCP).
    fn apply_channel_edit(&mut self, field: ChannelEditField, buf: &str) {
        if buf.is_empty() {
            self.status_message = Some("No value entered".into());
            return;
        }

        let used = self.filtered_channels();
        let Some(&ch_num) = used.get(self.channel_list_index) else {
            self.status_message = Some("No channel selected".into());
            return;
        };

        match field {
            ChannelEditField::Frequency => {
                // Parse as MHz, tune via CAT
                if let Ok(mhz) = buf.parse::<f64>() {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let hz = (mhz * 1_000_000.0) as u32;
                    if let Some(ref tx) = self.cmd_tx {
                        let _ = tx.send(crate::event::RadioCommand::TuneFreq {
                            band: self.target_band,
                            freq: hz,
                        });
                        self.status_message = Some(format!("Ch {ch_num}: tuning to {mhz:.6} MHz"));
                    }
                } else {
                    self.status_message = Some(format!("Invalid frequency: {buf}"));
                }
            }
            ChannelEditField::Name => {
                // Channel name editing requires MCP write (no CAT command for name-only).
                self.status_message = Some(format!(
                    "Ch {ch_num}: name editing requires MCP write — use MCP panel (m)"
                ));
            }
            ChannelEditField::Mode => {
                // Cycle mode via CAT
                if let Some(ref tx) = self.cmd_tx {
                    use kenwood_thd75::types::Mode;
                    let mode = match buf.to_uppercase().as_str() {
                        "FM" => Some(Mode::Fm),
                        "NFM" => Some(Mode::Nfm),
                        "AM" => Some(Mode::Am),
                        "DV" => Some(Mode::Dv),
                        "LSB" => Some(Mode::Lsb),
                        "USB" => Some(Mode::Usb),
                        "CW" => Some(Mode::Cw),
                        "DR" => Some(Mode::Dr),
                        "WFM" => Some(Mode::Wfm),
                        _ => None,
                    };
                    if let Some(mode) = mode {
                        let _ = tx.send(crate::event::RadioCommand::SetMode {
                            band: self.target_band,
                            mode,
                        });
                        self.status_message = Some(format!("Ch {ch_num}: mode set to {mode}"));
                    } else {
                        self.status_message = Some(format!(
                            "Unknown mode '{buf}' (try FM/NFM/AM/DV/LSB/USB/CW/DR/WFM)"
                        ));
                    }
                }
            }
            ChannelEditField::ToneMode
            | ChannelEditField::ToneFreq
            | ChannelEditField::Duplex
            | ChannelEditField::Offset => {
                // These fields are stored in the ME channel record and require
                // either a full ME write (which changes the live channel) or MCP
                // for permanent memory storage. Full ME write support is planned.
                self.status_message = Some(format!(
                    "Ch {ch_num}: {} editing not yet implemented — requires ME write",
                    field.label()
                ));
            }
        }
    }

    fn toggle_aprs_mode(&mut self) {
        match self.aprs_mode {
            AprsMode::Inactive => {
                // Build APRS config from MCP data if available, else use defaults.
                let (callsign, ssid) = if let McpState::Loaded { ref image, .. } = self.mcp {
                    let cs = image.aprs().my_callsign();
                    if cs.is_empty() {
                        ("N0CALL".to_string(), 7u8)
                    } else {
                        // Parse SSID from callsign if present (e.g., "KQ4NIT-9").
                        if let Some((call, ssid_str)) = cs.split_once('-') {
                            let ssid = ssid_str.parse::<u8>().unwrap_or(7);
                            (call.to_string(), ssid)
                        } else {
                            (cs, 7)
                        }
                    }
                } else {
                    ("N0CALL".to_string(), 7)
                };

                let config = Box::new(kenwood_thd75::AprsClientConfig::new(&callsign, ssid));
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::EnterAprs { config });
                    self.status_message = Some("Entering APRS mode...".into());
                }
            }
            AprsMode::Active => {
                if let Some(ref tx) = self.cmd_tx {
                    let _ = tx.send(crate::event::RadioCommand::ExitAprs);
                    self.status_message = Some("Exiting APRS mode...".into());
                }
            }
        }
    }
}
