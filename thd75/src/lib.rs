#![deny(unsafe_code)]
#![doc = include_str!("../README.md")]

// Dev-dependencies used only by the integration tests in `tests/`.
// Acknowledge them at the lib level so `unused_crate_dependencies`
// stays silent for the lib-test compilation unit, which sees every
// dev-dep but exercises none of these directly.
#[cfg(test)]
use proptest as _;
#[cfg(test)]
use serde_json as _;
// The client-stack crates are unconditional dev-dependencies (for tests and
// examples); with the corresponding lib feature off, the lib-test unit sees
// them without using them, so acknowledge them for that configuration only.
#[cfg(all(test, not(feature = "aprs")))]
use aprs_is as _;
#[cfg(all(test, not(feature = "aprs")))]
use kiss_tnc as _;

// Convenience re-exports for the most commonly used types.
pub use error::Error;
pub use radio::diagnostics::LinkDiagnosis;
pub use radio::if_tap::{
    IfTapConfig, IfTapEnterError, IfTapRestoreReport, IfTapRestoreStep, IfTapSavedState,
    IfTapSession,
};
pub use radio::programming::{McpPage, McpSession, McpWriteProgress, WritableMcpPage};
pub use radio::state_monitor::{BandState, StateChange, StateMonitor};
pub use radio::terminal_mode::{TerminalModeTransition, TerminalModeTransitionError};
pub use radio::{DesyncedRadio, FirmwareProfile, Radio};
#[cfg(target_os = "macos")]
pub use transport::BluetoothTransport;
pub use transport::{EitherTransport, SerialTransport};
pub use types::{
    ChannelDisplayName, FirmwareIdentity, HardwareVariant, ModelCode, RadioModel, RadioRegion,
    RadioType, RegularChannel, SerialInformation, SerialNumber,
};

// Memory image re-exports.
pub use memory::{MemoryError, MemoryImage};

// Generic crate re-exports at crate root for consumer convenience.
//
// These let existing downstream code keep using `kenwood_thd75::AprsClient`,
// `kenwood_thd75::KissFrame`, etc. without importing the generic crates
// directly. The items themselves live in `kiss-tnc`, `ax25-codec`, `aprs`,
// and `aprs-is`; inside this crate, use those crate paths directly rather
// than routing through these re-exports.
pub use ::aprs::{
    AprsData, AprsDataExtension, AprsError, AprsFrameHeader, AprsItem, AprsMessage, AprsMessenger,
    AprsObject, AprsPosition, AprsPositionlessWeatherReport, AprsQuery, AprsReportTimestamp,
    AprsReportTimestampFormat, AprsStatus, AprsStatusTimestamp, AprsSymbol, AprsTelemetry,
    AprsTextError, AprsTextField, AprsWeather, AprsWeatherTimestamp, BarometricPressure,
    BulletinAddressee, CompressedPositionText, Course, DigiAction, DigipeaterAliases,
    DigipeaterConfig, DigipeaterLimits, DigipeaterQueue, DirectMessageAddressee, Fahrenheit,
    Heading, Humidity, ItemName, ItemReport, Latitude, Longitude, Luminosity, MessageAddressee,
    MessageControl, MessageId, MessageText, MiceReport, MiceSpeed, MiceStatusText, ObjectName,
    ObjectReport, Phg, PhgDirectivity, PositionReport, PositionReportText, ReportState,
    SmartBeaconing, SmartBeaconingConfig, Speed, StationEntry, StationList, StatusText,
    SymbolTable, ThreeDigitWeatherValue, TimestampedStatusText, Tocall, WeatherComment,
    WeatherValueError, WindDirection,
};
#[cfg(feature = "aprs")]
pub use aprs_is::{
    AprsIsClient, AprsIsConfig, AprsIsError, AprsIsEvent, AprsIsUplinkLine, AprsIsUplinkLineError,
    IGateFormatError, Passcode, aprs_is_passcode, build_login_string, format_is_packet,
    parse_is_line,
};
pub use ax25_codec::{Ax25Address, Ax25Error, Ax25Packet, DigipeaterPath};
#[cfg(feature = "aprs")]
pub use kiss_tnc::{KissError, KissFrame};

// D75-specific re-exports.
#[cfg(feature = "aprs")]
pub use aprs::client::{AprsClient, AprsClientConfig, AprsEvent, IGateRfLocality, IGateToRfConfig};

// KISS session re-export.
#[cfg(feature = "aprs")]
pub use radio::kiss_session::KissSession;

// MMDVM session re-export.
#[cfg(feature = "dstar")]
pub use radio::mmdvm_session::{MmdvmSession, MmdvmShutdown};

// Link-recovery policy.
pub use session::ReconnectPolicy;

// Only model-owned lifecycle types are exported here. Shared runtime and
// protocol types are imported directly from their defining crates.
#[cfg(feature = "dstar")]
pub use dstar_gateway::{DstarGateway, PersistentMmdvm, TransientMmdvm};

// SD card re-exports.
pub use sdcard::SdCardError;
pub use sdcard::config::{ConfigFileModel, ConfigHeader, RadioConfig};

// Descendant modules use this lexical macro before their scalar definitions.
#[macro_use]
mod macros;

#[cfg(feature = "aprs")]
pub mod aprs;
#[cfg(feature = "dstar")]
pub mod dstar_gateway;
pub mod error;
pub mod memory;
pub mod protocol;
pub mod radio;
pub mod screen;
pub mod sdcard;
pub mod session;
pub mod transport;
pub mod types;
pub mod verify;
