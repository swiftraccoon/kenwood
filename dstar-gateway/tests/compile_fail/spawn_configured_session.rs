//! Calling `AsyncSession::spawn` with a `Session<P, Configured>` must
//! be a compile error: the tokio shell only accepts a session that
//! has already been driven through the handshake and promoted to
//! `Connected`.
//!
//! This is the shell-level counterpart to
//! `dstar-gateway-core/tests/compile_fail/send_voice_on_configured.rs`.
//! The core-level test proves that `send_voice` is gated on the
//! `Connected` typestate; this test proves that the async shell's
//! spawn entry point has the same gate.

use std::sync::Arc;

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

    let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);

    // ERROR: `AsyncSession::spawn` is implemented only for
    // `Session<P, Connected>`. Passing a `Session<_, Configured>`
    // fails to satisfy the `where`-clause on the `Session` type
    // parameter, which manifests as a type-mismatch error from rustc.
    let _ = AsyncSession::spawn(session, socket);
    Ok(())
}
