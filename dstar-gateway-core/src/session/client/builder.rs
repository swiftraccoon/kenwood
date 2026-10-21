//! Required-field builder for typed client sessions.

use std::marker::PhantomData;
use std::net::SocketAddr;

use crate::types::{DcsReflector, Module, RoutingCallsign};

use super::{
    ClientProtocol, Configured, DExtra, DPlus, Dcs, Protocol, Session, SessionConfig, SessionCore,
};

/// A required builder input has not been supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Missing;

/// A required builder input has been supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Provided;

/// Builder for [`Session<P, Configured>`], with compile-time required inputs.
///
/// `Cs`, `Lm`, `Rm`, and `Pe` track callsign, local module, reflector module
/// and peer. `Rc` tracks the additional target identity required only by DCS.
/// Setters consume the builder; [`Missing`] inputs prevent `build`.
#[derive(Debug)]
pub struct SessionBuilder<P, Cs, Lm, Rm, Pe, Rc = Missing> {
    callsign: Option<RoutingCallsign>,
    local_module: Option<Module>,
    reflector_module: Option<Module>,
    peer: Option<SocketAddr>,
    dcs_reflector: Option<DcsReflector>,
    marker: PhantomData<(P, Cs, Lm, Rm, Pe, Rc)>,
}

impl<P: Protocol, Cs, Lm, Rm, Pe, Rc> SessionBuilder<P, Cs, Lm, Rm, Pe, Rc> {
    /// Set the checked local callsign prefix.
    #[must_use]
    pub const fn callsign(
        self,
        callsign: RoutingCallsign,
    ) -> SessionBuilder<P, Provided, Lm, Rm, Pe, Rc> {
        SessionBuilder {
            callsign: Some(callsign),
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            peer: self.peer,
            dcs_reflector: self.dcs_reflector,
            marker: PhantomData,
        }
    }

    /// Set the local module letter.
    #[must_use]
    pub const fn local_module(self, module: Module) -> SessionBuilder<P, Cs, Provided, Rm, Pe, Rc> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: Some(module),
            reflector_module: self.reflector_module,
            peer: self.peer,
            dcs_reflector: self.dcs_reflector,
            marker: PhantomData,
        }
    }

    /// Set the target reflector module.
    #[must_use]
    pub const fn reflector_module(
        self,
        module: Module,
    ) -> SessionBuilder<P, Cs, Lm, Provided, Pe, Rc> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: self.local_module,
            reflector_module: Some(module),
            peer: self.peer,
            dcs_reflector: self.dcs_reflector,
            marker: PhantomData,
        }
    }

    /// Set the remote UDP endpoint.
    #[must_use]
    pub const fn peer(self, peer: SocketAddr) -> SessionBuilder<P, Cs, Lm, Rm, Provided, Rc> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            peer: Some(peer),
            dcs_reflector: self.dcs_reflector,
            marker: PhantomData,
        }
    }
}

impl<Cs, Lm, Rm, Pe, Rc> SessionBuilder<Dcs, Cs, Lm, Rm, Pe, Rc> {
    /// Supply the exact DCS reflector identity carried in LINK/UNLINK/POLL.
    #[must_use]
    pub const fn dcs_reflector(
        self,
        target: DcsReflector,
    ) -> SessionBuilder<Dcs, Cs, Lm, Rm, Pe, Provided> {
        SessionBuilder {
            callsign: self.callsign,
            local_module: self.local_module,
            reflector_module: self.reflector_module,
            peer: self.peer,
            dcs_reflector: Some(target),
            marker: PhantomData,
        }
    }
}

impl<P: Protocol, Rc> SessionBuilder<P, Provided, Provided, Provided, Provided, Rc> {
    fn finish(self, protocol: ClientProtocol) -> Session<P, Configured> {
        let (Some(callsign), Some(local_module), Some(reflector_module), Some(peer)) = (
            self.callsign,
            self.local_module,
            self.reflector_module,
            self.peer,
        ) else {
            unreachable!("Provided markers require every common field");
        };
        Session {
            inner: SessionCore::new(SessionConfig {
                protocol,
                callsign,
                local_module,
                reflector_module,
                peer,
            }),
            _protocol: PhantomData,
            _state: PhantomData,
        }
    }
}

impl SessionBuilder<DPlus, Provided, Provided, Provided, Provided> {
    /// Build a configured D-Plus session after all four common setters.
    #[must_use]
    pub fn build(self) -> Session<DPlus, Configured> {
        self.finish(ClientProtocol::DPlus)
    }
}

