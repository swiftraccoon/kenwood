//! Sans-io core for the `dstar-gateway` D-STAR reflector library.
//!
//! This crate is **runtime-agnostic and I/O-free**. It contains the
//! wire-format codecs, the typestate session machines, and the
//! supporting types and errors. The async (tokio) and blocking shells
//! live in the sibling [`dstar-gateway`] and [`dstar-gateway-server`]
//! crates respectively.
//!
//! See [`crate::types`] for validated primitives, [`crate::header`]
//! for the D-STAR radio header, [`crate::voice`] for voice frame
//! types, [`crate::hosts`] for the host file parser, and
//! [`crate::error`] for the error hierarchy.
//!
//!
//! [`dstar-gateway`]: https://github.com/swiftraccoon/dstar-gateway/tree/main/dstar-gateway
//! [`dstar-gateway-server`]: https://github.com/swiftraccoon/dstar-gateway/tree/main/dstar-gateway-server

pub mod codec;
pub mod dprs;
pub mod error;
pub mod header;
pub mod hosts;
pub mod session;
pub mod slowdata;
pub mod types;
pub mod validator;
pub mod voice;

pub use dprs::{DprsError, DprsReport, Latitude, Longitude, compute_crc, encode_dprs, parse_dprs};
pub use error::{
    DExtraError, DPlusError, DcsError, EncodeError, Error, IoOperation, ProtocolError, StateError,
    TimeoutError,
};
pub use header::{DStarHeader, ENCODED_LEN, crc_ccitt};
pub use hosts::{HostEntry, HostFile};
pub use session::client::{
    AnySession, Authenticated, ClientState, ClientStateKind, Closed, Configured, Connected,
    Connecting, DExtra, DPlus, Dcs, DisconnectReason, Disconnecting, Event, Failed, Missing,
    NoAuthRequired, Protocol, Provided, Session, SessionBuilder, SessionCore, VoiceEndReason,
};
pub use session::server::{
    ClientRejectedReason, ForwardableFrame, ServerEvent, ServerSession, ServerSessionCore,
};
pub use session::{Driver, Transmit};
pub use slowdata::{
    MAX_MESSAGE_LEN, SlowDataAssembler, SlowDataBlock, SlowDataBlockKind, SlowDataError,
    SlowDataText, SlowDataTextCollector, descramble, encode_text_message, scramble,
};
pub use types::{
    BandLetter, Callsign, Module, ProtocolKind, ReflectorCallsign, StreamId, Suffix, TypeError,
};
pub use validator::{
    AuthHostSkipReason, CallsignField, Diagnostic, DiagnosticSink, NullSink, TracingSink, VecSink,
};
pub use voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

// `proptest` is a dev-dependency used only in the integration test
// suites under `tests/`. The lib test crate compiles with dev-deps in
// scope too, so we acknowledge it here to keep
// `-D unused-crate-dependencies` happy.
#[cfg(test)]
use proptest as _;

// `trybuild` is a dev-dependency used by the compile-fail test harness;
// acknowledged here so the lint pass doesn't fire.
#[cfg(test)]
use trybuild as _;
