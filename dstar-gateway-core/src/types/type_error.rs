//! Validation errors for type construction.

/// Validation errors for `dstar-gateway-core` strong types.
///
/// All `try_from_*` constructors on the typed primitives in
/// [`crate::types`] return this error on rejection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TypeError {
    /// Supplied module character is not ASCII A-Z.
    #[error("invalid module letter '{got}' (must be ASCII A-Z)")]
    InvalidModule {
        /// The rejected character.
        got: char,
    },

    /// Supplied callsign is empty, too long, or contains non-ASCII.
    #[error("invalid callsign: {reason}")]
    InvalidCallsign {
        /// Human-readable reason for rejection.
        reason: &'static str,
    },

    /// Supplied suffix is too long, or contains non-ASCII.
    #[error("invalid suffix: {reason}")]
    InvalidSuffix {
        /// Human-readable reason for rejection.
        reason: &'static str,
    },

    /// Supplied reflector callsign does not match a known protocol prefix.
    #[error("invalid reflector callsign: {reason}")]
    InvalidReflectorCallsign {
        /// Human-readable reason for rejection.
        reason: &'static str,
    },

    /// Supplied band letter is not A, B, C, or D.
    #[error("invalid band letter '{got}' (must be A, B, C, or D)")]
    InvalidBandLetter {
        /// The rejected character.
        got: char,
    },
}
