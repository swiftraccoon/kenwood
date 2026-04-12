//! Calling `handle_unlink` on a `ServerSession<_, Closed>` must be
//! a compile error — `Closed` is a terminal state and no further
//! operations are valid on it.
//!
//! `handle_unlink` is only implemented on
//! `ServerSession<P, Linked>`. A `Closed` session cannot call it
//! because the state marker is wrong and the method does not
//! resolve.

use dstar_gateway_core::session::client::DExtra;
use dstar_gateway_core::session::server::{Closed, ServerSession};
use std::time::Instant;

fn main() {
    let session: ServerSession<DExtra, Closed> = todo!();

    // ERROR: no method named `handle_unlink` found for
    // `ServerSession<DExtra, Closed>` — `handle_unlink` is only
    // implemented on `ServerSession<P, Linked>`.
    let _ = session.handle_unlink(Instant::now(), &[0u8; 0]);
}
