// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Capability bitfields reported in the `GetVersion` response.
//!
//! Mirrors `m_capabilities1` and `m_capabilities2` in
//! `ref/MMDVMHost/Modem.cpp`. Protocol version 1 firmware returns a
//! synthetic constant set (`CAP1_DSTAR|DMR|YSF|P25|NXDN` and
//! `CAP2_POCSAG`), while version 2 firmware reports the real bits.

use crate::command::{CAP1_DMR, CAP1_DSTAR, CAP1_FM, CAP1_NXDN, CAP1_P25, CAP1_YSF, CAP2_POCSAG};

/// Capability bitfields returned in the `GetVersion` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Primary capability byte (see `CAP1_*` constants).
    pub cap1: u8,
    /// Secondary capability byte (see `CAP2_*` constants).
    pub cap2: u8,
}

impl Capabilities {
    /// Build a new capability bitfield.
    #[must_use]
    pub const fn new(cap1: u8, cap2: u8) -> Self {
        Self { cap1, cap2 }
    }

    /// D-STAR capable?
    #[must_use]
    pub const fn has_dstar(self) -> bool {
        (self.cap1 & CAP1_DSTAR) != 0
    }

    /// DMR capable?
    #[must_use]
    pub const fn has_dmr(self) -> bool {
        (self.cap1 & CAP1_DMR) != 0
    }

    /// YSF capable?
    #[must_use]
    pub const fn has_ysf(self) -> bool {
        (self.cap1 & CAP1_YSF) != 0
    }

    /// P25 capable?
    #[must_use]
    pub const fn has_p25(self) -> bool {
        (self.cap1 & CAP1_P25) != 0
    }

    /// NXDN capable?
    #[must_use]
    pub const fn has_nxdn(self) -> bool {
        (self.cap1 & CAP1_NXDN) != 0
    }

    /// Analog FM capable?
    #[must_use]
    pub const fn has_fm(self) -> bool {
        (self.cap1 & CAP1_FM) != 0
    }

    /// POCSAG paging capable?
    #[must_use]
    pub const fn has_pocsag(self) -> bool {
        (self.cap2 & CAP2_POCSAG) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_flags_clear_when_zero() {
        let c = Capabilities::new(0, 0);
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
        assert!(Capabilities::new(CAP1_DSTAR, 0).has_dstar());
        assert!(Capabilities::new(CAP1_DMR, 0).has_dmr());
        assert!(Capabilities::new(CAP1_YSF, 0).has_ysf());
        assert!(Capabilities::new(CAP1_P25, 0).has_p25());
        assert!(Capabilities::new(CAP1_NXDN, 0).has_nxdn());
        assert!(Capabilities::new(CAP1_FM, 0).has_fm());
        assert!(Capabilities::new(0, CAP2_POCSAG).has_pocsag());
    }

    #[test]
    fn full_fleet_all_flags() {
        let c = Capabilities::new(
            CAP1_DSTAR | CAP1_DMR | CAP1_YSF | CAP1_P25 | CAP1_NXDN | CAP1_FM,
            CAP2_POCSAG,
        );
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
        // ref/MMDVMHost/Modem.cpp:2027 — v1 always advertises this set.
        let c = Capabilities::new(
            CAP1_DSTAR | CAP1_DMR | CAP1_YSF | CAP1_P25 | CAP1_NXDN,
            CAP2_POCSAG,
        );
        assert!(c.has_dstar());
        assert!(!c.has_fm(), "v1 never advertises FM");
        assert!(c.has_pocsag());
    }
}
