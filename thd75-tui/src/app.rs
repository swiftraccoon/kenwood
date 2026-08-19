use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use kenwood_thd75::error::ValidationError;
use kenwood_thd75::memory::dstar::DstarReadError;
use kenwood_thd75::memory::{MemoryError, MemoryImage};
use kenwood_thd75::types::{
    AfGainLevel, AltitudeRainUnit, AmHighCut, AutoMuteReturnDelay, AutoPowerOff, BacklightControl,
    BacklightTimer, BandMode, BatteryLevel, BatterySaverInterval, BeaconMode, BeatShift,
    CwFilterWidth, CwPitch, DisplayUnits, DtmfPause, DtmfToneDuration, EmrVolume, Frequency,
    FrontPanelPfFunction, GpsSettings, Language, LinkedVolumeLevel, MicSensitivity, NmeaSentences,
    OperatingMode, PcOutputInterface, PowerLevel, RegularChannel, RepeaterCallKey, SMeterReading,
    ScanRestartDelay, ScanResumeMethod, SpeedDistanceUnit, SquelchLevel, SsbHighCut,
    StoredFrontPanelPfAssignment, TemperatureUnit, TransmitTimeout, VoiceAnnounceMode,
    VoiceGuideSpeed, VoxDelay, VoxGain,
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

/// Save raw MCP image to the cache file at `path`.
///
/// Logs errors but does not propagate; a failed cache write should not
/// block radio operation. The user will see a warning in the log.
pub(crate) fn save_cache_to(path: &Path, data: &[u8]) {
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(path = %parent.display(), "failed to create cache dir: {e}");
        return;
    }
    if let Err(e) = std::fs::write(path, data) {
        tracing::error!(path = %path.display(), "failed to write MCP cache: {e}");
    }
}

/// Load cached MCP image from `path`. Returns (image, age).
pub(crate) fn load_cache(path: &Path) -> Option<(MemoryImage, std::time::Duration)> {
    let data = std::fs::read(path).ok()?;
    let age = std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or_default();
    let image = MemoryImage::from_raw(data).ok()?;
    Some((image, age))
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
fn parse_reflector_input(input: &str) -> Option<(String, char)> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    match parts.as_slice() {
        [name, module_token, ..] => {
            let module = module_token.chars().next()?;
            if !module.is_ascii_uppercase() {
                return None;
            }
            Some((name.to_uppercase(), module))
        }
        [single] => {
            if single.len() < 4 {
                return None;
            }
            let module = single.chars().last()?;
            if !module.is_ascii_uppercase() {
                return None;
            }
            // `module` is ASCII (1 byte), so `len - 1` is a char boundary.
            let name = single.get(..single.len() - 1)?;
            Some((name.to_uppercase(), module))
        }
        [] => None,
    }
}

/// Number of rows in the settings list (must match `SettingRow::ALL.len()`).
pub(crate) const SETTINGS_COUNT: usize = 75;

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
            Self::ScanResumeCat => "SR (unqualified)",
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

/// Step a PF-key assignment through the official gapped menu domain.
/// Off-menu bytes are intentionally not coerced to an official assignment.
fn next_pf_key(current: StoredFrontPanelPfAssignment, delta: i8) -> Option<FrontPanelPfFunction> {
    let StoredFrontPanelPfAssignment::Official(current) = current else {
        return None;
    };
    step_selection(
        current,
        delta,
        &[
            FrontPanelPfFunction::Recording,
            FrontPanelPfFunction::VoiceMessage1,
            FrontPanelPfFunction::VoiceMessage2,
            FrontPanelPfFunction::VoiceMessage3,
            FrontPanelPfFunction::VoiceMessage4,
            FrontPanelPfFunction::VoiceGuidance,
            FrontPanelPfFunction::BatteryLevel,
            FrontPanelPfFunction::Vox,
            FrontPanelPfFunction::GroupName,
            FrontPanelPfFunction::Balance,
            FrontPanelPfFunction::Gps,
            FrontPanelPfFunction::TrackLog,
            FrontPanelPfFunction::Squelch,
            FrontPanelPfFunction::Shift,
            FrontPanelPfFunction::Step,
            FrontPanelPfFunction::Power,
            FrontPanelPfFunction::KeyLock,
            FrontPanelPfFunction::Lockout,
            FrontPanelPfFunction::MemoryToVfo,
            FrontPanelPfFunction::ToneSelect,
            FrontPanelPfFunction::NewMemory,
            FrontPanelPfFunction::VoiceAlert,
            FrontPanelPfFunction::LcdBrightness,
            FrontPanelPfFunction::DtmfChannel0,
            FrontPanelPfFunction::EcholinkChannel0,
            FrontPanelPfFunction::Tone1750Hz,
            FrontPanelPfFunction::MemoryInput,
        ],
    )
}

fn next_linked_volume(
    current: LinkedVolumeLevel,
    delta: i8,
) -> Result<LinkedVolumeLevel, ValidationError> {
    let current = current.as_raw();
    let next = if delta > 0 {
        current.saturating_add(1).min(7)
    } else {
        current.saturating_sub(1)
    };
    if next == 0 {
        Ok(LinkedVolumeLevel::VOLUME_LINK)
    } else {
        LinkedVolumeLevel::fixed(next)
    }
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
    /// MCP settings: ~3s per change, brief disconnect.
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
#[derive(Debug, Clone)]
pub(crate) struct BandState {
    pub frequency: Frequency,
    pub mode: OperatingMode,
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
            mode: OperatingMode::Fm,
            s_meter: SMeterReading::ZERO,
            squelch: SquelchLevel::OPEN,
            power_level: PowerLevel::High,
            busy: false,
            attenuator: false,
            step_size: None,
        }
    }
}

/// Aggregated radio state from the poller.
#[derive(Debug, Clone)]
pub(crate) struct RadioState {
    pub band_a: BandState,
    pub band_b: BandState,
    pub battery_level: BatteryLevel,
    pub beep: bool,
    pub band_mode: BandMode,
    pub bluetooth: bool,
    pub vox: bool,
    pub vox_gain: VoxGain,
    pub vox_delay: VoxDelay,
    pub af_gain: AfGainLevel,
    pub firmware_version: String,
    pub radio_type: String,
    pub gps_settings: Option<GpsSettings>,
    /// Validated, nonempty NMEA sentence selection.
    pub gps_sentences: Option<NmeaSentences>,
    /// GPS/Radio operating mode (GM read).
    pub gps_mode: Option<kenwood_thd75::types::GpsRadioMode>,
    pub beacon_mode: BeaconMode,
    pub fine_step: Option<kenwood_thd75::types::FineStep>,
    pub filter_width_ssb: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_cw: Option<kenwood_thd75::types::FilterWidthIndex>,
    pub filter_width_am: Option<kenwood_thd75::types::FilterWidthIndex>,
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
    /// Live APRS My Callsign read through CAT CS.
    pub aprs_callsign: Option<kenwood_thd75::types::AprsCallsign>,
}

impl Default for RadioState {
    fn default() -> Self {
        Self {
            band_a: BandState::default(),
            band_b: BandState::default(),
            battery_level: BatteryLevel::Empty,
            beep: false,
            band_mode: BandMode::Single,
            bluetooth: false,
            vox: false,
            vox_gain: VoxGain::ZERO,
            vox_delay: VoxDelay::MS_250,
            af_gain: AfGainLevel::ZERO,
            firmware_version: String::new(),
            radio_type: String::new(),
            gps_settings: None,
            gps_sentences: None,
            gps_mode: None,
            beacon_mode: BeaconMode::Manual,
            fine_step: None,
            filter_width_ssb: None,
            filter_width_cw: None,
            filter_width_am: None,
            dstar_urcall: String::new(),
            dstar_urcall_suffix: String::new(),
            dstar_rpt1: String::new(),
            dstar_rpt1_suffix: String::new(),
            dstar_rpt2: String::new(),
            dstar_rpt2_suffix: String::new(),
            dstar_gateway_mode: None,
            dstar_slot: None,
            aprs_callsign: None,
        }
    }
}

