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
use dstar_gateway_core::types::{Module, RoutingCallsign};
use tokio::net::UdpSocket;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::B)
        .reflector_module(Module::C)
        .peer("127.0.0.1:30001".parse()?)
        .build();

    // Transition to `Connecting`; the LINK packet is now in the
    // outbox but the handshake hasn't completed.
    let connecting = session.connect(Instant::now())?;

    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    // ERROR: `AsyncSession::spawn` only accepts `Session<P, Connected>`,
    // not `Session<P, Connecting>`. The type mismatch is rejected at
    // call site by the `S` parameter substitution.
    let _ = AsyncSession::spawn(connecting, socket);
    Ok(())
}
