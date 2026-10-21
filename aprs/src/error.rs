//! APRS protocol error type.

use thiserror::Error;

/// The byte-oriented APRS field rejected while decoding ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AprsWireTextField {
    /// Nine-byte padded message destination.
    MessageAddressee,
    /// Message body and trailers.
    MessageBody,
    /// Nine-byte padded object name.
    ObjectName,
    /// Object observation timestamp.
    ObjectTimestamp,
    /// Query text after its data-type byte.
    QueryBody,
    /// Status text and optional timestamp.
    StatusBody,
    /// Telemetry report after `T#`.
    TelemetryBody,
    /// Position observation timestamp.
    PositionTimestamp,
    /// Uncompressed-position comment.
    UncompressedPositionComment,
    /// Compressed-position comment.
    CompressedPositionComment,
    /// Mic-E trailing comment.
    MiceComment,
    /// Positionless-weather observation timestamp.
    WeatherTimestamp,
    /// Positionless-weather trailing comment.
    WeatherComment,
    /// Third-party packet's encapsulated header.
    ThirdPartyHeader,
    /// Maidenhead grid report.
    MaidenheadGrid,
    /// NMEA sentence transported in APRS.
    NmeaSentence,
    /// Station-capability keys and values.
    StationCapabilities,
}

impl std::fmt::Display for AprsWireTextField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MessageAddressee => "APRS message addressee",
            Self::MessageBody => "APRS message body",
            Self::ObjectName => "APRS object name",
            Self::ObjectTimestamp => "APRS object timestamp",
            Self::QueryBody => "APRS query body",
            Self::StatusBody => "APRS status body",
            Self::TelemetryBody => "APRS telemetry body",
            Self::PositionTimestamp => "APRS position timestamp",
            Self::UncompressedPositionComment => "APRS uncompressed-position comment",
            Self::CompressedPositionComment => "APRS compressed-position comment",
            Self::MiceComment => "APRS Mic-E comment",
            Self::WeatherTimestamp => "APRS weather timestamp",
            Self::WeatherComment => "APRS weather comment",
            Self::ThirdPartyHeader => "APRS third-party header",
            Self::MaidenheadGrid => "APRS Maidenhead grid",
            Self::NmeaSentence => "APRS NMEA sentence",
            Self::StationCapabilities => "APRS station capabilities",
        })
    }
}

/// Errors produced by APRS parsing, building, and stateful algorithms.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum AprsError {
    /// The info field is too short or has an unrecognized data type.
    #[error("invalid APRS format")]
    InvalidFormat,
    /// The position coordinates could not be parsed.
    #[error("invalid APRS coordinates")]
    InvalidCoordinates,
    /// Mic-E data requires the AX.25 destination address for decoding.
    #[error("Mic-E data requires destination address; use AprsData::from_bytes_with_destination")]
    MicERequiresDestination,
    /// The rounded Mic-E longitude reaches 180 degrees, outside the degree table.
    #[error("Mic-E longitude must round below 180 degrees at hundredth-minute precision")]
    UnrepresentableMiceLongitude,
    /// The messenger's bounded pending queue has no available message slot.
    #[error("APRS pending message capacity {capacity} is full")]
    PendingMessageCapacity {
        /// Maximum number of pending messages allowed by the configuration.
        capacity: std::num::NonZeroU16,
    },
    /// The bounded incoming-history map cannot retain another live identity.
    #[error("APRS incoming message capacity {capacity} is full")]
    IncomingMessageCapacity {
        /// Maximum live source/message-ID pairs retained for deduplication.
        capacity: std::num::NonZeroUsize,
    },
    /// Retry or incoming deduplication timing is zero.
    #[error("APRS messenger intervals must be nonzero")]
    InvalidMessengerInterval,
    /// A commit refers to a message that is no longer pending.
    #[error("APRS message is no longer pending")]
    UnknownPendingMessage,
    /// A commit precedes its retry interval, including regressing supplied time.
    #[error("APRS message is not eligible for another transmission yet")]
    MessageRetryNotDue,
    /// A commit would exceed the configured transmission-attempt limit.
    #[error("APRS message transmission attempts are exhausted")]
    MessageAttemptsExhausted,
    /// An optional Mic-E telemetry block had the wrong width or invalid hex.
    #[error("invalid Mic-E telemetry: {0}")]
    InvalidMiceTelemetry(#[source] crate::MiceTelemetryError),
    /// Text exceeds the destination and definition's complete body limit.
    #[error("APRS message text is {actual} bytes; maximum is {maximum}")]
    MessageTooLong {
        /// Observed text bytes, excluding message-ID trailers.
        actual: usize,
        /// Body limit selected by the destination and definition grammar.
        maximum: usize,
    },

    /// A textual wire field contained a byte outside seven-bit ASCII.
    #[error("{field} contains non-ASCII byte {byte:#04X} at byte index {index}")]
    InvalidTextByte {
        /// Name of the wire field being decoded.
        field: AprsWireTextField,
        /// Zero-based byte index within that field.
        index: usize,
        /// Byte that cannot be represented by the APRS text field.
        byte: u8,
    },

    // --- Validation variants for wire newtypes ---
    /// Latitude is not finite or outside `-90.0..=90.0`.
    #[error("invalid latitude: {0}")]
    InvalidLatitude(#[source] crate::CoordinateError),

    /// Longitude is not finite or outside `-180.0..=180.0`.
    #[error("invalid longitude: {0}")]
    InvalidLongitude(#[source] crate::CoordinateError),

    /// Speed value is out of range.
    #[error("invalid speed: {0}")]
    InvalidSpeed(#[source] crate::SpeedError),

    /// A `SmartBeaconing` configuration violates an algorithm invariant.
    #[error("invalid SmartBeaconing configuration: {0}")]
    InvalidSmartBeaconingConfig(#[source] crate::SmartBeaconingConfigError),

    /// Course is outside `0..=360` degrees.
    #[error("invalid course: {0}")]
    InvalidCourse(#[source] crate::CourseError),

    /// Message ID is empty, too long, or contains non-alphanumeric bytes.
    #[error("invalid message ID: {0}")]
    InvalidMessageId(#[source] crate::MessageIdError),

    /// Temperature (Fahrenheit) is outside `-99..=999`.
    #[error("invalid temperature: {0}")]
    InvalidTemperature(#[source] crate::TemperatureError),

    /// Symbol table code is not `/`, `\`, `0-9`, or `A-Z`.
    #[error("invalid symbol table: {0}")]
    InvalidSymbolTable(#[source] crate::SymbolTableError),

    /// APRS symbol code is outside the printable ASCII range.
    #[error("invalid APRS symbol: {0}")]
    InvalidSymbol(#[source] crate::SymbolError),

    /// Tocall string does not satisfy callsign format rules.
    #[error("invalid tocall: {0}")]
    InvalidTocall(#[source] ax25_codec::Ax25Error),

    /// Digipeater alias base failed AX.25 callsign validation.
    #[error("invalid digipeater alias: {0}")]
    InvalidDigipeaterAlias(#[source] crate::DigipeaterAliasError),

    /// Object name does not satisfy the APRS 1.0.1 §11 p.58 rule of
    /// "fixed 9-character" identifier (≤ 9 bytes of printable ASCII).
    #[error("invalid APRS object name: {0}")]
    InvalidObjectName(#[source] crate::AprsTextError),

    /// A report or positionless-weather timestamp failed shape or range
    /// validation.
    #[error("invalid APRS timestamp: {0}")]
    InvalidTimestamp(#[source] crate::TimestampError),
}
