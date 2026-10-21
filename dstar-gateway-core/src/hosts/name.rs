//! Case-insensitive identifiers used as the first host-file column.

use std::fmt;
use std::str::FromStr;

/// Nonempty printable-ASCII host-file token without `#`, canonicalized to uppercase.
///
/// This catalog identifier has no whitespace and is independent of fixed-width
/// on-air callsigns. A caller using a name in a filesystem path must separately
/// validate that path component. See the [host-file example](super::HostFile).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostName(String);

impl HostName {
    /// Validate an entire token and normalize ASCII letter case.
    ///
    /// # Errors
    ///
    /// Returns [`HostNameError::Empty`] for an empty name, or
    /// [`HostNameError::InvalidByte`] for `#`, whitespace, controls, or non-ASCII.
    pub fn new(value: &str) -> Result<Self, HostNameError> {
        if value.is_empty() {
            return Err(HostNameError::Empty);
        }
        if let Some((index, byte)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !byte.is_ascii_graphic() || *byte == b'#')
        {
            return Err(HostNameError::InvalidByte { index, byte });
        }
        Ok(Self(value.to_ascii_uppercase()))
    }

    /// Borrow the canonical catalog identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for HostName {
    type Err = HostNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for HostName {
    type Error = HostNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl AsRef<str> for HostName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<HostName> for String {
    fn from(value: HostName) -> Self {
        value.0
    }
}

impl fmt::Display for HostName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Host-file identifier rejected before collection insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HostNameError {
    /// No token was supplied.
    #[error("host name is empty")]
    Empty,
    /// A token byte is `#` or outside printable ASCII excluding space.
    #[error("host name byte {index} is not printable token ASCII: 0x{byte:02X}")]
    InvalidByte {
        /// Zero-based byte index.
        index: usize,
        /// Rejected original byte.
        byte: u8,
    },
}
