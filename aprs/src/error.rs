//! APRS protocol error type.

use thiserror::Error;

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
    #[error("Mic-E data requires destination address \u{2014} use parse_aprs_data_full()")]
    MicERequiresDestination,
    /// A digipeater path string could not be parsed.
    #[error("invalid digipeater path: {0}")]
    InvalidPath(String),
    /// The message text is too long (APRS 1.0.1 §14: max 67 characters).
    #[error("APRS message text exceeds 67 characters ({0} bytes)")]
    MessageTooLong(usize),

    // --- Validation variants for wire newtypes (Task 3) ---
    /// Latitude is not finite or outside `-90.0..=90.0`.
    #[error("invalid latitude: {0}")]
    InvalidLatitude(&'static str),

    /// Longitude is not finite or outside `-180.0..=180.0`.
    #[error("invalid longitude: {0}")]
    InvalidLongitude(&'static str),

    /// Speed value is out of range.
    #[error("invalid speed: {0}")]
    InvalidSpeed(&'static str),

    /// Course is outside `0..=360` degrees.
    #[error("invalid course: {0}")]
    InvalidCourse(&'static str),

    /// Message ID is empty, too long, or contains non-alphanumeric bytes.
    #[error("invalid message ID: {0}")]
    InvalidMessageId(&'static str),

    /// Temperature (Fahrenheit) is outside `-99..=999`.
    #[error("invalid temperature: {0}")]
    InvalidTemperature(&'static str),

    /// Symbol table code is not `/`, `\`, `0-9`, or `A-Z`.
    #[error("invalid symbol table: {0}")]
    InvalidSymbolTable(&'static str),

    /// APRS symbol code is outside the printable ASCII range.
    #[error("invalid APRS symbol: {0}")]
    InvalidSymbol(&'static str),

    /// Tocall string does not satisfy callsign format rules.
    #[error("invalid tocall: {0}")]
    InvalidTocall(&'static str),

    /// Digipeater alias failed validation (empty, non-ASCII).
    #[error("invalid digipeater alias: {0}")]
    InvalidDigipeaterAlias(&'static str),
}
