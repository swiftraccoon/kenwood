//! A DCS client cannot build without its control-packet target identity.

use dstar_gateway_core::{Configured, Dcs, Module, RoutingCallsign, Session};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let builder = Session::<Dcs, Configured>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer("127.0.0.1:30051".parse()?);
    let _session = builder.build();
    Ok(())
}
