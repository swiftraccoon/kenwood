//! Strongly-typed primitives for `dstar-gateway-core`.
//!
//! Every wire-format-relevant scalar is wrapped in a newtype that
//! validates at construction time.

pub use band_letter::BandLetter;
pub use callsign::Callsign;
pub use dcs_reflector::DcsReflector;
pub use frame_sequence::FrameSequence;
pub use module::Module;
pub use protocol_kind::ProtocolKind;
pub use reflector_callsign::ReflectorCallsign;
pub use repeater_identity::{ModuleSlot, RepeaterIdentity};
pub use routing_callsign::RoutingCallsign;
pub use stream_id::StreamId;
pub use suffix::Suffix;
pub use type_error::{TextFieldError, TypeError};
pub use ur_call_action::UrCallAction;
pub use wire_text::WireTextError;

mod band_letter;
mod callsign;
mod dcs_reflector;
mod frame_sequence;
mod module;
mod protocol_kind;
mod reflector_callsign;
mod repeater_identity;
mod routing_callsign;
mod stream_id;
mod suffix;
mod type_error;
mod ur_call_action;
pub(crate) mod wire_text;
