//! Same as `builder_missing_callsign` but with `local_module` missing.
//!
//! Proves the `Missing`/`Provided` typestate catches each required
//! builder field independently, not just the first one.

use dstar_gateway_core::session::client::{DExtra, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let builder = Session::<DExtra, _>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .reflector_module(Module::C)
        .peer("127.0.0.1:30001".parse()?);

    // ERROR: no method named `build` found for
    // `SessionBuilder<DExtra, Provided, Missing, Provided, Provided>`.
    let _session = builder.build();
    Ok(())
}
