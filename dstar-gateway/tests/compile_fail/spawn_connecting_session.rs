//! Calling `AsyncSession::spawn` with a `Session<P, Connecting>` must
//! be a compile error. Even though the session has already enqueued
//! its LINK packet, the shell's loop body is specialized to the
//! `Connected` state (where voice TX is valid), so a mid-handshake
//! session must complete the `.promote()` step before spawn can take
//! it.
//!
//! This guards the runtime property: the tokio shell will never
//! observe a handshake-in-flight session and try to dispatch a voice
//! command on it.

use std::sync::Arc;
use std::time::Instant;

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() {
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(Callsign::try_from_str("W1AW").unwrap())
        .local_module(Module::try_from_char('B').unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:30001".parse().unwrap())
        .build();

    // Transition to `Connecting` — the LINK packet is now in the
    // outbox but the handshake hasn't completed.
    let connecting = session.connect(Instant::now()).unwrap();

    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());

    // ERROR: `AsyncSession::spawn` only accepts `Session<P, Connected>`,
    // not `Session<P, Connecting>`. The type mismatch is rejected at
    // call site by the `S` parameter substitution.
    let _ = AsyncSession::spawn(connecting, socket);
}
