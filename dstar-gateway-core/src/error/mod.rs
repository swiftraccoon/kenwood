//! Error types for `dstar-gateway-core`.
//!
//! Per-protocol structured error variants are used so that callers
//! can pattern-match to distinguish I/O failures from timeouts from
//! protocol errors from type-validation errors.

mod encode;
mod io_operation;
mod protocol;
mod state;
mod timeout;

pub use crate::codec::dcs::error::DcsError;
pub use crate::codec::dextra::error::DExtraError;
pub use crate::codec::dplus::error::DPlusError;
pub use encode::EncodeError;
pub use io_operation::IoOperation;
pub use protocol::ProtocolError;
pub use state::StateError;
pub use timeout::TimeoutError;

use crate::types::TypeError;

/// Top-level error type for `dstar-gateway-core`.
///
/// All fallible operations in the core return `Result<_, Error>`.
/// Pattern-match this enum to distinguish I/O failures from
/// timeouts from protocol errors from type-validation errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Underlying I/O failure on a socket or TCP stream.
    ///
    /// Only emitted by the shell crates — `dstar-gateway-core` itself
    /// has no I/O. Carried here for the shell crates' convenience.
    #[error("I/O error during {operation:?}: {source}")]
    Io {
        /// Underlying `std::io::Error`.
        #[source]
        source: std::io::Error,
        /// What kind of I/O the operation was.
        operation: IoOperation,
    },

    /// Construction-time validation failure (Callsign, Module, etc.).
    #[error(transparent)]
    Type(#[from] TypeError),

    /// A typed deadline-exceeded error.
    #[error("operation timed out: {0}")]
    Timeout(#[source] TimeoutError),

    /// Protocol-level error (per-protocol structured cause).
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// State-machine residual that the typestate cannot prevent.
    #[error("session in invalid state: {0}")]
    State(#[source] StateError),
}

impl From<TimeoutError> for Error {
    fn from(value: TimeoutError) -> Self {
        Self::Timeout(value)
    }
}

impl From<StateError> for Error {
    fn from(value: StateError) -> Self {
        Self::State(value)
    }
}

#[cfg(test)]
mod top_level_tests {
    use super::*;
    use static_assertions::assert_impl_all;

    assert_impl_all!(Error: Send, Sync);

    #[test]
    fn error_is_send_sync_static() {
        // The static assertion above is the real test. This runtime
        // body just exists so cargo test reports a passing test.
    }

    #[test]
    fn type_error_into_top_level() {
        let te = TypeError::InvalidModule { got: '@' };
        let err: Error = te.into();
        assert!(matches!(
            err,
            Error::Type(TypeError::InvalidModule { got: '@' })
        ));
    }

    #[test]
    fn timeout_into_top_level() {
        let to = TimeoutError::Disconnect {
            deadline: std::time::Duration::from_secs(2),
            elapsed: std::time::Duration::from_secs(2),
        };
        let err: Error = to.into();
        assert!(matches!(err, Error::Timeout(_)));
    }

    #[test]
    fn protocol_into_top_level() {
        let pe = ProtocolError::DPlus(DPlusError::StreamIdZero);
        let err: Error = pe.into();
        assert!(matches!(
            err,
            Error::Protocol(ProtocolError::DPlus(DPlusError::StreamIdZero))
        ));
    }
}
