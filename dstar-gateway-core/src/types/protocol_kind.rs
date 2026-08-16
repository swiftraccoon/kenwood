//! Runtime protocol discriminator.
//!
//! Compile-time protocol distinction lives in the typestate marker
//! types `DPlus`, `DExtra`, `Dcs`. This enum is the runtime mirror,
//! used in error variants, diagnostics, log fields, and anywhere a
//! `Protocol` type parameter has been erased.

/// D-STAR reflector protocol discriminator.
///
/// Runtime mirror of the compile-time `Protocol` marker traits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProtocolKind {
    /// `DPlus` (REF reflectors, UDP port 20001 + TCP auth).
    DPlus,
    /// `DExtra` (XRF/XLX reflectors, UDP port 30001).
    DExtra,
    /// `DCS` (DCS reflectors, UDP port 30051).
    Dcs,
}

impl ProtocolKind {
    /// Default UDP port for this protocol.
    ///
    /// Per `ircDDBGateway/Common/DStarDefines.h:115-117`.
    #[must_use]
    pub const fn default_port(self) -> u16 {
        match self {
            Self::DPlus => 20001,
            Self::DExtra => 30001,
            Self::Dcs => 30051,
        }
    }

    /// Identify the protocol from its default UDP port.
    ///
    /// The exact inverse of [`ProtocolKind::default_port`]: `20001` is
    /// `DPlus`, `30001` is `DExtra`, `30051` is `DCS`. Returns `None` for
    /// every other port; nonstandard deployments need explicit protocol
    /// selection rather than a guess.
    #[must_use]
    pub const fn from_port(port: u16) -> Option<Self> {
        match port {
            20001 => Some(Self::DPlus),
            30001 => Some(Self::DExtra),
            30051 => Some(Self::Dcs),
            _ => None,
        }
    }

    /// Whether this protocol requires TCP authentication before UDP linking.
    ///
    /// Only `DPlus` requires this; see
    /// `ircDDBGateway/Common/DPlusAuthenticator.cpp:62-200`.
    #[must_use]
    pub const fn needs_auth(self) -> bool {
        matches!(self, Self::DPlus)
    }

    /// Identify the protocol from a reflector callsign prefix.
    ///
    /// Examines the first three characters (case-insensitive):
    ///
    /// - `"XRF"` or `"XLX"` → [`ProtocolKind::DExtra`]
    /// - `"REF"` → [`ProtocolKind::DPlus`]
    /// - `"DCS"` → [`ProtocolKind::Dcs`]
    ///
    /// Returns `None` for any other prefix or input shorter than 3 ASCII chars.
    #[must_use]
    pub fn from_reflector_prefix(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.len() < 3 {
            return None;
        }
        let prefix: [u8; 3] = [
            bytes.first()?.to_ascii_uppercase(),
            bytes.get(1)?.to_ascii_uppercase(),
            bytes.get(2)?.to_ascii_uppercase(),
        ];
        match &prefix {
            b"XRF" | b"XLX" => Some(Self::DExtra),
            b"REF" => Some(Self::DPlus),
            b"DCS" => Some(Self::Dcs),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_kind_default_port_dplus() {
        assert_eq!(ProtocolKind::DPlus.default_port(), 20001);
    }

    #[test]
    fn protocol_kind_default_port_dextra() {
        assert_eq!(ProtocolKind::DExtra.default_port(), 30001);
    }

    #[test]
    fn protocol_kind_default_port_dcs() {
        assert_eq!(ProtocolKind::Dcs.default_port(), 30051);
    }

    #[test]
    fn protocol_kind_needs_auth_only_dplus() {
        assert!(ProtocolKind::DPlus.needs_auth());
        assert!(!ProtocolKind::DExtra.needs_auth());
        assert!(!ProtocolKind::Dcs.needs_auth());
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_xrf() {
        assert_eq!(
            ProtocolKind::from_reflector_prefix("XRF030"),
            Some(ProtocolKind::DExtra)
        );
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_xlx() {
        assert_eq!(
            ProtocolKind::from_reflector_prefix("XLX307"),
            Some(ProtocolKind::DExtra)
        );
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_ref() {
        assert_eq!(
            ProtocolKind::from_reflector_prefix("REF030"),
            Some(ProtocolKind::DPlus)
        );
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_dcs() {
        assert_eq!(
            ProtocolKind::from_reflector_prefix("DCS001"),
            Some(ProtocolKind::Dcs)
        );
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_case_insensitive() {
        assert_eq!(
            ProtocolKind::from_reflector_prefix("ref030"),
            Some(ProtocolKind::DPlus)
        );
        assert_eq!(
            ProtocolKind::from_reflector_prefix("Ref030"),
            Some(ProtocolKind::DPlus)
        );
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_unknown() {
        assert_eq!(ProtocolKind::from_reflector_prefix("FOO123"), None);
    }

    #[test]
    fn protocol_kind_from_reflector_prefix_too_short() {
        assert_eq!(ProtocolKind::from_reflector_prefix("XR"), None);
    }

    #[test]
    fn protocol_kind_from_port_inverts_default_port() {
        for kind in [ProtocolKind::DPlus, ProtocolKind::DExtra, ProtocolKind::Dcs] {
            assert_eq!(ProtocolKind::from_port(kind.default_port()), Some(kind));
        }
        assert_eq!(ProtocolKind::from_port(30052), None);
        assert_eq!(ProtocolKind::from_port(0), None);
    }
}
