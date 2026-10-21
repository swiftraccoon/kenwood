//! Plain-text comments following an uncompressed APRS position.

use std::fmt;
use std::str::FromStr;

use super::DprsError;

/// Maximum bytes after an uncompressed position's symbol code.
///
/// APRS 1.0.1 chapter 8 specifies 43 characters; this plain-text representation
/// admits printable ASCII except the reserved TNC switch characters `|` and `~`.
pub const MAX_COMMENT_BYTES: usize = 43;

/// Zero to 43 bytes of plain APRS position comment text.
///
/// This value retains spaces and punctuation exactly. Binary extensions and
/// embedded telemetry are outside its plain-text grammar.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct DprsComment(String);

impl DprsComment {
    /// Empty comment, producing no bytes after the symbol code.
    pub const EMPTY: Self = Self(String::new());

    /// Validate comment width and every original byte before copying it.
    ///
    /// # Errors
    ///
    /// Returns [`DprsError::CommentTooLong`] above 43 bytes, or
    /// [`DprsError::InvalidCommentByte`] for controls, non-ASCII, `|` or `~`.
    pub fn new(value: &str) -> Result<Self, DprsError> {
        if value.len() > MAX_COMMENT_BYTES {
            return Err(DprsError::CommentTooLong { got: value.len() });
        }
        if let Some((index, byte)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !(b' '..=b'~').contains(byte) || matches!(byte, b'|' | b'~'))
        {
            return Err(DprsError::InvalidCommentByte { index, byte });
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrow the exact validated comment, including meaningful spaces.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for DprsComment {
    type Err = DprsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for DprsComment {
    type Error = DprsError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for DprsComment {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<DprsComment> for String {
    fn from(value: DprsComment) -> Self {
        value.0
    }
}

impl fmt::Display for DprsComment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
