// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Immutable remembered connection targets, validated before persistence or use.

use std::error::Error;
use std::fmt;
use std::num::{NonZeroU16, ParseIntError};

use dstar_gateway_core::hosts::{HostAddress, HostAddressError};
use dstar_gateway_core::types::{DcsReflector, Module, ProtocolKind, RoutingCallsign, TypeError};

/// Named form candidates for a remembered target.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SavedHostFields<'a> {
    pub(crate) callsign: &'a str,
    pub(crate) host: &'a str,
    pub(crate) port: &'a str,
    pub(crate) protocol: &'a str,
    pub(crate) module: char,
}

/// A remembered connection with checked identity, endpoint, protocol and module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedHost {
    callsign: RoutingCallsign,
    host: HostAddress,
    port: NonZeroU16,
    protocol: ProtocolKind,
    module: Module,
}

impl SavedHost {
    /// Validate all candidates without retaining partial or repaired values.
    pub(crate) fn new(fields: SavedHostFields<'_>) -> Result<Self, SavedHostError> {
        if fields.callsign.contains('|') {
            return Err(SavedHostError::Delimiter);
        }
        let callsign = RoutingCallsign::new(fields.callsign).map_err(SavedHostError::Identity)?;
        let host = HostAddress::new(fields.host).map_err(SavedHostError::Host)?;
        let port = fields.port.parse().map_err(SavedHostError::Port)?;
        let protocol = match fields.protocol {
            "DExtra" => ProtocolKind::DExtra,
            "DPlus" => ProtocolKind::DPlus,
            "Dcs" | "DCS" => {
                let _reflector =
                    DcsReflector::try_from_routing(callsign).map_err(SavedHostError::Identity)?;
                ProtocolKind::Dcs
            }
            _ => return Err(SavedHostError::Protocol),
        };
        let module = Module::try_from_char(fields.module).map_err(SavedHostError::Identity)?;
        Ok(Self {
            callsign,
            host,
            port,
            protocol,
            module,
        })
    }

    pub(crate) const fn callsign(&self) -> RoutingCallsign {
        self.callsign
    }
    pub(crate) const fn host(&self) -> &HostAddress {
        &self.host
    }
    pub(crate) const fn port(&self) -> NonZeroU16 {
        self.port
    }
    pub(crate) const fn protocol(&self) -> ProtocolKind {
        self.protocol
    }
    pub(crate) const fn module(&self) -> Module {
        self.module
    }

    /// The application's persisted token, independent of protocol debug output.
    pub(crate) fn protocol_name(&self) -> &'static str {
        match self.protocol {
            ProtocolKind::DExtra => "DExtra",
            ProtocolKind::DPlus => "DPlus",
            ProtocolKind::Dcs => "Dcs",
            _ => unreachable!("construction accepts exactly three persisted protocol tokens"),
        }
    }
}

/// A remembered connection candidate rejected before it can fill the form.
#[derive(Debug)]
pub(crate) enum SavedHostError {
    Delimiter,
    Identity(TypeError),
    Host(HostAddressError),
    Port(ParseIntError),
    Protocol,
}

impl fmt::Display for SavedHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Delimiter => f.write_str("saved target callsign cannot contain the | delimiter"),
            Self::Identity(error) => write!(f, "saved target identity: {error}"),
            Self::Host(error) => write!(f, "saved target address: {error}"),
            Self::Port(error) => write!(f, "saved target nonzero UDP port: {error}"),
            Self::Protocol => f.write_str("saved target protocol must be DExtra, DPlus or Dcs"),
        }
    }
}

impl Error for SavedHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Identity(error) => Some(error),
            Self::Host(error) => Some(error),
            Self::Port(error) => Some(error),
            Self::Protocol | Self::Delimiter => None,
        }
    }
}

/// Checked fields cannot contain the delimiter in this five-field grammar.
pub(super) fn encode_saved(value: &SavedHost) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        value.callsign,
        value.host,
        value.port,
        value.protocol_name(),
        value.module.as_char()
    )
}

/// Reject shape mismatches, extra module characters and invalid domain values.
pub(super) fn decode_saved(input: &str) -> Option<SavedHost> {
    let mut fields = input.split('|');
    let callsign = fields.next()?;
    let host = fields.next()?;
    let port = fields.next()?;
    let protocol = fields.next()?;
    let mut module = fields.next()?.chars();
    let module_letter = module.next()?;
    if module.next().is_some() || fields.next().is_some() {
        return None;
    }
    SavedHost::new(SavedHostFields {
        callsign,
        host,
        port,
        protocol,
        module: module_letter,
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::{decode_saved, encode_saved};

    #[test]
    fn restore_rejects_delimiters_invalid_domains_and_partial_modules()
    -> Result<(), Box<dyn std::error::Error>> {
        let valid = "REF030|[::1]|20001|DPlus|C";
        let host = decode_saved(valid).ok_or("valid target was rejected")?;
        assert!(
            super::SavedHost::new(super::SavedHostFields {
                callsign: "A|B",
                host: "localhost",
                port: "20001",
                protocol: "DPlus",
                module: 'C'
            })
            .is_err(),
            "constructor accepted the persistence delimiter"
        );
        assert!(
            decode_saved(&encode_saved(&host)).is_some(),
            "checked target did not round-trip"
        );
        for invalid in [
            "REF030|bad|host|20001|DPlus|C",
            "REF030|host.example|0|DPlus|C",
            "REF030|https://example.org|20001|DPlus|C",
            "REF030|host.example|20001|DPlus|CC",
            "REF030|host.example|20001|unknown|C",
            "POLARIS|host.example|30051|Dcs|C",
            "REF030|host.example|20001|DPlus|!",
        ] {
            assert!(
                decode_saved(invalid).is_none(),
                "invalid target restored: {invalid}"
            );
        }
        Ok(())
    }
}
