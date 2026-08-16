//! Validated value types for TH-D75 commands, memory records, and settings.
//!
//! Scalar types reject values outside their established wire or menu domains
//! at construction time. Composite record types use those scalars for known
//! fields and preserve unidentified storage bits when retained evidence does
//! not support a semantic interpretation. Broader operating constraints that
//! depend on radio state remain explicit fallible operations rather than
//! being hidden by lossy normalization.
//!
//! Retained values without a proven user-facing meaning use `Unidentified`
//! in their semantic name. When byte position is the only established fact,
//! the identifier states that exact byte and bit range instead (for example,
//! `ChannelByte0eBits3To2`). These forms preserve wire data without assigning
//! meaning that the available evidence does not establish.
//!
//! Menu numbers referenced throughout this module correspond to the
//! TH-D75 menu system as documented in User Manual Chapter 6 (Menu Mode).
//! The full menu table covers Menu No. 100-999 across categories:
//! TX/RX, Memory, Audio File, GPS, APRS, Digital (D-STAR), FM
//! Broadcasting, SD Card, and Configuration.

pub mod aprs;
pub mod band;
pub mod bluetooth;
pub mod channel;
pub mod channel_display_name;
pub mod cw;
pub mod dstar;
pub mod dtmf;
pub mod echolink;
pub mod fm;
pub mod frequency;
pub mod gps;
pub mod identity;
pub mod kiss;
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
    CatChannelRecord, CatMemoryChannelRecord, ChannelByte0eBits3To2, ChannelCodeUnidentifiedBits,
    ChannelTransmitValue, CrossToneField, CrossToneType, CurrentMemorySelector, FineStep,
    MemoryChannelAddress, MemoryChannelBand, MemoryGroup, StoredChannel, StoredChannelData,
    StoredChannelFlag, StoredChannelSlot,
};
pub use channel_display_name::{ChannelDisplayName, RegularChannel};
pub use dstar::DstarCallsignEntry;
pub use frequency::Frequency;
pub use identity::{
    FirmwareIdentity, HardwareVariant, ModelCode, RadioModel, RadioRegion, RadioType,
    SerialInformation, SerialNumber,
};
pub use kiss::{KissDuplex, KissParams, KissPersistence, KissSlotTime, KissTxDelay, KissTxTail};
pub use mode::{ChannelMode, OperatingMode, PowerLevel, ShiftDirection, StepSize};
pub use radio_params::{
    AfGainLevel, AntennaInput, BandMode, BatteryLevel, BeaconMode, DstarSlot, DvGatewayMode,
    FilterMode, FilterWidthIndex, GpsRadioMode, MEMORY_READ_WIRE_BOUND, MemoryReadOffset,
    MemoryReadTarget, MyPositionSelection, PacketDataRate, ReadLen, SMeterReading, SquelchLevel,
    TncControlMode, TncMode, TncState, TuningMode, UsbAudioOutput, VoxDelay, VoxGain,
};
pub use tone::{CTCSS_FREQUENCIES, CtcssCode, DCS_CODES, DcsCode, ToneCode, ToneMode};

// APRS subsystem re-exports.
pub use aprs::{
    AltnetAddress, AprsBand, AprsCallsign, AprsIcon, AprsLock, AprsNetwork, AprsNetworkType,
    AprsSettings, AprsStation, AprsSymbolCode, AprsSymbolTable, AutoReplyCallsignPrefix,
    AutoReplyDelay, AutoReplySettings, AutoReplyTarget, BeaconControl, BeaconInterval,
    BulletinGroupCode, BulletinGroupCodes, DcdSense, DigipeaterFunction, DisplayArea, FloodAlias,
    InterruptTime, MessageGroupCode, MessageGroupCodes, NavitraGroupCode, NavitraGroupMode,
    NavitraMessage, NavitraMessageSlot, NavitraSettings, NewNPacketPath, NotificationSettings,
    Others1PacketPath, Others2PacketPath, Others3PacketPath, PacketFilter, PacketFilterDistance,
    PacketFilterFlags, PacketFilterPositionLimit, PacketPath, PacketPathAbbreviation,
    PacketPathHops, PositionAmbiguity, PositionComment, QsyDistance, QsyLimitDistance, QsySettings,
    RegionPacketPath, RelayPacketPath, ReplyMessage, RxBeep, StatusTextSlot, StoredAprsMessage,
    StoredDigipeaterSettings, StoredFastRateSeconds, StoredHighSpeed, StoredLowSpeed,
    StoredSlowRateMinutes, StoredSmartBeaconingSettings, StoredStatusText, StoredTurnAngleDegrees,
    StoredTurnSlope, StoredTurnTimeSeconds, TraceAlias, TxDelay, UiCheckSeconds, UiDigiAlias,
    UiFloodSubstitution, UserPhrase, VoiceAlertMode, VoiceAlertSettings, WaypointFormat,
    WaypointNameLength, WaypointOutput, WaypointSettings,
};

