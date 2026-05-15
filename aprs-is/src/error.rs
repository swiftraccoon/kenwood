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
}
