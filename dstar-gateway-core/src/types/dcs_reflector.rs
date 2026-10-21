//! Checked target identity required by DCS control packets.

use std::fmt;
use std::str::FromStr;

use super::{Callsign, RoutingCallsign, TypeError};

/// Uppercase seven-byte reflector prefix beginning with `DCS`.
///
/// LINK, UNLINK and POLL packets embed this exact identity. Construction
/// validates its prefix and routing width; remote existence is determined
/// by the configured endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DcsReflector(RoutingCallsign);

impl DcsReflector {
    /// Validate the DCS prefix of an already checked routing field.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidDcsReflector`] without an uppercase `DCS` prefix.
    pub const fn try_from_routing(value: RoutingCallsign) -> Result<Self, TypeError> {
        if matches!(value.as_bytes(), [b'D', b'C', b'S', ..]) {
            Ok(Self(value))
        } else {
            Err(TypeError::InvalidDcsReflector)
        }
    }

    /// Validate the DCS prefix and canonicalize ASCII letters to uppercase.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidDcsReflector`] without the `DCS` prefix,
    /// or [`TypeError::InvalidRoutingCallsign`] for an invalid routing field.
    pub fn new(value: &str) -> Result<Self, TypeError> {
        let value = value.to_ascii_uppercase();
        Self::try_from_routing(RoutingCallsign::new(&value)?)
    }

    /// Padded eight-byte identity, with a space in its module slot.
    #[must_use]
    pub const fn callsign(self) -> Callsign {
        self.0.callsign()
    }

    /// Checked prefix for constructing repeater routing.
    #[must_use]
    pub const fn routing(self) -> RoutingCallsign {
        self.0
    }
}

impl FromStr for DcsReflector {
    type Err = TypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<&str> for DcsReflector {
    type Error = TypeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<Callsign> for DcsReflector {
    type Error = TypeError;

    fn try_from(value: Callsign) -> Result<Self, Self::Error> {
        Self::new(value.text().map_err(|_| TypeError::InvalidDcsReflector)?)
    }
}

impl TryFrom<RoutingCallsign> for DcsReflector {
    type Error = TypeError;

    fn try_from(value: RoutingCallsign) -> Result<Self, Self::Error> {
        Self::try_from_routing(value)
    }
}

impl From<DcsReflector> for Callsign {
    fn from(value: DcsReflector) -> Self {
        value.callsign()
    }
}

impl From<DcsReflector> for RoutingCallsign {
    fn from(value: DcsReflector) -> Self {
        value.routing()
    }
}

impl fmt::Display for DcsReflector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}
