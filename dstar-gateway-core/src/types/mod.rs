//! Strongly-typed primitives for `dstar-gateway-core`.
//!
//! Every wire-format-relevant scalar is wrapped in a newtype that
//! validates at construction time.

mod band_letter;
mod callsign;
mod module;
mod protocol_kind;
mod reflector_callsign;
mod stream_id;
mod suffix;
mod type_error;

pub use band_letter::BandLetter;
pub use callsign::Callsign;
pub use module::Module;
pub use protocol_kind::ProtocolKind;
pub use reflector_callsign::ReflectorCallsign;
pub use stream_id::StreamId;
pub use suffix::Suffix;
pub use type_error::TypeError;
