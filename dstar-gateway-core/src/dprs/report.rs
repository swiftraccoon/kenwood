//! Validated uncompressed DPRS position data and checksum selection.

use super::{DprsComment, DprsSource, DprsSymbol, Latitude, Longitude};

/// Named latitude and longitude, each validated before construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DprsPosition {
    /// Signed decimal degrees north of the equator, in `-90..=90`.
    pub latitude: Latitude,
    /// Signed decimal degrees east of Greenwich, in `-180..=180`.
    pub longitude: Longitude,
}

/// Validated source, uncompressed position, symbol and plain position comment.
///
/// Parsing and encoding are documented in the [DPRS workflow](super).
/// Route metadata is not retained; encoding uses `APDPRS,DSTAR*` and a CR
/// terminator. Coordinates are rounded to hundredths of a minute on encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct DprsReport {
    source: DprsSource,
    position: DprsPosition,
    symbol: DprsSymbol,
    comment: DprsComment,
}

impl DprsReport {
    /// Combine independently validated source, position, symbol and comment.
    #[must_use]
    pub const fn new(
        source: DprsSource,
        position: DprsPosition,
        symbol: DprsSymbol,
        comment: DprsComment,
    ) -> Self {
        Self {
            source,
            position,
            symbol,
            comment,
        }
    }

    /// Validated source identifier.
    #[must_use]
    pub const fn source(&self) -> &DprsSource {
        &self.source
    }

    /// Latitude in signed decimal degrees.
    #[must_use]
    pub const fn latitude(&self) -> Latitude {
        self.position.latitude
    }

    /// Longitude in signed decimal degrees.
    #[must_use]
    pub const fn longitude(&self) -> Longitude {
        self.position.longitude
    }

    /// Complete named position.
    #[must_use]
    pub const fn position(&self) -> DprsPosition {
        self.position
    }

    /// Validated table selector and symbol code.
    #[must_use]
    pub const fn symbol(&self) -> DprsSymbol {
        self.symbol
    }

    /// Plain position comment, retaining its exact printable bytes.
    #[must_use]
    pub const fn comment(&self) -> &DprsComment {
        &self.comment
    }
}

/// Checksum treatment for explicitly requested diagnostic parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChecksumPolicy {
    /// Reject a mismatch; this is the policy used by `FromStr`.
    Verify,
    /// Parse the checksum field's syntax but retain a report despite mismatch.
    ///
    /// This diagnostic option gives no checksum-integrity result. Normal receive
    /// callers should use `sentence.parse::<DprsReport>()`.
    Ignore,
}
