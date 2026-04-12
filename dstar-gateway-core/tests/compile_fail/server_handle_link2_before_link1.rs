//! Calling `handle_link2` on a `ServerSession<DPlus, Unknown>` must
//! be a compile error — `handle_link2` is only valid after a LINK1
//! has been received, which is tracked by the `Link1Received`
//! state marker.
//!
//! `handle_link2` is only implemented on
//! `ServerSession<DPlus, Link1Received>`. A freshly constructed
//! session is in `Unknown`, so the method does not resolve and the
//! typestate enforces the two-step `DPlus` handshake at compile time.

use dstar_gateway_core::session::client::DPlus;
use dstar_gateway_core::session::server::{ServerSession, Unknown};
use dstar_gateway_core::types::Callsign;
use std::time::Instant;

fn main() {
    let session: ServerSession<DPlus, Unknown> = todo!();
    let callsign = Callsign::from_wire_bytes(*b"W1AW    ");

    // ERROR: no method named `handle_link2` found for
    // `ServerSession<DPlus, Unknown>` — `handle_link2` is only
    // implemented on `ServerSession<DPlus, Link1Received>`.
    let _ = session.handle_link2(Instant::now(), callsign, &[0u8; 0]);
}
