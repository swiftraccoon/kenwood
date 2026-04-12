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

    /// Callsign field fails `Callsign::try_from_str`.
    #[error("DPRS sentence callsign field is invalid: {reason}")]
    InvalidCallsign {
        /// Why the callsign failed.
        reason: &'static str,
    },

    /// CRC mismatch — computed vs on-wire disagree.
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