impl SessionBuilder<DExtra, Provided, Provided, Provided, Provided> {
    /// Build a configured `DExtra` session after all four common setters.
    #[must_use]
    pub fn build(self) -> Session<DExtra, Configured> {
        self.finish(ClientProtocol::DExtra)
    }
}

impl SessionBuilder<Dcs, Provided, Provided, Provided, Provided, Provided> {
    /// Build a configured DCS session after all five required setters.
    #[must_use]
    pub fn build(self) -> Session<Dcs, Configured> {
        let Some(target) = self.dcs_reflector else {
            unreachable!("Provided target marker");
        };
        self.finish(ClientProtocol::Dcs(target))
    }
}

impl<P: Protocol> Session<P, Configured> {
    /// Start a builder requiring callsign, both modules and the remote endpoint.
    ///
    /// DCS additionally requires [`SessionBuilder::dcs_reflector`].
    ///
    /// ```
    /// use dstar_gateway_core::{Configured, Dcs, DcsReflector, Module, RoutingCallsign, Session};
    /// let session = Session::<Dcs, Configured>::builder()
    ///     .callsign(RoutingCallsign::new("W1AW")?)
    ///     .local_module(Module::B)
    ///     .reflector_module(Module::C)
    ///     .peer("127.0.0.1:30051".parse()?)
    ///     .dcs_reflector(DcsReflector::new("DCS030")?)
    ///     .build();
    /// assert_eq!(session.peer().port(), 30051);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub const fn builder() -> SessionBuilder<P, Missing, Missing, Missing, Missing> {
        SessionBuilder {
            callsign: None,
            local_module: None,
            reflector_module: None,
            peer: None,
            dcs_reflector: None,
            marker: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientStateKind;
    use std::net::{IpAddr, Ipv4Addr};

    const CS: RoutingCallsign = match RoutingCallsign::from_padded_bytes(*b"W1AW   ") {
        Ok(callsign) => callsign,
        Err(_) => unreachable!(),
    };
    const ADDR: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30001);

    #[test]
    fn builders_preserve_order_independence_and_protocol_configuration() {
        let dextra = Session::<DExtra, Configured>::builder()
            .peer(ADDR)
            .reflector_module(Module::C)
            .local_module(Module::B)
            .callsign(CS)
            .build();
        assert_eq!(dextra.state_kind(), ClientStateKind::Configured);
        assert_eq!(dextra.local_callsign(), CS.callsign());
        let dplus = Session::<DPlus, Configured>::builder()
            .callsign(CS)
            .local_module(Module::B)
            .reflector_module(Module::C)
            .peer(ADDR)
            .build();
        assert_eq!(dplus.state_kind(), ClientStateKind::Configured);
    }

    #[test]
    fn dcs_target_is_preserved_in_link_poll_and_unlink() -> Result<(), Box<dyn std::error::Error>> {
        let target = DcsReflector::new("dcs030")?;
        assert_eq!(target.callsign().text(), Ok("DCS030"));
        assert!(DcsReflector::new("REF030").is_err());
        assert!(DcsReflector::new("DCS030AB").is_err());
        let mut core = Session::<Dcs, Configured>::builder()
            .callsign(CS)
            .local_module(Module::B)
            .reflector_module(Module::C)
            .peer(ADDR)
            .dcs_reflector(target)
            .build()
            .inner;
        let now = std::time::Instant::now();
        core.enqueue_connect(now)?;
        let link = core.pop_transmit(now).ok_or("LINK missing")?;
        assert_eq!(
            link.payload.get(11..19),
            Some(b"DCS030  ".as_slice()),
            "LINK must use its configured target"
        );
        let mut reply = [0_u8; 14];
        let length = crate::codec::dcs::encode_connect_ack(
            &mut reply,
            crate::RepeaterIdentity {
                callsign: CS,
                module: Module::B,
            },
            Module::C,
        )?;
        core.handle_input(now, ADDR, reply.get(..length).ok_or("ACK length")?)?;
        let poll_time = now
            .checked_add(crate::codec::dcs::consts::KEEPALIVE_INTERVAL)
            .ok_or("poll deadline")?;
        core.handle_timeout(poll_time);
        let poll = core.pop_transmit(poll_time).ok_or("POLL missing")?;
        assert_eq!(
            poll.payload.get(9..17),
            Some(b"DCS030  ".as_slice()),
            "POLL must use its configured target"
        );
        core.enqueue_disconnect(poll_time)?;
        let unlink = core.pop_transmit(poll_time).ok_or("UNLINK missing")?;
        assert_eq!(
            unlink.payload.get(11..19),
            Some(b"DCS030  ".as_slice()),
            "UNLINK must use its configured target"
        );
        Ok(())
    }
}
