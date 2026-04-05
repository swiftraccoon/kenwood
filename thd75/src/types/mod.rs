//! Strong types with validation-on-construction for all TH-D75 radio parameters.
//!
//! Every type enforces firmware validation rules from the
//! `radio_validate_channel_params` function at address `0xC003C694` in
//! firmware v1.03.000. Invalid values are rejected at construction time,
//! making illegal radio states unrepresentable.

pub mod aprs;
pub mod band;
pub mod bluetooth;
pub mod channel;
pub mod cw;
pub mod dstar;
pub mod dtmf;
pub mod echolink;
pub mod fm;
pub mod frequency;
pub mod gps;
pub mod mode;
pub mod radio_params;
pub mod repeater;
pub mod scan;
pub mod settings;
pub mod system;
pub mod tone;
pub mod voice;
pub mod weather;
pub mod wireless;

pub use band::Band;
pub use channel::{
    ChannelMemory, ChannelName, CrossToneType, FineStep, FlashChannel, FlashDigitalSquelch,
    FlashDuplex,
};
pub use frequency::Frequency;
pub use mode::{MemoryMode, Mode, PowerLevel, ShiftDirection, StepSize};
pub use radio_params::{AfGainLevel, FilterMode, SMeterReading, SquelchLevel, VfoMemoryMode};
pub use tone::{
    CTCSS_FREQUENCIES, CtcssMode, DCS_CODES, DataSpeed, DcsCode, LockoutMode, ToneCode, ToneMode,
};

// APRS subsystem re-exports.
pub use aprs::{
    AprsBand, AprsCallsign, AprsConfig, AprsDataSpeed, AprsIcon, AprsMessage, AprsNetwork,
    AprsStation, AutoReplyConfig, AutoReplyDelay, AutoReplyType, BeaconControl, BeaconMethod,
    DcdSense, DigipeatAlias, DigipeatConfig, DisplayArea, FilterPhrase, FloodAlias, GroupCode,
    InterruptTime, NavitraConfig, NavitraGroupMode, NavitraMessage, NotificationConfig,
    PacketFilter, PacketFilterType, PacketPath, PositionAmbiguity, PositionComment, QsyConfig,
    ReplyMessage, SmartBeaconingConfig, StatusText, TraceAlias, TxDelay, VoiceAlertConfig,
    WaypointConfig, WaypointFormat,
};

// D-STAR subsystem re-exports.
pub use dstar::{
    CallsignEntry, DestinationSelect, DigitalSquelch, DigitalSquelchCode, DigitalSquelchType,
    DstarAutoReply, DstarAutoReplyMode, DstarCallsign, DstarConfig, DstarMessage, DstarModule,
    DstarQsoEntry, DstarSuffix, DvDrMode, EmrVolume, GatewayMode, HotspotEntry, PositionAccuracy,
    QsoDirection, ReflectorCommand, RepeaterDuplex, RepeaterEntry, RouteSelect,
};

// GPS subsystem re-exports.
pub use gps::{
    CoordinateFormat, GpsConfig, GpsDataTx, GpsFix, GpsOperatingMode, GpsPosition,
    GpsPositionAmbiguity, GridSquareFormat, NmeaSentences, PositionMemory, PositionName,
    TargetPoint, TrackLogConfig, TrackRecordMethod,
};

// FM broadcast radio re-exports.
pub use fm::{
    FM_RADIO_CHANNEL_COUNT, FM_RADIO_MAX_HZ, FM_RADIO_MIN_HZ, FmRadioChannel, FmRadioMode,
};

// Scan re-exports.
pub use scan::{ProgramScanEdge, ScanResumeMethod, ScanType, VisualRange, VisualScanRange};

// Settings re-exports.
pub use settings::{
    AltitudeRainUnit, AudioSettings, AutoPowerOff, BackgroundColor, BacklightControl, DateFormat,
    DisplayHoldTime, DisplayMethod, DisplaySettings, DisplayUnits, EqSetting, KeyLockType,
    Language, LedControl, MeterType, MicSensitivity, PfKeyFunction, PowerOnMessage,
    SpeedDistanceUnit, SystemSettings, TemperatureUnit, VoiceGuideSpeed,
};

// CW (Morse code) re-exports.
pub use cw::{CwConfig, CwDelay, CwPitch};

// DTMF re-exports.
pub use dtmf::{
    DtmfConfig, DtmfDigits, DtmfMemory, DtmfName, DtmfPause, DtmfSlot, DtmfSpeed, is_valid_dtmf,
};

// EchoLink re-exports.
pub use echolink::{EchoLinkMemory, EchoLinkName, EchoLinkNode, EchoLinkSlot};

// FM repeater re-exports.
pub use repeater::{RepeaterConfig, ToneBurstHold};

// Voice message re-exports.
pub use voice::{RepeatInterval, VoiceChannel, VoiceMessage, VoiceMessageName};

// Weather alert re-exports (TH-D75A only).
pub use weather::WeatherConfig;

// Wireless remote control re-exports.
pub use wireless::{WirelessControlConfig, WirelessPassword};
