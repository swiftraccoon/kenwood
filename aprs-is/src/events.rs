//! Events emitted by the APRS-IS client.

/// An event from the APRS-IS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AprsIsEvent {
    /// An APRS packet line was received (not a comment).
    ///
    /// The line is stripped of trailing `\r\n`. Parse with the standard
    /// APRS parsers in the `aprs` crate after splitting
    /// source/dest/path/data.
    Packet(String),

    /// A server comment line was received (starts with `#`).
    ///
    /// Comments carry server info, login responses, and keepalives.
    /// The line is stripped of trailing `\r\n`.
    Comment(String),

    /// The server accepted the login (`# logresp ... verified, server ...`).
    ///
    /// Emitted the first time a `logresp` line confirming `verified` is
    /// seen. `server` is the upstream server's hostname extracted from
    /// the comment, if present.
    LoggedIn {
        /// APRS-IS server hostname from the `logresp` line (e.g. `T2TEST`).
        server: Option<String>,
    },

    /// The server rejected the login (`# logresp ... unverified`).
    ///
    /// Emitted when the passcode does not validate for the given
    /// callsign. `reason` carries the full comment text for diagnosis.
    LoginRejected {
        /// Raw reason text from the server's `logresp` line.
        reason: String,
    },

    /// The TCP connection was closed (EOF from server).
    Disconnected,
}
