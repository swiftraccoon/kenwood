#![doc = include_str!("../README.md")]

pub use dprs::{
    ChecksumPolicy, DprsComment, DprsError, DprsPosition, DprsReport, DprsSource, DprsSourceError,
    DprsSymbol, Latitude, Longitude, SymbolCode, SymbolOverlay, SymbolTable, compute_crc,
};
pub use error::{
    DExtraError, DPlusError, DcsError, EncodeError, Error, IoOperation, ProtocolError, StateError,
    StateOperation, TimeoutError,
};
pub use header::{
    DstarHeader, ENCODED_LEN, HeaderFlag, HeaderFlags, RelayRouting, RepeaterControl, crc_ccitt,
};
pub use hosts::{HostEntry, HostFile};
pub use session::client::{
    AnySession, Authenticated, ClientProtocol, ClientState, ClientStateKind, Closed, Configured,
    Connected, Connecting, DExtra, DPlus, Dcs, DisconnectReason, Disconnecting, Event, Failed,
    Missing, NoAuthRequired, Protocol, Provided, Session, SessionBuilder, SessionConfig,
    SessionCore, VoiceEndReason,
};
pub use session::server::{ClientRejectedReason, ForwardableFrame, ServerEvent, ServerSessionCore};
pub use session::{Driver, Transmit};
pub use slowdata::{
    MAX_MESSAGE_LEN, SlowDataAssembler, SlowDataBlock, SlowDataBlockKind, SlowDataError,
    SlowDataText, SlowDataTextCollector, SlowDataTextMessage, SlowDataTextMessageError, descramble,
    scramble,
};
pub use types::{
    BandLetter, Callsign, DcsReflector, FrameSequence, Module, ModuleSlot, ProtocolKind,
    ReflectorCallsign, RepeaterIdentity, RoutingCallsign, StreamId, Suffix, TextFieldError,
    TypeError, UrCallAction, WireTextError,
};
pub use validator::{
    AuthHostSkipReason, CallsignField, Diagnostic, DiagnosticSink, NullSink, TracingSink, VecSink,
};
pub use voice::{AMBE_SILENCE, DSTAR_SYNC_BYTES, VoiceFrame};

// The library test target shares integration-only dev-dependencies.
#[cfg(test)]
use proptest as _;

#[cfg(test)]
use trybuild as _;

pub mod codec;
pub mod dprs;
pub mod echo;
pub mod error;
pub mod header;
pub mod hosts;
pub mod session;
pub mod slowdata;
pub mod types;
pub mod validator;
pub mod voice;
