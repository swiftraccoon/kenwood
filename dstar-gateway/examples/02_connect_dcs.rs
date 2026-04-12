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
//! header, so `send_header` caches the header internally and
//! `send_voice` pulls it back out on every frame.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dstar_gateway::tokio_shell::AsyncSession;
use dstar_gateway_core::session::Driver;
use dstar_gateway_core::session::client::{ClientStateKind, Configured, Dcs, Session};
use dstar_gateway_core::types::{Callsign, Module};
use tokio::net::UdpSocket;
use tokio::time::timeout;

// Examples are a separate compilation unit — acknowledge workspace
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
        .callsign(Callsign::try_from_str("W1AW")?)
        .local_module(Module::try_from_char('B')?)
        .reflector_module(Module::try_from_char('C')?)
        .peer("127.0.0.1:30051".parse()?)
        .build();

    let now = Instant::now();
    let mut connecting = session.connect(now)?;

    let Some(tx) = connecting.poll_transmit(now) else {
        return Err("no LINK enqueued".into());
    };
    let _ = sock.send_to(tx.payload, tx.dst).await?;

    let mut buf = [0u8; 2048];
    let Ok(recv) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf)).await else {
        eprintln!("timeout waiting for reflector reply — is 127.0.0.1:30051 listening?");
        return Ok(());
    };
    let (n, peer) = recv?;
    let slice = buf.get(..n).unwrap_or(&[]);
    connecting.handle_input(Instant::now(), peer, slice)?;

    if connecting.state_kind() != ClientStateKind::Connected {
        eprintln!("handshake did not complete");
        return Ok(());
    }

    let connected = connecting.promote()?;
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
