//! APRS-IS (APRS Internet Service) TCP client.
//!
//! Connects to APRS-IS core/tier2 servers, authenticates with
//! callsign+passcode, subscribes to filters, receives APRS packets
//! as TNC2-format text lines, and gates RF-heard traffic with
//! Q-construct rules.
//!
//! # Scope
//!
//! - [`AprsIsClient`] async TCP client with keepalive and bounded reader.
//! - [`AprsIsConfig`] login configuration, [`aprs_is_passcode`] hash.
//! - [`AprsIsFilter`] filter-command builder.
//! - [`QConstruct`] Q-construct classification.
//! - [`igate_format_for_is`] full `IGate` gating implementing the
//!   q.aspx algorithm (refuses to gate on `NOGATE`/`RFONLY`/`TCPIP`/
//!   `TCPXX`/loop, gates the heard path verbatim — preserving unused
//!   digipeaters and `*` markers — and selects `qAR`/`qAO` from the
//!   login state). For basic q-construct appending see
//!   [`format_is_packet_with_qconstruct`].
//! - [`parse_is_line`] TNC2 monitor-format parser.
//! - [`format_is_packet`] outbound line formatter.
//!
//! # References
//!
//! - APRS-IS: <http://www.aprs-is.net/>
//! - Q-construct: <http://www.aprs-is.net/q.aspx>

// The `aprs` crate is declared as a dep so downstream crates (e.g.
// thd75) can rely on a single version line across both crates and so
// future helpers in aprs-is can use its types without a Cargo.toml
// churn. aprs-is itself stays transport/format-focused and does not
// reach into the APRS info-field parsers, so the dep is acknowledged
// here to keep `-D unused-crate-dependencies` happy.
use aprs as _;

// `proptest` is a dev-dependency used only in the integration test
// suites. Acknowledge it here to keep `-D unused-crate-dependencies`
// happy when the lib test crate compiles with dev-deps in scope.
#[cfg(test)]
use proptest as _;

mod client;
mod error;
mod events;
mod filter;
mod line;
mod login;
mod q_construct;

pub use client::{
    AprsIsClient, CONNECT_TIMEOUT, KEEPALIVE_INTERVAL, MAX_IS_LINE_BYTES, MAX_IS_READ_LINE_BYTES,
};
pub use error::AprsIsError;
pub use events::{AprsIsEvent, AprsIsPacket};
pub use filter::AprsIsFilter;
pub use line::{AprsIsLine, format_is_packet, parse_is_line};
pub use login::{AprsIsConfig, Passcode, aprs_is_passcode, build_login_string};
pub use q_construct::{
    IGateError, QConstruct, format_is_packet_with_qconstruct, igate_format_for_is,
    igate_format_packet_for_is, igate_rewritten_path,
};
