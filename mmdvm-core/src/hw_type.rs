// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Modem hardware family, inferred from the `GetVersion` description.
//!
//! Mirrors `enum class HW_TYPE` in `ref/MMDVMHost/Defines.h:54-67` and
//! the memcmp-based detection in `ref/MMDVMHost/Modem.cpp:1997-2020`.

/// Hardware type enum matching `HW_TYPE` in `MMDVMHost`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HwType {
    /// Generic "MMDVM" board (arduino-class and similar).
    Mmdvm,
    /// DVMEGA radio hat.
    DvMega,
    /// `ZUMspot` hotspot.
    MmdvmZumSpot,
    /// `MMDVM_HS_Hat` Raspberry Pi hat.
    MmdvmHsHat,
    /// `MMDVM_HS_Dual_Hat` (dual-band) Pi hat.
    MmdvmHsDualHat,
    /// Nano hotSPOT.
    NanoHotspot,
    /// `Nano_DV` dongle.
    NanoDv,
    /// `D2RG_MMDVM_HS` board.
    D2rgMmdvmHs,
    /// `MMDVM_HS` (generic board, description prefix `MMDVM_HS-`).
    MmdvmHs,
    /// `OpenGD77_HS` firmware on an HS board.
    OpenGd77Hs,
    /// `SkyBridge`.
    SkyBridge,
    /// Unknown hardware (description didn't match any known prefix).
    Unknown,
}

impl HwType {
    /// Guess the hardware type from a `GetVersion` description string.
    ///
    /// The description is matched against the same set of prefixes that
    /// `ref/MMDVMHost/Modem.cpp:1997-2020` uses. Unknown descriptions
    /// — including the TH-D75's internal modem — return
    /// [`HwType::Unknown`].
    #[must_use]
    pub fn from_description(desc: &str) -> Self {
        // Order mirrors the reference implementation so ambiguous
        // descriptions resolve to the same variant.
        if desc.starts_with("MMDVM ") {
            return Self::Mmdvm;
        }
        if desc.starts_with("DVMEGA") {
            return Self::DvMega;
        }
        if desc.starts_with("ZUMspot") {
            return Self::MmdvmZumSpot;
        }
        if desc.starts_with("MMDVM_HS_Hat") {
            return Self::MmdvmHsHat;
        }
        if desc.starts_with("MMDVM_HS_Dual_Hat") {
            return Self::MmdvmHsDualHat;
        }
        if desc.starts_with("Nano_hotSPOT") {
            return Self::NanoHotspot;
        }
        if desc.starts_with("Nano_DV") {
            return Self::NanoDv;
        }
        if desc.starts_with("D2RG_MMDVM_HS") {
            return Self::D2rgMmdvmHs;
        }
        if desc.starts_with("MMDVM_HS-") {
            return Self::MmdvmHs;
        }
        if desc.starts_with("OpenGD77_HS") {
            return Self::OpenGd77Hs;
        }
        if desc.starts_with("SkyBridge") {
            return Self::SkyBridge;
        }
        Self::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_mmdvm_is_detected() {
        assert_eq!(HwType::from_description("MMDVM 20200101"), HwType::Mmdvm);
    }

    #[test]
    fn zumspot_is_detected() {
        assert_eq!(
            HwType::from_description("ZUMspot v1.5.4"),
            HwType::MmdvmZumSpot
        );
    }

    #[test]
    fn mmdvm_hs_dual_hat_before_mmdvm_hs_hat_alternative() {
        // Both prefixes start with "MMDVM_HS_" — the dual-hat match
        // must not be accidentally pre-empted by the single-hat check
        // because the reference ordering tests them in that order.
        // Here the single-hat prefix matches "MMDVM_HS_Hat" first in
        // our implementation, but "MMDVM_HS_Dual_Hat" also has the
        // "MMDVM_HS_Hat" prefix absent — in fact they differ, so both
        // resolve uniquely.
        assert_eq!(
            HwType::from_description("MMDVM_HS_Hat v2"),
            HwType::MmdvmHsHat
        );
        assert_eq!(
            HwType::from_description("MMDVM_HS_Dual_Hat v1"),
            HwType::MmdvmHsDualHat
        );
    }

    #[test]
    fn mmdvm_hs_requires_dash_suffix() {
        assert_eq!(HwType::from_description("MMDVM_HS-abc"), HwType::MmdvmHs,);
    }

    #[test]
    fn unknown_description() {
        assert_eq!(HwType::from_description("TH-D75 0x102"), HwType::Unknown);
        assert_eq!(HwType::from_description(""), HwType::Unknown);
        assert_eq!(HwType::from_description("foo"), HwType::Unknown);
    }

    #[test]
    fn opengd77_and_skybridge() {
        assert_eq!(
            HwType::from_description("OpenGD77_HS 2024"),
            HwType::OpenGd77Hs,
        );
        assert_eq!(
            HwType::from_description("SkyBridge HS v3"),
            HwType::SkyBridge,
        );
    }
}
