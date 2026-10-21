//! Error types for APRS-IS operations.

use crate::uplink::AprsIsUplinkLineError;

/// The checked login setting that rejected an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoginField {
    /// Callsign and optional APRS-IS SSID.
    Callsign,
    /// Server hostname or IP address.
    Server,
    /// Software-name login word.
    SoftwareName,
    /// Software-version login word.
    SoftwareVersion,
    /// Raw filter extension text.
    Filter,
}

impl std::fmt::Display for LoginField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Callsign => "callsign",
            Self::Server => "server",
            Self::SoftwareName => "software_name",
            Self::SoftwareVersion => "software_version",
            Self::Filter => "filter",
        })
    }
}

/// The violated login-field construction rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
#[non_exhaustive]
pub enum LoginFieldError {
    /// A field required ASCII but contained another encoding.
    #[error("must contain only ASCII characters")]
    NonAscii,
    /// The complete callsign and SSID exceeded nine bytes.
    #[error("must not exceed 9 ASCII characters including SSID")]
    IdentityTooLong,
    /// More than one SSID separator occurred.
    #[error("may contain only one SSID separator")]
    MultipleSsidSeparators,
    /// The base callsign contained fewer than three characters.
    #[error("base callsign must contain at least 3 alphanumeric characters")]
    BaseTooShort,
    /// The base contained a byte outside ASCII letters and digits.
    #[error("base callsign must contain only ASCII alphanumeric characters")]
    InvalidBase,
    /// The SSID contained zero or more than two characters.
    #[error("SSID must contain 1 or 2 characters")]
    InvalidSsidLength,
    /// The SSID contained a byte outside ASCII letters and digits.
    #[error("SSID must contain only ASCII alphanumeric characters")]
    InvalidSsid,
    /// The explicit SSID `-0` was supplied instead of omitting it.
    #[error("SSID zero must be omitted")]
    ExplicitZeroSsid,
    /// Eight hexadecimal characters are reserved for server IP identities.
    #[error("eight hexadecimal characters are reserved for server identities")]
    ReservedServerIdentity,
    /// A required software word was empty.
    #[error("must not be empty")]
    Empty,
    /// A software word contained whitespace or a control byte.
    #[error("must be one word without whitespace or control characters")]
    NotSingleWord,
    /// A raw filter contained a control byte.
    #[error("must not contain control characters")]
    ControlCharacter,
    /// A server setting was neither a DNS hostname nor an IP address.
    #[error("must be a DNS hostname or IP address")]
    InvalidEndpoint,
}

/// Errors that can occur during APRS-IS operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AprsIsError {
    /// Structured filter clauses violate their combination rules.
    #[error("invalid APRS-IS filter: {0}")]
    InvalidFilter(#[from] crate::FilterError),
    /// The TCP connection could not be established.
    #[error("APRS-IS connect failed: {0}")]
    Connect(#[source] std::io::Error),

    /// A read from the TCP socket failed.
    #[error("APRS-IS read failed: {0}")]
    Read(#[source] std::io::Error),

    /// A write to the TCP socket failed.
    #[error("APRS-IS write failed: {0}")]
    Write(#[source] std::io::Error),

    /// A previous send failed or was cancelled after taking the write half.
    /// Reconnect before sending another line.
    #[error("APRS-IS outgoing stream is retired; reconnect before sending")]
    ConnectionRetired,

    /// A packet upload was attempted before a verified server acknowledgement.
    /// The rejected line writes no bytes and leaves the stream available.
    #[error("APRS-IS packet upload requires a verified login; current state is {state}")]
    UnverifiedConnection {
        /// The current connection's correlated login state.
        state: crate::LoginState,
    },

    /// The shared connection deadline expired while writing the login line.
    #[error("APRS-IS login timed out")]
    LoginTimeout,

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
        field: LoginField,
        /// Human-readable reason the value was rejected.
        reason: LoginFieldError,
    },
}
