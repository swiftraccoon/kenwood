//! Error types for APRS-IS operations.

use crate::uplink::AprsIsUplinkLineError;

/// Errors that can occur during APRS-IS operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AprsIsError {
    /// The TCP connection could not be established.
    #[error("APRS-IS connect failed: {0}")]
    Connect(#[source] std::io::Error),

    /// A read from the TCP socket failed.
    #[error("APRS-IS read failed: {0}")]
    Read(#[source] std::io::Error),

    /// A write to the TCP socket failed.
    #[error("APRS-IS write failed: {0}")]
    Write(#[source] std::io::Error),

    /// The initial login handshake timed out.
    #[error("APRS-IS login timed out")]
    LoginTimeout,

    /// The server rejected the login credentials.
    #[error("APRS-IS login rejected: {0}")]
    LoginRejected(String),

    /// Outbound bytes did not form one safe, correctly framed APRS-IS
    /// line.
    #[error("invalid APRS-IS uplink line: {0}")]
    InvalidUplinkLine(#[from] AprsIsUplinkLineError),

    /// An inbound line from the server exceeded the read cap. A malicious or
    /// buggy server can otherwise stream an unbounded line until the process
    /// runs out of memory. The oversized line is drained up to its next
    /// newline, or rejected at EOF, rather than exposing its retained prefix
    /// as a packet. See [`crate::MAX_IS_READ_LINE_BYTES`].
    #[error("APRS-IS inbound line exceeded {max} bytes")]
    ReadLineTooLong {
        /// Read cap in bytes that was exceeded.
        max: usize,
    },

    /// A login field failed validation before the handshake line was
    /// built. Per <https://www.aprs-is.net/Connecting.aspx>, the
    /// software name and version must each be one word ("Do not use
    /// spaces") and the callsign is alphanumeric ASCII; an embedded
    /// space or `CRLF` would corrupt the space-delimited handshake or
    /// (for the filter) inject a second handshake line.
    #[error("APRS-IS login field {field} is invalid: {reason}")]
    InvalidLoginField {
        /// Name of the offending field (e.g. `"software_name"`).
        field: &'static str,
        /// Human-readable reason the value was rejected.
        reason: &'static str,
    },
}
