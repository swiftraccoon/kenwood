#![doc = include_str!("../README.md")]

// `proptest` is a dev-dependency used only in the integration test
// suites. Acknowledge it here to keep `-D unused-crate-dependencies`
// happy when the lib test crate compiles with dev-deps in scope.
#[cfg(test)]
use proptest as _;

pub use client::{
    AprsIsClient, CONNECT_TIMEOUT, KEEPALIVE_INTERVAL, MAX_IS_READ_LINE_BYTES, SEND_TIMEOUT,
};
pub use error::{AprsIsError, LoginField, LoginFieldError};
pub use events::{AprsIsEvent, AprsIsPacket, LoginRejection, LoginState};
pub use filter::{
    AprsIsFilter, FilterArea, FilterDistance, FilterError, FilterExtension, FilterOverlays,
    FilterPatterns, FilterQConstructs, FilterSymbols, FilterTypes,
};
pub use line::{
    AprsIsAddressField, AprsIsIdentity, AprsIsLine, AprsIsLineError, AprsIsPathElement,
    AprsIsRouteEntry, format_is_packet, parse_is_line,
};
pub use login::{AprsIsConfig, Passcode, VerifiedPasscode, aprs_is_passcode, build_login_string};
pub use q_construct::{
    IGateError, IGateFormatError, IgateMessaging, QConstruct, format_is_packet_with_qconstruct,
    igate_format_for_is, igate_format_packet_for_is, igate_rewritten_path,
};
pub use uplink::{AprsIsUplinkLine, AprsIsUplinkLineError, MAX_IS_LINE_BYTES};

mod client;
mod error;
mod events;
mod filter;
mod line;
mod login;
mod q_construct;
mod uplink;
