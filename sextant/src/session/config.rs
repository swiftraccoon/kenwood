// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Checked form-to-operation configuration, before resolution or socket I/O.

use std::error::Error;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;

use dstar_gateway_core::hosts::HostAddress;
use dstar_gateway_core::types::{DcsReflector, Module, ProtocolKind, RoutingCallsign, TypeError};

/// A checked host and nonzero UDP port; construction performs no resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    host: HostAddress,
    port: NonZeroU16,
}

impl Endpoint {
    pub(crate) const fn new(host: HostAddress, port: NonZeroU16) -> Self {
        Self { host, port }
    }

    pub(super) async fn resolve(&self) -> Result<SocketAddr, super::ConnectFailure> {
        if let Some(ip) = self.host.ip() {
            return Ok(SocketAddr::new(ip, self.port.get()));
        }
        let name = self
            .host
            .dns_name()
            .ok_or(super::ConnectFailure::NoAddress)?;
        tokio::net::lookup_host((name, self.port.get()))
            .await
            .map_err(super::ConnectFailure::Resolve)?
            .next()
            .ok_or(super::ConnectFailure::NoAddress)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.host.ip() {
            Some(ip) => SocketAddr::new(ip, self.port.get()).fmt(f),
            None => write!(f, "{}:{}", self.host, self.port),
        }
    }
}

/// An unspecified local address in the resolved peer's IP family.
pub(super) const fn local_bind_address(peer: SocketAddr) -> SocketAddr {
    let ip = match peer.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    };
    SocketAddr::new(ip, 0)
}

/// Named, checked routing identity shared by all supported protocols.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConnectionIdentity {
    pub(crate) operator: RoutingCallsign,
    pub(crate) local_module: Module,
    pub(crate) reflector: RoutingCallsign,
    pub(crate) reflector_module: Module,
}

/// The selected protocol retains every identity it requires for construction.
#[derive(Debug, Clone, Copy)]
pub(super) enum ConnectionProtocol {
    DExtra,
    DPlus,
    Dcs(DcsReflector),
}

/// Immutable connection intent, accepted only after complete form validation.
#[derive(Debug, Clone)]
pub(crate) struct ConnectConfig {
    pub(super) protocol: ConnectionProtocol,
    pub(super) callsign: RoutingCallsign,
    pub(super) local_module: Module,
    pub(super) reflector_callsign: RoutingCallsign,
    pub(super) reflector_module: Module,
    pub(super) endpoint: Endpoint,
    pub(super) reconnect_on_drop: bool,
}

impl ConnectConfig {
    pub(crate) fn new(
        protocol: ProtocolKind,
        identity: ConnectionIdentity,
        endpoint: Endpoint,
        reconnect_on_drop: bool,
    ) -> Result<Self, ConfigError> {
        let protocol = match protocol {
            ProtocolKind::DExtra => ConnectionProtocol::DExtra,
            ProtocolKind::DPlus => ConnectionProtocol::DPlus,
            ProtocolKind::Dcs => ConnectionProtocol::Dcs(
                DcsReflector::try_from_routing(identity.reflector)
                    .map_err(ConfigError::Reflector)?,
            ),
            other => return Err(ConfigError::UnsupportedProtocol(other)),
        };
        Ok(Self {
            protocol,
            callsign: identity.operator,
            local_module: identity.local_module,
            reflector_callsign: identity.reflector,
            reflector_module: identity.reflector_module,
            endpoint,
            reconnect_on_drop,
        })
    }

    pub(crate) const fn callsign(&self) -> RoutingCallsign {
        self.callsign
    }
}

/// Protocol selection or DCS routing identity rejected before any I/O.
#[derive(Debug)]
pub(crate) enum ConfigError {
    UnsupportedProtocol(ProtocolKind),
    Reflector(TypeError),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProtocol(protocol) => {
                write!(f, "unsupported reflector protocol {protocol:?}")
            }
            Self::Reflector(error) => write!(f, "DCS reflector identity: {error}"),
        }
    }
}

impl Error for ConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Reflector(error) => Some(error),
            Self::UnsupportedProtocol(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectConfig, ConnectionIdentity, Endpoint, local_bind_address};
    use dstar_gateway_core::hosts::HostAddress;
    use dstar_gateway_core::types::{Module, ProtocolKind, RoutingCallsign};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[tokio::test]
    async fn endpoints_resolve_names_and_preserve_ip_family() -> TestResult {
        let port = std::num::NonZeroU16::new(30001).ok_or("nonzero port")?;
        for host in ["127.0.0.1", "[::1]", "localhost"] {
            let endpoint = Endpoint::new(HostAddress::new(host)?, port);
            let resolved = endpoint.resolve().await?;
            assert!(
                resolved.ip().is_loopback(),
                "loopback endpoint changed address: {resolved}"
            );
            assert_eq!(resolved.port(), 30001, "resolution changed port");
            assert_eq!(
                local_bind_address(resolved).is_ipv6(),
                resolved.is_ipv6(),
                "IPv6 peer used IPv4 bind"
            );
        }
        Ok(())
    }

    #[test]
    fn dcs_identity_is_checked_before_connection() -> TestResult {
        let identity = ConnectionIdentity {
            operator: RoutingCallsign::new("W1AW")?,
            local_module: Module::A,
            reflector: RoutingCallsign::new("POLARIS")?,
            reflector_module: Module::B,
        };
        let endpoint = Endpoint::new(
            HostAddress::new("127.0.0.1")?,
            std::num::NonZeroU16::new(30051).ok_or("nonzero port")?,
        );
        assert!(
            ConnectConfig::new(ProtocolKind::Dcs, identity, endpoint, false).is_err(),
            "arbitrary reflector identity became DCS configuration"
        );
        Ok(())
    }
}
