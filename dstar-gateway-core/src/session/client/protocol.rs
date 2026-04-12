//! Sealed `Protocol` marker trait + the three reflector protocol markers.

use crate::types::ProtocolKind;

/// Sealed marker trait for D-STAR reflector protocols.
///
/// Only the three protocols defined in this module ([`DPlus`],
/// [`DExtra`], [`Dcs`]) may implement this trait. The seal prevents
/// downstream crates from adding fake protocols, which would
/// invalidate every typestate proof.
pub trait Protocol: sealed::Sealed + Copy + Send + Sync + 'static {
    /// Runtime discriminator for this protocol.
    const KIND: ProtocolKind;
    /// Default UDP port.
    const DEFAULT_PORT: u16;
    /// Whether this protocol requires TCP authentication before UDP linking.
    const NEEDS_AUTH: bool;
}

mod sealed {
    pub trait Sealed {}
}

/// `DPlus` (REF reflectors). Requires TCP authentication.
#[derive(Debug, Clone, Copy)]
pub struct DPlus;

/// `DExtra` (XRF/XLX reflectors).
#[derive(Debug, Clone, Copy)]
pub struct DExtra;

/// `DCS` reflectors.
#[derive(Debug, Clone, Copy)]
pub struct Dcs;

impl sealed::Sealed for DPlus {}
impl Protocol for DPlus {
    const KIND: ProtocolKind = ProtocolKind::DPlus;
    const DEFAULT_PORT: u16 = 20001;
    const NEEDS_AUTH: bool = true;
}

impl sealed::Sealed for DExtra {}
impl Protocol for DExtra {
    const KIND: ProtocolKind = ProtocolKind::DExtra;
    const DEFAULT_PORT: u16 = 30001;
    const NEEDS_AUTH: bool = false;
}

impl sealed::Sealed for Dcs {}
impl Protocol for Dcs {
    const KIND: ProtocolKind = ProtocolKind::Dcs;
    const DEFAULT_PORT: u16 = 30051;
    const NEEDS_AUTH: bool = false;
}

/// Protocols that do NOT require authentication (`DExtra`, `Dcs`).
///
/// Used as a trait bound on `Session<P, Configured>::connect` so
/// the no-auth path only exists for protocols that don't need it.
/// `DPlus` does NOT impl this — it requires `authenticate` first.
pub trait NoAuthRequired: Protocol {}
impl NoAuthRequired for DExtra {}
impl NoAuthRequired for Dcs {}

#[cfg(test)]
mod tests {
    use super::*;

    // `Protocol::KIND` / `DEFAULT_PORT` / `NEEDS_AUTH` are associated
    // consts, so `assert_eq!` against another literal is a
    // compile-time check clippy flags as "will be optimised out".
    // Pull the values into local bindings first — this is a runtime
    // read from the function's perspective, even though the optimiser
    // folds it.
    fn needs_auth_of<P: Protocol>() -> bool {
        <P as Protocol>::NEEDS_AUTH
    }
    fn kind_of<P: Protocol>() -> ProtocolKind {
        <P as Protocol>::KIND
    }
    fn default_port_of<P: Protocol>() -> u16 {
        <P as Protocol>::DEFAULT_PORT
    }

    #[test]
    fn dplus_kind_is_dplus() {
        assert_eq!(kind_of::<DPlus>(), ProtocolKind::DPlus);
        assert_eq!(default_port_of::<DPlus>(), 20001);
        assert!(needs_auth_of::<DPlus>());
    }

    #[test]
    fn dextra_kind_is_dextra() {
        assert_eq!(kind_of::<DExtra>(), ProtocolKind::DExtra);
        assert_eq!(default_port_of::<DExtra>(), 30001);
        assert!(!needs_auth_of::<DExtra>());
    }

    #[test]
    fn dcs_kind_is_dcs() {
        assert_eq!(kind_of::<Dcs>(), ProtocolKind::Dcs);
        assert_eq!(default_port_of::<Dcs>(), 30051);
        assert!(!needs_auth_of::<Dcs>());
    }
}
