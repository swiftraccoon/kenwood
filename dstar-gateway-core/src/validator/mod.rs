//! Lenient parser diagnostic stream.
//!
//! Codecs preserve their documented recoverable wire observations and send
//! detected conditions to a [`DiagnosticSink`]. Callers may also record their
//! own validation results and decide which observations require rejection.

pub use diagnostic::{AuthHostSkipReason, CallsignField, Diagnostic};
pub use sink::{DiagnosticSink, NullSink, TracingSink, VecSink};

mod diagnostic;
mod sink;
