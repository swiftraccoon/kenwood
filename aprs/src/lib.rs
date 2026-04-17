//! APRS (Automatic Packet Reporting System) protocol stack.
//!
//! This crate provides a std-only, sans-io implementation of the APRS
//! protocol over AX.25, including the core stateful algorithms used by
//! real APRS clients: digipeater path handling, `SmartBeaconing`, messaging
//! ack/rej tracking, and heard-station lists.
//!
//! # Scope
//!
//! - APRS 1.0.1 / 1.2 info-field parsing: position (compressed and
//!   uncompressed), Mic-E, messages, status, weather, telemetry,
//!   items, objects, queries.
//! - Builders for constructing outgoing info fields.
//! - Stateful algorithms: [`DigipeaterConfig`] (with 30s dup cache and
//!   viscous delay), [`SmartBeaconing`] (`HamHUD` formula), [`AprsMessenger`]
//!   (ack/rej + retry), [`StationList`] (heard-station DB with expiry).
//!
//! # Time handling
//!
//! This crate is sans-io and never calls `std::time::Instant::now()`.
//! Every stateful method accepts a `now: Instant` parameter; callers
//! in the tokio shell layer (e.g. `kenwood-thd75::aprs::AprsClient`)
//! read the wall clock once per iteration and thread it down.
//!
//! # References
//!
//! - APRS 1.0.1: <http://www.aprs.org/doc/APRS101.PDF>
//! - APRS 1.2: <http://www.aprs.org/aprs12.html>

mod build;
mod digipeater;
mod error;
mod item;
mod message;
mod messenger;
mod mic_e;
mod packet;
mod position;
mod smart_beaconing;
mod station_list;
mod status;
mod telemetry;
mod units;
mod weather;

pub use build::{
    build_aprs_item, build_aprs_item_packet, build_aprs_message, build_aprs_message_checked,
    build_aprs_message_packet, build_aprs_mice, build_aprs_mice_with_message,
    build_aprs_mice_with_message_packet, build_aprs_object, build_aprs_object_with_timestamp,
    build_aprs_object_with_timestamp_packet, build_aprs_position_compressed,
    build_aprs_position_compressed_packet, build_aprs_position_report,
    build_aprs_position_report_packet, build_aprs_position_weather,
    build_aprs_position_weather_packet, build_aprs_status, build_aprs_status_packet,
    build_aprs_weather, build_aprs_weather_packet, build_query_response_position,
};
pub use digipeater::{
    DEFAULT_DEDUP_TTL, DEFAULT_VISCOUS_DELAY, DigiAction, DigipeaterAlias, DigipeaterConfig,
};
pub use error::AprsError;
pub use item::{
    AprsItem, AprsObject, AprsQuery, parse_aprs_item, parse_aprs_object, parse_aprs_query,
};
pub use message::{AprsMessage, MAX_APRS_MESSAGE_TEXT_LEN, classify_ack_rej, parse_aprs_message};
pub use messenger::{
    AprsMessenger, INCOMING_DEDUP_WINDOW, MAX_RETRIES, MessengerConfig, RETRY_INTERVAL,
};
pub use mic_e::{MiceMessage, mice_message_bits, parse_aprs_data_full, parse_mice_position};
pub use packet::{
    AprsData, AprsDataExtension, AprsPacket, AprsTimestamp, MessageKind, ParseContext, Phg,
    PositionAmbiguity, TelemetryDefinition, TelemetryParameters, parse_aprs_data,
    parse_aprs_extensions,
};
pub use position::{
    AprsPosition, decode_base91_4, parse_aprs_position, parse_compressed_body,
    parse_uncompressed_body,
};
pub use smart_beaconing::{BeaconReason, BeaconState, SmartBeaconing, SmartBeaconingConfig};
pub use station_list::{StationEntry, StationList};
pub use status::{AprsStatus, parse_aprs_status};
pub use telemetry::{AprsTelemetry, parse_aprs_telemetry};
pub use units::{
    AprsSymbol, Course, Fahrenheit, Latitude, Longitude, MessageId, Speed, SymbolTable, Tocall,
};
pub use weather::{AprsWeather, extract_position_weather, parse_aprs_weather_positionless};

// `proptest` is a dev-dependency used only in the integration test
// suites. Acknowledge it here to keep `-D unused-crate-dependencies`
// happy when the lib test crate compiles with dev-deps in scope.
#[cfg(test)]
use proptest as _;
