//! `DiagnosticSink` trait and three concrete implementations.

use super::diagnostic::Diagnostic;

/// Sink for [`Diagnostic`] events emitted by lenient parsers.
///
/// Three concrete impls ship in this crate ([`NullSink`], [`VecSink`],
/// [`TracingSink`]); consumers can write their own to drive metrics,
/// alerting, strict-mode rejection, etc.
pub trait DiagnosticSink {
    /// Record a diagnostic.
    fn record(&mut self, diagnostic: Diagnostic);
}

/// Discards every diagnostic. Default for tests and pure-codec
/// callers that don't want to track parser observations.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullSink;

impl DiagnosticSink for NullSink {
    fn record(&mut self, _: Diagnostic) {}
}

/// Captures diagnostics into an in-memory `Vec`. Used by tests and
/// by `Session::diagnostics()` (the user-facing accessor).
#[derive(Debug, Default, Clone)]
pub struct VecSink {
    diagnostics: Vec<Diagnostic>,
}

impl VecSink {
    /// Number of recorded diagnostics.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.diagnostics.len()
    }

    /// True if no diagnostics have been recorded.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Drain all recorded diagnostics, leaving the sink empty.
    pub fn drain(&mut self) -> impl Iterator<Item = Diagnostic> + '_ {
        self.diagnostics.drain(..)
    }
}

impl DiagnosticSink for VecSink {
    fn record(&mut self, d: Diagnostic) {
        self.diagnostics.push(d);
    }
}

/// Routes every diagnostic to a `tracing::warn!` event.
///
/// Default sink for the `dstar-gateway` shell crate.
#[derive(Debug, Default, Clone, Copy)]
pub struct TracingSink;

impl DiagnosticSink for TracingSink {
    fn record(&mut self, diagnostic: Diagnostic) {
        tracing::warn!(?diagnostic, "dstar diagnostic recorded");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    const PEER1: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);
    const PEER2: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20002);

    #[test]
    fn null_sink_compiles_and_records_nothing() {
        let mut sink = NullSink;
        let d = Diagnostic::DuplicateLink1Ack { peer: PEER1 };
        sink.record(d);
    }

    #[test]
    fn vec_sink_collects_diagnostics() {
        let mut sink = VecSink::default();
        assert!(sink.is_empty());
        sink.record(Diagnostic::DuplicateLink1Ack { peer: PEER1 });
        sink.record(Diagnostic::DuplicateLink1Ack { peer: PEER2 });
        assert_eq!(sink.len(), 2);
        assert_eq!(sink.drain().count(), 2);
        assert!(sink.is_empty());
    }

    #[test]
    fn tracing_sink_can_be_invoked() {
        let mut sink = TracingSink;
        sink.record(Diagnostic::DuplicateLink1Ack { peer: PEER1 });
    }
}
