//! Calling `connect` on `Session<DPlus, Configured>` must be a compile
//! error — DPlus requires `.authenticate()` first.
//!
//! The `connect()` method is only implemented on
//! `Session<P, Configured> where P: NoAuthRequired`. `DPlus` does
//! NOT impl `NoAuthRequired` (only `DExtra` and `Dcs` do). This test
//! proves the trait bound keeps DPlus out.

use dstar_gateway_core::session::client::{DPlus, Session};
use dstar_gateway_core::types::{Callsign, Module};
use std::time::Instant;

fn main() {
    let session = Session::<DPlus, _>::builder()
        .callsign(Callsign::try_from_str("W1AW").unwrap())
        .local_module(Module::try_from_char('B').unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:20001".parse().unwrap())
        .build();

    // ERROR: no method named `connect` found for `Session<DPlus, Configured>`
    // because DPlus does not implement `NoAuthRequired`. Must call
    // `.authenticate(hosts)` first.
    let _connecting = session.connect(Instant::now());
}
