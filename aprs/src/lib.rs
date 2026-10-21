#![doc = include_str!("../README.md")]

// `proptest` is a dev-dependency used by `tests/adversarial_parse.rs`
// (the never-panics net over the parse entry points). Each test target
// is its own compilation unit that sees every dev-dep, so the lib test
// target must acknowledge it or `unused_crate_dependencies` fires.
#[cfg(test)]
use proptest as _;

pub use digipeater::{
    DEFAULT_DEDUP_TTL, DEFAULT_VISCOUS_DELAY, DigiAction, DigipeaterAlias, DigipeaterAliasError,
    DigipeaterAliases, DigipeaterCapacityError, DigipeaterConfig, DigipeaterLimits,
    DigipeaterQueue,
};
pub use error::{AprsError, AprsWireTextField};
pub use item::{AprsItem, AprsObject, AprsQuery, ReportState};
pub use message::{
    AnnouncementLetter, AprsMessage, BulletinAddressee, BulletinGroup, BulletinNumber,
    DirectMessageAddressee, MAX_APRS_MESSAGE_TEXT_LEN, MessageControl, MessageValueError,
};
pub use messenger::{
    AprsMessenger, INCOMING_DEDUP_WINDOW, MAX_RETRIES, MessengerConfig, RETRY_INTERVAL,
};
pub use mic_e::{MiceMessage, MiceTelemetry, MiceTelemetryError};
pub use packet::{
    AprsData, AprsDataExtension, AprsPacket, AprsReportTimestamp, AprsReportTimestampFormat,
    AprsWeatherTimestamp, CourseSpeed, DaoDatum, DaoExtension, DaoPrecision, ExperimentId,
    MaidenheadGrid, MessageKind, Phg, PhgDirectivity, PositionAmbiguity, RawWeatherFormat,
    TelemetryDefinition, TimestampError,
};
pub use position::{
    AprsPosition, CompressedAltitude, CompressedPositionData, CompressedRange, PositionMotion,
};
pub use reports::{AprsFrameHeader, ItemReport, MiceReport, ObjectReport, PositionReport};
pub use smart_beaconing::{
    BeaconReason, BeaconState, SmartBeaconing, SmartBeaconingConfig, SmartBeaconingConfigError,
    SmartBeaconingParameters,
};
pub use station_list::{StationEntry, StationIter, StationList, StationRadius, StationRadiusError};
pub use status::{AprsStatus, AprsStatusTimestamp, MAX_APRS_STATUS_TEXT_LEN};
pub use telemetry::{
    AprsTelemetry, TelemetryAnalogValue, TelemetryBits, TelemetryChannel, TelemetryComment,
    TelemetryEquation, TelemetryEquationCoefficients, TelemetryLabels, TelemetryProjectTitle,
    TelemetrySequence, TelemetryValueError,
};
pub use text::{
    AprsTextError, AprsTextField, BulletinText, CompressedPositionText, ItemName, MessageAddressee,
    MessageText, MiceStatusText, ObjectName, PositionReportText, QueryText, StatusText,
    TimestampedStatusText, WeatherComment,
};
pub use units::{
    AprsSymbol, CoordinateError, Course, CourseError, Fahrenheit, Heading, Latitude, Longitude,
    MessageId, MessageIdError, MiceSpeed, Speed, SpeedError, SymbolError, SymbolOverlay,
    SymbolTable, SymbolTableError, TemperatureError, Tocall,
};
pub use weather::{
    AprsPositionlessWeatherReport, AprsWeather, BarometricPressure, Humidity, Luminosity,
    ThreeDigitWeatherValue, WeatherValueError, WindDirection,
};

mod build;
mod digipeater;
mod error;
mod item;
mod message;
mod messenger;
mod mic_e;
mod packet;
mod position;
mod reports;
mod smart_beaconing;
mod station_list;
mod status;
mod telemetry;
mod text;
mod units;
mod weather;
