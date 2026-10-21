//! A function expecting `Session<Dcs, _>` cannot accept `Session<DPlus, _>`.
//! This proves the phantom protocol type discriminates at compile time:
//! even though both sessions wrap the same `SessionCore`, the
//! phantom marker is part of the type identity.

use dstar_gateway_core::session::client::{Configured, DPlus, Dcs, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};

fn accepts_dcs_only(_session: Session<Dcs, Configured>) {}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session: Session<DPlus, Configured> = Session::<DPlus, _>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer("127.0.0.1:20001".parse()?)
        .build();

    // ERROR: expected `Session<Dcs, _>`, found `Session<DPlus, _>`.
    accepts_dcs_only(session);
    Ok(())
}
