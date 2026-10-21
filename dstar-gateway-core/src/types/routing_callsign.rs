//! Checked seven-byte callsign prefix for repeater routing fields.

use std::fmt;
use std::str::FromStr;

use super::{Callsign, TypeError};

/// Printable, nonempty callsign occupying at most seven bytes plus padding.
///
/// A routing field reserves its eighth byte for a [`super::Module`].
/// Conversion from a received [`Callsign`] validates its text and rejects an
/// occupied eighth byte instead of discarding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoutingCallsign([u8; 7]);

impl RoutingCallsign {
    /// Validate exactly seven padded bytes without allocating.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidRoutingCallsign`] for all-space padding
    /// or any byte outside printable ASCII.
    pub const fn from_padded_bytes(bytes: [u8; 7]) -> Result<Self, TypeError> {
        let mut remaining = bytes.as_slice();
        let mut nonspace = false;
        while let Some((&byte, tail)) = remaining.split_first() {
            if byte < b' ' || byte > b'~' {
                return Err(TypeError::InvalidRoutingCallsign);
            }
            nonspace |= byte != b' ';
            remaining = tail;
        }
        if nonspace {
            Ok(Self(bytes))
        } else {
            Err(TypeError::InvalidRoutingCallsign)
        }
    }

    /// Validate and space-pad a routing callsign.
    ///
    /// # Errors
    ///
    /// Returns [`TypeError::InvalidRoutingCallsign`] for empty text, more than
    /// seven bytes after trailing padding removal, or nonprintable ASCII.
    pub fn new(value: &str) -> Result<Self, TypeError> {
        let text = value.trim_end_matches(' ');
        if text.is_empty()
            || text.len() > 7
            || !text.bytes().all(|byte| (b' '..=b'~').contains(&byte))
        {
            return Err(TypeError::InvalidRoutingCallsign);
        }
        let mut bytes = [b' '; 7];
        if let Some(destination) = bytes.get_mut(..text.len()) {
            destination.copy_from_slice(text.as_bytes());
        }
        Ok(Self(bytes))
    }

    /// Seven-byte padded representation, excluding the module slot.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 7] {
        &self.0
    }

    /// Complete eight-byte callsign with an unoccupied module slot.
    #[must_use]
    pub const fn callsign(self) -> Callsign {
        let [byte0, byte1, byte2, byte3, byte4, byte5, byte6] = self.0;
        Callsign::from_wire_bytes([byte0, byte1, byte2, byte3, byte4, byte5, byte6, b' '])
    }
}

impl TryFrom<Callsign> for RoutingCallsign {
    type Error = TypeError;

    fn try_from(value: Callsign) -> Result<Self, Self::Error> {
        let text = value
            .text()
            .map_err(|_| TypeError::InvalidRoutingCallsign)?;
        Self::new(text)
    }
}

impl TryFrom<&str> for RoutingCallsign {
    type Error = TypeError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl FromStr for RoutingCallsign {
    type Err = TypeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl From<RoutingCallsign> for Callsign {
    fn from(value: RoutingCallsign) -> Self {
        value.callsign()
    }
}

impl AsRef<[u8; 7]> for RoutingCallsign {
    fn as_ref(&self) -> &[u8; 7] {
        self.as_bytes()
    }
}

impl fmt::Display for RoutingCallsign {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.callsign(), formatter)
    }
}
