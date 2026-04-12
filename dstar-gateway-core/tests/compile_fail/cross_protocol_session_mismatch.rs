//! A function expecting `Session<Dcs, _>` cannot accept `Session<DPlus, _>`.
//! This proves the phantom protocol type discriminates at compile time
//! — even though both sessions wrap the same `SessionCore`, the
//! phantom marker is part of the type identity.

use dstar_gateway_core::session::client::{Configured, DPlus, Dcs, Session};
use dstar_gateway_core::types::{Callsign, Module};

fn accepts_dcs_only(_session: Session<Dcs, Configured>) {}

fn main() {
    let session: Session<DPlus, Configured> = Session::<DPlus, _>::builder()
        .callsign(Callsign::try_from_str("W1AW").unwrap())
        .local_module(Module::try_from_char('B').unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:20001".parse().unwrap())
        .build();

    // ERROR: expected `Session<Dcs, _>`, found `Session<DPlus, _>`.
    accepts_dcs_only(session);
}
