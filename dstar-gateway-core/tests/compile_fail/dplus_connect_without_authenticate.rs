//! Calling `connect` on `Session<DPlus, Configured>` must be a compile
//! error; DPlus requires `.authenticate()` first.
//!
//! The `connect()` method is only implemented on
//! `Session<P, Configured> where P: NoAuthRequired`. `DPlus` does
//! NOT impl `NoAuthRequired` (only `DExtra` and `Dcs` do). This test
//! proves the trait bound keeps DPlus out.

use dstar_gateway_core::session::client::{DPlus, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session = Session::<DPlus, _>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer("127.0.0.1:20001".parse()?)
        .build();

    // ERROR: no method named `connect` found for `Session<DPlus, Configured>`
    // because DPlus does not implement `NoAuthRequired`. Must call
    // `.authenticate(hosts)` first.
    let _connecting = session.connect(Instant::now());
    Ok(())
}
