//! Lenient parser diagnostic stream.
//!
//! The codec accepts garbage from the wire (so we never silently
//! lose real-world reflector traffic), but every parser pushes
//! structured `Diagnostic` events into a [`DiagnosticSink`]. The
//! consumer chooses what to do with them — log, drop, alert, or
//! reject the packet.

mod diagnostic;
mod sink;

pub use diagnostic::{AuthHostSkipReason, CallsignField, Diagnostic};
pub use sink::{DiagnosticSink, NullSink, TracingSink, VecSink};
