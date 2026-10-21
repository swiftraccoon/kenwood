use std::path::{Path, PathBuf};
use std::time::Instant;

use kenwood_thd75::error::ValidationError;
use kenwood_thd75::memory::{MemoryError, MemoryImage};
use kenwood_thd75::types::{
    AfGainLevel, AltitudeRainUnit, AmHighCut, AutoMuteReturnDelay, AutoPowerOff, BacklightControl,
    BacklightTimer, BandMode, BatteryLevel, BatterySaverInterval, BeaconMode, BeatShift,
    CwFilterWidth, CwPitch, DisplayUnits, DstarCallsign, DstarSuffix, DtmfPause, DtmfToneDuration,
    EmrVolume, Frequency, FrontPanelPfFunction, GpsSettings, Language, LinkedVolumeLevel,
    MicSensitivity, Module, NmeaSentences, OperatingMode, PcOutputInterface, PowerLevel,
    ReflectorCallsign, RegularChannel, RepeaterCallKey, SMeterReading, ScanRestartDelay,
    ScanResumeMethod, SpeedDistanceUnit, SquelchLevel, SsbHighCut, StoredFrontPanelPfAssignment,
    TemperatureUnit, TncDataBand, TransmitTimeout, VoiceAnnounceMode, VoiceGuideSpeed, VoxDelay,
    VoxGain,
};

use crate::settings_edit::SettingsEdit;

/// Maximum station rows retained by the dashboard independently of the client.
const APRS_STATION_CAPACITY: usize = 256;
/// Maximum outgoing delivery records displayed by the dashboard.
const APRS_HISTORY_CAPACITY: usize = 128;
/// Maximum UTF-8 bytes retained by a keyboard input buffer.
const INPUT_CAPACITY: usize = 1024;

/// Path to the MCP cache file.
///
/// Platform cache directories (no `dirs` crate needed):
/// - macOS: `~/Library/Caches`
/// - Linux: `$XDG_CACHE_HOME` or `~/.cache`
/// - Windows: `%LOCALAPPDATA%`
fn cache_path() -> Option<PathBuf> {
    cache_dir().map(|base| base.join("thd75-tui").join("mcp.bin"))
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

/// Parse D-STAR reflector-connect input into `(name, module)`.
///
/// Accepts two forms:
/// - Two-token: `"REF030 C"`, where the module is the first character of
///   the second token and must be an ASCII uppercase letter.
/// - Single-token: `"REF030C"`, at least 4 characters, with an ASCII
///   uppercase final character as the module letter and the rest as the
///   reflector name. This mirrors the REPL's reflector parsing, so a bare
///   name ending in a digit (`"REF030"`) is rejected instead of being
///   misread as name `"REF03"` + module `'0'`.
///
/// The returned name is uppercased. Anything else, including empty input
/// and a non-ASCII final character (which would make a byte-index slice
/// panic on a char boundary), returns `None`.
fn parse_reflector_input(input: &str) -> Option<(ReflectorCallsign, Module)> {
    let mut parts = input.split_whitespace();
    let first = parts.next()?;
    let (name, module) = if let Some(module) = parts.next() {
        if parts.next().is_some() || module.len() != 1 {
            return None;
        }
        (first, module.chars().next()?)
    } else {
        let module = first.chars().last()?;
        if !module.is_ascii() {
            return None;
        }
        (first.get(..first.len().checked_sub(1)?)?, module)
    };
    let module = Module::try_from_char(module).ok()?;
    let name = ReflectorCallsign::try_from_str(&name.to_ascii_uppercase()).ok()?;
    Some((name, module))
}

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
    /// SSB high-cut filter (MCP only).
    SsbHighCut,
    /// CW filter width (MCP only).
    CwWidth,
    /// AM high-cut filter (MCP only).
    AmHighCut,

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
    /// Timeout timer, 0-10 indexing the 0.5-10.0 minute table (MCP only).
    TimeoutTimer,
    /// TX inhibit (MCP only).
    TxInhibit,
    /// Beat shift type, Type 1-8 (MCP only).
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
    /// Key-lock configuration checkbox (MCP only).
    KeyLock,
    /// Frequency-lock configuration checkbox (MCP only).
    FrequencyLock,
    /// APRS lock: frequency checkbox (MCP only).
    AprsLockFrequency,
    /// APRS lock: PTT checkbox (MCP only).
    AprsLockPtt,
    /// APRS lock: APRS key checkbox (MCP only).
    AprsLockKey,

    // --- Display ---
    /// Backlight control (MCP only).
    BacklightControl,
    /// Backlight timer, 3-60 seconds (MCP only).
    BacklightTimer,
    /// Dual band (CAT: DL).
    DualBand,

    // --- Audio ---
    /// EMR volume level, 1-50 (MCP only).
    EmrVolumeLevel,
    /// Auto mute return time, 1-10 (MCP only).
    AutoMuteReturnTime,
    /// Voice announce mode, Off/Manual/Auto1/Auto2 (MCP only).
    Announce,
    /// Key beep (MCP only).
    KeyBeep,
    /// Beep volume 0-7, 0 = VOL Link (MCP only).
    BeepVolume,
    /// Voice volume 0-7, 0 = VOL Link (MCP only).
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
    /// GPS PC-output interface (MCP only).
    GpsBtInterface,
    /// APRS PC-output interface (MCP only).
    AprsUsbMode,

    // --- System ---
    /// Language (MCP only).
    Language,

    // --- Battery ---
    /// Battery saver interval, Off/0.2-5.0 s (MCP only).
    BatterySaver,
    /// Auto power off, Off/15/30/60 min (MCP only).
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
    /// Operating mode Band A (CAT: MD).
    OperatingModeA,
    /// Operating mode Band B (CAT: MD).
    OperatingModeB,
    /// Active band A/B (CAT: BC).
    ActiveBand,
    /// Tuning mode Band A (CAT: VM).
    TuningModeA,
    /// Tuning mode Band B (CAT: VM).
    TuningModeB,
    /// FM Radio on/off (CAT: FR).
    FmRadio,
    /// Packet data rate (CAT: AS).
    PacketDataRate,
    /// Beacon mode (CAT: PT).
    BeaconMode,
    /// GPS enabled (CAT: GP).
    GpsEnabled,
    /// GPS PC output (CAT: GP).
    GpsPcOutput,
    /// Auto-info notifications (CAT: AI).
    AutoInfo,
    /// APRS My Callsign (CAT: CS, read-only in this UI).
    AprsCallsign,
    /// D-STAR slot (CAT: DS).
    DstarSlot,
    /// Unidentified stock SR operation (displayed as quarantined).
    ScanResumeCat,
}

impl SettingRow {
    /// All settings rows in display order.
    pub(crate) const ALL: &[Self] = &[
        // RX
        Self::SquelchA,
        Self::SquelchB,
        Self::StepSizeA,
        Self::StepSizeB,
        Self::FineStep,
        Self::FilterWidthSsb,
        Self::FilterWidthCw,
        Self::FilterWidthAm,
        Self::SsbHighCut,
        Self::CwWidth,
        Self::AmHighCut,
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
        Self::KeyLock,
        Self::FrequencyLock,
        Self::AprsLockFrequency,
        Self::AprsLockPtt,
        Self::AprsLockKey,
        // Display
        Self::BacklightControl,
        Self::BacklightTimer,
        Self::DualBand,
        // Audio
        Self::EmrVolumeLevel,
        Self::AutoMuteReturnTime,
        Self::Announce,
        Self::KeyBeep,
        Self::BeepVolume,
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
        Self::AprsUsbMode,
        // System
        Self::Language,
        // Battery
        Self::BatterySaver,
        Self::AutoPowerOff,
        // CAT Radio Controls
        Self::PowerA,
        Self::PowerB,
        Self::AttenuatorA,
        Self::AttenuatorB,
        Self::OperatingModeA,
        Self::OperatingModeB,
        Self::ActiveBand,
        Self::TuningModeA,
        Self::TuningModeB,
        Self::FmRadio,
        Self::PacketDataRate,
        Self::BeaconMode,
        Self::GpsEnabled,
        Self::GpsPcOutput,
        Self::AutoInfo,
        Self::AprsCallsign,
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
            Self::SsbHighCut => "SSB High Cut",
            Self::CwWidth => "CW Width",
            Self::AmHighCut => "AM High Cut",
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
            Self::CwPitch => "CW Pitch",
            Self::DtmfSpeed => "DTMF Speed",
            Self::DtmfPauseTime => "DTMF Pause Time",
            Self::DtmfTxHold => "DTMF TX Hold",
            Self::RepeaterAutoOffset => "Repeater Auto Offset",
            Self::RepeaterCallKey => "Call Key Function",
            Self::MicSensitivity => "Mic Sensitivity",
            Self::PfKey1 => "PF Key 1",
            Self::PfKey2 => "PF Key 2",
            Self::KeyLock => "Key Lock",
            Self::FrequencyLock => "Frequency Lock",
            Self::AprsLockFrequency => "APRS Lock: Frequency",
            Self::AprsLockPtt => "APRS Lock: PTT",
            Self::AprsLockKey => "APRS Lock: APRS Key",
            Self::BacklightControl => "Backlight Control",
            Self::BacklightTimer => "Backlight Timer",
            Self::DualBand => "Dual Band",
            Self::EmrVolumeLevel => "EMR Volume Level",
            Self::AutoMuteReturnTime => "Auto Mute Return",
            Self::Announce => "Announce",
            Self::KeyBeep => "Key Beep",
            Self::BeepVolume => "Beep Volume",
            Self::VoiceVolume => "Voice Volume",
            Self::VoiceSpeed => "Voice Speed",
            Self::VolumeLock => "Volume Lock",
            Self::SpeedDistanceUnit => "Speed/Distance Unit",
            Self::AltitudeRainUnit => "Altitude/Rain Unit",
            Self::TemperatureUnit => "Temperature Unit",
            Self::Bluetooth => "Bluetooth",
            Self::BtAutoConnect => "BT Auto Connect",
            Self::GpsBtInterface => "GPS PC Interface",
            Self::AprsUsbMode => "APRS PC Interface",
            Self::Language => "Language",
            Self::BatterySaver => "Battery Saver",
            Self::AutoPowerOff => "Auto Power Off",
            Self::PowerA => "Power A",
            Self::PowerB => "Power B",
            Self::AttenuatorA => "Attenuator A",
            Self::AttenuatorB => "Attenuator B",
            Self::OperatingModeA => "Mode A",
            Self::OperatingModeB => "Mode B",
            Self::ActiveBand => "Active Band",
            Self::TuningModeA => "VFO/Mem A",
            Self::TuningModeB => "VFO/Mem B",
            Self::FmRadio => "FM Radio",
            Self::PacketDataRate => "Packet Data Rate",
            Self::BeaconMode => "Beacon Mode",
            Self::GpsEnabled => "GPS Enabled",
            Self::GpsPcOutput => "GPS PC Output",
            Self::AutoInfo => "Auto Info",
            Self::AprsCallsign => "APRS My Callsign",
            Self::DstarSlot => "D-STAR Slot",
            Self::ScanResumeCat => "SR (quarantined)",
        }
    }

    /// Section header label shown above this row. `None` means same group as previous row.
    pub(crate) const fn section_header(self) -> Option<&'static str> {
        match self {
            Self::SquelchA => Some("── RX ──"),
            Self::ScanResume => Some("── Scan ──"),
            Self::TimeoutTimer => Some("── TX ──"),
            Self::VoxEnabled => Some("── VOX ──"),
            Self::CwPitch => Some("── CW ──"),
            Self::DtmfSpeed => Some("── DTMF ──"),
            Self::RepeaterAutoOffset => Some("── Repeater ──"),
            Self::MicSensitivity => Some("── Auxiliary ──"),
            Self::KeyLock => Some("── Lock ──"),
            Self::BacklightControl => Some("── Display ──"),
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
                | Self::SsbHighCut
                | Self::CwWidth
                | Self::AmHighCut
                | Self::ScanResume
                | Self::DigitalScanResume
                | Self::ScanRestartTime
                | Self::ScanRestartCarrier
                | Self::TimeoutTimer
                | Self::BeatShift
                | Self::VoxGain
                | Self::VoxDelay
                | Self::CwPitch
                | Self::DtmfSpeed
                | Self::DtmfPauseTime
                | Self::RepeaterCallKey
                | Self::MicSensitivity
                | Self::PfKey1
                | Self::PfKey2
                | Self::BacklightControl
                | Self::BacklightTimer
                | Self::EmrVolumeLevel
                | Self::AutoMuteReturnTime
                | Self::Announce
                | Self::BeepVolume
                | Self::VoiceVolume
                | Self::VoiceSpeed
                | Self::SpeedDistanceUnit
                | Self::AltitudeRainUnit
                | Self::TemperatureUnit
                | Self::GpsBtInterface
                | Self::AprsUsbMode
                | Self::Language
                | Self::BatterySaver
                | Self::AutoPowerOff
                | Self::PowerA
                | Self::PowerB
                | Self::OperatingModeA
                | Self::OperatingModeB
                | Self::ActiveBand
                | Self::TuningModeA
                | Self::TuningModeB
                | Self::PacketDataRate
                | Self::BeaconMode
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
                | Self::DualBand
                | Self::Bluetooth
                | Self::PowerA
                | Self::PowerB
                | Self::AttenuatorA
                | Self::AttenuatorB
                | Self::OperatingModeA
                | Self::OperatingModeB
                | Self::ActiveBand
                | Self::TuningModeA
                | Self::TuningModeB
                | Self::PacketDataRate
                | Self::BeaconMode
                | Self::GpsEnabled
                | Self::GpsPcOutput
                | Self::AutoInfo
                | Self::AprsCallsign
                | Self::DstarSlot
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

/// Settings that require a guarded MCP page exchange and CAT restoration.
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

/// Step through an explicitly declared typed setting domain, saturating at
/// either end. A missing current value is reported to the caller instead of
/// silently choosing a default.
fn step_selection<T: Copy + PartialEq>(current: T, delta: i8, values: &[T]) -> Option<T> {
    let index = values.iter().position(|&value| value == current)?;
    let next_index = if delta > 0 {
        index.saturating_add(1).min(values.len().saturating_sub(1))
    } else {
        index.saturating_sub(1)
    };
    values.get(next_index).copied()
}

fn cycle_selection<T: Copy + PartialEq>(current: T, values: &[T]) -> Option<T> {
    let index = values.iter().position(|&value| value == current)?;
    values
        .get(index.saturating_add(1))
        .or_else(|| values.first())
        .copied()
}

/// Step a PF-key assignment through the official gapped menu domain.
/// Off-menu bytes are intentionally not coerced to an official assignment.
fn next_pf_key(current: StoredFrontPanelPfAssignment, delta: i8) -> Option<FrontPanelPfFunction> {
    let StoredFrontPanelPfAssignment::Official(current) = current else {
        return None;
    };
    step_selection(current, delta, &FrontPanelPfFunction::ALL)
}

fn next_linked_volume(
    current: LinkedVolumeLevel,
    delta: i8,
) -> Result<LinkedVolumeLevel, ValidationError> {
    let next = if delta > 0 {
        current.as_raw().saturating_add(1)
    } else {
        current.as_raw().saturating_sub(1)
    };
    LinkedVolumeLevel::try_from(next).or(Ok(current))
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
    /// CAT settings: instant, no disconnect.
    SettingsCat,
    /// Stored settings changed through guarded MCP page exchanges.
    SettingsMcp,
    Aprs,
    Dstar,
    Gps,
    Mcp,
    /// FM broadcast radio control (76-108 MHz WFM on Band B).
    FmRadio,
}

/// Which field is selected in channel edit mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChannelEditField {
    Name,
    OperatingMode,
    ToneMode,
    ToneFreq,
    Duplex,
    Offset,
}

