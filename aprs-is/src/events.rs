//! Events emitted by the APRS-IS client.

/// An APRS-IS packet line as received from the server.
///
/// Carries both the **wire-truth bytes** (`raw`, with the trailing
/// `\r\n` stripped) and a **lossy UTF-8 view** (`line`) for parsing and
/// display. The lossy view is built via [`String::from_utf8_lossy`],
/// which replaces invalid UTF-8 sequences with the U+FFFD replacement
/// character — this prevents a single non-UTF-8 byte (common in Mic-E
/// payloads, raw weather data, and Latin-1 comments) from killing the
/// TCP connection, while keeping the raw bytes available for callers
/// that need exact wire fidelity (e.g. `IGate` forwarding, packet
/// captures).
///
/// Pre-2026 versions of this crate decoded via `BufReader::read_line`,
/// which returns `Err(io::Error::InvalidData)` on the first non-UTF-8
/// byte. That error tore down the long-lived TCP session on the first
/// real binary-bearing packet — see git history for CB-4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsPacket {
    /// Wire-truth bytes for the packet line, **without** the trailing
    /// `\r` / `\n`. Callers that need to forward the packet across an
    /// `IGate` or write it to a capture file should use this field —
    /// `line` may carry replacement characters in place of any non-UTF-8
    /// byte and is not byte-identical to what the server sent.
    pub raw: Vec<u8>,
    /// Lossy UTF-8 view of `raw`, suitable for parsing via
    /// [`crate::parse_is_line`] and for display. Any byte that is not
    /// valid UTF-8 is replaced with U+FFFD.
    pub line: String,
}

/// An event from the APRS-IS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AprsIsEvent {
    /// An APRS packet line was received (not a comment).
    ///
    /// Carries the raw wire bytes plus a lossy UTF-8 view; see
    /// [`AprsIsPacket`] for the rationale.
    Packet(AprsIsPacket),

    /// A server comment line was received (starts with `#`).
    ///
    /// Comments carry server info, login responses, and keepalives.
    /// The line is stripped of trailing `\r\n` and decoded via
    /// [`String::from_utf8_lossy`]; APRS-IS comments are ASCII in
    /// practice, but the lossy path keeps a stray binary byte from
    /// killing the connection.
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
