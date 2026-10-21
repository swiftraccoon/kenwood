//! DPRS parser/encoder errors.

/// DPRS sentence parser errors.
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
#[non_exhaustive]
pub enum DprsError {
    /// Sentence does not start with `$$CRC`.
    #[error("DPRS sentence does not start with $$CRC")]
    MissingCrcPrefix,

    /// Sentence is shorter than the minimum viable length.
    #[error("DPRS sentence too short: {got} bytes")]
    TooShort {
        /// Observed length.
        got: usize,
    },

    /// Lat/lon field couldn't be parsed.
    #[error("DPRS sentence has malformed lat/lon field")]
    MalformedCoordinates,

    /// Latitude out of range `-90.0..=90.0` or NaN.
    #[error("latitude {got} out of range -90..=90")]
    LatitudeOutOfRange {
        /// Offending value.
        got: f64,
    },

    /// Longitude out of range `-180.0..=180.0` or NaN.
    #[error("longitude {got} out of range -180..=180")]
    LongitudeOutOfRange {
        /// Offending value.
        got: f64,
    },

    /// Source identifier fails its checked text grammar.
    #[error(transparent)]
    InvalidSource(#[from] super::source::DprsSourceError),
    /// The four hexadecimal CRC digits or following comma are malformed.
    #[error("DPRS checksum requires four hexadecimal digits followed by a comma")]
    MalformedCrcField,
    /// The source/destination separator, route, or position delimiter is malformed.
    #[error("DPRS TNC2 header or uncompressed position delimiter is malformed")]
    MalformedHeader,
    /// The sentence exceeds the implementation's bounded receive policy.
    #[error("DPRS sentence has {got} bytes including CR; maximum is {maximum}")]
    SentenceTooLong {
        /// Observed or reconstructed CR-terminated length.
        got: usize,
        /// Implementation resource limit, not a universal DPRS wire maximum.
        maximum: usize,
    },
    /// Table selector is not primary, alternate, or an uncompressed overlay.
    #[error("invalid APRS symbol table selector {got:?}")]
    InvalidSymbolTable {
        /// Rejected character.
        got: char,
    },
    /// Overlay is not a decimal digit or uppercase ASCII letter.
    #[error("invalid uncompressed APRS overlay {got:?}")]
    InvalidSymbolOverlay {
        /// Rejected character.
        got: char,
    },
    /// Symbol code is outside printable non-space ASCII.
    #[error("invalid APRS symbol code {got:?}")]
    InvalidSymbolCode {
        /// Rejected character.
        got: char,
    },
    /// Uncompressed position comment exceeds its 43-byte field.
    #[error("APRS position comment has {got} bytes; maximum is 43")]
    CommentTooLong {
        /// Supplied byte length.
        got: usize,
    },
    /// Plain comment contains a control, non-ASCII, or reserved TNC switch byte.
    #[error("APRS position comment byte {index} is invalid: 0x{byte:02X}")]
    InvalidCommentByte {
        /// Zero-based byte index.
        index: usize,
        /// Rejected original byte.
        byte: u8,
    },

    /// CRC mismatch: computed vs on-wire disagree.
    #[error("DPRS $$CRC mismatch: computed 0x{computed:04X}, on-wire 0x{on_wire:04X}")]
    CrcMismatch {
        /// Computed CRC.
        computed: u16,
        /// On-wire CRC.
        on_wire: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_prefix_display() {
        let err = DprsError::MissingCrcPrefix;
        assert!(err.to_string().contains("$$CRC"));
    }

    #[test]
    fn latitude_out_of_range_display() {
        let err = DprsError::LatitudeOutOfRange { got: 91.0 };
        assert!(err.to_string().contains("91"));
    }

    #[test]
    fn longitude_out_of_range_display() {
        let err = DprsError::LongitudeOutOfRange { got: -181.5 };
        assert!(err.to_string().contains("-181"));
    }

    #[test]
    fn crc_mismatch_display() {
        let err = DprsError::CrcMismatch {
            computed: 0x1234,
            on_wire: 0x5678,
        };
        let s = err.to_string();
        assert!(s.contains("1234"));
        assert!(s.contains("5678"));
    }
}
