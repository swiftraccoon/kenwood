//! Error types for APRS-IS operations.

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

    /// An outbound packet line would exceed the APRS-IS spec's 512-byte
    /// limit (including the trailing `CRLF`). Per
    /// <http://www.aprs-is.net/Connecting.aspx>: "No line may exceed
    /// 512 bytes including the CR/LF sequence." Servers silently
    /// truncate offending lines, so the client rejects them up-front
    /// to make the failure mode visible.
    #[error("APRS-IS line too long: {actual} bytes (max {max})")]
    LineTooLong {
        /// Length of the offending line including CRLF.
        actual: usize,
        /// Spec maximum (`512`).
        max: usize,
    },

    /// An inbound line from the server exceeded the read cap without a
    /// terminating newline. A malicious or buggy server (the client
    /// connects to arbitrary internet servers) can stream bytes with no
    /// `\n` to grow the read buffer until the process runs out of
    /// memory; the reader bounds each line and returns this error
    /// instead of growing unboundedly. The oversized line is drained up
    /// to the next newline so the stream resynchronises rather than
    /// wedging. See [`crate::MAX_IS_READ_LINE_BYTES`].
    #[error("APRS-IS inbound line exceeded {max} bytes without a newline")]
    ReadLineTooLong {
        /// Read cap in bytes that was exceeded.
        max: usize,
    },

    /// An outbound line body contained an embedded carriage return or
    /// line feed. Every APRS-IS line is framed by appending `CRLF`, so a
    /// raw `\r`/`\n` inside the caller- or RF-supplied content would
    /// inject a second, forged line onto the uplink stream. The send
    /// path rejects such bodies before writing them. See
    /// [`crate::AprsIsClient::send_raw_line`].
    #[error("APRS-IS line body contains an embedded CR or LF")]
    EmbeddedNewline,

    /// A login field failed validation before the handshake line was
    /// built. Per <http://www.aprs-is.net/Connecting.aspx>, the
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