/// Whether the D-STAR gateway is active in the radio task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DstarMode {
    /// Not in gateway mode: show CAT settings on the D-STAR panel.
    Inactive,
    /// Gateway mode active: `DstarGateway` is running in the radio task.
    Active,
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
    pub addressee: kenwood_thd75::MessageAddressee,
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
    Reading {
        page: u16,
        total: u16,
    },
    Loaded {
        image: MemoryImage,
        modified: bool,
    },
    Writing {
        page: u16,
        total: u16,
        /// Image retained while the write is in flight so success and failure
        /// both return the operator to a usable cached image.
        image: MemoryImage,
    },
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
    /// A single MCP byte was written successfully; update the in-memory
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
    /// APRS did not return a radio with proved ordinary CAT control.
    AprsRecoveryFailed(String),
    /// An APRS event was received from the radio task.
    AprsEvent(kenwood_thd75::AprsEvent),
    /// An APRS message was sent and assigned a message ID for tracking.
    AprsMessageSent {
        addressee: kenwood_thd75::MessageAddressee,
        text: kenwood_thd75::MessageText,
        message_id: kenwood_thd75::MessageId,
    },
    /// Error from the APRS subsystem.
    AprsError(String),
    /// The radio task has entered D-STAR gateway mode successfully.
    DstarStarted,
    /// The radio task has exited D-STAR gateway mode.
    DstarStopped,
    /// D-STAR did not return a radio with proved ordinary CAT control.
    DstarRecoveryFailed(String),
    /// A D-STAR event was received from the radio task (gateway mode).
    DstarEvent(kenwood_thd75::DstarEvent),
    /// Error from the D-STAR subsystem.
    DstarError(String),
    Quit,
}

