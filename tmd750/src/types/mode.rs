//! CAT band, operating-mode, and DV Gateway values.

use std::fmt;

use crate::error::ValidationError;

/// Receiver band used by band-indexed CAT commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Band {
    /// Band A (`0`).
    A = 0,
    /// Band B (`1`).
    B = 1,
}

impl fmt::Display for Band {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::A => formatter.write_str("A"),
            Self::B => formatter.write_str("B"),
        }
    }
}

impl TryFrom<u8> for Band {
    type Error = ValidationError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::A),
            1 => Ok(Self::B),
            _ => Err(ValidationError::InvalidBand { value }),
        }
    }
}

impl From<Band> for u8 {
    fn from(value: Band) -> Self {
        value as Self
    }
}

/// Operating mode reported by the TM-D750 `MD` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperatingMode {
    /// Analog FM (`0`).
    Fm,
    /// D-STAR Digital Voice (`1`).
    Dv,
    /// A wire value not yet assigned a qualified meaning.
    Unqualified(u8),
}

impl fmt::Display for OperatingMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fm => formatter.write_str("FM"),
            Self::Dv => formatter.write_str("DV"),
            Self::Unqualified(value) => write!(formatter, "unqualified value {value}"),
        }
    }
}

impl From<u8> for OperatingMode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Fm,
            1 => Self::Dv,
            other => Self::Unqualified(other),
        }
    }
}

impl From<OperatingMode> for u8 {
    fn from(value: OperatingMode) -> Self {
        match value {
            OperatingMode::Fm => 0,
            OperatingMode::Dv => 1,
            OperatingMode::Unqualified(raw) => raw,
        }
    }
}

/// Operating modes proven selectable through `MD` writes.
///
/// A live Band A `MD 0,7` write was rejected. Enter DR from the radio's
/// digital controls; its read value has not yet been observed and qualified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectableMode {
    /// Analog FM.
    Fm,
    /// D-STAR Digital Voice.
    Dv,
}

impl From<SelectableMode> for OperatingMode {
    fn from(value: SelectableMode) -> Self {
        match value {
            SelectableMode::Fm => Self::Fm,
            SelectableMode::Dv => Self::Dv,
        }
    }
}

/// Persistent DV Gateway mode reported by the read-only `GW` command.
///
/// Off and Terminal were observed on firmware 1.02 through main-unit USB,
/// with the DV Gateway routed to panel USB. A named read value does not
/// qualify a command to select that mode or prove a reflector connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DvGatewayMode {
    /// DV Gateway is off (`0`).
    Off,
    /// Terminal Mode is selected (`2`).
    Terminal,
    /// A wire value not yet assigned a qualified meaning.
    Unqualified(u8),
}

impl fmt::Display for DvGatewayMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => formatter.write_str("Off"),
            Self::Terminal => formatter.write_str("Terminal"),
            Self::Unqualified(value) => write!(formatter, "unqualified value {value}"),
        }
    }
}

impl From<u8> for DvGatewayMode {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Off,
            2 => Self::Terminal,
            other => Self::Unqualified(other),
        }
    }
}

impl From<DvGatewayMode> for u8 {
    fn from(value: DvGatewayMode) -> Self {
        match value {
            DvGatewayMode::Off => 0,
            DvGatewayMode::Terminal => 2,
            DvGatewayMode::Unqualified(raw) => raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn wire_values_round_trip() -> TestResult {
        for (raw, band) in [(0, Band::A), (1, Band::B)] {
            assert_eq!(Band::try_from(raw)?, band);
            assert_eq!(u8::from(band), raw);
        }
        for (raw, mode) in [
            (0, OperatingMode::Fm),
            (1, OperatingMode::Dv),
            (7, OperatingMode::Unqualified(7)),
        ] {
            assert_eq!(OperatingMode::from(raw), mode);
            assert_eq!(u8::from(mode), raw);
        }
        Ok(())
    }

    #[test]
    fn gateway_wire_values_have_only_observed_names() {
        for (raw, mode, label) in [
            (0, DvGatewayMode::Off, "Off"),
            (2, DvGatewayMode::Terminal, "Terminal"),
            (1, DvGatewayMode::Unqualified(1), "unqualified value 1"),
            (3, DvGatewayMode::Unqualified(3), "unqualified value 3"),
            (
                u8::MAX,
                DvGatewayMode::Unqualified(u8::MAX),
                "unqualified value 255",
            ),
        ] {
            assert_eq!(
                DvGatewayMode::from(raw),
                mode,
                "only observed GW meanings receive named variants"
            );
            assert_eq!(u8::from(mode), raw, "GW evidence must retain its raw value");
            assert_eq!(
                mode.to_string(),
                label,
                "GW labels must match qualification"
            );
        }
    }

    #[test]
    fn every_gateway_wire_value_round_trips_losslessly() {
        for raw in 0..=u8::MAX {
            let mode = DvGatewayMode::from(raw);
            assert_eq!(u8::from(mode), raw, "raw GW {raw} must survive conversion");
            if raw != 0 && raw != 2 {
                assert_eq!(
                    mode,
                    DvGatewayMode::Unqualified(raw),
                    "unobserved GW {raw} must not acquire a named meaning"
                );
            }
        }
    }

    #[test]
    fn unqualified_wire_values_are_preserved() {
        assert!(Band::try_from(2).is_err());
        assert_eq!(OperatingMode::from(2), OperatingMode::Unqualified(2));
        assert_eq!(DvGatewayMode::from(3), DvGatewayMode::Unqualified(3));
    }
}
