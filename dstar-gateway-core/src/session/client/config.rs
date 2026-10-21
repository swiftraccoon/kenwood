//! Complete, protocol-specific client session configuration.

use std::net::SocketAddr;

use crate::types::{DcsReflector, Module, ProtocolKind, RoutingCallsign};

/// Client protocol and its required control-packet target metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClientProtocol {
    /// D-Plus, with target selection supplied by the UDP endpoint.
    DPlus,
    /// `DExtra`, with target selection supplied by the UDP endpoint.
    DExtra,
    /// DCS, whose control packets require the exact reflector identity.
    Dcs(DcsReflector),
}

impl ClientProtocol {
    /// Protocol discriminator without its configuration data.
    #[must_use]
    pub const fn kind(self) -> ProtocolKind {
        match self {
            Self::DPlus => ProtocolKind::DPlus,
            Self::DExtra => ProtocolKind::DExtra,
            Self::Dcs(_) => ProtocolKind::Dcs,
        }
    }
}

/// Named configuration for a direct [`super::SessionCore`] client.
///
/// All fields are supplied before construction. The typed [`super::Session`]
/// builder additionally fixes its protocol at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionConfig {
    /// Protocol with any required target identity.
    pub protocol: ClientProtocol,
    /// Local callsign prefix, with its eighth byte reserved for the module.
    pub callsign: RoutingCallsign,
    /// Local gateway module.
    pub local_module: Module,
    /// Requested reflector module.
    pub reflector_module: Module,
    /// Remote UDP endpoint.
    pub peer: SocketAddr,
}
