use std::path::PathBuf;
use std::time::SystemTime;

use kenwood_thd75::memory::MemoryImage;
use kenwood_thd75::types::{Frequency, Mode, PowerLevel};

/// Path to the MCP cache file.
fn cache_path() -> PathBuf {
    let mut p = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("."));
    p.push("thd75-tui");
    p.push("mcp.bin");
    p
}

/// Save raw MCP image to disk cache.
///
/// Logs errors but does not propagate — a failed cache write should not
/// block radio operation. The user will see a warning in the log.
pub fn save_cache(data: &[u8]) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::error!(path = %parent.display(), "failed to create cache dir: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, data) {
        tracing::error!(path = %path.display(), "failed to write MCP cache: {e}");
    }
}

/// Load cached MCP image from disk. Returns (image, age).
pub fn load_cache() -> Option<(MemoryImage, std::time::Duration)> {
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
pub const SETTINGS_COUNT: usize = 85;

/// Settings row identifiers for the interactive settings list.
///
/// Organized by the radio's menu groups. Settings backed by CAT commands are
/// noted; all others modify the in-memory MCP image and require an MCP write
/// to take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingRow {
    // --- RX ---
    /// Squelch level Band A (CAT: SQ band,level).
    SquelchA,
    /// Squelch level Band B (CAT: SQ band,level).
    SquelchB,
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
    /// Lock (CAT: LC, inverted).
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
    /// Dual band (CAT: DL, inverted).
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
}

impl SettingRow {
    /// All settings rows in display order.
    pub const ALL: [Self; SETTINGS_COUNT] = [
        // RX
        Self::SquelchA,
        Self::SquelchB,
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
    ];

    /// Human-readable label for the setting.
    pub const fn label(self) -> &'static str {
        match self {
            Self::SquelchA => "Squelch A",
            Self::SquelchB => "Squelch B",
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
        }
    }

    /// Section header label shown above this row. `None` means same group as previous row.
    pub const fn section_header(self) -> Option<&'static str> {
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
    pub const fn is_numeric(self) -> bool {
        matches!(
            self,
            Self::SquelchA
                | Self::SquelchB
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
    pub const fn is_cat(self) -> bool {
        matches!(
            self,
            Self::SquelchA
                | Self::SquelchB
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
        )
    }
}

/// Settings that use instant CAT writes (no disconnect).
pub fn cat_settings() -> Vec<SettingRow> {
    SettingRow::ALL
        .iter()
        .copied()
        .filter(|r| r.is_cat())
        .collect()
}

/// Settings that require MCP page write (~3s, brief disconnect).
pub fn mcp_settings() -> Vec<SettingRow> {
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
pub enum Pane {
    BandA,
    BandB,
    Main,
    Detail,
}

impl Pane {
    pub const fn next(self) -> Self {
        match self {
            Self::BandA => Self::BandB,
            Self::BandB => Self::Main,
            Self::Main => Self::Detail,
            Self::Detail => Self::BandA,
        }
    }

    pub const fn prev(self) -> Self {
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
pub enum MainView {
    Channels,
    /// CAT settings — instant, no disconnect.
    SettingsCat,
    /// MCP settings — ~3s per change, brief disconnect.
    SettingsMcp,
    Aprs,
    Mcp,
}

/// Input mode for the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    /// Searching channels — buffer holds the search string.
    Search(String),
    /// Entering a frequency — buffer holds digits typed so far.
    FreqInput(String),
}

/// Live state for one band, updated by the radio poller.
#[derive(Debug, Clone)]
pub struct BandState {
    pub frequency: Frequency,
    pub mode: Mode,
    /// S-meter level (0–5). Driven by AI-pushed BY notifications, not polled.
    pub s_meter: u8,
    /// Squelch setting (0–6 on D75).
    pub squelch: u8,
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
            s_meter: 0,
            squelch: 0,
            power_level: PowerLevel::High,
            busy: false,
            attenuator: false,
            step_size: None,
        }
    }
}

/// Aggregated radio state from the poller.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct RadioState {
    pub band_a: BandState,
    pub band_b: BandState,
    /// Battery charge level (0–4): 0=Empty, 1=1/3, 2=2/3, 3=Full, 4=Charging.
    pub battery_level: u8,
    pub beep: bool,
    pub lock: bool,
    pub dual_band: bool,
    pub bluetooth: bool,
    pub vox: bool,
    /// VOX gain (0–9).
    pub vox_gain: u8,
    /// VOX delay (0–30).
    pub vox_delay: u8,
    /// Audio gain (0–99, 3-digit zero-padded on wire).
    pub af_gain: u8,
    pub firmware_version: String,
    pub radio_type: String,
    pub gps_enabled: bool,
    pub beacon_type: u8,
}

