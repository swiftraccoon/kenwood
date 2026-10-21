//! Minimal DCS connect example.
//!
//! Connects to a DCS reflector over loopback, listens for 10
//! seconds of inbound events, and disconnects gracefully. The
//! reflector address is hardcoded to `127.0.0.1:30051` so the
//! example **builds** hermetically; `cargo run` will fail to
//! handshake unless a real reflector is listening on that port.
//!
//! Run with:
//! ```text
//! cargo run -p dstar-gateway --example 02_connect_dcs
//! ```
//!
//! DCS is the simplest of the three client protocols: one
//! round-trip (519-byte LINK out, 14-byte ACK in) and no TCP auth
//! step. Voice frames are 100 bytes each and embed the D-STAR
//! header. `send_header` caches it and sends an initial sync frame;
//! `send_voice` includes it in every subsequent frame.

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::session::client::{Configured, Dcs, Session};
use dstar_gateway_core::types::{DcsReflector, Module, RoutingCallsign};
use tokio::net::UdpSocket;

// Examples are a separate compilation unit, so acknowledge workspace
// dev-deps we don't reference directly so the strict
// `unused_crate_dependencies` lint stays silent.
use pcap_parser as _;
use thiserror as _;
use tracing as _;
use trybuild as _;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let sock = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);

    let session: Session<Dcs, Configured> = Session::<Dcs, Configured>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .dcs_reflector(DcsReflector::new("DCS001")?)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer("127.0.0.1:30051".parse()?)
        .build();

    let connecting = session.connect(Instant::now())?;
    let connected = drive_connecting(connecting, &sock, Duration::from_secs(5)).await?;
    let mut async_session = AsyncSession::spawn(connected, Arc::clone(&sock));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => break,
            ev = async_session.next_event() => match ev {
                Some(e) => println!("event: {e:?}"),
                None => break,
            },
        }
    }

    async_session.disconnect().await?;
    Ok(())
}
