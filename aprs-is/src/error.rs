//! Error types for APRS-IS operations.

/// Errors that can occur during APRS-IS operations.
#[derive(Debug, thiserror::Error)]
pub enum AprsIsError {
    /// The TCP connection could not be established.
    #[error("APRS-IS connect failed: {0}")]
    Connect(std::io::Error),

    /// A read from the TCP socket failed.
    #[error("APRS-IS read failed: {0}")]
    Read(std::io::Error),

    /// A write to the TCP socket failed.
    #[error("APRS-IS write failed: {0}")]
    Write(std::io::Error),

    /// The initial login handshake timed out.
    #[error("APRS-IS login timed out")]
    LoginTimeout,

    /// The server rejected the login credentials.
    #[error("APRS-IS login rejected: {0}")]
    LoginRejected(String),
}
