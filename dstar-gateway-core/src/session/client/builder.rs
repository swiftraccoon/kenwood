//! Typed [`SessionBuilder`] for [`Session<P, Configured>`].
//!
//! The typestate builder uses marker types to track which required
//! fields have been set. Each setter consumes `self` and returns a new
//! [`SessionBuilder`] with the corresponding marker type flipped from
//! [`Missing`] to [`Provided`]. The [`SessionBuilder::build`] method
//! is only implemented on
//! `SessionBuilder<P, Provided, Provided, Provided, Provided>` — a
//! missing field turns `.build()` into a compile error.

use std::marker::PhantomData;
use std::net::SocketAddr;

use crate::types::{Callsign, Module};

use super::core::SessionCore;
use super::protocol::Protocol;
use super::session::Session;
use super::state::Configured;

/// Marker indicating a required builder field has NOT been set.
#[derive(Debug)]
pub struct Missing;

/// Marker indicating a required builder field HAS been set.
#[derive(Debug)]
pub struct Provided;

/// Typestate builder for [`Session<P, Configured>`].
///
/// Parameters:
///
/// - `P` — protocol marker ([`super::DPlus`], [`super::DExtra`],
///   [`super::Dcs`])
/// - `Cs` — [`Missing`] or [`Provided`], tracks whether the callsign
///   has been set
/// - `Lm` — tracks local module
/// - `Rm` — tracks reflector module
/// - `Pe` — tracks peer address
///
/// All four `Missing`/`Provided` type parameters start as
/// [`Missing`] and must be flipped to [`Provided`] before
/// [`Self::build`] becomes callable. The phantoms add zero runtime
/// cost.
#[derive(Debug)]
pub struct SessionBuilder<P: Protocol, Cs, Lm, Rm, Pe> {
    callsign: Option<Callsign>,
    local_module: Option<Module>,
    reflector_module: Option<Module>,
    peer: Option<SocketAddr>,
    /// Optional reflector callsign — only required for `DCS`
    /// sessions targeting a non-`DCS001` reflector. `None` means
    /// `SessionCore` falls back to its `DCS001  ` default (and
    /// logs a warning if the protocol is `DCS`).
    reflector_callsign: Option<Callsign>,
    _protocol: PhantomData<P>,
    _cs: PhantomData<Cs>,
    _lm: PhantomData<Lm>,
    _rm: PhantomData<Rm>,
    _pe: PhantomData<Pe>,
}

impl<P: Protocol, Cs, Lm, Rm, Pe> SessionBuilder<P, Cs, Lm, Rm, Pe> {
    /// Set the station callsign.
    #[must_use]
    pub const fn callsign(self, callsign: Callsign) -> SessionBuilder<P, Provided, Lm, Rm, Pe> {
        SessionBuilder {
            callsign: Some(callsign),
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            peer: self.peer,
            reflector_callsign: self.reflector_callsign,
            _protocol: PhantomData,
            _cs: PhantomData,
            _lm: PhantomData,
            _rm: PhantomData,
            _pe: PhantomData,
        }
    }

    /// Set the local module letter (the module on the client side).
    #[must_use]
    pub const fn local_module(self, module: Module) -> SessionBuilder<P, Cs, Provided, Rm, Pe> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: Some(module),
            reflector_module: self.reflector_module,
            peer: self.peer,
            reflector_callsign: self.reflector_callsign,
            _protocol: PhantomData,
            _cs: PhantomData,
            _lm: PhantomData,
            _rm: PhantomData,
            _pe: PhantomData,
        }
    }

    /// Set the reflector module letter (the module we want to link to).
    #[must_use]
    pub const fn reflector_module(self, module: Module) -> SessionBuilder<P, Cs, Lm, Provided, Pe> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: self.local_module,
            reflector_module: Some(module),
            peer: self.peer,
            reflector_callsign: self.reflector_callsign,
            _protocol: PhantomData,
            _cs: PhantomData,
            _lm: PhantomData,
            _rm: PhantomData,
            _pe: PhantomData,
        }
    }

    /// Set the reflector peer address.
    #[must_use]
    pub const fn peer(self, peer: SocketAddr) -> SessionBuilder<P, Cs, Lm, Rm, Provided> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            peer: Some(peer),
            reflector_callsign: self.reflector_callsign,
            _protocol: PhantomData,
            _cs: PhantomData,
            _lm: PhantomData,
            _rm: PhantomData,
            _pe: PhantomData,
        }
    }

    /// Set the target reflector's own callsign.
    ///
    /// **Required for `DCS` sessions targeting any reflector other
    /// than `DCS001`.** The DCS wire format embeds the reflector
    /// callsign in every LINK / UNLINK / POLL packet, and a real
    /// `DCS030` reflector will silently drop traffic whose embedded
    /// reflector callsign reads `DCS001  `. For `DPlus` and
    /// `DExtra` this is metadata only — the protocols do not carry
    /// the reflector callsign on the wire, so the setter is
    /// harmless when unused.
    ///
    /// Unlike the four required fields, this setter does not flip
    /// a typestate marker — sessions that do not need a reflector
    /// callsign keep building with four setters, and DCS sessions
    /// that forget it get a runtime warning at construction time
    /// plus a `DCS001  ` fallback. Upgrading this to a compile-time
    /// requirement for `Session<Dcs, _>` specifically is a future
    /// design refinement.
    #[must_use]
    pub const fn reflector_callsign(mut self, reflector_callsign: Callsign) -> Self {
        self.reflector_callsign = Some(reflector_callsign);
        self
    }
}

