//! Extraction failure type shared by every phase of the extractor.

use std::fmt;

/// Raised when the expected serializer shape cannot be extracted.
///
/// The extractor is deliberately narrow: any deviation from the reviewed
/// decompilation shape fails extraction with a message describing the
/// unexpected input instead of silently omitting a writer.
#[derive(Debug, thiserror::Error)]
pub struct ExtractError(String);

impl ExtractError {
    /// Build an error from any displayable message.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ExtractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<std::io::Error> for ExtractError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

/// Extraction result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, ExtractError>;

/// Shorthand constructor mirroring the Python `raise ExtractError(f"...")`.
macro_rules! extract_error {
    ($($arg:tt)*) => {
        $crate::error::ExtractError::new(format!($($arg)*))
    };
}
pub(crate) use extract_error;