impl ChannelEditField {
    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Name => Self::OperatingMode,
            Self::OperatingMode => Self::ToneMode,
            Self::ToneMode => Self::ToneFreq,
            Self::ToneFreq => Self::Duplex,
            Self::Duplex => Self::Offset,
            Self::Offset => Self::Name,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::OperatingMode => "Mode",
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
    /// Searching channels: buffer holds the search string.
    Search(String),
}

/// Live state for one band, updated by the radio poller.
#[derive(Debug, Clone, Default)]
pub(crate) struct BandState {
    pub frequency: Option<Frequency>,
    pub mode: Option<OperatingMode>,
    /// S-meter level (0–5). Driven by AI-pushed BY notifications, not polled.
    pub s_meter: Option<SMeterReading>,
    /// Squelch setting (0–6 on D75).
    pub squelch: Option<SquelchLevel>,
    pub power_level: Option<PowerLevel>,
    /// Squelch is open (receiving). Driven by AI-pushed BY notifications.
    pub busy: Option<bool>,
    pub attenuator: Option<bool>,
    pub step_size: Option<kenwood_thd75::types::StepSize>,
}

/// Aggregated radio state from the poller.
#[derive(Debug, Clone, Default)]
pub(crate) struct RadioState {
    pub band_a: BandState,
    pub band_b: BandState,
    pub battery_level: Option<BatteryLevel>,
    pub band_mode: Option<BandMode>,
    pub bluetooth: Option<bool>,
    pub vox: Option<bool>,
    pub vox_gain: Option<VoxGain>,
    pub vox_delay: Option<VoxDelay>,
    pub af_gain: Option<AfGainLevel>,
    pub firmware_version: String,
    pub radio_type: String,
    pub gps_settings: Option<GpsSettings>,
    /// Validated, nonempty NMEA sentence selection.
    pub gps_sentences: Option<NmeaSentences>,
    /// GPS/Radio operating mode (GM read).
    pub gps_mode: Option<kenwood_thd75::types::GpsRadioMode>,
    pub beacon_mode: Option<BeaconMode>,
    pub fine_step: Option<kenwood_thd75::types::FineStep>,
    pub filter_width_ssb: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_cw: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_am: Option<kenwood_thd75::types::FilterWidthIndex>,
    /// D-STAR URCALL callsign (8-char, space-padded).
    pub dstar_urcall: Option<kenwood_thd75::types::DstarCallsignEntry>,
    /// D-STAR RPT1 callsign.
    pub dstar_rpt1: Option<kenwood_thd75::types::DstarCallsignEntry>,
    /// D-STAR RPT2 callsign.
    pub dstar_rpt2: Option<kenwood_thd75::types::DstarCallsignEntry>,
    /// D-STAR gateway mode.
    pub dstar_gateway_mode: Option<kenwood_thd75::types::DvGatewayMode>,
    /// Active D-STAR slot.
    pub dstar_slot: Option<kenwood_thd75::types::DstarSlot>,
    /// Live APRS My Callsign read through CAT CS.
    pub aprs_callsign: Option<kenwood_thd75::types::AprsCallsign>,
}

/// Whether the APRS client is active in the radio task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AprsMode {
    /// Not in APRS mode: show stored settings on the APRS panel.
    Inactive,
    /// APRS mode active: `AprsClient` is running in the radio task.
    Active,
}

/// Tracking state for a sent APRS message.
#[derive(Debug, Clone)]
pub(crate) struct AprsMessageStatus {
    /// Destination callsign.
    pub addressee: kenwood_thd75::DirectMessageAddressee,
    /// Message text.
    pub text: kenwood_thd75::MessageText,
    /// Message ID from the messenger.
    pub message_id: kenwood_thd75::MessageId,
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
/// Retains the library's monotonic receive timestamp for elapsed-age display.
#[derive(Debug, Clone)]
pub(crate) struct AprsStationCache {
    pub callsign: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub speed_knots: Option<f64>,
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
    Reading {
        page: u16,
        total: u16,
        /// Confirmed image retained while a replacement read is in flight.
        cached: Option<MemoryImage>,
    },
    Loaded {
        image: MemoryImage,
        /// A failed write or stale comparison requires a fresh read before edits.
        requires_refresh: bool,
    },
    Writing {
        page: u16,
        total: u16,
        /// Image retained while the write is in flight so success and failure
        /// both return the operator to a usable cached image.
        image: MemoryImage,
        /// Exact staged request awaiting its verified completion.
        edit: Box<SettingsEdit>,
    },
}

/// All events that can flow into the update loop.
#[derive(Debug)]
pub(crate) enum Message {
    /// Scheduler wakeup for observing retained worker completion.
    Tick,
    Key(crossterm::event::KeyEvent),
    RadioUpdate(RadioState),
    RadioError(crate::radio_task::RadioFailure),
    /// Typed backend discovery/open failure during a replacement attempt.
    ConnectionError(std::sync::Arc<crate::radio_task::OpenFailure>),
    /// Informational progress, separate from a failed operation.
    Status(String),
    /// Connection metadata, separate from periodically unavailable observations.
    RadioIdentity {
        firmware_version: String,
        radio_type: String,
    },
    /// The sole terminal input thread failed; stop the dashboard.
    TerminalFailure(std::io::Error),
    /// A request did not reach its radio operation.
    CommandRejected {
        command: crate::event::RadioCommand,
        reason: crate::event::CommandRejection,
    },
    Disconnected,
    Reconnected,
    McpProgress {
        page: u16,
        total: u16,
    },
    McpReadComplete(Vec<u8>),
    /// A guarded typed setting completed with read-back verification.
    McpSettingApplied(Box<SettingsEdit>),
    McpError(crate::radio_task::McpFailure),
    /// The radio task has entered APRS mode successfully.
    AprsStarted,
    /// The radio task has exited APRS mode.
    AprsStopped,
    /// APRS did not return a radio with proved ordinary CAT control.
    AprsRecoveryFailed(String),
    /// An APRS event was received from the radio task.
    AprsEvent(Box<kenwood_thd75::AprsEvent>),
    /// The messenger accepted an outgoing message; transmission is still pending.
    AprsMessageQueued {
        addressee: kenwood_thd75::DirectMessageAddressee,
        text: kenwood_thd75::MessageText,
        message_id: kenwood_thd75::MessageId,
    },
    Quit,
}

/// Central application state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent view toggles and connection/quit flags accompany mutually exclusive panel and APRS mode enums."
)]
pub(crate) struct App {
    /// Where the MCP image is cached on disk. `None` disables caching
    /// entirely; the reducer's MCP arms then keep everything in
    /// memory, which is how tests drive them without touching the
    /// user's real cache file.
    pub cache_path: Option<PathBuf>,
    pub connected: bool,
    pub port_path: String,
    pub state: RadioState,
    pub focus: Pane,
    pub main_view: MainView,
    pub input_mode: InputMode,
    pub mcp: McpState,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub show_help: bool,
    pub channel_list_index: usize,
    /// Selected row in the CAT settings viewer.
    pub settings_cat_index: usize,
    /// Selected row in the MCP settings viewer.
    pub settings_mcp_index: usize,
    /// Active search filter for channel list (empty = show all).
    pub search_filter: String,
    /// Which band receives channel recalls and step/mode changes.
    pub target_band: kenwood_thd75::types::Band,
    /// Sender for commands to the radio background task.
    pub cmd_tx: Option<crate::event::CommandSender>,
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
    pub fm_radio_on: Option<bool>,
}

impl App {
    /// Borrow the last confirmed image while another read or write is pending.
    pub(crate) const fn confirmed_image(&self) -> Option<&MemoryImage> {
        match &self.mcp {
            McpState::Loaded { image, .. } | McpState::Writing { image, .. } => Some(image),
            McpState::Reading { cached, .. } => cached.as_ref(),
            McpState::Idle => None,
        }
    }

    /// Returns the list of used channel numbers, filtered by `search_filter`.
    pub(crate) fn filtered_channels(&self) -> Result<Vec<RegularChannel>, MemoryError> {
        if let Some(image) = self.confirmed_image() {
            let channels = image.channels();
            let filter = self.search_filter.to_uppercase();
            let mut filtered = Vec::new();
            for channel in RegularChannel::all() {
                if !channels.is_used(channel)? {
                    continue;
                }
                let entry = channels.get(channel)?;
                if filter.is_empty()
                    || entry.name().as_str().to_uppercase().contains(&filter)
                    || channel.to_string().contains(&filter)
                {
                    filtered.push(channel);
                }
            }
            Ok(filtered)
        } else {
            Ok(Vec::new())
        }
    }

    fn used_channel_count(&self) -> usize {
        self.filtered_channels()
            .map_or(0, |channels| channels.len())
    }

    /// Create a new app instance, loading MCP cache from disk if available.
    pub(crate) fn new(port_path: String) -> Self {
        Self::with_cache_path(port_path, cache_path().as_deref())
    }

    /// Create a new app instance, loading the MCP image from the given cache
    /// file when one is provided.
    ///
    /// `None` skips the disk cache entirely, so construction has no
    /// filesystem side effects; tests use this to avoid reading the real
    /// per-user cache directory.
    pub(crate) fn with_cache_path(port_path: String, cache: Option<&Path>) -> Self {
        let (mcp, status_message) = match cache.map_or(Ok(None), crate::cache::load) {
            Ok(Some(crate::cache::CachedImage { image, age })) => {
                let msg = age.map_or_else(
                    || "Loaded cached MCP data (age unavailable)".to_owned(),
                    |age| {
                        let mins = age.as_secs() / 60;
                        if mins < 60 {
                            format!("Loaded cached MCP data ({mins}m ago)")
                        } else if mins < 1440 {
                            format!("Loaded cached MCP data ({}h ago)", mins / 60)
                        } else {
                            format!("Loaded cached MCP data ({}d ago)", mins / 1440)
                        }
                    },
                );
                (
                    McpState::Loaded {
                        image,
                        requires_refresh: false,
                    },
                    Some(msg),
                )
            }
            Ok(None) => (McpState::Idle, None),
            Err(error) => (
                McpState::Idle,
                Some(format!("MCP cache unavailable: {error}")),
            ),
        };

        Self {
            cache_path: cache.map(Path::to_path_buf),
            connected: false,
            port_path,
            state: RadioState::default(),
            focus: Pane::BandA,
            main_view: MainView::Channels,
            input_mode: InputMode::Normal,
            mcp,
            should_quit: false,
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
            dstar_urcall_input: None,
            dstar_reflector_input: None,
            channel_edit_mode: false,
            channel_edit_field: ChannelEditField::Name,
            channel_edit_buffer: String::new(),
            fm_radio_on: None,
        }
    }