// D-STAR subsystem re-exports.
pub use dstar::{
    CallsignEntry, CallsignEntryError, CallsignListMemo, CallsignListName, CallsignListTextError,
    DestinationSelect, DigitalSquelch, DigitalSquelchCode, DigitalSquelchType, DstarCallsign,
    DstarGpsAutoTxInterval, DstarGpsDataTxSentence, DstarGpsDataTxSentences,
    DstarGpsDataTxSettings, DstarMessage, DstarMessageError, DstarQsoEntry, DstarSuffix, DvDrMode,
    EmrVolume, Module, QsoDirection, ReflectorCallsign, ReflectorCommand, RouteSelect,
    UrCallAction,
};

// GPS subsystem re-exports.
pub use gps::{
    CoordinateFormat, GpsBatterySaver, GpsSettings, GridSquareFormat, MyPosition, NmeaSentence,
    NmeaSentences, PositionAltitudeMeters, PositionName, TrackDistanceHundredths,
    TrackIntervalSeconds, TrackLogSettings, TrackRecordMethod,
};

// FM broadcast radio re-exports.
pub use fm::{FM_RADIO_CHANNEL_COUNT, FM_RADIO_MAX, FM_RADIO_MIN, FmRadioChannel, FmRadioMode};

// Scan re-exports.
pub use scan::{ProgramScanEdge, ScanResumeMethod, ScanType, VisualRange, VisualScanRange};

// Settings re-exports.
pub use settings::{
    AltitudeRainUnit, AmHighCut, AudioBalance, AudioSettings, AutoMuteReturnDelay, AutoPowerOff,
    BackgroundColor, BacklightControl, BacklightTimer, BatterySaverInterval, BeatShift,
    CallsignReadout, CursorShift, CwFilterWidth, DisplaySettings, DisplayUnits, DtmfToneDuration,
    FrontPanelPfFunction, InformationBacklight, KeyLockSelection, Language, LcdBrightness,
    LedControl, LinkedVolumeLevel, MeterType, MicSensitivity, MicrophonePfFunction, OffMenuPfCode,
    PcOutputInterface, PowerOnMessage, RepeaterCallKey, RxEqLevel, RxEqualizer, ScanRestartDelay,
    SingleBandDisplay, SpeedDistanceUnit, SsbHighCut, StoredFrontPanelPfAssignment, StoredTimeZone,
    SystemSettings, TemperatureUnit, TransmitTimeout, TxEqLevel, TxEqualizer, UsbAudioOutputLevel,
    UsbFunction, VoiceAnnounceMode, VoiceGuideSpeed,
};
pub use system::{RADIO_CLOCK_UNAVAILABLE_WIRE, RadioClock, RadioDateTime};

// CW (Morse code) re-exports.
pub use cw::{CwDelay, CwPitch, CwSettings};

// DTMF re-exports.
pub use dtmf::{
    DtmfDigits, DtmfMemory, DtmfName, DtmfPause, DtmfSettings, DtmfSlot, is_valid_dtmf,
};

// EchoLink re-exports.
pub use echolink::{EchoLinkCode, EchoLinkMemory, EchoLinkName, EchoLinkSlot};

// FM repeater re-exports.
pub use repeater::{RepeaterSettings, ToneBurstHold};

// Voice message re-exports.
pub use voice::{RepeatInterval, VoiceChannel, VoiceMessage, VoiceMessageName};

// Weather alert re-exports (TH-D75A only).
pub use weather::{WeatherAutoScan, WeatherSettings};

// Wireless remote control re-exports.
pub use wireless::RemoteControlCode;
