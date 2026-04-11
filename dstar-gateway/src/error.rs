//! Unified error type for dstar-gateway.
//!
//! Wraps I/O errors from tokio UDP sockets and adds protocol-specific
//! variants for connect timeouts, rejection, state violations, and
//! malformed auth responses. All public fallible methods on
//! `ReflectorClient`, `DExtraClient`, `DPlusClient`, and `DcsClient`
//! return `Result<_, Error>`.

use std::time::Duration;

/// Top-level error type for all dstar-gateway operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O error occurred on the underlying socket.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Connect did not complete within the supplied timeout.
    #[error("connect timeout after {0:?}")]
    ConnectTimeout(Duration),

    /// The reflector rejected the connection request.
    #[error("reflector rejected the connection")]
    Rejected,

    /// Operation attempted on a non-connected client.
    #[error("not connected")]
    NotConnected,

    /// DCS `send_voice` called before `send_header` — no cached header.
    #[error("DCS send_voice called before send_header")]
    NoTxHeader,

    /// `DPlus` auth server returned a malformed response.
    #[error("invalid DPlus auth response: {0}")]
    AuthResponseInvalid(&'static str),

    /// A type validation error (module, callsign, stream ID).
    #[error("invalid type: {0}")]
    Type(#[from] crate::types::TypeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_conversion_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::TimedOut, "test");
        let err: Error = io.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn connect_timeout_display() {
        let err = Error::ConnectTimeout(Duration::from_secs(5));
        assert!(err.to_string().contains("5s"));
    }

    #[test]
    fn rejected_display() {
        let err = Error::Rejected;
        assert_eq!(err.to_string(), "reflector rejected the connection");
    }

    #[test]
    fn auth_response_invalid_display() {
        let err = Error::AuthResponseInvalid("missing header");
        assert_eq!(
            err.to_string(),
            "invalid DPlus auth response: missing header"
        );
    }
}