/// MCP programming state machine.
#[derive(Debug)]
pub enum McpState {
    Idle,
    Reading { page: u16, total: u16 },
    Loaded { image: MemoryImage, modified: bool },
    Writing { page: u16, total: u16 },
    Reconnecting,
}

/// All events that can flow into the update loop.
#[derive(Debug)]
pub enum Message {
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
    Quit,
}

/// Central application state.
#[allow(clippy::struct_excessive_bools)]
pub struct App {
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
}

impl App {
    /// Returns the list of used channel numbers, filtered by `search_filter`.
    pub fn filtered_channels(&self) -> Vec<u16> {
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
    pub fn new(port_path: String) -> Self {
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
        }
    }

    /// Process a message and update state. Returns true if a render is needed.
    pub fn update(&mut self, msg: Message) -> bool {
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
                    buf.pop();
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
                    buf.pop();
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
            KeyCode::Char('a') => {
                self.main_view = MainView::Aprs;
                self.focus = Pane::Main;
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
            // Jump to first/last channel
            KeyCode::Char('g') if self.focus == Pane::Main => {
                self.channel_list_index = 0;
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
                        _ => {}
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
                        _ => {}
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
                if let Some(&ch_num) = used.get(self.channel_list_index) {
                    if let Some(ref tx) = self.cmd_tx {
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
                    (kenwood_thd75::types::Band::A, self.state.band_a.squelch)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.squelch)
                };
                let next = cur.saturating_sub(1);
                if let (Some(tx), Ok(level)) =
                    (&self.cmd_tx, kenwood_thd75::types::SquelchLevel::new(next))
                {
                    let _ = tx.send(crate::event::RadioCommand::SetSquelch { band, level });
                    self.status_message = Some(format!("Squelch → {next}"));
                }
                true
            }
            KeyCode::Char(']') if matches!(self.focus, Pane::BandA | Pane::BandB) => {
                let (band, cur) = if self.focus == Pane::BandA {
                    (kenwood_thd75::types::Band::A, self.state.band_a.squelch)
                } else {
                    (kenwood_thd75::types::Band::B, self.state.band_b.squelch)
                };
                let next = cur.saturating_add(1).min(6);
                if let (Some(tx), Ok(level)) =
                    (&self.cmd_tx, kenwood_thd75::types::SquelchLevel::new(next))
                {
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
            KeyCode::Char('r') if self.main_view == MainView::Mcp => {
                if matches!(self.mcp, McpState::Idle | McpState::Loaded { .. }) {
                    self.mcp = McpState::Reading {
                        page: 0,
                        total: 1955,
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
                        total: 1955,
                    };
                    self.status_message = Some("Starting MCP write...".into());
                    if let Some(ref tx) = self.cmd_tx {
                        let _ = tx.send(crate::event::RadioCommand::WriteMemory(data));
                    }
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
                    // LC inverted on D75: raw true = unlocked
                    let displayed = !self.state.lock;
                    let _ = tx.send(crate::event::RadioCommand::SetLock(displayed));
                    self.status_message = Some(format!("Lock → {}", on_off(!displayed)));
                    return;
                }
                SettingRow::DualBand => {
                    // DL inverted on D75: raw false = dual band on
                    let displayed = !self.state.dual_band;
                    let _ = tx.send(crate::event::RadioCommand::SetDualBand(displayed));
                    self.status_message = Some(format!("Dual band → {}", on_off(!displayed)));
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
                    self.status_message = Some("FM Radio toggled".into());
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

        // Get current value from MCP image, compute new value and offset
        let (offset, new_val, label): (u16, u8, String) =
            if let McpState::Loaded { ref image, .. } = self.mcp {
                let s = image.settings();
                match row {
                    SettingRow::TxInhibit => (
                        0x1019,
                        u8::from(!s.tx_inhibit()),
                        format!("TX Inhibit → {}", on_off(!s.tx_inhibit())),
                    ),
                    SettingRow::BeatShift => (
                        0x101A,
                        u8::from(!s.beat_shift()),
                        format!("Beat Shift → {}", on_off(!s.beat_shift())),
                    ),
                    SettingRow::VoxTxOnBusy => (
                        0x101E,
                        u8::from(!s.vox_tx_on_busy()),
                        format!("VOX TX Busy → {}", on_off(!s.vox_tx_on_busy())),
                    ),
                    SettingRow::CwBreakIn => (
                        0x101F,
                        u8::from(!s.cw_break_in()),
                        format!("CW Break-In → {}", on_off(!s.cw_break_in())),
                    ),
                    SettingRow::DtmfTxHold => (
                        0x1027,
                        u8::from(!s.dtmf_tx_hold()),
                        format!("DTMF TX Hold → {}", on_off(!s.dtmf_tx_hold())),
                    ),
                    SettingRow::RepeaterAutoOffset => (
                        0x1030,
                        u8::from(!s.repeater_auto_offset()),
                        format!("Rpt Auto Offset → {}", on_off(!s.repeater_auto_offset())),
                    ),
                    SettingRow::LockKeyA => (
                        0x1062,
                        u8::from(!s.lock_key_a()),
                        format!("Lock Key A → {}", on_off(!s.lock_key_a())),
                    ),
                    SettingRow::LockKeyB => (
                        0x1063,
                        u8::from(!s.lock_key_b()),
                        format!("Lock Key B → {}", on_off(!s.lock_key_b())),
                    ),
                    SettingRow::LockKeyC => (
                        0x1064,
                        u8::from(!s.lock_key_c()),
                        format!("Lock Key C → {}", on_off(!s.lock_key_c())),
                    ),
                    SettingRow::LockPtt => (
                        0x1065,
                        u8::from(!s.lock_key_ptt()),
                        format!("Lock PTT → {}", on_off(!s.lock_key_ptt())),
                    ),
                    SettingRow::AprsLock => (
                        0x1097,
                        u8::from(!s.aprs_lock()),
                        format!("APRS Lock → {}", on_off(!s.aprs_lock())),
                    ),
                    SettingRow::Announce => (
                        0x1070,
                        u8::from(!s.announce()),
                        format!("Announce → {}", on_off(!s.announce())),
                    ),
                    SettingRow::KeyBeep => (
                        0x1071,
                        u8::from(!s.key_beep()),
                        format!("Key Beep → {}", on_off(!s.key_beep())),
                    ),
                    SettingRow::VolumeLock => (
                        0x1076,
                        u8::from(!s.volume_lock()),
                        format!("Vol Lock → {}", on_off(!s.volume_lock())),
                    ),
                    SettingRow::BtAutoConnect => (
                        0x1079,
                        u8::from(!s.bt_auto_connect()),
                        format!("BT Auto Connect → {}", on_off(!s.bt_auto_connect())),
                    ),
                    SettingRow::UsbAudioOutput => (
                        0x1094,
                        u8::from(!s.usb_audio_output()),
                        format!("USB Audio Out → {}", on_off(!s.usb_audio_output())),
                    ),
                    SettingRow::InternetLink => (
                        0x1095,
                        u8::from(!s.internet_link()),
                        format!("Internet Link → {}", on_off(!s.internet_link())),
                    ),
                    SettingRow::PowerOnMessageFlag => (
                        0x1087,
                        u8::from(!s.power_on_message_flag()),
                        format!("PowerOn Msg → {}", on_off(!s.power_on_message_flag())),
                    ),
                    SettingRow::BatterySaver => (
                        0x10C0,
                        u8::from(!s.battery_saver()),
                        format!("Battery Saver → {}", on_off(!s.battery_saver())),
                    ),
                    _ => {
                        self.status_message = Some(format!("{}: use +/- to adjust", row.label()));
                        return;
                    }
                }
            } else {
                self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
                return;
            };

        // Send single-page MCP write — radio will disconnect briefly and reconnect
        let _ = tx.send(crate::event::RadioCommand::McpWriteByte {
            offset,
            value: new_val,
        });
        self.status_message = Some(format!("{label} — applying..."));
    }

    /// Adjust a numeric setting by delta with +/-.
    #[allow(clippy::too_many_lines)]
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
                    let cur = self.state.band_a.squelch;
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = kenwood_thd75::types::SquelchLevel::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::A,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch A → {next}"));
                    return;
                }
                SettingRow::SquelchB => {
                    let cur = self.state.band_b.squelch;
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(6)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(level) = kenwood_thd75::types::SquelchLevel::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetSquelch {
                            band: kenwood_thd75::types::Band::B,
                            level,
                        });
                    }
                    self.status_message = Some(format!("Squelch B → {next}"));
                    return;
                }
                SettingRow::VoxGain => {
                    let cur = self.state.vox_gain;
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(9)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(gain) = kenwood_thd75::types::VoxGain::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetVoxGain(gain));
                    }
                    self.status_message = Some(format!("VOX Gain → {next}"));
                    return;
                }
                SettingRow::VoxDelay => {
                    let cur = self.state.vox_delay;
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(30)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(delay) = kenwood_thd75::types::VoxDelay::new(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetVoxDelay(delay));
                    }
                    self.status_message = Some(format!("VOX Delay → {next}"));
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
                        Mode::Dr => Mode::Fm,
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
                        Mode::Dr => Mode::Fm,
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
                    let _ = tx.send(crate::event::RadioCommand::SetTncBaud(
                        kenwood_thd75::types::TncBaud::Bps1200,
                    ));
                    self.status_message =
                        Some("TNC Baud: not yet polled — send 1200 as placeholder".into());
                    return;
                }
                SettingRow::BeaconType => {
                    let cur = self.state.beacon_type;
                    let next = if delta > 0 {
                        cur.saturating_add(1).min(4)
                    } else {
                        cur.saturating_sub(1)
                    };
                    if let Ok(mode) = kenwood_thd75::types::BeaconMode::try_from(next) {
                        let _ = tx.send(crate::event::RadioCommand::SetBeaconType(mode));
                    }
                    self.status_message = Some(format!("Beacon Type → {next}"));
                    return;
                }
                SettingRow::CallsignSlot => {
                    if let Ok(slot) = kenwood_thd75::types::CallsignSlot::new(0) {
                        let _ = tx.send(crate::event::RadioCommand::SetCallsignSlot(slot));
                    }
                    self.status_message = Some("Callsign Slot: not yet polled".into());
                    return;
                }
                SettingRow::DstarSlot => {
                    if let Ok(slot) = kenwood_thd75::types::DstarSlot::new(1) {
                        let _ = tx.send(crate::event::RadioCommand::SetDstarSlot(slot));
                    }
                    self.status_message = Some("D-STAR Slot: not yet polled".into());
                    return;
                }
                _ => {}
            }
        }

        // MCP-backed numeric settings — write directly via single-page MCP
        let Some(tx) = self.cmd_tx.clone() else {
            return;
        };

        let (offset, new_val, label): (u16, u8, String) =
            if let McpState::Loaded { ref image, .. } = self.mcp {
                let s = image.settings();
                match row {
                    SettingRow::FmNarrow => (
                        0x100F,
                        s.fm_narrow().saturating_add_signed(delta),
                        format!("FM Narrow → {}", s.fm_narrow().saturating_add_signed(delta)),
                    ),
                    SettingRow::SsbHighCut => (
                        0x1011,
                        s.ssb_high_cut().saturating_add_signed(delta),
                        format!(
                            "SSB High Cut → {}",
                            s.ssb_high_cut().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::CwHighCut => (
                        0x1012,
                        s.cw_high_cut().saturating_add_signed(delta),
                        format!(
                            "CW High Cut → {}",
                            s.cw_high_cut().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::AmHighCut => (
                        0x1013,
                        s.am_high_cut().saturating_add_signed(delta),
                        format!(
                            "AM High Cut → {}",
                            s.am_high_cut().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::AutoFilter => (
                        0x100C,
                        s.auto_filter().saturating_add_signed(delta),
                        format!(
                            "Auto Filter → {}",
                            s.auto_filter().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::ScanResume => (
                        0x1007,
                        s.scan_resume().saturating_add_signed(delta),
                        format!(
                            "Scan Resume → {}",
                            s.scan_resume().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::DigitalScanResume => (
                        0x1008,
                        s.digital_scan_resume().saturating_add_signed(delta),
                        format!(
                            "Dig Scan Resume → {}",
                            s.digital_scan_resume().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::ScanRestartTime => (
                        0x1009,
                        s.scan_restart_time().saturating_add_signed(delta),
                        format!(
                            "Scan Restart Time → {}",
                            s.scan_restart_time().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::ScanRestartCarrier => (
                        0x100A,
                        s.scan_restart_carrier().saturating_add_signed(delta),
                        format!(
                            "Scan Restart Carrier → {}",
                            s.scan_restart_carrier().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::TimeoutTimer => (
                        0x1018,
                        s.timeout_timer().saturating_add_signed(delta),
                        format!(
                            "Timeout Timer → {}",
                            s.timeout_timer().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::CwDelayTime => (
                        0x1020,
                        s.cw_delay_time().saturating_add_signed(delta),
                        format!(
                            "CW Delay → {}",
                            s.cw_delay_time().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::CwPitch => (
                        0x1021,
                        s.cw_pitch().saturating_add_signed(delta),
                        format!("CW Pitch → {}", s.cw_pitch().saturating_add_signed(delta)),
                    ),
                    SettingRow::DtmfSpeed => (
                        0x1024,
                        s.dtmf_speed().saturating_add_signed(delta),
                        format!(
                            "DTMF Speed → {}",
                            s.dtmf_speed().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::DtmfPauseTime => (
                        0x1026,
                        s.dtmf_pause_time().saturating_add_signed(delta),
                        format!(
                            "DTMF Pause → {}",
                            s.dtmf_pause_time().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::RepeaterCallKey => (
                        0x1031,
                        s.repeater_call_key().saturating_add_signed(delta),
                        format!(
                            "Call Key → {}",
                            s.repeater_call_key().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::MicSensitivity => (
                        0x1040,
                        s.mic_sensitivity().saturating_add_signed(delta),
                        format!(
                            "Mic Sens → {}",
                            s.mic_sensitivity().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::PfKey1 => (
                        0x1041,
                        s.pf_key1().saturating_add_signed(delta),
                        format!("PF Key 1 → {}", s.pf_key1().saturating_add_signed(delta)),
                    ),
                    SettingRow::PfKey2 => (
                        0x1042,
                        s.pf_key2().saturating_add_signed(delta),
                        format!("PF Key 2 → {}", s.pf_key2().saturating_add_signed(delta)),
                    ),
                    SettingRow::KeyLockType => (
                        0x1061,
                        s.key_lock_type_raw().saturating_add_signed(delta).min(2),
                        format!(
                            "Lock Type → {}",
                            s.key_lock_type_raw().saturating_add_signed(delta).min(2)
                        ),
                    ),
                    SettingRow::DualDisplaySize => (
                        0x1066,
                        s.dual_display_size().saturating_add_signed(delta),
                        format!(
                            "Dual Display → {}",
                            s.dual_display_size().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::DisplayArea => (
                        0x1067,
                        s.display_area().saturating_add_signed(delta),
                        format!(
                            "Display Area → {}",
                            s.display_area().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::InfoLine => (
                        0x1068,
                        s.info_line().saturating_add_signed(delta),
                        format!("Info Line → {}", s.info_line().saturating_add_signed(delta)),
                    ),
                    SettingRow::BacklightControl => (
                        0x1069,
                        s.backlight_control().saturating_add_signed(delta),
                        format!(
                            "Backlight Ctrl → {}",
                            s.backlight_control().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::BacklightTimer => (
                        0x106A,
                        s.backlight_timer().saturating_add_signed(delta),
                        format!(
                            "Backlight Timer → {}",
                            s.backlight_timer().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::DisplayHoldTime => (
                        0x106B,
                        s.display_hold_time().saturating_add_signed(delta),
                        format!(
                            "Display Hold → {}",
                            s.display_hold_time().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::DisplayMethod => (
                        0x106C,
                        s.display_method().saturating_add_signed(delta),
                        format!(
                            "Display Method → {}",
                            s.display_method().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::PowerOnDisplay => (
                        0x106D,
                        s.power_on_display().saturating_add_signed(delta),
                        format!(
                            "PowerOn Display → {}",
                            s.power_on_display().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::EmrVolumeLevel => (
                        0x106E,
                        s.emr_volume_level().saturating_add_signed(delta),
                        format!(
                            "EMR Vol → {}",
                            s.emr_volume_level().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::AutoMuteReturnTime => (
                        0x106F,
                        s.auto_mute_return_time().saturating_add_signed(delta),
                        format!(
                            "Auto Mute → {}",
                            s.auto_mute_return_time().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::BeepVolume => {
                        let cur = s.beep_volume();
                        let v = if delta > 0 {
                            cur.saturating_add(1).min(7)
                        } else {
                            cur.saturating_sub(1).max(1)
                        };
                        (0x1072, v, format!("Beep Vol → {v}"))
                    }
                    SettingRow::VoiceLanguage => (
                        0x1073,
                        s.voice_language().saturating_add_signed(delta),
                        format!(
                            "Voice Lang → {}",
                            s.voice_language().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::VoiceVolume => (
                        0x1074,
                        s.voice_volume().saturating_add_signed(delta),
                        format!(
                            "Voice Vol → {}",
                            s.voice_volume().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::VoiceSpeed => (
                        0x1075,
                        s.voice_speed().saturating_add_signed(delta),
                        format!(
                            "Voice Speed → {}",
                            s.voice_speed().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::SpeedDistanceUnit => {
                        let v = if delta > 0 {
                            s.speed_distance_unit_raw().saturating_add(1).min(2)
                        } else {
                            s.speed_distance_unit_raw().saturating_sub(1)
                        };
                        (
                            0x1077,
                            v,
                            format!(
                                "Speed Unit → {}",
                                ["mph", "km/h", "knots"].get(v as usize).unwrap_or(&"?")
                            ),
                        )
                    }
                    SettingRow::AltitudeRainUnit => {
                        let v = if delta > 0 {
                            s.altitude_rain_unit_raw().saturating_add(1).min(1)
                        } else {
                            s.altitude_rain_unit_raw().saturating_sub(1)
                        };
                        (
                            0x1083,
                            v,
                            format!("Alt Unit → {}", if v == 0 { "ft/in" } else { "m/mm" }),
                        )
                    }
                    SettingRow::TemperatureUnit => {
                        let v = if delta > 0 {
                            s.temperature_unit_raw().saturating_add(1).min(1)
                        } else {
                            s.temperature_unit_raw().saturating_sub(1)
                        };
                        (
                            0x1084,
                            v,
                            format!("Temp Unit → {}", if v == 0 { "°F" } else { "°C" }),
                        )
                    }
                    SettingRow::GpsBtInterface => (
                        0x1080,
                        s.gps_bt_interface().saturating_add_signed(delta),
                        format!(
                            "GPS/BT → {}",
                            s.gps_bt_interface().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::PcOutputMode => (
                        0x1085,
                        s.pc_output_mode().saturating_add_signed(delta),
                        format!(
                            "PC Output → {}",
                            s.pc_output_mode().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::AprsUsbMode => (
                        0x1086,
                        s.aprs_usb_mode().saturating_add_signed(delta),
                        format!(
                            "APRS USB → {}",
                            s.aprs_usb_mode().saturating_add_signed(delta)
                        ),
                    ),
                    SettingRow::AutoPowerOff => {
                        let v = if delta > 0 {
                            s.auto_power_off_raw().saturating_add(1).min(4)
                        } else {
                            s.auto_power_off_raw().saturating_sub(1)
                        };
                        (
                            0x10D0,
                            v,
                            format!(
                                "Auto PwrOff → {}",
                                ["Off", "30m", "60m", "90m", "120m"]
                                    .get(v as usize)
                                    .unwrap_or(&"?")
                            ),
                        )
                    }
                    _ => {
                        self.status_message = Some(format!("{}: not adjustable", row.label()));
                        return;
                    }
                }
            } else {
                self.status_message = Some(format!("{}: load MCP data first (m → r)", row.label()));
                return;
            };

        let _ = tx.send(crate::event::RadioCommand::McpWriteByte {
            offset,
            value: new_val,
        });
        self.status_message = Some(format!("{label} — applying..."));
    }
}
