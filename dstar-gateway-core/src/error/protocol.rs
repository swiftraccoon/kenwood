//! `ProtocolError` umbrella for per-protocol structured errors.

use crate::codec::dcs::error::DcsError;
use crate::codec::dextra::error::DExtraError as DExtraCodecError;
use crate::codec::dplus::error::DPlusError;

/// Per-protocol structured errors.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolError {
    /// `DPlus` (REF) protocol error with structured cause.
    #[error(transparent)]
    DPlus(#[from] DPlusError),

    /// `DExtra` (XRF/XLX) protocol error with structured cause.
    #[error(transparent)]
    DExtra(#[from] DExtraCodecError),

    /// `DCS` protocol error with structured cause.
    #[error(transparent)]
    Dcs(#[from] DcsError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_error_dplus_display() {
        let inner = DPlusError::StreamIdZero;
        let err = ProtocolError::DPlus(inner);
        assert!(err.to_string().contains("DPlus stream id is zero"));
    }

    #[test]
    fn protocol_error_dextra_display() {
        let inner = DExtraCodecError::StreamIdZero;
        let err = ProtocolError::DExtra(inner);
        assert!(err.to_string().contains("DExtra stream id is zero"));
    }

    #[test]
    fn protocol_error_dcs_display() {
        let inner = DcsError::StreamIdZero;
        let err = ProtocolError::Dcs(inner);
        assert!(err.to_string().contains("DCS stream id is zero"));
    }

    #[test]
    fn from_dplus_error_composes_into_protocol_error() {
        let inner = DPlusError::UnknownPacketLength { got: 42 };
        let outer: ProtocolError = inner.into();
        assert!(matches!(
            outer,
            ProtocolError::DPlus(DPlusError::UnknownPacketLength { got: 42 })
        ));
    }

    #[test]
    fn from_dextra_error_composes_into_protocol_error() {
        let inner = DExtraCodecError::UnknownPacketLength { got: 42 };
        let outer: ProtocolError = inner.into();
        assert!(matches!(
            outer,
            ProtocolError::DExtra(DExtraCodecError::UnknownPacketLength { got: 42 })
        ));
    }

    #[test]
    fn from_dcs_error_composes_into_protocol_error() {
        let inner = DcsError::UnknownPacketLength { got: 42 };
        let outer: ProtocolError = inner.into();
        assert!(matches!(
            outer,
            ProtocolError::Dcs(DcsError::UnknownPacketLength { got: 42 })
        ));
    }

    #[test]
    fn from_dplus_error_composes_into_top_level_error() {
        let inner = DPlusError::StreamIdZero;
        let top: crate::error::Error = ProtocolError::DPlus(inner).into();
        assert!(matches!(
            top,
            crate::error::Error::Protocol(ProtocolError::DPlus(DPlusError::StreamIdZero))
        ));
    }

    #[test]
    fn from_dcs_error_composes_into_top_level_error() {
        let inner = DcsError::StreamIdZero;
        let top: crate::error::Error = ProtocolError::Dcs(inner).into();
        assert!(matches!(
            top,
            crate::error::Error::Protocol(ProtocolError::Dcs(DcsError::StreamIdZero))
        ));
    }
}
