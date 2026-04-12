//! Same as `builder_missing_callsign` but with `local_module` missing.
//!
//! Proves the `Missing`/`Provided` typestate catches each required
//! builder field independently — not just the first one.

use dstar_gateway_core::session::client::{DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module};

fn main() {
    let builder = Session::<DExtra, _>::builder()
        .callsign(Callsign::try_from_str("W1AW").unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:30001".parse().unwrap());

    // ERROR: no method named `build` found for
    // `SessionBuilder<DExtra, Provided, Missing, Provided, Provided>`.
    let _session = builder.build();
}