    /// Process a message and update state. Returns true if a render is needed.
    #[expect(
        clippy::too_many_lines,
        reason = "The reducer matches every worker/input message and makes each resulting state transition explicit in one table."
    )]
    pub(crate) fn update(&mut self, msg: Message) -> bool {
        match msg {
            Message::Tick => false,
            Message::Quit => {
                self.should_quit = true;
                true
            }
            Message::Key(key) => self.handle_key(key),
            Message::RadioUpdate(mut state) => {
                state.firmware_version = std::mem::take(&mut self.state.firmware_version);
                state.radio_type = std::mem::take(&mut self.state.radio_type);
                self.state = state;
                self.connected = true;
                true
            }
            Message::RadioError(err) => {
                self.status_message = Some(err.to_string());
                true
            }
            Message::ConnectionError(error) => {
                self.status_message = Some(error.to_string());
                true
            }
            Message::Status(status) => {
                self.status_message = Some(status);
                true
            }
            Message::RadioIdentity {
                firmware_version,
                radio_type,
            } => {
                self.state.firmware_version = firmware_version;
                self.state.radio_type = radio_type;
                true
            }
            Message::TerminalFailure(error) => {
                self.status_message = Some(format!("Terminal input failed: {error}"));
                self.should_quit = true;
                true
            }
            Message::CommandRejected { command, reason } => {
                if let crate::event::RadioCommand::ApplySetting(rejected) = command {
                    let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                    self.mcp = match previous {
                        McpState::Writing { image, edit, .. } if edit == rejected => {
                            McpState::Loaded {
                                image,
                                requires_refresh: false,
                            }
                        }
                        other => other,
                    };
                } else if matches!(command, crate::event::RadioCommand::ReadMemory) {
                    let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                    self.mcp = match previous {
                        McpState::Reading {
                            cached: Some(image),
                            ..
                        } => McpState::Loaded {
                            image,
                            requires_refresh: true,
                        },
                        other => other,
                    };
                }
                self.status_message = Some(format!("Command was not applied: {reason}"));
                true
            }
            Message::Disconnected => {
                self.connected = false;
                self.state = RadioState::default();
                self.status_message = Some("Disconnected, reconnecting...".into());
                true
            }
            Message::Reconnected => {
                self.connected = true;
                self.status_message = Some("Reconnected".into());
                true
            }
            Message::McpProgress { page, total } => {
                if let McpState::Writing {
                    page: current_page,
                    total: current_total,
                    ..
                } = &mut self.mcp
                {
                    *current_page = page;
                    *current_total = total;
                } else if let McpState::Reading {
                    page: current_page,
                    total: current_total,
                    ..
                } = &mut self.mcp
                {
                    *current_page = page;
                    *current_total = total;
                }
                true
            }
            Message::McpReadComplete(data) => {
                match MemoryImage::from_raw(data) {
                    Ok(image) => {
                        let cache = self.cache_image(&image);
                        self.mcp = McpState::Loaded {
                            image,
                            requires_refresh: false,
                        };
                        self.status_message = Some(format!("MCP read complete; {cache}"));
                    }
                    Err(e) => {
                        let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                        if let McpState::Reading {
                            cached: Some(image),
                            ..
                        } = previous
                        {
                            self.mcp = McpState::Loaded {
                                image,
                                requires_refresh: true,
                            };
                        }
                        self.status_message = Some(format!("MCP parse error: {e}"));
                    }
                }
                true
            }
            Message::McpSettingApplied(completed) => {
                let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                match previous {
                    McpState::Writing {
                        mut image, edit, ..
                    } if edit == completed => match edit.commit(&mut image) {
                        Ok(()) => {
                            let cache = self.cache_image(&image);
                            self.mcp = McpState::Loaded {
                                image,
                                requires_refresh: false,
                            };
                            self.status_message =
                                Some(format!("Setting write verified; CAT restored; {cache}"));
                        }
                        Err(error) => {
                            self.mcp = McpState::Loaded {
                                image,
                                requires_refresh: true,
                            };
                            self.status_message = Some(format!(
                                "Setting write completed; cached snapshot needs refresh: {error}"
                            ));
                        }
                    },
                    other => {
                        self.mcp = other;
                        self.status_message =
                            Some("Unexpected setting completion; re-read MCP data".into());
                    }
                }
                true
            }
            Message::McpError(err) => {
                // Uncertain writes and failed reads retain the last confirmed
                // image. A read without a prior image returns to Idle.
                let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                self.mcp = match previous {
                    McpState::Writing { image, .. }
                    | McpState::Reading {
                        cached: Some(image),
                        ..
                    } => McpState::Loaded {
                        image,
                        requires_refresh: true,
                    },
                    loaded @ McpState::Loaded { .. } => loaded,
                    _ => McpState::Idle,
                };
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
                self.status_message = Some("APRS mode stopped, CAT polling resumed".into());
                true
            }
            Message::AprsRecoveryFailed(error) => {
                self.aprs_mode = AprsMode::Inactive;
                self.connected = false;
                self.status_message = Some(format!(
                    "APRS session ended without usable CAT control: {error}; reconnecting..."
                ));
                true
            }
            Message::AprsEvent(event) => {
                self.handle_aprs_event(*event);
                true
            }
            Message::AprsMessageQueued {
                addressee,
                text,
                message_id,
            } => {
                if self.aprs_messages.len() >= APRS_HISTORY_CAPACITY {
                    let _oldest = self.aprs_messages.remove(0);
                }
                self.aprs_messages.push(AprsMessageStatus {
                    addressee,
                    text,
                    message_id,
                    state: AprsMessageState::Pending,
                });
                true
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "Keyboard dispatch shares global quit handling across input modes, panes and panel-specific operations."
    )]
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
                    if buf.len().saturating_add(c.len_utf8()) <= INPUT_CAPACITY {
                        buf.push(c);
                    }
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
                    {
                        let addressee =
                            match kenwood_thd75::DirectMessageAddressee::new(&station.callsign) {
                                Ok(addressee) => addressee,
                                Err(error) => {
                                    self.status_message = Some(format!(
                                        "Cannot message {}: {error}",
                                        station.callsign
                                    ));
                                    return true;
                                }
                            };
                        let text = match kenwood_thd75::MessageText::new(&text) {
                            Ok(text) => text,
                            Err(error) => {
                                self.status_message =
                                    Some(format!("Invalid APRS message: {error}"));
                                return true;
                            }
                        };
                        let label = format!("Message request for {addressee}");
                        self.queue_radio_command(
                            crate::event::RadioCommand::SendAprsMessage { addressee, text },
                            &label,
                        );
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    if buf.len().saturating_add(c.len_utf8()) <= INPUT_CAPACITY {
                        buf.push(c);
                    }
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
                    if !input.is_empty() {
                        match DstarCallsign::new(&input) {
                            Ok(callsign) => self.queue_radio_command(
                                crate::event::RadioCommand::SetUrcall {
                                    callsign,
                                    suffix: DstarSuffix::default(),
                                },
                                &format!("URCALL change to {input}"),
                            ),
                            Err(error) => {
                                self.status_message = Some(format!("Invalid URCALL: {error}"));
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    if buf.len().saturating_add(c.len_utf8()) <= DstarCallsign::MAX_LEN {
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
                    let Some((name, module)) = parse_reflector_input(&input) else {
                        self.status_message = Some("Invalid reflector (e.g. REF030 C)".into());
                        return true;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::PrepareReflectorLink { name, module },
                        &format!("Reflector {name} module {module} preparation"),
                    );
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    // Only characters that can appear in a reflector spec:
                    // rejecting non-ASCII here keeps the buffer single-byte
                    // per char, so parsing can never split a char boundary.
                    if buf.len() < INPUT_CAPACITY && (c.is_ascii_alphanumeric() || c == ' ') {
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
                    if self.channel_edit_buffer.len().saturating_add(c.len_utf8()) <= INPUT_CAPACITY
                    {
                        self.channel_edit_buffer.push(c);
                    }
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

        // --- Normal mode ---

        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
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
                match self.filtered_channels() {
                    Ok(used) if used.get(self.channel_list_index).is_some() => {
                        self.channel_edit_mode = true;
                        self.channel_edit_field = ChannelEditField::Name;
                        self.channel_edit_buffer.clear();
                        self.status_message = Some("Channel actions: Tab=select, Enter=inspect or queue live mode, Esc=close".into());
                    }
                    Ok(_) => {}
                    Err(error) => {
                        self.status_message = Some(format!("Channel data unavailable: {error}"));
                    }
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
                if self.main_view == MainView::Dstar && self.focus == Pane::Main {
                    // Toggle D-STAR gateway mode on/off when already viewing D-STAR panel.
                    self.show_dstar_availability();
                } else {
                    self.main_view = MainView::Dstar;
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
                        MainView::Dstar | MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        self.queue_radio_command(
                            crate::event::RadioCommand::FreqDown(kenwood_thd75::types::Band::A),
                            "Band A frequency step down",
                        );
                    }
                    Pane::BandB => {
                        self.queue_radio_command(
                            crate::event::RadioCommand::FreqDown(kenwood_thd75::types::Band::B),
                            "Band B frequency step down",
                        );
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
                        MainView::Dstar | MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        self.queue_radio_command(
                            crate::event::RadioCommand::FreqUp(kenwood_thd75::types::Band::A),
                            "Band A frequency step up",
                        );
                    }
                    Pane::BandB => {
                        self.queue_radio_command(
                            crate::event::RadioCommand::FreqUp(kenwood_thd75::types::Band::B),
                            "Band B frequency step up",
                        );
                    }
                    Pane::Detail => {}
                }
                true
            }
            KeyCode::Enter if self.focus == Pane::Main && self.main_view == MainView::Channels => {
                match self.filtered_channels() {
                    Ok(used) => {
                        if let Some(&channel) = used.get(self.channel_list_index) {
                            self.queue_radio_command(
                                crate::event::RadioCommand::TuneChannel {
                                    band: self.target_band,
                                    channel,
                                },
                                &format!("Band {} channel {channel} recall", self.target_band),
                            );
                        }
                    }
                    Err(error) => {
                        self.status_message = Some(format!("Channel data unavailable: {error}"));
                    }
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
                let Some(current) = self.observed(*current) else {
                    return true;
                };
                let next = match current {
                    PowerLevel::High => PowerLevel::Medium,
                    PowerLevel::Medium => PowerLevel::Low,
                    PowerLevel::Low => PowerLevel::ExtraLow,
                    PowerLevel::ExtraLow => PowerLevel::High,
                };
                self.queue_radio_command(
                    crate::event::RadioCommand::SetPower { band, level: next },
                    &format!("Band {band} power change to {next}"),
                );
                true
            }
            // Attenuator toggle on band pane
            KeyCode::Char('t') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (kenwood_thd75::types::Band::A, self.state.band_a.attenuator)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.attenuator)
                };
                let Some(cur) = self.observed(cur) else {
                    return true;
                };
                self.queue_radio_command(
                    crate::event::RadioCommand::SetAttenuator {
                        band,
                        enabled: !cur,
                    },
                    &format!("Band {band} attenuator change to {}", on_off(!cur)),
                );
                true
            }
            // Squelch adjust on band pane: [ and ]
            KeyCode::Char('[') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (kenwood_thd75::types::Band::A, self.state.band_a.squelch)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.squelch)
                };
                let Some(cur) = self.observed(cur) else {
                    return true;
                };
                let next = cur.as_raw().saturating_sub(1);
                if let Ok(level) = SquelchLevel::new(next) {
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetSquelch { band, level },
                        &format!("Band {band} squelch change to {next}"),
                    );
                }
                true
            }
            KeyCode::Char(']') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (kenwood_thd75::types::Band::A, self.state.band_a.squelch)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.squelch)
                };
                let Some(cur) = self.observed(cur) else {
                    return true;
                };
                let next = cur.as_raw().saturating_add(1).min(SquelchLevel::MAX);
                if let Ok(level) = SquelchLevel::new(next) {
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetSquelch { band, level },
                        &format!("Band {band} squelch change to {next}"),
                    );
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
                self.status_message =
                    Some("Manual beacon unavailable: no validated live local position".into());
                true
            }
            KeyCode::Char('r') if self.main_view == MainView::Mcp => {
                if matches!(self.mcp, McpState::Idle | McpState::Loaded { .. })
                    && let Some(tx) = &self.cmd_tx
                    && tx.send(crate::event::RadioCommand::ReadMemory).is_ok()
                {
                    let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                    let cached = match previous {
                        McpState::Loaded { image, .. } => Some(image),
                        _ => None,
                    };
                    self.mcp = McpState::Reading {
                        page: 0,
                        total: kenwood_thd75::protocol::programming::TOTAL_PAGES,
                        cached,
                    };
                    self.status_message = Some("MCP read queued".into());
                }
                true
            }
            // D-STAR: set CQ (URCALL = CQCQCQ)
            KeyCode::Char('C') if self.main_view == MainView::Dstar && self.focus == Pane::Main => {
                self.queue_radio_command(crate::event::RadioCommand::SetCQ, "CQ preparation");
                true
            }
            // D-STAR: set URCALL (prompt)
            KeyCode::Char('u') if self.main_view == MainView::Dstar && self.focus == Pane::Main => {
                self.dstar_urcall_input = Some(String::new());
                true
            }
            // D-STAR: prepare reflector link (prompt)
            KeyCode::Char('r') if self.main_view == MainView::Dstar && self.focus == Pane::Main => {
                self.dstar_reflector_input = Some(String::new());
                true
            }
            // D-STAR: prepare reflector unlink
            KeyCode::Char('U') if self.main_view == MainView::Dstar && self.focus == Pane::Main => {
                self.queue_radio_command(
                    crate::event::RadioCommand::PrepareReflectorUnlink,
                    "Reflector unlink preparation",
                );
                true
            }
            _ => false,
        }
    }

    /// Report caching independently from completed radio work.
    fn cache_image(&self, image: &MemoryImage) -> String {
        self.cache_path.as_ref().map_or_else(
            || "disk cache disabled".to_owned(),
            |path| match crate::cache::save(path, image) {
                Ok(()) => "disk cache updated".to_owned(),
                Err(error) => format!("disk cache failed: {error}"),
            },
        )
    }

    /// Require an observation from the current connection before relative edits.
    fn observed<T>(&mut self, value: Option<T>) -> Option<T> {
        if value.is_none() {
            self.status_message =
                Some("Current value unavailable; wait for a successful radio read".into());
        }
        value
    }

    /// Describe acceptance separately from later radio completion or failure.
    fn queue_radio_command(&mut self, command: crate::event::RadioCommand, label: &str) {
        self.status_message = Some(self.cmd_tx.as_ref().map_or_else(
            || format!("{label}: not queued (radio worker unavailable)"),
            |sender| match sender.send(command) {
                Ok(()) => format!("{label}: queued"),
                Err(error) => format!("{label}: not queued ({error})"),
            },
        ));
    }

    /// Queue one staged edit before changing its pending display state.
    fn queue_setting_edit(&mut self, edit: SettingsEdit, label: &str) {
        if !matches!(
            self.mcp,
            McpState::Loaded {
                requires_refresh: false,
                ..
            }
        ) {
            self.status_message =
                Some("A setting write is pending or MCP data needs refresh".into());
            return;
        }
        let Some(sender) = &self.cmd_tx else {
            self.status_message =
                Some("Setting change was not queued: radio worker unavailable".into());
            return;
        };
        let edit = Box::new(edit);
        if let Err(error) = sender.send(crate::event::RadioCommand::ApplySetting(edit.clone())) {
            self.status_message = Some(format!("Setting change was not queued: {error}"));
            return;
        }
        let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
        if let McpState::Loaded { image, .. } = previous {
            self.mcp = McpState::Writing {
                image,
                edit,
                page: 0,
                total: 1,
            };
            self.status_message = Some(format!("{label}: queued for guarded write"));
        } else {
            self.mcp = previous;
        }
    }

    /// Toggle a boolean setting or show hint for numeric ones.
    #[expect(
        clippy::too_many_lines,
        reason = "The dispatch follows the settings table and shares selection, staged-write and validation feedback across rows."
    )]
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
        if self.cmd_tx.is_some() {
            match row {
                SettingRow::DualBand => {
                    let Some(current) = self.observed(self.state.band_mode) else {
                        return;
                    };
                    let next = match current {
                        BandMode::Dual => BandMode::Single,
                        BandMode::Single => BandMode::Dual,
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetBandMode(next),
                        &format!("Band mode → {next}"),
                    );
                    return;
                }
                SettingRow::Bluetooth => {
                    let Some(current) = self.observed(self.state.bluetooth) else {
                        return;
                    };
                    let next = !current;
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetBluetooth(next),
                        &format!("Bluetooth → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::VoxEnabled => {
                    let Some(current) = self.observed(self.state.vox) else {
                        return;
                    };
                    let next = !current;
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetVox(next),
                        &format!("VOX → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::AttenuatorA => {
                    let Some(current) = self.observed(self.state.band_a.attenuator) else {
                        return;
                    };
                    let next = !current;
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetAttenuator {
                            band: kenwood_thd75::types::Band::A,
                            enabled: next,
                        },
                        &format!("Atten A → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::AttenuatorB => {
                    let Some(current) = self.observed(self.state.band_b.attenuator) else {
                        return;
                    };
                    let next = !current;
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetAttenuator {
                            band: kenwood_thd75::types::Band::B,
                            enabled: next,
                        },
                        &format!("Atten B → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::FmRadio => {
                    self.status_message = Some(
                        "FM controls are unavailable in this dashboard; use the radio controls"
                            .into(),
                    );
                    return;
                }
                SettingRow::GpsEnabled => {
                    let Some(current) = self.state.gps_settings else {
                        self.status_message =
                            Some("GPS settings unavailable; reconnect to refresh".into());
                        return;
                    };
                    let next = !current.enabled();
                    let settings = current.with_enabled(next);
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetGpsSettings(settings),
                        &format!("GPS → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::GpsPcOutput => {
                    let Some(current) = self.state.gps_settings else {
                        self.status_message =
                            Some("GPS settings unavailable; reconnect to refresh".into());
                        return;
                    };
                    let next = !current.pc_output();
                    let settings = current.with_pc_output(next);
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetGpsSettings(settings),
                        &format!("GPS PC output → {}", on_off(next)),
                    );
                    return;
                }
                SettingRow::AutoInfo => {
                    self.status_message =
                        Some("Auto-information controls are unavailable in this dashboard".into());
                    return;
                }
                SettingRow::AprsCallsign => {
                    self.status_message =
                        Some("APRS My Callsign is currently read-only in this view".into());
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

        // MCP-backed boolean settings: write directly to radio via single-page MCP
        if self.cmd_tx.is_none() {
            return;
        }

        let McpState::Loaded {
            ref image,
            requires_refresh: false,
        } = self.mcp
        else {
            self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
            return;
        };

        macro_rules! toggle_bool {
            ($getter:ident, $setter:ident, $label:expr) => {{
                match image.settings().$getter() {
                    Ok(current) => {
                        let new_val = !current;
                        match SettingsEdit::stage(image, |writer| writer.$setter(new_val)) {
                            Ok(Some(edit)) => {
                                self.queue_setting_edit(
                                    edit,
                                    &format!("{} → {}", $label, on_off(new_val)),
                                );
                            }
                            Ok(None) => {
                                self.status_message =
                                    Some(format!("{}: no setting byte changed", $label));
                            }
                            Err(error) => {
                                self.status_message = Some(format!("{}: {error}", $label));
                            }
                        }
                    }
                    Err(error) => {
                        self.status_message = Some(format!("{}: {error}", $label));
                    }
                }
            }};
        }

        match row {
            SettingRow::TxInhibit => toggle_bool!(tx_inhibit, set_tx_inhibit, "TX Inhibit"),
            SettingRow::VoxTxOnBusy => {
                toggle_bool!(vox_tx_on_busy, set_vox_tx_on_busy, "VOX TX Busy");
            }
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
            SettingRow::KeyLock => toggle_bool!(key_lock, set_key_lock, "Key Lock"),
            SettingRow::FrequencyLock => {
                toggle_bool!(frequency_lock, set_frequency_lock, "Freq Lock");
            }
            SettingRow::AprsLockFrequency => {
                toggle_bool!(
                    aprs_lock_frequency,
                    set_aprs_lock_frequency,
                    "APRS Lock Freq"
                );
            }
            SettingRow::AprsLockPtt => {
                toggle_bool!(aprs_lock_ptt, set_aprs_lock_ptt, "APRS Lock PTT");
            }
            SettingRow::AprsLockKey => {
                toggle_bool!(aprs_lock_key, set_aprs_lock_key, "APRS Lock Key");
            }
            SettingRow::KeyBeep => toggle_bool!(key_beep, set_key_beep, "Key Beep"),
            SettingRow::VolumeLock => toggle_bool!(volume_lock, set_volume_lock, "Vol Lock"),
            SettingRow::BtAutoConnect => {
                toggle_bool!(bt_auto_connect, set_bt_auto_connect, "BT Auto Connect");
            }
            _ => {
                self.status_message = Some(format!("{}: use +/- to adjust", row.label()));
            }
        }
    }

    /// Adjust a numeric setting by delta with +/-.
    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "The settings-row dispatch shares checked stepping and staged-write feedback across the model's distinct setting domains."
    )]
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
        if self.cmd_tx.is_some() {
            match row {
                SettingRow::SquelchA => {
                    let Some(current) = self.observed(self.state.band_a.squelch) else {
                        return;
                    };
                    let cur = current.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(SquelchLevel::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetSquelch {
                                band: kenwood_thd75::types::Band::A,
                                level,
                            },
                            &format!("Squelch A → {next}"),
                        );
                    }
                    return;
                }
                SettingRow::SquelchB => {
                    let Some(current) = self.observed(self.state.band_b.squelch) else {
                        return;
                    };
                    let cur = current.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(SquelchLevel::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetSquelch {
                                band: kenwood_thd75::types::Band::B,
                                level,
                            },
                            &format!("Squelch B → {next}"),
                        );
                    }
                    return;
                }
                SettingRow::VoxGain => {
                    let Some(current) = self.observed(self.state.vox_gain) else {
                        return;
                    };
                    let cur = current.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(VoxGain::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(gain) = VoxGain::new(next) {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetVoxGain(gain),
                            &format!("VOX gain → {next}"),
                        );
                    }
                    return;
                }
                SettingRow::VoxDelay => {
                    let Some(current) = self.observed(self.state.vox_delay) else {
                        return;
                    };
                    let cur = current.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(VoxDelay::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(delay) = VoxDelay::new(next) {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetVoxDelay(delay),
                            &format!("VOX delay → {next}"),
                        );
                    }
                    return;
                }
                SettingRow::StepSizeA => {
                    use kenwood_thd75::types::StepSize;
                    let Some(current) = self.observed(self.state.band_a.step_size) else {
                        return;
                    };
                    let Some(next) = step_selection(current, delta, &StepSize::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetStepSize {
                            band: kenwood_thd75::types::Band::A,
                            step: next,
                        },
                        &format!("Step A → {next}"),
                    );
                    return;
                }
                SettingRow::StepSizeB => {
                    use kenwood_thd75::types::StepSize;
                    let Some(current) = self.observed(self.state.band_b.step_size) else {
                        return;
                    };
                    let Some(next) = step_selection(current, delta, &StepSize::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetStepSize {
                            band: kenwood_thd75::types::Band::B,
                            step: next,
                        },
                        &format!("Step B → {next}"),
                    );
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
                    self.status_message = Some(
                        "SR is quarantined; configure scan resume through Menu 130/131".into(),
                    );
                    return;
                }
                SettingRow::PowerA => {
                    let Some(current) = self.observed(self.state.band_a.power_level) else {
                        return;
                    };
                    let Some(next) = cycle_selection(current, &PowerLevel::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetPower {
                            band: kenwood_thd75::types::Band::A,
                            level: next,
                        },
                        &format!("Power A → {next}"),
                    );
                    return;
                }
                SettingRow::PowerB => {
                    let Some(current) = self.observed(self.state.band_b.power_level) else {
                        return;
                    };
                    let Some(next) = cycle_selection(current, &PowerLevel::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetPower {
                            band: kenwood_thd75::types::Band::B,
                            level: next,
                        },
                        &format!("Power B → {next}"),
                    );
                    return;
                }
                SettingRow::OperatingModeA => {
                    use kenwood_thd75::types::OperatingMode;
                    let Some(current) = self.observed(self.state.band_a.mode) else {
                        return;
                    };
                    let Some(next) = cycle_selection(current, &OperatingMode::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetOperatingMode {
                            band: kenwood_thd75::types::Band::A,
                            mode: next,
                        },
                        &format!("Mode A → {next}"),
                    );
                    return;
                }
                SettingRow::OperatingModeB => {
                    use kenwood_thd75::types::OperatingMode;
                    let Some(current) = self.observed(self.state.band_b.mode) else {
                        return;
                    };
                    let Some(next) = cycle_selection(current, &OperatingMode::ALL) else {
                        return;
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetOperatingMode {
                            band: kenwood_thd75::types::Band::B,
                            mode: next,
                        },
                        &format!("Mode B → {next}"),
                    );
                    return;
                }
                SettingRow::ActiveBand => {
                    self.status_message =
                        Some("Active-band selection is unavailable in this settings list".into());
                    return;
                }
                SettingRow::TuningModeA => {
                    self.status_message = Some(
                        "Band A VFO/memory selection is unavailable in this settings list".into(),
                    );
                    return;
                }
                SettingRow::TuningModeB => {
                    self.status_message = Some(
                        "Band B VFO/memory selection is unavailable in this settings list".into(),
                    );
                    return;
                }
                SettingRow::PacketDataRate => {
                    let data_rate = if delta > 0 {
                        kenwood_thd75::types::PacketDataRate::Bps9600
                    } else {
                        kenwood_thd75::types::PacketDataRate::Bps1200
                    };
                    self.queue_radio_command(
                        crate::event::RadioCommand::SetPacketDataRate(data_rate),
                        &format!("Packet data rate → {data_rate}"),
                    );
                    return;
                }
                SettingRow::BeaconMode => {
                    let Some(current) = self.observed(self.state.beacon_mode) else {
                        return;
                    };
                    let cur = u8::from(current);
                    let next = if delta > 0 {
                        cur.saturating_add(1)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(mode) = BeaconMode::try_from(next) {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetBeaconMode(mode),
                            &format!("Beacon mode → {mode}"),
                        );
                    }
                    return;
                }
                SettingRow::DstarSlot => {
                    self.status_message =
                        Some("D-STAR slot changes are unavailable in this settings list".into());
                    return;
                }
                _ => {}
            }
        }

        // MCP-backed numeric settings: write directly via single-page MCP
        if self.cmd_tx.is_none() {
            return;
        }

        let McpState::Loaded {
            ref image,
            requires_refresh: false,
        } = self.mcp
        else {
            self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
            return;
        };

        /// Read a typed setting, step it within an explicit typed domain, then
        /// transactionally apply its typed setter.
        macro_rules! adjust_typed {
            ($getter:ident, $setter:ident, $step:expr, $label:expr, $display:expr) => {{
                match image.settings().$getter() {
                    Ok(current) => match ($step)(current, delta) {
                        Some(next) => {
                            let display = ($display)(next);
                            match SettingsEdit::stage(image, |writer| writer.$setter(next)) {
                                Ok(Some(edit)) => {
                                    self.queue_setting_edit(
                                        edit,
                                        &format!("{} → {}", $label, display),
                                    );
                                }
                                Ok(None) => {
                                    self.status_message =
                                        Some(format!("{} unchanged ({display})", $label));
                                }
                                Err(error) => {
                                    self.status_message = Some(format!("{}: {error}", $label));
                                }
                            }
                        }
                        None => {
                            self.status_message = Some(format!(
                                "{}: current stored value is not an adjustable menu choice",
                                $label
                            ));
                        }
                    },
                    Err(error) => {
                        self.status_message = Some(format!("{}: {error}", $label));
                    }
                }
            }};
        }

        /// Apply a numeric constructor that reports a validation error rather
        /// than representing an invalid menu choice as absence.
        macro_rules! adjust_validated_typed {
            ($getter:ident, $setter:ident, $step:expr, $label:expr, $display:expr) => {{
                match image.settings().$getter() {
                    Ok(current) => match ($step)(current, delta) {
                        Ok(next) => {
                            let display = ($display)(next);
                            match SettingsEdit::stage(image, |writer| writer.$setter(next)) {
                                Ok(Some(edit)) => {
                                    self.queue_setting_edit(
                                        edit,
                                        &format!("{} → {}", $label, display),
                                    );
                                }
                                Ok(None) => {
                                    self.status_message =
                                        Some(format!("{} unchanged ({display})", $label));
                                }
                                Err(error) => {
                                    self.status_message = Some(format!("{}: {error}", $label));
                                }
                            }
                        }
                        Err(error) => {
                            self.status_message = Some(format!("{}: {error}", $label));
                        }
                    },
                    Err(error) => {
                        self.status_message = Some(format!("{}: {error}", $label));
                    }
                }
            }};
        }

        match row {
            SettingRow::SsbHighCut => {
                adjust_typed!(
                    ssb_high_cut,
                    set_ssb_high_cut,
                    |current: SsbHighCut, direction| step_selection(
                        current,
                        direction,
                        &SsbHighCut::ALL,
                    ),
                    "SSB High Cut",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::CwWidth => {
                adjust_typed!(
                    cw_width,
                    set_cw_width,
                    |current: CwFilterWidth, direction| step_selection(
                        current,
                        direction,
                        &CwFilterWidth::ALL,
                    ),
                    "CW Width",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::AmHighCut => {
                adjust_typed!(
                    am_high_cut,
                    set_am_high_cut,
                    |current: AmHighCut, direction| step_selection(
                        current,
                        direction,
                        &AmHighCut::ALL,
                    ),
                    "AM High Cut",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::ScanResume => {
                adjust_typed!(
                    scan_resume,
                    set_scan_resume,
                    |current, direction| step_selection(current, direction, &ScanResumeMethod::ALL,),
                    "Scan Resume",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::DigitalScanResume => {
                adjust_typed!(
                    digital_scan_resume,
                    set_digital_scan_resume,
                    |current, direction| step_selection(current, direction, &ScanResumeMethod::ALL,),
                    "Dig Scan Resume",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::ScanRestartTime => {
                adjust_validated_typed!(
                    scan_restart_time,
                    set_scan_restart_time,
                    |current: ScanRestartDelay, direction| {
                        let seconds = if direction > 0 {
                            current.as_seconds().saturating_add(1)
                        } else {
                            current.as_seconds().saturating_sub(1)
                        };
                        ScanRestartDelay::new(seconds).or(Ok::<_, ValidationError>(current))
                    },
                    "Scan Restart Time",
                    |value: ScanRestartDelay| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::ScanRestartCarrier => {
                adjust_validated_typed!(
                    scan_restart_carrier,
                    set_scan_restart_carrier,
                    |current: ScanRestartDelay, direction| {
                        let seconds = if direction > 0 {
                            current.as_seconds().saturating_add(1)
                        } else {
                            current.as_seconds().saturating_sub(1)
                        };
                        ScanRestartDelay::new(seconds).or(Ok::<_, ValidationError>(current))
                    },
                    "Scan Restart Carrier",
                    |value: ScanRestartDelay| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::TimeoutTimer => {
                adjust_typed!(
                    timeout_timer,
                    set_timeout_timer,
                    |current, direction| step_selection(current, direction, &TransmitTimeout::ALL,),
                    "Timeout Timer",
                    |value: TransmitTimeout| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::BeatShift => {
                adjust_typed!(
                    beat_shift,
                    set_beat_shift,
                    |current, direction| step_selection(current, direction, &BeatShift::ALL,),
                    "Beat Shift",
                    |value: BeatShift| format!("Type {}", u8::from(value) + 1)
                );
            }
            SettingRow::CwPitch => {
                adjust_typed!(
                    cw_pitch,
                    set_cw_pitch,
                    |current: CwPitch, direction| {
                        let hertz = if direction > 0 {
                            current
                                .as_hz()
                                .saturating_add(CwPitch::STEP_HZ)
                                .min(CwPitch::MAX_HZ)
                        } else {
                            current
                                .as_hz()
                                .saturating_sub(CwPitch::STEP_HZ)
                                .max(CwPitch::MIN_HZ)
                        };
                        match CwPitch::new(hertz) {
                            Ok(pitch) => Some(pitch),
                            Err(error) => {
                                tracing::error!(
                                    %error,
                                    hertz,
                                    "clamped CW pitch violated the typed menu domain"
                                );
                                None
                            }
                        }
                    },
                    "CW Pitch",
                    |value: CwPitch| format!("{} Hz", value.as_hz())
                );
            }
            SettingRow::DtmfSpeed => {
                adjust_typed!(
                    dtmf_speed,
                    set_dtmf_speed,
                    |current, direction| step_selection(current, direction, &DtmfToneDuration::ALL,),
                    "DTMF Speed",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::DtmfPauseTime => {
                adjust_typed!(
                    dtmf_pause_time,
                    set_dtmf_pause_time,
                    |current, direction| step_selection(current, direction, &DtmfPause::ALL,),
                    "DTMF Pause",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::RepeaterCallKey => {
                adjust_typed!(
                    repeater_call_key,
                    set_repeater_call_key,
                    |current, direction| step_selection(current, direction, &RepeaterCallKey::ALL,),
                    "Call Key",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::MicSensitivity => {
                adjust_typed!(
                    mic_sensitivity,
                    set_mic_sensitivity,
                    |current, direction| step_selection(current, direction, &MicSensitivity::ALL,),
                    "Mic Sens",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::PfKey1 => {
                adjust_typed!(
                    pf_key1,
                    set_pf_key1,
                    next_pf_key,
                    "PF Key 1",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::PfKey2 => {
                adjust_typed!(
                    pf_key2,
                    set_pf_key2,
                    next_pf_key,
                    "PF Key 2",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::BacklightControl => {
                adjust_typed!(
                    backlight_control,
                    set_backlight_control,
                    |current, direction| step_selection(current, direction, &BacklightControl::ALL,),
                    "Backlight Ctrl",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::BacklightTimer => {
                adjust_validated_typed!(
                    backlight_timer,
                    set_backlight_timer,
                    |current: BacklightTimer, direction| {
                        let seconds = if direction > 0 {
                            current.as_seconds().saturating_add(1)
                        } else {
                            current.as_seconds().saturating_sub(1)
                        };
                        BacklightTimer::new(seconds).or(Ok::<_, ValidationError>(current))
                    },
                    "Backlight Timer",
                    |value: BacklightTimer| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::EmrVolumeLevel => {
                adjust_validated_typed!(
                    emr_volume_level,
                    set_emr_volume_level,
                    |current: EmrVolume, direction| {
                        let level = if direction > 0 {
                            current.as_raw().saturating_add(1).min(EmrVolume::MAX)
                        } else {
                            current.as_raw().saturating_sub(1).max(EmrVolume::MIN)
                        };
                        EmrVolume::new(level)
                    },
                    "EMR Vol",
                    |value: EmrVolume| format!("Level {}", value.as_raw())
                );
            }
            SettingRow::AutoMuteReturnTime => {
                adjust_validated_typed!(
                    auto_mute_return_time,
                    set_auto_mute_return_time,
                    |current: AutoMuteReturnDelay, direction| {
                        let seconds = if direction > 0 {
                            current.as_seconds().saturating_add(1)
                        } else {
                            current.as_seconds().saturating_sub(1)
                        };
                        AutoMuteReturnDelay::new(seconds).or(Ok::<_, ValidationError>(current))
                    },
                    "Auto Mute",
                    |value: AutoMuteReturnDelay| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::Announce => {
                adjust_typed!(
                    announce,
                    set_announce,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &VoiceAnnounceMode::ALL,
                    ),
                    "Announce",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::BeepVolume => {
                adjust_validated_typed!(
                    beep_volume,
                    set_beep_volume,
                    next_linked_volume,
                    "Beep Vol",
                    |value: LinkedVolumeLevel| value
                        .fixed_level()
                        .map_or_else(|| "VOL Link".to_owned(), |level| format!("Level {level}"),)
                );
            }
            SettingRow::VoiceVolume => {
                adjust_validated_typed!(
                    voice_volume,
                    set_voice_volume,
                    next_linked_volume,
                    "Voice Vol",
                    |value: LinkedVolumeLevel| value
                        .fixed_level()
                        .map_or_else(|| "VOL Link".to_owned(), |level| format!("Level {level}"),)
                );
            }
            SettingRow::VoiceSpeed => {
                adjust_typed!(
                    voice_speed,
                    set_voice_speed,
                    |current, direction| step_selection(current, direction, &VoiceGuideSpeed::ALL,),
                    "Voice Speed",
                    |value: VoiceGuideSpeed| format!("Speed {}", u8::from(value) + 1)
                );
            }
            SettingRow::SpeedDistanceUnit => {
                adjust_typed!(
                    display_units,
                    set_speed_distance_unit,
                    |current: DisplayUnits, direction| step_selection(
                        current.speed_distance,
                        direction,
                        &SpeedDistanceUnit::ALL,
                    ),
                    "Speed Unit",
                    |value| match value {
                        SpeedDistanceUnit::MilesPerHour => "mph".to_owned(),
                        SpeedDistanceUnit::KilometersPerHour => "km/h".to_owned(),
                        SpeedDistanceUnit::Knots => "knots".to_owned(),
                    }
                );
            }
            SettingRow::AltitudeRainUnit => {
                adjust_typed!(
                    display_units,
                    set_altitude_rain_unit,
                    |current: DisplayUnits, direction| step_selection(
                        current.altitude_rain,
                        direction,
                        &AltitudeRainUnit::ALL,
                    ),
                    "Alt Unit",
                    |value| match value {
                        AltitudeRainUnit::FeetInch => "ft/in".to_owned(),
                        AltitudeRainUnit::MetersMm => "m/mm".to_owned(),
                    }
                );
            }
            SettingRow::TemperatureUnit => {
                adjust_typed!(
                    display_units,
                    set_temperature_unit,
                    |current: DisplayUnits, direction| step_selection(
                        current.temperature,
                        direction,
                        &TemperatureUnit::ALL,
                    ),
                    "Temp Unit",
                    |value| match value {
                        TemperatureUnit::Fahrenheit => "°F".to_owned(),
                        TemperatureUnit::Celsius => "°C".to_owned(),
                    }
                );
            }
            SettingRow::GpsBtInterface => {
                adjust_typed!(
                    gps_pc_output_interface,
                    set_gps_pc_output_interface,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &PcOutputInterface::ALL,
                    ),
                    "GPS PC Interface",
                    |value| match value {
                        PcOutputInterface::Usb => "USB".to_owned(),
                        PcOutputInterface::Bluetooth => "Bluetooth".to_owned(),
                    }
                );
            }
            SettingRow::AprsUsbMode => {
                adjust_typed!(
                    aprs_pc_output_interface,
                    set_aprs_pc_output_interface,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &PcOutputInterface::ALL,
                    ),
                    "APRS PC Interface",
                    |value| match value {
                        PcOutputInterface::Usb => "USB".to_owned(),
                        PcOutputInterface::Bluetooth => "Bluetooth".to_owned(),
                    }
                );
            }
            SettingRow::Language => {
                adjust_typed!(
                    language,
                    set_language,
                    |current, direction| step_selection(current, direction, &Language::ALL,),
                    "Language",
                    |value| match value {
                        Language::English => "English".to_owned(),
                        Language::Japanese => "Japanese".to_owned(),
                    }
                );
            }
            SettingRow::BatterySaver => {
                adjust_typed!(
                    battery_saver,
                    set_battery_saver,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &BatterySaverInterval::ALL,
                    ),
                    "Battery Saver",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::AutoPowerOff => {
                adjust_typed!(
                    auto_power_off,
                    set_auto_power_off,
                    |current, direction| step_selection(current, direction, &AutoPowerOff::ALL,),
                    "Auto PwrOff",
                    |value| match value {
                        AutoPowerOff::Off => "Off".to_owned(),
                        AutoPowerOff::Min15 => "15 min".to_owned(),
                        AutoPowerOff::Min30 => "30 min".to_owned(),
                        AutoPowerOff::Min60 => "60 min".to_owned(),
                    }
                );
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
                let source = source.to_string();
                // Build a minimal cache entry from position data.
                let known = self.aprs_stations.iter().any(|s| s.callsign == source);
                if known {
                    if let Some(cached) =
                        self.aprs_stations.iter_mut().find(|s| s.callsign == source)
                    {
                        cached.latitude = Some(position.latitude().as_degrees());
                        cached.longitude = Some(position.longitude().as_degrees());
                        cached.speed_knots = position.speed().map(kenwood_thd75::Speed::as_knots);
                        cached.course_degrees =
                            position.course().map(kenwood_thd75::Course::as_degrees);
                        cached.symbol_table = Some(position.symbol().table_char());
                        cached.symbol_code = Some(position.symbol().code_char());
                        cached.comment =
                            (!position.comment().is_empty()).then(|| position.comment().to_owned());
                        cached.last_heard = Instant::now();
                    }
                } else {
                    self.aprs_stations.push(AprsStationCache {
                        callsign: source,
                        latitude: Some(position.latitude().as_degrees()),
                        longitude: Some(position.longitude().as_degrees()),
                        speed_knots: position.speed().map(kenwood_thd75::Speed::as_knots),
                        course_degrees: position.course().map(kenwood_thd75::Course::as_degrees),
                        symbol_table: Some(position.symbol().table_char()),
                        symbol_code: Some(position.symbol().code_char()),
                        comment: if position.comment().is_empty() {
                            None
                        } else {
                            Some(position.comment().to_owned())
                        },
                        packet_count: 1,
                        last_path: Vec::new(),
                        last_heard: Instant::now(),
                    });
                }
                self.sort_aprs_stations();
            }
            AprsEvent::MessageReceived(msg) => {
                self.status_message = Some(format!(
                    "APRS message to {}: {}",
                    msg.addressee(),
                    msg.text()
                ));
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
            AprsEvent::DigipeaterCapacityExceeded { queue } => {
                self.status_message = Some(format!("APRS relay queue is full: {queue:?}"));
            }
            AprsEvent::QueryResponded { to } => {
                self.status_message = Some(format!("Responded to query from {to}"));
            }
            AprsEvent::RawPacket(_) => {
                // Raw packets have no panel; they are not displayed.
            }
        }
    }

    /// Update the station cache from a `StationEntry`.
    fn update_station_cache(&mut self, entry: &kenwood_thd75::StationEntry) {
        let cached = AprsStationCache {
            callsign: entry.address().to_string(),
            latitude: entry.position().map(|p| p.latitude().as_degrees()),
            longitude: entry.position().map(|p| p.longitude().as_degrees()),
            speed_knots: entry
                .position()
                .and_then(kenwood_thd75::AprsPosition::speed)
                .map(kenwood_thd75::Speed::as_knots),
            course_degrees: entry
                .position()
                .and_then(kenwood_thd75::AprsPosition::course)
                .map(kenwood_thd75::Course::as_degrees),
            symbol_table: entry.position().map(|p| p.symbol().table_char()),
            symbol_code: entry.position().map(|p| p.symbol().code_char()),
            comment: entry
                .position()
                .filter(|p| !p.comment().is_empty())
                .map(|p| p.comment().to_owned()),
            packet_count: entry.packet_count(),
            last_path: entry.last_path().iter().map(ToString::to_string).collect(),
            last_heard: entry.last_heard(),
        };

        if let Some(idx) = self
            .aprs_stations
            .iter()
            .position(|s| s.callsign == cached.callsign)
        {
            if let Some(slot) = self.aprs_stations.get_mut(idx) {
                *slot = cached;
            }
        } else {
            self.aprs_stations.push(cached);
        }
        self.sort_aprs_stations();
    }

    /// Sort stations by most recently heard.
    fn sort_aprs_stations(&mut self) {
        self.aprs_stations
            .sort_by(|a, b| b.last_heard.cmp(&a.last_heard));
        self.aprs_stations.truncate(APRS_STATION_CAPACITY);
        self.aprs_station_index = self
            .aprs_station_index
            .min(self.aprs_stations.len().saturating_sub(1));
    }

    fn toggle_gps(&mut self) {
        let Some(current) = self.state.gps_settings else {
            self.status_message = Some("GPS settings unavailable; reconnect to refresh".into());
            return;
        };
        let next = !current.enabled();
        let settings = current.with_enabled(next);
        self.queue_radio_command(
            crate::event::RadioCommand::SetGpsSettings(settings),
            &format!(
                "GPS change to {}",
                if next { "enabled" } else { "disabled" }
            ),
        );
    }

    fn toggle_gps_pc_output(&mut self) {
        let Some(current) = self.state.gps_settings else {
            self.status_message = Some("GPS settings unavailable; reconnect to refresh".into());
            return;
        };
        let next = !current.pc_output();
        let settings = current.with_pc_output(next);
        self.queue_radio_command(
            crate::event::RadioCommand::SetGpsSettings(settings),
            &format!(
                "GPS PC output change to {}",
                if next { "enabled" } else { "disabled" }
            ),
        );
    }

    fn show_dstar_availability(&mut self) {
        self.status_message = Some(match kenwood_thd75::DstarGateway::<kenwood_thd75::transport::EitherTransport>::wire_configuration() {
            Err(error) => format!("D-STAR modem monitor unavailable: {error}"),
            Ok(_configuration) => "D-STAR modem monitoring is unavailable in this dashboard".to_owned(),
        });
    }

    fn toggle_fm_radio(&mut self) {
        self.status_message =
            Some("FM controls are unavailable in this dashboard; use the radio controls".into());
    }

    /// Apply an available live control from the channel detail buffer.
    ///
    /// Mode changes affect the live band. Stored fields remain read-only.
    fn apply_channel_edit(&mut self, field: ChannelEditField, buf: &str) {
        if buf.is_empty() {
            self.status_message = Some("No value entered".into());
            return;
        }

        let used = match self.filtered_channels() {
            Ok(used) => used,
            Err(error) => {
                self.status_message = Some(format!("Channel data unavailable: {error}"));
                return;
            }
        };
        let Some(&ch_num) = used.get(self.channel_list_index) else {
            self.status_message = Some("No channel selected".into());
            return;
        };

        match field {
            ChannelEditField::Name => {
                self.status_message =
                    Some(format!("Ch {ch_num}: stored name editing is unavailable"));
            }
            ChannelEditField::OperatingMode => {
                use kenwood_thd75::types::OperatingMode;
                match buf.parse::<OperatingMode>() {
                    Ok(mode) => {
                        self.queue_radio_command(
                            crate::event::RadioCommand::SetOperatingMode {
                                band: self.target_band,
                                mode,
                            },
                            &format!("Live band {} mode change to {mode}", self.target_band),
                        );
                    }
                    Err(error) => {
                        self.status_message = Some(format!("Invalid operating mode: {error}"));
                    }
                }
            }
            ChannelEditField::ToneMode
            | ChannelEditField::ToneFreq
            | ChannelEditField::Duplex
            | ChannelEditField::Offset => {
                self.status_message = Some(format!(
                    "Ch {ch_num}: stored {} editing is unavailable",
                    field.label()
                ));
            }
        }
    }

    fn toggle_aprs_mode(&mut self) {
        match self.aprs_mode {
            AprsMode::Inactive => {
                // The live CS read is the authority for the station identity.
                // Never substitute a placeholder callsign into a mode that can
                // transmit on the operator's behalf.
                let Some(callsign) = self.state.aprs_callsign.as_ref() else {
                    self.status_message = Some(
                        "Cannot enter APRS mode: the radio has no readable APRS callsign".into(),
                    );
                    return;
                };

                let data_band = match self.target_band {
                    kenwood_thd75::types::Band::A => TncDataBand::A,
                    kenwood_thd75::types::Band::B => TncDataBand::B,
                };
                let config = match kenwood_thd75::AprsClientConfig::new(
                    callsign.address().clone(),
                    data_band,
                ) {
                    Ok(config) => Box::new(config),
                    Err(error) => {
                        self.status_message = Some(format!(
                            "Cannot enter APRS mode with the configured station identity: {error}"
                        ));
                        return;
                    }
                };
                self.queue_radio_command(
                    crate::event::RadioCommand::EnterAprs { config },
                    &format!("APRS start on {data_band}"),
                );
            }
            AprsMode::Active => {
                self.queue_radio_command(crate::event::RadioCommand::ExitAprs, "APRS stop");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use kenwood_thd75::types::{
        FrontPanelPfFunction, GpsSettings, LinkedVolumeLevel, StoredFrontPanelPfAssignment,
        TncDataBand,
    };
    use tokio::sync::mpsc::Receiver;

    use super::{
        App, AprsMode, AprsStationCache, InputMode, MainView, McpState, MemoryImage, Message, Pane,
        RadioState, SettingRow, cat_settings, next_linked_volume, next_pf_key,
        parse_reflector_input,
    };
    use crate::event::{RadioCommand, command_channel};
    use crate::settings_edit::SettingsEdit;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn unsupported_modem_profile_never_queues_terminal_entry() -> TestResult {
        let (mut app, mut commands) = command_app();
        app.show_dstar_availability();
        assert!(
            commands.try_recv().is_err(),
            "unsupported modem profile queued radio I/O"
        );
        let status = app
            .status_message
            .ok_or("missing unsupported-profile status")?;
        let expected=kenwood_thd75::DstarGateway::<kenwood_thd75::transport::EitherTransport>::wire_configuration();
        let Err(error) = expected else {
            return Err("Kenwood profile unexpectedly supported".into());
        };
        assert!(
            status.contains(&error.to_string()),
            "profile refusal was hidden: {status}"
        );
        Ok(())
    }

    #[test]
    fn linked_volume_steps_across_link_and_fixed_levels_and_saturates() -> TestResult {
        let level_1 = LinkedVolumeLevel::fixed(1)?;
        let level_7 = LinkedVolumeLevel::fixed(7)?;

        assert_eq!(
            next_linked_volume(LinkedVolumeLevel::VOLUME_LINK, 1)?,
            level_1
        );
        assert_eq!(
            next_linked_volume(level_1, -1)?,
            LinkedVolumeLevel::VOLUME_LINK
        );
        assert_eq!(next_linked_volume(level_7, 1)?, level_7);
        assert_eq!(
            next_linked_volume(LinkedVolumeLevel::VOLUME_LINK, -1)?,
            LinkedVolumeLevel::VOLUME_LINK
        );
        Ok(())
    }

    #[test]
    fn pf_key_step_refuses_to_invent_an_assignment_for_off_menu_bytes() {
        for raw in [31, 0xFF] {
            assert_eq!(
                next_pf_key(StoredFrontPanelPfAssignment::from(raw), 1),
                None
            );
            assert_eq!(
                next_pf_key(StoredFrontPanelPfAssignment::from(raw), -1),
                None
            );
        }

        assert_eq!(
            next_pf_key(
                StoredFrontPanelPfAssignment::Official(FrontPanelPfFunction::VoiceMessage4),
                1,
            ),
            Some(FrontPanelPfFunction::VoiceGuidance)
        );
    }

    fn mcp_failure() -> crate::radio_task::McpFailure {
        crate::radio_task::McpFailure::Read(std::sync::Arc::new(
            kenwood_thd75::Error::CatRecoveryRequired,
        ))
    }

    /// Failed reads and uncertain writes retain the last confirmed image.
    #[test]
    fn mcp_error_preserves_a_loaded_image() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        let raw = vec![0u8; 500_480];
        assert!(app.update(Message::McpReadComplete(raw)));
        assert!(
            matches!(app.mcp, McpState::Loaded { .. }),
            "precondition: the image is loaded"
        );

        assert!(app.update(Message::McpError(mcp_failure())));
        assert!(
            matches!(app.mcp, McpState::Loaded { .. }),
            "a failed MCP write must not destroy the cached image"
        );

        let image = MemoryImage::from_raw(vec![0; 500_480])?;
        let edit = Box::new(
            SettingsEdit::stage(&image, |writer| writer.set_key_beep(true))?
                .ok_or("missing edit")?,
        );
        app.mcp = McpState::Writing {
            page: 12,
            total: 100,
            image,
            edit,
        };
        assert!(app.update(Message::McpError(mcp_failure())));
        assert!(
            matches!(
                app.mcp,
                McpState::Loaded {
                    requires_refresh: true,
                    ..
                }
            ),
            "a real in-flight write failure must restore the retained image"
        );

        // An error without a retained image leaves the browser idle.
        let mut fresh = App::with_cache_path(String::new(), None);
        assert!(fresh.update(Message::McpError(mcp_failure())));
        assert!(
            matches!(fresh.mcp, McpState::Idle),
            "without a loaded image, an MCP error resets to Idle"
        );
        Ok(())
    }

    #[test]
    fn mcp_write_completion_restores_loaded_state_without_reconnect() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        app.connected = true;
        let image = MemoryImage::from_raw(vec![0; 500_480])?;
        let edit = Box::new(
            SettingsEdit::stage(&image, |writer| writer.set_key_beep(true))?
                .ok_or("missing edit")?,
        );
        app.mcp = McpState::Writing {
            page: 100,
            total: 100,
            image,
            edit: edit.clone(),
        };

        assert!(app.update(Message::McpSettingApplied(edit)));
        let McpState::Loaded {
            ref image,
            requires_refresh,
        } = app.mcp
        else {
            return Err("completed write must restore the loaded image".into());
        };
        assert!(!requires_refresh, "verified write needs no refresh");
        assert!(image.settings().key_beep()?);
        assert!(app.connected, "successful MCP recovery must stay connected");
        assert_eq!(
            app.status_message.as_deref(),
            Some("Setting write verified; CAT restored; disk cache disabled")
        );
        Ok(())
    }

    #[test]
    fn failed_replacement_read_retains_confirmed_snapshot() -> TestResult {
        let (mut app, mut commands) = command_app();
        assert!(app.update(Message::McpReadComplete(vec![0x5A; 500_480])));
        app.main_view = MainView::Mcp;
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(commands.try_recv()?, RadioCommand::ReadMemory));
        assert!(app.update(Message::McpReadComplete(vec![0; 3])));
        let McpState::Loaded {
            image,
            requires_refresh,
        } = app.mcp
        else {
            return Err("failed read lost confirmed image".into());
        };
        assert!(requires_refresh);
        assert_eq!(image.as_raw().first(), Some(&0x5A));
        Ok(())
    }

    /// A poll that carries empty static fields must not erase what the
    /// radio reported once at connect. Firmware version and radio type
    /// are read a single time; the periodic poll leaves them blank, so
    /// a naive `self.state = state` blanks the header on every tick.
    #[test]
    fn radio_update_preserves_only_connect_metadata_and_clears_unavailable_observations()
    -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        assert!(app.update(Message::RadioIdentity {
            firmware_version: "1.03".to_owned(),
            radio_type: "TH-D75A".to_owned()
        }));
        let initial = RadioState {
            dstar_urcall: Some(kenwood_thd75::types::DstarCallsignEntry::new(
                kenwood_thd75::types::DstarCallsign::new("CQCQCQ")?,
                kenwood_thd75::types::DstarSuffix::default(),
            )),
            ..RadioState::default()
        };
        assert!(app.update(Message::RadioUpdate(initial)));

        // A routine poll: no firmware, no radio type, no D-STAR fields.
        assert!(app.update(Message::RadioUpdate(RadioState::default())));

        assert_eq!(
            app.state.firmware_version, "1.03",
            "firmware version must survive a poll that omits it"
        );
        assert_eq!(
            app.state.radio_type, "TH-D75A",
            "radio type must survive a poll that omits it"
        );
        assert!(
            app.state.dstar_urcall.is_none(),
            "failed URCALL read preserved stale state"
        );
        assert!(app.connected, "a RadioUpdate marks the link connected");
        Ok(())
    }

    /// A poll that DOES carry a value must overwrite the old one:
    /// the preservation above must not become a write-once latch.
    #[test]
    fn radio_update_overwrites_fields_the_poll_provides() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        let initial = RadioState {
            dstar_urcall: Some(kenwood_thd75::types::DstarCallsignEntry::new(
                kenwood_thd75::types::DstarCallsign::new("CQCQCQ")?,
                kenwood_thd75::types::DstarSuffix::default(),
            )),
            ..RadioState::default()
        };
        assert!(app.update(Message::RadioUpdate(initial)));

        let updated = RadioState {
            dstar_urcall: Some(kenwood_thd75::types::DstarCallsignEntry::new(
                kenwood_thd75::types::DstarCallsign::new("W1AW")?,
                kenwood_thd75::types::DstarSuffix::default(),
            )),
            ..RadioState::default()
        };
        assert!(app.update(Message::RadioUpdate(updated)));

        assert_eq!(
            app.state
                .dstar_urcall
                .as_ref()
                .map(|entry| entry.callsign.as_str()),
            Some("W1AW"),
            "a poll that carries a URCALL must replace the cached one"
        );
        Ok(())
    }

    fn command_app() -> (App, Receiver<RadioCommand>) {
        let (feedback, _feedback_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = command_channel(feedback);
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        (app, rx)
    }

    #[test]
    fn dstar_controls_distinguish_queue_acceptance_from_closed_or_missing_worker() -> TestResult {
        let (mut app, mut commands) = command_app();
        app.main_view = MainView::Dstar;
        app.focus = Pane::Main;
        press(&mut app, KeyCode::Char('C'));
        assert!(matches!(commands.try_recv()?, RadioCommand::SetCQ));
        assert_eq!(
            app.status_message.as_deref(),
            Some("CQ preparation: queued")
        );

        drop(commands);
        app.dstar_reflector_input = Some("REF030 C".to_owned());
        press(&mut app, KeyCode::Enter);
        let rejected = app
            .status_message
            .as_deref()
            .ok_or("missing rejection status")?;
        assert!(rejected.contains("not queued"), "{rejected}");
        assert!(rejected.contains("closed"), "{rejected}");
        assert!(
            !rejected.contains("key up"),
            "unaccepted routing change invited transmission"
        );

        app.cmd_tx = None;
        press(&mut app, KeyCode::Char('U'));
        let unavailable = app
            .status_message
            .as_deref()
            .ok_or("missing unavailable status")?;
        assert!(unavailable.contains("not queued"), "{unavailable}");
        assert!(unavailable.contains("unavailable"), "{unavailable}");
        Ok(())
    }

    #[test]
    fn full_or_rejected_setting_queue_never_commits_the_candidate() -> TestResult {
        let (mut app, mut commands) = command_app();
        let image =
            MemoryImage::from_raw(vec![0; kenwood_thd75::protocol::programming::TOTAL_SIZE])?;
        let edit = SettingsEdit::stage(&image, |writer| writer.set_key_beep(true))?
            .ok_or("missing edit")?;
        app.mcp = McpState::Loaded {
            image,
            requires_refresh: false,
        };
        let sender = app.cmd_tx.as_ref().ok_or("missing command sender")?.clone();
        while sender
            .send(RadioCommand::FreqUp(kenwood_thd75::types::Band::A))
            .is_ok()
        {}
        app.queue_setting_edit(edit.clone(), "Key beep");
        let McpState::Loaded { ref image, .. } = app.mcp else {
            return Err("rejected queue changed MCP state".into());
        };
        assert!(
            !image.settings().key_beep()?,
            "unaccepted write changed displayed value"
        );
        while commands.try_recv().is_ok() {}
        app.queue_setting_edit(edit, "Key beep");
        assert!(
            !app.confirmed_image()
                .ok_or("pending write hid confirmed image")?
                .settings()
                .key_beep()?,
            "pending write changed displayed snapshot"
        );
        let RadioCommand::ApplySetting(pending) = commands.try_recv()? else {
            return Err("missing setting command".into());
        };
        assert!(app.update(Message::CommandRejected {
            command: RadioCommand::ApplySetting(pending),
            reason: crate::event::CommandRejection::WrongMode
        }));
        let McpState::Loaded {
            ref image,
            requires_refresh,
        } = app.mcp
        else {
            return Err("worker rejection lost MCP image".into());
        };
        assert!(!requires_refresh);
        assert!(
            !image.settings().key_beep()?,
            "worker rejection committed a candidate"
        );
        Ok(())
    }

    #[test]
    fn missing_observations_and_blind_image_shortcut_never_queue_writes() -> TestResult {
        let (mut app, mut commands) = command_app();
        for pane in [Pane::BandA, Pane::BandB] {
            app.focus = pane;
            for key in ['p', 't', '[', ']'] {
                press(&mut app, KeyCode::Char(key));
            }
        }
        app.main_view = MainView::SettingsCat;
        for target in [
            SettingRow::Bluetooth,
            SettingRow::VoxEnabled,
            SettingRow::AttenuatorA,
            SettingRow::AttenuatorB,
            SettingRow::DualBand,
        ] {
            app.settings_cat_index = cat_settings()
                .iter()
                .position(|row| *row == target)
                .ok_or("missing CAT row")?;
            app.toggle_setting();
        }
        app.main_view = MainView::Mcp;
        press(&mut app, KeyCode::Char('w'));
        app.main_view = MainView::Aprs;
        app.focus = Pane::Main;
        app.aprs_mode = AprsMode::Active;
        press(&mut app, KeyCode::Char('b'));
        assert!(
            commands.try_recv().is_err(),
            "unavailable state or unsupported shortcut queued radio I/O"
        );
        Ok(())
    }

    #[test]
    fn aprs_display_history_and_station_rows_stay_bounded() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        for index in 0..=super::APRS_HISTORY_CAPACITY {
            assert!(app.update(Message::AprsMessageQueued {
                addressee: kenwood_thd75::DirectMessageAddressee::new("W1AW")?,
                text: kenwood_thd75::MessageText::new("fixture")?,
                message_id: kenwood_thd75::MessageId::new(&index.to_string())?,
            }));
        }
        assert_eq!(app.aprs_messages.len(), 128);
        assert_eq!(
            app.aprs_messages
                .first()
                .map(|message| message.message_id.as_str()),
            Some("1")
        );
        let now = std::time::Instant::now();
        for index in 0..=super::APRS_STATION_CAPACITY {
            app.aprs_stations.push(AprsStationCache {
                callsign: format!("W{index}"),
                latitude: None,
                longitude: None,
                speed_knots: None,
                course_degrees: None,
                symbol_table: None,
                symbol_code: None,
                comment: None,
                packet_count: 1,
                last_path: Vec::new(),
                last_heard: now
                    .checked_add(std::time::Duration::from_millis(u64::try_from(index)?))
                    .ok_or("fixture clock overflow")?,
            });
        }
        app.sort_aprs_stations();
        assert_eq!(app.aprs_stations.len(), 256);
        assert!(
            !app.aprs_stations
                .iter()
                .any(|station| station.callsign == "W0"),
            "oldest station was retained over a newer observation"
        );
        Ok(())
    }

    /// `McpProgress` must not mistake a write for a read: while a write
    /// is in flight, progress ticks stay `Writing` (the UI otherwise
    /// reports "Reading page N/M" during a write).
    #[test]
    fn mcp_progress_does_not_downgrade_a_write_to_a_read() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        let image = MemoryImage::from_raw(vec![0; 500_480])?;
        let edit = Box::new(
            SettingsEdit::stage(&image, |writer| writer.set_key_beep(true))?
                .ok_or("missing edit")?,
        );
        app.mcp = McpState::Writing {
            page: 0,
            total: 100,
            image,
            edit,
        };

        assert!(app.update(Message::McpProgress {
            page: 5,
            total: 100,
        }));
        assert!(
            matches!(app.mcp, McpState::Writing { page: 5, .. }),
            "progress during a write must stay Writing, got {:?}",
            app.mcp
        );

        let mut reader = App::with_cache_path(String::new(), None);
        reader.mcp = McpState::Reading {
            page: 0,
            total: 100,
            cached: None,
        };
        assert!(reader.update(Message::McpProgress {
            page: 3,
            total: 100,
        }));
        assert!(
            matches!(reader.mcp, McpState::Reading { page: 3, .. }),
            "progress with no write in flight is a read, got {:?}",
            reader.mcp
        );
        Ok(())
    }

    /// Connection-state messages drive the `connected` flag both ways.
    #[test]
    fn disconnect_and_reconnect_toggle_the_connected_flag() {
        let mut app = App::with_cache_path(String::new(), None);
        assert!(app.update(Message::RadioUpdate(RadioState::default())));
        assert!(app.connected);

        assert!(app.update(Message::Disconnected));
        assert!(!app.connected, "Disconnected clears the connected flag");

        assert!(app.update(Message::Reconnected));
        assert!(app.connected, "Reconnected sets it again");
    }

    #[test]
    fn mode_recovery_failures_never_claim_that_cat_polling_resumed() {
        let mut app = App::with_cache_path(String::new(), None);
        app.connected = true;
        assert!(app.update(Message::AprsStarted));
        assert!(app.update(Message::AprsRecoveryFailed("identity proof failed".into())));
        assert_eq!(app.aprs_mode, AprsMode::Inactive);
        assert!(!app.connected);
        let aprs_status = app.status_message.as_deref().unwrap_or_default();
        assert!(aprs_status.contains("without usable CAT control"));
        assert!(!aprs_status.contains("polling resumed"));
    }

    #[test]
    fn parse_single_token_splits_trailing_module() -> TestResult {
        let (name, module) = parse_reflector_input("REF030C").ok_or("expected REF030C to parse")?;
        assert_eq!(name.to_string(), "REF030", "reflector name");
        assert_eq!(module.as_char(), 'C', "reflector module");
        Ok(())
    }

    #[test]
    fn parse_two_token_form() -> TestResult {
        let (name, module) =
            parse_reflector_input("REF030 C").ok_or("expected `REF030 C` to parse")?;
        assert_eq!(name.to_string(), "REF030", "reflector name");
        assert_eq!(module.as_char(), 'C', "reflector module");
        Ok(())
    }

    #[test]
    fn parse_uppercases_name() -> TestResult {
        let (name, module) =
            parse_reflector_input("xlx757 B").ok_or("expected `xlx757 B` to parse")?;
        assert_eq!(name.to_string(), "XLX757", "reflector name");
        assert_eq!(module.as_char(), 'B', "reflector module");
        Ok(())
    }

    #[test]
    fn parse_rejects_single_token_trailing_digit() {
        let parsed = parse_reflector_input("REF030");
        assert!(
            parsed.is_none(),
            "trailing digit is not a module letter, got {parsed:?}"
        );
    }

    #[test]
    fn parse_rejects_lowercase_module() {
        for input in ["REF030c", "REF030 c"] {
            let parsed = parse_reflector_input(input);
            assert!(
                parsed.is_none(),
                "lowercase module must be rejected for {input:?}, got {parsed:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_short_empty_and_non_ascii_without_panic() {
        // Reject incomplete names and non-ASCII suffixes before byte slicing.
        for input in ["", " ", "AB", "ABC", "é", "REFé"] {
            let parsed = parse_reflector_input(input);
            assert!(
                parsed.is_none(),
                "expected {input:?} to be rejected, got {parsed:?}"
            );
        }
    }

    /// An `App` in D-STAR reflector-input mode with a command channel
    /// attached, plus the receiving end for asserting on emitted commands.
    fn reflector_app() -> (App, Receiver<RadioCommand>) {
        let (mut app, rx) = command_app();
        app.dstar_reflector_input = Some(String::new());
        (app, rx)
    }

    fn aprs_compose_app() -> (App, Receiver<RadioCommand>) {
        let (mut app, rx) = command_app();
        app.aprs_compose = Some(String::new());
        app.aprs_stations.push(AprsStationCache {
            callsign: "W1AW".to_owned(),
            latitude: None,
            longitude: None,
            speed_knots: None,
            course_degrees: None,
            symbol_table: None,
            symbol_code: None,
            comment: None,
            packet_count: 1,
            last_path: Vec::new(),
            last_heard: std::time::Instant::now(),
        });
        (app, rx)
    }

    #[test]
    fn aprs_compose_emits_validated_message_values() -> TestResult {
        let (mut app, mut rx) = aprs_compose_app();
        type_str(&mut app, "Hello");
        press(&mut app, KeyCode::Enter);

        match rx.try_recv()? {
            RadioCommand::SendAprsMessage { addressee, text } => {
                assert_eq!(addressee.as_str(), "W1AW");
                assert_eq!(text.as_str(), "Hello");
                Ok(())
            }
            other => Err(format!("expected SendAprsMessage, got {other:?}").into()),
        }
    }

    #[test]
    fn aprs_compose_rejects_unrepresentable_text_before_radio_task() -> TestResult {
        let (mut app, mut rx) = aprs_compose_app();
        type_str(&mut app, "reserved | telemetry delimiter");
        press(&mut app, KeyCode::Enter);

        assert!(
            rx.try_recv().is_err(),
            "invalid text emitted a radio command"
        );
        let status = app.status_message.ok_or("expected validation status")?;
        assert!(status.contains("Invalid APRS message"));
        Ok(())
    }

    #[test]
    fn vox_delay_adjust_clamps_to_the_lib_maximum_and_reports_only_sent_values() -> TestResult {
        use kenwood_thd75::types::VoxDelay;

        let (mut app, mut rx) = command_app();
        app.main_view = MainView::SettingsCat;
        app.settings_cat_index = cat_settings()
            .iter()
            .position(|row| matches!(row, SettingRow::VoxDelay))
            .ok_or("VOX delay row missing from the CAT settings table")?;
        app.state.vox_delay = Some(VoxDelay::MS_3000);

        app.adjust_setting(1);

        let sent = rx.try_recv()?;
        assert!(
            matches!(sent, RadioCommand::SetVoxDelay(delay) if delay.as_raw() == VoxDelay::MAX),
            "stepping past the top must clamp to the radio's maximum: {sent:?}"
        );
        let message = app.status_message.clone().ok_or("missing status message")?;
        assert!(
            message.contains(&VoxDelay::MAX.to_string()),
            "the status line must report the value actually sent: {message}"
        );
        Ok(())
    }

    fn app_with_gps_settings(settings: GpsSettings) -> (App, Receiver<RadioCommand>) {
        let (mut app, rx) = command_app();
        app.state.gps_settings = Some(settings);
        (app, rx)
    }

    fn assert_gps_settings(rx: &mut Receiver<RadioCommand>, expected: GpsSettings) -> TestResult {
        match rx.try_recv()? {
            RadioCommand::SetGpsSettings(actual) => {
                assert_eq!(actual, expected);
                Ok(())
            }
            other => Err(format!("expected SetGpsSettings, got {other:?}").into()),
        }
    }

    #[test]
    fn gps_shortcuts_preserve_the_other_settings_field() -> TestResult {
        let (mut gps_app, mut gps_rx) =
            app_with_gps_settings(GpsSettings::new().with_pc_output(true));
        gps_app.toggle_gps();
        assert_gps_settings(
            &mut gps_rx,
            GpsSettings::new().with_enabled(true).with_pc_output(true),
        )?;

        let (mut output_app, mut output_rx) =
            app_with_gps_settings(GpsSettings::new().with_enabled(true));
        output_app.toggle_gps_pc_output();
        assert_gps_settings(
            &mut output_rx,
            GpsSettings::new().with_enabled(true).with_pc_output(true),
        )
    }

    #[test]
    fn gps_settings_rows_preserve_the_other_settings_field() -> TestResult {
        let gps_rows = cat_settings();
        let gps_enabled_index = gps_rows
            .iter()
            .position(|row| *row == SettingRow::GpsEnabled)
            .ok_or("GPS enabled row missing")?;
        let gps_output_index = gps_rows
            .iter()
            .position(|row| *row == SettingRow::GpsPcOutput)
            .ok_or("GPS PC output row missing")?;

        let (mut gps_app, mut gps_rx) =
            app_with_gps_settings(GpsSettings::new().with_pc_output(true));
        gps_app.main_view = MainView::SettingsCat;
        gps_app.settings_cat_index = gps_enabled_index;
        gps_app.toggle_setting();
        assert_gps_settings(
            &mut gps_rx,
            GpsSettings::new().with_enabled(true).with_pc_output(true),
        )?;

        let (mut output_app, mut output_rx) =
            app_with_gps_settings(GpsSettings::new().with_enabled(true));
        output_app.main_view = MainView::SettingsCat;
        output_app.settings_cat_index = gps_output_index;
        output_app.toggle_setting();
        assert_gps_settings(
            &mut output_rx,
            GpsSettings::new().with_enabled(true).with_pc_output(true),
        )
    }

    #[test]
    fn gps_toggles_refuse_when_current_settings_are_unavailable() -> TestResult {
        let (mut app, mut rx) = command_app();

        app.toggle_gps();
        assert!(
            rx.try_recv().is_err(),
            "unknown GPS state must not emit a write"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("GPS settings unavailable; reconnect to refresh")
        );

        app.toggle_gps_pc_output();
        assert!(
            rx.try_recv().is_err(),
            "unknown PC-output state must not emit a write"
        );

        app.main_view = MainView::SettingsCat;
        let rows = cat_settings();
        for target in [SettingRow::GpsEnabled, SettingRow::GpsPcOutput] {
            app.settings_cat_index = rows
                .iter()
                .position(|row| *row == target)
                .ok_or("GPS settings row missing")?;
            app.toggle_setting();
            assert!(
                rx.try_recv().is_err(),
                "unknown GPS settings state must not emit a write"
            );
        }
        Ok(())
    }

    fn press(app: &mut App, code: KeyCode) {
        let _render = app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn band_panes_do_not_offer_arbitrary_frequency_entry() {
        let (mut app, mut rx) = command_app();

        for pane in [Pane::BandA, Pane::BandB] {
            app.focus = pane;
            press(&mut app, KeyCode::Char('f'));
            assert_eq!(
                app.input_mode,
                InputMode::Normal,
                "lowercase f must not open an unsupported frequency editor on {pane:?}"
            );
            assert!(
                rx.try_recv().is_err(),
                "lowercase f must not send a radio command on {pane:?}"
            );
        }
    }

    #[test]
    fn aprs_mode_refuses_to_invent_a_station_identity() {
        let (mut app, mut rx) = command_app();

        app.toggle_aprs_mode();

        assert!(
            rx.try_recv().is_err(),
            "a missing radio callsign must not start a transmitting APRS session"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("Cannot enter APRS mode: the radio has no readable APRS callsign")
        );
    }

    #[test]
    fn aprs_mode_uses_the_live_radio_callsign() -> TestResult {
        let (mut app, mut rx) = command_app();
        app.state.aprs_callsign = Some(kenwood_thd75::types::AprsCallsign::new("KQ4NIT-9")?);
        app.target_band = kenwood_thd75::types::Band::B;

        app.toggle_aprs_mode();

        match rx.try_recv()? {
            RadioCommand::EnterAprs { config } => {
                let source = config.source().ok_or("expected a station identity")?;
                assert_eq!(source.to_string(), "KQ4NIT-9");
                assert_eq!(config.data_band(), TncDataBand::B);
                assert_eq!(
                    app.status_message.as_deref(),
                    Some("APRS start on Band B: queued")
                );
                Ok(())
            }
            other => Err(format!("expected EnterAprs, got {other:?}").into()),
        }
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn assert_prepare_link(
        rx: &mut Receiver<RadioCommand>,
        expected_name: &str,
        expected_module: char,
    ) -> TestResult {
        let cmd = rx.try_recv()?;
        match cmd {
            RadioCommand::PrepareReflectorLink { name, module } => {
                assert_eq!(name.to_string(), expected_name, "reflector name");
                assert_eq!(module.as_char(), expected_module, "reflector module");
                Ok(())
            }
            other => Err(format!("expected PrepareReflectorLink, got {other:?}").into()),
        }
    }

    #[test]
    fn reflector_input_non_ascii_is_rejected_without_panic() -> TestResult {
        let (mut app, mut rx) = reflector_app();
        type_str(&mut app, "REFé");
        press(&mut app, KeyCode::Enter);
        let recv = rx.try_recv();
        assert!(
            recv.is_err(),
            "non-ASCII reflector input must not emit a command, got {recv:?}"
        );
        let status = app.status_message.ok_or("expected a status message")?;
        assert!(
            status.contains("Invalid reflector"),
            "expected invalid-reflector status, got {status:?}"
        );
        Ok(())
    }

    #[test]
    fn reflector_single_token_trailing_digit_is_rejected() -> TestResult {
        let (mut app, mut rx) = reflector_app();
        type_str(&mut app, "REF030");
        press(&mut app, KeyCode::Enter);
        let recv = rx.try_recv();
        assert!(
            recv.is_err(),
            "input ending in a digit has no module letter and must not \
             emit a command, got {recv:?}"
        );
        let status = app.status_message.ok_or("expected a status message")?;
        assert!(
            status.contains("Invalid reflector"),
            "expected invalid-reflector status, got {status:?}"
        );
        Ok(())
    }

    #[test]
    fn reflector_single_token_with_module_connects() -> TestResult {
        let (mut app, mut rx) = reflector_app();
        type_str(&mut app, "REF030C");
        press(&mut app, KeyCode::Enter);
        assert_prepare_link(&mut rx, "REF030", 'C')
    }

    #[test]
    fn reflector_two_token_form_connects() -> TestResult {
        let (mut app, mut rx) = reflector_app();
        type_str(&mut app, "REF030 C");
        press(&mut app, KeyCode::Enter);
        assert_prepare_link(&mut rx, "REF030", 'C')
    }

    #[test]
    fn reflector_lowercase_input_is_uppercased() -> TestResult {
        let (mut app, mut rx) = reflector_app();
        type_str(&mut app, "ref030c");
        press(&mut app, KeyCode::Enter);
        assert_prepare_link(&mut rx, "REF030", 'C')
    }
}
