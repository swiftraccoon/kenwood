// Portions of this file are derived from MMDVMHost by Jonathan Naylor
// G4KLX, Copyright (C) 2015-2026, licensed under GPL-2.0-or-later.
// See LICENSE for full attribution.

//! Modem hardware family, inferred from the `GetVersion` description.
//!
//! Mirrors `enum class HW_TYPE` in `MMDVMHost/Defines.h:54-67` and
//! the memcmp-based detection in `MMDVMHost/Modem.cpp:1997-2020`.

use std::fmt;

/// Hardware type enum matching `HW_TYPE` in `MMDVMHost`.
///
/// Classification uses reported description prefixes. See the
/// [version example](crate#parse-version-and-status-responses).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HwType {
    /// The `MMDVM ` description prefix.
    Mmdvm,
    /// The `DVMEGA` description prefix.
    DvMega,
    /// The `ZUMspot` description prefix.
    MmdvmZumSpot,
    /// The `MMDVM_HS_Hat` description prefix.
    MmdvmHsHat,
    /// The `MMDVM_HS_Dual_Hat` description prefix.
    MmdvmHsDualHat,
    /// The `Nano_hotSPOT` description prefix.
    NanoHotspot,
    /// The `Nano_DV` description prefix.
    NanoDv,
    /// The `D2RG_MMDVM_HS` description prefix.
    D2rgMmdvmHs,
    /// The `MMDVM_HS-` description prefix.
    MmdvmHs,
    /// The `OpenGD77_HS` description prefix.
    OpenGd77Hs,
    /// The `SkyBridge` description prefix.
    SkyBridge,
    /// Unknown hardware (description didn't match any known prefix).
    Unknown,
}

impl HwType {
    /// Guess the hardware type from a `GetVersion` description string.
    ///
    /// The description is matched against the same set of prefixes that
    /// `MMDVMHost/Modem.cpp:1997-2020` uses. Other descriptions return
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

impl fmt::Display for HwType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mmdvm => "MMDVM",
            Self::DvMega => "DVMEGA",
            Self::MmdvmZumSpot => "ZUMspot",
            Self::MmdvmHsHat => "MMDVM_HS_Hat",
            Self::MmdvmHsDualHat => "MMDVM_HS_Dual_Hat",
            Self::NanoHotspot => "Nano_hotSPOT",
            Self::NanoDv => "Nano_DV",
            Self::D2rgMmdvmHs => "D2RG_MMDVM_HS",
            Self::MmdvmHs => "MMDVM_HS",
            Self::OpenGd77Hs => "OpenGD77_HS",
            Self::SkyBridge => "SkyBridge",
            Self::Unknown => "unknown",
        })
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
        // Both prefixes start with "MMDVM_HS_" but diverge at the
        // next character ("Hat" vs "Dual_Hat"), so the single-hat
        // check (which runs first, mirroring the reference order)
        // can never shadow the dual-hat match. This test pins that
        // both resolve to their own variant.
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
