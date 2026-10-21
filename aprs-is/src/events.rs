//! Events emitted by the APRS-IS client.

use std::borrow::Cow;

/// The server's correlated acknowledgement for the current connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoginState {
    /// No matching login response has arrived.
    Pending,
    /// The server accepted the identity for packet uploads.
    Verified,
    /// The server accepted a feed-only login using passcode `-1`.
    ReceiveOnly,
    /// The server rejected the supplied nonnegative passcode.
    Rejected,
}

impl std::fmt::Display for LoginState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Pending => "pending",
            Self::Verified => "verified",
            Self::ReceiveOnly => "receive-only",
            Self::Rejected => "rejected",
        })
    }
}

/// An APRS-IS packet line as received from the server.
///
/// Retains the original bytes without trailing line terminators. [`Self::line`]
/// derives a lossy UTF-8 view from those bytes for display; forwarding and
/// captures use [`Self::raw`] to preserve binary information fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AprsIsPacket {
    raw: Vec<u8>,
}

impl AprsIsPacket {
    pub(crate) const fn from_received(raw: Vec<u8>) -> Self {
        Self { raw }
    }

    /// Borrows the exact received packet bytes without line terminators.
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Borrows valid UTF-8 or replaces malformed sequences with U+FFFD.
    #[must_use]
    pub fn line(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.raw)
    }

    /// Transfers the exact received bytes without copying their allocation.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.raw
    }
}

impl AsRef<[u8]> for AprsIsPacket {
    fn as_ref(&self) -> &[u8] {
        self.raw()
    }
}

/// A correlated server response that rejected passcode authentication.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LoginRejection {
    /// The server returned `unverified` for this connection's passcode login.
    #[error("server returned unverified: {response}")]
    Unverified {
        /// Complete received response text without line terminators.
        response: String,
    },
}

impl LoginRejection {
    /// Borrows the complete response for diagnostics.
    #[must_use]
    pub fn response(&self) -> &str {
        match self {
            Self::Unverified { response } => response,
        }
    }
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

    /// The server acknowledged this connection's login identity.
    ///
    /// Emitted once for `verified`, or for `unverified` with receive-only
    /// authentication. `server` is extracted from the response when present.
    LoggedIn {
        /// APRS-IS server hostname from the `logresp` line (e.g. `T2TEST`).
        server: Option<String>,
    },

    /// The server rejected the login (`# logresp ... unverified`).
    ///
    /// Emitted when the passcode does not validate for the given
    /// callsign. `reason` carries the full comment text for diagnosis.
    LoginRejected {
        /// Typed rejection retaining the server's complete response.
        reason: LoginRejection,
    },

    /// The TCP connection was closed (EOF from server).
    Disconnected,
}
