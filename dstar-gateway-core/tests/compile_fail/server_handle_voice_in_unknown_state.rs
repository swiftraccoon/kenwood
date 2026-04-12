//! Calling `handle_voice_data` on a `ServerSession<_, Unknown>` must
//! be a compile error — voice frames are only valid in the
//! `Streaming` state.
//!
//! `handle_voice_data` is only implemented on
//! `ServerSession<P, Streaming>`. A freshly constructed session is
//! in `Unknown`, so the method does not resolve and the typestate
//! keeps voice frames out of an unlinked session.

use dstar_gateway_core::session::client::DExtra;
use dstar_gateway_core::session::server::{ServerSession, Unknown};
use std::time::Instant;

fn main() {
    let session: ServerSession<DExtra, Unknown> = todo!();

    // ERROR: no method named `handle_voice_data` found for
    // `ServerSession<DExtra, Unknown>` — `handle_voice_data` is only
    // implemented on `ServerSession<P, Streaming>`.
    let _ = session.handle_voice_data(Instant::now(), &[0u8; 0]);
}
