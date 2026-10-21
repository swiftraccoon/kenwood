//! Complete local identity for module-bearing reflector control packets.

use super::{Callsign, Module, RoutingCallsign};

/// Checked seven-byte station prefix and its separately encoded module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterIdentity {
    /// Checked station prefix, excluding its module.
    pub callsign: RoutingCallsign,
    /// Local station module.
    pub module: Module,
}

impl RepeaterIdentity {
    /// Eight-byte repeater field with the local module in its last byte.
    #[must_use]
    pub const fn to_callsign(self) -> Callsign {
        let [byte0, byte1, byte2, byte3, byte4, byte5, byte6] = *self.callsign.as_bytes();
        Callsign::from_wire_bytes([
            byte0,
            byte1,
            byte2,
            byte3,
            byte4,
            byte5,
            byte6,
            self.module.as_byte(),
        ])
    }
}

impl From<RepeaterIdentity> for Callsign {
    fn from(value: RepeaterIdentity) -> Self {
        value.to_callsign()
    }
}

impl TryFrom<Callsign> for RepeaterIdentity {
    type Error = super::TypeError;

    fn try_from(value: Callsign) -> Result<Self, Self::Error> {
        let [byte0, byte1, byte2, byte3, byte4, byte5, byte6, module] = *value.as_bytes();
        Ok(Self {
            callsign: RoutingCallsign::from_padded_bytes([
                byte0, byte1, byte2, byte3, byte4, byte5, byte6,
            ])?,
            module: Module::try_from(module)?,
        })
    }
}

/// An A-Z module or the empty module slot carried by a NAK/unlink reply.
///
/// `ircDDBGateway/Common/ConnectData.cpp::getDCSData` emits an empty NAK
/// target. `xlxd/src/cdcsprotocol.cpp::EncodeConnectNackPacket` emits either
/// the requested module or an empty disconnect target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModuleSlot {
    /// A selected module.
    Module(Module),
    /// Space byte, with no selected module.
    Empty,
}

impl ModuleSlot {
    /// The exact module letter or ASCII-space byte.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Module(module) => module.as_byte(),
            Self::Empty => b' ',
        }
    }
}

impl From<Module> for ModuleSlot {
    fn from(value: Module) -> Self {
        Self::Module(value)
    }
}

impl From<ModuleSlot> for u8 {
    fn from(value: ModuleSlot) -> Self {
        value.as_byte()
    }
}

impl TryFrom<u8> for ModuleSlot {
    type Error = super::TypeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == b' ' {
            Ok(Self::Empty)
        } else {
            Module::try_from(value).map(Self::Module)
        }
    }
}
