//! Minimal `DExtra` connect example.
//!
//! Connects to a `DExtra` (XRF/XLX) reflector over loopback, listens
//! for 10 seconds of inbound events, and disconnects gracefully.
//! The reflector address is hardcoded to `127.0.0.1:30001` so the
//! example **builds** hermetically; `cargo run` will fail to
//! handshake unless a real reflector is listening on that port.
//!
//! Run with:
//! ```text
//! cargo run -p dstar-gateway --example 03_connect_dextra
//! ```
//!
//! `DExtra` is the "middle" of the three protocols: one round-trip
//! (11-byte LINK out, 14-byte ACK in) and no TCP auth, but
//! voice frames use the traditional DSVT-style 56-byte header +
//! 27-byte voice data layout rather than DCS's single 100-byte
//! combined frame.

#[cfg(feature = "insecure-plaintext-xlx-directory")]
use reqwest as _;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::{AsyncSession, drive_connecting};
use dstar_gateway_core::session::client::{Configured, DExtra, Session};
use dstar_gateway_core::types::{Module, RoutingCallsign};
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

    let session: Session<DExtra, Configured> = Session::<DExtra, Configured>::builder()
        .callsign(RoutingCallsign::new("W1AW")?)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer("127.0.0.1:30001".parse()?)
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
