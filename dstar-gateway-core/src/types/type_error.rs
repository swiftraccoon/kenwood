//! Validation errors for type construction.

/// Validation errors for `dstar-gateway-core` strong types.
///
/// All `try_from_*` constructors on the typed primitives in
/// [`crate::types`] return this error on rejection.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TypeError {
    /// The supplied reflector identity lacks the DCS protocol prefix.
    #[error("DCS reflector identity must begin with DCS")]
    InvalidDcsReflector,
    /// A routing prefix cannot fit its seven-byte printable field.
    #[error("routing callsign must contain one through seven printable ASCII bytes")]
    InvalidRoutingCallsign,
    /// A transmit index includes flags or lies outside the 21-frame domain.
    #[error("D-STAR frame index {got} is outside 0..=20")]
    InvalidFrameSequence {
        /// The rejected raw value.
        got: u8,
    },
    /// Supplied module character is not ASCII A-Z.
    #[error("invalid module letter '{got}' (must be ASCII A-Z)")]
    InvalidModule {
        /// The rejected character.
        got: char,
    },
    /// Text for a module does not contain exactly one ASCII byte.
    #[error("module text has {got} bytes; exactly one ASCII letter is required")]
    InvalidModuleLength {
        /// Supplied byte length.
        got: usize,
    },

    /// Supplied callsign is empty, too long, or contains non-ASCII.
    #[error("invalid callsign: {reason}")]
    InvalidCallsign {
        /// Typed text-field rejection.
        #[source]
        reason: TextFieldError,
    },

    /// Supplied suffix is too long, or contains non-ASCII.
    #[error("invalid suffix: {reason}")]
    InvalidSuffix {
        /// Typed text-field rejection.
        #[source]
        reason: TextFieldError,
    },

    /// Supplied reflector callsign does not match a known protocol prefix.
    #[error("reflector callsign must begin with REF, XRF, XLX or DCS")]
    InvalidReflectorCallsign,

    /// Supplied band letter is not A, B, C, or D.
    #[error("invalid band letter '{got}' (must be A, B, C, or D)")]
    InvalidBandLetter {
        /// The rejected character.
        got: char,
    },
}

/// Rejection of a printable fixed-width text field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum TextFieldError {
    /// Removing trailing padding left an empty required field.
    #[error("required text field is empty")]
    Empty,
    /// Text exceeds the field width after trailing padding is removed.
    #[error("text has {got} bytes; maximum is {max}")]
    TooLong {
        /// Supplied unpadded byte length.
        got: usize,
        /// Maximum field width in bytes.
        max: usize,
    },
    /// Text contains a byte outside ASCII space through tilde.
    #[error("byte {byte:#04X} at offset {offset} is not printable ASCII")]
    NonPrintable {
        /// Zero-based byte offset.
        offset: usize,
        /// Rejected byte.
        byte: u8,
    },
}