impl<P: Protocol> SessionBuilder<P, Provided, Provided, Provided, Provided> {
    /// Build the [`Session<P, Configured>`].
    ///
    /// Only callable when all four required fields have been set —
    /// any [`Missing`] marker turns this into a compile error.
    ///
    /// The [`Provided`] type parameters are the typestate proof that
    /// every field was set; the `Option` unwrapping below is
    /// therefore infallible at the type level, and we use
    /// [`unreachable!`] in the impossible branches rather than
    /// [`Option::expect`] (which is lint-denied in lib code).
    #[must_use]
    pub fn build(self) -> Session<P, Configured> {
        let Some(callsign) = self.callsign else {
            unreachable!("Provided marker guarantees callsign is Some");
        };
        let Some(local_module) = self.local_module else {
            unreachable!("Provided marker guarantees local_module is Some");
        };
        let Some(reflector_module) = self.reflector_module else {
            unreachable!("Provided marker guarantees reflector_module is Some");
        };
        let Some(peer) = self.peer else {
            unreachable!("Provided marker guarantees peer is Some");
        };
        let core = SessionCore::new_with_reflector_callsign(
            P::KIND,
            callsign,
            local_module,
            reflector_module,
            peer,
            self.reflector_callsign,
        );
        Session {
            inner: core,
            _protocol: PhantomData,
            _state: PhantomData,
        }
    }
}

impl<P: Protocol> Session<P, Configured> {
    /// Entry point for building a typed [`Session<P, Configured>`].
    ///
    /// Returns a builder with every required field marked
    /// [`Missing`]. Chain `.callsign()`, `.local_module()`,
    /// `.reflector_module()`, and `.peer()` in any order, then call
    /// `.build()`. Skipping any of the four setters turns the
    /// `.build()` call into a compile error.
    ///
    /// # Example
    ///
    /// ```
    /// use dstar_gateway_core::session::client::{Configured, DExtra, Session};
    /// use dstar_gateway_core::types::{Callsign, Module};
    ///
    /// let session: Session<DExtra, Configured> =
    ///     Session::<DExtra, Configured>::builder()
    ///         .callsign(Callsign::try_from_str("W1AW")?)
    ///         .local_module(Module::try_from_char('B')?)
    ///         .reflector_module(Module::try_from_char('C')?)
    ///         .peer("127.0.0.1:30001".parse()?)
    ///         .build();
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub const fn builder() -> SessionBuilder<P, Missing, Missing, Missing, Missing> {
        SessionBuilder {
            callsign: None,
            local_module: None,
            reflector_module: None,
            peer: None,
            reflector_callsign: None,
            _protocol: PhantomData,
            _cs: PhantomData,
            _lm: PhantomData,
            _rm: PhantomData,
            _pe: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::client::protocol::{DExtra, DPlus};
    use crate::session::client::state::ClientStateKind;
    use std::net::{IpAddr, Ipv4Addr};

    const CS: Callsign = Callsign::from_wire_bytes(*b"W1AW    ");
    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);
    const ADDR_DPLUS: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20001);

    #[test]
    fn dextra_builder_happy_path() {
        let session = Session::<DExtra, Configured>::builder()
            .callsign(CS)
            .local_module(Module::B)
            .reflector_module(Module::C)
            .peer(ADDR)
            .build();
        assert_eq!(session.state_kind(), ClientStateKind::Configured);
        assert_eq!(session.peer(), ADDR);
        assert_eq!(session.local_callsign(), CS);
    }

    #[test]
    fn dextra_builder_field_order_independent() {
        // Same as above but setters in a different order.
        let session = Session::<DExtra, Configured>::builder()
            .peer(ADDR)
            .reflector_module(Module::C)
            .local_module(Module::B)
            .callsign(CS)
            .build();
        assert_eq!(session.state_kind(), ClientStateKind::Configured);
    }

    #[test]
    fn dplus_builder_builds_configured() {
        let session = Session::<DPlus, Configured>::builder()
            .callsign(CS)
            .local_module(Module::B)
            .reflector_module(Module::C)
            .peer(ADDR_DPLUS)
            .build();
        assert_eq!(session.state_kind(), ClientStateKind::Configured);
    }
}
