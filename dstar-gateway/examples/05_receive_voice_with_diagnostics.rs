#![cfg(feature = "examples-network")]
//! Connect to a `DExtra` reflector and log events + validator diagnostics.
//!
//! Wires an [`AsyncSession<DExtra>`] to a running XRF/XLX reflector,
//! subscribes to the event stream, and also drains
//! [`dstar_gateway_core::validator::Diagnostic`] entries emitted by
//! the lenient parsers inside the core state machine. The two streams
//! run side-by-side so a reader can correlate a flaky inbound packet
//! against the matching diagnostic.
//!
//! Gated behind the `examples-network` feature because it requires a
//! real reflector. The reflector hostname and module are read from
//! the `REFLECTOR_HOST` / `REFLECTOR_MODULE` env vars.
//!
//! ```text
//! DSTAR_CALLSIGN=N0CALL REFLECTOR_HOST=xrf030.example.com:30001 \
//! REFLECTOR_MODULE=C \
//!     cargo run -p dstar-gateway --example 05_receive_voice_with_diagnostics \
//!     --features examples-network
//! ```

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};
use tokio::net::UdpSocket;

// Acknowledged workspace dev-deps.
use pcap_parser as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "xrf030.example.com:30001".to_string());
    let reflector_module = env::var("REFLECTOR_MODULE")
        .unwrap_or_else(|_| "C".to_string())
        .parse::<Module>()?;

    // Resolve the reflector and bind a local ephemeral socket.
    let peer = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::lookup_host(&reflector_host),
    )
    .await??
    .next()
    .ok_or("failed to resolve reflector host")?;
    let bind = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = Arc::new(UdpSocket::bind(bind).await?);

    // Build and drive the Configured -> Connecting -> Connected
    // handshake. `DExtra` has no auth step, so this is a single
    // round-trip.
    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(reflector_module)
        .peer(peer)
        .build();

    let connecting = session.connect(Instant::now())?;
    let mut connected = drive_connecting(connecting, &sock, Duration::from_secs(5)).await?;

    // Drain any diagnostics from the handshake phase BEFORE handing
    // the session off to the async shell. Once `AsyncSession::spawn`
    // takes ownership the sans-io `Session` is no longer accessible
    // from this task, so diagnostics would require a different API
    // (a shell-side `diagnostics()` accessor, which isn't part of
    // this example's scope).
    for diag in connected.diagnostics() {
        tracing::warn!(?diag, "handshake diagnostic");
    }

    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&sock));

    // Listen for 30 seconds of traffic. `Event::VoiceStart` is the
    // interesting one: it carries the decoded header AND the
    // `Vec<Diagnostic>` accumulated while parsing it. Anything that
    // doesn't bind to a stream (keepalive echoes, disconnect)
    // still surfaces as the normal event stream.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            ev = async_session.next_event() => match ev {
                Some(event) => {
                    tracing::info!(?event, "event");
                    if let dstar_gateway_core::session::client::Event::VoiceStart {
                        diagnostics, ..
                    } = &event
                    {
                        for diag in diagnostics {
                            tracing::warn!(?diag, "header parse diagnostic");
                        }
                    }
                }
                None => break,
            },
        }
    }

    async_session.disconnect().await?;
    Ok(())
}
