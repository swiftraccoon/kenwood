// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Capability bitfields reported in the `GetVersion` response.
//!
//! Mirrors `m_capabilities1` and `m_capabilities2` in
//! `MMDVMHost/Modem.cpp`. The host supplies legacy defaults for protocol 1;
//! protocol 2 reports the two capability bytes on the wire.
//! See the [version example](crate#parse-version-and-status-responses).

use std::fmt;

use crate::command::{CAP1_DMR, CAP1_DSTAR, CAP1_FM, CAP1_NXDN, CAP1_P25, CAP1_YSF, CAP2_POCSAG};

/// Capability bitfields returned in the `GetVersion` response.
///
/// Unknown bits are retained. Numeric formatting treats the first wire byte
/// as the low eight bits and the second as the high eight bits.
/// See the [version example](crate#parse-version-and-status-responses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Capabilities {
    bytes: [u8; 2],
}

impl Capabilities {
    /// Construct an empty capability set.
    #[must_use]
    pub const fn new() -> Self {
        Self { bytes: [0; 2] }
    }

    /// Preserve both reported bytes, including unrecognized capability bits.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self { bytes }
    }

    /// Return the first and second capability bytes in wire order.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 2] {
        self.bytes
    }

    /// Return the bitset with the first capability byte in the low eight bits.
    #[must_use]
    pub const fn bits(self) -> u16 {
        u16::from_le_bytes(self.bytes)
    }

    /// Whether the D-STAR capability bit is set.
    #[must_use]
    pub const fn has_dstar(self) -> bool {
        (self.bytes[0] & CAP1_DSTAR) != 0
    }

    /// Whether the DMR capability bit is set.
    #[must_use]
    pub const fn has_dmr(self) -> bool {
        (self.bytes[0] & CAP1_DMR) != 0
    }

    /// Whether the Yaesu System Fusion capability bit is set.
    #[must_use]
    pub const fn has_ysf(self) -> bool {
        (self.bytes[0] & CAP1_YSF) != 0
    }

    /// Whether the P25 capability bit is set.
    #[must_use]
    pub const fn has_p25(self) -> bool {
        (self.bytes[0] & CAP1_P25) != 0
    }

    /// Whether the NXDN capability bit is set.
    #[must_use]
    pub const fn has_nxdn(self) -> bool {
        (self.bytes[0] & CAP1_NXDN) != 0
    }

    /// Whether the analog FM capability bit is set.
    #[must_use]
    pub const fn has_fm(self) -> bool {
        (self.bytes[0] & CAP1_FM) != 0
    }

    /// Whether the POCSAG paging capability bit is set.
    #[must_use]
    pub const fn has_pocsag(self) -> bool {
        (self.bytes[1] & CAP2_POCSAG) != 0
    }
}

impl From<[u8; 2]> for Capabilities {
    fn from(bytes: [u8; 2]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl From<Capabilities> for [u8; 2] {
    fn from(capabilities: Capabilities) -> Self {
        capabilities.to_bytes()
    }
}

impl fmt::LowerHex for Capabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.bits(), formatter)
    }
}
impl fmt::UpperHex for Capabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.bits(), formatter)
    }
}
impl fmt::Octal for Capabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Octal::fmt(&self.bits(), formatter)
    }
}
impl fmt::Binary for Capabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Binary::fmt(&self.bits(), formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_flags_clear_when_zero() {
        let c = Capabilities::new();
        assert!(!c.has_dstar());
        assert!(!c.has_dmr());
        assert!(!c.has_ysf());
        assert!(!c.has_p25());
        assert!(!c.has_nxdn());
        assert!(!c.has_fm());
        assert!(!c.has_pocsag());
    }

    #[test]
    fn single_flag_detection() {
        assert!(Capabilities::from_bytes([CAP1_DSTAR, 0]).has_dstar());
        assert!(Capabilities::from_bytes([CAP1_DMR, 0]).has_dmr());
        assert!(Capabilities::from_bytes([CAP1_YSF, 0]).has_ysf());
        assert!(Capabilities::from_bytes([CAP1_P25, 0]).has_p25());
        assert!(Capabilities::from_bytes([CAP1_NXDN, 0]).has_nxdn());
        assert!(Capabilities::from_bytes([CAP1_FM, 0]).has_fm());
        assert!(Capabilities::from_bytes([0, CAP2_POCSAG]).has_pocsag());
    }

    #[test]
    fn full_fleet_all_flags() {
        let c = Capabilities::from_bytes([
            CAP1_DSTAR | CAP1_DMR | CAP1_YSF | CAP1_P25 | CAP1_NXDN | CAP1_FM,
            CAP2_POCSAG,
        ]);
        assert!(c.has_dstar());
        assert!(c.has_dmr());
        assert!(c.has_ysf());
        assert!(c.has_p25());
        assert!(c.has_nxdn());
        assert!(c.has_fm());
        assert!(c.has_pocsag());
    }

    #[test]
    fn protocol_v1_canonical_caps() {
        // MMDVMHost/Modem.cpp:2027: v1 always advertises this set.
        let c = Capabilities::from_bytes([
            CAP1_DSTAR | CAP1_DMR | CAP1_YSF | CAP1_P25 | CAP1_NXDN,
            CAP2_POCSAG,
        ]);
        assert!(c.has_dstar());
        assert!(!c.has_fm(), "v1 never advertises FM");
        assert!(c.has_pocsag());
    }
}
