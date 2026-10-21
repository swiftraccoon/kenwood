#![cfg(feature = "examples-network")]
//! Send 1.2 seconds of silence to a `DPlus` (REF) reflector.
//!
//! Demonstrates the full `DPlus` TX flow:
//! 1. Run the TCP auth step via `AuthClient::authenticate` to
//!    fetch the cached host list.
//! 2. Build a `Session<DPlus, Configured>`, promote through
//!    `Authenticated -> Connecting -> Connected` on the sans-io core.
//! 3. Hand the `Connected` session off to `AsyncSession::spawn`.
//! 4. Send a header, 60 voice frames of `AMBE_SILENCE` (1.2 s @ 50 fps),
//!    and a final EOT. Disconnect gracefully.
//!
//! Gated behind the `examples-network` feature because it requires a
//! real reflector and the `DPlus` auth server. The reflector hostname
//! is read from the `REFLECTOR_HOST` env var and defaults to
//! `ref030.example.com:20001` (which will not resolve in a hermetic
//! build). Run with:
//!
//! ```text
//! DSTAR_CALLSIGN=N0CALL REFLECTOR_HOST=ref030.dstargateway.org:20001 \
//! REFLECTOR_CALLSIGN=REF030 \
//!     cargo run -p dstar-gateway --example 04_send_voice_dplus \
//!     --features examples-network
//! ```
//!
//! **Transmits on real reflectors, so set `ACTUALLY_TRANSMIT=1` to opt
//! in.** Without the opt-in the example resolves the address, runs
//! the handshake, and exits without keying any voice frames.

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::auth::AuthClient;
use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::RelayRouting;
use dstar_gateway_core::header::DstarHeader;
use dstar_gateway_core::session::client::{Authenticated, Configured, DPlus, Session};
use dstar_gateway_core::types::{FrameSequence, Module, RoutingCallsign, StreamId, Suffix};
use dstar_gateway_core::voice::VoiceFrame;
use tokio::net::UdpSocket;

// Examples are a separate compilation unit, so acknowledge workspace
// dev-deps we don't reference directly so the strict
// `unused_crate_dependencies` lint stays silent.
use pcap_parser as _;
use thiserror as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let callsign = RoutingCallsign::new(&env::var("DSTAR_CALLSIGN")?)?;
    let reflector_host =
        env::var("REFLECTOR_HOST").unwrap_or_else(|_| "ref030.example.com:20001".to_string());
    let reflector_callsign = RoutingCallsign::new(
        &env::var("REFLECTOR_CALLSIGN").unwrap_or_else(|_| "REF030".to_string()),
    )?;
    let actually_transmit = env::var("ACTUALLY_TRANSMIT").ok().as_deref() == Some("1");

    // 1. TCP auth: fetches the host list cached by the DPlus auth
    //    server. The returned `HostList` is required to promote the
    //    core's typestate past `Configured`.
    let auth_client = AuthClient::new();
    let host_list = auth_client.authenticate(callsign.callsign()).await?;
    tracing::info!("auth succeeded, {} known hosts", host_list.len());

    // 3. Build a Configured session, attach the host list via the
    //    `authenticate` typestate hop, then trigger the connect.
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

    let session: Session<DPlus, Configured> = Session::<DPlus, Configured>::builder()
        .callsign(callsign)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer(peer)
        .build();

    let authed: Session<DPlus, Authenticated> = session.authenticate(host_list)?;
    let connecting = authed.connect(Instant::now())?;
    let connected = drive_connecting(connecting, &sock, Duration::from_secs(5)).await?;
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&sock));

    // 5. If the operator opted in, key the transmitter for 1.2 seconds
    //    (60 frames at 20 ms).
    if actually_transmit {
        let header = DstarHeader::for_relay(
            RelayRouting {
                operator: callsign,
                local_module: Module::B,
                reflector: reflector_callsign,
                reflector_module: Module::C,
            },
            callsign.callsign(),
            Suffix::EMPTY,
        );
        let sid = StreamId::new(0x1234).ok_or("non-zero stream id")?;
        async_session.send_header(header, sid).await?;

        let silence = VoiceFrame::silence();
        let mut seq = FrameSequence::ZERO;
        for _ in 0..60 {
            async_session.send_voice(sid, seq, silence).await?;
            seq = seq.next();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        async_session.send_eot(sid, seq).await?;
        tracing::info!("TX complete");
    } else {
        tracing::info!(
            "connected but ACTUALLY_TRANSMIT is not 1; skipping voice send to avoid keying the air"
        );
    }

    async_session.disconnect().await?;
    Ok(())
}
