//! Calling `.build()` on a partially-constructed builder must be a
//! compile error. Specifically, `.build()` is only implemented on
//! `SessionBuilder<P, Provided, Provided, Provided, Provided>` — with
//! any field still `Missing`, there is no matching impl.

use dstar_gateway_core::session::client::{DExtra, Session};
use dstar_gateway_core::types::Module;

fn main() {
    // Missing callsign — only 3 of 4 required fields set.
    let builder = Session::<DExtra, _>::builder()
        .local_module(Module::try_from_char('B').unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:30001".parse().unwrap());

    // ERROR: no method named `build` found for
    // `SessionBuilder<DExtra, Missing, Provided, Provided, Provided>`.
    let _session = builder.build();
}