/// Central application state.
#[expect(
    clippy::struct_excessive_bools,
    reason = "App aggregates the TUI's global mode flags (connected, should_quit, \
              quit_pending, show_help) plus several view-specific toggles. Each flag \
              drives a distinct Ratatui branch in the event/render loop and mutates at a \
              distinct site; collapsing into a `HashSet<AppFlag>` would force every \
              dispatch to go through a hash lookup for no reader benefit, and the flags \
              are not semantically related enough to form a coherent enum. This is the \
              right data model, even if it looks bool-heavy to clippy."
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
    /// Which band receives channel recalls and step/mode changes.
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
    pub dstar_mode: DstarMode,
    /// D-STAR last heard entries (gateway mode).
    pub dstar_last_heard: Vec<kenwood_thd75::LastHeardEntry>,
    /// Selected index in the D-STAR last heard list.
    pub dstar_last_heard_index: usize,
    /// Current D-STAR text message (from slow data).
    pub dstar_text_message: Option<kenwood_thd75::SlowDataTextMessage>,
    /// Current D-STAR RX header (gateway mode).
    pub dstar_rx_header: Option<kenwood_thd75::DstarHeader>,
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
    pub(crate) fn filtered_channels(&self) -> Result<Vec<RegularChannel>, MemoryError> {
        if let McpState::Loaded { ref image, .. } = self.mcp {
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
        Self::with_cache_path(port_path, Some(cache_path().as_path()))
    }

    /// Create a new app instance, loading the MCP image from the given cache
    /// file when one is provided.
    ///
    /// `None` skips the disk cache entirely, so construction has no
    /// filesystem side effects; tests use this to avoid reading the real
    /// per-user cache directory.
    pub(crate) fn with_cache_path(port_path: String, cache: Option<&Path>) -> Self {
        let (mcp, status_message) = match cache.and_then(load_cache) {
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
            cache_path: cache.map(Path::to_path_buf),
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
            dstar_mode: DstarMode::Inactive,
            dstar_last_heard: Vec::new(),
            dstar_last_heard_index: 0,
            dstar_text_message: None,
            dstar_rx_header: None,
            dstar_rx_active: false,
            dstar_urcall_input: None,
            dstar_reflector_input: None,
            channel_edit_mode: false,
            channel_edit_field: ChannelEditField::Name,
            channel_edit_buffer: String::new(),
            fm_radio_on: false,
        }
    }

    /// Process a message and update state. Returns true if a render is needed.
    #[expect(
        clippy::too_many_lines,
        reason = "`update` is the TUI's single message-dispatch entry point: it matches \
                  over every `Message` variant (radio state updates, status text, D-STAR \
                  events, APRS events, TX responses, etc.) and translates each into the \
                  corresponding `App` state change. Splitting per variant would create a \
                  fan of one-line helper methods with no real abstraction benefit; the \
                  match statement itself IS the dispatch table."
    )]
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
                if state.aprs_callsign.is_none() {
                    state.aprs_callsign.clone_from(&self.state.aprs_callsign);
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
                } else {
                    self.mcp = McpState::Reading { page, total };
                }
                true
            }
            Message::McpReadComplete(data) => {
                if let Some(ref path) = self.cache_path {
                    save_cache_to(path, &data);
                }
                match MemoryImage::from_raw(data) {
                    Ok(image) => {
                        self.mcp = McpState::Loaded {
                            image,
                            modified: false,
                        };
                        self.status_message = Some("MCP read complete, cached to disk".into());
                    }
                    Err(e) => {
                        self.mcp = McpState::Idle;
                        self.status_message = Some(format!("MCP parse error: {e}"));
                    }
                }
                true
            }
            Message::McpWriteComplete => {
                let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                self.mcp = match previous {
                    McpState::Writing { image, .. } => {
                        if let Some(ref path) = self.cache_path {
                            save_cache_to(path, image.as_raw());
                        }
                        McpState::Loaded {
                            image,
                            modified: false,
                        }
                    }
                    other => other,
                };
                self.status_message = Some("MCP write complete; radio control restored".into());
                true
            }
            Message::McpByteWritten { offset, value } => {
                // Update the cached memory image with the single byte that
                // was just written via MCP, so the TUI stays in sync without
                // requiring a full re-read after the radio's internal reset.
                if let McpState::Loaded { ref mut image, .. } = self.mcp {
                    if let Some(byte) = image.as_raw_mut().get_mut(offset as usize) {
                        *byte = value;
                    }
                    if let Some(ref path) = self.cache_path {
                        save_cache_to(path, image.as_raw());
                    }
                }
                self.status_message = Some(format!(
                    "MCP 0x{offset:04X} = {value}; radio control restored"
                ));
                true
            }
            Message::McpError(err) => {
                // A failed write retains the exact image the operator was
                // trying to write. Only a failed read with no prior image
                // returns to Idle.
                let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                self.mcp = match previous {
                    McpState::Writing { image, .. } => McpState::Loaded {
                        image,
                        modified: true,
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
                    state: AprsMessageState::Pending,
                });
                true
            }
            Message::AprsError(err) => {
                self.status_message = Some(format!("APRS: {err}"));
                true
            }
            Message::DstarStarted => {
                self.dstar_mode = DstarMode::Active;
                self.status_message = Some("D-STAR gateway mode active".into());
                true
            }
            Message::DstarStopped => {
                self.dstar_mode = DstarMode::Inactive;
                self.dstar_rx_active = false;
                self.dstar_rx_header = None;
                self.status_message =
                    Some("D-STAR gateway mode stopped, CAT polling resumed".into());
                true
            }
            Message::DstarRecoveryFailed(error) => {
                self.dstar_mode = DstarMode::Inactive;
                self.dstar_rx_active = false;
                self.dstar_rx_header = None;
                self.connected = false;
                self.status_message = Some(format!(
                    "D-STAR session ended without usable CAT control: {error}; reconnecting..."
                ));
                true
            }
            Message::DstarEvent(event) => {
                self.handle_dstar_event(event);
                true
            }
            Message::DstarError(err) => {
                self.status_message = Some(format!("D-STAR: {err}"));
                true
            }
        }
    }

    #[expect(
        clippy::cognitive_complexity,
        clippy::too_many_lines,
        reason = "`handle_key` is the TUI's single keyboard dispatch entry point: it \
                  branches on (`InputMode`, `MainView`, key) to route every keystroke in \
                  the app. Splitting per mode would either (a) duplicate the Ctrl-C \
                  global-quit handling at every site or (b) add a second-level dispatch \
                  layer for no reader benefit. The function is linear: read input mode, \
                  match keystroke, update state, return whether a redraw is needed. \
                  `too_many_lines` fires because this single dispatch covers every mode, \
                  which is the correct granularity."
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
                        let addressee =
                            match kenwood_thd75::MessageAddressee::new(&station.callsign) {
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
                        let _send = tx.send(crate::event::RadioCommand::SendAprsMessage {
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
                        let _send = tx.send(crate::event::RadioCommand::SetUrcall {
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
                    let Some((name, module)) = parse_reflector_input(&input) else {
                        self.status_message = Some("Invalid reflector (e.g. REF030 C)".into());
                        return true;
                    };
                    if let Some(ref tx) = self.cmd_tx {
                        let _send = tx.send(crate::event::RadioCommand::PrepareReflectorLink {
                            name: name.clone(),
                            module,
                        });
                        self.status_message = Some(format!(
                            "Preparing {name} module {module}; key up to send the link command"
                        ));
                    }
                }
                KeyCode::Backspace => {
                    let _ = buf.pop();
                }
                KeyCode::Char(c) => {
                    // Only characters that can appear in a reflector spec:
                    // rejecting non-ASCII here keeps the buffer single-byte
                    // per char, so parsing can never split a char boundary.
                    if buf.len() < 12 && (c.is_ascii_alphanumeric() || c == ' ') {
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
                match self.filtered_channels() {
                    Ok(used) if used.get(self.channel_list_index).is_some() => {
                        self.channel_edit_mode = true;
                        self.channel_edit_field = ChannelEditField::Name;
                        self.channel_edit_buffer.clear();
                        self.status_message =
                            Some("Edit mode: Tab=next field, Enter=apply, Esc=cancel".into());
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
                    self.toggle_dstar_mode();
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
                        MainView::Dstar => {
                            let max = self.dstar_last_heard.len().saturating_sub(1);
                            self.dstar_last_heard_index =
                                self.dstar_last_heard_index.saturating_add(1).min(max);
                        }
                        MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _send = tx.send(crate::event::RadioCommand::FreqDown(
                                kenwood_thd75::types::Band::A,
                            ));
                        }
                    }
                    Pane::BandB => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _send = tx.send(crate::event::RadioCommand::FreqDown(
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
                        MainView::Dstar => {
                            self.dstar_last_heard_index =
                                self.dstar_last_heard_index.saturating_sub(1);
                        }
                        MainView::Gps | MainView::Mcp | MainView::FmRadio => {}
                    },
                    Pane::BandA => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _send = tx.send(crate::event::RadioCommand::FreqUp(
                                kenwood_thd75::types::Band::A,
                            ));
                        }
                    }
                    Pane::BandB => {
                        if let Some(ref tx) = self.cmd_tx {
                            let _send = tx.send(crate::event::RadioCommand::FreqUp(
                                kenwood_thd75::types::Band::B,
                            ));
                        }
                    }
                    Pane::Detail => {}
                }
                true
            }
            KeyCode::Enter if self.focus == Pane::Main && self.main_view == MainView::Channels => {
                match self.filtered_channels() {
                    Ok(used) => {
                        if let Some(&channel) = used.get(self.channel_list_index)
                            && let Some(ref tx) = self.cmd_tx
                        {
                            let band_label = if self.target_band == kenwood_thd75::types::Band::B {
                                "B"
                            } else {
                                "A"
                            };
                            let _send = tx.send(crate::event::RadioCommand::TuneChannel {
                                band: self.target_band,
                                channel,
                            });
                            self.status_message =
                                Some(format!("Tuning Band {band_label} to channel {channel}..."));
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
                let next = match current {
                    PowerLevel::High => PowerLevel::Medium,
                    PowerLevel::Medium => PowerLevel::Low,
                    PowerLevel::Low => PowerLevel::ExtraLow,
                    PowerLevel::ExtraLow => PowerLevel::High,
                };
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::SetPower { band, level: next });
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
                    let _send = tx.send(crate::event::RadioCommand::SetAttenuator {
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
                        self.state.band_a.squelch.as_raw(),
                    )
                } else {
                    (
                        kenwood_thd75::types::Band::B,
                        self.state.band_b.squelch.as_raw(),
                    )
                };
                let next = cur.saturating_sub(1);
                if let (Some(tx), Ok(level)) = (&self.cmd_tx, SquelchLevel::new(next)) {
                    let _send = tx.send(crate::event::RadioCommand::SetSquelch { band, level });
                    self.status_message = Some(format!("Squelch → {next}"));
                }
                true
            }
            KeyCode::Char(']') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (
                        kenwood_thd75::types::Band::A,
                        self.state.band_a.squelch.as_raw(),
                    )
                } else {
                    (
                        kenwood_thd75::types::Band::B,
                        self.state.band_b.squelch.as_raw(),
                    )
                };
                let next = cur.saturating_add(1).min(6);
                if let (Some(tx), Ok(level)) = (&self.cmd_tx, SquelchLevel::new(next)) {
                    let _send = tx.send(crate::event::RadioCommand::SetSquelch { band, level });
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
                    // Use 0,0 as placeholder; real GPS position would come from the radio.
                    let _send = tx.send(crate::event::RadioCommand::BeaconPosition {
                        latitude: kenwood_thd75::Latitude::EQUATOR,
                        longitude: kenwood_thd75::Longitude::PRIME_MERIDIAN,
                        comment: kenwood_thd75::PositionReportText::default(),
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
                        let _send = tx.send(crate::event::RadioCommand::ReadMemory);
                    }
                }
                true
            }
            KeyCode::Char('w') if self.main_view == MainView::Mcp => {
                let previous = std::mem::replace(&mut self.mcp, McpState::Idle);
                if let McpState::Loaded { image, .. } = previous {
                    let data = image.as_raw().to_vec();
                    self.mcp = McpState::Writing {
                        page: 0,
                        total: kenwood_thd75::protocol::programming::TOTAL_PAGES,
                        image,
                    };
                    self.status_message = Some("Starting MCP write...".into());
                    if let Some(ref tx) = self.cmd_tx {
                        let _send = tx.send(crate::event::RadioCommand::WriteMemory(data));
                    }
                } else {
                    self.mcp = previous;
                }
                true
            }
            // D-STAR: set CQ (URCALL = CQCQCQ)
            KeyCode::Char('C')
                if self.main_view == MainView::Dstar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DstarMode::Inactive =>
            {
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::SetCQ);
                    self.status_message = Some("URCALL set to CQCQCQ".into());
                }
                true
            }
            // D-STAR: set URCALL (prompt)
            KeyCode::Char('u')
                if self.main_view == MainView::Dstar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DstarMode::Inactive =>
            {
                self.dstar_urcall_input = Some(String::new());
                true
            }
            // D-STAR: prepare reflector link (prompt)
            KeyCode::Char('r')
                if self.main_view == MainView::Dstar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DstarMode::Inactive =>
            {
                self.dstar_reflector_input = Some(String::new());
                true
            }
            // D-STAR: prepare reflector unlink
            KeyCode::Char('U')
                if self.main_view == MainView::Dstar
                    && self.focus == Pane::Main
                    && self.dstar_mode == DstarMode::Inactive =>
            {
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::PrepareReflectorUnlink);
                    self.status_message =
                        Some("Preparing unlink; key up to send the command".into());
                }
                true
            }
            _ => false,
        }
    }

    /// Toggle a boolean setting or show hint for numeric ones.
    #[expect(
        clippy::too_many_lines,
        reason = "`toggle_setting` is paired with `adjust_setting`; together they are \
                  the TUI's dispatch into the settings-mutation path. It matches over \
                  every settings row and issues the corresponding RadioCommand. Splitting \
                  would duplicate the row-resolution logic; keeping it linear matches the \
                  settings table's order."
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
        if let Some(ref tx) = self.cmd_tx.clone() {
            match row {
                SettingRow::DualBand => {
                    let next = match self.state.band_mode {
                        BandMode::Dual => BandMode::Single,
                        BandMode::Single => BandMode::Dual,
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetBandMode(next));
                    self.status_message = Some(format!("Band mode → {next}"));
                    return;
                }
                SettingRow::Bluetooth => {
                    let next = !self.state.bluetooth;
                    let _send = tx.send(crate::event::RadioCommand::SetBluetooth(next));
                    self.status_message = Some(format!("Bluetooth → {}", on_off(next)));
                    return;
                }
                SettingRow::VoxEnabled => {
                    let next = !self.state.vox;
                    let _send = tx.send(crate::event::RadioCommand::SetVox(next));
                    self.status_message = Some(format!("VOX → {}", on_off(next)));
                    return;
                }
                SettingRow::AttenuatorA => {
                    let next = !self.state.band_a.attenuator;
                    let _send = tx.send(crate::event::RadioCommand::SetAttenuator {
                        band: kenwood_thd75::types::Band::A,
                        enabled: next,
                    });
                    self.status_message = Some(format!("Atten A → {}", on_off(next)));
                    return;
                }
                SettingRow::AttenuatorB => {
                    let next = !self.state.band_b.attenuator;
                    let _send = tx.send(crate::event::RadioCommand::SetAttenuator {
                        band: kenwood_thd75::types::Band::B,
                        enabled: next,
                    });
                    self.status_message = Some(format!("Atten B → {}", on_off(next)));
                    return;
                }
                SettingRow::FmRadio => {
                    self.status_message = Some(
                        "FM Radio is read-only: retained hardware evidence rejects FR writes"
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
                    let settings = GpsSettings::new(next, current.pc_output());
                    let _send = tx.send(crate::event::RadioCommand::SetGpsSettings(settings));
                    self.status_message = Some(format!("GPS → {}", on_off(next)));
                    return;
                }
                SettingRow::GpsPcOutput => {
                    let Some(current) = self.state.gps_settings else {
                        self.status_message =
                            Some("GPS settings unavailable; reconnect to refresh".into());
                        return;
                    };
                    let next = !current.pc_output();
                    let settings = GpsSettings::new(current.enabled(), next);
                    let _send = tx.send(crate::event::RadioCommand::SetGpsSettings(settings));
                    self.status_message = Some(format!("GPS PC Output → {}", on_off(next)));
                    return;
                }
                SettingRow::AutoInfo => {
                    self.status_message = Some("Auto Info: not yet wired".into());
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
        let Some(tx) = self.cmd_tx.clone() else {
            return;
        };

        let McpState::Loaded { ref mut image, .. } = self.mcp else {
            self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
            return;
        };

        macro_rules! toggle_bool {
            ($getter:ident, $setter:ident, $label:expr) => {{
                match image.settings().$getter() {
                    Ok(current) => {
                        let new_val = !current;
                        match image.modify_setting(|writer| writer.$setter(new_val)) {
                            Ok(Some((offset, value))) => {
                                let _send = tx.send(crate::event::RadioCommand::McpWriteByte {
                                    offset,
                                    value,
                                });
                                self.status_message = Some(format!(
                                    "{} → {}, applying...",
                                    $label,
                                    on_off(new_val)
                                ));
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
        reason = "`adjust_setting` is the TUI's single entry point for +/- delta edits to \
                  every CAT- or MCP-backed numeric setting. Dispatches on the settings \
                  row's type (enum CAT setting, numeric CAT setting, MCP setting, …) to \
                  the appropriate wire-format encoder and sends via the `RadioCommand` \
                  channel. Splitting per setting-type would require duplicating the \
                  delta-clamping and feedback logic at every site."
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
        if let Some(ref tx) = self.cmd_tx.clone() {
            match row {
                SettingRow::SquelchA => {
                    let cur = self.state.band_a.squelch.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        let _send = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::A,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch A → {next}"));
                    return;
                }
                SettingRow::SquelchB => {
                    let cur = self.state.band_b.squelch.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = SquelchLevel::new(next) {
                        let _send = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::B,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch B → {next}"));
                    return;
                }
                SettingRow::VoxGain => {
                    let cur = self.state.vox_gain.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(VoxGain::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(gain) = VoxGain::new(next) {
                        let _send = tx.send(crate::event::RadioCommand::SetVoxGain(gain));
                        self.status_message = Some(format!("VOX Gain → {next}"));
                    }
                    return;
                }
                SettingRow::VoxDelay => {
                    let cur = self.state.vox_delay.as_raw();
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(VoxDelay::MAX)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(delay) = VoxDelay::new(next) {
                        let _send = tx.send(crate::event::RadioCommand::SetVoxDelay(delay));
                        self.status_message = Some(format!("VOX Delay → {next}"));
                    }
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
                    let Some(&next) = steps.get(next_idx) else {
                        return;
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetStepSize {
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
                    let Some(&next) = steps.get(next_idx) else {
                        return;
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetStepSize {
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
                    self.status_message = Some(
                        "SR is quarantined; configure scan resume through Menu 130/131".into(),
                    );
                    return;
                }
                SettingRow::PowerA => {
                    let next = match self.state.band_a.power_level {
                        PowerLevel::High => PowerLevel::Medium,
                        PowerLevel::Medium => PowerLevel::Low,
                        PowerLevel::Low => PowerLevel::ExtraLow,
                        PowerLevel::ExtraLow => PowerLevel::High,
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetPower {
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
                    let _send = tx.send(crate::event::RadioCommand::SetPower {
                        band: kenwood_thd75::types::Band::B,
                        level: next,
                    });
                    self.status_message = Some(format!("Power B → {next}"));
                    return;
                }
                SettingRow::OperatingModeA => {
                    use kenwood_thd75::types::OperatingMode;
                    let next = match self.state.band_a.mode {
                        OperatingMode::Fm => OperatingMode::Nfm,
                        OperatingMode::Nfm => OperatingMode::Am,
                        OperatingMode::Am => OperatingMode::Lsb,
                        OperatingMode::Lsb => OperatingMode::Usb,
                        OperatingMode::Usb => OperatingMode::Cw,
                        OperatingMode::Cw => OperatingMode::Dv,
                        OperatingMode::Dv => OperatingMode::Dr,
                        OperatingMode::Dr => OperatingMode::Wfm,
                        OperatingMode::Wfm => OperatingMode::CwReverse,
                        OperatingMode::CwReverse => OperatingMode::Fm,
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetOperatingMode {
                        band: kenwood_thd75::types::Band::A,
                        mode: next,
                    });
                    self.status_message = Some(format!("Mode A → {next}"));
                    return;
                }
                SettingRow::OperatingModeB => {
                    use kenwood_thd75::types::OperatingMode;
                    let next = match self.state.band_b.mode {
                        OperatingMode::Fm => OperatingMode::Nfm,
                        OperatingMode::Nfm => OperatingMode::Am,
                        OperatingMode::Am => OperatingMode::Lsb,
                        OperatingMode::Lsb => OperatingMode::Usb,
                        OperatingMode::Usb => OperatingMode::Cw,
                        OperatingMode::Cw => OperatingMode::Dv,
                        OperatingMode::Dv => OperatingMode::Dr,
                        OperatingMode::Dr => OperatingMode::Wfm,
                        OperatingMode::Wfm => OperatingMode::CwReverse,
                        OperatingMode::CwReverse => OperatingMode::Fm,
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetOperatingMode {
                        band: kenwood_thd75::types::Band::B,
                        mode: next,
                    });
                    self.status_message = Some(format!("Mode B → {next}"));
                    return;
                }
                SettingRow::ActiveBand => {
                    self.status_message = Some("Active Band: BC command not yet wired".into());
                    return;
                }
                SettingRow::TuningModeA => {
                    self.status_message = Some("VFO/Mem A: VM command not yet wired".into());
                    return;
                }
                SettingRow::TuningModeB => {
                    self.status_message = Some("VFO/Mem B: VM command not yet wired".into());
                    return;
                }
                SettingRow::PacketDataRate => {
                    let data_rate = if delta > 0 {
                        kenwood_thd75::types::PacketDataRate::Bps9600
                    } else {
                        kenwood_thd75::types::PacketDataRate::Bps1200
                    };
                    let _send = tx.send(crate::event::RadioCommand::SetPacketDataRate(data_rate));
                    self.status_message = Some(format!("Packet Data Rate → {data_rate}"));
                    return;
                }
                SettingRow::BeaconMode => {
                    let cur = u8::from(self.state.beacon_mode);
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(3)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(mode) = BeaconMode::try_from(next) {
                        let _send = tx.send(crate::event::RadioCommand::SetBeaconMode(mode));
                        self.status_message = Some(format!("Beacon Mode → {mode}"));
                    }
                    return;
                }
                SettingRow::DstarSlot => {
                    self.status_message = Some("D-STAR Slot: not yet polled, cannot adjust".into());
                    return;
                }
                _ => {}
            }
        }

        // MCP-backed numeric settings: write directly via single-page MCP
        let Some(tx) = self.cmd_tx.clone() else {
            return;
        };

        let McpState::Loaded { ref mut image, .. } = self.mcp else {
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
                            match image.modify_setting(|writer| writer.$setter(next)) {
                                Ok(Some((offset, value))) => {
                                    let _send = tx.send(crate::event::RadioCommand::McpWriteByte {
                                        offset,
                                        value,
                                    });
                                    self.status_message =
                                        Some(format!("{} → {}, applying...", $label, display));
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
                            match image.modify_setting(|writer| writer.$setter(next)) {
                                Ok(Some((offset, value))) => {
                                    let _send = tx.send(crate::event::RadioCommand::McpWriteByte {
                                        offset,
                                        value,
                                    });
                                    self.status_message =
                                        Some(format!("{} → {}, applying...", $label, display));
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
                        &[
                            SsbHighCut::Khz2_2,
                            SsbHighCut::Khz2_4,
                            SsbHighCut::Khz2_6,
                            SsbHighCut::Khz2_8,
                            SsbHighCut::Khz3_0,
                        ],
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
                        &[
                            CwFilterWidth::Khz0_3,
                            CwFilterWidth::Khz0_5,
                            CwFilterWidth::Khz1_0,
                            CwFilterWidth::Khz1_5,
                            CwFilterWidth::Khz2_0,
                        ],
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
                        &[
                            AmHighCut::Khz3_0,
                            AmHighCut::Khz4_5,
                            AmHighCut::Khz6_0,
                            AmHighCut::Khz7_5,
                        ],
                    ),
                    "AM High Cut",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::ScanResume => {
                adjust_typed!(
                    scan_resume,
                    set_scan_resume,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            ScanResumeMethod::TimeOperated,
                            ScanResumeMethod::CarrierOperated,
                            ScanResumeMethod::Seek,
                        ],
                    ),
                    "Scan Resume",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::DigitalScanResume => {
                adjust_typed!(
                    digital_scan_resume,
                    set_digital_scan_resume,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            ScanResumeMethod::TimeOperated,
                            ScanResumeMethod::CarrierOperated,
                            ScanResumeMethod::Seek,
                        ],
                    ),
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
                            current.as_seconds().saturating_add(1).min(10)
                        } else {
                            current.as_seconds().saturating_sub(1).max(1)
                        };
                        ScanRestartDelay::new(seconds)
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
                            current.as_seconds().saturating_add(1).min(10)
                        } else {
                            current.as_seconds().saturating_sub(1).max(1)
                        };
                        ScanRestartDelay::new(seconds)
                    },
                    "Scan Restart Carrier",
                    |value: ScanRestartDelay| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::TimeoutTimer => {
                adjust_typed!(
                    timeout_timer,
                    set_timeout_timer,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            TransmitTimeout::Seconds30,
                            TransmitTimeout::Seconds60,
                            TransmitTimeout::Seconds90,
                            TransmitTimeout::Seconds120,
                            TransmitTimeout::Seconds150,
                            TransmitTimeout::Seconds180,
                            TransmitTimeout::Seconds210,
                            TransmitTimeout::Seconds240,
                            TransmitTimeout::Seconds270,
                            TransmitTimeout::Seconds300,
                            TransmitTimeout::Seconds600,
                        ],
                    ),
                    "Timeout Timer",
                    |value: TransmitTimeout| format!("{} s", value.as_seconds())
                );
            }
            SettingRow::BeatShift => {
                adjust_typed!(
                    beat_shift,
                    set_beat_shift,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            BeatShift::Type1,
                            BeatShift::Type2,
                            BeatShift::Type3,
                            BeatShift::Type4,
                            BeatShift::Type5,
                            BeatShift::Type6,
                            BeatShift::Type7,
                            BeatShift::Type8,
                        ],
                    ),
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
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            DtmfToneDuration::Ms50,
                            DtmfToneDuration::Ms100,
                            DtmfToneDuration::Ms150,
                        ],
                    ),
                    "DTMF Speed",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::DtmfPauseTime => {
                adjust_typed!(
                    dtmf_pause_time,
                    set_dtmf_pause_time,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            DtmfPause::Ms100,
                            DtmfPause::Ms250,
                            DtmfPause::Ms500,
                            DtmfPause::Ms750,
                            DtmfPause::Ms1000,
                            DtmfPause::Ms1500,
                            DtmfPause::Ms2000,
                        ],
                    ),
                    "DTMF Pause",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::RepeaterCallKey => {
                adjust_typed!(
                    repeater_call_key,
                    set_repeater_call_key,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[RepeaterCallKey::CallChannel, RepeaterCallKey::Tone1750Hz,],
                    ),
                    "Call Key",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::MicSensitivity => {
                adjust_typed!(
                    mic_sensitivity,
                    set_mic_sensitivity,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            MicSensitivity::High,
                            MicSensitivity::Medium,
                            MicSensitivity::Low,
                        ],
                    ),
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
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            BacklightControl::Manual,
                            BacklightControl::On,
                            BacklightControl::Auto,
                            BacklightControl::AutoDcIn,
                        ],
                    ),
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
                            current.as_seconds().saturating_add(1).min(60)
                        } else {
                            current.as_seconds().saturating_sub(1).max(3)
                        };
                        BacklightTimer::new(seconds)
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
                            current.as_seconds().saturating_add(1).min(10)
                        } else {
                            current.as_seconds().saturating_sub(1).max(1)
                        };
                        AutoMuteReturnDelay::new(seconds)
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
                        &[
                            VoiceAnnounceMode::Off,
                            VoiceAnnounceMode::Manual,
                            VoiceAnnounceMode::Auto1,
                            VoiceAnnounceMode::Auto2,
                        ],
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
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            VoiceGuideSpeed::Speed1,
                            VoiceGuideSpeed::Speed2,
                            VoiceGuideSpeed::Speed3,
                            VoiceGuideSpeed::Speed4,
                        ],
                    ),
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
                        &[
                            SpeedDistanceUnit::MilesPerHour,
                            SpeedDistanceUnit::KilometersPerHour,
                            SpeedDistanceUnit::Knots,
                        ],
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
                        &[AltitudeRainUnit::FeetInch, AltitudeRainUnit::MetersMm],
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
                        &[TemperatureUnit::Fahrenheit, TemperatureUnit::Celsius],
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
                        &[PcOutputInterface::Usb, PcOutputInterface::Bluetooth],
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
                        &[PcOutputInterface::Usb, PcOutputInterface::Bluetooth],
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
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[Language::English, Language::Japanese],
                    ),
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
                        &[
                            BatterySaverInterval::Off,
                            BatterySaverInterval::Seconds0_2,
                            BatterySaverInterval::Seconds0_4,
                            BatterySaverInterval::Seconds0_6,
                            BatterySaverInterval::Seconds0_8,
                            BatterySaverInterval::Seconds1,
                            BatterySaverInterval::Seconds2,
                            BatterySaverInterval::Seconds3,
                            BatterySaverInterval::Seconds4,
                            BatterySaverInterval::Seconds5,
                        ],
                    ),
                    "Battery Saver",
                    |value| format!("{value:?}")
                );
            }
            SettingRow::AutoPowerOff => {
                adjust_typed!(
                    auto_power_off,
                    set_auto_power_off,
                    |current, direction| step_selection(
                        current,
                        direction,
                        &[
                            AutoPowerOff::Off,
                            AutoPowerOff::Min15,
                            AutoPowerOff::Min30,
                            AutoPowerOff::Min60,
                        ],
                    ),
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
                // Build a minimal cache entry from position data.
                let known = self.aprs_stations.iter().any(|s| s.callsign == source);
                if known {
                    if let Some(cached) =
                        self.aprs_stations.iter_mut().find(|s| s.callsign == source)
                    {
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
                    }
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
                if let Some(m) = self
                    .aprs_messages
                    .iter_mut()
                    .find(|m| m.message_id.as_str() == id.as_str())
                {
                    m.state = AprsMessageState::Delivered;
                }
                self.status_message = Some(format!("Message {id} delivered"));
            }
            AprsEvent::MessageRejected(id) => {
                if let Some(m) = self
                    .aprs_messages
                    .iter_mut()
                    .find(|m| m.message_id.as_str() == id.as_str())
                {
                    m.state = AprsMessageState::Rejected;
                }
                self.status_message = Some(format!("Message {id} rejected"));
            }
            AprsEvent::MessageExpired(id) => {
                if let Some(m) = self
                    .aprs_messages
                    .iter_mut()
                    .find(|m| m.message_id.as_str() == id.as_str())
                {
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
    }

    /// Toggle APRS mode on or off.
    fn handle_dstar_event(&mut self, event: kenwood_thd75::DstarEvent) {
        use kenwood_thd75::DstarEvent;
        match event {
            DstarEvent::VoiceStart(header) => {
                self.dstar_rx_active = true;
                self.dstar_rx_header = Some(header);
                self.dstar_text_message = None;
            }
            DstarEvent::VoiceData(_frame) => {
                // Voice data: no UI action needed.
            }
            DstarEvent::VoiceEnd => {
                self.dstar_rx_active = false;
            }
            DstarEvent::VoiceLost => {
                self.dstar_rx_active = false;
                self.status_message = Some("D-STAR: voice lost (no clean EOT)".into());
            }
            DstarEvent::EventsDropped { count } => {
                self.dstar_rx_active = false;
                self.dstar_rx_header = None;
                self.status_message =
                    Some(format!("D-STAR: modem event stream lost {count} event(s)"));
            }
            DstarEvent::ProtocolViolation(violation) => {
                self.status_message = Some(format!("D-STAR: {violation}"));
            }
            DstarEvent::TextMessage(text) => {
                self.dstar_text_message = Some(text);
            }
            DstarEvent::StationHeard(entry) => {
                // Update the last-heard list (newest first).
                if let Some(idx) = self
                    .dstar_last_heard
                    .iter()
                    .position(|e| e.callsign == entry.callsign)
                {
                    let _removed = self.dstar_last_heard.remove(idx);
                }
                self.dstar_last_heard.insert(0, entry);
                // Limit to 100 entries.
                self.dstar_last_heard.truncate(100);
            }
            DstarEvent::UrCallCommand(action) => {
                self.status_message = Some(format!("D-STAR: URCALL command detected: {action:?}"));
            }
            DstarEvent::StatusUpdate(_status) => {
                // Modem status: no UI action needed.
            }
            DstarEvent::ModemEvent(event) => {
                self.status_message = Some(format!("D-STAR modem: {event:?}"));
            }
        }
    }

    fn toggle_gps(&mut self) {
        let Some(current) = self.state.gps_settings else {
            self.status_message = Some("GPS settings unavailable; reconnect to refresh".into());
            return;
        };
        let next = !current.enabled();
        if let Some(ref tx) = self.cmd_tx {
            let settings = GpsSettings::new(next, current.pc_output());
            let _send = tx.send(crate::event::RadioCommand::SetGpsSettings(settings));
            self.status_message =
                Some(format!("GPS {}", if next { "enabled" } else { "disabled" }));
        }
    }

    fn toggle_gps_pc_output(&mut self) {
        let Some(current) = self.state.gps_settings else {
            self.status_message = Some("GPS settings unavailable; reconnect to refresh".into());
            return;
        };
        let next = !current.pc_output();
        if let Some(ref tx) = self.cmd_tx {
            let settings = GpsSettings::new(current.enabled(), next);
            let _send = tx.send(crate::event::RadioCommand::SetGpsSettings(settings));
            self.status_message = Some(format!(
                "GPS PC Output {}",
                if next { "enabled" } else { "disabled" }
            ));
        }
    }

    fn toggle_dstar_mode(&mut self) {
        match self.dstar_mode {
            DstarMode::Inactive => {
                let callsign = if let McpState::Loaded { image, .. } = &self.mcp {
                    match image.dstar().my_callsign() {
                        Ok(Some(callsign)) => callsign,
                        Ok(None) => {
                            self.status_message = Some(
                                "D-STAR MY callsign is not configured; set Menu 610 before entering gateway mode"
                                    .into(),
                            );
                            return;
                        }
                        Err(error @ DstarReadError::MissingRange { .. }) => {
                            self.status_message =
                                Some(format!("D-STAR MY callsign data is incomplete: {error}"));
                            return;
                        }
                        Err(error) => {
                            self.status_message =
                                Some(format!("D-STAR MY callsign data is invalid: {error}"));
                            return;
                        }
                    }
                } else {
                    self.status_message = Some(
                        "D-STAR MY callsign unavailable; load MCP data before entering gateway mode"
                            .into(),
                    );
                    return;
                };

                let config = kenwood_thd75::DstarGatewayConfig::new(callsign);
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::EnterDstar { config });
                    self.status_message = Some("Entering D-STAR gateway mode...".into());
                }
            }
            DstarMode::Active => {
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::ExitDstar);
                    self.status_message = Some("Exiting D-STAR gateway mode...".into());
                }
            }
        }
    }

    fn toggle_fm_radio(&mut self) {
        self.status_message =
            Some("FM Radio is read-only: retained hardware evidence rejects FR writes".into());
    }

    /// Apply a channel edit from the edit buffer.
    ///
    /// This does not write the selected memory record. Mode changes only the
    /// live band, and the remaining fields report that no editor is available.
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
                // Channel name editing requires MCP write (no CAT command for name-only).
                self.status_message = Some(format!(
                    "Ch {ch_num}: name editing requires MCP write; use MCP panel (m)"
                ));
            }
            ChannelEditField::OperatingMode => {
                // Cycle mode via CAT
                if let Some(ref tx) = self.cmd_tx {
                    use kenwood_thd75::types::OperatingMode;
                    let mode = match buf.to_uppercase().as_str() {
                        "FM" => Some(OperatingMode::Fm),
                        "NFM" => Some(OperatingMode::Nfm),
                        "AM" => Some(OperatingMode::Am),
                        "DV" => Some(OperatingMode::Dv),
                        "LSB" => Some(OperatingMode::Lsb),
                        "USB" => Some(OperatingMode::Usb),
                        "CW" => Some(OperatingMode::Cw),
                        "DR" => Some(OperatingMode::Dr),
                        "WFM" => Some(OperatingMode::Wfm),
                        _ => None,
                    };
                    if let Some(mode) = mode {
                        let _send = tx.send(crate::event::RadioCommand::SetOperatingMode {
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
                // These fields are stored in the ME channel record. No
                // hardware-qualified ME writer is exposed; permanent memory
                // storage would instead require a verified MCP editor.
                self.status_message = Some(format!(
                    "Ch {ch_num}: {} editing not yet implemented; requires ME write",
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

                let config = match kenwood_thd75::AprsClientConfig::new(callsign.address().clone())
                {
                    Ok(config) => Box::new(config),
                    Err(error) => {
                        self.status_message = Some(format!(
                            "Cannot enter APRS mode with the configured station identity: {error}"
                        ));
                        return;
                    }
                };
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::EnterAprs { config });
                    self.status_message = Some("Entering APRS mode...".into());
                }
            }
            AprsMode::Active => {
                if let Some(ref tx) = self.cmd_tx {
                    let _send = tx.send(crate::event::RadioCommand::ExitAprs);
                    self.status_message = Some("Exiting APRS mode...".into());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use kenwood_thd75::types::{
        FrontPanelPfFunction, GpsSettings, LinkedVolumeLevel, StoredFrontPanelPfAssignment,
    };
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::{
        App, AprsMode, AprsStationCache, DstarMode, InputMode, MainView, McpState, MemoryImage,
        Message, Pane, RadioState, SettingRow, cat_settings, next_linked_volume, next_pf_key,
        parse_reflector_input,
    };
    use crate::event::RadioCommand;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

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

    // ── Reducer: `update(Message) -> bool` ────────────────────────
    //
    // `App::update` is a pure state machine whose only effect channel
    // is `cmd_tx`, an unbounded tokio channel that works without a
    // runtime, so these need no async harness. Each test below pins a
    // regression this file has actually shipped.

    /// A failed MCP write must NOT wipe the loaded image.
    ///
    /// The cache exists so the operator can keep working (and retry the
    /// write) after a transient MCP failure; resetting to `Idle` on
    /// error threw away a 55-second full-memory read.
    #[test]
    fn mcp_error_preserves_a_loaded_image() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        let raw = vec![0u8; 500_480];
        assert!(app.update(Message::McpReadComplete(raw)));
        assert!(
            matches!(app.mcp, McpState::Loaded { .. }),
            "precondition: the image is loaded"
        );

        assert!(app.update(Message::McpError("write failed".into())));
        assert!(
            matches!(app.mcp, McpState::Loaded { .. }),
            "a failed MCP write must not destroy the cached image"
        );

        let image = MemoryImage::from_raw(vec![0x5A; 500_480])?;
        app.mcp = McpState::Writing {
            page: 12,
            total: 100,
            image,
        };
        assert!(app.update(Message::McpError("in-flight write failed".into())));
        assert!(
            matches!(app.mcp, McpState::Loaded { modified: true, .. }),
            "a real in-flight write failure must restore the retained image"
        );

        // With no image loaded, an error DOES return to Idle.
        let mut fresh = App::with_cache_path(String::new(), None);
        assert!(fresh.update(Message::McpError("read failed".into())));
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
        let image = MemoryImage::from_raw(vec![0xA5; 500_480])?;
        app.mcp = McpState::Writing {
            page: 100,
            total: 100,
            image,
        };

        assert!(app.update(Message::McpWriteComplete));
        let McpState::Loaded {
            ref image,
            modified,
        } = app.mcp
        else {
            return Err("completed write must restore the loaded image".into());
        };
        assert!(!modified, "completed write must clear the modified flag");
        assert_eq!(image.as_raw().first().copied(), Some(0xA5));
        assert!(app.connected, "successful MCP recovery must stay connected");
        assert_eq!(
            app.status_message.as_deref(),
            Some("MCP write complete; radio control restored")
        );
        Ok(())
    }

    /// `McpByteWritten` patches the cached image in place, so the TUI
    /// stays in sync without a full re-read after the radio's internal reset.
    #[test]
    fn mcp_byte_written_patches_the_cached_image() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        assert!(app.update(Message::McpReadComplete(vec![0u8; 500_480])));

        assert!(app.update(Message::McpByteWritten {
            offset: 0x1000,
            value: 0x42,
        }));

        let McpState::Loaded { ref image, .. } = app.mcp else {
            return Err("image must still be loaded".into());
        };
        assert_eq!(
            image.as_raw().get(0x1000).copied(),
            Some(0x42),
            "the written byte must be patched into the cached image"
        );
        Ok(())
    }

    /// A poll that carries empty static fields must not erase what the
    /// radio told us once at connect. Firmware version and radio type
    /// are read a single time; the periodic poll leaves them blank, so
    /// a naive `self.state = state` blanks the header on every tick.
    #[test]
    fn radio_update_preserves_connect_only_fields() {
        let mut app = App::with_cache_path(String::new(), None);
        let initial = RadioState {
            firmware_version: "1.03".to_owned(),
            radio_type: "TH-D75A".to_owned(),
            dstar_urcall: "CQCQCQ".to_owned(),
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
        assert_eq!(
            app.state.dstar_urcall, "CQCQCQ",
            "D-STAR URCALL must survive a poll that omits it"
        );
        assert!(app.connected, "a RadioUpdate marks the link connected");
    }

    /// A poll that DOES carry a value must overwrite the old one:
    /// the preservation above must not become a write-once latch.
    #[test]
    fn radio_update_overwrites_fields_the_poll_provides() {
        let mut app = App::with_cache_path(String::new(), None);
        let initial = RadioState {
            dstar_urcall: "CQCQCQ".to_owned(),
            ..RadioState::default()
        };
        assert!(app.update(Message::RadioUpdate(initial)));

        let updated = RadioState {
            dstar_urcall: "W1AW".to_owned(),
            ..RadioState::default()
        };
        assert!(app.update(Message::RadioUpdate(updated)));

        assert_eq!(
            app.state.dstar_urcall, "W1AW",
            "a poll that carries a URCALL must replace the cached one"
        );
    }

    fn dstar_gateway_app() -> (App, UnboundedReceiver<RadioCommand>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        (app, rx)
    }

    #[test]
    fn dstar_gateway_uses_the_validated_mcp_callsign() -> TestResult {
        let (mut app, mut rx) = dstar_gateway_app();
        let mut raw = vec![0u8; 500_480];
        raw.get_mut(0x1CA8..0x1CB0)
            .ok_or("MY-callsign field missing from test image")?
            .copy_from_slice(b"KQ4NIT  ");
        assert!(app.update(Message::McpReadComplete(raw)));

        app.toggle_dstar_mode();
        match rx.try_recv()? {
            RadioCommand::EnterDstar { config } => {
                assert_eq!(config.callsign.as_str(), "KQ4NIT");
            }
            other => return Err(format!("expected EnterDstar, got {other:?}").into()),
        }
        assert_eq!(
            app.status_message.as_deref(),
            Some("Entering D-STAR gateway mode...")
        );
        Ok(())
    }

    #[test]
    fn dstar_gateway_refuses_to_fabricate_an_unconfigured_callsign() {
        let (mut app, mut rx) = dstar_gateway_app();
        assert!(app.update(Message::McpReadComplete(vec![0u8; 500_480])));

        app.toggle_dstar_mode();
        assert!(
            rx.try_recv().is_err(),
            "an unconfigured MY callsign must not emit EnterDstar"
        );
        assert_eq!(
            app.status_message.as_deref(),
            Some("D-STAR MY callsign is not configured; set Menu 610 before entering gateway mode")
        );
    }

    #[test]
    fn dstar_gateway_surfaces_invalid_or_unavailable_mcp_data() -> TestResult {
        let (mut unavailable, mut unavailable_rx) = dstar_gateway_app();
        unavailable.toggle_dstar_mode();
        assert!(unavailable_rx.try_recv().is_err());
        assert_eq!(
            unavailable.status_message.as_deref(),
            Some("D-STAR MY callsign unavailable; load MCP data before entering gateway mode")
        );

        let (mut invalid, mut invalid_rx) = dstar_gateway_app();
        let mut raw = vec![0u8; 500_480];
        *raw.get_mut(0x1CA1)
            .ok_or("MY-callsign selector missing from test image")? = 6;
        assert!(invalid.update(Message::McpReadComplete(raw)));
        invalid.toggle_dstar_mode();
        assert!(invalid_rx.try_recv().is_err());
        let status = invalid
            .status_message
            .as_deref()
            .ok_or("invalid MCP data must set a status message")?;
        assert!(
            status.starts_with("D-STAR MY callsign data is invalid:"),
            "unexpected status: {status}"
        );
        assert!(
            status.contains("invalid value 6"),
            "unexpected status: {status}"
        );
        Ok(())
    }

    /// `McpProgress` must not mistake a write for a read: while a write
    /// is in flight, progress ticks stay `Writing` (the UI otherwise
    /// reports "Reading page N/M" during a write).
    #[test]
    fn mcp_progress_does_not_downgrade_a_write_to_a_read() -> TestResult {
        let mut app = App::with_cache_path(String::new(), None);
        app.mcp = McpState::Writing {
            page: 0,
            total: 100,
            image: MemoryImage::from_raw(vec![0; 500_480])?,
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

        // From Idle, progress means a read is underway.
        let mut reader = App::with_cache_path(String::new(), None);
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

        app.connected = true;
        assert!(app.update(Message::DstarStarted));
        assert!(app.update(Message::DstarRecoveryFailed(
            "transport reopen failed".into()
        )));
        assert_eq!(app.dstar_mode, DstarMode::Inactive);
        assert!(!app.connected);
        let dstar_status = app.status_message.as_deref().unwrap_or_default();
        assert!(dstar_status.contains("without usable CAT control"));
        assert!(!dstar_status.contains("polling resumed"));
    }

    #[test]
    fn parse_single_token_splits_trailing_module() -> TestResult {
        let (name, module) = parse_reflector_input("REF030C").ok_or("expected REF030C to parse")?;
        assert_eq!(name, "REF030", "reflector name");
        assert_eq!(module, 'C', "reflector module");
        Ok(())
    }

    #[test]
    fn parse_two_token_form() -> TestResult {
        let (name, module) =
            parse_reflector_input("REF030 C").ok_or("expected `REF030 C` to parse")?;
        assert_eq!(name, "REF030", "reflector name");
        assert_eq!(module, 'C', "reflector module");
        Ok(())
    }

    #[test]
    fn parse_uppercases_name() -> TestResult {
        let (name, module) =
            parse_reflector_input("xlx757 B").ok_or("expected `xlx757 B` to parse")?;
        assert_eq!(name, "XLX757", "reflector name");
        assert_eq!(module, 'B', "reflector module");
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
        // "é" is 2 bytes: the pre-fix code byte-sliced `len - 1` and panicked
        // mid-char. The parser must reject these instead (single-token inputs
        // need >= 4 chars and an ASCII uppercase module letter).
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
    fn reflector_app() -> (App, UnboundedReceiver<RadioCommand>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        app.dstar_reflector_input = Some(String::new());
        (app, rx)
    }

    fn aprs_compose_app() -> (App, UnboundedReceiver<RadioCommand>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
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

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        app.main_view = MainView::SettingsCat;
        app.settings_cat_index = cat_settings()
            .iter()
            .position(|row| matches!(row, SettingRow::VoxDelay))
            .ok_or("VOX delay row missing from the CAT settings table")?;
        app.state.vox_delay = VoxDelay::MS_3000;

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

    fn app_with_gps_settings(settings: GpsSettings) -> (App, UnboundedReceiver<RadioCommand>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        app.state.gps_settings = Some(settings);
        (app, rx)
    }

    fn assert_gps_settings(
        rx: &mut UnboundedReceiver<RadioCommand>,
        expected: GpsSettings,
    ) -> TestResult {
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
        let (mut gps_app, mut gps_rx) = app_with_gps_settings(GpsSettings::new(false, true));
        gps_app.toggle_gps();
        assert_gps_settings(&mut gps_rx, GpsSettings::new(true, true))?;

        let (mut output_app, mut output_rx) = app_with_gps_settings(GpsSettings::new(true, false));
        output_app.toggle_gps_pc_output();
        assert_gps_settings(&mut output_rx, GpsSettings::new(true, true))
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

        let (mut gps_app, mut gps_rx) = app_with_gps_settings(GpsSettings::new(false, true));
        gps_app.main_view = MainView::SettingsCat;
        gps_app.settings_cat_index = gps_enabled_index;
        gps_app.toggle_setting();
        assert_gps_settings(&mut gps_rx, GpsSettings::new(true, true))?;

        let (mut output_app, mut output_rx) = app_with_gps_settings(GpsSettings::new(true, false));
        output_app.main_view = MainView::SettingsCat;
        output_app.settings_cat_index = gps_output_index;
        output_app.toggle_setting();
        assert_gps_settings(&mut output_rx, GpsSettings::new(true, true))
    }

    #[test]
    fn gps_toggles_refuse_when_current_settings_are_unavailable() -> TestResult {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);

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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);

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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);

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
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::with_cache_path(String::new(), None);
        app.cmd_tx = Some(tx);
        app.state.aprs_callsign = Some(kenwood_thd75::types::AprsCallsign::new("KQ4NIT-9")?);

        app.toggle_aprs_mode();

        match rx.try_recv()? {
            RadioCommand::EnterAprs { config } => {
                let source = config.source().ok_or("expected a station identity")?;
                assert_eq!(source.to_string(), "KQ4NIT-9");
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
        rx: &mut UnboundedReceiver<RadioCommand>,
        expected_name: &str,
        expected_module: char,
    ) -> TestResult {
        let cmd = rx.try_recv()?;
        match cmd {
            RadioCommand::PrepareReflectorLink { name, module } => {
                assert_eq!(name, expected_name, "reflector name");
                assert_eq!(module, expected_module, "reflector module");
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
